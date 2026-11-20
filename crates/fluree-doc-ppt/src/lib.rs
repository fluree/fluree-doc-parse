//! PowerPoint 97-2003 presentations (`.ppt`) to DoCO-typed elements.
//!
//! A `.ppt` is an OLE compound file whose `PowerPoint Document` stream is a
//! tree of records, each a version, an instance, a type and a length. The
//! file is saved by appending edits: the `Current User` stream points at the
//! last edit, each edit at the one before it and at a persist directory
//! mapping object ids to offsets in the stream, the newest mapping winning.
//! Through it, the document's slide list names each slide, in order, and
//! each slide's container holds its shapes, whose text is in text atoms
//! (`TextCharsAtom`, UTF-16; `TextBytesAtom`, one byte a character) after a
//! header saying what the text is: a title, a body, other text.
//!
//! **What is read**: each slide is a page, in the order the slide list
//! gives; a title placeholder's text is a level-1 heading, every other text
//! a paragraph per line. Where a slide's shapes hold no text (an older
//! writer keeps it in the slide list instead) the slide list's text is read.
//! **What is not**: speaker notes, the masters' placeholder prompts,
//! bullets and list levels (so no list items), tables (their cells' text is
//! read as paragraphs), pictures, charts and embedded objects, and the
//! summary information. Encrypted files fail as malformed or unsupported.
//!
//! **Bounds**: the compound file is opened by the `cfb` crate, which
//! refuses a looping sector chain; the stream is read to at most
//! [`MAX_STREAM_BYTES`]; records nest at most [`MAX_DEPTH`] deep, at most
//! [`MAX_EDITS`] edits are followed, each once, each persist directory
//! read once and at most [`MAX_PERSIST_ENTRIES`] of their entries, and at
//! most [`MAX_SLIDES`] slides read.

use fluree_doc_model::Element;
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};

/// The most bytes the `PowerPoint Document` stream is read to: 256 MiB.
pub const MAX_STREAM_BYTES: u64 = 256 << 20;
/// The deepest nesting of containers followed.
pub const MAX_DEPTH: usize = 64;
/// The most edits followed back from the last.
pub const MAX_EDITS: usize = 4_096;
/// The most slides read.
pub const MAX_SLIDES: usize = 10_000;
/// The most persist directory entries read, over every edit: about four
/// million.
pub const MAX_PERSIST_ENTRIES: usize = 1 << 22;

const DOCUMENT: u16 = 0x03E8;
const SLIDE: u16 = 0x03EE;
const SLIDE_LIST: u16 = 0x0FF0;
const SLIDE_PERSIST: u16 = 0x03F3;
const TEXT_HEADER: u16 = 0x0F9F;
const TEXT_CHARS: u16 = 0x0FA0;
const TEXT_BYTES: u16 = 0x0FA8;
const USER_EDIT: u16 = 0x0FF5;
const PERSIST_DIRECTORY: u16 = 0x1772;
const CRYPT_SESSION: u16 = 0x2F14;
/// An OfficeArt shape container (`SpContainer`).
const SHAPE: u16 = 0xF004;
/// `OEPlaceholderAtom`: which placeholder a shape is.
const PLACEHOLDER: u16 = 0x0BC3;

#[derive(Debug)]
pub enum PptError {
    /// Not a compound file, or a broken one.
    Container(String),
    /// No `PowerPoint Document` stream.
    NotPowerPoint,
    /// A record or offset that leads nowhere.
    Malformed(String),
    /// The file is encrypted.
    Encrypted,
    /// Past a bound.
    TooLarge(String),
}

impl std::fmt::Display for PptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Container(e) => write!(f, "not a readable .ppt: {e}"),
            Self::NotPowerPoint => write!(
                f,
                "not a PowerPoint document: no PowerPoint Document stream"
            ),
            Self::Malformed(e) => write!(f, "malformed PowerPoint document: {e}"),
            Self::Encrypted => write!(f, "the PowerPoint document is encrypted"),
            Self::TooLarge(e) => write!(f, "PowerPoint document too large: {e}"),
        }
    }
}

impl std::error::Error for PptError {}

/// Whether `bytes` is a PowerPoint 97-2003 presentation: a compound file
/// with a `PowerPoint Document` stream.
pub fn is_ppt(bytes: &[u8]) -> bool {
    cfb::CompoundFile::open(Cursor::new(bytes)).is_ok_and(|cf| cf.is_stream("/PowerPoint Document"))
}

