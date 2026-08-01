//! `fdoc convert` — the product command: PDFs in, Markdown/XHTML/JSON out.

use crate::cli::{ConvertArgs, Format};
use crate::commands::common::{self, TierConfig};
use fluree_doc_pdf::document::Element;
use fluree_doc_pdf::{extract_bytes, outline};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn run(args: &ConvertArgs, verbose: bool, quiet: bool) -> i32 {
    let pages = match args.pages.as_deref().map(parse_pages) {
        Some(Ok(set)) => Some(set),
        Some(Err(e)) => {
            eprintln!("error: --pages: {e}");
            return 2;
        }
        None => None,
    };
    let mut cfg = TierConfig::from_env_with(
        args.layout_boxes.as_deref(),
        args.tier_results.as_deref(),
        args.structure_results.as_deref(),
        args.emit_anchors,
    );
    // Once per invocation, not once per document.
    cfg.resolve_escalation(args.escalate, args.no_escalate, quiet);
    cfg.verbose = verbose;
    let cfg = &cfg;

    // Expand directories; `-` means stdin and must be the sole input.
    let stdin_input = args.inputs.len() == 1 && args.inputs[0] == Path::new("-");
    let files: Vec<PathBuf> = if stdin_input {
        Vec::new()
    } else {
        let mut v = Vec::new();
        for input in &args.inputs {
            if input.is_dir() {
                v.extend(common::pdfs_in(input));
            } else {
                v.push(input.clone());
            }
        }
        v
    };

    if stdin_input {
        let mut data = Vec::new();
        if let Err(e) = std::io::stdin().lock().read_to_end(&mut data) {
            eprintln!("error: reading stdin: {e}");
            return 1;
        }
        return match convert_bytes(data, "stdin", cfg, args, pages.as_deref(), quiet) {
            Ok(out) => write_out(&out, args.output.as_deref()),
            Err(e) => {
                eprintln!("error: stdin: {e}");
                1
            }
        };
    }

    if files.is_empty() {
        eprintln!("error: no PDF inputs found");
        return 2;
    }
    if files.len() > 1 && args.out_dir.is_none() {
        eprintln!("error: multiple inputs require --out-dir");
        return 2;
    }

    // Single file: to stdout or -o.
    if files.len() == 1 && args.out_dir.is_none() {
        let f = &files[0];
        let t0 = std::time::Instant::now();
        let r = convert_path(f, cfg, args, pages.as_deref(), quiet);
        if verbose {
            eprintln!("{}: {:.1}ms", f.display(), t0.elapsed().as_secs_f64() * 1e3);
        }
        return match r {
            Ok(out) => write_out(&out, args.output.as_deref()),
            Err(e) => {
                eprintln!("error: {}: {e}", f.display());
                1
            }
        };
    }

    // Batch: one output file per input, worker threads over a shared cursor.
    let out_dir = args.out_dir.as_deref().unwrap();
    if let Err(e) = std::fs::create_dir_all(out_dir) {
        eprintln!("error: cannot create {}: {e}", out_dir.display());
        return 1;
    }
    let jobs = match args.jobs {
        0 => std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1),
        n => n,
    }
    .min(files.len());
    // `report.pdf` and `report.docx` share a stem, so naming outputs by stem
    // alone makes one silently overwrite the other. Disambiguate only where
    // a stem actually repeats, so the ordinary single-format batch keeps
    // plain names.
    let dests = destinations(&files, out_dir, ext(args.format));
    let cursor = AtomicUsize::new(0);
    let failures = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| loop {
                let i = cursor.fetch_add(1, Ordering::Relaxed);
                let Some(f) = files.get(i) else { break };
                let t0 = std::time::Instant::now();
                match convert_path(f, cfg, args, pages.as_deref(), quiet) {
                    Ok(out) => {
                        let dst = &dests[i];
                        if let Err(e) = std::fs::write(dst, out) {
                            eprintln!("error: writing {}: {e}", dst.display());
                            failures.fetch_add(1, Ordering::Relaxed);
                        } else if verbose {
                            eprintln!("{}: {:.1}ms", f.display(), t0.elapsed().as_secs_f64() * 1e3);
                        }
                    }
                    Err(e) => {
                        eprintln!("error: {}: {e}", f.display());
                        failures.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    let failed = failures.load(Ordering::Relaxed);
    if !quiet {
        eprintln!(
            "converted {}/{} files -> {}",
            files.len() - failed,
            files.len(),
            out_dir.display()
        );
    }
    if failed > 0 {
        1
    } else {
        0
    }
}

/// Source formats `convert` accepts. PDF is the geometric path; the others
/// carry their structure explicitly and need no inference.
fn ext_is(p: &Path, kinds: &[&str]) -> bool {
    p.extension()
        .and_then(|x| x.to_str())
        .is_some_and(|x| kinds.iter().any(|k| x.eq_ignore_ascii_case(k)))
}

fn convert_path(
    pdf: &Path,
    cfg: &TierConfig,
    args: &ConvertArgs,
    pages: Option<&[usize]>,
    quiet: bool,
) -> Result<String, String> {
    let data = std::fs::read(pdf).map_err(|e| e.to_string())?;
    let stem = common::stem_of(pdf);
    // Structural formats: the source declares what a PDF makes us infer, so
    // these readers map rather than measure and carry no geometry.
    if ext_is(pdf, &["md", "markdown", "txt", "text"]) {
        let text = String::from_utf8(data).map_err(|e| format!("not UTF-8: {e}"))?;
        return Ok(render(
            &fluree_doc_markdown::parse(&text),
            stem,
            args,
            Vec::new(),
            &fluree_doc_model::Notes::default(),
        ));
    }
    if ext_is(pdf, &["html", "htm", "xhtml"]) {
        let text = String::from_utf8_lossy(&data).into_owned();
        return Ok(render(
            &fluree_doc_html::parse(&text),
            stem,
            args,
            Vec::new(),
            &fluree_doc_model::Notes::default(),
        ));
    }
    // Word's macro-enabled and template variants are the same OOXML
    // package with a different extension.
    if ext_is(pdf, &["docx", "docm", "dotx", "dotm"]) {
        let els = fluree_doc_docx::parse(&data).map_err(|e| e.to_string())?;
        return Ok(render(
            &els,
            stem,
            args,
            Vec::new(),
            &fluree_doc_model::Notes::default(),
        ));
    }
    if ext_is(pdf, &["pptx", "pptm", "potx", "potm", "ppsx", "ppsm"]) {
        let els = fluree_doc_pptx::parse(&data).map_err(|e| e.to_string())?;
        return Ok(render(
            &els,
            stem,
            args,
            Vec::new(),
            &fluree_doc_model::Notes::default(),
        ));
    }
    if fluree_doc_pdf::image::Format::sniff(&data).is_some() {
        return convert_image(data, stem, cfg, args, quiet);
    }
    convert_bytes(data, common::stem_of(pdf), cfg, args, pages, quiet)
}

/// A bare image: one page of pixels, and only the deep reader can read it.
///
/// Every other source has a deterministic reading to fall back on. This one
/// has none, so with no reader configured the honest output is nothing — and
/// saying so matters more here than anywhere else, because an empty document
/// is otherwise indistinguishable from a blank image.
fn convert_image(
    data: Vec<u8>,
    stem: &str,
    cfg: &TierConfig,
    args: &ConvertArgs,
    quiet: bool,
) -> Result<String, String> {
    let format = fluree_doc_pdf::image::Format::sniff(&data).ok_or("not a recognised image")?;
    let doc = fluree_doc_pdf::image::as_document(&data)
        .ok_or_else(|| format!("{} header declares no usable size", format.mime()))?;
    let sizes: Vec<fluree_doc_model::PageSize> = doc
        .pages
        .iter()
        .map(|p| fluree_doc_model::PageSize {
            index: p.index,
            width: p.width,
            height: p.height,
        })
        .collect();
    let mut elements: Vec<Element> = Vec::new();
    if cfg.escalate {
        let readings = crate::escalate::read_image(
            std::path::Path::new(stem),
            &data,
            format.mime(),
            &cfg.config,
            cfg.verbose,
        )?;
        if !readings.is_empty() {
            // One synthetic element for the splice to replace, so the image
            // travels the same page-tier path a scanned PDF page does.
            elements.push(Element {
                id: String::new(),
                kind: "doco:Paragraph".into(),
                page: 0,
                bbox: Some(doc.pages[0].images[0].bbox),
                text: String::new(),
                level: None,
                cells: None,
                header_rows: None,
                sub_headers: None,
                merged_down: None,
                merged_left: None,
                figure: None,
                links: None,
                provenance: "rust",
                evidence: "layout",
            });
            fluree_doc_pdf::arbiter::splice_with_page(
                &mut elements,
                stem,
                &readings,
                None,
                &[Vec::new()],
            );
            elements.retain(|e| !e.text.trim().is_empty());
        }
    }
    if elements.is_empty() && !quiet {
        eprintln!(
            "note: {} carries no text layer, so only a model can read it",
            format.mime()
        );
        eprintln!(
            "      {}",
            if cfg.escalate {
                "the reader returned nothing for it"
            } else {
                "run `fdoc config gemini --credentials <key.json>` to enable one"
            }
        );
    }
    Ok(render(
        &elements,
        stem,
        args,
        sizes,
        &fluree_doc_model::Notes::default(),
    ))
}

/// Emit an element stream in the requested format. Shared by every source.
fn render(
    elements: &[Element],
    stem: &str,
    args: &ConvertArgs,
    pages: Vec<fluree_doc_model::PageSize>,
    notes: &fluree_doc_model::Notes,
) -> String {
    match args.format {
        Format::Md => fluree_doc_model::to_markdown_with(elements, notes),
        Format::Xhtml => fluree_doc_model::to_xhtml_with(elements, notes),
        Format::Json => serde_json::to_string_pretty(elements).unwrap(),
        Format::Doco => {
            let opts = fluree_doc_pdf::doco::DocoOptions {
                base_iri: args
                    .base_iri
                    .clone()
                    .unwrap_or_else(|| format!("urn:fluree-doc-parse:{stem}")),
                doc_iri: args.doc_iri.clone(),
                pages,
                unread: notes.unread.clone(),
            };
            fluree_doc_pdf::doco::to_doco(elements, &opts)
        }
        Format::Text => fluree_doc_pdf::doco::to_text(elements),
    }
}

fn convert_bytes(
    data: Vec<u8>,
    stem: &str,
    cfg: &TierConfig,
    args: &ConvertArgs,
    pages: Option<&[usize]>,
    quiet: bool,
) -> Result<String, String> {
    let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(data.clone()))
        .map_err(|e| format!("parse: {e:?}"))?;
    // Kept for the crop pass, which re-derives the escalation anchors.
    let data_for_crops = if cfg.escalate {
        data.clone()
    } else {
        Vec::new()
    };
    let ol = outline::extract(&raw);
    let mut doc = extract_bytes(data).map_err(|e| format!("extract: {e}"))?;
    let opts = cfg.options_for(stem);
    let mut a = fluree_doc_pdf::document::analyze_with(&mut doc, &ol, &opts);
    common::arbitrate_layout_titles(cfg.layout_boxes.as_deref(), stem, &mut a.elements);
    // The page's own text, so the arbiter can ask whether an escalated
    // reading says anything the page does not.
    let page_text: Vec<Vec<String>> = doc
        .pages
        .iter()
        .map(|p| fluree_doc_pdf::fidelity::page_lines(&p.glyphs))
        .collect();
    common::apply_tiers(
        cfg.tier_results.as_deref(),
        cfg.structure_results.as_deref(),
        stem,
        &mut a.elements,
        &page_text,
        &a.furniture,
    );
    // A configured reader, in this same command. Sidecars win where both are
    // present: `--tier-results` names readings someone already has, and
    // paying to produce them again would be surprising.
    if cfg.escalate && cfg.tier_results.is_none() {
        let readings = crate::escalate::read_document(
            std::path::Path::new(stem),
            &data_for_crops,
            &doc,
            &raw,
            &cfg.config,
            pages,
            cfg.verbose,
        )?;
        if !readings.is_empty() {
            fluree_doc_pdf::arbiter::splice_with_page(
                &mut a.elements,
                stem,
                &readings,
                None,
                &page_text,
            );
            fluree_doc_pdf::arbiter::scrub_furniture(&mut a.elements, &a.furniture);
        }
    }
    // After the tiers: an escalated reading replaces the text an anchor has to
    // be found in.
    fluree_doc_pdf::link::attach(
        &mut a.elements,
        &fluree_doc_pdf::link::extract(&raw),
        &doc.pages,
    );
    if let Some(keep) = pages {
        a.elements.retain(|e: &Element| keep.contains(&e.page));
    }
    // Page geometry travels with the graph: a bbox cannot be placed on a
    // rendered page without the size of the page it came from.
    let sizes = doc
        .pages
        .iter()
        .filter(|p| pages.is_none_or(|keep| keep.contains(&p.index)))
        .map(|p| fluree_doc_model::PageSize {
            index: p.index,
            width: p.width,
            height: p.height,
        })
        .collect();
    // After the tiers: a page is unread only once whatever was going to read
    // it has run.
    let notes = fluree_doc_model::Notes {
        unread: fluree_doc_pdf::unread_pages(&doc, &a.elements),
    };
    if let (Some(note), false) = (notes.summary(), quiet) {
        eprintln!("warning: {note}");
    }
    Ok(render(&a.elements, stem, args, sizes, &notes))
}

/// One output path per input, keeping stem names unless a stem repeats —
/// in which case the source extension joins it (`report.docx.md`).
fn destinations(files: &[PathBuf], out_dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for f in files {
        *seen.entry(common::stem_of(f)).or_default() += 1;
    }
    files
        .iter()
        .map(|f| {
            let stem = common::stem_of(f);
            let name = if seen.get(stem).copied().unwrap_or(0) > 1 {
                match f.extension().and_then(|x| x.to_str()) {
                    Some(src) => format!("{stem}.{src}.{ext}"),
                    None => format!("{stem}.{ext}"),
                }
            } else {
                format!("{stem}.{ext}")
            };
            out_dir.join(name)
        })
        .collect()
}

fn write_out(out: &str, dst: Option<&Path>) -> i32 {
    match dst {
        Some(p) => {
            if let Err(e) = std::fs::write(p, out) {
                eprintln!("error: writing {}: {e}", p.display());
                return 1;
            }
            0
        }
        None => {
            print!("{out}");
            0
        }
    }
}

fn ext(format: Format) -> &'static str {
    match format {
        Format::Md => "md",
        Format::Xhtml => "xhtml",
        Format::Json => "json",
        Format::Doco => "jsonld",
        Format::Text => "txt",
    }
}

