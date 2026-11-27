//! Source-agnostic document model and emitters.
//!
//! A parser's only job is to produce [`Element`]s; Markdown, XHTML, DoCO
//! JSON-LD and the plain-text projection all follow from them. Keeping this
//! crate free of any source-format dependency is what lets a Markdown or
//! DOCX consumer avoid compiling a PDF engine.

pub mod budget;
pub mod doco;
pub mod element;
pub mod emit;
pub mod geom;
pub mod marks;
pub mod media;
pub mod merges;
pub mod message;
pub mod outline;
#[cfg(any(test, feature = "test-util"))]
pub mod scaling;
pub mod symbols;

pub use doco::{to_doco, to_text, DocoOptions};
pub use element::{
    is_stray_control, xsd_date_time, xsd_temporal, Attachment, Datum, DocumentInfo, Element, Link,
    Notes, OcrReading, ParseWarning, Target, Turn, UnreadImage, UnreadPage, UnreadRegion,
};
pub use emit::{
    to_markdown, to_markdown_with, to_markdown_with_options, to_xhtml, to_xhtml_with,
    MarkdownOptions, MdLinks,
};
pub use geom::{BBox, PageSize};
pub use media::{
    pictures, read_pictures, stored_size, under_model_floor, Allowance, Media, MediaSource,
    Picture, PictureReader, PictureReads, Unread, MIN_LONG_SIDE_PX, MIN_SHORT_SIDE_PX,
};
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
