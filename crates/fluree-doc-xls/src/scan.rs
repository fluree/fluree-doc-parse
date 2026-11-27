//! The check an `.xls` passes before calamine reads it, the workbook
//! calamine is given instead of the file, and what the walk reads that
//! calamine does not give.
//!
//! A BIFF8 workbook stream is a run of records, each a type and a length
//! (two bytes each) and its data, a `CONTINUE` record carrying on the one
//! before it. The workbook's globals come first, from a `BOF` record to the
//! first `EOF`, then each sheet's, at the offset its `BOUNDSHEET8` record
//! names; a chart embedded in a sheet nests a substream of its own inside
//! the sheet's.
//!
//! calamine is not given the file. It reads a compound file with a reader
//! of its own, indexes record data without checking its length, reserves
//! what a `DIMENSIONS` record declares, and parses every formula; on input
//! written to break it, it panics or asks for gigabytes, and either ends the
//! process when panics abort. So the walk reads the file's records the way
//! calamine would, checks each one calamine would read against what calamine
//! indexes in it, counts what calamine would allocate for it, and writes a
//! new workbook stream holding only those records: the globals calamine
//! needs (`BOF`, `CODEPAGE`, `DATEMODE`, `FORMAT`, `XF`, `BOUNDSHEET8`,
//! `SST`), then each sheet's cells and merges once, its formulas emptied to
//! their cached value. [`compound`] puts that stream (and the summary
//! information, if small) in a compound file of its own for calamine to
//! open.

use crate::{XlsError, MAX_CELLS, MAX_SHEETS, MAX_STREAM_BYTES, MAX_TEXT_CHARS};
use fluree_doc_model::budget::{Meter, TooLarge};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};

/// What the walk found in one sheet, in the order the workbook lists them.
#[derive(Debug, Default, Clone)]
pub(crate) struct Sheet {
    /// Cells whose font is bold, by (row, column).
    pub bold: HashSet<(u32, u32)>,
    /// Rows frozen at the top by the sheet's pane.
    pub frozen_rows: u32,
}

/// The walk's result: each sheet's bold cells and frozen rows, and the
/// workbook stream calamine reads.
pub(crate) struct Scanned {
    pub sheets: Vec<Sheet>,
    pub stream: Vec<u8>,
}

const BOF: u16 = 0x0809;
const EOF: u16 = 0x000A;
const CONTINUE: u16 = 0x003C;
const FILEPASS: u16 = 0x002F;
const CODEPAGE: u16 = 0x0042;
const DATEMODE: u16 = 0x0022;
const FORMAT: u16 = 0x041E;
const FONT: u16 = 0x0031;
const XF: u16 = 0x00E0;
const BOUNDSHEET: u16 = 0x0085;
const SST: u16 = 0x00FC;
const WINDOW2: u16 = 0x023E;
const PANE: u16 = 0x0041;
const NUMBER: u16 = 0x0203;
const LABEL: u16 = 0x0204;
const RSTRING: u16 = 0x00D6;
const BOOLERR: u16 = 0x0205;
const STRING: u16 = 0x0207;
const RK: u16 = 0x027E;
const LABELSST: u16 = 0x00FD;
const MULRK: u16 = 0x00BD;
const MERGECELLS: u16 = 0x00E5;
const FORMULA: u16 = 0x0006;

/// The most `CONTINUE` records one record may carry: calamine takes each
/// from the front of a list, in time the square of their number. 8,192 of
/// them hold a shared string table of 64 MiB.
const MAX_CONTINUES: usize = 8_192;

/// The most bytes of summary information handed on: a property set is a
/// few hundred bytes, and calamine decodes every property it lists.
const MAX_SUMMARY_BYTES: u64 = 32 << 10;

/// The workbook stream's name, where `bytes` is a compound file that has
/// one at its root.
pub(crate) fn workbook_stream_name(bytes: &[u8]) -> Option<&'static str> {
    let cf = cfb::CompoundFile::open(std::io::Cursor::new(bytes)).ok()?;
    ["/Workbook", "/Book"].into_iter().find(|n| cf.is_stream(n))
}

