//! Source-agnostic document model and emitters.
//!
//! A parser's only job is to produce [`Element`]s; Markdown, XHTML, DoCO
//! JSON-LD and the plain-text projection all follow from them. Keeping this
//! crate free of any source-format dependency is what lets a Markdown or
//! DOCX consumer avoid compiling a PDF engine.

pub mod doco;
pub mod element;
pub mod emit;
pub mod geom;
pub mod merges;

pub use doco::{to_doco, to_text, DocoOptions};
pub use element::{Element, Link, Target};
pub use emit::{to_markdown, to_xhtml};
pub use geom::{BBox, PageSize};
pub use merges::{denormalize, Merges};
