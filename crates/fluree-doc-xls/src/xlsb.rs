//! Excel binary workbooks (`.xlsb`): a zip of BIFF12 record streams,
//! read here record by record and laid out as an `.xls` is.
//!
//! calamine reads `.xlsb` too, but indexes the data of the records it reads
//! without checking their length, and a shared string a cell names without
//! checking it is there: on a file written to break it, it panics, which
//! ends the process where panics abort. The records a sheet needs are few
//! (a workbook's sheet list, its shared strings and number formats, a
//! sheet's rows and cells), so they are read here, each checked against its
//! length. The package is first inflated once, to nowhere, against
//! [`MAX_PART_BYTES`] each part and [`MAX_TOTAL_BYTES`] in all, and its
//! entries counted; a sheet part is read once however many times the
//! workbook lists it, at most [`crate::MAX_CELLS`] cells are read, a cell
//! past Excel's grid is none, and each cell and its text (a shared string
//! for each cell that names it) are spent from the document's [`Budget`]
//! before they are copied.

use crate::{value, XlsError, MAX_CELLS};
use fluree_doc_model::budget::{Budget, TooLarge};
use fluree_doc_model::{DocumentInfo, Element};
use fluree_doc_xlsx::{GridCell, GridSheet};
use fluree_doc_xlsx::{MAX_COLUMNS, MAX_ROWS};
use std::io::{Cursor, Read};

/// The most bytes one part inflates to: 64 MiB (a sixteenth built for the
/// fuzz targets, as the budget is, [`fluree_doc_model::budget::scaled`]).
pub const MAX_PART_BYTES: u64 = fluree_doc_model::budget::scaled(64 << 20);
/// The most bytes all parts inflate to: 256 MiB (scaled as
/// [`MAX_PART_BYTES`]).
pub const MAX_TOTAL_BYTES: u64 = fluree_doc_model::budget::scaled(256 << 20);
/// The most entries the archive may hold.
pub const MAX_ENTRIES: usize = 10_000;

/// Whether `bytes` is an Excel binary workbook: a zip holding
/// `xl/workbook.bin`.
pub fn is_xlsb(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK")
        && fluree_doc_ooxml::Package::open(bytes)
            .is_ok_and(|p| p.part_names().iter().any(|n| n == "xl/workbook.bin"))
}

/// Inflate every entry against the bounds, reading nothing out.
fn check(bytes: &[u8]) -> Result<(), XlsError> {
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| XlsError::Package(e.to_string()))?;
    if zip.len() > MAX_ENTRIES {
        return Err(XlsError::TooLarge(format!("more than {MAX_ENTRIES} parts")));
    }
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| XlsError::Package(e.to_string()))?;
        let name = entry.name().map(|n| n.into_owned()).unwrap_or_default();
        let n = std::io::copy(
            &mut (&mut entry).take(MAX_PART_BYTES + 1),
            &mut std::io::sink(),
        )
        .map_err(|e| XlsError::Package(format!("{name}: {e}")))?;
        if n > MAX_PART_BYTES {
            return Err(XlsError::TooLarge(format!(
                "{name} inflates past {MAX_PART_BYTES} bytes"
            )));
        }
        total += n;
        if total > MAX_TOTAL_BYTES {
            return Err(XlsError::TooLarge(format!(
                "its parts inflate past {MAX_TOTAL_BYTES} bytes"
            )));
        }
    }
    Ok(())
}

/// Parse an `.xlsb` file's bytes into elements, in sheet order, with its
/// core properties.
pub fn parse_xlsb(bytes: &[u8]) -> Result<(Vec<Element>, DocumentInfo), XlsError> {
    parse_within(bytes, &mut Budget::default())
}

/// [`parse_xlsb`], against `budget`.
fn parse_within(
    bytes: &[u8],
    budget: &mut Budget,
) -> Result<(Vec<Element>, DocumentInfo), XlsError> {
    check(bytes)?;
    let sheets = read(bytes, budget)?;
    Ok((
        fluree_doc_xlsx::grid_elements(sheets, "xlsb", budget).map_err(too_large)?,
        fluree_doc_ooxml::info(bytes),
    ))
}

