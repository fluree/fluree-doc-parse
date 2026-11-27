//! Word 97-2003 documents (`.doc`) to DoCO-typed document elements.
//!
//! A `.doc` is an OLE compound file. Its `WordDocument` stream opens with
//! the File Information Block (FIB), which says where everything else is;
//! its text is stored in pieces, anywhere in that stream, each either one
//! byte a character (Windows-1252) or two (UTF-16), and the piece table in
//! the table stream (`0Table` or `1Table`) says which run of character
//! positions each piece holds. Reading the piece table is what reads a
//! document saved by Word's "fast save", whose text is no longer in order.
//!
//! **What is read.** The main text, split into paragraphs at Word's
//! paragraph marks, a line break keeping its line; tables, by each
//! paragraph's properties (in a table, the end of a row), as tables of
//! cells; footnotes, endnotes and comments after the body, marked by their
//! `evidence` as the DOCX reader marks them, a note opening with its number.
//! A field shows its result, not its code. Fields that are pictures or
//! drawn objects leave nothing. Text a tracked change deleted, and hidden
//! text, are left out by the characters' own properties (CHPX), as Word
//! shows the document. The summary information's title and author are read
//! ([`parse_with_info`]).
//!
//! **What is not, in this round.** Styles: no heading, title or list is
//! told apart from a paragraph, and no list number is computed. Headers and
//! footers and text boxes, which [`Parsed::left_out`] names where they hold
//! text, comment authors, a table's header row (a table's `header_rows` is
//! left undetected), merged cells, pictures, and the summary information's
//! dates. Word 95 and earlier, and encrypted files, are refused.
//!
//! **Bounds.** The compound file is opened by the `cfb` crate, which refuses
//! a sector chain that loops or runs past the file. The `WordDocument`
//! stream is read to at most [`MAX_STREAM_BYTES`] and the table stream to
//! [`MAX_TABLE_BYTES`]; at most [`MAX_PIECES`] pieces and [`MAX_CHARS`]
//! characters are read, the piece table's positions only growing, so no
//! character is read twice; each FKP page is read once, at most
//! [`MAX_PARA_RUNS`] runs of paragraph properties kept, and a paragraph's
//! found by a binary search. The characters are read one at a time, never
//! held all at once: pieces may name the same stretch of the stream again,
//! as long as their positions grow. Every character kept, note number
//! written, cell (a row padded to the widest included) and element made is
//! spent from the document's [`Budget`] before it is, and past it the
//! document is refused.

use fluree_doc_model::budget::{Budget, TooLarge};
use fluree_doc_model::{DocumentInfo, Element};
use std::io::{Cursor, Read};

/// The most bytes the `WordDocument` stream is read to: 256 MiB.
pub const MAX_STREAM_BYTES: u64 = 256 << 20;
/// The most bytes the table stream is read to: 64 MiB.
pub const MAX_TABLE_BYTES: u64 = 64 << 20;
/// The most pieces read.
pub const MAX_PIECES: usize = 1 << 20;
/// The most characters read.
pub const MAX_CHARS: u64 = 64 << 20;
/// The most paragraph property runs read from the FKP pages: about four
/// million, a run a paragraph or more.
pub const MAX_PARA_RUNS: usize = 1 << 22;

#[derive(Debug)]
pub enum DocError {
    /// Not a compound file, or a broken one.
    Container(String),
    /// A compound file with no `WordDocument` stream.
    NotWord,
    /// A Word 95 or older file, or one with no piece table.
    Unsupported(String),
    /// The file is encrypted.
    Encrypted,
    /// A structure points outside its stream.
    Malformed(String),
    /// Past a bound.
    TooLarge(String),
}

impl std::fmt::Display for DocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Container(e) => write!(f, "not a readable .doc: {e}"),
            Self::NotWord => write!(
                f,
                "not a Word document: the compound file has no WordDocument stream"
            ),
            Self::Unsupported(e) => write!(f, "unsupported Word document: {e}"),
            Self::Encrypted => write!(f, "the Word document is encrypted"),
            Self::Malformed(e) => write!(f, "malformed Word document: {e}"),
            Self::TooLarge(e) => write!(f, "Word document too large: {e}"),
        }
    }
}

impl std::error::Error for DocError {}

