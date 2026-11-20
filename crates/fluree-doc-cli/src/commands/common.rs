//! Shared plumbing: input discovery, tier configuration, and the two
//! post-analysis passes (layout title promotion, model-tier splicing) used by
//! `convert` and the dev renderers alike.

use std::path::{Path, PathBuf};

/// Which reader a file goes to, by its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceKind {
    Pdf,
    Markdown,
    Text,
    Html,
    Docx,
    Pptx,
    Xlsx,
    /// A delimited file: CSV, or TSV by its extension.
    Csv,
    /// An Excel 97-2003 workbook (BIFF8).
    Xls,
    /// An XPS or OpenXPS document.
    Xps,
    /// A Word 97-2003 document.
    Doc,
    /// An OpenDocument text, spreadsheet or presentation.
    Odf,
    Transcript,
    Email,
    Image,
    /// A format recognised, by its extension or its content, that no reader
    /// reads yet ([`unsupported`]): refused by name rather than handed to the
    /// PDF reader, whose `parse: Invalid` would read as a corrupt PDF.
    Unsupported,
}

/// Every extension `convert` reads, by the reader it goes to. Conversion
/// dispatches on this table and directory discovery lists what it names, so
/// a file one accepts the other finds. Word, PowerPoint and Excel's
/// macro-enabled, template and show variants are the same package under
/// another name.
const SOURCE_EXTENSIONS: &[(SourceKind, &[&str])] = &[
    (SourceKind::Pdf, &["pdf"]),
    (SourceKind::Markdown, &["md", "markdown"]),
    (SourceKind::Text, &["txt", "text"]),
    (SourceKind::Html, &["html", "htm", "xhtml"]),
    (SourceKind::Docx, &["docx", "docm", "dotx", "dotm"]),
    (
        SourceKind::Pptx,
        &["pptx", "pptm", "potx", "potm", "ppsx", "ppsm"],
    ),
    (SourceKind::Xlsx, &["xlsx", "xlsm", "xltx", "xltm"]),
    (SourceKind::Csv, &["csv", "tsv"]),
    (SourceKind::Xls, &["xls", "xlt"]),
    (SourceKind::Xps, &["xps", "oxps"]),
    (SourceKind::Doc, &["doc", "dot"]),
    (SourceKind::Odf, &["odt", "ott", "ods", "ots", "odp", "otp"]),
    (SourceKind::Transcript, &["vtt", "srt"]),
    (SourceKind::Email, &["eml", "msg"]),
    (SourceKind::Image, fluree_doc_pdf::image::EXTENSIONS),
];

/// Document formats with no reader yet, by extension, with what each is.
///
/// Listed so that a file of one is refused by name, and counted as such in
/// a batch, rather than read as a PDF and failing as a corrupt one. A format
/// leaves this list when its reader lands.
const UNSUPPORTED: &[(&str, &str)] = &[
    ("xlsb", "Excel binary workbook"),
    ("ppt", "PowerPoint 97-2003 presentation"),
    ("pps", "PowerPoint 97-2003 slide show"),
    ("pot", "PowerPoint 97-2003 template"),
    ("rtf", "Rich Text Format document"),
    ("wpd", "WordPerfect document"),
    ("pages", "Apple Pages document"),
    ("numbers", "Apple Numbers spreadsheet"),
    ("key", "Apple Keynote presentation"),
];

/// The reader a file's extension names, if it names one; [`SourceKind::Unsupported`]
/// for a format no reader reads yet.
pub(crate) fn source_kind(path: &Path) -> Option<SourceKind> {
    let ext = path.extension()?.to_str()?;
    if UNSUPPORTED.iter().any(|(e, _)| ext.eq_ignore_ascii_case(e)) {
        return Some(SourceKind::Unsupported);
    }
    SOURCE_EXTENSIONS
        .iter()
        .find(|(_, exts)| exts.iter().any(|e| ext.eq_ignore_ascii_case(e)))
        .map(|(kind, _)| *kind)
}