/// A record's header and where its data is.
#[derive(Debug, Clone, Copy)]
struct Record {
    instance: u16,
    typ: u16,
    /// Where its data starts and ends in the stream.
    data: (usize, usize),
    container: bool,
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// The record at `at`, if its header and data lie within `d`.
fn record(d: &[u8], at: usize) -> Option<Record> {
    let ver_inst = u16_at(d, at)?;
    let typ = u16_at(d, at + 2)?;
    let len = u32_at(d, at + 4)? as usize;
    let start = at + 8;
    let end = start.checked_add(len)?;
    (end <= d.len()).then_some(Record {
        instance: ver_inst >> 4,
        typ,
        data: (start, end),
        container: ver_inst & 0x0F == 0x0F,
    })
}

/// The records directly inside a container.
fn children(d: &[u8], r: &Record) -> Vec<Record> {
    let mut out = Vec::new();
    let mut at = r.data.0;
    while at + 8 <= r.data.1 {
        let Some(c) = record(d, at).filter(|c| c.data.1 <= r.data.1) else {
            break;
        };
        out.push(c);
        at = c.data.1;
    }
    out
}

fn stream(cf: &mut cfb::CompoundFile<Cursor<&[u8]>>, name: &str) -> Result<Vec<u8>, PptError> {
    let s = cf
        .open_stream(name)
        .map_err(|e| PptError::Container(e.to_string()))?;
    if s.len() > MAX_STREAM_BYTES {
        return Err(PptError::TooLarge(format!(
            "{name}: {} bytes, more than the {MAX_STREAM_BYTES} read",
            s.len()
        )));
    }
    let mut v = Vec::new();
    s.take(MAX_STREAM_BYTES)
        .read_to_end(&mut v)
        .map_err(|e| PptError::Container(e.to_string()))?;
    Ok(v)
}

/// The persist directory, newest edit first, and the document's id: from
/// the last edit back through every earlier one.
fn persist(doc: &[u8], mut edit: usize) -> Result<(HashMap<u32, u32>, u32), PptError> {
    let malformed = |w: &str| PptError::Malformed(w.to_string());
    let mut map: HashMap<u32, u32> = HashMap::new();
    let mut doc_id = None;
    let mut seen = HashSet::new();
    // Several edits may name one directory: it is read once. Directories
    // laid inside each other's data are as many records, so the entries
    // read are counted too.
    let mut dirs = HashSet::new();
    let mut entries = 0usize;
    loop {
        if !seen.insert(edit) || seen.len() > MAX_EDITS {
            break;
        }
        let r = record(doc, edit)
            .filter(|r| r.typ == USER_EDIT)
            .ok_or_else(|| malformed("the last edit is not where the file says"))?;
        let d = &doc[r.data.0..r.data.1];
        let last = u32_at(d, 8).ok_or_else(|| malformed("an edit is cut short"))? as usize;
        let dir = u32_at(d, 12).ok_or_else(|| malformed("an edit is cut short"))? as usize;
        doc_id = doc_id.or(u32_at(d, 16));
        let fresh = dirs.insert(dir);
        let dir = record(doc, dir)
            .filter(|r| r.typ == PERSIST_DIRECTORY)
            .ok_or_else(|| malformed("a persist directory is not where its edit says"))?;
        let mut at = if fresh { dir.data.0 } else { dir.data.1 };
        entries += (dir.data.1 - at) / 4;
        if entries > MAX_PERSIST_ENTRIES {
            return Err(PptError::TooLarge(format!(
                "persist directories of more than {MAX_PERSIST_ENTRIES} entries"
            )));
        }
        while at + 4 <= dir.data.1 {
            let head = u32_at(doc, at).unwrap_or(0);
            let (first, count) = (head & 0x000F_FFFF, (head >> 20) as usize);
            at += 4;
            for k in 0..count {
                let Some(off) = u32_at(doc, at + 4 * k).filter(|_| at + 4 * k + 4 <= dir.data.1)
                else {
                    break;
                };
                map.entry(first + k as u32).or_insert(off);
            }
            at += 4 * count;
        }
        if last == 0 {
            break;
        }
        edit = last;
    }
    Ok((
        map,
        doc_id.ok_or_else(|| malformed("no edit names the document"))?,
    ))
}

/// A run of text and what its header says it is.
#[derive(Debug, Clone, PartialEq)]
struct Text {
    /// `TextHeaderAtom`'s type: 0 title, 6 centred title, others body text.
    kind: u32,
    text: String,
}

/// Whether a shape is a title placeholder: its `OEPlaceholderAtom` names a
/// title (`PT_Title`, `PT_CenterTitle`, `PT_VerticalTitle`, or a master's
/// title).
fn title_placeholder(d: &[u8], r: &Record, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    children(d, r).iter().any(|c| {
        if c.typ == PLACEHOLDER {
            return d
                .get(c.data.0 + 4)
                .is_some_and(|t| matches!(t, 0x01 | 0x0D | 0x0F | 0x11));
        }
        c.container && title_placeholder(d, c, depth + 1)
    })
}

/// A text atom's text: UTF-16 (`TextCharsAtom`), or one byte a character
/// (`TextBytesAtom`), the low byte of UTF-16, which is Latin-1.
fn atom_text(typ: u16, data: &[u8]) -> String {
    if typ == TEXT_CHARS {
        let units: Vec<u16> = data
            .chunks_exact(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        data.iter().map(|&b| b as char).collect()
    }
}

/// Every text atom under `r`, each with the header before it, in order.
/// `title` makes every text a title, the shape being a title placeholder.
fn texts(d: &[u8], r: &Record, depth: usize, kind: &mut u32, title: bool, out: &mut Vec<Text>) {
    if depth > MAX_DEPTH {
        return;
    }
    for c in children(d, r) {
        let data = &d[c.data.0..c.data.1];
        match c.typ {
            TEXT_HEADER => *kind = u32_at(data, 0).unwrap_or(4),
            TEXT_CHARS | TEXT_BYTES => out.push(Text {
                kind: if title { 0 } else { *kind },
                text: atom_text(c.typ, data),
            }),
            // A shape: a placeholder that is a slide's title makes its text
            // the title, whatever its text header says (LibreOffice writes
            // `Other` for every slide text).
            SHAPE if c.container => {
                let title = title || title_placeholder(d, &c, depth + 1);
                texts(d, &c, depth + 1, kind, title, out);
            }
            _ if c.container => texts(d, &c, depth + 1, kind, title, out),
            _ => {}
        }
    }
}

/// Parse a `.ppt` file's bytes into elements, a slide a page.
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, PptError> {
    let mut cf = cfb::CompoundFile::open(Cursor::new(bytes))
        .map_err(|e| PptError::Container(e.to_string()))?;
    if !cf.is_stream("/PowerPoint Document") {
        return Err(PptError::NotPowerPoint);
    }
    if cf.is_stream("/EncryptedSummary") {
        return Err(PptError::Encrypted);
    }
    let doc = stream(&mut cf, "/PowerPoint Document")?;
    let user = stream(&mut cf, "/Current User")?;
    // CurrentUserAtom: its header, then its size, a token, and the offset
    // of the last edit.
    let edit = u32_at(&user, 16)
        .ok_or_else(|| PptError::Malformed("the Current User stream is cut short".into()))?
        as usize;
    let (map, doc_id) = persist(&doc, edit)?;
    let at = |id: u32| map.get(&id).and_then(|&o| record(&doc, o as usize));
    let document = at(doc_id).filter(|r| r.typ == DOCUMENT).ok_or_else(|| {
        PptError::Malformed("the document is not where the persist directory says".into())
    })?;
    if children(&doc, &document)
        .iter()
        .any(|c| c.typ == CRYPT_SESSION)
    {
        return Err(PptError::Encrypted);
    }
    // The slide list (instance 0): its slides in order, each with the text
    // the list itself keeps for it.
    let mut slides: Vec<(u32, Vec<Text>)> = Vec::new();
    if let Some(list) = children(&doc, &document)
        .into_iter()
        .find(|c| c.typ == SLIDE_LIST && c.instance == 0)
    {
        let mut kind = 4;
        for c in children(&doc, &list) {
            let data = &doc[c.data.0..c.data.1];
            match c.typ {
                SLIDE_PERSIST => {
                    if slides.len() >= MAX_SLIDES {
                        return Err(PptError::TooLarge(format!("more than {MAX_SLIDES} slides")));
                    }
                    slides.push((u32_at(data, 0).unwrap_or(0), Vec::new()));
                    kind = 4;
                }
                TEXT_HEADER => kind = u32_at(data, 0).unwrap_or(4),
                TEXT_CHARS | TEXT_BYTES => {
                    if let Some((_, t)) = slides.last_mut() {
                        t.push(Text {
                            kind,
                            text: atom_text(c.typ, data),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for (page, (id, listed)) in slides.into_iter().enumerate() {
        let mut found = Vec::new();
        if let Some(slide) = at(id).filter(|r| r.typ == SLIDE) {
            texts(&doc, &slide, 0, &mut 4, false, &mut found);
        }
        let runs = if found.iter().any(|t| !t.text.trim().is_empty()) {
            found
        } else {
            listed
        };
        for t in runs {
            let title = matches!(t.kind, 0 | 6);
            let lines: Vec<String> = t
                .text
                .split(['\r', '\u{b}'])
                .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|l| !l.is_empty())
                .collect();
            if title {
                if !lines.is_empty() {
                    out.push(Element {
                        page,
                        level: Some(1),
                        ..Element::new("doco:SectionTitle", lines.join(" "), "ppt", "ppt")
                    });
                }
                continue;
            }
            for l in lines {
                out.push(Element {
                    page,
                    ..Element::new("doco:Paragraph", l, "ppt", "ppt")
                });
            }
        }
    }
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
