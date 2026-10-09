//! `fdoc convert` — the product command: PDFs in, Markdown/XHTML/JSON out.

use crate::cli::{ConvertArgs, Format};
use crate::commands::common::{self, TierConfig};
use fluree_doc_pdf::document::Element;
use fluree_doc_pdf::{extract_bytes, outline};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The input being converted, beyond its bytes.
struct Source<'a> {
    /// What sidecar files and messages name it by: the file's stem, or
    /// `stdin`.
    stem: &'a str,
    /// What this input's outputs are named by in its run: its stem, unless
    /// another input shares that (see [`slots`]).
    slot: String,
    /// Whether `stem` is a file's own name rather than a stand-in.
    named: bool,
    /// What the output records as the input's name: `--source-name`, else
    /// the file's name. Standard input has none.
    name: Option<String>,
    /// The input's bytes as lowercase hex SHA-256.
    sha256: String,
}

impl<'a> Source<'a> {
    fn file(path: &'a Path, slot: &str, data: &[u8], args: &ConvertArgs) -> Self {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        Source {
            stem: common::stem_of(path),
            slot: slot.to_string(),
            named: true,
            name: args.source_name.clone().or(name),
            sha256: fluree_doc_model::sha256_hex(data),
        }
    }

    fn stdin(data: &[u8], args: &ConvertArgs) -> Self {
        Source {
            stem: "stdin",
            slot: "stdin".to_string(),
            named: false,
            name: args.source_name.clone(),
            sha256: fluree_doc_model::sha256_hex(data),
        }
    }
}

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
                v.extend(common::sources_in(input));
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
        let src = Source::stdin(&data, args);
        let converted = if fluree_doc_transcript::Format::sniff(&data).is_some() {
            convert_transcript(&data, &src, args, quiet)
        } else if fluree_doc_email::Format::sniff(&data).is_some() {
            convert_email(&data, &src, args, quiet)
        } else {
            convert_bytes(data, &src, cfg, args, pages.as_deref(), quiet)
        };
        return match converted {
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
    // Given to several inputs, a flag naming one document would name them
    // all alike: one set of node IRIs, so their graphs merge in a store,
    // and one tag, so a retraction removes them together.
    if let (true, Some(flag)) = (files.len() > 1, single_input_flag(args)) {
        eprintln!(
            "error: {flag} names one input, and there are {}",
            files.len()
        );
        return 2;
    }

    // Single file: to stdout or -o.
    if files.len() == 1 && args.out_dir.is_none() {
        let f = &files[0];
        let t0 = std::time::Instant::now();
        let r = convert_path(f, common::stem_of(f), cfg, args, pages.as_deref(), quiet);
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
    let slots = slots(&files);
    let cursor = AtomicUsize::new(0);
    let failures = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| loop {
                let i = cursor.fetch_add(1, Ordering::Relaxed);
                let Some(f) = files.get(i) else { break };
                let t0 = std::time::Instant::now();
                match convert_path(f, &slots[i], cfg, args, pages.as_deref(), quiet) {
                    Ok(out) => {
                        let dst = &out_dir.join(format!("{}.{}", slots[i], ext(args.format)));
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

/// The first flag given that names a single document.
fn single_input_flag(args: &ConvertArgs) -> Option<&'static str> {
    [
        ("--doc-iri", args.doc_iri.is_some()),
        ("--base-iri", args.base_iri.is_some()),
        ("--source-name", args.source_name.is_some()),
    ]
    .into_iter()
    .find_map(|(flag, given)| given.then_some(flag))
}

fn convert_path(
    path: &Path,
    slot: &str,
    cfg: &TierConfig,
    args: &ConvertArgs,
    pages: Option<&[usize]>,
    quiet: bool,
) -> Result<String, String> {
    use common::SourceKind;
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let src = Source::file(path, slot, &data, args);
    let kind = common::source_kind(path);
    // A transcript by its content before anything by its name: `.vtt` has no
    // registered type on most systems, so it arrives renamed as often as not.
    if fluree_doc_transcript::Format::sniff(&data).is_some() || kind == Some(SourceKind::Transcript)
    {
        return convert_transcript(&data, &src, args, quiet);
    }
    // An email by its content too: a saved message is `.eml`, `.mht`, `.txt`
    // or nothing at all, depending on who saved it.
    if fluree_doc_email::Format::sniff(&data).is_some() || kind == Some(SourceKind::Email) {
        return convert_email(&data, &src, args, quiet);
    }
    // What a file declares about itself: its title, author and dates.
    let declared = |info| fluree_doc_model::Notes {
        info,
        ..Default::default()
    };
    // Structural formats: the source declares what a PDF makes us infer, so
    // these readers map rather than measure and carry no geometry.
    let (elements, notes) = match kind {
        Some(SourceKind::Markdown) => {
            let text = String::from_utf8(data).map_err(|e| format!("not UTF-8: {e}"))?;
            (
                fluree_doc_markdown::parse(&text),
                fluree_doc_model::Notes::default(),
            )
        }
        Some(SourceKind::Html) => {
            let text = fluree_doc_html::decode(&data);
            (
                fluree_doc_html::parse(&text),
                declared(fluree_doc_html::info(&text)),
            )
        }
        Some(SourceKind::Docx) => {
            let (elements, info) =
                fluree_doc_docx::parse_with_info(&data).map_err(|e| e.to_string())?;
            (elements, declared(info))
        }
        Some(SourceKind::Pptx) => {
            let (elements, info) =
                fluree_doc_pptx::parse_with_info(&data).map_err(|e| e.to_string())?;
            (elements, declared(info))
        }
        // A workbook: each sheet is a page, its islands of cells are tables.
        Some(SourceKind::Xlsx) => {
            let (elements, info) =
                fluree_doc_xlsx::parse_with_info(&data).map_err(|e| e.to_string())?;
            (elements, declared(info))
        }
        _ if fluree_doc_pdf::image::Format::sniff(&data).is_some() => {
            return convert_image(data, &src, cfg, args, quiet);
        }
        _ => return convert_bytes(data, &src, cfg, args, pages, quiet),
    };
    Ok(render(&elements, &src, args, Vec::new(), &notes))
}

/// A meeting transcript or a caption file: one paragraph per speaker turn.
///
/// A transcript with no cues converts to nothing, and says so, because that
/// output is otherwise indistinguishable from a file that was not read.
fn convert_transcript(
    data: &[u8],
    src: &Source,
    args: &ConvertArgs,
    quiet: bool,
) -> Result<String, String> {
    let stem = src.stem;
    let elements = fluree_doc_transcript::parse(data).map_err(|e| e.to_string())?;
    if elements.is_empty() && !quiet {
        eprintln!("note: {stem}: the transcript holds no cues, so the output is empty");
    }
    Ok(render(
        &elements,
        src,
        args,
        Vec::new(),
        &fluree_doc_model::Notes::default(),
    ))
}

/// An email: each message of its thread, and its attachments described.
///
/// An attachment is a document of its own, so it is not in this output.
/// `--attachments DIR` saves the files for converting on their own; without
/// it, the note on stderr says what was left out.
fn convert_email(
    data: &[u8],
    src: &Source,
    args: &ConvertArgs,
    quiet: bool,
) -> Result<String, String> {
    let stem = src.stem;
    let email = fluree_doc_email::parse(data).map_err(|e| e.to_string())?;
    let count = email.attachments.len();
    let plural = if count == 1 { "" } else { "s" };
    match &args.attachments {
        Some(dir) if count > 0 => {
            // The slot, not the stem: two `mail.eml` in one batch would
            // otherwise save their attachments over each other's.
            let dir = dir.join(&src.slot);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            let mut taken = std::collections::HashSet::new();
            for (i, a) in email.attachments.iter().enumerate() {
                let path = dir.join(file_name(a.info.filename.as_deref(), i, &mut taken));
                std::fs::write(&path, &a.bytes)
                    .map_err(|e| format!("writing {}: {e}", path.display()))?;
            }
            if !quiet {
                eprintln!(
                    "note: {stem}: {count} attachment{plural} saved to {}",
                    dir.display()
                );
            }
        }
        None if count > 0 && !quiet => {
            let names: Vec<String> = email
                .attachments
                .iter()
                .map(|a| {
                    a.info
                        .filename
                        .clone()
                        .unwrap_or_else(|| a.info.content_type.clone())
                })
                .collect();
            eprintln!(
                "note: {stem}: {count} attachment{plural} not converted ({}); \
                 pass --attachments DIR to save them",
                names.join(", ")
            );
        }
        _ => {}
    }
    Ok(render(
        &email.elements,
        src,
        args,
        Vec::new(),
        &email.notes(),
    ))
}

/// A safe, unique file name for an attachment: its own name with any path
/// taken off, or a numbered stand-in, and unique within its email.
fn file_name(
    name: Option<&str>,
    index: usize,
    taken: &mut std::collections::HashSet<String>,
) -> String {
    let base = name
        .and_then(|n| n.rsplit(['/', '\\']).next())
        .map(|n| n.trim().trim_start_matches('.').to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("attachment-{}", index + 1));
    let mut candidate = base.clone();
    let mut n = 2;
    while !taken.insert(candidate.clone()) {
        candidate = match base.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem} ({n}).{ext}"),
            None => format!("{base} ({n})"),
        };
        n += 1;
    }
    candidate
}

/// A bare image: one page of pixels, and only the deep reader can read it.
///
/// Every other source has a deterministic reading to fall back on. This one
/// has none, so with no reader configured the honest output is nothing — and
/// saying so matters more here than anywhere else, because an empty document
/// is otherwise indistinguishable from a blank image.
fn convert_image(
    data: Vec<u8>,
    src: &Source,
    cfg: &TierConfig,
    args: &ConvertArgs,
    quiet: bool,
) -> Result<String, String> {
    let stem = src.stem;
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
            folio: None,
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
                turn: None,
                message: None,
                resumes: None,
                signature: false,
                datums: None,
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
        src,
        args,
        sizes,
        &fluree_doc_model::Notes::default(),
    ))
}