/// An OLE compound file's signature: Word, Excel and PowerPoint 97-2003
/// files, and Outlook's `.msg`, all start with it.
const COMPOUND_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Why `convert` refuses a file it has no reader for, naming the format: by
/// its extension where that names one ([`UNSUPPORTED`]), else by its content.
/// `None` for a file some reader takes.
///
/// Only called for an input [`input_kind`] found unsupported, or for one read
/// as a PDF: a compound file that is not an Outlook message (the e-mail
/// reader takes those first) is an Office 97-2003 file, never a PDF.
pub(crate) fn unsupported(path: Option<&Path>, data: &[u8]) -> Option<String> {
    let ext = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .and_then(|ext| {
            UNSUPPORTED
                .iter()
                .find(|(e, _)| ext.eq_ignore_ascii_case(e))
        });
    match ext {
        Some((e, what)) => Some(format!(
            "unsupported format (.{e}, {what}): no reader for it yet"
        )),
        None if data.starts_with(&COMPOUND_MAGIC) => Some(
            "unsupported format (an OLE compound file that is no Word or Excel 97-2003 document \
             nor Outlook message, such as a PowerPoint 97-2003 file): no reader for it yet"
                .into(),
        ),
        None => None,
    }
}

/// Whether `data` is a PDF by its header, which a reader finds within the
/// first kilobyte.
fn is_pdf(data: &[u8]) -> bool {
    data[..data.len().min(1024)]
        .windows(5)
        .any(|w| w == b"%PDF-")
}

/// The reader a file goes to, by its content where that says, else by its
/// extension: what `convert` reads it as, and `render-routed` crops it as.
///
/// A transcript, an email and an Excel 97-2003 workbook by their content
/// first: `.vtt` has no
/// registered type on most systems, and a saved message is `.eml`, `.mht`,
/// `.txt` or nothing at all, so either arrives renamed as often as not. Then
/// the formats an extension names (Markdown, plain text, HTML and the Office
/// packages); then an image by its first bytes. A format no reader reads yet,
/// by its extension (unless its content is a PDF or an image) or as a
/// compound file that is not a message, is [`SourceKind::Unsupported`];
/// anything else is read as a PDF.
pub(crate) fn input_kind(path: &Path, data: &[u8]) -> SourceKind {
    let kind = source_kind(path);
    if fluree_doc_transcript::Format::sniff(data).is_some() || kind == Some(SourceKind::Transcript)
    {
        return SourceKind::Transcript;
    }
    if fluree_doc_email::Format::sniff(data).is_some() || kind == Some(SourceKind::Email) {
        return SourceKind::Email;
    }
    // An Excel 97-2003 workbook by its content: a compound file holding a
    // workbook stream, whatever its name.
    if data.starts_with(&COMPOUND_MAGIC) && fluree_doc_xls::is_xls(data) {
        return SourceKind::Xls;
    }
    // An OpenDocument file by its `mimetype`, whatever its name.
    if fluree_doc_odf::Format::sniff(data).is_some() {
        return SourceKind::Odf;
    }
    // And a Word 97-2003 document: one holding a WordDocument stream.
    if data.starts_with(&COMPOUND_MAGIC) && fluree_doc_doc::is_doc(data) {
        return SourceKind::Doc;
    }
    match kind {
        Some(
            k @ (SourceKind::Markdown
            | SourceKind::Text
            | SourceKind::Html
            | SourceKind::Docx
            | SourceKind::Pptx
            | SourceKind::Xlsx
            | SourceKind::Csv
            | SourceKind::Xps
            | SourceKind::Doc
            | SourceKind::Odf),
        ) => k,
        _ if fluree_doc_pdf::image::Format::sniff(data).is_some() => SourceKind::Image,
        _ if is_pdf(data) => SourceKind::Pdf,
        Some(SourceKind::Unsupported) => SourceKind::Unsupported,
        _ if data.starts_with(&COMPOUND_MAGIC) => SourceKind::Unsupported,
        _ => SourceKind::Pdf,
    }
}

/// The files in a directory `convert` reads, in name order.
pub(crate) fn sources_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && source_kind(p).is_some())
        .collect();
    v.sort();
    v
}

pub(crate) fn stem_of(pdf: &Path) -> &str {
    pdf.file_stem().and_then(|x| x.to_str()).unwrap_or("doc")
}

/// The crop render scale, the library's: the crops `render-routed`
/// writes and the ones `convert --escalate` sends are the same pixels.
pub(crate) use fluree_doc_pdf::escalate::VLM_RENDER_SCALE;