const SUMMARY: &str = "/\u{5}SummaryInformation";

/// Open the compound file, read its workbook stream and walk it: refused
/// where a bound in the crate documentation is passed or a record is one
/// calamine would misread. With it, the summary information stream where
/// there is one of at most [`MAX_SUMMARY_BYTES`].
pub(crate) fn scan(bytes: &[u8]) -> Result<(Scanned, Option<Vec<u8>>), XlsError> {
    let mut cf = cfb::CompoundFile::open(std::io::Cursor::new(bytes))
        .map_err(|e| XlsError::Container(e.to_string()))?;
    let name = ["/Workbook", "/Book"]
        .into_iter()
        .find(|n| cf.is_stream(n))
        .ok_or(XlsError::NoWorkbook)?;
    let data = read_stream(&mut cf, name, MAX_STREAM_BYTES)?.ok_or_else(|| {
        XlsError::TooLarge(format!(
            "a workbook stream of more than the {MAX_STREAM_BYTES} bytes read"
        ))
    })?;
    // Properties are a nicety: a summary that is too large or unreadable is
    // left out, not a reason to refuse the workbook.
    let summary = if cf.is_stream(SUMMARY) {
        read_stream(&mut cf, SUMMARY, MAX_SUMMARY_BYTES)
            .ok()
            .flatten()
    } else {
        None
    };
    Ok((walk(&data)?, summary))
}

/// A stream's bytes, or `None` where it holds more than `max`.
fn read_stream(
    cf: &mut cfb::CompoundFile<std::io::Cursor<&[u8]>>,
    name: &str,
    max: u64,
) -> Result<Option<Vec<u8>>, XlsError> {
    let stream = cf
        .open_stream(name)
        .map_err(|e| XlsError::Container(e.to_string()))?;
    if stream.len() > max {
        return Ok(None);
    }
    let mut data = Vec::new();
    stream
        .take(max)
        .read_to_end(&mut data)
        .map_err(|e| XlsError::Container(e.to_string()))?;
    Ok(Some(data))
}

/// A compound file holding `stream` as its workbook and `summary` as its
/// summary information: all calamine opens.
pub(crate) fn compound(stream: &[u8], summary: Option<&[u8]>) -> Result<Vec<u8>, XlsError> {
    let io = |e: std::io::Error| XlsError::Container(e.to_string());
    let mut cf =
        cfb::CompoundFile::create_with_version(cfb::Version::V3, std::io::Cursor::new(Vec::new()))
            .map_err(io)?;
    cf.create_stream("/Workbook")
        .and_then(|mut s| s.write_all(stream))
        .map_err(io)?;
    if let Some(summary) = summary {
        cf.create_stream(SUMMARY)
            .and_then(|mut s| s.write_all(summary))
            .map_err(io)?;
    }
    cf.flush().map_err(io)?;
    Ok(cf.into_inner().into_inner())
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn malformed(what: &str) -> XlsError {
    XlsError::Biff(what.to_string())
}

fn too_large(e: TooLarge) -> XlsError {
    XlsError::TooLarge(e.to_string())
}

/// One record as calamine groups them: its type, its data, the data of the
/// `CONTINUE` records right after it.
struct Rec<'a> {
    typ: u16,
    data: &'a [u8],
    cont: Vec<&'a [u8]>,
}

/// A record's type, data, and where the record after it starts.
type Header<'a> = (u16, &'a [u8], usize);

