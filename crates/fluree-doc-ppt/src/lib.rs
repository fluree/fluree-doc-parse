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
//! a paragraph per paragraph mark, a line break kept as a line. A paragraph
//! its properties (`StyleTextPropAtom`) give a bullet or an indent level past
//! the first is a list item, as a PPTX's is; where they do not say, a body
//! placeholder's text, which the master bullets, is. Where a slide's shapes
//! hold no text (an older writer keeps it in the slide list instead) the
//! slide list's text is read. A slide's **speaker notes**, the notes slide
//! its own atom names, follow its content as paragraphs whose `evidence` is
//! `notes`, so a consumer that wants what the audience saw leaves them out.
//! A **table**, a group of shapes whose first says it is one, is rebuilt
//! from its cells' positions.
//! **What is not**: the masters' placeholder prompts, the list level itself
//! (the element model keeps a level on headings only), pictures, charts and embedded objects, and the
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
const SLIDE_ATOM: u16 = 0x03EF;
const NOTES: u16 = 0x03F0;
const TEXT_HEADER: u16 = 0x0F9F;
const TEXT_CHARS: u16 = 0x0FA0;
const TEXT_BYTES: u16 = 0x0FA8;
const STYLE_TEXT_PROP: u16 = 0x0FA1;
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
    /// Its paragraphs' properties (`StyleTextPropAtom`), where they follow
    /// it: runs of characters, each with its indent level and whether it
    /// says it is bulleted.
    paras: Vec<ParaRun>,
}

/// A run of characters sharing paragraph properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParaRun {
    /// Characters covered, each paragraph's mark included.
    count: u32,
    /// 0 for the outermost level.
    indent: u16,
    /// Whether the run says it has a bullet; `None` where it does not say,
    /// and the master's style decides.
    bullet: Option<bool>,
}