/// An opened OCR engine, shared by every document of a run.
#[derive(Clone)]
pub(crate) struct OcrHandle(pub std::sync::Arc<dyn fluree_doc_vision::OcrEngine>);

impl std::fmt::Debug for OcrHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OcrHandle({})", self.0.name())
    }
}

/// Where the optional model tiers read their sidecar files from. Flags
/// override the corresponding `FDOC_*` environment variables; the environment
/// remains so benchmark harnesses can configure tiers without touching the
/// adapter command line.
#[derive(Debug, Clone, Default)]
pub(crate) struct TierConfig {
    pub layout_boxes: Option<PathBuf>,
    pub tier_results: Option<PathBuf>,
    pub structure_results: Option<PathBuf>,
    /// OCR word readings, added as glyphs before analysis.
    pub ocr_words: Option<PathBuf>,
    /// An OCR engine reading routed pixels in this run, opened once.
    pub ocr: Option<OcrHandle>,
    pub emit_anchors: bool,
    /// Call the configured reader during this run.
    pub escalate: bool,
    /// What the config file says, loaded once per invocation rather than per
    /// document — a batch of a thousand files must not read it a thousand
    /// times.
    pub config: crate::config::Config,
    pub verbose: bool,
}

impl TierConfig {
    pub fn from_env() -> Self {
        Self {
            layout_boxes: std::env::var_os("FDOC_TITLE_BOXES").map(PathBuf::from),
            tier_results: std::env::var_os("FDOC_TIER_RESULTS").map(PathBuf::from),
            structure_results: std::env::var_os("FDOC_STRUCTURE_RESULTS").map(PathBuf::from),
            ocr_words: std::env::var_os("FDOC_OCR_WORDS").map(PathBuf::from),
            ocr: None,
            emit_anchors: std::env::var_os("FDOC_VLM_ANCHORS").is_some(),
            escalate: false,
            config: crate::config::Config::default(),
            verbose: false,
        }
    }

    /// Environment config with per-flag overrides on top.
    pub fn from_env_with(
        layout_boxes: Option<&Path>,
        tier_results: Option<&Path>,
        structure_results: Option<&Path>,
        ocr_words: Option<&Path>,
        emit_anchors: bool,
    ) -> Self {
        let env = Self::from_env();
        Self {
            layout_boxes: layout_boxes.map(Path::to_path_buf).or(env.layout_boxes),
            tier_results: tier_results.map(Path::to_path_buf).or(env.tier_results),
            structure_results: structure_results
                .map(Path::to_path_buf)
                .or(env.structure_results),
            ocr_words: ocr_words.map(Path::to_path_buf).or(env.ocr_words),
            ocr: None,
            emit_anchors: emit_anchors || env.emit_anchors,
            escalate: env.escalate,
            config: env.config,
            verbose: env.verbose,
        }
    }

    /// Decide whether this run calls a reader, and say so when the answer is
    /// a surprise.
    ///
    /// A configured provider escalates by default, because configuring one is
    /// itself the decision to use it. Asking for escalation without a
    /// provider is not an error — the deterministic parse is still the right
    /// output — but it must not be silent, or a batch run under a wrong
    /// assumption looks like a working one.
    pub fn resolve_escalation(&mut self, on: bool, off: bool, quiet: bool) {
        let loaded = match crate::config::load() {
            Ok(l) => l,
            Err(e) => {
                eprintln!("warning: {e}");
                eprintln!("         parsing deterministically");
                return;
            }
        };
        self.config = loaded.config;
        let ready = self.config.reader_is_configured();
        self.escalate = !off && (on || (ready && self.config.escalation.enabled));
        if self.escalate && !ready {
            let why = self
                .config
                .missing()
                .unwrap_or_else(|| "no reader is configured".into());
            eprintln!("warning: --escalate was asked for but {why}");
            eprintln!("         run `fdoc config gemini --credentials <key.json>` to set one up");
            eprintln!("         parsing deterministically");
            self.escalate = false;
        }
        if self.escalate && self.tier_results.is_some() && !quiet {
            eprintln!("note: --tier-results supplies the readings; not calling the model");
        }
    }

