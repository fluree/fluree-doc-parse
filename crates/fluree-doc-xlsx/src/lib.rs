//! XLSX (OOXML) to DoCO-typed document elements.
//!
//! A workbook declares its geometry and nothing else. Every cell has an exact
//! row and column — the one thing the PDF engine has to measure — and no
//! cell says what it is. A sheet is a grid on which someone laid out a
//! title, a few labelled fields, a table and a note, and reading it as one
//! table per sheet returns a mostly-empty rectangle with the structure
//! flattened out of it. So this reader measures after all, but only shape:
//! which cells sit together, and what a row of one cell above a block of
//! many is.
//!
//! * **A sheet is a page.** `page` carries the sheet's index in the
//!   workbook's own order, and the sheet's name opens it as a level-1
//!   heading.
//! * **Islands are elements.** Occupied cells that touch along an edge form
//!   an island; an empty row or column between two blocks separates them,
//!   which is how a spreadsheet's author separates them. Each island is a
//!   table, except that a single cell alone on its row at the top is the
//!   island's title and one at the bottom is its note.
//! * **Merges are read as Excel shows them.** A merged range displays its
//!   top-left cell and hides the rest, stale values included; the hidden
//!   cells become `merged_left` / `merged_down` continuations exactly as the
//!   docx reader and the PDF engine express them.
//! * **Values are the cached ones.** A formula's `<v>` is what the file
//!   last computed and what Excel shows; nothing is recalculated. Numbers
//!   render as the shortest decimal that round-trips, dates and times as
//!   ISO where the cell's number format says the number is one, and
//!   percentages as percentages.
//!
//! No `bbox`: a cell address is not a position on a page, and a consumer
//! wanting the address has the table's row and column.

use fluree_doc_model::Element;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::Read;

#[derive(Debug)]
pub enum XlsxError {
    Zip(String),
    Xml(String),
    NoWorkbook,
}

impl std::fmt::Display for XlsxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zip(e) => write!(f, "not a readable .xlsx: {e}"),
            Self::Xml(e) => write!(f, "malformed workbook XML: {e}"),
            Self::NoWorkbook => write!(f, "archive has no xl/workbook.xml"),
        }
    }
}

impl std::error::Error for XlsxError {}

/// Parse a `.xlsx` file's bytes into elements, in sheet order.
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, XlsxError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| XlsxError::Zip(e.to_string()))?;
    let read = |zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str| -> Option<String> {
        let mut s = String::new();
        zip.by_name(name).ok()?.read_to_string(&mut s).ok()?;
        Some(s)
    };
    let workbook = read(&mut zip, "xl/workbook.xml").ok_or(XlsxError::NoWorkbook)?;
    let rels = read(&mut zip, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let shared = read(&mut zip, "xl/sharedStrings.xml")
        .map(|x| parse_shared_strings(&x))
        .transpose()?
        .unwrap_or_default();
    let styles = read(&mut zip, "xl/styles.xml")
        .map(|x| parse_styles(&x))
        .transpose()?
        .unwrap_or_default();
    let (sheets, date1904) = parse_workbook_xml(&workbook)?;
    let targets = parse_rels(&rels)?;

    let mut out = Vec::new();
    for (page, sheet) in sheets.iter().enumerate() {
        let Some(target) = targets.get(&sheet.rid) else {
            continue;
        };
        let path = if let Some(p) = target.strip_prefix('/') {
            p.to_string()
        } else {
            format!("xl/{target}")
        };
        let Some(xml) = read(&mut zip, &path) else {
            continue;
        };
        let ctx = Context {
            shared: &shared,
            styles: &styles,
            date1904,
        };
        out.extend(parse_sheet_xml(&sheet.name, &xml, page, &ctx)?);
    }
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    Ok(out)
}

/// What a sheet needs from the rest of the package.
pub struct Context<'a> {
    pub shared: &'a [String],
    pub styles: &'a Styles,
    /// The workbook counts days from 1904-01-01 rather than 1900.
    pub date1904: bool,
}

struct SheetRef {
    name: String,
    rid: String,
}

