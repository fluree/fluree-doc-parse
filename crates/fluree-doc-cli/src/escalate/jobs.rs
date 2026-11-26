//! What the CLI knows about a document's crops that the library does not.
//!
//! The crop set is chosen in one place, the library's
//! [`fluree_doc_pdf::escalate`], for every command that asks: `fdoc dev
//! render-routed` writes them to disk for an external reader and `fdoc
//! triage` counts them, and `fdoc convert --escalate` sends them to a
//! configured one, from the analysis it splices into. All three take
//! `crops_at_anchors` of an analysis made with the same options, so each
//! region crop is named after the anchor it fills, less the tables the
//! structure readings (`FDOC_STRUCTURE_RESULTS`, `--structure-results`)
//! veto. They must agree — a crop set that differs between them means the
//! cached benchmark scores describe a pipeline nobody runs. `render-routed`
//! writes the vetoed tables too, for the structure reader, and names them
//! apart ([`to_render`]).
//! What the CLI adds is its hint: whether column doubt escalates.

use crate::commands::common::TierConfig;
use fluree_doc_pdf::arbiter::TierBackend;
use fluree_doc_pdf::document::Analysis;
use fluree_doc_pdf::escalate::{CropHints, CropJobs, PageReasons};
use fluree_doc_pdf::Document;
use std::ffi::OsStr;
use std::path::Path;

/// The crops `analysis` asks a reader for, as `convert --escalate` sends
/// them and `triage` counts them: its anchors
/// ([`fluree_doc_pdf::escalate::crops_at_anchors`]), less the tables the
/// structure readings in `structure` veto, whose readings the
/// splice would throw away, with the reason each whole page among them was
/// asked for, by which `escalation.max_crops` ranks it. `extracted` is the
/// document before the analysis changed it.
pub(crate) fn at_anchors(
    extracted: &Document,
    analysis: &Analysis,
    stem: &str,
    hints: &CropHints,
    structure: Option<&dyn TierBackend>,
) -> (CropJobs, PageReasons) {
    let (mut jobs, reasons) =
        fluree_doc_pdf::escalate::crops_at_anchors_explained(extracted, analysis, hints);
    if let Some(structure) = structure {
        fluree_doc_pdf::escalate::without_vetoed(&mut jobs, &analysis.elements, stem, structure);
    }
    (jobs, reasons)
}

/// The crops `dev render-routed` writes for `analysis`, the reason each
/// whole page among them was asked for, and the names of those the
/// structure readings in `structure_results` veto.
///
/// Every crop is written, the vetoed ones included: the structure reader
/// reads the table crops too, and a crop left out because of a structure
/// reading would never be read again to refresh it. Less the vetoed ones,
/// they are the crops [`at_anchors`] leaves for `convert --escalate` to
/// send and `triage` to count.
pub(crate) fn to_render(
    extracted: &Document,
    analysis: &Analysis,
    stem: &str,
    hints: &CropHints,
    structure_results: Option<&Path>,
) -> (CropJobs, PageReasons, Vec<String>) {
    let (jobs, reasons) =
        fluree_doc_pdf::escalate::crops_at_anchors_explained(extracted, analysis, hints);
    let vetoed = structure_results.map_or_else(Vec::new, |dir| {
        let structure = fluree_doc_pdf::arbiter::FixtureBackend::new(dir.to_path_buf());
        fluree_doc_pdf::arbiter::vetoed_tables(&analysis.elements, stem, &structure)
    });
    (jobs, reasons, vetoed)
}

/// The hints a document's crops are chosen and prompted with, from `tiers`
/// whichever command asks, so none can choose another crop set: column
/// doubt when [`escalates_columns`].
pub(crate) fn hints(tiers: &TierConfig) -> CropHints {
    CropHints {
        on_column_doubt: escalates_columns(tiers),
        regions_apart: false,
    }
}

/// Decides column doubt for one run over the configuration: `0`, `false`,
/// `off` or `no` turn it off, any other value on.
pub(crate) const COLUMNS_ENV: &str = "FDOC_ESCALATE_COLUMNS";

/// What a value of [`COLUMNS_ENV`] decides, `None` when it is unset.
///
/// Read as a person writes it, trimmed and in any case: `False` or `0\n`
/// turning column doubt on would read the opposite of what was set. A value
/// that is neither a usual yes nor a usual no turns it on, as setting the
/// variable does for the other switches, with a warning.
pub(crate) fn columns_override(env: Option<&OsStr>) -> Option<bool> {
    let v = env?;
    let word = v.to_str().map(|w| w.trim().to_ascii_lowercase());
    Some(match word.as_deref() {
        Some("0" | "false" | "off" | "no") => false,
        Some("" | "1" | "true" | "on" | "yes") => true,
        _ => {
            eprintln!(
                "warning: {COLUMNS_ENV}={} is neither on nor off; reading it as on (0 turns it off)",
                v.to_string_lossy()
            );
            true
        }
    })
}

/// Whether a page in column doubt is read whole: as [`COLUMNS_ENV`] says
/// when it is set (`tiers.columns`), else as the configuration does
/// (`escalation.on_column_doubt`). The one answer every command that
/// chooses, counts or advises about crops takes.
pub(crate) fn escalates_columns(tiers: &TierConfig) -> bool {
    tiers
        .columns
        .unwrap_or(tiers.config.escalation.on_column_doubt)
}