fn too_large(e: TooLarge) -> XlsError {
    XlsError::TooLarge(e.to_string())
}

/// One BIFF12 record: its type and its data.
struct Rec<'a> {
    typ: u16,
    data: &'a [u8],
}

/// The records of a part, in order; an error where one runs past the end.
fn records(part: &[u8]) -> impl Iterator<Item = Result<Rec<'_>, XlsError>> {
    let cut = || XlsError::Biff("a record runs past the end of its part".into());
    let mut at = 0usize;
    std::iter::from_fn(move || {
        if at >= part.len() {
            return None;
        }
        // The type: one or two bytes of seven bits; the length: up to
        // four.
        let mut byte = || {
            let b = part.get(at).copied();
            at += 1;
            b
        };
        let Some(b) = byte() else {
            return Some(Err(cut()));
        };
        let mut typ = u16::from(b & 0x7F);
        if b & 0x80 != 0 {
            let Some(b) = byte() else {
                return Some(Err(cut()));
            };
            typ |= u16::from(b & 0x7F) << 7;
        }
        let mut len = 0usize;
        for i in 0..4 {
            let Some(b) = byte() else {
                return Some(Err(cut()));
            };
            len |= usize::from(b & 0x7F) << (7 * i);
            if b & 0x80 == 0 {
                break;
            }
        }
        let Some(data) = part.get(at..at + len) else {
            at = part.len();
            return Some(Err(cut()));
        };
        at += len;
        Some(Ok(Rec { typ, data }))
    })
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// An `XLWideString` at `at`: its length in characters, then UTF-16; the
/// string and where it ends. `None` where it runs past `d`, or is null.
fn wide(d: &[u8], at: usize) -> Option<(String, usize)> {
    let n = u32_at(d, at)? as usize;
    let end = n.checked_mul(2)?.checked_add(at + 4)?;
    let units: Vec<u16> = d
        .get(at + 4..end)?
        .chunks_exact(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .collect();
    Some((String::from_utf16_lossy(&units), end))
}

fn malformed(what: &str) -> XlsError {
    XlsError::Biff(what.to_string())
}

/// What the workbook part says: whether it counts dates from 1904, and its
/// sheets, each its name and its relationship id; at most [`MAX_ENTRIES`]
/// of them, as many as the package may hold parts.
fn workbook(part: &[u8]) -> Result<(bool, Vec<(String, String)>), XlsError> {
    let mut date1904 = false;
    let mut sheets = Vec::new();
    for r in records(part) {
        let r = r?;
        match r.typ {
            // BrtWbProp: its flags, the first of which is the 1904 system.
            0x0099 => date1904 = u32_at(r.data, 0).is_some_and(|f| f & 1 != 0),
            // BrtBundleSh: state, tab id, relationship id, name.
            0x009C => {
                if u32_at(r.data, 8) == Some(u32::MAX) {
                    continue;
                }
                let cut = || malformed("a sheet record is cut short");
                let (rel, end) = wide(r.data, 8).ok_or_else(cut)?;
                let (name, _) = wide(r.data, end).ok_or_else(cut)?;
                if sheets.len() == MAX_ENTRIES {
                    return Err(XlsError::TooLarge(format!(
                        "more than {MAX_ENTRIES} sheets listed"
                    )));
                }
                sheets.push((name, rel));
            }
            // BrtEndBundleShs.
            0x0090 => break,
            _ => {}
        }
    }
    Ok((date1904, sheets))
}

/// The cell formats the styles part declares, in order: whether each
/// shows a date. A built-in format is one by its id; a custom one by its
/// code. An elapsed time (`[h]:mm`) is a number, as calamine reads it.
fn date_formats(part: &[u8]) -> Result<Vec<bool>, XlsError> {
    let mut custom: std::collections::HashMap<u16, bool> = std::collections::HashMap::new();
    let mut out = Vec::new();
    let mut in_xfs = false;
    for r in records(part) {
        let r = r?;
        match r.typ {
            // BrtFmt: an id and its code.
            0x002C => {
                let (Some(id), Some((code, _))) = (u16_at(r.data, 0), wide(r.data, 2)) else {
                    return Err(malformed("a number format record is cut short"));
                };
                let elapsed = ["[h", "[m", "[s"]
                    .iter()
                    .any(|p| code.to_ascii_lowercase().contains(p));
                let date = matches!(
                    fluree_doc_xlsx::custom_fmt(&code),
                    fluree_doc_xlsx::NumFmt::Date
                        | fluree_doc_xlsx::NumFmt::Time
                        | fluree_doc_xlsx::NumFmt::DateTime
                );
                custom.insert(id, date && !elapsed);
            }
            // BrtBeginCellXFs, BrtEndCellXFs: the cell formats between.
            0x0269 => in_xfs = true,
            0x026A => in_xfs = false,
            // BrtXF: its number format's id second.
            0x002F if in_xfs => {
                let id = u16_at(r.data, 2)
                    .ok_or_else(|| malformed("a cell format record is cut short"))?;
                out.push(
                    custom
                        .get(&id)
                        .copied()
                        .unwrap_or(matches!(id, 14..=22 | 27..=36 | 50..=58)),
                );
            }
            _ => {}
        }
    }
    Ok(out)
}

/// The shared strings, in order: no more text than their part holds, which
/// is read to at most [`MAX_PART_BYTES`], and each entry spent from
/// `budget` as a cell slot before it is kept (an empty one is a record of
/// seven bytes, and a string in the table). A copy of one is spent where a
/// cell names it.
fn shared_strings(part: &[u8], budget: &mut Budget) -> Result<Vec<String>, XlsError> {
    let mut out = Vec::new();
    for r in records(part) {
        let r = r?;
        // BrtSSTItem: a flags byte, then the string.
        if r.typ == 0x0013 {
            let (s, _) =
                wide(r.data, 1).ok_or_else(|| malformed("a shared string record is cut short"))?;
            budget.spend_cells(1).map_err(too_large)?;
            out.push(s);
        }
    }
    Ok(out)
}

/// A cell's value, as calamine gives it, from a value record: its data past
/// the column and style. `None` for a record that is no value.
fn cell_value(
    typ: u16,
    v: &[u8],
    date: bool,
    date1904: bool,
    strings: &[String],
    budget: &mut Budget,
) -> Result<Option<calamine::Data>, XlsError> {
    use calamine::{CellErrorType, Data, ExcelDateTime, ExcelDateTimeType};
    let cut = || malformed("a cell record is cut short");
    let number = |n: f64| {
        if date {
            Data::DateTime(ExcelDateTime::new(n, ExcelDateTimeType::DateTime, date1904))
        } else {
            Data::Float(n)
        }
    };
    Ok(Some(match typ {
        // BrtCellRk: a number in thirty bits, an integer or the high bits of
        // a double, divided by a hundred where its flag says.
        0x0002 => {
            let rk = u32_at(v, 0).ok_or_else(cut)?;
            let n = if rk & 2 != 0 {
                f64::from((rk as i32) >> 2)
            } else {
                f64::from_bits(u64::from(rk & 0xFFFF_FFFC) << 32)
            };
            number(if rk & 1 != 0 { n / 100.0 } else { n })
        }
        // BrtCellReal, BrtFmlaNum.
        0x0005 | 0x0009 => number(f64::from_le_bytes(
            v.get(..8).ok_or_else(cut)?.try_into().map_err(|_| cut())?,
        )),
        // BrtCellBool, BrtFmlaBool.
        0x0004 | 0x000A => Data::Bool(*v.first().ok_or_else(cut)? != 0),
        // BrtCellError, BrtFmlaError.
        0x0003 | 0x000B => Data::Error(match *v.first().ok_or_else(cut)? {
            0x00 => CellErrorType::Null,
            0x07 => CellErrorType::Div0,
            0x0F => CellErrorType::Value,
            0x17 => CellErrorType::Ref,
            0x1D => CellErrorType::Name,
            0x24 => CellErrorType::Num,
            0x2A => CellErrorType::NA,
            0x2B => CellErrorType::GettingData,
            e => return Err(malformed(&format!("an unknown error value {e:#04x}"))),
        }),
        // BrtCellSt, BrtFmlaString: the text in the record.
        0x0006 | 0x0008 => {
            let (s, _) = wide(v, 0).ok_or_else(cut)?;
            budget.spend_text(&s).map_err(too_large)?;
            Data::String(s)
        }
        // BrtCellIsst: a shared string, copied for this cell.
        0x0007 => {
            let i = u32_at(v, 0).ok_or_else(cut)? as usize;
            let s = strings
                .get(i)
                .ok_or_else(|| malformed("a cell names a shared string that is not there"))?;
            budget.spend_text(s).map_err(too_large)?;
            Data::String(s.clone())
        }
        _ => return Ok(None),
    }))
}

fn read(bytes: &[u8], budget: &mut Budget) -> Result<Vec<GridSheet>, XlsError> {
    let mut pkg = fluree_doc_ooxml::Package::open(bytes).map_err(XlsError::Package)?;
    let part = |pkg: &mut fluree_doc_ooxml::Package<'_>, name: &str| {
        pkg.read_bytes(name, MAX_PART_BYTES)
            .map_err(XlsError::Package)
    };
    let book = "xl/workbook.bin";
    let (date1904, listed) = workbook(&part(&mut pkg, book)?)?;
    let names = pkg.part_names();
    let has = |n: &str| names.iter().any(|p| p == n);
    let strings = match has("xl/sharedStrings.bin") {
        true => shared_strings(&part(&mut pkg, "xl/sharedStrings.bin")?, budget)?,
        false => Vec::new(),
    };
    let dates = match has("xl/styles.bin") {
        true => date_formats(&part(&mut pkg, "xl/styles.bin")?)?,
        false => Vec::new(),
    };
    let rels = pkg.relationships(book);
    // Each sheet's part by its relationship id, the first internal one
    // that has it: found in one lookup per sheet listed, where a scan of
    // every relationship for each made the listing's cost its square.
    let mut targets = std::collections::HashMap::new();
    for r in rels.iter().filter(|r| !r.external) {
        targets.entry(r.id.as_str()).or_insert(r.target.as_str());
    }
    let mut sheets = Vec::new();
    let mut total = 0u64;
    // A sheet part is read once, at its first listing: listed again, under
    // any name, it would be inflated and laid out again for each.
    let mut read = std::collections::HashSet::new();
    for (name, rel) in listed {
        let target = *targets
            .get(rel.as_str())
            .ok_or_else(|| malformed(&format!("sheet {name} names no part")))?;
        if !read.insert(target) {
            continue;
        }
        let data = part(&mut pkg, target)?;
        let mut cells = Vec::new();
        let mut row = 0u32;
        for r in records(&data) {
            let r = r?;
            match r.typ {
                // BrtRowHdr: the row the cells after it are on; past
                // Excel's last row, the sheet ends, as calamine reads it.
                0x0000 => {
                    row =
                        u32_at(r.data, 0).ok_or_else(|| malformed("a row record is cut short"))?;
                    if row > 0x0010_0000 {
                        break;
                    }
                    continue;
                }
                // BrtEndSheetData.
                0x0092 => break,
                0x0002..=0x000B => {}
                _ => continue,
            }
            // A cell: its column, its style in 24 bits, then its value.
            let (Some(col), Some(style)) = (u32_at(r.data, 0), u32_at(r.data, 4)) else {
                return Err(malformed("a cell record is cut short"));
            };
            // A column past Excel's grid, which a cell names in 32 bits, is
            // no cell: the last a u32 holds was column 0's neighbour.
            if col >= MAX_COLUMNS || row >= MAX_ROWS {
                continue;
            }
            let date = dates
                .get((style & 0x00FF_FFFF) as usize)
                .copied()
                .unwrap_or(false);
            let Some(v) = cell_value(r.typ, &r.data[8..], date, date1904, &strings, budget)? else {
                continue;
            };
            total += 1;
            if total > MAX_CELLS {
                return Err(XlsError::TooLarge(format!(
                    "its sheets hold more than {MAX_CELLS} cells"
                )));
            }
            let Some((text, datum)) = value(&v, date1904) else {
                continue;
            };
            budget.spend_cells(1).map_err(too_large)?;
            cells.push(GridCell {
                row,
                col,
                text,
                datum,
                bold: false,
            });
        }
        sheets.push(GridSheet {
            name,
            cells,
            ..GridSheet::default()
        });
    }
    Ok(sheets)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// One BIFF12 record: its type and size as variable-length integers,
    /// then its data.
    fn rec(typ: u16, data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        if typ >= 0x80 {
            v.push((typ & 0x7F) as u8 | 0x80);
            v.push((typ >> 7) as u8);
        } else {
            v.push(typ as u8);
        }
        let mut n = data.len();
        loop {
            let b = (n & 0x7F) as u8;
            n >>= 7;
            if n == 0 {
                v.push(b);
                break;
            }
            v.push(b | 0x80);
        }
        v.extend(data);
        v
    }

    fn wide(s: &str) -> Vec<u8> {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut v = (units.len() as u32).to_le_bytes().to_vec();
        for u in units {
            v.extend(u.to_le_bytes());
        }
        v
    }

    /// A cell record's head: its column and style.
    fn head(col: u32, style: u32) -> Vec<u8> {
        [col.to_le_bytes(), style.to_le_bytes()].concat()
    }

    /// A one-sheet `.xlsb`: a header row of shared strings over rows of a
    /// name, an amount and a date, and an inline string below.
    pub(crate) fn book(extra: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let strings = [
            "Customer",
            "Amount",
            "Shipped",
            "Alex Sample",
            "Sam Example",
        ];
        let mut sst = rec(
            0x9F,
            &[
                (strings.len() as u32).to_le_bytes(),
                (strings.len() as u32).to_le_bytes(),
            ]
            .concat(),
        );
        for s in strings {
            sst.extend(rec(0x13, &[vec![0u8], wide(s)].concat()));
        }
        let xf = |fmt: u16| {
            let mut d = vec![0u8; 16];
            d[2..4].copy_from_slice(&fmt.to_le_bytes());
            rec(0x2F, &d)
        };
        let styles = [
            rec(0x267, &0u32.to_le_bytes()),
            rec(0x269, &2u32.to_le_bytes()),
            xf(0),
            xf(14),
        ]
        .concat();
        let mut bundle = 0u32.to_le_bytes().to_vec();
        bundle.extend(1u32.to_le_bytes());
        bundle.extend(wide("rId1"));
        bundle.extend(wide("Orders"));
        let workbook = [rec(0x9C, &bundle), rec(0x90, &[]), rec(0x9D, &[0; 4])].concat();
        let isst = |row_col: u32, i: u32| {
            rec(0x07, &[head(row_col, 0), i.to_le_bytes().to_vec()].concat())
        };
        let real = |col: u32, style: u32, v: f64| {
            rec(0x05, &[head(col, style), v.to_le_bytes().to_vec()].concat())
        };
        let row = |r: u32| rec(0x00, &[r.to_le_bytes().to_vec(), vec![0; 13]].concat());
        let sheet = [
            rec(0x81, &[]),
            rec(
                0x94,
                &[0u32, 3, 0, 2]
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<u8>>(),
            ),
            rec(0x91, &[]),
            row(0),
            isst(0, 0),
            isst(1, 1),
            isst(2, 2),
            row(1),
            isst(0, 3),
            real(1, 0, 12.5),
            real(2, 1, 46095.0),
            row(2),
            isst(0, 4),
            real(1, 0, 7.0),
            real(2, 1, 46096.0),
            row(4),
            rec(
                0x06,
                &[head(0, 0), wide("Prepared by Alex Sample")].concat(),
            ),
            rec(0x92, &[]),
        ]
        .concat();
        let rels = "<Relationships><Relationship Id=\"rId1\" Target=\"worksheets/sheet1.bin\"/></Relationships>";
        let mut buf = Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        let parts: Vec<(&str, Vec<u8>)> = vec![
            ("xl/workbook.bin", workbook),
            ("xl/_rels/workbook.bin.rels", rels.as_bytes().to_vec()),
            ("xl/sharedStrings.bin", sst),
            ("xl/styles.bin", styles),
            ("xl/worksheets/sheet1.bin", sheet),
        ];
        for (name, data) in parts.iter().chain(extra) {
            z.start_file(*name, opts).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
        buf.into_inner()
    }

    /// A one-sheet `.xlsb` of shared `strings` and the sheet `cells`
    /// (records between its row header and its end).
    fn sheet_book(strings: &[String], cells: &[Vec<u8>]) -> Vec<u8> {
        listed_book(strings, cells, 1)
    }

    /// [`sheet_book`], its one sheet listed `times` times.
    fn listed_book(strings: &[String], cells: &[Vec<u8>], times: usize) -> Vec<u8> {
        related_book(strings, cells, times, 0)
    }

    /// [`listed_book`], its workbook relating `others` ids no sheet names
    /// before the sheet's.
    fn related_book(strings: &[String], cells: &[Vec<u8>], times: usize, others: usize) -> Vec<u8> {
        let mut sst = rec(0x9F, &[0u8; 8]);
        for s in strings {
            sst.extend(rec(0x13, &[vec![0u8], wide(s)].concat()));
        }
        let mut bundle = [0u32.to_le_bytes(), 1u32.to_le_bytes()].concat();
        bundle.extend(wide("rId1"));
        bundle.extend(wide("S"));
        let workbook = [rec(0x9C, &bundle).repeat(times), rec(0x90, &[])].concat();
        let sheet = [
            rec(0x91, &[]),
            rec(0x00, &[0u8; 17]),
            cells.concat(),
            rec(0x92, &[]),
        ]
        .concat();
        let rels = format!(
            "<Relationships>{}<Relationship Id=\"rId1\" Target=\"worksheets/sheet1.bin\"/></Relationships>",
            "<Relationship Id=\"p\" Target=\"a\"/>".repeat(others)
        );
        let mut buf = Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        for (name, data) in [
            ("xl/workbook.bin", workbook),
            ("xl/_rels/workbook.bin.rels", rels.as_bytes().to_vec()),
            ("xl/sharedStrings.bin", sst),
            ("xl/worksheets/sheet1.bin", sheet),
        ] {
            z.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(&data).unwrap();
        }
        z.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn a_shared_string_named_by_many_cells_counts_for_each() {
        // 5,000 cells naming one string of 65,536 characters: 320 MiB of
        // copies from a package of a few kilobytes.
        let big = "z".repeat(1 << 16);
        let cells: Vec<Vec<u8>> = (0..5_000u32)
            .map(|c| rec(0x07, &[head(c, 0), 0u32.to_le_bytes().to_vec()].concat()))
            .collect();
        let bytes = sheet_book(std::slice::from_ref(&big), &cells);
        assert!(
            matches!(parse_xlsb(&bytes), Err(XlsError::TooLarge(ref m)) if m.contains("text")),
            "{:?}",
            parse_xlsb(&bytes).map(|(e, _)| e.len())
        );
        let bytes = sheet_book(&[big], &cells[..3]);
        assert!(parse_xlsb(&bytes).is_ok());
    }

    /// An inline string cell at `col`.
    fn inline(col: u32, text: &str) -> Vec<u8> {
        rec(0x06, &[head(col, 0), wide(text)].concat())
    }

    #[test]
    fn a_cell_past_excels_last_column_is_no_cell() {
        // Columns 0 and 4,294,967,295 on a row, then a block under them:
        // the last column a u32 holds was column 0's neighbour, an island
        // across every column, walked a row at a time.
        let row = |r: u32| rec(0x00, &[r.to_le_bytes().to_vec(), vec![0; 13]].concat());
        let cells = [
            inline(0, "a"),
            inline(u32::MAX, "far"),
            inline(16_384, "past"),
            row(1),
            inline(0, "b"),
            inline(1, "c"),
        ];
        let bytes = sheet_book(&[], &cells);
        // Not read at all, before the layout would drop it.
        let sheets = read(&bytes, &mut Budget::default()).unwrap();
        assert!(sheets[0].cells.iter().all(|c| c.col < MAX_COLUMNS));
        let (els, _) = parse_xlsb(&bytes).unwrap();
        let all: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();
        assert!(all.iter().any(|t| t.contains('c')), "{all:?}");
        assert!(
            all.iter()
                .all(|t| !t.contains("far") && !t.contains("past")),
            "{all:?}"
        );
    }

    #[test]
    fn each_cell_read_is_spent_before_it_is_kept() {
        use fluree_doc_model::budget::{CELL_BYTES, TEXT_BYTES};
        let cells: Vec<Vec<u8>> = (0..1_000u32).map(|c| inline(2 * c, "x")).collect();
        let bytes = sheet_book(&[], &cells);
        let budget = |n: u64| Budget::new(n * CELL_BYTES + 1_000 * TEXT_BYTES, 0);
        assert!(read(&bytes, &mut budget(1_000)).is_ok());
        assert!(matches!(
            read(&bytes, &mut budget(999)),
            Err(XlsError::TooLarge(ref m)) if m.contains("cells")
        ));
    }

    #[test]
    fn a_sheet_listed_again_is_read_once() {
        let cells = [inline(0, "a")];
        let (els, _) = parse_xlsb(&listed_book(&[], &cells, 1_000)).unwrap();
        assert_eq!(els.iter().filter(|e| e.text == "S").count(), 1, "{els:?}");
        assert!(matches!(
            parse_xlsb(&listed_book(&[], &cells, MAX_ENTRIES + 1)),
            Err(XlsError::TooLarge(ref m)) if m.contains("sheets")
        ));
    }

    #[test]
    fn each_shared_string_is_spent_before_it_is_kept() {
        // An empty one is a record of seven bytes and a string in the
        // table: nine million of them in 100 KB zipped.
        use fluree_doc_model::budget::CELL_BYTES;
        let part: Vec<u8> = (0..1_000)
            .flat_map(|_| rec(0x13, &[vec![0u8], wide("")].concat()))
            .collect();
        let slots = |n: u64| Budget::new(n * CELL_BYTES, 0);
        assert_eq!(
            shared_strings(&part, &mut slots(1_000)).unwrap().len(),
            1_000
        );
        assert!(matches!(
            shared_strings(&part, &mut slots(999)),
            Err(XlsError::TooLarge(ref m)) if m.contains("cells")
        ));
    }

    #[test]
    fn a_sheet_is_found_among_the_relationships_in_one_lookup() {
        // 10,000 listings and 500,000 relationships: each listing scanning
        // them all, 1.9 million of them took 28 s in a release build.
        let bytes = related_book(&[], &[inline(0, "a")], MAX_ENTRIES, 500_000);
        assert!(parse_xlsb(&bytes).is_ok());
        fluree_doc_model::scaling::assert_linear(
            "sheets listed among relationships",
            500,
            |n| related_book(&[], &[inline(0, "a")], n, 50 * n),
            |b| assert!(parse_xlsb(b).is_ok()),
        );
    }

    #[test]
    fn a_shared_strings_copies_count_at_their_worst_output() {
        // 600 cells naming one string of 65,000 characters: 39 MB of
        // copies; of control characters, each six bytes in JSON, 234 MB of
        // output and the copies of it.
        let cells: Vec<Vec<u8>> = (0..600u32)
            .map(|c| rec(0x07, &[head(c, 0), 0u32.to_le_bytes().to_vec()].concat()))
            .collect();
        let book = |c: char| sheet_book(&[c.to_string().repeat(65_000)], &cells);
        assert!(parse_xlsb(&book('y')).is_ok());
        assert!(matches!(
            parse_xlsb(&book('\u{1}')),
            Err(XlsError::TooLarge(ref m)) if m.contains("text")
        ));
    }

    #[test]
    fn records_cut_short_are_malformed_not_a_panic() {
        let strings = ["a".to_string()];
        for cell in [
            // A number with four of its eight bytes.
            rec(0x05, &[head(0, 0), vec![0u8; 4]].concat()),
            // A cell with no style.
            rec(0x07, &[0u8; 4]),
            // A string longer than its record.
            rec(
                0x06,
                &[head(0, 0), 50u32.to_le_bytes().to_vec(), vec![0u8; 4]].concat(),
            ),
            // A boolean with no value.
            rec(0x04, &head(0, 0)),
            // A record whose length runs past the part.
            vec![0x05, 0x7F, 1, 2, 3],
        ] {
            assert!(
                matches!(
                    parse_xlsb(&sheet_book(&strings, std::slice::from_ref(&cell))),
                    Err(XlsError::Biff(_))
                ),
                "{cell:?}"
            );
        }
    }

    #[test]
    fn a_binary_workbook_reads_as_an_xlsx_one() {
        assert!(is_xlsb(&book(&[])));
        let (els, _) = parse_xlsb(&book(&[])).unwrap();
        assert_eq!(els[0].text, "Orders");
        let t = els.iter().find(|e| e.kind == "doco:Table").unwrap();
        let cells = t.cells.as_ref().unwrap();
        assert_eq!(cells[0], ["Customer", "Amount", "Shipped"]);
        assert_eq!(cells[1], ["Alex Sample", "12.5", "2026-03-14"]);
        assert_eq!(t.header_rows, Some(1));
        let d = t.datums.as_ref().unwrap();
        assert_eq!(d[2][2].as_ref().unwrap().datatype, "xsd:date");
        assert!(els.iter().any(|e| e.text == "Prepared by Alex Sample"));
        assert!(els.iter().all(|e| e.provenance == "xlsb"));
    }

    #[test]
    fn a_part_that_inflates_past_its_bound_is_refused_before_calamine() {
        let big = vec![0u8; (MAX_PART_BYTES + 1) as usize];
        let bytes = book(&[("xl/media/huge.bin", big)]);
        assert!(matches!(parse_xlsb(&bytes), Err(XlsError::TooLarge(_))));
        assert!(!is_xlsb(b"PK\x03\x04"));
        // Named `.xlsb`, a file that is no zip says so, not that it is no
        // `.xls`.
        let err = parse_xlsb(b"a\tb\n1\t2\n").unwrap_err();
        assert!(matches!(err, XlsError::Package(_)));
        assert!(err.to_string().starts_with("not a readable .xlsb"), "{err}");
    }

    #[test]
    fn a_broken_reference_is_a_malformed_file_not_a_panic() {
        // The sheet names a shared string that is not there: calamine
        // indexes past its table.
        let mut bytes = book(&[]);
        let _ = &mut bytes;
        let sheet_bad = {
            let mut z = zip::ZipArchive::new(Cursor::new(book(&[]))).unwrap();
            let mut out = Cursor::new(Vec::new());
            let mut w = zip::ZipWriter::new(&mut out);
            for i in 0..z.len() {
                let mut f = z.by_index(i).unwrap();
                let name = f.name().unwrap().into_owned();
                let mut data = Vec::new();
                f.read_to_end(&mut data).unwrap();
                if name == "xl/worksheets/sheet1.bin" {
                    // The first shared-string index, 0, becomes 999.
                    let at = data.windows(2).position(|w| w == [0x07, 0x0C]).unwrap() + 2 + 8;
                    data[at..at + 4].copy_from_slice(&999u32.to_le_bytes());
                }
                w.start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                w.write_all(&data).unwrap();
            }
            w.finish().unwrap();
            out.into_inner()
        };
        assert!(matches!(parse_xlsb(&sheet_bad), Err(XlsError::Biff(_))));
    }
}