/// The record at `at` and where the next one starts; `None` at the end.
fn record(stream: &[u8], at: usize) -> Result<Option<(Rec<'_>, usize)>, XlsError> {
    let header = |at: usize| -> Result<Option<Header<'_>>, XlsError> {
        if at >= stream.len() {
            return Ok(None);
        }
        let (Some(typ), Some(len)) = (u16_at(stream, at), u16_at(stream, at + 2)) else {
            return Err(malformed("a record header is cut short"));
        };
        let end = at + 4 + usize::from(len);
        let data = stream
            .get(at + 4..end)
            .ok_or_else(|| malformed("a record runs past the end of the stream"))?;
        Ok(Some((typ, data, end)))
    };
    let Some((typ, data, mut next)) = header(at)? else {
        return Ok(None);
    };
    let mut cont = Vec::new();
    while u16_at(stream, next) == Some(CONTINUE) {
        let Some((_, d, end)) = header(next)? else {
            break;
        };
        if cont.len() == MAX_CONTINUES {
            return Err(XlsError::TooLarge(format!(
                "a record continued more than {MAX_CONTINUES} times"
            )));
        }
        cont.push(d);
        next = end;
    }
    Ok(Some((Rec { typ, data, cont }, next)))
}

fn emit(out: &mut Vec<u8>, typ: u16, data: &[u8]) {
    out.extend(typ.to_le_bytes());
    // Every record emitted is one read, or shorter: its length fits.
    out.extend((data.len() as u16).to_le_bytes());
    out.extend(data);
}

/// The length, in characters, of each string of a shared string table, read
/// as calamine reads it (`parse_sst`, `read_rich_extended_string`,
/// `read_dbcs`), refused wherever calamine would index past what is there.
fn sst_lengths(r: &Rec<'_>, budget: &mut Meter) -> Result<Vec<u32>, XlsError> {
    let short = || malformed("the shared string table is cut short");
    let mut data = r.data.get(8..).ok_or_else(short)?;
    let mut conts = r.cont.iter();
    let mut out = Vec::new();
    loop {
        if data.is_empty() {
            match conts.next() {
                Some(d) => data = d,
                None => break,
            }
        }
        if out.len() as u64 >= MAX_CELLS {
            return Err(XlsError::TooLarge(format!(
                "more than {MAX_CELLS} shared strings"
            )));
        }
        // An empty continuation is an empty string.
        if data.is_empty() {
            out.push(0);
            continue;
        }
        if data.len() < 3 {
            return Err(short());
        }
        let cch = u16_at(data, 0).unwrap_or(0);
        let flags = data[2];
        data = &data[3..];
        let mut high = flags & 0x1 != 0;
        let mut skip = 0usize;
        if flags & 0x8 != 0 {
            skip += 4 * usize::from(u16_at(data, 0).ok_or_else(short)?);
            data = &data[2..];
        }
        if flags & 0x4 != 0 {
            let n = u32_at(data, 0).ok_or_else(short)?;
            skip += usize::try_from(i32::from_le_bytes(n.to_le_bytes()))
                .map_err(|_| malformed("a shared string's extension has a negative size"))?;
            data = &data[4..];
        }
        budget.spend(u64::from(cch)).map_err(too_large)?;
        // The characters, carried on into continuations, each opening with
        // the flag saying whether its characters are one byte or two.
        let mut left = usize::from(cch);
        loop {
            let width = if high { 2 } else { 1 };
            let l = (data.len() / width).min(left);
            data = &data[l * width..];
            left -= l;
            if left == 0 {
                break;
            }
            data = conts.next().ok_or_else(short)?;
            let (&flag, rest) = data.split_first().ok_or_else(short)?;
            high = flag & 0x1 != 0;
            data = rest;
        }
        // Formatting runs and phonetic data, carried on likewise.
        while skip > 0 {
            if data.is_empty() {
                data = conts.next().ok_or_else(short)?;
            }
            let l = skip.min(data.len());
            data = &data[l..];
            skip -= l;
        }
        out.push(u32::from(cch));
    }
    Ok(out)
}