impl From<TooLarge> for DocError {
    fn from(e: TooLarge) -> Self {
        Self::TooLarge(e.to_string())
    }
}

/// Whether `bytes` is a Word 97-2003 document: a compound file with a
/// `WordDocument` stream.
pub fn is_doc(bytes: &[u8]) -> bool {
    cfb::CompoundFile::open(Cursor::new(bytes)).is_ok_and(|cf| cf.is_stream("/WordDocument"))
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn stream(
    cf: &mut cfb::CompoundFile<Cursor<&[u8]>>,
    name: &str,
    max: u64,
) -> Result<Vec<u8>, DocError> {
    let s = cf
        .open_stream(name)
        .map_err(|e| DocError::Container(e.to_string()))?;
    if s.len() > max {
        return Err(DocError::TooLarge(format!(
            "{name}: {} bytes, more than the {max} read",
            s.len()
        )));
    }
    let mut v = Vec::new();
    s.take(max)
        .read_to_end(&mut v)
        .map_err(|e| DocError::Container(e.to_string()))?;
    Ok(v)
}

/// One piece: the character positions it holds, where its text starts in
/// the `WordDocument` stream, and whether it is one byte a character.
#[derive(Debug, Clone, Copy)]
struct Piece {
    cp: (u32, u32),
    fc: u32,
    compressed: bool,
}

/// What the FIB says, of what this reader uses.
struct Fib {
    table: &'static str,
    /// Character counts of the stories, in the order they follow each
    /// other: main text, footnotes, headers, (macro), comments, endnotes.
    ccp: [u32; 6],
    clx: (u32, u32),
    bte_papx: (u32, u32),
    bte_chpx: (u32, u32),
    /// Characters of the text boxes' stories, the body's and the headers'
    /// (`ccpTxbx`, `ccpHdrTxbx`), which follow the stories above.
    ccp_boxes: u32,
}

fn fib(wd: &[u8]) -> Result<Fib, DocError> {
    let malformed = || DocError::Malformed("the file information block is cut short".into());
    if u16_at(wd, 0) != Some(0xA5EC) {
        return Err(DocError::Unsupported(
            "no Word 97 file information block".into(),
        ));
    }
    let n_fib = u16_at(wd, 2).ok_or_else(malformed)?;
    let flags = u16_at(wd, 0x0A).ok_or_else(malformed)?;
    if flags & 0x0100 != 0 {
        return Err(DocError::Encrypted);
    }
    // Word 97 is 0x00C1; Word 6 and 95 wrote lower, and keep their text
    // otherwise.
    if n_fib < 0x00C1 {
        return Err(DocError::Unsupported(format!(
            "file format version {n_fib:#x}, Word 95 or older"
        )));
    }
    let lw = |i: usize| u32_at(wd, 0x40 + 4 * i).ok_or_else(malformed);
    let pair = |i: usize| -> Result<(u32, u32), DocError> {
        let at = 0x9A + 8 * i;
        Ok((
            u32_at(wd, at).ok_or_else(malformed)?,
            u32_at(wd, at + 4).ok_or_else(malformed)?,
        ))
    };
    Ok(Fib {
        table: if flags & 0x0200 != 0 {
            "/1Table"
        } else {
            "/0Table"
        },
        ccp: [lw(3)?, lw(4)?, lw(5)?, lw(6)?, lw(7)?, lw(8)?],
        clx: pair(33)?,
        bte_papx: pair(13)?,
        bte_chpx: pair(12)?,
        ccp_boxes: lw(9)?.saturating_add(lw(10)?),
    })
}

/// The piece table, from the CLX: past any property runs (`Prc`, type 1),
/// the `Pcdt` (type 2) holding `n + 1` character positions and `n` piece
/// descriptors.
fn pieces(table: &[u8], (fc, lcb): (u32, u32)) -> Result<Vec<Piece>, DocError> {
    let clx = table
        .get(fc as usize..(fc as usize).saturating_add(lcb as usize))
        .ok_or_else(|| {
            DocError::Malformed("the piece table lies outside the table stream".into())
        })?;
    let mut at = 0usize;
    while clx.get(at) == Some(&0x01) {
        let n = u16_at(clx, at + 1).unwrap_or(0) as i16;
        at += 3 + n.max(0) as usize;
    }
    if clx.get(at) != Some(&0x02) {
        return Err(DocError::Unsupported("no piece table".into()));
    }
    let len = u32_at(clx, at + 1).unwrap_or(0) as usize;
    let plc = clx
        .get(at + 5..at + 5 + len)
        .ok_or_else(|| DocError::Malformed("the piece table is cut short".into()))?;
    let n = plc.len().saturating_sub(4) / 12;
    if n > MAX_PIECES {
        return Err(DocError::TooLarge(format!("more than {MAX_PIECES} pieces")));
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let (Some(c0), Some(c1)) = (u32_at(plc, 4 * i), u32_at(plc, 4 * (i + 1))) else {
            break;
        };
        // The positions only grow: a piece table whose pieces overlap would
        // read the same characters again for each.
        if c1 < c0 {
            return Err(DocError::Malformed(
                "the piece table's character positions go backwards".into(),
            ));
        }
        let pcd = 4 * (n + 1) + 8 * i;
        let Some(raw) = u32_at(plc, pcd + 2) else {
            break;
        };
        let compressed = raw & 0x4000_0000 != 0;
        let fc = raw & 0x3FFF_FFFF;
        if c1 > c0 {
            out.push(Piece {
                cp: (c0, c1),
                fc: if compressed { fc / 2 } else { fc },
                compressed,
            });
        }
    }
    Ok(out)
}

/// One character of the text, and where it sits in the `WordDocument`
/// stream (its paragraph mark's position finds the paragraph's properties).
#[derive(Debug, Clone, Copy)]
struct Char {
    c: char,
    fc: u32,
}

/// The characters at positions `cp0..cp1`, through the pieces, which
/// [`pieces`] leaves in order and apart, so at most `cp1 - cp0` of them,
/// one at a time.
fn chars<'a>(
    wd: &'a [u8],
    pieces: &'a [Piece],
    cp0: u32,
    cp1: u32,
) -> impl Iterator<Item = Char> + 'a {
    pieces.iter().flat_map(move |p| {
        let (lo, hi) = (p.cp.0.max(cp0), p.cp.1.min(cp1));
        let mut cp = lo;
        std::iter::from_fn(move || {
            if cp >= hi {
                return None;
            }
            let k = cp - p.cp.0;
            cp += 1;
            if p.compressed {
                let fc = p.fc.saturating_add(k);
                let &b = wd.get(fc as usize)?;
                let c = encoding_rs::WINDOWS_1252
                    .decode_without_bom_handling(&[b])
                    .0
                    .chars()
                    .next()
                    .unwrap_or('\u{fffd}');
                Some(Char { c, fc })
            } else {
                let fc = p.fc.saturating_add(2 * k);
                let u = u32::from(u16_at(wd, fc as usize)?);
                // A character past the first plane (an emoji) is two code
                // units, a surrogate pair, each of which alone is none.
                let low = (0xD800..0xDC00)
                    .contains(&u)
                    .then(|| u16_at(wd, fc as usize + 2))
                    .flatten()
                    .map(u32::from)
                    .filter(|l| cp < hi && (0xDC00..0xE000).contains(l));
                let c = match low {
                    Some(l) => {
                        cp += 1;
                        char::from_u32(0x10000 + ((u - 0xD800) << 10) + (l - 0xDC00))
                    }
                    None => char::from_u32(u),
                };
                Some(Char {
                    c: c.unwrap_or('\u{fffd}'),
                    fc,
                })
            }
        })
    })
}