/// What `triage` tells the user about the pages in column doubt it lists
/// when they are not read whole: how to turn that on, or, when the run
/// turned it off, that it did, as the configuration may already ask for it.
pub(crate) fn column_advice(tiers: &TierConfig) -> Option<String> {
    if escalates_columns(tiers) {
        return None;
    }
    Some(if tiers.columns.is_some() {
        format!("COLUMN pages are kept off for this run by {COLUMNS_ENV}=0")
    } else {
        "COLUMN pages do not escalate by default — `fdoc config set escalation.on_column_doubt true`"
            .to_owned()
    })
}

/// How many whole pages of a selection were chosen for their column doubt
/// alone, by the `reasons` the selection gave them. A page a scan, a
/// heading or a figure doubt already asked for is read whole whatever the
/// configuration says about columns, so it is not counted.
pub(crate) fn whole_for_columns(reasons: &PageReasons) -> usize {
    reasons
        .values()
        .filter(|r| **r == fluree_doc_pdf::escalate::CropReason::ColumnDoubt)
        .count()
}

/// What a command that writes or counts crops says on stderr when the
/// config file, and not the run, made `pages` pages in column doubt crops of
/// the whole page. `dev render-routed` and `triage` follow the config as
/// `convert` does, so a file left in a working directory changes the crops a
/// benchmark cache is read from: that must be seen where it happens, with
/// the way to undo it.
pub(crate) fn column_note(tiers: &TierConfig, pages: usize) -> Option<String> {
    if pages == 0 || tiers.columns.is_some() || !tiers.config.escalation.on_column_doubt {
        return None;
    }
    let source = tiers
        .config_path
        .as_ref()
        .map_or_else(|| "the config file".to_owned(), |p| p.display().to_string());
    Some(format!(
        "note: {pages} page(s) in column doubt are cropped whole, as \
         `escalation.on_column_doubt` in {source} asks; {COLUMNS_ENV}=0 turns it off for one run"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiers(on_column_doubt: bool, columns: Option<bool>) -> TierConfig {
        let mut tiers = TierConfig {
            config_path: Some(".fdoc/config.toml".into()),
            columns,
            ..Default::default()
        };
        tiers.config.escalation.on_column_doubt = on_column_doubt;
        tiers
    }

    #[test]
    fn the_environment_decides_column_doubt_for_one_run_either_way() {
        assert_eq!(columns_override(None), None);
        assert_eq!(columns_override(Some(OsStr::new("0"))), Some(false));
        assert_eq!(columns_override(Some(OsStr::new("1"))), Some(true));
        // Presence, as for the other switches: an empty value turns it on.
        assert_eq!(columns_override(Some(OsStr::new(""))), Some(true));
        for off in ["false", "OFF", "no", " 0", "0\n"] {
            assert_eq!(
                columns_override(Some(OsStr::new(off))),
                Some(false),
                "{off:?}"
            );
        }
        for on in ["true", "Yes", "on ", "2"] {
            assert_eq!(columns_override(Some(OsStr::new(on))), Some(true), "{on:?}");
        }
        assert!(!escalates_columns(&tiers(false, None)));
        assert!(escalates_columns(&tiers(true, None)));
        assert!(!escalates_columns(&tiers(true, Some(false))));
        assert!(escalates_columns(&tiers(false, Some(true))));
    }

    /// The advice names what would change the answer: the config file,
    /// unless the run turned column doubt off over it, which setting the
    /// file again would not undo.
    #[test]
    fn column_advice_follows_what_turned_column_doubt_off() {
        let set = "fdoc config set escalation.on_column_doubt true";
        let kept_off = "kept off for this run by FDOC_ESCALATE_COLUMNS=0";
        let advice = |configured, columns| column_advice(&tiers(configured, columns));
        assert!(advice(false, None).is_some_and(|a| a.contains(set)));
        for configured in [true, false] {
            let a = advice(configured, Some(false)).expect("advice");
            assert!(a.contains(kept_off) && !a.contains(set), "{a}");
            assert_eq!(advice(configured, Some(true)), None);
        }
        assert_eq!(advice(true, None), None);
    }

    /// A page read whole for a scan or a heading doubt is read whole with
    /// column doubt off too: the note counts only the pages column doubt
    /// alone asked for.
    #[test]
    fn only_pages_read_for_their_columns_alone_are_counted() {
        use fluree_doc_pdf::escalate::CropReason;
        let reasons = PageReasons::from([
            (0, CropReason::Scanned),
            (1, CropReason::HeadingDoubt),
            (2, CropReason::ColumnDoubt),
            (3, CropReason::Region),
        ]);
        assert_eq!(whole_for_columns(&reasons), 1);
    }

    #[test]
    fn whole_pages_the_config_file_asks_for_are_noted_with_their_source() {
        let note = column_note(&tiers(true, None), 2).expect("a note");
        assert!(note.contains("2 page(s)"), "{note}");
        assert!(note.contains(".fdoc/config.toml"), "{note}");
        assert!(note.contains("FDOC_ESCALATE_COLUMNS=0"), "{note}");
        // No page read whole, the file not asking, or the run deciding: nothing to say.
        assert_eq!(column_note(&tiers(true, None), 0), None);
        assert_eq!(column_note(&tiers(false, None), 2), None);
        assert_eq!(column_note(&tiers(true, Some(true)), 2), None);
    }
}
