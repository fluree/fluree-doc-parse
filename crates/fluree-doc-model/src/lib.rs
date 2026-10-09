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
pub mod message;

pub use doco::{to_doco, to_text, DocoOptions};
pub use element::{Attachment, DocumentInfo, Element, Link, Notes, Target, Turn, UnreadPage};
pub use emit::{to_markdown, to_markdown_with, to_xhtml, to_xhtml_with};
pub use geom::{BBox, PageSize};
pub use merges::{denormalize, Merges};
pub use message::{Mailbox, Message};

/// The SHA-256 of some bytes, as lowercase hex: how a document and each file
/// it carries are identified, whatever they are named.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
