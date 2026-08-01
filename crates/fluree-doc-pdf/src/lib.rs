//! Fluree PDF extraction core.
//!
//! Two layers. The extraction layer parses a PDF into positioned glyphs,
//! normalizes the text while preserving the offset→bbox mapping, and resolves
//! entity spans back to rectangles for overlay rendering ([`extract_file`],
//! [`Glyph`], [`text`], [`overlay`]).
//!
//! Above it, the layout layer infers structure from geometry: glyphs into
//! [lines](mod@line) and [`block`]s, [columns](mod@column) and reading order, [`heading`] levels
//! cross-checked against the PDF [`outline`] tree, [`rule`]-driven and
//! clustered [`table`] grids, page [`furniture`] stripped before assembly, and
//! [`doco`] emission on top. [`route`] and [`arbiter`] decide, per region,
//! where deterministic inference is too weak to trust and a model tier should
//! arbitrate the structure.
//!
//! # What is a compatibility surface
//!
//! Two tiers, and the distinction is load-bearing because most of this crate
//! is `pub` for a mechanical reason rather than a deliberate one: `fdoc` is a
//! separate crate, so everything `fdoc dev` inspects has to be reachable.
//!
//! **Supported.** These follow semver and are what a consumer should build on:
//!
//! - [`extract_file`] / [`extract_bytes`] → [`Document`]
//! - [`image::as_document`] — a bare raster image as a one-page document
//! - [`document::analyze_with`] → `Analysis`, plus [`document::to_markdown`]
//!   and [`document::to_xhtml`]
//! - [`doco::to_doco`] / [`doco::to_text`] and [`doco::DocoOptions`]
//! - [`route::decide`] and [`route::signals`] — escalation verdicts
//! - [`forms::fields`] — AcroForm values
//! - [`outline::extract`] — the PDF bookmark tree
//! - [`link::extract`] / [`link::attach`] — link annotations and their anchors
//! - [`arbiter::TierBackend`] and [`arbiter::splice`] — the model-tier contract
//! - [`overlay::rects_for_glyph_range`] — glyph span → highlight rectangles
//! - [`overlay::highlight`] — a text-projection span → its page and rectangles
//! - `render::page` — rasterising a page, behind the `render` feature
//!
//! The element model itself lives in `fluree-doc-model`, which is the crate to
//! depend on if you only consume elements and never parse a PDF.
//!
//! **Internal.** Everything else — [`block`], [columns](mod@column), [`dedup`],
//! [`furniture`], [`geom`], [`heading`], [lines](mod@line), [`rule`],
//! [`table`], [`text`] — is layout machinery whose shape follows whatever the
//! measurements demand. It is documented because the reasoning is worth
//! reading, not because it is stable, and it may change in any release. This
//! is the same promise `fdoc dev` makes about its output.

pub mod arbiter;
pub mod block;
pub mod column;
pub mod dedup;
pub mod doco;
pub mod document;
#[cfg(feature = "render")]
pub mod escalate;
pub(crate) mod extract;
pub mod fidelity;
pub mod figure;
pub mod forms;
pub mod furniture;
pub mod geom;
pub(crate) mod glyph;
pub mod heading;
pub mod image;
pub mod line;
pub mod link;
pub mod outline;
pub mod overlay;

/// Pages the router says carry content, which the final element stream does
/// not hold.
///
/// Called after the tiers, because "unread" is only true once whatever was
/// going to read it has run. The router's verdict alone is not the answer: a
/// page that escalated and came back is read, and a page nobody escalated is
/// not.
///
/// Routing is not consulted for every page — it is not free, and a page with
/// a normal amount of text cannot be one of these. Only pages the output is
/// nearly empty for are asked about.
pub fn unread_pages(
    doc: &Document,
    elements: &[fluree_doc_model::Element],
) -> Vec<fluree_doc_model::UnreadPage> {
    /// Characters a page's elements must fall below to be worth asking the
    /// router about. A page holding real prose is not an unread page whatever
    /// the router would say.
    const NEARLY_EMPTY: usize = 40;
    let mut out = Vec::new();
    for p in &doc.pages {
        let text: usize = elements
            .iter()
            .filter(|e| e.page == p.index)
            .map(|e| e.text.trim().chars().count())
            .sum();
        if text >= NEARLY_EMPTY {
            continue;
        }
        if let route::Route::Vlm(reason) = route::decide(p).0 {
            out.push(fluree_doc_model::UnreadPage {
                index: p.index,
                reason: format!("{reason:?}"),
            });
        }
    }
    out
}
#[cfg(feature = "render")]
pub mod render;
pub mod route;
pub mod rule;
pub mod table;
pub mod text;

pub use block::Block;
pub use extract::{extract_bytes, extract_file, Document, ExtractError, Page};
pub use furniture::Furniture;
pub use geom::BBox;
pub use glyph::Glyph;
pub use line::Line;
pub use text::PageText;
