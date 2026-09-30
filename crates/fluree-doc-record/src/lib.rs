//! Declared records to DoCO-typed document elements.
//!
//! An export from a content system is a record: a news article as the ten
//! fields its publisher stores it under, an asset as its metadata and its
//! caption track. Read as a file it is markup with some prose in it. The
//! markup is found by an extractor as readily as the prose, the offsets
//! count escaped characters nobody sees, and the publication date is one
//! more string.
//!
//! This crate reads a record in two steps that are kept apart:
//!
//! - its notation, XML or JSON, into a [`Record`]: every field that holds a
//!   value, where the source keeps it, as a person reads it;
//! - its [`SourceFormat`], the declaration of what each field is, into a
//!   document: content becomes elements, facts become statements about the
//!   document, values from a controlled list become the concepts they name.
//!
//! Nothing is inferred and nothing is transformed. A record no declared
//! format recognises is not read by this crate at all: it stays the text
//! file its notation makes it.
//!
//! Every element carries the path of the field it was read from. Its
//! offsets count the characters of the field's value, so a mention found
//! in the text is found in the record.

mod convert;
mod format;
pub mod json;
mod record;
pub mod xml;

pub use convert::{convert, Converted};
pub use format::{
    recognise, Content, ContentKind, Enum, Match, MatchOn, Metadata, Recognition, SourceFormat,
    TrackRole, TrackRule,
};
pub use record::{decode, Field, Record, Segment, Syntax, Track};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordError {
    /// The file is not well-formed in its notation.
    Malformed(String),
    /// The file is well-formed and is not a record.
    NotARecord(&'static str),
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "not well-formed: {e}"),
            Self::NotARecord(why) => write!(f, "not a record: {why}"),
        }
    }
}

impl std::error::Error for RecordError {}

/// The notation of a file that is XML or JSON, by its first character.
///
/// A record opens with `<` or `{`. Anything else is not one, whatever its
/// name or its declared type says.
pub fn sniff(bytes: &[u8]) -> Option<Syntax> {
    let head = decode(&bytes[..bytes.len().min(1024)]);
    match head.trim_start().chars().next()? {
        '<' => Some(Syntax::Xml),
        '{' => Some(Syntax::Json),
        _ => None,
    }
}

/// Read a file's bytes into a record, by the notation its content shows.
pub fn read(bytes: &[u8]) -> Result<Record, RecordError> {
    let text = decode(bytes);
    match text.trim_start().chars().next() {
        Some('<') => xml::read(&text),
        Some('{') => json::read(&text),
        _ => Err(RecordError::NotARecord(
            "the document opens with neither `<` nor `{`",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_read_by_what_it_holds() {
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(
                "\n<record><title>Île</title></record>"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        assert_eq!(sniff(&utf16), Some(Syntax::Xml));
        assert_eq!(read(&utf16).unwrap().first("title"), Some("Île"));
        assert_eq!(sniff(b"  {\"a\": 1}"), Some(Syntax::Json));
        assert_eq!(read(b"  {\"a\": 1}").unwrap().first("a"), Some("1"));
        assert_eq!(sniff(b"Plain text."), None);
        assert_eq!(sniff(b""), None);
        assert!(matches!(
            read(b"Plain text."),
            Err(RecordError::NotARecord(_))
        ));
    }
}
