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