    pub fn options_for(&self, stem: &str) -> fluree_doc_pdf::document::AnalyzeOptions {
        fluree_doc_pdf::document::AnalyzeOptions {
            layout_prefix: self.layout_boxes.as_ref().map(|d| d.join(stem)),
            emit_anchors: self.emit_anchors || self.tier_results.is_some(),
            table_conf_debug: std::env::var_os("FDOC_TABLE_CONF").is_some(),
            insert_missing_tables: std::env::var_os("FDOC_INSERT_TABLES").is_some(),
        }
    }
}

/// Build core options from the CLI's environment variables.
pub(crate) fn opts_for(pdf: &Path) -> fluree_doc_pdf::document::AnalyzeOptions {
    TierConfig::from_env().options_for(stem_of(pdf))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LayoutRole {
    DocumentTitle,
    FloatTitle,
    FloatBody,
    Ignore,
}

fn layout_role(label: &str) -> LayoutRole {
    match label {
        "paragraph_title" | "doc_title" | "title" => LayoutRole::DocumentTitle,
        "figure_title" | "chart_title" | "table_title" => LayoutRole::FloatTitle,
        "figure_caption" | "chart" | "image" | "figure" => LayoutRole::FloatBody,
        _ => LayoutRole::Ignore,
    }
}

fn box_covers(
    element: fluree_doc_model::geom::BBox,
    &(x0, y0, x1, y1): &(f64, f64, f64, f64),
) -> bool {
    let area = ((element.x1 - element.x0) * (element.y1 - element.y0)).max(1.0);
    let ix = (element.x1.min(x1) - element.x0.max(x0)).max(0.0);
    let iy = (element.y1.min(y1) - element.y0.max(y0)).max(0.0);
    ix * iy >= 0.5 * area
}

fn explicit_float_caption(text: &str) -> bool {
    let Some(first) = text.split_whitespace().next() else {
        return false;
    };
    matches!(
        first
            .trim_matches(|c: char| !c.is_ascii_alphabetic())
            .to_ascii_lowercase()
            .as_str(),
        "figure" | "fig" | "table" | "chart"
    )
}

/// Arbitrate headings where a layout detector saw a title or float.
///
/// `dir` holds `<stem>_p<N>_page.json` files — layout-detector boxes over 2x
/// page renders. Document-title boxes promote short prose blocks. Float-body
/// boxes demote non-outline headings. Float-title boxes demote explicit
/// `Figure`/`Table`/`Chart` captions, but otherwise defer to the deterministic
/// reading because the benchmark treats some descriptive float titles as
/// document headings.
pub(crate) fn arbitrate_layout_titles(
    dir: Option<&Path>,
    stem: &str,
    elements: &mut [fluree_doc_pdf::document::Element],
) {
    let Some(dir) = dir else {
        return;
    };
    let mut boxes_per_page: std::collections::HashMap<usize, Vec<(f64, f64, f64, f64)>> =
        Default::default();
    let mut float_title_boxes: std::collections::HashMap<usize, Vec<(f64, f64, f64, f64)>> =
        Default::default();
    let mut caption_boxes: std::collections::HashMap<usize, Vec<(f64, f64, f64, f64)>> =
        Default::default();
    let mut pages_seen: std::collections::HashSet<usize> = Default::default();
    for e in elements.iter() {
        if !pages_seen.insert(e.page) {
            continue;
        }
        let f = dir.join(format!("{stem}_p{}_page.json", e.page));
        let Ok(txt) = std::fs::read_to_string(&f) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
            continue;
        };
        for b in v.as_array().into_iter().flatten() {
            let label = b["label"].as_str().unwrap_or("");
            let score = b["score"].as_f64().unwrap_or(0.0);
            let role = layout_role(label);
            if role == LayoutRole::Ignore || score < 0.6 {
                continue;
            }
            let c: Vec<f64> = b["box"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_f64())
                .collect();
            if c.len() == 4 {
                // Pixel space at 2x back to PDF units.
                let r = (c[0] / 2.0, c[1] / 2.0, c[2] / 2.0, c[3] / 2.0);
                match role {
                    LayoutRole::DocumentTitle => {
                        boxes_per_page.entry(e.page).or_default().push(r);
                    }
                    LayoutRole::FloatTitle => {
                        float_title_boxes.entry(e.page).or_default().push(r);
                    }
                    LayoutRole::FloatBody => {
                        caption_boxes.entry(e.page).or_default().push(r);
                    }
                    LayoutRole::Ignore => {}
                }
            }
        }
    }
    // Arbitrated demotion: a heading of ours sitting in a figure/chart body is
    // float content. Float titles are deliberately excluded: this corpus'
    // ground truth blesses some of them as document headings.
    for e in elements.iter_mut() {
        if e.kind != "doco:SectionTitle" || e.evidence == "outline" {
            continue;
        }
        let eb = e.rect();
        let in_caption = caption_boxes
            .get(&e.page)
            .is_some_and(|bs| bs.iter().any(|b| box_covers(eb, b)));
        let in_float_title = float_title_boxes
            .get(&e.page)
            .is_some_and(|bs| bs.iter().any(|b| box_covers(eb, b)));
        let in_title = boxes_per_page
            .get(&e.page)
            .is_some_and(|bs| bs.iter().any(|b| box_covers(eb, b)));
        if (in_caption || (in_float_title && explicit_float_caption(&e.text))) && !in_title {
            e.kind = "doco:Paragraph".to_string();
            e.level = None;
            e.evidence = "layout-caption";
        }
    }
    for e in elements.iter_mut() {
        if e.kind != "doco:Paragraph" && e.kind != "doc:ListItem" {
            continue;
        }
        if e.text.chars().count() > 120 {
            continue;
        }
        let Some(boxes) = boxes_per_page.get(&e.page) else {
            continue;
        };
        let eb = e.rect();
        for b in boxes {
            if box_covers(eb, b) {
                e.kind = "doco:SectionTitle".to_string();
                e.level = Some(2);
                e.evidence = "layout-title";
                break;
            }
        }
    }
}