/// Emit an element stream in the requested format. Shared by every source.
fn render(
    elements: &[Element],
    src: &Source,
    args: &ConvertArgs,
    pages: Vec<fluree_doc_model::PageSize>,
    notes: &fluree_doc_model::Notes,
) -> String {
    match args.format {
        Format::Md => fluree_doc_model::to_markdown_with(elements, notes),
        Format::Xhtml => fluree_doc_model::to_xhtml_with(elements, notes),
        Format::Json => serde_json::to_string_pretty(elements).unwrap(),
        Format::Doco => {
            // A caller naming the document names its nodes too: they are
            // minted under its IRI, so they cost the store no namespace the
            // document IRI does not.
            let base_iri = args
                .base_iri
                .clone()
                .or_else(|| args.doc_iri.clone())
                .unwrap_or_else(|| {
                    fluree_doc_model::doco::default_base_iri(
                        src.named.then_some(src.stem),
                        &src.sha256,
                    )
                });
            let opts = fluree_doc_pdf::doco::DocoOptions {
                base_iri,
                doc_iri: args.doc_iri.clone(),
                pages,
                unread: notes.unread.clone(),
                running_text: notes.running_text.clone(),
                info: notes.info.clone(),
                attachments: notes.attachments.clone(),
                sha256: Some(src.sha256.clone()),
                source_name: src.name.clone(),
            };
            fluree_doc_pdf::doco::to_doco(elements, &opts)
        }
        Format::Text => fluree_doc_pdf::doco::to_text(elements),
    }
}

