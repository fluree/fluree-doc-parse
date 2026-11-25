//! `fdoc convert` — the product command: PDFs in, Markdown/XHTML/JSON out.

use crate::cli::{ConvertArgs, Format};
use crate::commands::common::{self, TierConfig};
use fluree_doc_pdf::document::Element;
use fluree_doc_pdf::{extract_bytes_with, outline, ExtractOptions};
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
        args.ocr_words.as_deref(),
        args.emit_anchors,
    );
    // Once per invocation, not once per document.
    cfg.resolve_escalation(args.escalate, args.no_escalate, quiet);
    cfg.verbose = verbose;
    // A flag beats the environment: `--ocr` given on the command line reads
    // the pixels, whatever `FDOC_OCR_WORDS` holds (the two flags together are
    // refused by the parser).
    if args.ocr.is_some() && args.ocr_words.is_none() && cfg.ocr_words.take().is_some() && verbose {
        eprintln!("note: --ocr given, FDOC_OCR_WORDS ignored");
    }
    // A word directory that is not there is a mistake in the command, not
    // a batch with nothing read yet.
    if let Some(dir) = cfg.ocr_words.as_deref().filter(|d| !d.is_dir()) {
        eprintln!("error: --ocr-words: {} is not a directory", dir.display());
        return 2;
    }
    // Words already read win over reading them again with an engine the
    // config names, as `--tier-results` wins over a configured model.
    if cfg.ocr_words.is_none() {
        match crate::commands::ocr::engine_for_convert(
            args.ocr,
            args.models.as_deref(),
            args.ocr_max_megapixels,
            &cfg.config.ocr,
        ) {
            Ok(engine) => cfg.ocr = engine.map(common::OcrHandle),
            Err(e) => {
                eprintln!("error: --ocr: {e}");
                return 2;
            }
        }
    }
    if args.models.is_some() && cfg.ocr.is_none() && !quiet {
        eprintln!(
            "warning: --models names an OCR engine's assets, but no engine reads with them (--ocr)"
        );
    }
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

    // Readings are looked up by stem: two inputs of one stem (`report.pdf`
    // and `report.png`) would each find the other's.
    if (cfg.ocr_words.is_some() || cfg.tier_results.is_some()) && !quiet {
        let mut seen: std::collections::HashMap<&str, &Path> = Default::default();
        for f in &files {
            let stem = common::stem_of(f);
            if let Some(other) = seen.insert(stem, f) {
                eprintln!(
                    "warning: {} and {} share the name {stem}: readings looked up by it may be the other's",
                    other.display(),
                    f.display()
                );
            }
        }
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
    // What the OCR engine says about an image it reads names this file.
    let _doc = super::ocr::Reading::of(path);
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
        // Plain text is read as lines, not as Markdown: an indented line
        // is not code and a row of `=` makes no heading. Markdown saved
        // under a text file's name is told by its markup and read as such.
        Some(SourceKind::Text) => {
            let text = fluree_doc_text::decode(&data);
            let elements = if fluree_doc_text::is_markdown(&text) {
                fluree_doc_markdown::parse(&text)
            } else {
                fluree_doc_text::parse(&text)
            };
            (elements, fluree_doc_model::Notes::default())
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
            // The page header and footer are read into the text, marked as
            // what they are; their lines are also the running text a PDF's
            // identity block is kept in, so a graph finds a document's
            // identity in one place whatever it was read from.
            let running_text = elements
                .iter()
                .filter(|e| matches!(e.evidence, "header" | "footer"))
                .flat_map(|e| {
                    fluree_doc_model::doco::projection_text(e)
                        .lines()
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .filter(|l| !l.trim().is_empty())
                .collect();
            (
                elements,
                fluree_doc_model::Notes {
                    running_text,
                    ..declared(info)
                },
            )
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

/// A bare image: one page of pixels, read by an OCR engine or a model.
///
/// With an OCR engine (`--ocr`), or the words one wrote (`--ocr-words`,
/// `<stem>_p0_full.json` or `<stem>.json`), its words are laid out like a
/// scanned page's. Otherwise, or where OCR read nothing, only the deep reader
/// can read it.
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
    // Upright: a phone photo's page is the size the reader sees it at, as
    // OCR's is. Only the header is read.
    let header = fluree_doc_image::header(&data).ok();
    let pages = header.map_or(1, |h| h.pages);
    if pages > 1 && !quiet {
        eprintln!("warning: {stem}: only the first of its {pages} pages is read");
    }
    // A model may read what OCR leaves unread, as on a scanned PDF page.
    let model_after = cfg.escalate && cfg.tier_results.is_none();
    if cfg.ocr.is_some() || cfg.ocr_words.is_some() {
        match ocr_image(&data, header, src, cfg, quiet) {
            Ok(read) if read.read_something || !model_after => {
                if let (Some(note), false) = (read.notes.summary(), quiet) {
                    eprintln!("warning: {note}");
                }
                return Ok(render(&read.elements, src, args, read.sizes, &read.notes));
            }
            Ok(_) => {
                if !quiet {
                    eprintln!("note: {stem}: OCR read nothing; the configured model reads it");
                }
            }
            Err(e) if model_after => {
                if !quiet {
                    eprintln!("warning: {stem}: {e}; the configured model reads it");
                }
            }
            Err(e) => return Err(e),
        }
    }
    let (w, h) = header
        .map(|h| (h.width, h.height))
        .or_else(|| fluree_doc_pdf::image::dimensions(&data))
        .filter(|&(w, h)| w > 0 && h > 0)
        .ok_or_else(|| format!("{} header declares no usable size", format.mime()))?;
    let doc = fluree_doc_pdf::image::page_of(w, h, false);
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
    // A reading someone already has (`--tier-results`, `<stem>_p0_full.json`),
    // else the configured reader, as for a scanned PDF page.
    let readings: Option<Box<dyn fluree_doc_pdf::arbiter::TierBackend>> =
        if let Some(dir) = cfg.tier_results.as_deref() {
            Some(Box::new(fluree_doc_pdf::arbiter::FixtureBackend::new(
                dir.to_path_buf(),
            )))
        } else if cfg.escalate {
            let readings = crate::escalate::read_image(
                std::path::Path::new(stem),
                &data,
                format.mime(),
                &cfg.config,
                cfg.verbose,
            )?;
            (!readings.is_empty()).then(|| Box::new(readings) as Box<_>)
        } else {
            None
        };
    if let Some(readings) = readings {
        // One synthetic element for the splice to replace, so the image
        // travels the same page-tier path a scanned PDF page does.
        elements.push(Element {
            bbox: Some(doc.pages[0].images[0].bbox),
            ..Element::new("doco:Paragraph", String::new(), "rust", "layout")
        });
        fluree_doc_pdf::arbiter::splice_with_page(
            &mut elements,
            stem,
            readings.as_ref(),
            None,
            &[Vec::new()],
        );
        elements.retain(|e| !e.text.trim().is_empty());
    }
    if elements.is_empty() && !quiet {
        eprintln!(
            "note: {} carries no text layer, so only a model can read it",
            format.mime()
        );
        eprintln!(
            "      {}",
            if cfg.tier_results.is_some() {
                "--tier-results holds no reading of it (`<stem>_p0_full.json`)"
            } else if cfg.escalate {
                "the reader returned nothing for it"
            } else {
                "run `fdoc config gemini --credentials <key.json>` to enable one"
            }
        );
    }
    // An image nothing read is listed unread, so the output says so too. A
    // reading of the whole image read it, however short.
    let mut unread = if elements.is_empty() {
        fluree_doc_pdf::unread_pages(&doc, &elements)
    } else {
        Vec::new()
    };
    unread.extend(later_pages(pages));
    let notes = fluree_doc_model::Notes {
        unread,
        ..Default::default()
    };
    Ok(render(&elements, src, args, sizes, &notes))
}

/// What OCR made of a bare image.
struct ImageOcr {
    elements: Vec<Element>,
    sizes: Vec<fluree_doc_model::PageSize>,
    notes: fluree_doc_model::Notes,
    /// Some word was taken, or a reading spliced over the page.
    read_something: bool,
}

/// A bare image read by OCR: its words laid on a one-page document and
/// analysed like a scanned page. An engine reads the image decoded upright;
/// words already read (`--ocr-words`) need only its header for the page.
fn ocr_image(
    data: &[u8],
    header: Option<fluree_doc_image::Header>,
    src: &Source,
    cfg: &TierConfig,
    quiet: bool,
) -> Result<ImageOcr, String> {
    let stem = src.stem;
    let (mut doc, flag, injected) = match &cfg.ocr {
        Some(engine) => {
            let image = fluree_doc_image::decode(data)?;
            if image.truncated && !quiet {
                eprintln!(
                    "warning: {stem}: the file ends before its image does; part of it may be blank"
                );
            }
            let mut doc = fluree_doc_image::document(&image);
            let reading = engine
                .0
                .read(&image.pixels)
                .map_err(|e| format!("--ocr: {e}"))?;
            let page = fluree_doc_pdf::ocr::Crop::Page;
            let injected = fluree_doc_pdf::ocr::inject(&mut doc, &[(0, page, reading)]);
            (doc, "--ocr", injected)
        }
        None => {
            let h = header.ok_or("the image's header could not be read")?;
            let mut doc = fluree_doc_pdf::image::page_of(h.width, h.height, h.bitonal);
            let dir = cfg.ocr_words.as_deref().expect("words or an engine");
            // The words of its crop (`<stem>_p0_full.json`), or of the
            // image file (`<stem>.json`, as `fdoc ocr` names them): in a
            // directory shared with a PDF of the same stem, the one read on
            // this image is the one sized as it is.
            let mut readings = common::ocr_readings(dir, stem, &doc, quiet);
            readings.extend(image_words(dir, stem, quiet));
            let sized = |r: &fluree_doc_pdf::ocr::WordReading| {
                r.coords == fluree_doc_pdf::ocr::Coords::Page
                    || (r.width, r.height) == (Some(f64::from(h.width)), Some(f64::from(h.height)))
            };
            let pages = readings
                .iter()
                .filter(|r| r.1 == fluree_doc_pdf::ocr::Crop::Page)
                .count();
            if let Some(k) = readings.iter().position(|r| sized(&r.2)) {
                readings = vec![readings.swap_remove(k)];
            } else if !readings.is_empty() {
                if !quiet {
                    eprintln!(
                        "warning: --ocr-words: {stem}: no reading is sized as the image ({} × {}): none read",
                        h.width, h.height
                    );
                }
                readings.clear();
            }
            if pages > 1 && !quiet {
                eprintln!(
                    "warning: --ocr-words: {stem}: both {stem}_p0_full.json and {stem}.json; read the one sized as the image"
                );
            }
            let injected = fluree_doc_pdf::ocr::inject(&mut doc, &readings);
            (doc, "--ocr-words", injected)
        }
    };
    report_ocr(stem, flag, &injected, cfg.verbose, quiet);
    let mut a = fluree_doc_pdf::document::analyze_with(&mut doc, &[], &cfg.options_for(stem));
    // Readings someone already has (`--tier-results`, `--structure-results`)
    // replace what OCR made of the page, as on a PDF page.
    let page_text: Vec<Vec<String>> = doc
        .pages
        .iter()
        .map(|p| fluree_doc_pdf::fidelity::page_lines(&p.glyphs))
        .collect();
    common::apply_tiers(
        cfg.tier_results.as_deref(),
        cfg.structure_results.as_deref(),
        cfg.emit_anchors,
        stem,
        &mut a.elements,
        &page_text,
        &a.furniture,
    );
    fluree_doc_pdf::ocr::mark_provenance(&mut a.elements, &injected);
    let sizes = doc
        .pages
        .iter()
        .map(|p| fluree_doc_model::PageSize {
            index: p.index,
            width: p.width,
            height: p.height,
            folio: None,
        })
        .collect();
    let unread = fluree_doc_pdf::unread_pages(&doc, &a.elements);
    let read_something = unread.is_empty();
    let mut notes = fluree_doc_model::Notes {
        unread,
        ..Default::default()
    };
    // The pages after the first of a multi-page TIFF, never read.
    notes
        .unread
        .extend(later_pages(header.map_or(1, |h| h.pages)));
    Ok(ImageOcr {
        elements: a.elements,
        sizes,
        notes,
        read_something,
    })
}

/// The pages after the first of a file of `pages` (a multi-page TIFF): never
/// read, so listed unread, for the output to say so.
fn later_pages(pages: usize) -> impl Iterator<Item = fluree_doc_model::UnreadPage> {
    (1..pages).map(|index| fluree_doc_model::UnreadPage {
        index,
        reason: "LaterPage".into(),
    })
}

/// The words `fdoc ocr` read from the image file itself: `<stem>.json`.
fn image_words(
    dir: &Path,
    stem: &str,
    quiet: bool,
) -> Option<(
    usize,
    fluree_doc_pdf::ocr::Crop,
    fluree_doc_pdf::ocr::WordReading,
)> {
    let path = dir.join(format!("{stem}.json"));
    let text = std::fs::read_to_string(&path).ok()?;
    match fluree_doc_pdf::ocr::WordReading::from_json(&text) {
        Ok(r) => Some((0, fluree_doc_pdf::ocr::Crop::Page, r)),
        Err(e) => {
            if !quiet {
                eprintln!("warning: --ocr-words: {}: {e}", path.display());
            }
            None
        }
    }
}

/// Emit an element stream in the requested format. Shared by every source.
fn render(
    elements: &[Element],
    src: &Source,
    args: &ConvertArgs,
    pages: Vec<fluree_doc_model::PageSize>,
    notes: &fluree_doc_model::Notes,
) -> String {
    // No source writes a stray control character into what it emits: a
    // model's reading or another format's text can carry one as surely as a
    // PDF's font.
    let scrubbed: Vec<Element>;
    let elements = if elements.iter().any(|e| {
        e.text.chars().any(fluree_doc_model::is_stray_control)
            || e.cells
                .iter()
                .flatten()
                .flatten()
                .any(|c| c.chars().any(fluree_doc_model::is_stray_control))
    }) {
        scrubbed = elements
            .iter()
            .cloned()
            .map(|mut e| {
                e.scrub_controls();
                e
            })
            .collect();
        &scrubbed[..]
    } else {
        elements
    };
    match args.format {
        Format::Md => fluree_doc_model::to_markdown_with_options(
            elements,
            notes,
            &fluree_doc_model::MarkdownOptions {
                links: args.md_links.into(),
            },
        ),
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

/// The note when a document has more OCR crops than `ocr.max_crops`.
pub(crate) fn ocr_cap_note(stem: &str, max: usize, capped: usize) -> String {
    format!(
        "note: --ocr: {stem}: reading {max} crops and leaving {capped} unread (pages read whole first, then routed regions, then small ones) — raise `ocr.max_crops` (0 for no limit) to read them all"
    )
}

/// One stderr line per OCR reading set aside, and with `--verbose` one per
/// reading taken.
fn report_ocr(
    stem: &str,
    flag: &str,
    injected: &[fluree_doc_pdf::ocr::Injected],
    verbose: bool,
    quiet: bool,
) {
    for r in injected {
        let name = format!("{stem}_p{}_{}", r.page, r.crop.tag());
        match r.refused {
            Some(why) if !quiet => eprintln!("warning: {flag}: {name}: {why}"),
            None if verbose => eprintln!(
                "ocr: {name}: {} words taken, {} dropped, skew {:.1}°{}",
                r.taken,
                r.dropped,
                r.skew_deg,
                r.mean_conf
                    .map(|c| format!(", mean confidence {c:.3}"))
                    .unwrap_or_default()
            ),
            _ => {}
        }
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
    let ol = outline::extract(&raw);
    let opts = cfg.options_for(stem);
    // Images are probed for the router, which runs for escalation, for the
    // anchors and sidecars analysis routes for, and for OCR words, which go
    // only where it sends a reader; and for an OCR engine, which reads the
    // texty images too small to route. A deterministic parse has no use for
    // the decode.
    let mut eopts = ExtractOptions::default();
    eopts.probe_images = cfg.escalate
        || cfg.ocr_words.is_some()
        || cfg.ocr.is_some()
        || fluree_doc_pdf::document::routing_is_consulted(&opts);
    let mut doc = extract_bytes_with(data, &eopts).map_err(|e| format!("extract: {e}"))?;
    // OCR words become glyphs before anything measures the pages: the
    // analysis reads them like any text, and a region they fill no longer
    // asks a model for a reading.
    let injected = if let Some(engine) = &cfg.ocr {
        let max = cfg.config.ocr.max_crops();
        // At the stored resolution of what is under each crop (#11): an
        // engine reads small text a 2× render loses.
        let pass = fluree_doc_pdf::ocr::read_pages(
            &mut doc,
            &raw,
            engine.0.as_ref(),
            pages,
            fluree_doc_pdf::escalate::CropScale::Stored,
            max,
        );
        for (crop, e) in &pass.failed {
            if !quiet {
                eprintln!("warning: --ocr: {stem}_{crop}: {e}");
            }
        }
        if pass.capped > 0 && !quiet {
            eprintln!("{}", ocr_cap_note(stem, max, pass.capped));
        }
        report_ocr(stem, "--ocr", &pass.injected, cfg.verbose, quiet);
        pass.injected
    } else if let Some(dir) = cfg.ocr_words.as_deref() {
        // Only the pages asked for, as the engine reads only those: words
        // already read for the others must not change how these read.
        let mut readings = common::ocr_readings(dir, stem, &doc, quiet);
        if let Some(keep) = pages {
            readings.retain(|(page, _, _)| keep.contains(page));
        }
        let injected = fluree_doc_pdf::ocr::inject(&mut doc, &readings);
        report_ocr(stem, "--ocr-words", &injected, cfg.verbose, quiet);
        injected
    } else {
        Vec::new()
    };
    // A configured reader reads the whole pages chosen from the document as
    // extracted, which the analysis below changes.
    let read_here = cfg.escalate && cfg.tier_results.is_none();
    let extracted = read_here.then(|| doc.clone());
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
        cfg.emit_anchors,
        stem,
        &mut a.elements,
        &page_text,
        &a.furniture,
    );
    // A configured reader, in this same command. Sidecars win where both are
    // present: `--tier-results` names readings someone already has, and
    // paying to produce them again would be surprising.
    if let Some(extracted) = &extracted {
        read_in_run(
            &mut a,
            extracted,
            cfg,
            stem,
            &page_text,
            pages,
            |jobs, hints| {
                crate::escalate::read_document(
                    std::path::Path::new(stem),
                    jobs,
                    hints,
                    &doc,
                    &raw,
                    &cfg.config,
                    cfg.verbose,
                )
            },
        )?;
    }
    // After the tiers, which replace whole pages; before the links, which a
    // footer carries as often as the body does.
    let (first, last) = match pages {
        Some(keep) => (
            keep.iter().copied().min().unwrap_or(0),
            keep.iter().copied().max().unwrap_or(0),
        ),
        None => (0, doc.pages.len().saturating_sub(1)),
    };
    fluree_doc_pdf::furniture::keep_running(&mut a.elements, &a.running, first, last);
    // After the tiers, whose splices keep their own provenance, and after the
    // running lines kept once, which may be OCR's too.
    fluree_doc_pdf::ocr::mark_provenance(&mut a.elements, &injected);
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
        // A page number identifies nothing, bare or as `Page 3 of 15`; the
        // rest of the running block is the document's own name for itself.
        running_text: a
            .furniture
            .iter()
            .filter(|(text, vary)| fluree_doc_pdf::furniture::says_something(text, *vary))
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

/// Take the crops the analysis asks for, have `read` read them, and splice
/// what it returned into that analysis.
///
/// The crops are the analysis's own anchors
/// ([`fluree_doc_pdf::escalate::crops_at_anchors`]), so each reading fills
/// the anchor its crop was named after; `extracted`, the document before
/// the analysis changed it, chooses the whole pages. A table the
/// `--structure-results` veto would keep deterministic whatever the model
/// read is left out.
///
/// The splice runs even when `read` returned nothing: the analysis carries an
/// anchor for every crop it could have asked for, and a crop goes unread when
/// `--pages` or `escalation.max_crops` leaves it out or the reader finds
/// nothing printed on it. The splice is what drops an anchor no reading
/// answers, so skipping it would print the bare token where nobody asked for
/// one. With `--emit-anchors` the splice keeps it, on purpose.
///
/// `read` is the reader `convert` calls, taken as an argument so a test can
/// stand one in without the network. It is handed the crops
/// [`crate::escalate::to_read`] leaves: those on the pages `pages` names,
/// within `escalation.max_crops`, ranked by the reasons they were chosen for.
fn read_in_run(
    a: &mut fluree_doc_pdf::document::Analysis,
    extracted: &fluree_doc_pdf::Document,
    cfg: &TierConfig,
    stem: &str,
    page_text: &[Vec<String>],
    pages: Option<&[usize]>,
    read: impl FnOnce(
        fluree_doc_pdf::escalate::CropJobs,
        &fluree_doc_pdf::escalate::CropHints,
    ) -> Result<fluree_doc_pdf::escalate::Readings, String>,
) -> Result<(), String> {
    let hints = crate::escalate::jobs::hints(extracted, stem, cfg);
    // A table the structure readings veto keeps its grid whatever the model
    // reads, so reading it would be paid for and thrown away, and take the
    // place of a crop under `escalation.max_crops`. One backend for the
    // choice and the splice, which read the same structure files.
    let structure = common::structure_backend(cfg.structure_results.as_deref());
    let structure = structure
        .as_ref()
        .map(|s| s as &dyn fluree_doc_pdf::arbiter::TierBackend);
    let (jobs, reasons) = crate::escalate::jobs::at_anchors(extracted, a, stem, &hints, structure);
    let jobs = crate::escalate::to_read(
        std::path::Path::new(stem),
        jobs,
        &reasons,
        &cfg.config,
        pages,
    );
    let readings = read(jobs, &hints)?;
    common::splice(
        cfg.emit_anchors,
        &mut a.elements,
        stem,
        &readings,
        structure,
        page_text,
        &a.furniture,
    );
    Ok(())
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

    /// A text page with a raster table the glyph layer cannot read: prose
    /// above and below, a texty image between. The router asks for that
    /// region alone, which is what a live reader is then paid to read.
    fn page_with_a_raster_region() -> fluree_doc_pdf::Document {
        let mut doc = fluree_doc_pdf::image::page_of(600, 800, false);
        let page = &mut doc.pages[0];
        page.images[0].bbox = fluree_doc_pdf::BBox {
            x0: 50.0,
            y0: 300.0,
            x1: 550.0,
            y1: 550.0,
        };
        let line = "The figures below are printed as an image of a table.";
        let baselines = (0..14)
            .map(|i| 60.0 + 14.0 * f64::from(i))
            .chain((0..14).map(|i| 580.0 + 14.0 * f64::from(i)));
        for y in baselines {
            for (i, c) in line.chars().enumerate() {
                let x = 50.0 + 5.0 * i as f64;
                let draw_index = page.glyphs.len();
                page.glyphs.push(fluree_doc_pdf::Glyph {
                    text: c.to_string(),
                    bbox: (c != ' ').then_some(fluree_doc_pdf::BBox {
                        x0: x,
                        y0: y - 8.0,
                        x1: x + 5.0,
                        y1: y,
                    }),
                    page: 0,
                    origin: (x, y),
                    rotation_deg: 0.0,
                    font_size: 10.0,
                    advance: Some(5.0),
                    weight: None,
                    draw_index,
                });
            }
        }
        doc
    }

    /// `convert --escalate` with a configured reader and no sidecars, with a
    /// stand-in for the network: the options it analyses with, then
    /// [`read_in_run`] as `convert_bytes` calls it, with a reader answering
    /// the crop it is asked for with `answer`, or with nothing.
    fn convert_live(emit_anchors: bool, answer: Option<&str>) -> Vec<Element> {
        let cfg = TierConfig {
            escalate: true,
            emit_anchors,
            ..Default::default()
        };
        let extracted = page_with_a_raster_region();
        let mut doc = extracted.clone();
        let mut a = fluree_doc_pdf::document::analyze_with(&mut doc, &[], &cfg.options_for("doc"));
        let page_text = vec![fluree_doc_pdf::fidelity::page_lines(&doc.pages[0].glyphs)];
        read_in_run(
            &mut a,
            &extracted,
            &cfg,
            "doc",
            &page_text,
            None,
            |jobs, _| {
                let crops: Vec<String> = jobs
                    .iter()
                    .flat_map(|(page, regions)| {
                        regions
                            .iter()
                            .flatten()
                            .map(move |(tag, _)| format!("p{page}_{tag}"))
                    })
                    .collect();
                assert_eq!(crops, ["p0_r0"], "the region alone is read");
                let mut readings = fluree_doc_pdf::escalate::Readings::default();
                if let Some(text) = answer {
                    readings.insert(crops[0].clone(), text.into());
                }
                Ok(readings)
            },
        )
        .expect("a stand-in reader does not fail");
        a.elements
    }

    /// A page of prose with two raster regions, A above B, and a label
    /// printed over A in faux bold: each of its ten letters drawn twice at
    /// one spot. As extracted, A holds twenty glyphs and the glyph layer
    /// reads it; once the analysis drops the doubles it holds ten, and A is
    /// a region the router asks a reader for, as B is.
    fn page_with_faux_bold_over_a_region() -> fluree_doc_pdf::Document {
        let mut doc = page_with_a_raster_region();
        let page = &mut doc.pages[0];
        let (a, b) = (
            fluree_doc_pdf::BBox {
                x0: 50.0,
                y0: 260.0,
                x1: 550.0,
                y1: 400.0,
            },
            fluree_doc_pdf::BBox {
                x0: 50.0,
                y0: 420.0,
                x1: 550.0,
                y1: 560.0,
            },
        );
        page.images[0].bbox = a;
        let mut second = page.images[0];
        second.bbox = b;
        page.images.push(second);
        // Prose above and below the two regions only.
        page.glyphs.retain(|g| {
            let y = g.origin.1;
            !(a.y0..=b.y1).contains(&y)
        });
        for pass in 0..2 {
            for (i, c) in "LABELTEXTS".chars().enumerate() {
                let x = 200.0 + 6.0 * i as f64;
                let draw_index = page.glyphs.len();
                page.glyphs.push(fluree_doc_pdf::Glyph {
                    text: c.to_string(),
                    bbox: Some(fluree_doc_pdf::BBox {
                        x0: x + 0.1 * f64::from(pass),
                        y0: 322.0,
                        x1: x + 5.0 + 0.1 * f64::from(pass),
                        y1: 330.0,
                    }),
                    page: 0,
                    origin: (x + 0.1 * f64::from(pass), 330.0),
                    rotation_deg: 0.0,
                    font_size: 10.0,
                    advance: Some(6.0),
                    weight: None,
                    draw_index,
                });
            }
        }
        doc
    }

    /// A reading lands where the crop it was read from sits. The crops a
    /// live run sends and the anchors its analysis mints are one set: a
    /// region the analysis counts the router out of as extracted is still
    /// asked for, and every reading is filed under the anchor of its own
    /// crop, not under a neighbour's that took its index.
    /// An OCR reading names its region as the analysis's anchors do: a
    /// label in faux bold over a capture covers it as drawn and not once
    /// cleaned, and both count it.
    #[test]
    fn an_ocr_reading_and_an_anchor_name_a_region_alike() {
        let doc = page_with_faux_bold_over_a_region();
        let regions = match &fluree_doc_pdf::document::region_routes(&doc)[0] {
            fluree_doc_pdf::route::Route::VlmRegions(r) => r.clone(),
            other => panic!("{other:?}"),
        };
        let mut analysed = doc.clone();
        let a = fluree_doc_pdf::document::analyze_with(
            &mut analysed,
            &[],
            &fluree_doc_pdf::document::AnalyzeOptions {
                emit_anchors: true,
                ..Default::default()
            },
        );
        let anchors: Vec<(String, f64)> = a
            .elements
            .iter()
            .filter(|e| e.evidence == "route")
            .map(|e| (e.text.clone(), e.rect().y0))
            .collect();
        assert_eq!(
            anchors,
            regions
                .iter()
                .enumerate()
                .map(|(i, b)| (format!("[[VLM:p0:r{i}]]"), b.y0))
                .collect::<Vec<_>>()
        );
        assert_eq!(anchors.len(), 2, "{anchors:?}");
        // A reading of the second region is looked up under its name.
        let dir = std::env::temp_dir().join(format!(
            "fdoc-ocr-names-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let words = r#"{"coords":"px","width":100,"height":100,"words":[{"text":"B","box":[10,10,20,20]}]}"#;
        std::fs::write(dir.join("doc_p0_r1.json"), words).unwrap();
        let read = common::ocr_readings(&dir, "doc", &doc, true);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].1, fluree_doc_pdf::ocr::Crop::Region(1));
    }

    #[test]
    fn a_live_region_reading_lands_at_its_own_crop() {
        let cfg = TierConfig {
            escalate: true,
            ..Default::default()
        };
        let extracted = page_with_faux_bold_over_a_region();
        let mut doc = extracted.clone();
        let mut a = fluree_doc_pdf::document::analyze_with(&mut doc, &[], &cfg.options_for("doc"));
        let page_text = vec![fluree_doc_pdf::fidelity::page_lines(&doc.pages[0].glyphs)];
        read_in_run(
            &mut a,
            &extracted,
            &cfg,
            "doc",
            &page_text,
            None,
            |jobs, _| {
                let mut readings = fluree_doc_pdf::escalate::Readings::default();
                for (page, regions) in &jobs {
                    for (tag, bbox) in regions.iter().flatten() {
                        readings.insert(
                            format!("p{page}_{tag}"),
                            format!("Reading of the crop at y0={}.", bbox.y0),
                        );
                    }
                }
                Ok(readings)
            },
        )
        .expect("a stand-in reader does not fail");
        let read: Vec<(f64, &str)> = a
            .elements
            .iter()
            .filter(|e| e.provenance == "vlm")
            .map(|e| (e.rect().y0, e.text.as_str()))
            .collect();
        assert_eq!(
            read,
            [
                (260.0, "Reading of the crop at y0=260."),
                (420.0, "Reading of the crop at y0=420."),
            ]
        );
    }

    /// The crops `dev render-routed` writes for an external reader
    /// (`jobs::to_render`, from an analysis with anchors on, as that command
    /// makes it) are named after the anchor of `convert --tier-results` each
    /// fills, so a sidecar reading lands where its crop sits, as a live one
    /// does. So are the crops of the library's `plan`, for a caller that
    /// has only the extracted document.
    #[test]
    fn render_routed_names_its_crops_after_the_anchors_they_fill() {
        let cfg = TierConfig {
            tier_results: Some(PathBuf::from("readings")),
            ..Default::default()
        };
        let extracted = page_with_faux_bold_over_a_region();
        let hints = fluree_doc_pdf::escalate::CropHints::default();
        let rendered = {
            let options = fluree_doc_pdf::document::AnalyzeOptions {
                emit_anchors: true,
                ..TierConfig::default().options_for("doc")
            };
            let a = fluree_doc_pdf::document::analyze_with(&mut extracted.clone(), &[], &options);
            let (jobs, _, vetoed) =
                crate::escalate::jobs::to_render(&extracted, &a, "doc", &hints, None);
            assert!(vetoed.is_empty());
            jobs
        };
        let planned =
            fluree_doc_pdf::escalate::plan(&extracted, &[], &cfg.options_for("doc"), &hints);
        let mut doc = extracted.clone();
        let a = fluree_doc_pdf::document::analyze_with(&mut doc, &[], &cfg.options_for("doc"));
        let anchors: Vec<(String, f64)> = a
            .elements
            .iter()
            .filter(|e| e.evidence == "route")
            .map(|e| (e.text.clone(), e.rect().y0))
            .collect();
        assert_eq!(
            anchors,
            [
                ("[[VLM:p0:r0]]".into(), 260.0),
                ("[[VLM:p0:r1]]".into(), 420.0)
            ]
        );
        let crops = |jobs: &fluree_doc_pdf::escalate::CropJobs| -> Vec<(String, f64)> {
            jobs.iter()
                .flat_map(|(page, regions)| {
                    regions
                        .iter()
                        .flatten()
                        .map(move |(tag, bbox)| (format!("[[VLM:p{page}:{tag}]]"), bbox.y0))
                })
                .collect()
        };
        assert_eq!(crops(&rendered), anchors);
        assert_eq!(crops(&planned), anchors);
    }

    /// `triage` prices the crops a live run is about to pay for, from the
    /// same anchors: on a page where the analysis counts a region the page
    /// as extracted does not, it counts both regions, as the run reads both.
    #[test]
    fn triage_prices_the_crops_a_live_run_reads() {
        let cfg = TierConfig {
            escalate: true,
            ..Default::default()
        };
        let extracted = page_with_faux_bold_over_a_region();
        let mut doc = extracted.clone();
        let mut a = fluree_doc_pdf::document::analyze_with(&mut doc, &[], &cfg.options_for("doc"));
        let priced = crate::commands::triage::priced(&extracted, &a, "doc", &cfg).0;
        let mut read = 0;
        read_in_run(&mut a, &extracted, &cfg, "doc", &[], None, |jobs, _| {
            read = fluree_doc_pdf::escalate::crop_count(&jobs);
            Ok(fluree_doc_pdf::escalate::Readings::default())
        })
        .expect("a stand-in reader does not fail");
        assert_eq!((priced, read), (2, 2));
    }

    /// Two pages read whole: a scan, and before it a page that routes a
    /// region as extracted and reads as a scan once its faux-bold label
    /// counts once, so it is read whole in place of that region.
    fn a_scan_after_a_page_read_whole_for_its_region() -> fluree_doc_pdf::Document {
        let mut doc = page_with_a_raster_region();
        let mut scan = doc.pages[0].clone();
        scan.index = 1;
        scan.glyphs.clear();
        let page = &mut doc.pages[0];
        page.images[0].bbox = fluree_doc_pdf::BBox {
            x0: 0.0,
            y0: 0.0,
            x1: 600.0,
            y1: 500.0,
        };
        page.glyphs.clear();
        for line in 0..6 {
            for (i, c) in "LABELTEXTS".chars().enumerate() {
                for pass in 0..2 {
                    let x = 50.0 + 6.0 * i as f64 + 0.1 * f64::from(pass);
                    let y = 600.0 + 14.0 * f64::from(line);
                    let draw_index = page.glyphs.len();
                    page.glyphs.push(fluree_doc_pdf::Glyph {
                        text: c.to_string(),
                        bbox: Some(fluree_doc_pdf::BBox {
                            x0: x,
                            y0: y - 8.0,
                            x1: x + 5.0,
                            y1: y,
                        }),
                        page: 0,
                        origin: (x, y),
                        rotation_deg: 0.0,
                        font_size: 10.0,
                        advance: Some(6.0),
                        weight: None,
                        draw_index,
                    });
                }
            }
        }
        doc.pages.push(scan);
        doc
    }

    /// Under `escalation.max_crops`, a page a live run reads whole in place
    /// of its region ranks with routed regions, after a scan, by the reason
    /// it was chosen for: routed again as analysed, it would read as a scan
    /// and come first.
    #[test]
    fn under_the_ceiling_a_page_read_for_its_region_comes_after_a_scan() {
        let mut cfg = TierConfig {
            escalate: true,
            ..Default::default()
        };
        cfg.config.escalation.max_crops = 1;
        let extracted = a_scan_after_a_page_read_whole_for_its_region();
        let mut doc = extracted.clone();
        let mut a = fluree_doc_pdf::document::analyze_with(&mut doc, &[], &cfg.options_for("doc"));
        assert!(matches!(
            fluree_doc_pdf::route::decide(&extracted.pages[0]).0,
            fluree_doc_pdf::route::Route::VlmRegions(_)
        ));
        assert!(matches!(
            fluree_doc_pdf::route::decide(&doc.pages[0]).0,
            fluree_doc_pdf::route::Route::Vlm(_)
        ));
        let mut asked = Vec::new();
        read_in_run(&mut a, &extracted, &cfg, "doc", &[], None, |jobs, _| {
            asked = jobs;
            Ok(fluree_doc_pdf::escalate::Readings::default())
        })
        .expect("a stand-in reader does not fail");
        assert_eq!(asked, [(1, None)], "the scan, not the region's page");
    }

    /// `dev render-routed` writes a reason for every whole page it writes:
    /// a page read whole in place of its region gives the region's.
    #[test]
    fn render_routed_gives_a_page_read_for_its_region_that_reason() {
        let cfg = TierConfig::default();
        let extracted = a_scan_after_a_page_read_whole_for_its_region();
        let mut doc = extracted.clone();
        let a = fluree_doc_pdf::document::analyze_with(
            &mut doc,
            &[],
            &fluree_doc_pdf::document::AnalyzeOptions {
                emit_anchors: true,
                ..cfg.options_for("doc")
            },
        );
        let hints = crate::escalate::jobs::hints(&extracted, "doc", &cfg);
        let (jobs, reasons, _) =
            crate::escalate::jobs::to_render(&extracted, &a, "doc", &hints, None);
        assert_eq!(jobs, [(0, None), (1, None)]);
        let named: Vec<&str> = jobs
            .iter()
            .map(|(page, _)| reasons.get(page).expect("a reason").as_str())
            .collect();
        assert_eq!(named, ["region", "near_blank"]);
    }

    /// A page of prose in three panels under a full-width header: a page-wide
    /// projection finds one gutter of the two, so the page is in column
    /// doubt.
    fn page_in_column_doubt() -> fluree_doc_pdf::Document {
        let mut doc = page_with_a_raster_region();
        let page = &mut doc.pages[0];
        page.images.clear();
        page.glyphs.clear();
        let mut put = |x: f64, y: f64| {
            let draw_index = page.glyphs.len();
            page.glyphs.push(fluree_doc_pdf::Glyph {
                text: "x".into(),
                bbox: Some(fluree_doc_pdf::BBox {
                    x0: x,
                    y0: y,
                    x1: x + 9.0,
                    y1: y + 10.0,
                }),
                page: 0,
                origin: (x, y + 10.0),
                rotation_deg: 0.0,
                font_size: 10.0,
                advance: None,
                weight: None,
                draw_index,
            });
        };
        for row in 0..7 {
            for i in 0..30 {
                put(50.0 + f64::from(i) * 10.0, 100.0 + f64::from(row) * 12.0);
            }
        }
        for i in 0..45 {
            put(50.0 + f64::from(i) * 10.0, 184.0);
        }
        for row in 0..14 {
            for x0 in [50.0, 250.0, 460.0] {
                for i in 0..12 {
                    put(x0 + f64::from(i) * 10.0, 220.0 + f64::from(row) * 12.0);
                }
            }
        }
        doc
    }

    /// `triage` reads the configuration a live run reads: a page in column
    /// doubt is read whole when `escalation.on_column_doubt` says so, or
    /// `FDOC_ESCALATE_COLUMNS` over it, and counted then, and neither
    /// otherwise.
    #[test]
    fn triage_prices_column_doubt_as_the_configuration_has_it() {
        let counts = |on_column_doubt: bool, columns: Option<bool>| {
            let mut cfg = TierConfig {
                escalate: true,
                columns,
                ..Default::default()
            };
            cfg.config.escalation.on_column_doubt = on_column_doubt;
            let extracted = page_in_column_doubt();
            let mut doc = extracted.clone();
            let mut a =
                fluree_doc_pdf::document::analyze_with(&mut doc, &[], &cfg.options_for("doc"));
            assert!(
                fluree_doc_pdf::document::column_doubt(&extracted.pages[0], &a).is_some(),
                "the page is in column doubt"
            );
            let priced = crate::commands::triage::priced(&extracted, &a, "doc", &cfg).0;
            let mut read = 0;
            read_in_run(&mut a, &extracted, &cfg, "doc", &[], None, |jobs, _| {
                read = fluree_doc_pdf::escalate::crop_count(&jobs);
                Ok(fluree_doc_pdf::escalate::Readings::default())
            })
            .expect("a stand-in reader does not fail");
            (priced, read)
        };
        // `TierConfig` holds the run's override; the default one has none,
        // whatever this process's environment says.
        assert_eq!(counts(true, None), (1, 1));
        assert_eq!(counts(false, None), (0, 0));
        assert_eq!(counts(true, Some(false)), (0, 0));
        assert_eq!(counts(false, Some(true)), (1, 1));
    }

    /// A crop the reader found nothing on was read: asked for anchors, the
    /// run still drops its anchor, as no one is left to fill it.
    #[test]
    fn asked_for_anchors_a_crop_found_empty_leaves_none() {
        let elements = convert_live(true, Some(""));
        assert!(elements.iter().all(|e| !e.text.contains("[[VLM")));
        assert!(elements.iter().all(|e| e.provenance != "vlm"));
    }

    #[test]
    fn a_region_a_live_reader_reads_lands_in_the_output() {
        let elements = convert_live(false, Some("Revenue rose 12% to 4.2 billion."));
        let read: Vec<&Element> = elements.iter().filter(|e| e.provenance == "vlm").collect();
        assert_eq!(read.len(), 1, "the reading was paid for and must be used");
        assert_eq!(read[0].text, "Revenue rose 12% to 4.2 billion.");
        assert!(elements.iter().all(|e| !e.text.contains("[[VLM")));
    }

    #[test]
    fn a_region_no_reading_answers_leaves_no_anchor_behind() {
        let elements = convert_live(false, None);
        assert!(!elements.is_empty());
        assert!(
            elements.iter().all(|e| !e.text.contains("[[VLM")),
            "an unanswered anchor must drop, not print"
        );
    }

    /// Asked for anchors, a run that reads some of them itself leaves the
    /// rest for whoever fills anchors: what it read lands, what it did not
    /// stays marked.
    #[test]
    fn asked_for_anchors_a_live_run_leaves_the_unanswered_ones() {
        let unanswered = convert_live(true, None);
        let anchors: Vec<&str> = unanswered
            .iter()
            .filter(|e| e.text.contains("[[VLM"))
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(anchors, ["[[VLM:p0:r0]]"]);
        let answered = convert_live(true, Some("Revenue rose 12% to 4.2 billion."));
        assert!(answered.iter().all(|e| !e.text.contains("[[VLM")));
        assert!(answered.iter().any(|e| e.provenance == "vlm"));
    }

    /// The prose of [`page_with_a_raster_region`] without its image: a page
    /// nothing routes, whose crops are those of the anchors its analysis is
    /// given.
    fn page_of_prose() -> fluree_doc_pdf::Document {
        let mut doc = page_with_a_raster_region();
        doc.pages[0].images.clear();
        doc
    }

    /// A table anchor and the 2x2 grid it labels.
    fn an_anchored_grid() -> Vec<Element> {
        vec![
            Element::new(
                "doco:Figure",
                "[[VLMTAB:p0:t0]]",
                "rust",
                "table-confidence",
            ),
            Element {
                cells: Some(vec![
                    vec!["12".into(), "34".into()],
                    vec!["56".into(), "78".into()],
                ]),
                ..Element::new("doco:Table", "12 34 56 78", "rust", "grid")
            },
        ]
    }

    /// A deep reading of that table, of its own shape: unopposed, it wins.
    const DEEP_READING: &str =
        "<table><tr><td>12</td><td>34</td></tr><tr><td>56</td><td>78</td></tr></table>";

    /// A directory holding `doc_p0_t0.json`, a reading of one table block
    /// `html`, in the sidecar format; removed with the returned guard.
    struct Sidecars(PathBuf);

    impl Sidecars {
        fn holding(kind: &str, html: &str) -> Self {
            // Unique per test, not per clock tick: tests run in parallel, and
            // a clock that ticks in microseconds gave two the same name.
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("fdoc-{kind}-{}-{n}", std::process::id()));
            std::fs::create_dir(&dir).unwrap();
            let block = serde_json::json!([{"parsing_res_list": [
                {"block_label": "table", "block_content": html}
            ]}]);
            std::fs::write(dir.join("doc_p0_t0.json"), block.to_string()).unwrap();
            Self(dir)
        }
    }

    impl Drop for Sidecars {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The same two rows with one column fewer than the grid: the structure
    /// reading that corroborates it.
    fn corroborating_structure() -> Sidecars {
        Sidecars::holding(
            "structure",
            "<table><tr><td>12 34</td></tr><tr><td>56 78</td></tr></table>",
        )
    }

    /// A live reader's reading of a table answers to the structure reading
    /// as a sidecar's does: where `--structure-results` corroborates the
    /// grid, the three-way veto keeps it. And since the veto would discard
    /// whatever the model read, the model is not asked to read it. Asked for
    /// anchors, the vetoed table's anchor is answered all the same, by the
    /// veto, and drops rather than waiting for a reading the veto discards.
    #[test]
    fn a_live_table_reading_answers_to_the_structure_veto() {
        let structure = corroborating_structure();
        let read_live = |structure_results: Option<PathBuf>, emit_anchors: bool| {
            let cfg = TierConfig {
                escalate: true,
                emit_anchors,
                structure_results,
                ..Default::default()
            };
            let extracted = page_of_prose();
            let mut a = fluree_doc_pdf::document::analyze_with(
                &mut extracted.clone(),
                &[],
                &cfg.options_for("doc"),
            );
            a.elements = an_anchored_grid();
            let mut asked = Vec::new();
            let mut pages = Vec::new();
            read_in_run(&mut a, &extracted, &cfg, "doc", &[], None, |jobs, _| {
                pages = jobs.iter().map(|(page, _)| *page).collect();
                let mut readings = fluree_doc_pdf::escalate::Readings::default();
                for (page, regions) in &jobs {
                    for (tag, _) in regions.iter().flatten() {
                        let crop = format!("p{page}_{tag}");
                        readings.insert(crop.clone(), DEEP_READING.into());
                        asked.push(crop);
                    }
                }
                Ok(readings)
            })
            .expect("a stand-in reader does not fail");
            (pages, asked, a.elements)
        };
        let (pages, asked, unopposed) = read_live(None, false);
        assert_eq!(pages, [0]);
        assert_eq!(asked, ["p0_t0"]);
        assert_eq!(unopposed.len(), 1);
        assert_eq!(unopposed[0].provenance, "vlm", "the reading alone wins");
        for emit_anchors in [false, true] {
            let (pages, asked, vetoed) = read_live(Some(structure.0.clone()), emit_anchors);
            assert!(asked.is_empty(), "a vetoed table is not paid for");
            assert!(
                pages.is_empty(),
                "nor is the page it leaves with nothing to read"
            );
            assert_eq!(vetoed.len(), 1, "{emit_anchors}: {vetoed:?}");
            assert_eq!(vetoed[0].provenance, "rust", "the veto keeps the grid");
        }
    }

    /// `triage` does not price a table the structure readings veto, as a
    /// live run does not read it.
    #[test]
    fn triage_does_not_price_a_vetoed_table() {
        let structure = corroborating_structure();
        let priced = |structure_results: Option<PathBuf>| {
            let cfg = TierConfig {
                escalate: true,
                structure_results,
                ..Default::default()
            };
            let extracted = page_of_prose();
            let mut a = fluree_doc_pdf::document::analyze_with(
                &mut extracted.clone(),
                &[],
                &cfg.options_for("doc"),
            );
            a.elements = an_anchored_grid();
            crate::commands::triage::priced(&extracted, &a, "doc", &cfg).0
        };
        assert_eq!(priced(None), 1);
        assert_eq!(priced(Some(structure.0.clone())), 0);
    }

    /// `dev render-routed` writes a table the structure readings veto, since
    /// the structure reader reads it too, and names it apart: less what it
    /// names, it writes the crops `triage` counts and a live run sends.
    #[test]
    fn render_routed_writes_a_vetoed_table_and_names_it() {
        let structure = corroborating_structure();
        let cfg = TierConfig {
            escalate: true,
            structure_results: Some(structure.0.clone()),
            ..Default::default()
        };
        let extracted = page_of_prose();
        let mut a = fluree_doc_pdf::document::analyze_with(
            &mut extracted.clone(),
            &[],
            &cfg.options_for("doc"),
        );
        a.elements = an_anchored_grid();
        let hints = crate::escalate::jobs::hints(&extracted, "doc", &cfg);
        let names = |jobs: &fluree_doc_pdf::escalate::CropJobs| -> Vec<String> {
            jobs.iter()
                .flat_map(|(page, regions)| {
                    regions
                        .iter()
                        .flatten()
                        .map(move |(tag, _)| format!("p{page}_{tag}"))
                })
                .collect()
        };
        let render = |structure_results| {
            crate::escalate::jobs::to_render(&extracted, &a, "doc", &hints, structure_results)
        };
        let (written, _, vetoed) = render(cfg.structure_results.as_deref());
        assert_eq!(
            names(&written),
            ["p0_t0"],
            "written for the structure reader"
        );
        assert_eq!(vetoed, ["p0_t0"]);
        let structure = common::structure_backend(cfg.structure_results.as_deref());
        let (sent, _) = crate::escalate::jobs::at_anchors(
            &extracted,
            &a,
            "doc",
            &hints,
            structure.as_ref().map(|s| s as _),
        );
        let left: Vec<String> = names(&written)
            .into_iter()
            .filter(|n| !vetoed.contains(n))
            .collect();
        assert_eq!(left, names(&sent));
        assert_eq!(
            crate::commands::triage::priced(&extracted, &a, "doc", &cfg).0,
            left.len()
        );
        let (written, _, vetoed) = render(None);
        assert_eq!(names(&written), ["p0_t0"]);
        assert!(
            vetoed.is_empty(),
            "nothing vetoes without structure readings"
        );
    }

    /// `--tier-results` with `--structure-results`: the sidecar reading of a
    /// table answers to the same veto.
    #[test]
    fn a_sidecar_table_reading_answers_to_the_structure_veto() {
        let tiers = Sidecars::holding("tiers", DEEP_READING);
        let structure = corroborating_structure();
        let splice = |structure_results: Option<&std::path::Path>| {
            let mut elements = an_anchored_grid();
            common::apply_tiers(
                Some(&tiers.0),
                structure_results,
                false,
                "doc",
                &mut elements,
                &[],
                &[],
            );
            elements
        };
        let unopposed = splice(None);
        assert_eq!(unopposed.len(), 1);
        assert_eq!(unopposed[0].provenance, "vlm", "the reading alone wins");
        let vetoed = splice(Some(&structure.0));
        assert_eq!(vetoed.len(), 1);
        assert_eq!(vetoed[0].provenance, "rust", "the veto keeps the grid");
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