/// The OCR word readings `dir` holds for one document: for each page, the
/// whole-page reading `{stem}_p{N}_full.json` and one `{stem}_p{N}_r{i}.json`
/// per region the router routes, in page and crop order.
///
/// The names are asked for, not searched: a batch shares one directory, and
/// listing it once per document would cost every document the whole batch.
/// Region readings are asked for only up to the page's routed regions; a
/// whole-page reading is asked for on every page, so one the router would
/// not send a reader is still found and refused aloud.
///
/// Unlike the model-tier sidecars, a file that does not parse is reported
/// (unless `--quiet`): a word reading is produced by the embedder's own
/// pipeline, and one it cannot read is a defect in it, not a partial batch.
/// Scores out of range are not: [`warn_scores_out_of_range`] says so of the
/// readings the caller takes, once it has chosen them.
pub(crate) fn ocr_readings(
    dir: &Path,
    stem: &str,
    doc: &fluree_doc_pdf::Document,
    quiet: bool,
) -> Vec<(
    usize,
    fluree_doc_pdf::ocr::Crop,
    fluree_doc_pdf::ocr::WordReading,
)> {
    use fluree_doc_pdf::ocr::{Crop, WordReading};
    let mut out = Vec::new();
    for p in &doc.pages {
        // The regions no model is sent too: an engine reads them all.
        let regions = fluree_doc_pdf::route::ocr_regions(p).len();
        let crops = std::iter::once(Crop::Page).chain((0..regions).map(Crop::Region));
        for crop in crops {
            let name = format!("{stem}_p{}_{}.json", p.index, crop.tag());
            let path = dir.join(&name);
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    if !quiet {
                        eprintln!("warning: --ocr-words: {name}: {e}");
                    }
                    continue;
                }
            };
            match WordReading::from_json(&text) {
                Ok(r) => out.push((p.index, crop, r)),
                Err(e) => {
                    if !quiet {
                        eprintln!("warning: --ocr-words: {name}: {e}");
                    }
                }
            }
        }
    }
    out
}