/// A sheet's records, read from its offset as calamine reads them.
#[derive(Default)]
struct SheetRead {
    sheet: Sheet,
    /// The records calamine is given, `BOF` and `EOF` included.
    records: Vec<u8>,
    /// Cells calamine would make, characters it would allocate for them,
    /// and merged ranges it would list.
    cells: u64,
    chars: u64,
    merges: u64,
    /// First and last row and column a cell or formula names: calamine
    /// lays the sheet out as a dense grid over them.
    extent: Option<(u32, u32, u32, u32)>,
}

impl SheetRead {
    fn add(&mut self, row: u32, col: u32) {
        let e = self.extent.get_or_insert((row, row, col, col));
        *e = (e.0.min(row), e.1.max(row), e.2.min(col), e.3.max(col));
    }

    fn area(&self) -> u64 {
        self.extent.map_or(0, |(r0, r1, c0, c1)| {
            u64::from(r1 - r0 + 1) * u64::from(c1 - c0 + 1)
        })
    }
}

/// The globals a walk keeps.
struct Globals<'a> {
    xf_bold: Vec<bool>,
    sst: Vec<u32>,
    /// Each `BOUNDSHEET8`: the offset it names and its data past the offset.
    sheets: Vec<(usize, &'a [u8])>,
    records: Vec<u8>,
    /// The `SST` record and its continuations, emitted last.
    sst_records: Vec<u8>,
}

fn globals<'a>(stream: &'a [u8], budget: &mut Meter) -> Result<Globals<'a>, XlsError> {
    let mut g = Globals {
        xf_bold: Vec::new(),
        sst: Vec::new(),
        sheets: Vec::new(),
        records: Vec::new(),
        sst_records: Vec::new(),
    };
    let mut font_bold: Vec<bool> = Vec::new();
    let mut at = 0usize;
    let mut first = true;
    // calamine reads the globals to the first EOF, whatever the nesting.
    while let Some((r, next)) = record(stream, at)? {
        at = next;
        let d = r.data;
        if first {
            if r.typ != BOF || d.len() < 2 {
                return Err(malformed("the workbook stream does not open with a BOF"));
            }
            emit(&mut g.records, BOF, d);
            first = false;
            continue;
        }
        let short = |what: &str| malformed(&format!("a {what} record is cut short"));
        match r.typ {
            EOF => break,
            FILEPASS => return Err(malformed("the workbook is encrypted")),
            CODEPAGE | DATEMODE => {
                if d.len() < 2 {
                    return Err(short("code page or date mode"));
                }
                emit(&mut g.records, r.typ, d);
            }
            FORMAT if d.len() >= 2 => emit(&mut g.records, r.typ, d),
            FONT => font_bold.push(u16_at(d, 6).is_some_and(|w| w >= 700)),
            XF => {
                if d.len() < 4 {
                    return Err(short("cell format"));
                }
                // Font index 4 is never written: the indices above it count
                // from one past it.
                let f = usize::from(u16_at(d, 0).unwrap_or(0));
                let f = if f > 4 { f - 1 } else { f };
                g.xf_bold.push(font_bold.get(f).copied().unwrap_or(false));
                emit(&mut g.records, r.typ, d);
            }
            BOUNDSHEET => {
                if d.len() < 6 {
                    return Err(short("sheet"));
                }
                if g.sheets.len() >= MAX_SHEETS {
                    return Err(XlsError::TooLarge(format!(
                        "more than {MAX_SHEETS} sheet records"
                    )));
                }
                let pos = u32_at(d, 0).unwrap_or(0) as usize;
                g.sheets.push((pos, &d[4..]));
            }
            SST => {
                g.sst = sst_lengths(&r, budget)?;
                g.sst_records.clear();
                emit(&mut g.sst_records, SST, d);
                for c in &r.cont {
                    emit(&mut g.sst_records, CONTINUE, c);
                }
            }
            _ => {}
        }
    }
    Ok(g)
}

