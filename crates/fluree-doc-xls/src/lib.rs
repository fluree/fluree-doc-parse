//! Excel 97-2003 workbooks (`.xls`, BIFF8) to DoCO-typed document elements,
//! and Excel binary workbooks (`.xlsb`, see [`parse_xlsb`]).
//!
//! The cells are read by [calamine](https://crates.io/crates/calamine), and
//! laid out by the XLSX reader ([`fluree_doc_xlsx::grid_elements`]): each
//! sheet a page opened by its name, its islands of cells tables, merges as
//! continuations, rows frozen at the top a table's header. What a BIFF file
//! stores differs from what a package does in one way that shows: calamine
//! types a number as a date by its format, and gives no other format, so a
//! number reads as Excel's General format shows it (`0.125`, not `12.5%`)
//! while a date reads as an ISO date. What a cell stores rides beside it
//! typed, as an XLSX cell's does.
//!
//! **Bounds.** calamine is not given the file: it reads a compound file
//! with a reader of its own that follows a looping sector chain, reserves
//! what a sheet's `DIMENSIONS` record declares, lays each sheet out as a
//! dense grid from its first used cell to its last, copies a shared string
//! into every cell that names it, and indexes record data without checking
//! its length. So the file is read first ([`scan`]): the compound file is
//! opened by the `cfb` crate, which refuses a chain that loops or runs past
//! the file; its workbook stream is read whole (at most
//! [`MAX_STREAM_BYTES`]) and its records walked as calamine would walk them,
//! each record calamine would read checked against what calamine indexes in
//! it. A workbook whose cells, laid-out grids or merged ranges add up to
//! more than [`MAX_CELLS`], or whose cells would copy more than
//! [`MAX_TEXT_CHARS`] characters of text, is refused. calamine is then
//! given a compound file written here, holding a workbook stream of only
//! the records it needs, each sheet once and its formulas emptied to their
//! cached values, and the summary information where it is small. The same
//! walk reads what calamine does not give: which cells are bold, and how
//! many rows each sheet freezes.

mod scan;
mod xlsb;

pub use xlsb::{is_xlsb, parse_xlsb};

use fluree_doc_model::{DocumentInfo, Element};
use fluree_doc_xlsx::{GridCell, GridSheet};

/// The most bytes the workbook stream may hold: 256 MiB. An `.xls` is not
/// compressed, so this is also about the file's size.
pub const MAX_STREAM_BYTES: u64 = 256 << 20;

/// The most cells a workbook's sheets may hold, and the most their grids may
/// span, summed over its sheets, each from its first used cell to its last:
/// about four million. calamine holds a value and a formula for each.
pub const MAX_CELLS: u64 = 1 << 22;

/// The most characters of text calamine is let allocate for a workbook's
/// shared strings and cells, a shared string counted again for each cell
/// that names it: 64 million, at most 192 MiB of UTF-8.
pub const MAX_TEXT_CHARS: u64 = 64 << 20;

/// The most sheet records a workbook's globals may list: each is a sheet
/// calamine reads, and a page laid out, however many name one sheet.
pub const MAX_SHEETS: usize = 1 << 16;

#[derive(Debug)]
pub enum XlsError {
    /// Not a compound file, or one whose structure is broken.
    Container(String),
    /// Not a zip, or one whose structure is broken: an `.xlsb` that is not
    /// one.
    Package(String),
    /// A compound file with no workbook stream: a Word or PowerPoint file.
    NoWorkbook,
    /// Past one of the bounds above.
    TooLarge(String),
    /// The workbook stream could not be read.
    Biff(String),
}

impl std::fmt::Display for XlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Container(e) => write!(f, "not a readable .xls: {e}"),
            Self::Package(e) => write!(f, "not a readable .xlsb: {e}"),
            Self::NoWorkbook => write!(
                f,
                "not a workbook: the compound file has no Workbook stream"
            ),
            Self::TooLarge(e) => write!(f, "workbook too large: {e}"),
            Self::Biff(e) => write!(f, "malformed workbook: {e}"),
        }
    }
}

impl std::error::Error for XlsError {}

/// Whether `bytes` is an Excel 97-2003 workbook: a compound file with a
/// `Workbook` stream (or `Book`, BIFF5's name for it) at its root.
pub fn is_xls(bytes: &[u8]) -> bool {
    scan::workbook_stream_name(bytes).is_some()
}

/// Parse an `.xls` file's bytes into elements, in sheet order.
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, XlsError> {
    parse_with_info(bytes).map(|(elements, _)| elements)
}

/// [`parse`], with what the file declares about itself in its summary
/// information: title, author, dates.
pub fn parse_with_info(bytes: &[u8]) -> Result<(Vec<Element>, DocumentInfo), XlsError> {
    let (scanned, summary) = scan::scan(bytes)?;
    let book = scan::compound(&scanned.stream, summary.as_deref())?;
    drop(summary);
    read(&book, scanned.sheets)
}