fn convert_bytes(
    data: Vec<u8>,
    src: &Source,
    cfg: &TierConfig,
    args: &ConvertArgs,
    pages: Option<&[usize]>,
    quiet: bool,
) -> Result<String, String> {
    let stem = src.stem;
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
        .map(|p| {
            let (width, height) = p.display_size();
            fluree_doc_model::PageSize {
                index: p.index,
                width,
                height,
                folio: a.folios.get(p.index).cloned().flatten(),
            }
        })
        .collect();
    // After the tiers: a page is unread only once whatever was going to read
    // it has run.
    let notes = fluree_doc_model::Notes {
        unread: fluree_doc_pdf::unread_pages(&doc, &a.elements),
        // A bare page number identifies nothing; the rest of the running
        // block is the document's own name for itself.
        running_text: a
            .furniture
            .iter()
            .filter(|(text, _)| text.chars().any(char::is_alphabetic))
            .map(|(text, _)| text.clone())
            .collect(),
        info: fluree_doc_pdf::info::read(&raw),
        ..Default::default()
    };
    if let (Some(note), false) = (notes.summary(), quiet) {
        eprintln!("warning: {note}");
    }
    Ok(render(&a.elements, src, args, sizes, &notes))
}

/// A name per input for its outputs in one run: its stem, unless another
/// input shares it, then with its source extension (`report.docx`), then
/// numbered (`report.md (2)`) where even that repeats, as `a/report.md` and
/// `b/report.md` do. Allocated before any conversion starts, so no two
/// inputs write one file or one attachment directory. Compared without
/// case, as the file systems that ignore it would.
fn slots(files: &[PathBuf]) -> Vec<String> {
    let mut stems: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for f in files {
        *stems.entry(common::stem_of(f).to_lowercase()).or_default() += 1;
    }
    let mut taken = std::collections::HashSet::new();
    files
        .iter()
        .map(|f| {
            let stem = common::stem_of(f);
            let base = match f.extension().and_then(|x| x.to_str()) {
                Some(src) if stems[&stem.to_lowercase()] > 1 => format!("{stem}.{src}"),
                _ => stem.to_string(),
            };
            let mut slot = base.clone();
            let mut n = 2;
            while !taken.insert(slot.to_lowercase()) {
                slot = format!("{base} ({n})");
                n += 1;
            }
            slot
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
        let slots = |names: &[&str]| slots(&names.iter().map(PathBuf::from).collect::<Vec<_>>());
        // Five sources named `demo` wrote one file and silently lost four.
        assert_eq!(
            slots(&["demo.md", "demo.docx", "report.pdf"]),
            ["demo.md", "demo.docx", "report"]
        );
        // The same name in two directories, and a name differing in case,
        // which a case-insensitive file system holds as one.
        assert_eq!(
            slots(&["a/report.md", "b/report.md", "c/Report.md"]),
            ["report.md", "report.md (2)", "Report.md (3)"]
        );
        // A plain stem that a disambiguated name already took.
        assert_eq!(
            slots(&["x/report.md", "y/report.md", "report.md.txt"]),
            ["report.md", "report.md (2)", "report.md (3)"]
        );
    }

    #[test]
    fn a_flag_naming_one_document_refuses_several_inputs() {
        use clap::Parser;
        let convert = |extra: &[&str]| {
            let argv = ["fdoc", "convert", "a.md", "b.md", "--out-dir", "out"];
            let cli = crate::cli::Cli::try_parse_from(argv.iter().chain(extra)).unwrap();
            match cli.command {
                crate::cli::Commands::Convert(args) => args,
                _ => unreachable!(),
            }
        };
        for flag in ["--doc-iri", "--base-iri", "--source-name"] {
            let args = convert(&[flag, "urn:doc:a"]);
            assert_eq!(single_input_flag(&args), Some(flag));
            assert_eq!(run(&args, false, true), 2, "{flag}");
        }
        assert_eq!(single_input_flag(&convert(&[])), None);
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