/// Read the sheet at `pos` as calamine does: its first record its `BOF`,
/// nested substreams skipped, to its `EOF`. Each byte of the records walked
/// is spent from `walked`.
fn sheet(
    stream: &[u8],
    pos: usize,
    g: &Globals<'_>,
    budget: &mut Meter,
    walked: &mut Meter,
) -> Result<SheetRead, XlsError> {
    let mut s = SheetRead::default();
    let not_a_sheet = || malformed("a sheet's offset is not where a sheet starts");
    let Some((first, mut at)) = record(stream, pos)? else {
        return Err(not_a_sheet());
    };
    if first.typ != BOF {
        return Err(not_a_sheet());
    }
    emit(&mut s.records, BOF, first.data);
    let bold = |ixfe: Option<u16>| {
        ixfe.and_then(|x| g.xf_bold.get(usize::from(x)).copied())
            .unwrap_or(false)
    };
    let short = |what: &str| malformed(&format!("a {what} record is cut short"));
    let mut depth = 0usize;
    let mut frozen = false;
    // Where the last formula is: its string value, in a STRING record,
    // goes there.
    let mut formula_at = (0u32, 0u32);
    while let Some((r, next)) = record(stream, at)? {
        walked.spend_len(next - at).map_err(too_large)?;
        at = next;
        let d = r.data;
        if depth > 0 {
            match r.typ {
                BOF => depth += 1,
                EOF => depth -= 1,
                _ => {}
            }
            continue;
        }
        let cell = |d: &[u8], min: usize, what: &str| -> Result<(u32, u32), XlsError> {
            if d.len() < min {
                return Err(short(what));
            }
            Ok((
                u32::from(u16_at(d, 0).unwrap_or(0)),
                u32::from(u16_at(d, 2).unwrap_or(0)),
            ))
        };
        let mut chars = 0u64;
        match r.typ {
            BOF => {
                depth = 1;
                continue;
            }
            EOF => break,
            NUMBER | RK | BOOLERR | LABELSST | LABEL | RSTRING => {
                let min = match r.typ {
                    NUMBER => 14,
                    RK | LABELSST => 10,
                    BOOLERR => 8,
                    _ => 6,
                };
                let (row, col) = cell(d, min, "cell")?;
                s.add(row, col);
                s.cells += 1;
                if bold(u16_at(d, 4)) {
                    s.sheet.bold.insert((row, col));
                }
                chars = match r.typ {
                    LABELSST => u32_at(d, 6)
                        .and_then(|i| g.sst.get(i as usize))
                        .map_or(0, |&n| u64::from(n)),
                    LABEL | RSTRING => u64::from(u16_at(d, 6).unwrap_or(0)),
                    _ => 0,
                };
                emit(&mut s.records, r.typ, d);
            }
            STRING => {
                s.add(formula_at.0, formula_at.1);
                s.cells += 1;
                chars = u64::from(u16_at(d, 0).unwrap_or(0));
                emit(&mut s.records, r.typ, d);
            }
            FORMULA => {
                let (row, col) = cell(d, 20, "formula")?;
                formula_at = (row, col);
                s.add(row, col);
                // Its cached value, and the text calamine writes for a
                // formula it cannot rebuild.
                s.cells += 1;
                chars = 80;
                if bold(u16_at(d, 4)) {
                    s.sheet.bold.insert((row, col));
                }
                // The formula's tokens are not passed on: calamine indexes
                // them unchecked, and they are not read.
                let mut emptied = d[..20].to_vec();
                emptied.extend([0, 0]);
                emit(&mut s.records, r.typ, &emptied);
            }
            MULRK => {
                // Row, first column, a format and a value per cell, then the
                // last column; calamine refuses any other length.
                let (row, first) = cell(d, 6, "multiple-number")?;
                let last = u32::from(u16_at(d, d.len() - 2).unwrap_or(0));
                if last < first || d.len() != 6 + 6 * (last - first + 1) as usize {
                    return Err(malformed(
                        "a multiple-number record's length is not its cells'",
                    ));
                }
                for k in 0..=(last - first) {
                    s.add(row, first + k);
                    s.cells += 1;
                    if bold(u16_at(d, 4 + 6 * k as usize)) {
                        s.sheet.bold.insert((row, first + k));
                    }
                }
                emit(&mut s.records, r.typ, d);
            }
            MERGECELLS => {
                let n = usize::from(u16_at(d, 0).ok_or_else(|| short("merge"))?);
                let len = 2 + 8 * n;
                if d.len() < len {
                    return Err(short("merge"));
                }
                s.merges += n as u64;
                emit(&mut s.records, r.typ, &d[..len]);
            }
            WINDOW2 => frozen = u16_at(d, 0).is_some_and(|g| g & 0x0008 != 0),
            PANE if frozen => s.sheet.frozen_rows = u32::from(u16_at(d, 2).unwrap_or(0)),
            _ => {}
        }
        s.chars += chars;
        budget.spend(chars).map_err(too_large)?;
    }
    emit(&mut s.records, EOF, &[]);
    Ok(s)
}