/// The paragraph runs a `StyleTextPropAtom` opens with, as many as cover
/// `chars` characters (its character runs follow and are not read). Each is
/// a count, an indent level, and the properties its mask says it holds, in
/// the order the format sets.
fn para_runs(d: &[u8], chars: usize) -> Vec<ParaRun> {
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut covered = 0usize;
    while covered < chars + 1 && out.len() < 100_000 {
        let (Some(count), Some(indent), Some(mask)) =
            (u32_at(d, at), u16_at(d, at + 4), u32_at(d, at + 6))
        else {
            break;
        };
        at += 10;
        let mut flags = None;
        // (mask bits, bytes) in order: bullet flags, char, font, size,
        // colour, alignment, line spacing, space before and after, left
        // margin, indent, default tab size, tab stops, font alignment, wrap
        // flags, text direction.
        let fields: [(u32, usize); 16] = [
            (0x0000_000F, 2),
            (0x0000_0080, 2),
            (0x0000_0010, 2),
            (0x0000_0040, 2),
            (0x0000_0020, 4),
            (0x0000_0800, 2),
            (0x0000_1000, 2),
            (0x0000_2000, 2),
            (0x0000_4000, 2),
            (0x0000_0100, 2),
            (0x0000_0400, 2),
            (0x0000_8000, 2),
            (0x0010_0000, 0),
            (0x0001_0000, 2),
            (0x000E_0000, 2),
            (0x0020_0000, 2),
        ];
        for (bits, size) in fields {
            if mask & bits == 0 {
                continue;
            }
            if bits == 0x0000_000F {
                flags = u16_at(d, at);
            }
            if bits == 0x0010_0000 {
                // Tab stops: a count, then four bytes each.
                let n = usize::from(u16_at(d, at).unwrap_or(0));
                at += 2 + 4 * n;
                continue;
            }
            at += size;
        }
        out.push(ParaRun {
            count,
            indent,
            bullet: (mask & 0x1 != 0).then(|| flags.is_some_and(|f| f & 0x1 != 0)),
        });
        covered += count as usize;
        if count == 0 {
            break;
        }
    }
    out
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

/// What a slide shows, in its shapes' order: text, or a table.
#[derive(Debug, Clone, PartialEq)]
enum Item {
    Text(Text),
    Table(Table),
}

/// A table rebuilt from its cells' positions.
#[derive(Debug, Clone, PartialEq, Default)]
struct Table {
    rows: Vec<Vec<String>>,
    /// Same shape as `rows`: the cell continues the one to its left, or the
    /// one above it (a merged cell spans it).
    left: Vec<bool>,
    down: Vec<bool>,
}

/// The most cells a table is rebuilt from, and the most its grid may hold;
/// past either its cells are read as text.
const MAX_TABLE_CELLS: usize = 10_000;
const MAX_TABLE_GRID: usize = 1 << 20;

/// An OfficeArt group (`SpgrContainer`) and the anchor a shape inside one
/// is placed by (`ChildAnchor`, left, top, right, bottom).
const GROUP: u16 = 0xF003;
const CHILD_ANCHOR: u16 = 0xF00F;
/// A shape's header (`FSP`), its instance the shape's type; and its
/// properties, in the primary and the tertiary tables.
const SHAPE_HEADER: u16 = 0xF00A;
const OPT: u16 = 0xF00B;
const TERTIARY_OPT: u16 = 0xF122;
/// `tableProperties`: set on the group a table is drawn as.
const TABLE_PROPERTIES: u16 = 0x03A0;
/// `msosptLine`: a cell's border, not a cell.
const LINE_SHAPE: u16 = 20;

/// The properties an `OPT` record lists: each id, its value, and for a
/// complex one the bytes its value counts, which follow the list in order.
fn properties(d: &[u8], r: &Record) -> Vec<(u16, u32, Option<Vec<u8>>)> {
    let n = r.instance as usize;
    let data = &d[r.data.0..r.data.1];
    let mut out = Vec::with_capacity(n.min(1024));
    let mut complex = 6 * n;
    for k in 0..n {
        let (Some(op), Some(value)) = (u16_at(data, 6 * k), u32_at(data, 6 * k + 2)) else {
            break;
        };
        let bytes = if op & 0x8000 != 0 {
            let end = complex.saturating_add(value as usize);
            let b = data.get(complex..end).map(<[u8]>::to_vec);
            complex = end;
            b
        } else {
            None
        };
        out.push((op & 0x3FFF, value, bytes));
    }
    out
}

/// Whether a shape (or a group's first shape) sets property `id`.
fn has_property(d: &[u8], shape: &Record, id: u16) -> bool {
    children(d, shape)
        .iter()
        .filter(|c| matches!(c.typ, OPT | TERTIARY_OPT))
        .any(|c| properties(d, c).iter().any(|(p, _, _)| *p == id))
}

/// A table drawn as a group of shapes: the group's first shape says it is
/// one (`tableProperties`), each other shape is a cell placed by its child
/// anchor, holding its text. The grid is rebuilt from the cells' edges: a
/// cell spanning several columns or rows makes the cells it covers
/// continuations. `None` where the group is no table, or one past the
/// bounds above.
fn table(d: &[u8], group: &Record, depth: usize) -> Option<Table> {
    let shapes: Vec<Record> = children(d, group)
        .into_iter()
        .filter(|c| c.typ == SHAPE && c.container)
        .collect();
    let first = shapes.first()?;
    if !has_property(d, first, TABLE_PROPERTIES) {
        return None;
    }
    let mut cells: Vec<([i32; 4], String)> = Vec::new();
    for s in &shapes[1..] {
        let parts = children(d, s);
        if parts
            .iter()
            .any(|p| p.typ == SHAPE_HEADER && p.instance == LINE_SHAPE)
        {
            continue;
        }
        let Some(a) = parts.iter().find(|p| p.typ == CHILD_ANCHOR) else {
            continue;
        };
        let at = |k: usize| {
            u32_at(d, a.data.0 + 4 * k)
                .filter(|_| a.data.0 + 4 * k + 4 <= a.data.1)
                .map(|v| v as i32)
        };
        let (Some(l), Some(t), Some(r), Some(b)) = (at(0), at(1), at(2), at(3)) else {
            continue;
        };
        if r <= l || b <= t {
            continue;
        }
        let mut found = Vec::new();
        texts(d, s, depth + 1, &mut 4, false, &mut found);
        let text = found
            .iter()
            .filter_map(|i| match i {
                Item::Text(t) => Some(tidy(&t.text.replace('\r', " "))),
                Item::Table(_) => None,
            })
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        cells.push(([l, t, r, b], text));
        if cells.len() > MAX_TABLE_CELLS {
            return None;
        }
    }
    if cells.is_empty() {
        return None;
    }
    let edges = |a: usize, b: usize| {
        let mut v: Vec<i32> = cells.iter().flat_map(|(c, _)| [c[a], c[b]]).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let (xs, ys) = (edges(0, 2), edges(1, 3));
    let (ncols, nrows) = (xs.len() - 1, ys.len() - 1);
    if ncols == 0 || nrows == 0 || ncols.saturating_mul(nrows) > MAX_TABLE_GRID {
        return None;
    }
    let pos = |v: &[i32], x: i32| v.binary_search(&x).unwrap_or(0);
    let mut grid = Table {
        rows: vec![vec![String::new(); ncols]; nrows],
        left: vec![false; ncols * nrows],
        down: vec![false; ncols * nrows],
    };
    for ([l, t, r, b], text) in cells {
        let (c0, c1, r0, r1) = (pos(&xs, l), pos(&xs, r), pos(&ys, t), pos(&ys, b));
        grid.rows[r0][c0] = text;
        for row in r0..r1 {
            for col in c0..c1 {
                if col > c0 {
                    grid.left[row * ncols + col] = true;
                }
                if row > r0 && col == c0 {
                    grid.down[row * ncols + col] = true;
                }
            }
        }
    }
    Some(grid)
}

/// What a container shows, in order: each text atom with the header before
/// it and the paragraph properties after it, and each table.
/// `title` makes every text a title, the shape being a title placeholder.
fn texts(d: &[u8], r: &Record, depth: usize, kind: &mut u32, title: bool, out: &mut Vec<Item>) {
    if depth > MAX_DEPTH {
        return;
    }
    for c in children(d, r) {
        let data = &d[c.data.0..c.data.1];
        match c.typ {
            TEXT_HEADER => *kind = u32_at(data, 0).unwrap_or(4),
            TEXT_CHARS | TEXT_BYTES => out.push(Item::Text(Text {
                kind: if title { 0 } else { *kind },
                text: atom_text(c.typ, data),
                paras: Vec::new(),
            })),
            // The properties of the text just read, beside it.
            STYLE_TEXT_PROP => {
                if let Some(Item::Text(t)) = out.last_mut() {
                    if t.paras.is_empty() {
                        t.paras = para_runs(data, t.text.encode_utf16().count());
                    }
                }
            }
            // A group a table is drawn as is the table; any other group
            // is its shapes.
            GROUP if c.container => match table(d, &c, depth + 1) {
                Some(t) => out.push(Item::Table(t)),
                None => texts(d, &c, depth + 1, kind, title, out),
            },
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

/// A rebuilt table as a `doco:Table`. PowerPoint declares no header row,
/// so `header_rows` is left undetected.
fn table_element(t: Table, page: usize) -> Element {
    let text = t
        .rows
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = Element {
        page,
        ..Element::new("doco:Table", text, "ppt", "ppt")
    };
    e.merged_left = t.left.iter().any(|x| *x).then_some(t.left);
    e.merged_down = t.down.iter().any(|x| *x).then_some(t.down);
    e.cells = Some(t.rows);
    e
}

/// A text's paragraphs, split at their marks, a line break kept as a line;
/// each with whether it is a list item. A paragraph is one where its
/// properties give it a bullet or an indent level past the first, as a
/// PPTX paragraph is; where they do not say, a body placeholder's text
/// (`Body`, `HalfBody`, `QuarterBody`), which the master bullets, is one.
fn paragraphs(t: &Text) -> Vec<(String, bool)> {
    let body = matches!(t.kind, 1 | 7 | 8);
    let mut out = Vec::new();
    let mut start = 0usize;
    for p in t.text.split('\r') {
        // The run covering the paragraph's first character.
        let mut at = 0usize;
        let run = t.paras.iter().find(|r| {
            at += r.count as usize;
            at > start
        });
        start += p.encode_utf16().count() + 1;
        let text = tidy(p);
        if text.is_empty() {
            continue;
        }
        let listy = match run {
            Some(r) => r.bullet.unwrap_or(body) || r.indent > 0,
            None => body,
        };
        out.push((text, listy));
    }
    out
}

/// A paragraph's text: its line breaks as lines, each line's runs of
/// whitespace one space.
fn tidy(s: &str) -> String {
    s.split('\u{b}')
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
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
                            paras: Vec::new(),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    // The notes list (instance 2): each notes slide's id, and where it is.
    let mut notes_at: HashMap<u32, u32> = HashMap::new();
    if let Some(list) = children(&doc, &document)
        .into_iter()
        .find(|c| c.typ == SLIDE_LIST && c.instance == 2)
    {
        for c in children(&doc, &list)
            .into_iter()
            .filter(|c| c.typ == SLIDE_PERSIST)
        {
            let data = &doc[c.data.0..c.data.1];
            if let (Some(persist), Some(id)) = (u32_at(data, 0), u32_at(data, 12)) {
                notes_at.insert(id, persist);
            }
        }
    }
    let mut out = Vec::new();
    for (page, (id, listed)) in slides.into_iter().enumerate() {
        let mut found = Vec::new();
        let slide = at(id).filter(|r| r.typ == SLIDE);
        if let Some(slide) = &slide {
            texts(&doc, slide, 0, &mut 4, false, &mut found);
        }
        // The speaker's notes: the notes slide the slide's own atom names
        // (`notesIdRef`, after its layout, placeholders and master), read
        // after the slide.
        let notes: Vec<Item> = slide
            .and_then(|s| {
                children(&doc, &s)
                    .into_iter()
                    .find(|c| c.typ == SLIDE_ATOM)
                    .and_then(|a| u32_at(&doc[a.data.0..a.data.1], 16))
            })
            .filter(|&n| n != 0)
            .and_then(|n| notes_at.get(&n))
            .and_then(|&p| at(p))
            .filter(|r| r.typ == NOTES)
            .map(|r| {
                let mut v = Vec::new();
                texts(&doc, &r, 0, &mut 2, false, &mut v);
                v
            })
            .unwrap_or_default();
        let shows = |i: &Item| match i {
            Item::Text(t) => !t.text.trim().is_empty(),
            Item::Table(_) => true,
        };
        let runs = if found.iter().any(shows) {
            found
        } else {
            listed.into_iter().map(Item::Text).collect()
        };
        for item in runs {
            let t = match item {
                Item::Text(t) => t,
                Item::Table(t) => {
                    out.push(table_element(t, page));
                    continue;
                }
            };
            let title = matches!(t.kind, 0 | 6);
            if title {
                let line = tidy(&t.text.replace(['\r', '\u{b}'], " "));
                if !line.is_empty() {
                    out.push(Element {
                        page,
                        level: Some(1),
                        ..Element::new("doco:SectionTitle", line, "ppt", "ppt")
                    });
                }
                continue;
            }
            for (text, listy) in paragraphs(&t) {
                let kind = if listy {
                    "doc:ListItem"
                } else {
                    "doco:Paragraph"
                };
                out.push(Element {
                    page,
                    ..Element::new(kind, text, "ppt", "ppt")
                });
            }
        }
        for t in notes.into_iter().filter_map(|i| match i {
            Item::Text(t) => Some(t),
            Item::Table(_) => None,
        }) {
            for (l, _) in paragraphs(&t) {
                out.push(Element {
                    page,
                    ..Element::new("doco:Paragraph", l, "ppt", "notes")
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