/// A paragraph's table properties, by the position of its mark.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ParaProps {
    in_table: bool,
    /// The paragraph ends a table row.
    row_end: bool,
}

/// The paragraph properties the FKPs give, by stream position ranges:
/// `(fc start, fc end, props)`, sorted by start, for [`props_at`]. Each FKP
/// page is read once, however many entries of the bin table name it.
fn para_props(
    wd: &[u8],
    table: &[u8],
    (fc, lcb): (u32, u32),
) -> Result<Vec<(u32, u32, ParaProps)>, DocError> {
    let mut out = Vec::new();
    let Some(plc) = table.get(fc as usize..(fc as usize).saturating_add(lcb as usize)) else {
        return Ok(out);
    };
    let n = plc.len().saturating_sub(4) / 8;
    let mut pages = std::collections::HashSet::new();
    for i in 0..n {
        let Some(pn) = u32_at(plc, 4 * (n + 1) + 4 * i) else {
            break;
        };
        if !pages.insert(pn & 0x003F_FFFF) {
            continue;
        }
        let start = (pn & 0x003F_FFFF) as usize * 512;
        let Some(page) = wd.get(start..start + 512) else {
            continue;
        };
        let crun = page[511] as usize;
        for r in 0..crun {
            let (Some(f0), Some(f1)) = (u32_at(page, 4 * r), u32_at(page, 4 * (r + 1))) else {
                break;
            };
            let bx = 4 * (crun + 1) + 13 * r;
            let Some(&b_offset) = page.get(bx) else {
                break;
            };
            let props = match b_offset {
                0 => ParaProps::default(),
                o => papx(&page[(o as usize * 2).min(511)..511]),
            };
            out.push((f0, f1, props));
        }
        if out.len() > MAX_PARA_RUNS {
            return Err(DocError::TooLarge(format!(
                "more than {MAX_PARA_RUNS} paragraph property runs"
            )));
        }
    }
    // Sorted by start, the earlier entry first among equal starts, as the
    // bin table lists them.
    out.sort_by_key(|&(f0, _, _)| f0);
    Ok(out)
}

