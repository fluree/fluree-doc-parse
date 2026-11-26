//! The mode a run reads in: what it may use beyond the document itself.
//!
//! Each mode builds on the one before:
//! - `deterministic`: the document alone; nothing leaves the machine.
//! - `ocr`: and the built-in OCR engine on the pages and regions whose text
//!   is pixels.
//! - `vlm`: and a model reader, for what OCR cannot do.
//!
//! The mode comes from `--mode`, else `FDOC_MODE`, else the config's `mode`,
//! else `deterministic`. A mode whose needs are not met is an error, never a
//! quieter run in a lower one: a batch under a wrong assumption must not
//! look like a working one. [`resolve`] decides it from every input at once,
//! so every command that runs the pipeline decides it alike.

use std::fmt;

/// What a run may use beyond the document.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// The document alone: no OCR, no model; nothing leaves the machine.
    Deterministic,
    /// And the built-in OCR engine on pages and regions whose text is pixels.
    Ocr,
    /// And a model reader, for what OCR cannot do.
    Vlm,
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Deterministic
    }
}

impl Mode {
    /// The mode a name gives: `deterministic`, `ocr` or `vlm`.
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "deterministic" => Some(Mode::Deterministic),
            "ocr" => Some(Mode::Ocr),
            "vlm" => Some(Mode::Vlm),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Deterministic => "deterministic",
            Mode::Ocr => "ocr",
            Mode::Vlm => "vlm",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Where the mode came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `--mode`, or a flag that asks for a mode (`--escalate`).
    Flag(&'static str),
    /// `FDOC_MODE`.
    Env,
    /// The config's `mode`.
    Config,
    /// None of them: `deterministic`.
    Default,
    /// Raised to by an input that needs it: `--ocr`, `--ocr-words`,
    /// `--tier-results`, `--emit-anchors` or their variables.
    Raised(&'static str),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Flag(name) => f.write_str(name),
            Source::Env => f.write_str("FDOC_MODE"),
            Source::Config => f.write_str("the config's `mode`"),
            Source::Default => f.write_str("the default"),
            Source::Raised(by) => write!(f, "{by}, which needs it"),
        }
    }
}

/// Whether an input was given, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Given {
    #[default]
    No,
    /// By its variable.
    Env,
    /// By its flag.
    Flag,
}

impl Given {
    pub fn any(self) -> bool {
        self != Given::No
    }
}

/// Everything the mode is decided from.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    /// `--mode`.
    pub flag: Option<Mode>,
    /// `FDOC_MODE`, as set; empty is unset.
    pub env: Option<String>,
    /// The config's `mode`.
    pub config: Option<Mode>,
    /// `--escalate`.
    pub escalate: bool,
    /// `--no-escalate`.
    pub no_escalate: bool,
    /// `--ocr <engine>` or `FDOC_OCR`.
    pub ocr: Given,
    /// `--ocr-words` or `FDOC_OCR_WORDS`: OCR readings from files, which
    /// stand in for the engine.
    pub ocr_words: Given,
    /// `--tier-results` or `FDOC_TIER_RESULTS`: model readings from files,
    /// which stand in for the model.
    pub tier_results: Given,
    /// `--emit-anchors` or `FDOC_VLM_ANCHORS`: anchors left for a model
    /// called elsewhere.
    pub emit_anchors: Given,
    /// An OCR assets directory is named (`--models`, `FDOC_OCR_MODELS`,
    /// `ocr.models`).
    pub assets: bool,
    /// The config sets up a model reader.
    pub provider: bool,
    /// The config names an OCR engine (`ocr.engine`), which once turned OCR
    /// on by itself.
    pub legacy_ocr: bool,
    /// The config's model reader is enabled (`escalation.enabled`), which
    /// once turned it on by itself.
    pub legacy_vlm: bool,
}

/// The mode decided, and what the run does in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub mode: Mode,
    pub source: Source,
    /// Call the configured model reader in this run: `vlm` with a reader,
    /// no readings from files, and no `--no-escalate`.
    pub call_model: bool,
    /// Lines to say once, before the first document.
    pub notes: Vec<String>,
}