/// Say (unless `--quiet`) that `name` reports no confidence, `n` of its
/// `words` scores being outside 0-1
/// ([`WordReading::scores_out_of_range`](fluree_doc_pdf::ocr::WordReading::scores_out_of_range)).
/// The sidecar's `conf` is 0-1: a reading scored 0-100, as Tesseract
/// scores, would otherwise lose its confidence without a word of why, and
/// with it the noise rule's judgement by confidence of a reading of a
/// region or of a picture
/// ([`MIN_PICTURE_CONFIDENCE`](fluree_doc_pdf::ocr::MIN_PICTURE_CONFIDENCE)):
/// only its boxes are judged, as those of a reading scored not at all are.
/// A reading of a whole page, or of an image read as a file, is not judged
/// noise at all.
pub(crate) fn warn_scores_out_of_range(name: &str, n: usize, words: usize, quiet: bool) {
    if n > 0 && !quiet {
        eprintln!(
            "warning: --ocr-words: {name}: {n} of {words} word score(s) outside 0-1, so no confidence is reported for it, nor, for a region or a picture, is it judged noise by its confidence (`conf` is 0-1)"
        );
    }
}

/// Splice model-tier readings (VLM crops, and optionally table-structure
/// readings for the three-way veto) into the element stream. The readings
/// of the region crops named in `screens` ([`pasted_regions`]) are a
/// screen's, their headings read as paragraphs ([`screen_blocks`]).
pub(crate) fn apply_tiers(
    tier_results: Option<&Path>,
    structure_results: Option<&Path>,
    stem: &str,
    elements: &mut Vec<fluree_doc_pdf::document::Element>,
    pages: &[Vec<String>],
    furniture: &[(String, bool)],
    screens: &std::collections::HashSet<String>,
) {
    let Some(dir) = tier_results else {
        return;
    };
    let vlm = fluree_doc_pdf::arbiter::FixtureBackend::new(dir.to_path_buf());
    let structure =
        structure_results.map(|d| fluree_doc_pdf::arbiter::FixtureBackend::new(d.to_path_buf()));
    splice_readings(
        elements,
        stem,
        &vlm,
        structure
            .as_ref()
            .map(|s| s as &dyn fluree_doc_pdf::arbiter::TierBackend),
        pages,
        furniture,
        screens,
    );
}

/// Splice model readings, supplied or read by the configured model, into
/// the element stream: those of the region crops named in `screens`
/// ([`pasted_regions`]) are a screen's, their headings read as paragraphs
/// ([`screen_blocks`]).
pub(crate) fn splice_readings(
    elements: &mut Vec<fluree_doc_pdf::document::Element>,
    stem: &str,
    readings: &dyn fluree_doc_pdf::arbiter::TierBackend,
    structure: Option<&dyn fluree_doc_pdf::arbiter::TierBackend>,
    pages: &[Vec<String>],
    furniture: &[(String, bool)],
    screens: &std::collections::HashSet<String>,
) {
    let readings = Screens {
        inner: readings,
        crops: screens,
    };
    fluree_doc_pdf::arbiter::splice_with_page(elements, stem, &readings, structure, pages);
    // The same furniture the deterministic pass already stripped, so a page
    // reads the same whether or not it escalated.
    fluree_doc_pdf::arbiter::scrub_furniture(elements, furniture);
}