/// Parse a 1-based page-range list (`3`, `1-5`, `1,4,9-12`) into 0-based
/// page indices.
pub(crate) fn parse_pages(spec: &str) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (lo, hi) = match part.split_once('-') {
            Some((a, b)) => (parse_page(a)?, parse_page(b)?),
            None => {
                let p = parse_page(part)?;
                (p, p)
            }
        };
        if lo > hi {
            return Err(format!("range '{part}' is descending"));
        }
        out.extend(lo - 1..hi);
    }
    if out.is_empty() {
        return Err("no pages selected".into());
    }
    Ok(out)
}

fn parse_page(s: &str) -> Result<usize, String> {
    match s.trim().parse::<usize>() {
        Ok(0) => Err("pages are 1-based".into()),
        Ok(n) => Ok(n),
        Err(_) => Err(format!("'{s}' is not a page number")),
    }
}

// The bench harness compatibility contract: `fdoc md <pdf>` must emit exactly
// what `convert <pdf>` emits, environment tiers applied, no page filter.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colliding_stems_get_disambiguated() {
        // Five sources named `demo` wrote one file and silently lost four.
        let files: Vec<PathBuf> = ["demo.md", "demo.docx", "report.pdf"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let d = destinations(&files, Path::new("/out"), "jsonld");
        assert_eq!(d[0], Path::new("/out/demo.md.jsonld"));
        assert_eq!(d[1], Path::new("/out/demo.docx.jsonld"));
        // A stem that does not repeat keeps its plain name.
        assert_eq!(d[2], Path::new("/out/report.jsonld"));
    }

    #[test]
    fn page_ranges_parse() {
        assert_eq!(parse_pages("3").unwrap(), vec![2]);
        assert_eq!(parse_pages("1-3").unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_pages("1,4,6-8").unwrap(), vec![0, 3, 5, 6, 7]);
        assert!(parse_pages("0").is_err());
        assert!(parse_pages("5-2").is_err());
        assert!(parse_pages("x").is_err());
    }
}
