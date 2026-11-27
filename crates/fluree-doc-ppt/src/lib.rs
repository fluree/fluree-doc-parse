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
//! placeholder's text, which the master bullets, is. PowerPoint keeps a
//! placeholder's text (the title, the body) in the slide list, the shape
//! naming it there (`OutlineTextRefAtom`): it is read where the shape
//! stands, among the slide's pictures, tables and text boxes. Where a
//! slide's shapes show nothing (an older writer keeps all its text in the
//! slide list, naming none) the slide list's text is read. A slide's **speaker notes**, the notes slide
//! its own atom names, follow its content as paragraphs whose `evidence` is
//! `notes`, so a consumer that wants what the audience saw leaves them out.
//! A **table**, a group of shapes whose first says it is one, is rebuilt
//! from its cells' positions.
//! **Pictures** are each a figure where they stand, their alternative
//! text its text, their bytes in the picture store ([`parse_with_media`],
//! [`read_media`]) for an OCR engine or a model to read.
//! A list item's indent level is its `depth`.
//! **What is not**: the masters' placeholder prompts, charts and embedded
//! objects, and the summary information. Encrypted files fail as malformed
//! or unsupported.
//!
//! **Bounds**: the compound file is opened by the `cfb` crate, which
//! refuses a looping sector chain; the stream is read to at most
//! [`MAX_STREAM_BYTES`]; records nest at most [`MAX_DEPTH`] deep, at most
//! [`MAX_EDITS`] edits are followed, each once, each persist directory
//! read once and at most [`MAX_PERSIST_ENTRIES`] of their entries, and at
//! most [`MAX_SLIDES`] slides read, each once, as each notes slide, by
//! where they are in the stream (slide ids that all lead to one place are
//! one slide). Every text read, before it is decoded, every element and
//! every table cell, before it is made, is spent from the document's
//! [`Budget`], and past it the deck is refused; so is each record a slide's
//! walk visits, as a step of work, so that slides laid inside each other's
//! records cannot make the deck walked once for each. A picture shown by several
//! shapes is listed at each, and read once.

use fluree_doc_model::budget::{Budget, TooLarge};
use fluree_doc_model::{Element, Media, MediaSource};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};

/// The most bytes the `PowerPoint Document` stream is read to: 256 MiB (a
/// sixteenth built for the fuzz targets, as the budget is,
/// [`fluree_doc_model::budget::scaled`]).
pub const MAX_STREAM_BYTES: u64 = fluree_doc_model::budget::scaled(256 << 20);
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
/// `OutlineTextRefAtom`: a placeholder whose text the slide list keeps, by
/// its index among the slide's texts there.
const OUTLINE_TEXT_REF: u16 = 0x0F9E;
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
    TooLarge(TooLarge),
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

/// A bound passed is the error's source: [`TooLarge::limit`] says which.
impl std::error::Error for PptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TooLarge(e) => Some(e),
            _ => None,
        }
    }
}

impl From<TooLarge> for PptError {
    fn from(e: TooLarge) -> Self {
        Self::TooLarge(e)
    }
}

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
        return Err(PptError::TooLarge(TooLarge::fixed(format!(
            "{name}: {} bytes, more than the {MAX_STREAM_BYTES} read",
            s.len()
        ))));
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
            return Err(PptError::TooLarge(TooLarge::fixed(format!(
                "persist directories of more than {MAX_PERSIST_ENTRIES} entries"
            ))));
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
/// (`TextBytesAtom`), the low byte of UTF-16, which is Latin-1; spent from
/// `budget` before it is decoded, character by character.
fn atom_text(typ: u16, data: &[u8], budget: &mut Budget) -> Result<String, PptError> {
    let chars = || -> Box<dyn Iterator<Item = char> + '_> {
        if typ == TEXT_CHARS {
            let units = data
                .chunks_exact(2)
                .map(|p| u16::from_le_bytes([p[0], p[1]]));
            Box::new(char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')))
        } else {
            Box::new(data.iter().map(|&b| char::from(b)))
        }
    };
    budget.spend_chars(chars())?;
    Ok(chars().collect())
}