/// The stretches of the stream whose characters are deleted (a tracked
/// deletion, `sprmCFRMarkDel`) or hidden (`sprmCFVanish`), as the CHPX
/// FKPs say: `(fc start, fc end)`, sorted by start, for [`hidden_at`]. Each
/// FKP page is read once, however many entries of the bin table name it.
/// What a character style hides is not seen.
fn hidden_runs(
    wd: &[u8],
    table: &[u8],
    (fc, lcb): (u32, u32),
) -> Result<Vec<(u32, u32)>, DocError> {
    let mut out = Vec::new();
    let Some(plc) = table.get(fc as usize..(fc as usize).saturating_add(lcb as usize)) else {
        return Ok(out);
    };
    let n = plc.len().saturating_sub(4) / 8;
    let mut pages = std::collections::HashSet::new();
    let mut runs = 0usize;
    for i in 0..n {
        let Some(pn) = u32_at(plc, 4 * (n + 1) + 4 * i) else {
            break;
        };
        if !pages.insert(pn & 0x003F_FFFF) {
            continue;
        }
        let start = (pn & 0x003F_FFFF) as usize * 512;
        let Some(page) = wd.get(start..start + 512) else {
            continue;
        };
        let crun = page[511] as usize;
        runs += crun;
        if runs > MAX_PARA_RUNS {
            return Err(DocError::TooLarge(format!(
                "more than {MAX_PARA_RUNS} character property runs"
            )));
        }
        for r in 0..crun {
            let (Some(f0), Some(f1)) = (u32_at(page, 4 * r), u32_at(page, 4 * (r + 1))) else {
                break;
            };
            let Some(&b) = page.get(4 * (crun + 1) + r) else {
                break;
            };
            if b != 0 && chpx_hides(&page[(b as usize * 2).min(511)..511]) {
                out.push((f0, f1));
            }
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// Whether a CHPX's properties delete or hide its characters: a toggle
/// set on (`1`), or to the opposite of its style's (`0x81`), taken as on.
fn chpx_hides(d: &[u8]) -> bool {
    let Some(&cb) = d.first() else {
        return false;
    };
    let Some(grp) = d.get(1..(1 + cb as usize).min(d.len())) else {
        return false;
    };
    let mut at = 0usize;
    while at + 2 <= grp.len() {
        let op = u16_at(grp, at).unwrap_or(0);
        at += 2;
        let size = match (op >> 13) & 7 {
            0 | 1 => 1,
            2 | 4 | 5 => 2,
            3 => 4,
            7 => 3,
            _ => {
                let n = *grp.get(at).unwrap_or(&0) as usize;
                at += 1;
                n
            }
        };
        // `sprmCFRMarkDel`, `sprmCFVanish`.
        if matches!(op, 0x0800 | 0x083C) && matches!(grp.get(at), Some(1 | 0x81)) {
            return true;
        }
        at += size;
    }
    false
}

/// Whether the character at `fc` is in one of `runs`.
fn hidden_at(runs: &[(u32, u32)], fc: u32) -> bool {
    let i = runs.partition_point(|&(f0, _)| f0 <= fc);
    runs[..i].last().is_some_and(|&(_, f1)| fc < f1)
}

/// The properties of the paragraph whose mark is at `fc`: those of the
/// range holding it that starts last at or before it. Runs do not overlap
/// in a well-formed file; in one where they do, this is one of them.
fn props_at(props: &[(u32, u32, ParaProps)], fc: u32) -> ParaProps {
    let i = props.partition_point(|&(f0, _, _)| f0 <= fc);
    props[..i]
        .last()
        .filter(|&&(_, f1, _)| fc < f1)
        .map(|&(_, _, p)| p)
        .unwrap_or_default()
}

/// The table properties a PAPX in an FKP sets: `sprmPFInTable` (0x2416),
/// `sprmPFTtp` (0x2417) and, for a nested table, `sprmPFInnerTtp` (0x244C).
fn papx(d: &[u8]) -> ParaProps {
    let mut props = ParaProps::default();
    let Some(&cb) = d.first() else {
        return props;
    };
    let (start, len) = if cb != 0 {
        (1, 2 * cb as usize - 1)
    } else {
        (2, 2 * *d.get(1).unwrap_or(&0) as usize)
    };
    let Some(grp) = d.get(start..(start + len).min(d.len())) else {
        return props;
    };
    // Past the style index.
    let mut at = 2usize;
    while at + 2 <= grp.len() {
        let op = u16_at(grp, at).unwrap_or(0);
        at += 2;
        let size = match (op >> 13) & 7 {
            0 | 1 => 1,
            2 | 4 | 5 => 2,
            3 => 4,
            7 => 3,
            _ => {
                // Variable: a size byte, or two for the table definition.
                if op == 0xD608 {
                    let n = u16_at(grp, at).unwrap_or(0) as usize;
                    at += 1;
                    n
                } else {
                    let n = *grp.get(at).unwrap_or(&0) as usize;
                    at += 1;
                    n
                }
            }
        };
        let on = grp.get(at).is_some_and(|v| *v != 0);
        match op {
            0x2416 => props.in_table = on,
            0x2417 | 0x244C => props.row_end = on,
            _ => {}
        }
        at += size;
    }
    props
}

/// A paragraph read: its text and how it ended.
struct Para {
    text: String,
    /// `'\r'`, a paragraph mark, or `'\x07'`, a cell or row mark.
    mark: char,
    props: ParaProps,
}

/// The paragraphs of a run of characters, with fields shown by their
/// results and Word's control characters made text or left out, each handed
/// to `out` as it ends. `notes` numbers the automatic note references
/// (`\x02`) in a note story, in the format it gives; in the main text they
/// are left out. Each character kept, and each note number, is spent from
/// `budget` before it is.
fn paragraphs(
    chars: impl IntoIterator<Item = Char>,
    props: &[(u32, u32, ParaProps)],
    mut notes: Option<&mut dyn FnMut() -> String>,
    budget: &mut Budget,
    out: &mut dyn FnMut(Para, &mut Budget) -> Result<(), DocError>,
) -> Result<(), DocError> {
    let mut text = String::new();
    // Fields: whether each open one is past its separator (showing its
    // result), and how many are not, which hide what they hold. Counted as
    // they open and close, rather than by looking at every open field for
    // each character: a file opening a million fields, then holding a
    // million characters, cost their product.
    let mut fields: Vec<bool> = Vec::new();
    let mut hiding = 0usize;
    for ch in chars {
        let shown = hiding == 0;
        match ch.c {
            '\u{13}' => {
                fields.push(false);
                hiding += 1;
            }
            '\u{14}' => {
                if let Some(f) = fields.last_mut().filter(|f| !**f) {
                    *f = true;
                    hiding -= 1;
                }
            }
            '\u{15}' => {
                if fields.pop() == Some(false) {
                    hiding -= 1;
                }
            }
            '\r' | '\u{7}' => {
                let p = props_at(props, ch.fc);
                let para = Para {
                    text: std::mem::take(&mut text),
                    mark: ch.c,
                    props: p,
                };
                out(para, budget)?;
                fields.clear();
                hiding = 0;
            }
            _ if !shown => {}
            '\u{2}' => {
                if let Some(next) = notes.as_mut() {
                    let label = next();
                    budget.spend_text(&label)?;
                    text.push_str(&label);
                }
            }
            c if (c as u32) < 0x20 && !matches!(c, '\u{b}' | '\u{c}' | '\t' | '\u{1e}') => {}
            c => {
                let c = match c {
                    '\u{b}' | '\u{c}' => '\n',
                    '\t' => ' ',
                    '\u{1e}' => '-',
                    c => c,
                };
                budget.spend_chars([c])?;
                text.push(c);
            }
        }
    }
    if !text.trim().is_empty() {
        let para = Para {
            text,
            mark: '\r',
            props: ParaProps::default(),
        };
        out(para, budget)?;
    }
    Ok(())
}

/// A paragraph's text tidied: its lines trimmed, its runs of spaces one.
fn tidy(s: &str) -> String {
    s.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Paragraphs laid out as elements as they come: paragraphs, and tables of
/// the cells their marks close. Each element and each cell is spent from
/// the budget before it is made, a table's rows padded to its widest
/// included.
struct Layout<'o> {
    evidence: &'static str,
    out: &'o mut Vec<Element>,
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: Vec<String>,
    /// The cells spent for `rows`, padded to `width`, and for `row`.
    spent: u64,
    width: usize,
}

impl<'o> Layout<'o> {
    fn new(evidence: &'static str, out: &'o mut Vec<Element>) -> Self {
        Self {
            evidence,
            out,
            rows: Vec::new(),
            row: Vec::new(),
            cell: Vec::new(),
            spent: 0,
            width: 0,
        }
    }

    fn element(&self, kind: &str, text: String) -> Element {
        Element::new(kind, text, "doc", self.evidence)
    }

    /// The row read so far is one of the table's, which spends the padding
    /// it and the rows above it take to the widest.
    fn end_row(&mut self, budget: &mut Budget) -> Result<(), TooLarge> {
        if self.row.is_empty() {
            return Ok(());
        }
        self.width = self.width.max(self.row.len());
        let table = (self.rows.len() as u64 + 1).saturating_mul(self.width as u64);
        budget.spend_cells(table.saturating_sub(self.spent))?;
        self.spent = self.spent.max(table);
        self.rows.push(std::mem::take(&mut self.row));
        Ok(())
    }

    fn flush_table(&mut self, budget: &mut Budget) -> Result<(), TooLarge> {
        self.end_row(budget)?;
        if self.rows.is_empty() {
            return Ok(());
        }
        budget.spend_elements(1)?;
        let width = self.width;
        for r in self.rows.iter_mut() {
            r.resize(width, String::new());
        }
        let text = self
            .rows
            .iter()
            .map(|r| r.join(" | "))
            .collect::<Vec<_>>()
            .join("\n");
        let mut e = self.element("doco:Table", text);
        e.cells = Some(std::mem::take(&mut self.rows));
        self.out.push(e);
        (self.spent, self.width) = (0, 0);
        Ok(())
    }

    fn para(&mut self, p: Para, budget: &mut Budget) -> Result<(), DocError> {
        let text = tidy(&p.text);
        if p.mark == '\u{7}' && p.props.row_end {
            self.end_row(budget)?;
            return Ok(());
        }
        if p.mark == '\u{7}' {
            self.cell.push(text);
            budget.spend_cells(1)?;
            self.spent += 1;
            let cell = std::mem::take(&mut self.cell)
                .into_iter()
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            // A cell that holds text is a node of the output of its own.
            if !cell.is_empty() {
                budget.spend_elements(1)?;
            }
            self.row.push(cell);
            return Ok(());
        }
        if p.props.in_table {
            self.cell.push(text);
            return Ok(());
        }
        self.flush_table(budget)?;
        if !text.is_empty() {
            budget.spend_elements(1)?;
            let e = self.element("doco:Paragraph", text);
            self.out.push(e);
        }
        Ok(())
    }

    fn finish(mut self, budget: &mut Budget) -> Result<(), TooLarge> {
        self.flush_table(budget)
    }
}

/// A story's paragraphs laid out into `out`.
fn lay_out(
    chars: impl IntoIterator<Item = Char>,
    props: &[(u32, u32, ParaProps)],
    notes: Option<&mut dyn FnMut() -> String>,
    evidence: &'static str,
    budget: &mut Budget,
    out: &mut Vec<Element>,
) -> Result<(), DocError> {
    let mut layout = Layout::new(evidence, out);
    paragraphs(chars, props, notes, budget, &mut |p, b| layout.para(p, b))?;
    Ok(layout.finish(budget)?)
}

/// Roman numerals in lower case, as Word numbers endnotes by default.
fn roman(mut n: u32) -> String {
    let mut out = String::new();
    for (v, s) in [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ] {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

/// Parse a `.doc` file's bytes into elements: the body, then its
/// footnotes, endnotes and comments.
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, DocError> {
    parse_within(bytes, &mut Budget::default())
}

/// What [`parse_with_info`] reads: the elements, what the file declares
/// about itself, and what it holds that is not read.
#[derive(Debug, Clone)]
pub struct Parsed {
    pub elements: Vec<Element>,
    /// Its summary information's title and author.
    pub info: DocumentInfo,
    /// What the document holds that this reader does not read, and leaves
    /// out: `headers and footers`, `text boxes`. Said, so that it is not
    /// lost without a word.
    pub left_out: Vec<&'static str>,
}

/// [`parse`], with the title and author the summary information declares
/// and what is left out.
pub fn parse_with_info(bytes: &[u8]) -> Result<Parsed, DocError> {
    read(bytes, &mut Budget::default())
}

/// [`parse`], against `budget`.
fn parse_within(bytes: &[u8], budget: &mut Budget) -> Result<Vec<Element>, DocError> {
    read(bytes, budget).map(|p| p.elements)
}

/// The most bytes of the summary information read: a property set is a
/// few hundred bytes; a larger one is left unread.
const MAX_SUMMARY_BYTES: u64 = 32 << 10;

/// [`parse_with_info`], against `budget`.
fn read(bytes: &[u8], budget: &mut Budget) -> Result<Parsed, DocError> {
    let mut cf = cfb::CompoundFile::open(Cursor::new(bytes))
        .map_err(|e| DocError::Container(e.to_string()))?;
    if !cf.is_stream("/WordDocument") {
        return Err(DocError::NotWord);
    }
    // What the file says about itself is a nicety: one too large, or that
    // does not read, is left out.
    let info = SUMMARY_STREAMS
        .iter()
        .find(|n| cf.is_stream(n))
        .and_then(|n| stream(&mut cf, n, MAX_SUMMARY_BYTES).ok())
        .map(|b| summary(&b))
        .unwrap_or_default();
    let wd = stream(&mut cf, "/WordDocument", MAX_STREAM_BYTES)?;
    let fib = fib(&wd)?;
    if !cf.is_stream(fib.table) {
        return Err(DocError::Malformed(format!(
            "no {} stream",
            &fib.table[1..]
        )));
    }
    let table = stream(&mut cf, fib.table, MAX_TABLE_BYTES)?;
    let pieces = pieces(&table, fib.clx)?;
    let total: u64 = fib.ccp.iter().map(|&c| u64::from(c)).sum::<u64>() + u64::from(fib.ccp_boxes);
    if total > MAX_CHARS {
        return Err(DocError::TooLarge(format!(
            "{total} characters, more than the {MAX_CHARS} read"
        )));
    }
    let props = para_props(&wd, &table, fib.bte_papx)?;
    let hidden = hidden_runs(&wd, &table, fib.bte_chpx)?;
    // The stories, one after another in character positions.
    let mut starts = [0u32; 7];
    for i in 0..6 {
        starts[i + 1] = starts[i].saturating_add(fib.ccp[i]);
    }
    // Each story's characters, those a tracked deletion removed or that are
    // hidden left out, as Word shows the document: they are not its text.
    // A field's marks are kept, so its code and result stay told apart.
    let story = |i: usize| {
        chars(&wd, &pieces, starts[i], starts[i + 1])
            .filter(|c| matches!(c.c, '\u{13}'..='\u{15}') || !hidden_at(&hidden, c.fc))
    };
    let mut out = Vec::new();
    lay_out(story(0), &props, None, "doc", budget, &mut out)?;
    // Notes: footnotes (story 1) numbered 1, 2, …; endnotes (5) i, ii, ….
    for (i, evidence) in [(1, "footnote"), (5, "endnote")] {
        let mut n = 0u32;
        let mut next = || {
            n += 1;
            if evidence == "endnote" {
                roman(n)
            } else {
                n.to_string()
            }
        };
        lay_out(
            story(i),
            &props,
            Some(&mut next),
            evidence,
            budget,
            &mut out,
        )?;
    }
    // Comments (story 4), after everything Word prints.
    lay_out(story(4), &props, None, "comment", budget, &mut out)?;
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    // What is not read: the headers' story (story 2), where it holds more
    // than the notes' separators, and the text boxes' stories after the
    // endnotes'.
    let says = |mut cs: Box<dyn Iterator<Item = Char> + '_>| cs.any(|c| c.c.is_alphanumeric());
    let mut left_out = Vec::new();
    if says(Box::new(story(2))) {
        left_out.push("headers and footers");
    }
    let boxes = starts[6].saturating_add(fib.ccp_boxes);
    if says(Box::new(chars(&wd, &pieces, starts[6], boxes))) {
        left_out.push("text boxes");
    }
    Ok(Parsed {
        elements: out,
        info,
        left_out,
    })
}

/// The summary information's stream names: the property set a compound
/// file keeps its title and author in.
const SUMMARY_STREAMS: [&str; 1] = ["/\u{5}SummaryInformation"];

/// The title and author a summary information property set holds
/// (`PIDSI_TITLE`, 2, and `PIDSI_AUTHOR`, 4), as strings of its code page
/// (`PID_CODEPAGE`, 1) or UTF-16. Whatever does not read is left out.
fn summary(b: &[u8]) -> DocumentInfo {
    let mut info = DocumentInfo::default();
    // The header, then the first section's id and offset.
    let Some(section) = u32_at(b, 44).map(|o| o as usize) else {
        return info;
    };
    let Some(n) = u32_at(b, section + 4).map(|n| (n as usize).min(1024)) else {
        return info;
    };
    let prop = |i: usize| -> Option<(u32, usize)> {
        let id = u32_at(b, section + 8 + 8 * i)?;
        let at = section.checked_add(u32_at(b, section + 12 + 8 * i)? as usize)?;
        Some((id, at))
    };
    let props: Vec<(u32, usize)> = (0..n).map_while(prop).collect();
    let code_page = props
        .iter()
        .find(|(id, _)| *id == 1)
        .and_then(|&(_, at)| u16_at(b, at + 4))
        .unwrap_or(1252);
    let text = |at: usize| -> Option<String> {
        let len = (u32_at(b, at + 4)? as usize).min(MAX_SUMMARY_BYTES as usize);
        let s = match u32_at(b, at)? {
            // VT_LPSTR: bytes of the code page, a NUL at the end.
            0x1E => {
                let bytes = b.get(at + 8..at + 8 + len)?;
                let bytes = bytes.split(|&c| c == 0).next().unwrap_or_default();
                code_page_encoding(code_page)
                    .decode_without_bom_handling(bytes)
                    .0
                    .into_owned()
            }
            // VT_LPWSTR: UTF-16 code units, a NUL at the end.
            0x1F => {
                let units: Vec<u16> = b
                    .get(at + 8..at + 8 + 2 * len)?
                    .chunks_exact(2)
                    .map(|p| u16::from_le_bytes([p[0], p[1]]))
                    .take_while(|&u| u != 0)
                    .collect();
                String::from_utf16_lossy(&units)
            }
            _ => return None,
        };
        let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
        (!s.is_empty()).then_some(s)
    };
    let get = |pid: u32| {
        props
            .iter()
            .find(|(id, _)| *id == pid)
            .and_then(|&(_, at)| text(at))
    };
    info.title = get(2);
    info.creators = get(4).into_iter().collect();
    info
}

/// The encoding of a Windows code page, Windows-1252 where it is not one
/// this reader knows.
fn code_page_encoding(cp: u16) -> &'static encoding_rs::Encoding {
    let label = match cp {
        65001 => "utf-8",
        932 => "shift_jis",
        936 => "gbk",
        949 => "euc-kr",
        950 => "big5",
        874 | 1250..=1258 => {
            return encoding_rs::Encoding::for_label(format!("windows-{cp}").as_bytes())
                .unwrap_or(encoding_rs::WINDOWS_1252)
        }
        _ => "windows-1252",
    };
    encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::WINDOWS_1252)
}

#[cfg(test)]
mod tests;