/// Decide the mode from `i`, or say why it cannot be had.
pub fn resolve(i: &Inputs) -> Result<Resolved, String> {
    let env =
        match i.env.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => Some(Mode::parse(s).ok_or_else(|| {
                format!("FDOC_MODE={s:?} is not a mode: deterministic, ocr or vlm")
            })?),
            None => None,
        };
    if i.escalate && i.no_escalate {
        return Err("--escalate and --no-escalate contradict each other".into());
    }
    // Two flags that ask for different modes contradict each other; a flag
    // beats a variable or the config.
    if let Some(m) = i.flag {
        if i.no_escalate && m == Mode::Vlm {
            return Err("--no-escalate and --mode vlm contradict each other".into());
        }
        if i.escalate && m != Mode::Vlm {
            return Err(format!(
                "--escalate asks for vlm and --mode {m} for less: give one"
            ));
        }
        let needs = |given: Given, flag: &str, at: Mode| -> Result<(), String> {
            if given == Given::Flag && m < at {
                return Err(format!(
                    "{flag} needs --mode {at} or more, and --mode {m} was given"
                ));
            }
            Ok(())
        };
        needs(i.ocr, "--ocr", Mode::Ocr)?;
        needs(i.ocr_words, "--ocr-words", Mode::Ocr)?;
        needs(i.tier_results, "--tier-results", Mode::Vlm)?;
        needs(i.emit_anchors, "--emit-anchors", Mode::Vlm)?;
    }
    let (mut mode, mut source) = match (i.flag, i.escalate, env, i.config) {
        (Some(m), ..) => (m, Source::Flag("--mode")),
        (None, true, ..) => (Mode::Vlm, Source::Flag("--escalate")),
        (None, false, Some(m), _) => (m, Source::Env),
        (None, false, None, Some(m)) => (m, Source::Config),
        (None, false, None, None) => (Mode::Deterministic, Source::Default),
    };
    // Nothing chose a mode: a config written before modes may expect one.
    let unchosen = source == Source::Default;
    let mut notes = Vec::new();
    // What an input needs raises the mode to it. A variable's need yields
    // to a flag's mode, and says so.
    let flagged = matches!(source, Source::Flag(_));
    let raisers = [
        (i.ocr, Mode::Ocr, "--ocr", "FDOC_OCR"),
        (i.ocr_words, Mode::Ocr, "--ocr-words", "FDOC_OCR_WORDS"),
        (
            i.tier_results,
            Mode::Vlm,
            "--tier-results",
            "FDOC_TIER_RESULTS",
        ),
        (
            i.emit_anchors,
            Mode::Vlm,
            "--emit-anchors",
            "FDOC_VLM_ANCHORS",
        ),
    ];
    for (given, at, flag, var) in raisers {
        if !given.any() || mode >= at {
            continue;
        }
        if given == Given::Env && flagged {
            notes.push(format!(
                "note: {var} is set, but {source} asks for {mode}: {var} is not used"
            ));
            continue;
        }
        mode = at;
        source = Source::Raised(if given == Given::Flag { flag } else { var });
    }
    // `--no-escalate`: no model is called. Readings from files are no call,
    // and a mode they raise stays; one a variable or the config asked for
    // comes down to `ocr`.
    let sidecars = i.tier_results.any() || i.emit_anchors.any();
    if i.no_escalate && mode == Mode::Vlm && !sidecars {
        mode = Mode::Ocr;
        source = Source::Flag("--no-escalate");
    }
    if mode >= Mode::Ocr && !i.assets && !i.ocr_words.any() {
        let how = if mode == Mode::Ocr {
            format!("mode ocr (from {source})")
        } else {
            format!("mode vlm (from {source}) builds on ocr, which")
        };
        return Err(format!(
            "{how} needs the OCR assets: set --models DIR (or FDOC_OCR_MODELS, or ocr.models in the config), or read words from files with --ocr-words"
        ));
    }
    if mode == Mode::Vlm && !i.provider && !sidecars {
        return Err(format!(
            "mode vlm (from {source}) needs a model reader: run `fdoc config gemini --credentials <key.json>`, or read its readings from files with --tier-results"
        ));
    }
    let call_model = mode == Mode::Vlm && i.provider && !i.tier_results.any() && !i.no_escalate;
    // A config that once turned OCR or a model on by itself, with no mode:
    // say what it no longer does, also where an input raised the mode short
    // of it (`--ocr` with a provider set up once read with the model too).
    if unchosen {
        let lost = if i.legacy_vlm && i.provider && !i.no_escalate && mode < Mode::Vlm {
            Some(("a model reader", Mode::Vlm))
        } else if i.legacy_ocr && mode < Mode::Ocr {
            Some(("OCR", Mode::Ocr))
        } else {
            None
        };
        if let Some((what, to)) = lost {
            notes.push(format!(
                "note: the config sets up {what} but no mode; reading {mode}. Set mode = \"{to}\" in the config, or give --mode {to}"
            ));
        }
    }
    Ok(Resolved {
        mode,
        source,
        call_model,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Inputs {
        Inputs {
            assets: true,
            provider: true,
            ..Default::default()
        }
    }

    #[test]
    fn the_mode_and_its_source_from_every_input() {
        type Case = (&'static str, fn(&mut Inputs), Mode, Source, bool);
        let cases: Vec<Case> = vec![
            (
                "nothing",
                |_| {},
                Mode::Deterministic,
                Source::Default,
                false,
            ),
            (
                "--mode ocr",
                |i| i.flag = Some(Mode::Ocr),
                Mode::Ocr,
                Source::Flag("--mode"),
                false,
            ),
            (
                "--mode vlm",
                |i| i.flag = Some(Mode::Vlm),
                Mode::Vlm,
                Source::Flag("--mode"),
                true,
            ),
            (
                "FDOC_MODE",
                |i| i.env = Some("ocr".into()),
                Mode::Ocr,
                Source::Env,
                false,
            ),
            (
                "FDOC_MODE empty",
                |i| i.env = Some(" ".into()),
                Mode::Deterministic,
                Source::Default,
                false,
            ),
            (
                "config",
                |i| i.config = Some(Mode::Vlm),
                Mode::Vlm,
                Source::Config,
                true,
            ),
            (
                "flag over env over config",
                |i| {
                    i.flag = Some(Mode::Ocr);
                    i.env = Some("vlm".into());
                    i.config = Some(Mode::Deterministic);
                },
                Mode::Ocr,
                Source::Flag("--mode"),
                false,
            ),
            (
                "env over config",
                |i| {
                    i.env = Some("deterministic".into());
                    i.config = Some(Mode::Vlm);
                },
                Mode::Deterministic,
                Source::Env,
                false,
            ),
            (
                "--escalate",
                |i| i.escalate = true,
                Mode::Vlm,
                Source::Flag("--escalate"),
                true,
            ),
            (
                "--no-escalate alone",
                |i| i.no_escalate = true,
                Mode::Deterministic,
                Source::Default,
                false,
            ),
            (
                "--no-escalate caps the config's vlm at ocr",
                |i| {
                    i.no_escalate = true;
                    i.config = Some(Mode::Vlm);
                },
                Mode::Ocr,
                Source::Flag("--no-escalate"),
                false,
            ),
            (
                "--no-escalate leaves ocr",
                |i| {
                    i.no_escalate = true;
                    i.env = Some("ocr".into());
                },
                Mode::Ocr,
                Source::Env,
                false,
            ),
            (
                "--ocr raises",
                |i| i.ocr = Given::Flag,
                Mode::Ocr,
                Source::Raised("--ocr"),
                false,
            ),
            (
                "FDOC_OCR raises",
                |i| i.ocr = Given::Env,
                Mode::Ocr,
                Source::Raised("FDOC_OCR"),
                false,
            ),
            (
                "--ocr under --no-escalate: today's OCR run",
                |i| {
                    i.ocr = Given::Flag;
                    i.no_escalate = true;
                },
                Mode::Ocr,
                Source::Raised("--ocr"),
                false,
            ),
            (
                "--ocr-words raises, and stands in for the assets",
                |i| {
                    i.ocr_words = Given::Flag;
                    i.assets = false;
                },
                Mode::Ocr,
                Source::Raised("--ocr-words"),
                false,
            ),
            (
                "--tier-results raises to vlm and calls nothing",
                |i| i.tier_results = Given::Flag,
                Mode::Vlm,
                Source::Raised("--tier-results"),
                false,
            ),
            (
                "FDOC_TIER_RESULTS under --no-escalate stays vlm",
                |i| {
                    i.tier_results = Given::Env;
                    i.no_escalate = true;
                },
                Mode::Vlm,
                Source::Raised("FDOC_TIER_RESULTS"),
                false,
            ),
            (
                "--emit-anchors raises to vlm, and with a reader calls it",
                |i| i.emit_anchors = Given::Flag,
                Mode::Vlm,
                Source::Raised("--emit-anchors"),
                true,
            ),
            (
                "--emit-anchors needs no reader",
                |i| {
                    i.emit_anchors = Given::Flag;
                    i.provider = false;
                },
                Mode::Vlm,
                Source::Raised("--emit-anchors"),
                false,
            ),
            (
                "a mode at or over what an input needs stays",
                |i| {
                    i.config = Some(Mode::Vlm);
                    i.ocr = Given::Flag;
                },
                Mode::Vlm,
                Source::Config,
                true,
            ),
        ];
        for (name, set, mode, source, call) in cases {
            let mut i = base();
            set(&mut i);
            let r = resolve(&i).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(
                (r.mode, r.source, r.call_model),
                (mode, source, call),
                "{name}"
            );
        }
    }

    #[test]
    fn contradictions_and_unmet_needs_are_errors() {
        let cases: Vec<(&str, fn(&mut Inputs), &str)> = vec![
            (
                "--escalate --no-escalate",
                |i| {
                    i.escalate = true;
                    i.no_escalate = true;
                },
                "contradict",
            ),
            (
                "--no-escalate --mode vlm",
                |i| {
                    i.no_escalate = true;
                    i.flag = Some(Mode::Vlm);
                },
                "contradict",
            ),
            (
                "--escalate --mode ocr",
                |i| {
                    i.escalate = true;
                    i.flag = Some(Mode::Ocr);
                },
                "--escalate asks for vlm",
            ),
            (
                "--ocr --mode deterministic",
                |i| {
                    i.ocr = Given::Flag;
                    i.flag = Some(Mode::Deterministic);
                },
                "--ocr needs --mode ocr",
            ),
            (
                "--ocr-words --mode deterministic",
                |i| {
                    i.ocr_words = Given::Flag;
                    i.flag = Some(Mode::Deterministic);
                },
                "--ocr-words needs --mode ocr",
            ),
            (
                "--tier-results --mode ocr",
                |i| {
                    i.tier_results = Given::Flag;
                    i.flag = Some(Mode::Ocr);
                },
                "--tier-results needs --mode vlm",
            ),
            (
                "FDOC_MODE nonsense",
                |i| i.env = Some("fast".into()),
                "is not a mode",
            ),
            (
                "ocr without assets",
                |i| {
                    i.flag = Some(Mode::Ocr);
                    i.assets = false;
                },
                "needs the OCR assets",
            ),
            (
                "vlm without assets",
                |i| {
                    i.flag = Some(Mode::Vlm);
                    i.assets = false;
                },
                "builds on ocr",
            ),
            (
                "--tier-results without assets",
                |i| {
                    i.tier_results = Given::Flag;
                    i.assets = false;
                },
                "builds on ocr",
            ),
            (
                "vlm without a reader",
                |i| {
                    i.config = Some(Mode::Vlm);
                    i.provider = false;
                },
                "needs a model reader",
            ),
            (
                "--escalate without a reader",
                |i| {
                    i.escalate = true;
                    i.provider = false;
                },
                "needs a model reader",
            ),
        ];
        for (name, set, says) in cases {
            let mut i = base();
            set(&mut i);
            let e = resolve(&i)
                .err()
                .unwrap_or_else(|| panic!("{name}: no error"));
            assert!(e.contains(says), "{name}: {e}");
        }
    }

    #[test]
    fn a_variable_yields_to_a_flag_and_says_so() {
        let mut i = base();
        i.flag = Some(Mode::Ocr);
        i.tier_results = Given::Env;
        let r = resolve(&i).unwrap();
        assert_eq!(r.mode, Mode::Ocr);
        assert!(
            r.notes.iter().any(|n| n.contains("FDOC_TIER_RESULTS")),
            "{:?}",
            r.notes
        );
    }

    #[test]
    fn a_config_that_turned_ocr_or_a_model_on_by_itself_says_what_it_no_longer_does() {
        let mut i = base();
        i.legacy_vlm = true;
        let r = resolve(&i).unwrap();
        assert_eq!(r.mode, Mode::Deterministic);
        assert_eq!(r.notes.len(), 1);
        assert!(r.notes[0].contains("a model reader") && r.notes[0].contains("mode = \"vlm\""));
        // Under `--no-escalate` the model was never called: only OCR was.
        i.no_escalate = true;
        assert!(resolve(&i).unwrap().notes.is_empty());
        i.legacy_ocr = true;
        let r = resolve(&i).unwrap();
        assert!(r.notes[0].contains("OCR") && r.notes[0].contains("mode = \"ocr\""));
        // A mode set says nothing.
        i.config = Some(Mode::Deterministic);
        assert!(resolve(&i).unwrap().notes.is_empty());

        // `--ocr` with a provider set up once read with the model too: the
        // raise to ocr stops short of it, and says so.
        let mut i = base();
        i.legacy_vlm = true;
        i.ocr = Given::Flag;
        let r = resolve(&i).unwrap();
        assert_eq!(r.mode, Mode::Ocr);
        assert!(!r.call_model);
        assert_eq!(r.notes.len(), 1);
        assert!(
            r.notes[0].contains("reading ocr") && r.notes[0].contains("mode = \"vlm\""),
            "{}",
            r.notes[0]
        );
        // OCR an input asked for is OCR the config would have run.
        let mut i = base();
        i.legacy_ocr = true;
        i.ocr = Given::Flag;
        assert!(resolve(&i).unwrap().notes.is_empty());
        // Readings from files are the model's part, read without a call.
        let mut i = base();
        i.legacy_vlm = true;
        i.tier_results = Given::Flag;
        assert!(resolve(&i).unwrap().notes.is_empty());
    }
}