/// What a slide shows, in its shapes' order: text, or a table.
#[derive(Debug, Clone, PartialEq)]
enum Item {
    Text(Text),
    Table(Table),
    /// A picture: its index in the document's picture store (`pib`, from
    /// 1), and its alternative text.
    Picture(u32, String),
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
/// `pib`: the picture store entry a picture shows, from 1.
const PIB: u16 = 0x0104;
/// `wzDescription`: a shape's alternative text, UTF-16.
const DESCRIPTION: u16 = 0x0381;
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
/// bounds above, decided from the cells' anchors before any cell's text is
/// read, so a group is read once whichever it is.
fn table(
    d: &[u8],
    group: &Record,
    depth: usize,
    budget: &mut Budget,
) -> Result<Option<Table>, PptError> {
    let shapes: Vec<Record> = children(d, group)
        .into_iter()
        .filter(|c| c.typ == SHAPE && c.container)
        .collect();
    let Some(first) = shapes.first() else {
        return Ok(None);
    };
    if !has_property(d, first, TABLE_PROPERTIES) {
        return Ok(None);
    }
    let mut cells: Vec<([i32; 4], &Record)> = Vec::new();
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
        cells.push(([l, t, r, b], s));
        if cells.len() > MAX_TABLE_CELLS {
            return Ok(None);
        }
    }
    if cells.is_empty() {
        return Ok(None);
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
        return Ok(None);
    }
    let pos = |v: &[i32], x: i32| v.binary_search(&x).unwrap_or(0);
    // Each cell is (first column, last, first row, last) in the grid. The
    // cells of a table tile it: cells that overlap, each spanning the
    // grid, would mark it again for each.
    let spans: Vec<(usize, usize, usize, usize)> = cells
        .iter()
        .map(|([l, t, r, b], _)| (pos(&xs, *l), pos(&xs, *r), pos(&ys, *t), pos(&ys, *b)))
        .collect();
    let covered = spans
        .iter()
        .map(|&(c0, c1, r0, r1)| (c1 - c0).saturating_mul(r1 - r0))
        .fold(0usize, usize::saturating_add);
    if covered > MAX_TABLE_GRID {
        return Ok(None);
    }
    budget.spend_cells((ncols * nrows) as u64)?;
    let mut grid = Table {
        rows: vec![vec![String::new(); ncols]; nrows],
        left: vec![false; ncols * nrows],
        down: vec![false; ncols * nrows],
    };
    for ((_, s), (c0, c1, r0, r1)) in cells.iter().zip(spans) {
        let mut found = Vec::new();
        texts(d, s, depth + 1, (&mut 4, false), &[], budget, &mut found)?;
        let text = found
            .iter()
            .filter_map(|i| match i {
                Item::Text(t) => Some(tidy(&t.text.replace('\r', " "))),
                Item::Table(_) | Item::Picture(..) => None,
            })
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        // A cell that holds text, at what the output makes of it, its text
        // spent as it was decoded.
        if !text.is_empty() {
            budget.spend_text_cells(1)?;
        }
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
    Ok(Some(grid))
}

/// A shape that shows a picture from the store (`pib`), with its
/// alternative text where it has one.
fn picture(d: &[u8], shape: &Record, budget: &mut Budget) -> Result<Option<Item>, PptError> {
    let props: Vec<(u16, u32, Option<Vec<u8>>)> = children(d, shape)
        .iter()
        .filter(|c| matches!(c.typ, OPT | TERTIARY_OPT))
        .flat_map(|c| properties(d, c))
        .collect();
    let Some(pib) = props.iter().find(|(id, _, _)| *id == PIB).map(|p| p.1) else {
        return Ok(None);
    };
    let alt = props
        .iter()
        .find(|(id, _, _)| *id == DESCRIPTION)
        .and_then(|(_, _, b)| b.as_ref())
        .map(|b| {
            let units: Vec<u16> = b
                .chunks_exact(2)
                .map(|p| u16::from_le_bytes([p[0], p[1]]))
                .take_while(|u| *u != 0)
                .collect();
            String::from_utf16_lossy(&units)
        })
        .unwrap_or_default();
    budget.spend_text(&alt)?;
    Ok(Some(Item::Picture(
        pib,
        alt.split_whitespace().collect::<Vec<_>>().join(" "),
    )))
}

/// What a container shows, in order: each text atom with the header before
/// it and the paragraph properties after it, each text of the slide list a
/// placeholder names (`listed`, the slide's texts there), and each table.
/// `title` makes every text a title, the shape being a title placeholder.
/// Each record visited is a step of `budget`'s work.
fn texts(
    d: &[u8],
    r: &Record,
    depth: usize,
    (kind, title): (&mut u32, bool),
    listed: &[Text],
    budget: &mut Budget,
    out: &mut Vec<Item>,
) -> Result<(), PptError> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let kids = children(d, r);
    budget.spend_work(kids.len() as u64)?;
    for c in kids {
        let data = &d[c.data.0..c.data.1];
        match c.typ {
            TEXT_HEADER => *kind = u32_at(data, 0).unwrap_or(4),
            // A placeholder's text, kept in the slide list: a copy of it,
            // spent, where the shape stands.
            OUTLINE_TEXT_REF => {
                if let Some(t) = u32_at(data, 0).and_then(|i| listed.get(i as usize)) {
                    budget.spend_text(&t.text)?;
                    out.push(Item::Text(Text {
                        kind: if title { 0 } else { t.kind },
                        ..t.clone()
                    }));
                }
            }
            TEXT_CHARS | TEXT_BYTES => out.push(Item::Text(Text {
                kind: if title { 0 } else { *kind },
                text: atom_text(c.typ, data, budget)?,
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
            GROUP if c.container => match table(d, &c, depth + 1, budget)? {
                Some(t) => out.push(Item::Table(t)),
                None => texts(d, &c, depth + 1, (kind, title), listed, budget, out)?,
            },
            // A shape: a placeholder that is a slide's title makes its text
            // the title, whatever its text header says (LibreOffice writes
            // `Other` for every slide text).
            SHAPE if c.container => {
                if let Some(p) = picture(d, &c, budget)? {
                    out.push(p);
                }
                let title = title || title_placeholder(d, &c, depth + 1);
                texts(d, &c, depth + 1, (kind, title), listed, budget, out)?;
            }
            _ if c.container => texts(d, &c, depth + 1, (kind, title), listed, budget, out)?,
            _ => {}
        }
    }
    Ok(())
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
/// Each is an element, spent from `budget` before it is made. A list item's
/// indent level is its depth.
fn paragraphs(t: &Text, budget: &mut Budget) -> Result<Vec<(String, bool, usize)>, TooLarge> {
    let body = matches!(t.kind, 1 | 7 | 8);
    let mut out = Vec::new();
    let mut start = 0usize;
    // The run covering the paragraph's first character, and where it ends:
    // the paragraphs and the runs both go forward, so each is passed once.
    let mut runs = t.paras.iter();
    let mut run = runs.next();
    let mut end = run.map_or(0, |r| r.count as usize);
    for p in t.text.split('\r') {
        while run.is_some() && end <= start {
            run = runs.next();
            end += run.map_or(0, |r| r.count as usize);
        }
        start += p.encode_utf16().count() + 1;
        if p.trim().is_empty() {
            continue;
        }
        budget.spend_elements(1)?;
        let text = tidy(p);
        if text.is_empty() {
            continue;
        }
        let listy = match run {
            Some(r) => r.bullet.unwrap_or(body) || r.indent > 0,
            None => body,
        };
        let depth = run
            .filter(|_| listy)
            .map_or(0, |r| usize::from(r.indent.min(8)));
        out.push((text, listy, depth));
    }
    Ok(out)
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
    parse_with_media(bytes).map(|(elements, _)| elements)
}

/// The most bytes one picture is read to: 64 MiB, as a DOCX's (scaled as
/// [`MAX_STREAM_BYTES`]).
pub const MAX_MEDIA_BYTES: u64 = fluree_doc_model::budget::scaled(64 << 20);

/// Where a picture is kept: in the `Pictures` stream at an offset, or in
/// the document stream, inside its store entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Stored {
    Delayed(usize),
    Inline(usize),
}

/// The picture store's entries (`FBSE`), in order: where each picture is
/// in the `Pictures` stream, or the record holding it inline.
fn picture_store(doc: &[u8], document: &Record) -> Vec<Option<Stored>> {
    let mut out = Vec::new();
    let mut stack = vec![*document];
    // Down to the store: the drawing group, its container, the store.
    while let Some(r) = stack.pop() {
        for c in children(doc, &r) {
            match c.typ {
                0x040B | 0xF000 | 0xF001 if c.container => stack.push(c),
                0xF007 => {
                    let data = &doc[c.data.0..c.data.1];
                    let name = usize::from(*data.get(33).unwrap_or(&0));
                    // A picture kept in the entry itself follows its name.
                    let inline = c.data.0 + 36 + name;
                    out.push(if inline + 8 <= c.data.1 {
                        Some(Stored::Inline(inline))
                    } else {
                        u32_at(data, 28).map(|o| Stored::Delayed(o as usize))
                    });
                }
                _ => {}
            }
        }
        if out.len() > 100_000 {
            break;
        }
    }
    out
}

/// A picture record's image: its bytes past the record's own header, its
/// unique ids and its tag. A drawing (EMF, WMF, PICT) is returned as stored,
/// compressed, and is no image a reader decodes.
fn blip(d: &[u8], at: usize) -> Result<&[u8], String> {
    let r = record(d, at).ok_or("the picture is not where its entry says")?;
    if (r.data.1 - r.data.0) as u64 > MAX_MEDIA_BYTES {
        return Err(format!("a picture of more than {MAX_MEDIA_BYTES} bytes"));
    }
    let uids = if r.instance & 1 == 1 { 32 } else { 16 };
    let skip = match r.typ {
        // JPEG, PNG, DIB, TIFF, CMYK JPEG: the ids, then a tag byte.
        0xF01D | 0xF01E | 0xF01F | 0xF029 | 0xF02A => uids + 1,
        // EMF, WMF, PICT: the ids, then a metafile header.
        0xF01A..=0xF01C => uids + 34,
        _ => return Err(format!("not a picture record ({:#06x})", r.typ)),
    };
    d.get(r.data.0 + skip..r.data.1)
        .ok_or_else(|| "the picture record is cut short".into())
}

/// The bytes of one of the pictures [`parse_with_media`] lists, by its
/// `part` (`Pictures#<n>`), at most [`MAX_MEDIA_BYTES`]. This opens the deck
/// for the one picture: to read several, open a [`MediaReader`] once.
pub fn read_media(bytes: &[u8], part: &str) -> Result<Vec<u8>, String> {
    MediaReader::open(bytes)
        .map_err(|e| e.to_string())?
        .read(part)
}

/// A deck's picture store, opened once for every picture read from it:
/// opening the deck, its streams and its edits, again for each picture of a
/// deck of many costs the deck's size each time.
pub struct MediaReader {
    doc: Vec<u8>,
    store: Vec<Option<Stored>>,
    /// The `Pictures` stream, where an entry keeps its picture there.
    pictures: Option<Result<Vec<u8>, String>>,
}

impl MediaReader {
    pub fn open(bytes: &[u8]) -> Result<Self, PptError> {
        let Opened {
            doc,
            document,
            mut cf,
            ..
        } = open(bytes)?;
        let store = picture_store(&doc, &document);
        let delayed = store.iter().any(|e| matches!(e, Some(Stored::Delayed(_))));
        let pictures = delayed.then(|| stream(&mut cf, "/Pictures").map_err(|e| e.to_string()));
        Ok(Self {
            doc,
            store,
            pictures,
        })
    }

    /// The bytes of the picture `part` names, as [`read_media`] reads them.
    pub fn read(&self, part: &str) -> Result<Vec<u8>, String> {
        self.picture(part).map(<[u8]>::to_vec)
    }

    /// The first `n` bytes of the picture `part` names, at most: enough to
    /// tell its format without copying the rest. Several entries of a
    /// deck's store may point at one large picture, and each is looked at.
    pub fn head(&self, part: &str, n: usize) -> Result<Vec<u8>, String> {
        self.picture(part).map(|p| p[..n.min(p.len())].to_vec())
    }

    /// The picture `part` names, where the deck holds it.
    fn picture(&self, part: &str) -> Result<&[u8], String> {
        let pib: usize = part
            .strip_prefix("Pictures#")
            .and_then(|n| n.parse().ok())
            .filter(|&n| n > 0)
            .ok_or_else(|| format!("{part}: no such picture"))?;
        let entry = self
            .store
            .get(pib - 1)
            .copied()
            .flatten()
            .ok_or_else(|| format!("{part}: no such picture"))?;
        let got = match entry {
            Stored::Inline(at) => blip(&self.doc, at),
            Stored::Delayed(at) => match &self.pictures {
                Some(Ok(p)) => blip(p, at),
                Some(Err(e)) => Err(e.clone()),
                None => Err("no Pictures stream".into()),
            },
        };
        got.map_err(|e| format!("{part}: {e}"))
    }
}

/// A deck opened through its last edit.
struct Opened<'a> {
    /// The `PowerPoint Document` stream.
    doc: Vec<u8>,
    /// Its document container.
    document: Record,
    /// The persist directory: object id to offset in `doc`.
    map: HashMap<u32, u32>,
    cf: cfb::CompoundFile<Cursor<&'a [u8]>>,
}

/// Open a deck: its document stream, and its document container through
/// the last edit's persist directory. An encrypted deck is refused.
fn open(bytes: &[u8]) -> Result<Opened<'_>, PptError> {
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
    let document = map
        .get(&doc_id)
        .and_then(|&o| record(&doc, o as usize))
        .filter(|r| r.typ == DOCUMENT)
        .ok_or_else(|| {
            PptError::Malformed("the document is not where the persist directory says".into())
        })?;
    if children(&doc, &document)
        .iter()
        .any(|c| c.typ == CRYPT_SESSION)
    {
        return Err(PptError::Encrypted);
    }
    Ok(Opened {
        doc,
        document,
        map,
        cf,
    })
}

/// [`parse`], with the pictures its slides show, each a figure
/// (`figure-<slide>-<n>`, numbered across the deck) whose text is its
/// alternative text, and the store entry holding it (`Pictures#<n>`), for an
/// OCR engine or a model to read; [`read_media`] reads one's bytes.
pub fn parse_with_media(bytes: &[u8]) -> Result<(Vec<Element>, Vec<Media>), PptError> {
    parse_with_media_within(bytes, &mut Budget::default())
}

/// [`parse_with_media`], against `budget`, the deck's, and leaving it
/// spent: every text read, from the slide list, the slides, their notes and
/// their tables, spent before it is decoded; every element and cell before
/// it is made. A caller that goes on to read the deck's pictures spends what
/// their readings make from what the deck left of it, so a deck and its
/// pictures are bounded together.
pub fn parse_with_media_within(
    bytes: &[u8],
    budget: &mut Budget,
) -> Result<(Vec<Element>, Vec<Media>), PptError> {
    let Opened {
        doc, document, map, ..
    } = open(bytes)?;
    let at = |id: u32| map.get(&id).and_then(|&o| record(&doc, o as usize));
    // The slide list (instance 0): its slides in order, each with the text
    // the list itself keeps for it.
    let mut slides: Vec<(u32, Vec<Text>)> = Vec::new();
    if let Some(list) = children(&doc, &document)
        .into_iter()
        .find(|c| c.typ == SLIDE_LIST && c.instance == 0)
    {
        // Each header opens a text of the slide, its text and its
        // paragraphs' properties after it: a placeholder names it by its
        // place among them, an empty one included.
        let entries = children(&doc, &list);
        budget.spend_work(entries.len() as u64)?;
        for c in entries {
            let data = &doc[c.data.0..c.data.1];
            let Some((_, texts)) = slides.last_mut() else {
                if c.typ == SLIDE_PERSIST {
                    slides.push((u32_at(data, 0).unwrap_or(0), Vec::new()));
                }
                continue;
            };
            match c.typ {
                SLIDE_PERSIST => {
                    if slides.len() >= MAX_SLIDES {
                        return Err(PptError::TooLarge(TooLarge::fixed(format!(
                            "more than {MAX_SLIDES} slides"
                        ))));
                    }
                    slides.push((u32_at(data, 0).unwrap_or(0), Vec::new()));
                }
                TEXT_HEADER => texts.push(Text {
                    kind: u32_at(data, 0).unwrap_or(4),
                    text: String::new(),
                    paras: Vec::new(),
                }),
                TEXT_CHARS | TEXT_BYTES => {
                    let text = atom_text(c.typ, data, budget)?;
                    match texts.last_mut() {
                        Some(t) if t.text.is_empty() => t.text = text,
                        _ => texts.push(Text {
                            kind: 4,
                            text,
                            paras: Vec::new(),
                        }),
                    }
                }
                STYLE_TEXT_PROP => {
                    if let Some(t) = texts.last_mut().filter(|t| t.paras.is_empty()) {
                        t.paras = para_runs(data, t.text.encode_utf16().count());
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
    // The pictures the store holds, by their `pib`, each named by the first
    // entry that points where it is: entries may point any number of times
    // at one picture, and a picture is read once by its name, however many
    // show it.
    let mut first: HashMap<Stored, u32> = HashMap::new();
    let blips: HashMap<u32, u32> = picture_store(&doc, &document)
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let pib = i as u32 + 1;
            e.map(|at| (pib, *first.entry(at).or_insert(pib)))
        })
        .collect();
    let mut media = Vec::new();
    let mut figures = 0usize;
    let mut out = Vec::new();
    // A slide is listed once, and a notes slide belongs to one slide: one
    // listed again, or named again, is not read again. Each is known by
    // where it is in the stream: the persist directory may send any number
    // of ids to one place.
    let mut places = HashSet::new();
    slides.retain(|(id, _)| map.get(id).is_none_or(|&o| places.insert(o)));
    let mut notes_read = HashSet::new();
    for (page, (id, listed)) in slides.into_iter().enumerate() {
        let mut found = Vec::new();
        let slide = at(id).filter(|r| r.typ == SLIDE);
        if let Some(slide) = &slide {
            texts(&doc, slide, 0, (&mut 4, false), &listed, budget, &mut found)?;
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
            .filter(|&p| map.get(p).is_some_and(|&o| notes_read.insert(o)))
            .and_then(|&p| at(p))
            .filter(|r| r.typ == NOTES)
            .map(|r| {
                let mut v = Vec::new();
                texts(&doc, &r, 0, (&mut 2, false), &[], budget, &mut v).map(|()| v)
            })
            .transpose()?
            .unwrap_or_default();
        let shows = |i: &Item| match i {
            Item::Text(t) => !t.text.trim().is_empty(),
            Item::Table(_) | Item::Picture(..) => true,
        };
        // The slide's shapes, the slide list's texts they name among them;
        // where they show nothing, and name none, the list's texts.
        let runs = if found.iter().any(shows) {
            found
        } else {
            listed.into_iter().map(Item::Text).collect()
        };
        for item in runs {
            let t = match item {
                Item::Text(t) => t,
                Item::Table(t) => {
                    budget.spend_elements(1)?;
                    let e = table_element(t, page);
                    // What DoCO copies of it: a merged value in each cell
                    // it covers.
                    budget.spend_copies(&e)?;
                    out.push(e);
                    continue;
                }
                Item::Picture(pib, alt) => {
                    budget.spend_elements(1)?;
                    figures += 1;
                    let id = format!("figure-{page}-{figures}");
                    // Each shape that shows it: the reader reads a picture
                    // once, however many show it.
                    media.push(Media {
                        figure: id.clone(),
                        source: match blips.get(&pib) {
                            Some(named) => MediaSource::Part(format!("Pictures#{named}")),
                            None => MediaSource::Missing(format!("Pictures#{pib}")),
                        },
                        index: 0,
                    });
                    out.push(Element {
                        page,
                        figure: Some(id),
                        ..Element::new("doco:Figure", alt, "ppt", "ppt")
                    });
                    continue;
                }
            };
            let title = matches!(t.kind, 0 | 6);
            if title {
                let line = tidy(&t.text.replace(['\r', '\u{b}'], " "));
                if !line.is_empty() {
                    budget.spend_elements(1)?;
                    out.push(Element {
                        page,
                        level: Some(1),
                        ..Element::new("doco:SectionTitle", line, "ppt", "ppt")
                    });
                }
                continue;
            }
            for (text, listy, depth) in paragraphs(&t, budget)? {
                let kind = if listy {
                    "doc:ListItem"
                } else {
                    "doco:Paragraph"
                };
                out.push(Element {
                    page,
                    depth,
                    ..Element::new(kind, text, "ppt", "ppt")
                });
            }
        }
        for t in notes.into_iter().filter_map(|i| match i {
            Item::Text(t) => Some(t),
            Item::Table(_) | Item::Picture(..) => None,
        }) {
            for (l, ..) in paragraphs(&t, budget)? {
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
    Ok((out, media))
}

#[cfg(test)]
mod tests;