/// Walk a workbook stream's records, and write the stream calamine reads.
pub(crate) fn walk(stream: &[u8]) -> Result<Scanned, XlsError> {
    let mut chars = Meter::new(MAX_TEXT_CHARS, "characters of text");
    let g = globals(stream, &mut chars)?;
    // Each sheet read once, however many sheet records name it; calamine
    // reads it again for each, and each counts.
    let mut read: HashMap<usize, SheetRead> = HashMap::new();
    let mut order: Vec<usize> = Vec::new();
    let mut cells = Meter::new(MAX_CELLS, "cells");
    let mut area = Meter::new(MAX_CELLS, "cells laid out");
    let mut merges = Meter::new(MAX_CELLS, "merged ranges");
    // The sheets' records walked, each sheet once: sheets lie apart in the
    // stream, so their walks cover it at most once. Sheets laid inside each
    // other (each a `BOF` further in, the next skipped as a nested
    // substream) are each walked to the end, the square of the stream in
    // all; past twice the stream, the workbook is refused.
    let mut walked = Meter::new(
        2 * stream.len() as u64,
        "bytes of sheet records walked, twice the stream",
    );
    for &(pos, _) in &g.sheets {
        if let Some(s) = read.get(&pos) {
            chars.spend(s.chars).map_err(too_large)?;
        } else {
            read.insert(pos, sheet(stream, pos, &g, &mut chars, &mut walked)?);
            order.push(pos);
        }
        let s = &read[&pos];
        let hold = |e: TooLarge| XlsError::TooLarge(format!("its sheets hold {e}"));
        cells.spend(s.cells).map_err(hold)?;
        area.spend(s.area()).map_err(hold)?;
        merges.spend(s.merges).map_err(hold)?;
    }
    // The stream: the globals, the sheet records at their new offsets, the
    // shared strings, the end of the globals; then each sheet once.
    let sheet_record = |pos: u32, rest: &[u8]| {
        let mut d = pos.to_le_bytes().to_vec();
        d.extend(rest);
        d
    };
    let head = g.records.len()
        + g.sheets
            .iter()
            .map(|(_, rest)| 4 + 4 + rest.len())
            .sum::<usize>()
        + g.sst_records.len()
        + 4;
    let mut at_new: HashMap<usize, u32> = HashMap::new();
    let mut at = head;
    for pos in &order {
        at_new.insert(*pos, at as u32);
        at += read[pos].records.len();
    }
    let mut out = Vec::with_capacity(at);
    out.extend(&g.records);
    for (pos, rest) in &g.sheets {
        emit(&mut out, BOUNDSHEET, &sheet_record(at_new[pos], rest));
    }
    out.extend(&g.sst_records);
    emit(&mut out, EOF, &[]);
    for pos in &order {
        out.extend(&read[pos].records);
    }
    let sheets = g
        .sheets
        .iter()
        .map(|(pos, _)| read[pos].sheet.clone())
        .collect();
    Ok(Scanned {
        sheets,
        stream: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::rec;

    fn bof(dt: u16) -> Vec<u8> {
        let mut d = 0x0600u16.to_le_bytes().to_vec();
        d.extend(dt.to_le_bytes());
        d.extend([0u8; 12]);
        rec(BOF, &d)
    }

    /// A workbook stream whose one sheet, of `body`, is listed `times`
    /// times, after `globals`.
    fn listed(globals: &[Vec<u8>], body: &[Vec<u8>], times: usize) -> Vec<u8> {
        let head = |pos: u32| {
            let mut g = vec![bof(5)];
            g.extend(globals.iter().cloned());
            for _ in 0..times {
                let mut d = pos.to_le_bytes().to_vec();
                d.extend([0, 0, 1, 0, b'S']);
                g.push(rec(BOUNDSHEET, &d));
            }
            g.push(rec(EOF, &[]));
            g.concat()
        };
        let pos = head(0).len() as u32;
        [head(pos), bof(0x10), body.concat(), rec(EOF, &[])].concat()
    }

    /// A LABEL cell declaring `cch` characters, which calamine allocates.
    fn label(row: u16, cch: u16) -> Vec<u8> {
        let d: Vec<u8> = [row, 0, 0, cch]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        rec(LABEL, &d)
    }

    fn too_large(r: Result<Scanned, XlsError>, what: &str) -> bool {
        matches!(r, Err(XlsError::TooLarge(ref m)) if m.contains(what))
    }

    #[test]
    fn a_sheet_listed_again_counts_its_text_again() {
        // 36 million characters a listing, 64 million read at most.
        let body: Vec<Vec<u8>> = (0..600).map(|r| label(r, 60_000)).collect();
        assert!(walk(&listed(&[], &body, 1)).is_ok());
        assert!(too_large(walk(&listed(&[], &body, 2)), "characters"));
    }

    #[test]
    fn a_sheet_listed_again_counts_its_merges_again() {
        // A record of 8,191 merged ranges, the most its length holds, and
        // about four million ranges read at most.
        let mut m = 8_191u16.to_le_bytes().to_vec();
        for i in 0..8_191u16 {
            m.extend([i, i, 0, 1].iter().flat_map(|v| v.to_le_bytes()));
        }
        let body = [rec(MERGECELLS, &m)];
        assert!(walk(&listed(&[], &body, 512)).is_ok());
        assert!(too_large(walk(&listed(&[], &body, 513)), "merged"));
    }

    #[test]
    fn a_record_continued_past_its_bound_is_refused() {
        // Empty continuations of the shared string table, each an empty
        // string to calamine, which takes each from the front of a list.
        let sst = rec(SST, &[0u8; 8]);
        let conts = |n: usize| vec![rec(CONTINUE, &[]); n];
        let globals = |n: usize| [vec![sst.clone()], conts(n)].concat();
        assert!(walk(&listed(&globals(MAX_CONTINUES), &[], 1)).is_ok());
        assert!(too_large(
            walk(&listed(&globals(MAX_CONTINUES + 1), &[], 1)),
            "continued"
        ));
    }

    #[test]
    fn the_shared_strings_count_their_characters() {
        let mut sst = [2u32, 2]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<u8>>();
        for s in ["abcdef", "ghijk"] {
            sst.extend((s.len() as u16).to_le_bytes());
            sst.push(0);
            sst.extend(s.as_bytes());
        }
        let bytes = rec(SST, &sst);
        let (r, _) = record(&bytes, 0).unwrap().unwrap();
        let mut budget = Meter::new(11, "characters of text");
        assert_eq!(sst_lengths(&r, &mut budget).unwrap(), [6, 5]);
        let mut budget = Meter::new(10, "characters of text");
        assert!(matches!(
            sst_lengths(&r, &mut budget),
            Err(XlsError::TooLarge(ref m)) if m.contains("characters")
        ));
    }
}