fn parse_workbook_xml(xml: &str) -> Result<(Vec<SheetRef>, bool), XlsxError> {
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut sheets = Vec::new();
    let mut date1904 = false;
    loop {
        match r.read_event_into(&mut buf) {
            Err(e) => return Err(XlsxError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                "sheet" => {
                    let name = attr(&e, "name").unwrap_or_default();
                    let rid = attr(&e, "id").unwrap_or_default();
                    sheets.push(SheetRef { name, rid });
                }
                "workbookPr" => {
                    date1904 =
                        attr(&e, "date1904").is_some_and(|v| matches!(v.as_str(), "1" | "true"));
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    Ok((sheets, date1904))
}

/// Relationship id → target path, relative to `xl/`.
fn parse_rels(xml: &str) -> Result<HashMap<String, String>, XlsxError> {
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut out = HashMap::new();
    loop {
        match r.read_event_into(&mut buf) {
            Err(e) => return Err(XlsxError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if local(e.name().as_ref()) == "Relationship" =>
            {
                if let (Some(id), Some(target)) = (attr(&e, "Id"), attr(&e, "Target")) {
                    out.insert(id, target);
                }
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// The shared string table, one entry per `<si>`, rich-text runs joined and
/// phonetic guides (`<rPh>`) left out.
pub fn parse_shared_strings(xml: &str) -> Result<Vec<String>, XlsxError> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    let mut in_t = false;
    let mut phonetic = 0usize;
    loop {
        match r.read_event_into(&mut buf) {
            Err(e) => return Err(XlsxError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => match local(e.name().as_ref()) {
                "si" => cur = Some(String::new()),
                "rPh" => phonetic += 1,
                "t" if phonetic == 0 => in_t = true,
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                if local(e.name().as_ref()) == "si" {
                    out.push(String::new());
                }
            }
            Ok(Event::Text(t)) => {
                if in_t {
                    if let Some(s) = cur.as_mut() {
                        s.push_str(&t.unescape().unwrap_or_default());
                    }
                }
            }
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                "si" => out.push(cur.take().unwrap_or_default()),
                "rPh" => phonetic = phonetic.saturating_sub(1),
                "t" => in_t = false,
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// What a cell's style index resolves to: how its number displays, and
/// whether its font is bold and how large.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellStyle {
    pub format: NumFmt,
    pub bold: bool,
    pub size: f32,
}

impl Default for CellStyle {
    fn default() -> Self {
        Self {
            format: NumFmt::General,
            bold: false,
            size: 11.0,
        }
    }
}

/// The cell styles a workbook declares, by `s` index.
#[derive(Debug, Default, Clone)]
pub struct Styles {
    pub xfs: Vec<CellStyle>,
}

impl Styles {
    fn get(&self, s: Option<usize>) -> CellStyle {
        s.and_then(|i| self.xfs.get(i).copied()).unwrap_or_default()
    }
}

/// How a numeric cell displays. Excel's format language is large; this
/// reads the part that changes what a number *means* — a date is not a
/// count of days, a percentage is not a fraction — and leaves the rest
/// (thousands separators, currency signs, colours) to the shortest decimal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumFmt {
    General,
    Date,
    Time,
    DateTime,
    /// Decimal places shown.
    Percent(usize),
}

/// Built-in number formats, by id. Ids 14–22 are dates and times; 27–36
/// and 50–58 are locale dates; 45–47 are elapsed times; 9 and 10 are
/// percentages.
fn builtin_fmt(id: u32) -> NumFmt {
    match id {
        9 => NumFmt::Percent(0),
        10 => NumFmt::Percent(2),
        14..=17 | 27..=36 | 50..=58 => NumFmt::Date,
        18..=21 | 45..=47 => NumFmt::Time,
        22 => NumFmt::DateTime,
        _ => NumFmt::General,
    }
}

/// Classify a custom format code. Quoted literals, bracketed conditions and
/// locale tags, and escaped characters say nothing about the value, so they
/// are dropped before the date letters are looked for.
pub fn custom_fmt(code: &str) -> NumFmt {
    let mut bare = String::new();
    let mut chars = code.chars();
    let mut quoted = false;
    let mut bracket = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            _ if quoted => {}
            '[' => bracket = true,
            ']' => bracket = false,
            _ if bracket => {}
            '\\' | '_' | '*' => {
                chars.next();
            }
            ';' => break, // the positive section decides
            c => bare.push(c.to_ascii_lowercase()),
        }
    }
    if bare.contains('%') {
        let decimals = bare
            .split_once('.')
            .map(|(_, tail)| tail.chars().take_while(|c| *c == '0').count())
            .unwrap_or(0);
        return NumFmt::Percent(decimals);
    }
    let date = bare.contains('y') || bare.contains('d');
    let time = bare.contains('h') || bare.contains('s');
    let month = bare.contains('m');
    match (date, time, month) {
        (true, true, _) => NumFmt::DateTime,
        (true, false, _) => NumFmt::Date,
        (false, true, _) => NumFmt::Time,
        (false, false, true) => NumFmt::Date,
        _ => NumFmt::General,
    }
}

/// `xl/styles.xml`: custom number formats, fonts, and the cell style table
/// that indexes both.
pub fn parse_styles(xml: &str) -> Result<Styles, XlsxError> {
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut custom: HashMap<u32, NumFmt> = HashMap::new();
    let mut fonts: Vec<(bool, f32)> = Vec::new();
    let mut xfs: Vec<CellStyle> = Vec::new();
    let mut in_fonts = false;
    let mut in_cell_xfs = false;
    let mut font: Option<(bool, f32)> = None;
    loop {
        let event = r.read_event_into(&mut buf);
        let (e, empty) = match event {
            Err(e) => return Err(XlsxError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => (e, false),
            Ok(Event::Empty(e)) => (e, true),
            Ok(Event::End(e)) => {
                match local(e.name().as_ref()) {
                    "fonts" => in_fonts = false,
                    "font" if in_fonts => fonts.push(font.take().unwrap_or((false, 11.0))),
                    "cellXfs" => in_cell_xfs = false,
                    _ => {}
                }
                buf.clear();
                continue;
            }
            Ok(_) => {
                buf.clear();
                continue;
            }
        };
        let name = e.name();
        let tag = local(name.as_ref());
        match tag {
            "numFmt" => {
                if let (Some(id), Some(code)) = (attr(&e, "numFmtId"), attr(&e, "formatCode")) {
                    if let Ok(id) = id.parse::<u32>() {
                        custom.insert(id, custom_fmt(&code));
                    }
                }
            }
            "fonts" => in_fonts = true,
            "font" if in_fonts => {
                if empty {
                    fonts.push((false, 11.0));
                } else {
                    font = Some((false, 11.0));
                }
            }
            "b" if in_fonts => {
                if let Some(f) = font.as_mut() {
                    f.0 = !attr(&e, "val").is_some_and(|v| matches!(v.as_str(), "0" | "false"));
                }
            }
            "sz" if in_fonts => {
                if let (Some(f), Some(v)) = (font.as_mut(), attr(&e, "val")) {
                    if let Ok(sz) = v.parse::<f32>() {
                        f.1 = sz;
                    }
                }
            }
            "cellXfs" => in_cell_xfs = true,
            "xf" if in_cell_xfs => {
                let num = attr(&e, "numFmtId")
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(0);
                let format = custom
                    .get(&num)
                    .copied()
                    .unwrap_or_else(|| builtin_fmt(num));
                let (bold, size) = attr(&e, "fontId")
                    .and_then(|v| v.parse::<usize>().ok())
                    .and_then(|i| fonts.get(i).copied())
                    .unwrap_or((false, 11.0));
                xfs.push(CellStyle { format, bold, size });
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(Styles { xfs })
}

/// One occupied cell of a sheet, in grid coordinates.
#[derive(Debug, Clone, Default, PartialEq)]
struct Cell {
    text: String,
    bold: bool,
    size: f32,
}

/// A rectangle of cells, inclusive, 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Range {
    r0: u32,
    c0: u32,
    r1: u32,
    c1: u32,
}

impl Range {
    fn width(&self) -> u32 {
        self.c1 - self.c0 + 1
    }
    fn height(&self) -> u32 {
        self.r1 - self.r0 + 1
    }
}

/// Column letters to a 0-based index: `A` → 0, `Z` → 25, `AA` → 26.
pub fn column_index(letters: &str) -> Option<u32> {
    let mut n: u32 = 0;
    let mut any = false;
    for c in letters.chars() {
        if !c.is_ascii_alphabetic() {
            return None;
        }
        n = n
            .checked_mul(26)?
            .checked_add(c.to_ascii_uppercase() as u32 - 'A' as u32 + 1)?;
        any = true;
    }
    any.then(|| n - 1)
}

/// `B7` → (row 6, col 1).
fn cell_ref(r: &str) -> Option<(u32, u32)> {
    let split = r.find(|c: char| c.is_ascii_digit())?;
    let col = column_index(&r[..split])?;
    let row = r[split..].parse::<u32>().ok()?.checked_sub(1)?;
    Some((row, col))
}

/// `A1:C4` → the rectangle; a bare `A1` is a one-cell range.
fn parse_range(s: &str) -> Option<Range> {
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    let (r0, c0) = cell_ref(a)?;
    let (r1, c1) = cell_ref(b)?;
    Some(Range {
        r0: r0.min(r1),
        c0: c0.min(c1),
        r1: r0.max(r1),
        c1: c0.max(c1),
    })
}

/// A number as Excel's General format shows it: an integer when it is one,
/// otherwise the shortest decimal that reads back to the same value — which
/// is how `6.7400000000000002` in the file is `6.74` on screen.
pub fn format_general(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Civil date from days since 1970-01-01 (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A serial date as ISO text. Excel counts days from 1899-12-30 in the 1900
/// system — the day before 1900-01-01, so that serial 1 is New Year's Day —
/// and keeps a day that never happened, 1900-02-29, at serial 60; a serial
/// below 61 is one day off from that. The 1904 system counts from 1904-01-01.
pub fn format_serial(v: f64, fmt: NumFmt, date1904: bool) -> String {
    if v < 0.0 || !v.is_finite() {
        return format_general(v);
    }
    let days = v.floor() as i64;
    let secs = ((v - v.floor()) * 86_400.0).round() as i64;
    let (days, secs) = if secs >= 86_400 {
        (days + 1, 0)
    } else {
        (days, secs)
    };
    // Days between 1970-01-01 and each epoch.
    let epoch = if date1904 {
        -24_107
    } else if days >= 61 {
        -25_569
    } else {
        -25_568
    };
    let (y, m, d) = civil_from_days(days + epoch);
    let date = format!("{y:04}-{m:02}-{d:02}");
    let time = format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    );
    match fmt {
        NumFmt::Date => date,
        NumFmt::Time => time,
        _ => format!("{date} {time}"),
    }
}

/// Render a numeric cell's cached value through its style.
pub fn format_number(raw: &str, style: CellStyle, date1904: bool) -> String {
    let Ok(v) = raw.trim().parse::<f64>() else {
        return raw.trim().to_string();
    };
    match style.format {
        NumFmt::General => format_general(v),
        NumFmt::Percent(decimals) => format!("{:.*}%", decimals, v * 100.0),
        fmt => format_serial(v, fmt, date1904),
    }
}

/// Everything the sheet XML says that the layout needs.
#[derive(Debug, Default)]
struct Sheet {
    cells: BTreeMap<(u32, u32), Cell>,
    merges: Vec<Range>,
    /// Rows frozen at the top by a `pane`, which is how a sheet declares
    /// its header rows.
    frozen_rows: u32,
    /// `autoFilter` and table-part ranges: the header row of each is its
    /// first row.
    filtered: Vec<Range>,
}

/// Parse one sheet's XML into elements tagged with `page`.
pub fn parse_sheet_xml(
    name: &str,
    xml: &str,
    page: usize,
    ctx: &Context<'_>,
) -> Result<Vec<Element>, XlsxError> {
    let sheet = read_sheet(xml, ctx)?;
    let mut out = vec![element(
        "doco:SectionTitle",
        name.to_string(),
        Some(1),
        page,
    )];
    out.extend(layout(&sheet, page));
    Ok(out)
}

fn read_sheet(xml: &str, ctx: &Context<'_>) -> Result<Sheet, XlsxError> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut sheet = Sheet::default();

    let mut row: u32 = 0;
    let mut next_row: u32 = 0;
    let mut col: u32 = 0;
    // The cell being read: position, type, style, and the text of its
    // value or inline string as it arrives.
    let mut cur: Option<((u32, u32), String, Option<usize>)> = None;
    let mut value = String::new();
    let mut in_v = false;
    let mut in_is_t = false;
    let mut in_is = false;

    loop {
        let event = r.read_event_into(&mut buf);
        let (e, empty) = match event {
            Err(e) => return Err(XlsxError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => (e, false),
            Ok(Event::Empty(e)) => (e, true),
            Ok(Event::Text(t)) => {
                if in_v || in_is_t {
                    value.push_str(&t.unescape().unwrap_or_default());
                }
                buf.clear();
                continue;
            }
            Ok(Event::End(e)) => {
                match local(e.name().as_ref()) {
                    "v" => in_v = false,
                    "is" => in_is = false,
                    "t" => in_is_t = false,
                    "c" => {
                        if let Some((pos, kind, style)) = cur.take() {
                            let st = ctx.styles.get(style);
                            let text = cell_text(&kind, &value, st, ctx);
                            if !text.is_empty() {
                                sheet.cells.insert(
                                    pos,
                                    Cell {
                                        text,
                                        bold: st.bold,
                                        size: st.size,
                                    },
                                );
                            }
                        }
                        value.clear();
                    }
                    _ => {}
                }
                buf.clear();
                continue;
            }
            Ok(_) => {
                buf.clear();
                continue;
            }
        };
        let tag = local(e.name().as_ref()).to_string();
        match tag.as_str() {
            "row" => {
                row = attr(&e, "r")
                    .and_then(|v| v.parse::<u32>().ok())
                    .and_then(|v| v.checked_sub(1))
                    .unwrap_or(next_row);
                next_row = row + 1;
                col = 0;
            }
            "c" => {
                let pos = attr(&e, "r")
                    .and_then(|v| cell_ref(&v))
                    .unwrap_or((row, col));
                col = pos.1 + 1;
                // `<c r="B1" s="19"/>` is a styled empty cell: no
                // End event will follow, and it holds nothing.
                if !empty {
                    let kind = attr(&e, "t").unwrap_or_default();
                    let style = attr(&e, "s").and_then(|v| v.parse::<usize>().ok());
                    cur = Some((pos, kind, style));
                    value.clear();
                }
            }
            "v" => in_v = true,
            "is" => in_is = true,
            "t" if in_is => in_is_t = true,
            "mergeCell" => {
                if let Some(range) = attr(&e, "ref").and_then(|v| parse_range(&v)) {
                    sheet.merges.push(range);
                }
            }
            "pane" => {
                let frozen = attr(&e, "state").is_some_and(|s| s == "frozen" || s == "frozenSplit");
                if frozen {
                    sheet.frozen_rows = attr(&e, "ySplit")
                        .and_then(|v| v.parse::<f64>().ok())
                        .map(|v| v as u32)
                        .unwrap_or(0);
                }
            }
            "autoFilter" => {
                if let Some(range) = attr(&e, "ref").and_then(|v| parse_range(&v)) {
                    sheet.filtered.push(range);
                }
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(sheet)
}

/// A cell's display text from its type, cached value and style.
fn cell_text(kind: &str, value: &str, st: CellStyle, ctx: &Context<'_>) -> String {
    let text = match kind {
        "s" => value
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|i| ctx.shared.get(i).cloned())
            .unwrap_or_default(),
        "inlineStr" | "str" | "d" => value.to_string(),
        "b" => match value.trim() {
            "1" | "true" => "TRUE".into(),
            "0" | "false" => "FALSE".into(),
            other => other.to_string(),
        },
        "e" => value.trim().to_string(),
        _ => format_number(value, st, ctx.date1904),
    };
    // A cell's hard line breaks become spaces, as a Word cell's do: the
    // text projection joins a row's cells with tabs and its rows with
    // newlines, and a newline inside a cell would break that row.
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lay a sheet's cells out as elements: islands of touching cells, each a
/// table with its title peeled off the top and its note off the bottom.
fn layout(sheet: &Sheet, page: usize) -> Vec<Element> {
    // Merged ranges show their anchor and hide the rest. The hidden cells
    // still occupy their positions — they are part of what the range
    // covers — but carry no text of their own.
    let mut cells: BTreeMap<(u32, u32), Cell> = sheet.cells.clone();
    let mut merge_of: HashMap<(u32, u32), Range> = HashMap::new();
    for m in &sheet.merges {
        for r in m.r0..=m.r1 {
            for c in m.c0..=m.c1 {
                if (r, c) != (m.r0, m.c0) {
                    cells.remove(&(r, c));
                }
                merge_of.insert((r, c), *m);
            }
        }
    }
    let mut occupied: HashSet<(u32, u32)> = cells.keys().copied().collect();
    for m in &sheet.merges {
        if cells.contains_key(&(m.r0, m.c0)) {
            for r in m.r0..=m.r1 {
                for c in m.c0..=m.c1 {
                    occupied.insert((r, c));
                }
            }
        }
    }

    // The sheet's body size: what most text is set in, so a larger cell
    // reads as display type.
    let modal_size = {
        let mut hist: HashMap<u32, usize> = HashMap::new();
        for c in cells.values() {
            *hist.entry((c.size * 2.0).round() as u32).or_default() += c.text.len();
        }
        hist.into_iter()
            .max_by_key(|(_, n)| *n)
            .map(|(k, _)| k as f32 / 2.0)
            .unwrap_or(11.0)
    };

    let mut islands = islands(&occupied);
    islands.sort_by_key(|isl| (isl.range.r0, isl.range.c0));

    let mut placed: Vec<((u32, u32), Element)> = Vec::new();
    for isl in islands {
        placed.extend(island_elements(
            &isl, &cells, &merge_of, sheet, modal_size, page,
        ));
    }
    placed.sort_by_key(|(at, _)| *at);
    placed.into_iter().map(|(_, e)| e).collect()
}

struct Island {
    members: HashSet<(u32, u32)>,
    range: Range,
}

/// Connected components of occupied cells, four-connected: an empty row or
/// column between two blocks separates them.
fn islands(occupied: &HashSet<(u32, u32)>) -> Vec<Island> {
    let mut seen: HashSet<(u32, u32)> = HashSet::new();
    let mut out = Vec::new();
    let mut seeds: Vec<(u32, u32)> = occupied.iter().copied().collect();
    seeds.sort_unstable();
    for seed in seeds {
        if seen.contains(&seed) {
            continue;
        }
        let mut members = HashSet::new();
        let mut queue = VecDeque::from([seed]);
        seen.insert(seed);
        let mut range = Range {
            r0: seed.0,
            c0: seed.1,
            r1: seed.0,
            c1: seed.1,
        };
        while let Some((r, c)) = queue.pop_front() {
            members.insert((r, c));
            range.r0 = range.r0.min(r);
            range.r1 = range.r1.max(r);
            range.c0 = range.c0.min(c);
            range.c1 = range.c1.max(c);
            let neighbours = [
                (r.wrapping_sub(1), c),
                (r + 1, c),
                (r, c.wrapping_sub(1)),
                (r, c + 1),
            ];
            for n in neighbours {
                if occupied.contains(&n) && seen.insert(n) {
                    queue.push_back(n);
                }
            }
        }
        out.push(Island { members, range });
    }
    out
}

/// The anchors an island's row holds — cells with text, at their own
/// position or as the anchor of a merge — within the island.
fn row_anchors<'a>(
    isl: &Island,
    cells: &'a BTreeMap<(u32, u32), Cell>,
    r: u32,
) -> Vec<(u32, &'a Cell)> {
    (isl.range.c0..=isl.range.c1)
        .filter(|c| isl.members.contains(&(r, *c)))
        .filter_map(|c| cells.get(&(r, c)).map(|cell| (c, cell)))
        .collect()
}

/// Longest text that can be a title rather than a paragraph, in words.
const MAX_TITLE_WORDS: usize = 12;

/// Display type is set at least this much larger than the body.
const DISPLAY_SIZE_RATIO: f32 = 1.15;

fn title_like(cell: &Cell, modal_size: f32) -> bool {
    (cell.bold || cell.size >= modal_size * DISPLAY_SIZE_RATIO)
        && cell.text.split_whitespace().count() <= MAX_TITLE_WORDS
}

fn island_elements(
    isl: &Island,
    cells: &BTreeMap<(u32, u32), Cell>,
    merge_of: &HashMap<(u32, u32), Range>,
    sheet: &Sheet,
    modal_size: f32,
    page: usize,
) -> Vec<((u32, u32), Element)> {
    let mut out = Vec::new();
    let mut top = isl.range.r0;
    let mut bottom = isl.range.r1;

    // A lone cell is a title when it is set as one, else a paragraph.
    if isl.range.width() == 1 && isl.range.height() == 1 {
        if let Some((_, cell)) = row_anchors(isl, cells, top).first() {
            let (kind, level) = if title_like(cell, modal_size) {
                ("doco:SectionTitle", Some(2))
            } else {
                ("doco:Paragraph", None)
            };
            out.push((
                (top, isl.range.c0),
                element(kind, cell.text.clone(), level, page),
            ));
        }
        return out;
    }

    // Peel single-cell rows off the top: the first that is set as display
    // type is the island's title, the rest are paragraphs above the table.
    let mut titled = false;
    while top <= bottom {
        let anchors = row_anchors(isl, cells, top);
        match anchors.as_slice() {
            [] => top += 1,
            [(c, cell)] if isl.range.width() > 1 => {
                let title = !titled && title_like(cell, modal_size);
                titled |= title;
                let (kind, level) = if title {
                    ("doco:SectionTitle", Some(2))
                } else {
                    ("doco:Paragraph", None)
                };
                out.push(((top, *c), element(kind, cell.text.clone(), level, page)));
                top += 1;
            }
            _ => break,
        }
    }
    // And off the bottom: a lone cell under a table is its note. A table
    // is at least two rows, so a two-row block keeps its second row even
    // when that row holds one cell.
    while bottom > top {
        let anchors = row_anchors(isl, cells, bottom);
        match anchors.as_slice() {
            [] => bottom -= 1,
            [(c, cell)] if isl.range.width() > 1 && bottom - top >= 2 => {
                out.push((
                    (bottom, *c),
                    element("doco:Paragraph", cell.text.clone(), None, page),
                ));
                bottom -= 1;
            }
            _ => break,
        }
    }
    if top > bottom {
        return out;
    }

    // The table: trim columns the remaining rows leave empty, then read the
    // grid with merges as continuations.
    let (mut c0, mut c1) = (isl.range.c1, isl.range.c0);
    for r in top..=bottom {
        for (c, _) in row_anchors(isl, cells, r) {
            let m = merge_of.get(&(r, c)).copied();
            c0 = c0.min(c);
            c1 = c1.max(m.map_or(c, |m| m.c1.min(isl.range.c1)));
        }
    }
    if c1 < c0 {
        return out;
    }
    let range = Range {
        r0: top,
        c0,
        r1: bottom,
        c1,
    };
    let n_rows = range.height() as usize;
    let width = range.width() as usize;
    let mut grid = vec![String::new(); n_rows * width];
    let mut m_left = vec![false; n_rows * width];
    let mut m_down = vec![false; n_rows * width];
    let mut bold = vec![false; n_rows * width];
    for r in range.r0..=range.r1 {
        for c in range.c0..=range.c1 {
            if !isl.members.contains(&(r, c)) {
                continue;
            }
            let i = (r - range.r0) as usize * width + (c - range.c0) as usize;
            if let Some(cell) = cells.get(&(r, c)) {
                grid[i] = cell.text.clone();
                bold[i] = cell.bold;
            }
            if let Some(m) = merge_of.get(&(r, c)) {
                if c > m.c0 && c > range.c0 {
                    m_left[i] = true;
                }
                if r > m.r0 && r > range.r0 && c == m.c0.max(range.c0) {
                    m_down[i] = true;
                }
            }
        }
    }
    let rows: Vec<Vec<String>> = (0..n_rows)
        .map(|r| grid[r * width..(r + 1) * width].to_vec())
        .collect();
    if n_rows == 1 && width == 1 {
        let text = rows[0][0].clone();
        if !text.is_empty() {
            out.push((
                (range.r0, range.c0),
                element("doco:Paragraph", text, None, page),
            ));
        }
        return out;
    }

    let header_rows = header_rows(&range, &rows, &bold, sheet);
    // Banner bands inside the body: one merged cell across the full width.
    let sub_headers: Vec<usize> = (header_rows..n_rows)
        .filter(|&r| {
            let row = &rows[r];
            let filled = row.iter().filter(|c| !c.is_empty()).count();
            filled == 1
                && !row[0].is_empty()
                && width > 1
                && (1..width).all(|c| m_left[r * width + c])
        })
        .collect();

    let text = rows
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = element("doco:Table", text, None, page);
    e.cells = Some(rows);
    // Measured, so stated even when it is zero: absent would read as
    // undetected, and a consumer then presumes one.
    e.header_rows = Some(header_rows);
    e.sub_headers = (!sub_headers.is_empty()).then_some(sub_headers);
    e.merged_left = m_left.iter().any(|x| *x).then_some(m_left);
    e.merged_down = m_down.iter().any(|x| *x).then_some(m_down);
    out.push(((range.r0, range.c0), e));
    out
}

/// How many leading rows of a table are its header.
///
/// The sheet says so where it can: rows frozen by a pane, or the first row
/// of an autofilter range. Otherwise the first row is a header when it
/// names every column and is set bold, or names every column in text while
/// a later row carries numbers — a label row over data. A key/value block
/// whose first row is a label and a value has none.
fn header_rows(range: &Range, rows: &[Vec<String>], bold: &[bool], sheet: &Sheet) -> usize {
    let n_rows = rows.len();
    let width = rows.first().map_or(0, |r| r.len());
    if n_rows < 2 || width == 0 {
        return 0;
    }
    if sheet.frozen_rows > range.r0 {
        let n = (sheet.frozen_rows - range.r0) as usize;
        if n < n_rows {
            return n;
        }
    }
    if sheet
        .filtered
        .iter()
        .any(|f| f.r0 == range.r0 && f.c0 <= range.c1 && f.c1 >= range.c0)
    {
        return 1;
    }
    let first = &rows[0];
    if first.iter().any(|c| c.is_empty()) {
        return 0;
    }
    if bold[..width].iter().all(|b| *b) {
        return 1;
    }
    let numeric = |s: &str| {
        s.parse::<f64>().is_ok() || s.ends_with('%') && s[..s.len() - 1].parse::<f64>().is_ok()
    };
    let labels = first.iter().all(|c| !numeric(c));
    let data_below = rows[1..].iter().any(|r| r.iter().any(|c| numeric(c)));
    usize::from(labels && data_below)
}

fn local(qname: &[u8]) -> &str {
    let s = std::str::from_utf8(qname).unwrap_or("");
    s.rsplit(':').next().unwrap_or(s)
}

fn attr(e: &quick_xml::events::BytesStart<'_>, want: &str) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        (local(a.key.as_ref()) == want)
            .then(|| String::from_utf8_lossy(a.value.as_ref()).to_string())
    })
}

fn element(kind: &str, text: String, level: Option<usize>, page: usize) -> Element {
    Element {
        id: String::new(),
        kind: kind.into(),
        page,
        bbox: None,
        text,
        level,
        cells: None,
        header_rows: None,
        sub_headers: None,
        merged_down: None,
        merged_left: None,
        figure: None,
        links: None,
        turn: None,
        message: None,
        resumes: None,
        signature: false,
        provenance: "xlsx",
        evidence: "xlsx",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    /// A cell: reference, type attribute (`""` for a number), style index,
    /// and the inner XML.
    fn c(r: &str, t: &str, s: usize, inner: &str) -> String {
        let t = if t.is_empty() {
            String::new()
        } else {
            format!(" t=\"{t}\"")
        };
        format!("<c r=\"{r}\"{t} s=\"{s}\">{inner}</c>")
    }

    fn text(r: &str, s: usize, value: &str) -> String {
        c(r, "inlineStr", s, &format!("<is><t>{value}</t></is>"))
    }

    fn num(r: &str, s: usize, value: &str) -> String {
        c(r, "", s, &format!("<v>{value}</v>"))
    }

    /// Rows are grouped by the leading digits of the references.
    fn sheet(cells: &[String], extra: &str) -> String {
        let mut rows: BTreeMap<u32, Vec<&String>> = BTreeMap::new();
        for cell in cells {
            let r = cell.split("r=\"").nth(1).unwrap();
            let r = r.split('"').next().unwrap();
            let (row, _) = cell_ref(r).unwrap();
            rows.entry(row).or_default().push(cell);
        }
        let body: String = rows
            .iter()
            .map(|(r, cells)| {
                format!(
                    "<row r=\"{}\">{}</row>",
                    r + 1,
                    cells.iter().map(|c| c.as_str()).collect::<String>()
                )
            })
            .collect();
        format!("<worksheet {NS}>{extra}<sheetData>{body}</sheetData></worksheet>")
    }

    /// Styles 0: body; 1: bold body; 2: bold 16pt; 3: 9pt; 4: percent with
    /// one decimal; 5: date; 6: date-time.
    fn styles() -> Styles {
        let st = |format, bold, size| CellStyle { format, bold, size };
        Styles {
            xfs: vec![
                st(NumFmt::General, false, 11.0),
                st(NumFmt::General, true, 11.0),
                st(NumFmt::General, true, 16.0),
                st(NumFmt::General, false, 9.0),
                st(NumFmt::Percent(1), false, 11.0),
                st(NumFmt::Date, false, 11.0),
                st(NumFmt::DateTime, false, 11.0),
            ],
        }
    }

    fn parse_sheet(xml: &str) -> Vec<Element> {
        let styles = styles();
        let ctx = Context {
            shared: &[],
            styles: &styles,
            date1904: false,
        };
        parse_sheet_xml("Sheet1", xml, 3, &ctx).unwrap()
    }

    fn kinds(els: &[Element]) -> Vec<String> {
        els.iter()
            .map(|e| match e.level {
                Some(l) => format!("{}:{l}", e.kind),
                None => e.kind.clone(),
            })
            .collect()
    }

    #[test]
    fn column_letters_map_to_indices() {
        assert_eq!(column_index("A"), Some(0));
        assert_eq!(column_index("Z"), Some(25));
        assert_eq!(column_index("AA"), Some(26));
        assert_eq!(column_index("AB"), Some(27));
        assert_eq!(column_index("ZZ"), Some(701));
        assert_eq!(column_index(""), None);
        assert_eq!(column_index("A1"), None);
        assert_eq!(cell_ref("B7"), Some((6, 1)));
        assert_eq!(cell_ref("AA100"), Some((99, 26)));
        assert_eq!(
            parse_range("C4:A1"),
            Some(Range {
                r0: 0,
                c0: 0,
                r1: 3,
                c1: 2
            })
        );
    }

    #[test]
    fn general_numbers_are_the_shortest_round_trip() {
        assert_eq!(
            format_general("6.7400000000000002".parse().unwrap()),
            "6.74"
        );
        assert_eq!(format_general(1876.0), "1876");
        assert_eq!(format_general(-3.0), "-3");
        assert_eq!(format_general(0.5), "0.5");
        assert_eq!(format_general(1e-7), "0.0000001");
    }

    #[test]
    fn serial_dates_and_times_render_as_iso() {
        assert_eq!(format_serial(45123.0, NumFmt::Date, false), "2023-07-16");
        assert_eq!(
            format_serial(45123.75, NumFmt::DateTime, false),
            "2023-07-16 18:00:00"
        );
        assert_eq!(format_serial(0.5, NumFmt::Time, false), "12:00:00");
        assert_eq!(format_serial(1.0, NumFmt::Date, false), "1900-01-01");
        assert_eq!(format_serial(61.0, NumFmt::Date, false), "1900-03-01");
        assert_eq!(format_serial(0.0, NumFmt::Date, true), "1904-01-01");
        assert_eq!(format_serial(-1.0, NumFmt::Date, false), "-1");
    }

    #[test]
    fn custom_format_codes_classify_by_their_bare_letters() {
        assert_eq!(custom_fmt("yyyy-mm-dd"), NumFmt::Date);
        assert_eq!(custom_fmt("h:mm"), NumFmt::Time);
        assert_eq!(custom_fmt("d-mmm-yy h:mm"), NumFmt::DateTime);
        assert_eq!(custom_fmt("[$-409]mmm-yy;@"), NumFmt::Date);
        assert_eq!(custom_fmt("0.0%"), NumFmt::Percent(1));
        assert_eq!(custom_fmt("0%"), NumFmt::Percent(0));
        assert_eq!(custom_fmt("#,##0.00"), NumFmt::General);
        assert_eq!(custom_fmt("\"Days:\" 0"), NumFmt::General);
        assert_eq!(custom_fmt("0.00E+00"), NumFmt::General);
        assert_eq!(custom_fmt("General"), NumFmt::General);
        assert_eq!(custom_fmt("@"), NumFmt::General);
    }

    #[test]
    fn percent_cells_render_as_percentages() {
        let st = styles().xfs[4];
        assert_eq!(format_number("0.125", st, false), "12.5%");
        assert_eq!(format_number("1", st, false), "100.0%");
    }

    #[test]
    fn shared_strings_join_runs_and_skip_phonetics() {
        let xml = format!(
            "<sst {NS}><si><t>plain</t></si><si><r><t>rich </t></r><r><rPr><b/></rPr><t>text</t></r></si><si><t>漢字</t><rPh><t>かんじ</t></rPh></si><si/><si><t xml:space=\"preserve\"> spaced </t></si></sst>"
        );
        let s = parse_shared_strings(&xml).unwrap();
        assert_eq!(s, vec!["plain", "rich text", "漢字", "", " spaced "]);
    }

    #[test]
    fn styles_resolve_fonts_and_formats_by_index() {
        let xml = format!(
            "<styleSheet {NS}><numFmts count=\"1\"><numFmt numFmtId=\"164\" formatCode=\"yyyy-mm-dd\"/></numFmts>\
             <fonts count=\"3\"><font><sz val=\"11\"/></font><font><b/><sz val=\"14\"/></font><font/></fonts>\
             <cellStyleXfs count=\"1\"><xf numFmtId=\"10\" fontId=\"1\"/></cellStyleXfs>\
             <cellXfs count=\"4\"><xf numFmtId=\"0\" fontId=\"0\"/><xf numFmtId=\"164\" fontId=\"1\" applyNumberFormat=\"1\"/><xf numFmtId=\"10\" fontId=\"2\"><alignment/></xf><xf numFmtId=\"22\" fontId=\"1\"/></cellXfs></styleSheet>"
        );
        let st = parse_styles(&xml).unwrap();
        assert_eq!(st.xfs.len(), 4, "cellStyleXfs must not be counted");
        assert_eq!(
            st.xfs[0],
            CellStyle {
                format: NumFmt::General,
                bold: false,
                size: 11.0
            }
        );
        assert_eq!(
            st.xfs[1],
            CellStyle {
                format: NumFmt::Date,
                bold: true,
                size: 14.0
            }
        );
        assert_eq!(st.xfs[2].format, NumFmt::Percent(2));
        assert_eq!(st.xfs[3].format, NumFmt::DateTime);
        assert!(st.xfs[3].bold);
    }

    #[test]
    fn a_sheet_opens_with_its_name_and_its_index_is_the_page() {
        let els = parse_sheet(&sheet(&[text("A1", 0, "hello")], ""));
        assert_eq!(kinds(&els), vec!["doco:SectionTitle:1", "doco:Paragraph"]);
        assert_eq!(els[0].text, "Sheet1");
        assert!(els.iter().all(|e| e.page == 3));
        assert!(els.iter().all(|e| e.bbox.is_none()));
    }

    #[test]
    fn a_block_with_a_bold_first_row_is_a_table_with_a_header() {
        let xml = sheet(
            &[
                text("A1", 1, "Part"),
                text("B1", 1, "Description"),
                text("C1", 1, "Weight"),
                text("A2", 0, "BR-100"),
                text("B2", 0, "Bracket"),
                num("C2", 0, "6.7400000000000002"),
                text("A3", 0, "BR-101"),
                text("B3", 0, "Bracket, wide"),
                num("C3", 0, "9"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(kinds(&els), vec!["doco:SectionTitle:1", "doco:Table"]);
        let t = &els[1];
        assert_eq!(t.header_rows, Some(1));
        let cells = t.cells.as_ref().unwrap();
        assert_eq!(cells[0], vec!["Part", "Description", "Weight"]);
        assert_eq!(cells[1], vec!["BR-100", "Bracket", "6.74"]);
        assert_eq!(cells[2][2], "9");
        assert!(t.merged_left.is_none() && t.merged_down.is_none());
    }

    #[test]
    fn a_label_row_over_numbers_is_a_header_without_bold() {
        let xml = sheet(
            &[
                text("A1", 0, "Year"),
                text("B1", 0, "Total"),
                num("A2", 0, "2023"),
                num("B2", 0, "17"),
            ],
            "",
        );
        assert_eq!(parse_sheet(&xml)[1].header_rows, Some(1));
    }

    #[test]
    fn a_label_value_block_gets_no_header() {
        let xml = sheet(
            &[
                text("A1", 1, "Owner"),
                text("B1", 0, "Facilities team"),
                text("A2", 1, "Budget"),
                text("B2", 0, "Q3 plan"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(kinds(&els), vec!["doco:SectionTitle:1", "doco:Table"]);
        assert_eq!(els[1].header_rows, Some(0));
    }

    #[test]
    fn a_title_on_top_of_a_block_is_peeled_as_a_heading() {
        let xml = sheet(
            &[
                text("A1", 2, "Sales by region"),
                text("A2", 1, "Region"),
                text("B2", 1, "Units"),
                text("A3", 0, "EMEA"),
                num("B3", 0, "28"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(
            kinds(&els),
            vec!["doco:SectionTitle:1", "doco:SectionTitle:2", "doco:Table"]
        );
        assert_eq!(els[1].text, "Sales by region");
        assert_eq!(els[2].cells.as_ref().unwrap().len(), 2);
        assert_eq!(els[2].header_rows, Some(1));
    }

    #[test]
    fn only_the_first_peeled_row_is_the_title() {
        // A bold title, a bold code under it, a long summary: one heading
        // and two paragraphs, then the fields as a table.
        let xml = sheet(
            &[
                text("A1", 2, "Project brief — Bridge repaint"),
                text("A2", 1, "P-01"),
                text("B3", 1, "Repaint the north span before the autumn closure"),
                text("A4", 1, "Owner"),
                text("B4", 0, "Facilities team"),
                text("C4", 0, "due in May"),
                text("A5", 1, "Budget"),
                text("B5", 0, "Q3 plan"),
            ],
            "<mergeCells count=\"2\"><mergeCell ref=\"A1:B1\"/><mergeCell ref=\"A2:B2\"/></mergeCells>",
        );
        let els = parse_sheet(&xml);
        assert_eq!(
            kinds(&els),
            vec![
                "doco:SectionTitle:1",
                "doco:SectionTitle:2",
                "doco:Paragraph",
                "doco:Paragraph",
                "doco:Table"
            ]
        );
        assert_eq!(els[2].text, "P-01");
        let t = &els[4];
        let cells = t.cells.as_ref().unwrap();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0], vec!["Owner", "Facilities team", "due in May"]);
        assert_eq!(cells[1], vec!["Budget", "Q3 plan", ""]);
        assert_eq!(t.header_rows, Some(0));
    }

    #[test]
    fn a_lone_cell_is_a_heading_when_set_as_display_type() {
        // A title in A1, a blank row, the table from A3: two islands.
        let xml = sheet(
            &[
                text("A1", 2, "Quarterly figures"),
                text("A3", 1, "Q"),
                text("B3", 1, "Revenue"),
                text("A4", 0, "Q1"),
                num("B4", 0, "10"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(
            kinds(&els),
            vec!["doco:SectionTitle:1", "doco:SectionTitle:2", "doco:Table"]
        );
        // The same cell in body type is a paragraph.
        let xml = sheet(&[text("A1", 0, "Just a remark"), text("A3", 0, "x")], "");
        assert_eq!(
            kinds(&parse_sheet(&xml)),
            vec!["doco:SectionTitle:1", "doco:Paragraph", "doco:Paragraph"]
        );
    }

    #[test]
    fn an_empty_column_separates_two_tables() {
        let xml = sheet(
            &[
                text("A1", 1, "L"),
                text("B1", 1, "M"),
                num("A2", 0, "1"),
                num("B2", 0, "2"),
                text("D1", 1, "R"),
                text("E1", 1, "S"),
                num("D2", 0, "3"),
                num("E2", 0, "4"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(
            kinds(&els),
            vec!["doco:SectionTitle:1", "doco:Table", "doco:Table"]
        );
        assert_eq!(els[1].cells.as_ref().unwrap()[0], vec!["L", "M"]);
        assert_eq!(els[2].cells.as_ref().unwrap()[0], vec!["R", "S"]);
    }

    #[test]
    fn merged_ranges_show_their_anchor_and_hide_the_rest() {
        let xml = sheet(
            &[
                text("A1", 1, "Item"),
                text("B1", 1, "Q1"),
                text("C1", 1, "Q2"),
                text("A2", 0, "Group A"),
                num("B2", 0, "1"),
                num("C2", 0, "2"),
                text("A3", 0, "stale"),
                num("B3", 0, "3"),
                num("C3", 0, "4"),
                text("A4", 0, "Both quarters"),
                num("B4", 0, "9"),
                text("C4", 0, "hidden"),
            ],
            "<mergeCells count=\"2\"><mergeCell ref=\"A2:A3\"/><mergeCell ref=\"B4:C4\"/></mergeCells>",
        );
        let els = parse_sheet(&xml);
        let t = &els[1];
        let cells = t.cells.as_ref().unwrap();
        assert_eq!(
            cells[2],
            vec!["", "3", "4"],
            "the hidden cell's stale text is not shown"
        );
        assert_eq!(cells[3], vec!["Both quarters", "9", ""]);
        let w = 3;
        let md = t.merged_down.as_ref().unwrap();
        assert!(md[2 * w], "A3 continues A2");
        let ml = t.merged_left.as_ref().unwrap();
        assert!(ml[3 * w + 2], "C4 continues B4");
        assert!(!ml[3 * w + 1]);
    }

    #[test]
    fn a_note_under_a_table_is_a_paragraph() {
        let xml = sheet(
            &[
                text("A1", 1, "Part"),
                text("B1", 1, "Price"),
                text("A2", 0, "BR-100"),
                num("B2", 0, "12.5"),
                text("A3", 3, "Note: prices are provisional."),
            ],
            "<mergeCells count=\"1\"><mergeCell ref=\"A3:B3\"/></mergeCells>",
        );
        let els = parse_sheet(&xml);
        assert_eq!(
            kinds(&els),
            vec!["doco:SectionTitle:1", "doco:Table", "doco:Paragraph"]
        );
        assert_eq!(els[1].cells.as_ref().unwrap().len(), 2);
        assert!(els[2].text.starts_with("Note:"));
    }

    #[test]
    fn frozen_rows_are_the_header() {
        let xml = sheet(
            &[
                text("A1", 0, "Region"),
                text("B1", 0, "Metric"),
                text("A2", 0, "Region"),
                text("B2", 0, "Units"),
                text("A3", 0, "EMEA"),
                text("B3", 0, "many"),
            ],
            "<sheetViews><sheetView workbookViewId=\"0\"><pane ySplit=\"2\" topLeftCell=\"A3\" activePane=\"bottomLeft\" state=\"frozen\"/></sheetView></sheetViews>",
        );
        assert_eq!(parse_sheet(&xml)[1].header_rows, Some(2));
    }

    #[test]
    fn an_autofilter_range_names_its_header_row() {
        let xml = sheet(
            &[
                text("A1", 0, "Name"),
                text("B1", 0, "Team"),
                text("A2", 0, "Ada"),
                text("B2", 0, "Blue"),
            ],
            "<autoFilter ref=\"A1:B2\"/>",
        );
        assert_eq!(parse_sheet(&xml)[1].header_rows, Some(1));
    }

    #[test]
    fn a_full_width_merged_row_inside_a_table_is_a_sub_header() {
        let xml = sheet(
            &[
                text("A1", 1, "Part"),
                text("B1", 1, "Price"),
                text("A2", 1, "Fasteners"),
                text("A3", 0, "BR-100"),
                num("B3", 0, "12.5"),
            ],
            "<mergeCells count=\"1\"><mergeCell ref=\"A2:B2\"/></mergeCells>",
        );
        let t = &parse_sheet(&xml)[1];
        assert_eq!(t.header_rows, Some(1));
        assert_eq!(t.sub_headers, Some(vec![1]));
    }

    #[test]
    fn values_read_through_their_types() {
        let styles = styles();
        let ctx = Context {
            shared: &["from the table".to_string()],
            styles: &styles,
            date1904: false,
        };
        let xml = sheet(
            &[
                c("A1", "s", 0, "<v>0</v>"),
                text("B1", 0, "two\nlines"),
                c("C1", "b", 0, "<v>1</v>"),
                c("D1", "e", 0, "<v>#N/A</v>"),
                c("E1", "str", 0, "<f>A1&amp;B1</f><v>joined</v>"),
                c("F1", "", 0, "<f>SUM(A2:A9)</f><v>42</v>"),
                c("G1", "", 5, "<v>45123</v>"),
                c("H1", "", 6, "<v>45123.75</v>"),
                c("I1", "d", 0, "<v>2023-07-16</v>"),
                c("J1", "", 4, "<v>0.125</v>"),
            ],
            "",
        );
        let els = parse_sheet_xml("S", &xml, 0, &ctx).unwrap();
        let row = &els[1].cells.as_ref().unwrap()[0];
        assert_eq!(
            row,
            &[
                "from the table",
                "two lines",
                "TRUE",
                "#N/A",
                "joined",
                "42",
                "2023-07-16",
                "2023-07-16 18:00:00",
                "2023-07-16",
                "12.5%"
            ]
        );
    }

    #[test]
    fn a_formula_with_no_cached_value_holds_nothing() {
        // Saved without recalculation: the cell has a formula and no `<v>`.
        // Nothing is computed here, so the cell is empty and the sheet has
        // only its title.
        let xml = sheet(&[c("A1", "", 0, "<f>NOW()</f>")], "");
        assert_eq!(kinds(&parse_sheet(&xml)), vec!["doco:SectionTitle:1"]);
    }

    #[test]
    fn cells_without_references_follow_in_sequence() {
        // No `r` attributes anywhere: rows and cells count from where the
        // previous one left off, and a self-closing `<c/>` takes a column.
        let xml = format!(
            "<worksheet {NS}><sheetData><row><c t=\"inlineStr\"><is><t>a</t></is></c><c t=\"inlineStr\"><is><t>b</t></is></c><c t=\"inlineStr\"><is><t>x</t></is></c></row><row><c t=\"inlineStr\"><is><t>c</t></is></c><c/><c><v>3</v></c></row></sheetData></worksheet>"
        );
        let cells = parse_sheet(&xml)[1].cells.clone().unwrap();
        assert_eq!(cells, vec![vec!["a", "b", "x"], vec!["c", "", "3"]]);
    }

    fn package(sheets: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write;
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let mut put = |name: &str, body: &str| {
            zw.start_file(name, opts).unwrap();
            zw.write_all(body.as_bytes()).unwrap();
        };
        let list: String = sheets
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                format!(
                    "<sheet name=\"{name}\" sheetId=\"{}\" r:id=\"rId{}\"/>",
                    i + 1,
                    i + 1
                )
            })
            .collect();
        put(
            "xl/workbook.xml",
            &format!("<workbook {NS}><sheets>{list}</sheets></workbook>"),
        );
        // Relationship ids deliberately do not follow file numbering.
        let rels: String = sheets
            .iter()
            .enumerate()
            .map(|(i, _)| {
                format!(
                    "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{}.xml\"/>",
                    i + 1,
                    sheets.len() - i
                )
            })
            .collect();
        put(
            "xl/_rels/workbook.xml.rels",
            &format!("<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{rels}</Relationships>"),
        );
        for (i, (_, xml)) in sheets.iter().enumerate() {
            put(&format!("xl/worksheets/sheet{}.xml", sheets.len() - i), xml);
        }
        zw.finish().unwrap().into_inner()
    }

    #[test]
    fn sheets_come_out_in_workbook_order_as_pages() {
        let first = sheet(&[text("A1", 0, "one")], "");
        let second = sheet(&[text("A1", 0, "two")], "");
        let bytes = package(&[("Index", &first), ("Detail", &second)]);
        let els = parse(&bytes).unwrap();
        let titles: Vec<(&str, usize)> = els
            .iter()
            .filter(|e| e.kind == "doco:SectionTitle")
            .map(|e| (e.text.as_str(), e.page))
            .collect();
        assert_eq!(titles, vec![("Index", 0), ("Detail", 1)]);
        assert_eq!(els[1].text, "one");
        assert_eq!(els[3].text, "two");
        assert!(els
            .iter()
            .enumerate()
            .all(|(i, e)| e.id == format!("elem-{:05}", i + 1)));
    }

    #[test]
    fn a_package_without_a_workbook_is_refused() {
        use std::io::Write;
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zw.start_file("word/document.xml", opts).unwrap();
        zw.write_all(b"<w:document/>").unwrap();
        let bytes = zw.finish().unwrap().into_inner();
        assert!(matches!(parse(&bytes), Err(XlsxError::NoWorkbook)));
        assert!(matches!(parse(b"not a zip"), Err(XlsxError::Zip(_))));
    }
}