/// A model's reading of a screen pasted into a document (a picture a DOCX,
/// an XLSX or an e-mail places, or a region of a PDF's page of text), with
/// its headings read as paragraphs: they
/// are the screen's, not the document's, and as headings they would put the
/// document's own text under them. A title block is read as text, and a
/// line marked up as a Markdown heading loses its marks; a table is left as
/// it is.
pub(crate) fn screen_blocks(
    blocks: Vec<fluree_doc_pdf::arbiter::Block>,
) -> Vec<fluree_doc_pdf::arbiter::Block> {
    blocks
        .into_iter()
        .map(|mut b| {
            if matches!(b.label.as_str(), "title" | "doc_title" | "paragraph_title") {
                b.label = "text".into();
            }
            if !b.content.trim_start().starts_with("<table") {
                b.content = b
                    .content
                    .lines()
                    .map(|line| {
                        let t = line.trim_start();
                        let marks = t.len() - t.trim_start_matches('#').len();
                        let rest = &t[marks..];
                        if (1..=6).contains(&marks) && rest.starts_with([' ', '\t']) {
                            rest.trim_start()
                        } else {
                            line
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            b
        })
        .collect()
}

/// The crops a screen pasted into a page fills, whose readings are read as
/// a screen's: the region crops (`p<N>_r<i>`) of the anchors in `elements`
/// a pasted region fills ([`fluree_doc_pdf::route::pasted`]), the rule
/// OCR's reading of it follows ([`fluree_doc_pdf::ocr::mark_provenance`]).
/// A region that, with the images it abuts, covers half its page or more is
/// the page's content, a scan under a stamped line the text layer carries,
/// whole or stored in strips, and keeps its headings.
///
/// A page read whole (`p<N>_full`) keeps its headings, even one near blank
/// but for such a screen, where OCR demotes those whose words land in the
/// screen: a model's reading has no positions to tell the screen's title
/// from the page's own, a cover's drawn in outlines or a caption that
/// repeats the screen's name.
pub(crate) fn pasted_regions(
    elements: &[fluree_doc_pdf::document::Element],
    pages: &[fluree_doc_pdf::Page],
) -> std::collections::HashSet<String> {
    // The anchors by page, each judged with its page's other anchors
    // ([`fluree_doc_pdf::route::pasted_set`]): the page's pictures are
    // measured once.
    let mut by_page: Vec<(
        &fluree_doc_pdf::Page,
        Vec<String>,
        Vec<fluree_doc_pdf::BBox>,
    )> = Vec::new();
    for e in elements {
        let Some(crop) = (e.text.strip_prefix("[[VLM:")).and_then(|t| t.strip_suffix("]]")) else {
            continue;
        };
        let (Some(b), Some(page)) = (e.bbox, pages.iter().find(|p| p.index == e.page)) else {
            continue;
        };
        let at = match by_page.iter().position(|(p, ..)| p.index == page.index) {
            Some(at) => at,
            None => {
                by_page.push((page, Vec::new(), Vec::new()));
                by_page.len() - 1
            }
        };
        // An element's box is in the display frame, the page's images in
        // its reading frame.
        by_page[at].1.push(crop.replace(':', "_"));
        by_page[at].2.push(page.from_display(b));
    }
    by_page
        .into_iter()
        .flat_map(|(page, crops, boxes)| {
            let pasted = fluree_doc_pdf::route::pasted_set(page, &boxes);
            crops
                .into_iter()
                .zip(pasted)
                .filter_map(|(c, p)| p.then_some(c))
        })
        .collect()
}

/// Model readings whose crops named in `crops` are read as a screen's
/// ([`screen_blocks`]).
struct Screens<'a> {
    inner: &'a dyn fluree_doc_pdf::arbiter::TierBackend,
    crops: &'a std::collections::HashSet<String>,
}

impl fluree_doc_pdf::arbiter::TierBackend for Screens<'_> {
    fn read(&self, stem: &str, crop: &str) -> Option<Vec<fluree_doc_pdf::arbiter::Block>> {
        let blocks = self.inner.read(stem, crop)?;
        Some(if self.crops.contains(crop) {
            screen_blocks(blocks)
        } else {
            blocks
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluree_doc_model::{element::Element, geom::BBox};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn element(kind: &str, text: &str, bbox: BBox) -> Element {
        Element {
            bbox: Some(bbox),
            level: (kind == "doco:SectionTitle").then_some(2),
            ..Element::new(kind, text, "rust", "font-size")
        }
    }

    #[test]
    fn a_screens_headings_are_read_as_paragraphs() {
        let block = |label: &str, content: &str| fluree_doc_pdf::arbiter::Block {
            label: label.into(),
            content: content.into(),
        };
        let got = screen_blocks(vec![
            block("doc_title", "SCREENSHOT"),
            block("text", "## Balance 9,876.54\n#7781 opened\n####### not one"),
            block("table", "<table><tr><td># kept</td></tr></table>"),
        ]);
        let got: Vec<(&str, &str)> = got
            .iter()
            .map(|b| (b.label.as_str(), b.content.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("text", "SCREENSHOT"),
                ("text", "Balance 9,876.54\n#7781 opened\n####### not one"),
                ("table", "<table><tr><td># kept</td></tr></table>"),
            ]
        );
    }

    #[test]
    fn a_reading_of_a_pasted_region_is_spliced_as_a_screens() {
        // The configured model's readings and those supplied go through
        // the same splice: a region crop named a screen loses its heading,
        // another keeps it.
        struct Titled;
        impl fluree_doc_pdf::arbiter::TierBackend for Titled {
            fn read(&self, _: &str, crop: &str) -> Option<Vec<fluree_doc_pdf::arbiter::Block>> {
                // Region crops only: a whole page's reading would replace it.
                crop.contains("_r").then(|| {
                    vec![fluree_doc_pdf::arbiter::Block {
                        label: "doc_title".into(),
                        content: format!("Balance of {crop}"),
                    }]
                })
            }
        }
        let anchor = |region: &str| {
            Element::new(
                "doco:Paragraph",
                format!("[[VLM:p0:{region}]]"),
                "rust",
                "region-anchor",
            )
        };
        let mut elements = vec![anchor("r0"), anchor("r1")];
        let screens = std::collections::HashSet::from(["p0_r0".to_string()]);
        splice_readings(&mut elements, "doc", &Titled, None, &[], &[], &screens);
        let texts: Vec<&str> = elements.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["Balance of p0_r0", "# Balance of p0_r1"]);
    }

    #[test]
    fn a_format_with_no_reader_is_refused_by_name_not_read_as_a_pdf() {
        let compound = [&COMPOUND_MAGIC[..], &[0u8; 504]].concat();
        let zip = b"PK\x03\x04 an Apple package".to_vec();
        for (name, data) in [
            ("letter.pot", &compound),
            ("Letter.PPT", &compound),
            ("slides.key", &zip),
            ("notes.rtf", &b"{\\rtf1 text}".to_vec()),
        ] {
            let path = Path::new(name);
            assert_eq!(input_kind(path, data), SourceKind::Unsupported, "{name}");
            let why = unsupported(Some(path), data).unwrap();
            let ext = path.extension().unwrap().to_str().unwrap().to_lowercase();
            assert!(
                why.starts_with(&format!("unsupported format (.{ext}, ")),
                "{why}"
            );
        }
        // Discovery lists them, so a batch counts them.
        assert_eq!(
            source_kind(Path::new("a.wpd")),
            Some(SourceKind::Unsupported)
        );
        // A compound file by its content alone, renamed or from stdin.
        assert_eq!(
            input_kind(Path::new("scan"), &compound),
            SourceKind::Unsupported
        );
        assert!(unsupported(None, &compound)
            .unwrap()
            .contains("compound file"));
        // Content that is a PDF or an image is read as one, whatever its name.
        let pdf = b"%PDF-1.4\n%junk".to_vec();
        assert_eq!(input_kind(Path::new("x.rtf"), &pdf), SourceKind::Pdf);
        assert_eq!(input_kind(Path::new("x.dat"), &pdf), SourceKind::Pdf);
        assert_eq!(unsupported(None, &pdf), None);
        let png = b"\x89PNG\r\n\x1a\n....".to_vec();
        assert_eq!(input_kind(Path::new("x.wpd"), &png), SourceKind::Image);
        // Anything else still goes to the PDF reader, as before.
        assert_eq!(input_kind(Path::new("x.bin"), b"junk"), SourceKind::Pdf);
    }

    #[test]
    fn float_titles_are_neutral_but_float_bodies_demote() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("fdoc-layout-{nonce}"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("sample_p0_page.json"),
            r#"[
                {"label":"figure_title","score":0.9,"box":[0,0,200,20]},
                {"label":"figure_title","score":0.9,"box":[0,40,200,60]},
                {"label":"chart","score":0.9,"box":[0,80,200,100]}
            ]"#,
        )
        .unwrap();
        let mut elements = vec![
            element(
                "doco:SectionTitle",
                "Performance details",
                BBox {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 100.0,
                    y1: 10.0,
                },
            ),
            element(
                "doco:SectionTitle",
                "Figure 4.5. Fuel mix",
                BBox {
                    x0: 0.0,
                    y0: 20.0,
                    x1: 100.0,
                    y1: 30.0,
                },
            ),
            element(
                "doco:SectionTitle",
                "OCR-Recall",
                BBox {
                    x0: 0.0,
                    y0: 40.0,
                    x1: 100.0,
                    y1: 50.0,
                },
            ),
        ];

        arbitrate_layout_titles(Some(&dir), "sample", &mut elements);

        assert_eq!(elements[0].kind, "doco:SectionTitle");
        assert_eq!(elements[1].kind, "doco:Paragraph");
        assert_eq!(elements[1].evidence, "layout-caption");
        assert_eq!(elements[2].kind, "doco:Paragraph");
        assert_eq!(elements[2].evidence, "layout-caption");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
