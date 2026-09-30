//! A record as its source holds it: named fields, and tracks of timed ones.

/// The notation a record is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Syntax {
    Xml,
    Json,
    /// Avid's Asset eXchange Format: XML by notation, read by its own reader
    /// because what it holds is an asset and its tracks, not a tree.
    Axf,
}

impl Syntax {
    /// The `provenance` every element read from this notation carries.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Xml => "xml",
            Self::Json => "json",
            Self::Axf => "axf",
        }
    }
}

/// One value of the record and where the record keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// The value's address in the source: `/record/tags/tag[2]` in XML, the
    /// JSON Pointer `/tags/1` in JSON. One value, one path.
    pub path: String,
    /// What the source calls the field, the same for every value of it:
    /// `tags/tag`, `tags`. This is the name a format declares.
    pub name: String,
    /// The value as a person reads it: character references decoded, the
    /// notation's escapes undone.
    pub value: String,
}

/// A stretch of a recording and what the source says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Milliseconds from the start of the recording.
    pub start_ms: u64,
    pub end_ms: u64,
    pub fields: Vec<Field>,
}

/// One timeline of the recording: its captions, its stories, its shots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub name: String,
    /// The track's address in the source, the form [`Field::path`] takes.
    pub path: String,
    pub segments: Vec<Segment>,
}

/// A record read out of its notation and nothing more: no field has been
/// given a meaning yet. A [`SourceFormat`](crate::SourceFormat) gives them
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub syntax: Syntax,
    /// What the record says it is: the root element of an XML record, the
    /// class of an asset. Empty for JSON, which names no root.
    pub root: String,
    /// Every field holding a value, in the source's order.
    pub fields: Vec<Field>,
    pub tracks: Vec<Track>,
}

impl Record {
    /// Whether the record holds a field called `name`.
    pub fn has(&self, name: &str) -> bool {
        self.fields.iter().any(|f| f.name == name)
    }

    /// The first value of the field called `name`.
    pub fn first(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.value.as_str())
    }
}

/// The text of a file, whatever Unicode encoding it arrived in.
///
/// Exports from Windows systems are UTF-16 with a byte order mark as often
/// as they are UTF-8, and an XML declaration naming the encoding is inside
/// the text it describes. The mark is what says how to read the bytes; with
/// none, a file whose second byte is zero is UTF-16 all the same, since no
/// record opens with a NUL character.
pub fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        return utf16(rest, u16::from_be_bytes);
    }
    match bytes {
        [a, 0, ..] if *a != 0 => utf16(bytes, u16::from_le_bytes),
        [0, b, ..] if *b != 0 => utf16(bytes, u16::from_be_bytes),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| unit([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(s: &str, mark: bool) -> Vec<u8> {
        let mut out: Vec<u8> = if mark { vec![0xFF, 0xFE] } else { Vec::new() };
        out.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
        out
    }

    #[test]
    fn a_file_decodes_by_its_mark_or_by_its_shape() {
        let text = "<record><title>Île</title></record>";
        assert_eq!(decode(text.as_bytes()), text);
        assert_eq!(decode(&[b"\xEF\xBB\xBF", text.as_bytes()].concat()), text);
        assert_eq!(decode(&utf16le(text, true)), text);
        assert_eq!(decode(&utf16le(text, false)), text);
        let be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        assert_eq!(decode(&be), text);
    }
}
