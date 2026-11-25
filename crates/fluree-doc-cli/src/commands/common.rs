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
    Transcript,
    Email,
    Image,
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
    (SourceKind::Transcript, &["vtt", "srt"]),
    (SourceKind::Email, &["eml", "msg"]),
    (SourceKind::Image, fluree_doc_pdf::image::EXTENSIONS),
];

/// The reader a file's extension names, if it names one.
pub(crate) fn source_kind(path: &Path) -> Option<SourceKind> {
    let ext = path.extension()?.to_str()?;
    SOURCE_EXTENSIONS
        .iter()
        .find(|(_, exts)| exts.iter().any(|e| ext.eq_ignore_ascii_case(e)))
        .map(|(kind, _)| *kind)
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
    /// The file `config` was read from, when [`Self::configured`] found one.
    pub config_path: Option<PathBuf>,
    /// What [`COLUMNS_ENV`](crate::escalate::jobs::COLUMNS_ENV) decides about
    /// column doubt for this run, over `config`: `Some(false)` for `0`,
    /// `Some(true)` for any other value, `None` unset. Read once with the
    /// rest of the environment, so every command resolves column doubt from
    /// the same two inputs ([`crate::escalate::jobs::escalates_columns`]).
    pub columns: Option<bool>,
    pub verbose: bool,
    /// `FDOC_TABLE_CONF`: print each table's confidence signals.
    pub table_conf_debug: bool,
    /// `FDOC_INSERT_TABLES`: anchor the tables a layout detector found and
    /// no grid covered.
    pub insert_missing_tables: bool,
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
            config_path: None,
            columns: crate::escalate::jobs::columns_override(
                std::env::var_os(crate::escalate::jobs::COLUMNS_ENV).as_deref(),
            ),
            verbose: false,
            table_conf_debug: std::env::var_os("FDOC_TABLE_CONF").is_some(),
            insert_missing_tables: std::env::var_os("FDOC_INSERT_TABLES").is_some(),
        }
    }

    /// Environment config and the config file, for a command that calls no
    /// reader but chooses the crops one would be handed: what the file says
    /// about escalation (`escalation.on_column_doubt`) changes that set, so
    /// a command that left it out would count a set `convert --escalate`
    /// does not send. A file that cannot be read is warned about and left
    /// out, as `convert` leaves it out.
    pub fn configured() -> Self {
        let mut tiers = Self::from_env();
        match crate::config::load() {
            Ok(l) => {
                tiers.config_path = l.exists.then_some(l.path);
                tiers.config = l.config;
            }
            Err(e) => eprintln!("warning: {e}"),
        }
        tiers
    }

    /// Environment config with per-flag overrides on top.
    pub fn from_env_with(
        layout_boxes: Option<&Path>,
        tier_results: Option<&Path>,
        structure_results: Option<&Path>,
        ocr_words: Option<&Path>,
        emit_anchors: bool,
    ) -> Self {
        Self::from_env().with_flags(
            layout_boxes,
            tier_results,
            structure_results,
            ocr_words,
            emit_anchors,
        )
    }

    /// `self` with the flags of [`Self::from_env_with`] on top. What no flag
    /// overrides is kept, column doubt among it: `convert` takes it from the
    /// environment as `triage` and `render-routed` do.
    fn with_flags(
        self,
        layout_boxes: Option<&Path>,
        tier_results: Option<&Path>,
        structure_results: Option<&Path>,
        ocr_words: Option<&Path>,
        emit_anchors: bool,
    ) -> Self {
        Self {
            layout_boxes: layout_boxes.map(Path::to_path_buf).or(self.layout_boxes),
            tier_results: tier_results.map(Path::to_path_buf).or(self.tier_results),
            structure_results: structure_results
                .map(Path::to_path_buf)
                .or(self.structure_results),
            ocr_words: ocr_words.map(Path::to_path_buf).or(self.ocr_words),
            emit_anchors: emit_anchors || self.emit_anchors,
            ..self
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
            // Anchors whenever this run splices readings, from sidecars or
            // from a reader it calls itself: a region, table or inserted
            // table reading lands only at its anchor, and one with nowhere
            // to land was paid for and dropped. The splice removes every
            // anchor no reading answers, so none reaches the output unless
            // `--emit-anchors` asked for them.
            emit_anchors: self.emit_anchors || self.tier_results.is_some() || self.escalate,
            table_conf_debug: self.table_conf_debug,
            insert_missing_tables: self.insert_missing_tables,
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
        // Small regions too: an engine reads what a model call would not pay for.
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

/// Splice model-tier readings (VLM crops, and optionally table-structure
/// readings for the three-way veto) into the element stream.
pub(crate) fn apply_tiers(
    tier_results: Option<&Path>,
    structure_results: Option<&Path>,
    emit_anchors: bool,
    stem: &str,
    elements: &mut Vec<fluree_doc_pdf::document::Element>,
    pages: &[Vec<String>],
    furniture: &[(String, bool)],
) {
    let Some(dir) = tier_results else {
        return;
    };
    let vlm = fluree_doc_pdf::arbiter::FixtureBackend::new(dir.to_path_buf());
    let structure = structure_backend(structure_results);
    splice(
        emit_anchors,
        elements,
        stem,
        &vlm,
        structure.as_ref().map(|s| s as _),
        pages,
        furniture,
    );
}

/// The structure readings in `structure_results`, for the three-way veto.
/// One per run of a document, so the crops chosen and the splice read each
/// file once between them.
pub(crate) fn structure_backend(
    structure_results: Option<&Path>,
) -> Option<fluree_doc_pdf::arbiter::FixtureBackend> {
    structure_results.map(|d| fluree_doc_pdf::arbiter::FixtureBackend::new(d.to_path_buf()))
}

/// Splice readings into the element stream, from sidecars or from a reader
/// called in this run.
///
/// `structure` is read for the three-way veto whichever produced
/// `readings`: a table reading is as likely to be wrong from a model called
/// here as from a sidecar, and the structure reading is what overrules it.
///
/// An anchor no reading answers drops, unless the anchors were asked for
/// (`--emit-anchors`): then it stays where it is for a later tier to fill,
/// and only what was read in this run lands.
pub(crate) fn splice(
    emit_anchors: bool,
    elements: &mut Vec<fluree_doc_pdf::document::Element>,
    stem: &str,
    readings: &dyn fluree_doc_pdf::arbiter::TierBackend,
    structure: Option<&dyn fluree_doc_pdf::arbiter::TierBackend>,
    pages: &[Vec<String>],
    furniture: &[(String, bool)],
) {
    if emit_anchors {
        fluree_doc_pdf::arbiter::splice_leaving_unanswered(
            elements, stem, readings, structure, pages,
        );
    } else {
        fluree_doc_pdf::arbiter::splice_with_page(elements, stem, readings, structure, pages);
    }
    // The same furniture the deterministic pass already stripped, so a page
    // reads the same whether or not it escalated.
    fluree_doc_pdf::arbiter::scrub_furniture(elements, furniture);
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

    /// `convert`'s flags override the environment they name and keep the
    /// rest of it: `FDOC_ESCALATE_COLUMNS` decides column doubt for
    /// `convert` as for the commands that take no flag.
    #[test]
    fn the_flags_keep_what_the_environment_says_about_column_doubt() {
        for columns in [None, Some(false), Some(true)] {
            let env = TierConfig {
                tier_results: Some(PathBuf::from("env-readings")),
                columns,
                ..Default::default()
            };
            let cfg = env.with_flags(None, Some(Path::new("flag-readings")), None, None, true);
            assert_eq!(cfg.columns, columns);
            assert_eq!(cfg.tier_results, Some(PathBuf::from("flag-readings")));
            assert!(cfg.emit_anchors);
        }
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