fn read(book: &[u8], scanned: Vec<scan::Sheet>) -> Result<(Vec<Element>, DocumentInfo), XlsError> {
    use calamine::{Reader, Xls};
    let too_large = |e: fluree_doc_model::budget::TooLarge| XlsError::TooLarge(e.to_string());
    // What the workbook makes once calamine has read it: each cell, its
    // text, and what the layout makes of them.
    let mut budget = fluree_doc_model::budget::Budget::default();
    let mut wb: Xls<_> = calamine::open_workbook_from_rs(std::io::Cursor::new(book))
        .map_err(|e: calamine::XlsError| XlsError::Biff(e.to_string()))?;
    let date1904 = wb.has_1904_epoch();
    let names = wb.sheet_names();
    let mut sheets = Vec::with_capacity(names.len());
    for (i, name) in names.iter().enumerate() {
        let range = wb
            .worksheet_range(name)
            .map_err(|e| XlsError::Biff(e.to_string()))?;
        let found = scanned.get(i);
        let mut cells = Vec::new();
        // `used_cells` counts from the range's first cell.
        let (r0, c0) = range.start().unwrap_or((0, 0));
        for (row, col, v) in range.used_cells() {
            let (row, col) = (r0 + row as u32, c0 + col as u32);
            let Some((text, datum)) = value(v, date1904) else {
                continue;
            };
            budget.spend_cells(1).map_err(too_large)?;
            budget.spend_text(&text).map_err(too_large)?;
            cells.push(GridCell {
                row,
                col,
                text,
                datum,
                bold: found.is_some_and(|s| s.bold.contains(&(row, col))),
            });
        }
        let merges = wb
            .merge_cells_by_sheet_name(name)
            .unwrap_or_default()
            .into_iter()
            .map(|d| (d.start.0, d.start.1, d.end.0, d.end.1))
            .collect();
        sheets.push(GridSheet {
            name: name.clone(),
            cells,
            merges,
            frozen_rows: found.map_or(0, |s| s.frozen_rows),
        });
    }
    let info = wb
        .workbook_properties()
        .map(|p| DocumentInfo {
            title: p.title.filter(|t| !t.trim().is_empty()),
            creators: p
                .creator
                .into_iter()
                .filter(|c| !c.trim().is_empty())
                .collect(),
            created: p
                .created
                .as_deref()
                .and_then(fluree_doc_model::xsd_date_time),
            modified: p
                .modified
                .as_deref()
                .and_then(fluree_doc_model::xsd_date_time),
        })
        .unwrap_or_default();
    let elements = fluree_doc_xlsx::grid_elements(sheets, "xls", &mut budget).map_err(too_large)?;
    Ok((elements, info))
}

/// A cell's text as Excel shows it under the General format, a date as an
/// ISO date, and what it stores, typed. `None` for an empty cell.
fn value(v: &calamine::Data, date1904: bool) -> Option<(String, Option<fluree_doc_model::Datum>)> {
    use calamine::Data;
    use fluree_doc_xlsx::{cell_datum, format_general, format_serial, CellStyle, NumFmt};
    let number = |n: f64, fmt: NumFmt| {
        let style = CellStyle {
            format: fmt.clone(),
            ..CellStyle::default()
        };
        let text = match fmt {
            NumFmt::General => format_general(n),
            fmt => format_serial(n, fmt, date1904),
        };
        (text, cell_datum("n", &n.to_string(), &style, date1904))
    };
    Some(match v {
        Data::Empty => return None,
        Data::String(s) => (s.clone(), None),
        Data::Int(i) => number(*i as f64, NumFmt::General),
        Data::Float(f) => number(*f, NumFmt::General),
        Data::Bool(b) => (
            if *b { "TRUE" } else { "FALSE" }.into(),
            cell_datum(
                "b",
                if *b { "1" } else { "0" },
                &CellStyle::default(),
                date1904,
            ),
        ),
        Data::DateTime(dt) if dt.is_duration() => number(dt.as_f64(), NumFmt::General),
        Data::DateTime(dt) => {
            let n = dt.as_f64();
            let fmt = if n.fract() == 0.0 {
                NumFmt::Date
            } else if n < 1.0 {
                NumFmt::Time
            } else {
                NumFmt::DateTime
            };
            number(n, fmt)
        }
        Data::DateTimeIso(s) => (
            s.clone(),
            fluree_doc_model::xsd_temporal(s).map(|(t, v)| fluree_doc_model::Datum::new(t, v)),
        ),
        Data::DurationIso(s) => (s.clone(), None),
        Data::Error(e) => (e.to_string(), None),
    })
}

#[cfg(test)]
mod tests;
