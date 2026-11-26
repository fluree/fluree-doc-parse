//! Shared plumbing: input discovery, the run's configuration and mode, and
//! the model-tier splice used by `convert` and the dev renderers alike.

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
    pub tier_results: Option<PathBuf>,
    pub structure_results: Option<PathBuf>,
    /// OCR word readings, added as glyphs before analysis.
    pub ocr_words: Option<PathBuf>,
    /// An OCR engine reading routed pixels in this run, opened once.
    pub ocr: Option<OcrHandle>,
    pub emit_anchors: bool,
    /// Call the configured reader during this run.
    pub escalate: bool,
    /// The mode this run reads in ([`Self::resolve_mode`]).
    pub mode: crate::mode::Mode,
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
}

impl TierConfig {
    pub fn from_env() -> Self {
        Self {
            tier_results: std::env::var_os("FDOC_TIER_RESULTS").map(PathBuf::from),
            structure_results: std::env::var_os("FDOC_STRUCTURE_RESULTS").map(PathBuf::from),
            ocr_words: std::env::var_os("FDOC_OCR_WORDS").map(PathBuf::from),
            ocr: None,
            emit_anchors: std::env::var_os("FDOC_VLM_ANCHORS").is_some(),
            escalate: false,
            mode: crate::mode::Mode::Deterministic,
            config: crate::config::Config::default(),
            config_path: None,
            columns: crate::escalate::jobs::columns_override(
                std::env::var_os(crate::escalate::jobs::COLUMNS_ENV).as_deref(),
            ),
            verbose: false,
            table_conf_debug: std::env::var_os("FDOC_TABLE_CONF").is_some(),
        }
    }

    /// Environment config with per-flag overrides on top.
    pub fn from_env_with(
        tier_results: Option<&Path>,
        structure_results: Option<&Path>,
        ocr_words: Option<&Path>,
        emit_anchors: bool,
    ) -> Self {
        Self::from_env().with_flags(tier_results, structure_results, ocr_words, emit_anchors)
    }

    /// `self` with the flags of [`Self::from_env_with`] on top. What no flag
    /// overrides is kept, column doubt among it: `convert` takes it from the
    /// environment as `triage` and `render-routed` do.
    fn with_flags(
        self,
        tier_results: Option<&Path>,
        structure_results: Option<&Path>,
        ocr_words: Option<&Path>,
        emit_anchors: bool,
    ) -> Self {
        Self {
            tier_results: tier_results.map(Path::to_path_buf).or(self.tier_results),
            structure_results: structure_results
                .map(Path::to_path_buf)
                .or(self.structure_results),
            ocr_words: ocr_words.map(Path::to_path_buf).or(self.ocr_words),
            emit_anchors: emit_anchors || self.emit_anchors,
            ..self
        }
    }

    /// Decide the run's mode ([`crate::mode::resolve`]) from its flags, the
    /// environment and the config, once per invocation, and set what follows
    /// from it: whether a model is called, and which readings from files are
    /// used. Says its notes once, and with `verbose` the mode; an unmet need
    /// is the error returned.
    pub fn resolve_mode(
        &mut self,
        flags: &ModeFlags<'_>,
        verbose: bool,
        quiet: bool,
    ) -> Result<crate::mode::Resolved, String> {
        use crate::mode::{Given, Inputs, Mode};
        match crate::config::load() {
            Ok(l) => {
                self.config_path = l.exists.then_some(l.path);
                self.config = l.config;
            }
            Err(e) => eprintln!("warning: {e}"),
        }
        let given = |flag: bool, env: bool| match (flag, env) {
            (true, _) => Given::Flag,
            (false, true) => Given::Env,
            _ => Given::No,
        };
        let set = |var: &str| std::env::var_os(var).is_some_and(|v| !v.is_empty());
        let inputs = Inputs {
            flag: flags.mode,
            env: std::env::var("FDOC_MODE").ok(),
            config: self.config.mode,
            escalate: flags.escalate,
            no_escalate: flags.no_escalate,
            ocr: given(flags.ocr, set("FDOC_OCR")),
            ocr_words: given(flags.ocr_words, self.ocr_words.is_some()),
            tier_results: given(flags.tier_results, self.tier_results.is_some()),
            emit_anchors: given(flags.emit_anchors, self.emit_anchors),
            assets: flags.plans_only
                || crate::commands::ocr::models_dir(flags.models, &self.config.ocr).is_some(),
            provider: flags.plans_only || self.config.reader_is_configured(),
            legacy_ocr: self.config.ocr.engine.is_some(),
            legacy_vlm: self.config.escalation.enabled,
        };
        let r = crate::mode::resolve(&inputs)?;
        // Readings from files a variable named, under a flag's lower mode,
        // are not used: [`crate::mode::resolve`] has said so.
        if r.mode < Mode::Vlm {
            if !flags.tier_results {
                self.tier_results = None;
            }
            if !flags.emit_anchors {
                self.emit_anchors = false;
            }
        }
        if r.mode < Mode::Ocr && !flags.ocr_words {
            self.ocr_words = None;
        }
        self.mode = r.mode;
        self.escalate = r.call_model;
        if !quiet {
            for note in &r.notes {
                eprintln!("{note}");
            }
            if r.mode == Mode::Vlm && self.tier_results.is_some() && inputs.provider {
                eprintln!("note: --tier-results supplies the readings; not calling the model");
            }
        }
        if verbose {
            eprintln!("mode: {} (from {})", r.mode, r.source);
        }
        Ok(r)
    }

    pub fn options_for(&self) -> fluree_doc_pdf::document::AnalyzeOptions {
        fluree_doc_pdf::document::AnalyzeOptions {
            // Anchors whenever this run splices readings, from sidecars or
            // from a reader it calls itself: a region or table reading lands
            // only at its anchor, and one with nowhere
            // to land was paid for and dropped. The splice removes every
            // anchor no reading answers, so none reaches the output unless
            // `--emit-anchors` asked for them.
            emit_anchors: self.emit_anchors || self.tier_results.is_some() || self.escalate,
            table_conf_debug: self.table_conf_debug,
        }
    }
}

/// The flags a command was given that bear on its mode
/// ([`TierConfig::resolve_mode`]); readings from files given by their
/// variables are read from the environment.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ModeFlags<'a> {
    pub mode: Option<crate::mode::Mode>,
    pub escalate: bool,
    pub no_escalate: bool,
    /// `--ocr` was given.
    pub ocr: bool,
    pub models: Option<&'a Path>,
    /// `--ocr-words` was given.
    pub ocr_words: bool,
    /// `--tier-results` was given.
    pub tier_results: bool,
    /// `--emit-anchors` was given.
    pub emit_anchors: bool,
    /// The command plans or renders crops for a reader, reading none itself
    /// (`triage`, `dev render-routed`): the mode is decided as `convert`
    /// decides it, but what reading needs is not asked of it.
    pub plans_only: bool,
}

/// Build core options from the CLI's environment variables.
pub(crate) fn opts_for() -> fluree_doc_pdf::document::AnalyzeOptions {
    TierConfig::from_env().options_for()
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
    // Numbered as the analysis's anchors and crops are.
    for p in &fluree_doc_pdf::document::routing_pages(doc) {
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
            let cfg = env.with_flags(Some(Path::new("flag-readings")), None, None, true);
            assert_eq!(cfg.columns, columns);
            assert_eq!(cfg.tier_results, Some(PathBuf::from("flag-readings")));
            assert!(cfg.emit_anchors);
        }
    }
}
