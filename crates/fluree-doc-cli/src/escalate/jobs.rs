//! What the CLI knows about a document's crops that the library does not.
//!
//! The crop set is chosen in one place, the library's
//! [`fluree_doc_pdf::escalate::plan`], for every command that asks:
//! `fdoc dev render-routed` writes them to disk for an external reader,
//! `fdoc convert --escalate` sends them to a configured one, and `fdoc
//! triage` counts them. They must agree — a crop set that differs between
//! them means the cached benchmark scores describe a pipeline nobody runs.
//! What the CLI adds is its hints: the layout detector's table boxes, from
//! sidecar files, and whether column doubt escalates.

use crate::commands::dev::layout_tables;
use fluree_doc_pdf::escalate::CropHints;
use fluree_doc_pdf::Document;
use std::path::Path;

/// The hints a document's crops are chosen and prompted with: the tables
/// the layout detector boxed on each page, read from `layout_boxes`, and
/// column doubt when the configuration asks for it or `FDOC_ESCALATE_COLUMNS`
/// does for one run.
pub(crate) fn hints(
    doc: &Document,
    stem: &str,
    layout_boxes: Option<&Path>,
    on_column_doubt: bool,
) -> CropHints {
    CropHints {
        on_column_doubt: on_column_doubt || std::env::var_os("FDOC_ESCALATE_COLUMNS").is_some(),
        layout_tables: doc
            .pages
            .iter()
            .map(|p| (p.index, layout_tables(layout_boxes, stem, p.index)))
            .filter(|(_, boxes)| !boxes.is_empty())
            .collect(),
    }
}
