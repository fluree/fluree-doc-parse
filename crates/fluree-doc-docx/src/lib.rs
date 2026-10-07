//! DOCX (OOXML) to DoCO-typed document elements.
//!
//! Word declares outright what a PDF makes us infer, and the difference runs
//! the whole way through: `w:pStyle` names the heading level, `w:numPr` marks
//! a list item, `w:tbl` bounds a real table, and `w:gridSpan` / `w:vMerge`
//! state cell merges that the PDF engine has to read back out of ruling
//! geometry. So this reader measures nothing — it maps.
//!
//! Two consequences worth being explicit about:
//!
//! * **No geometry.** A `.docx` stores a flow, not a layout; page boundaries
//!   and coordinates only exist once something lays it out. Every element's
//!   `bbox` is `None` and `page` is 0, rather than a zeroed box that would
//!   read as a real position.
//! * **No escalation.** There is nothing for a model tier to arbitrate: the
//!   structure is not a hypothesis.

use fluree_doc_model::Element;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::{HashMap, HashSet};
use std::io::Read;

#[derive(Debug)]
pub enum DocxError {
    Zip(String),
    Xml(String),
    NoDocument,
}

impl std::fmt::Display for DocxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zip(e) => write!(f, "not a readable .docx: {e}"),
            Self::Xml(e) => write!(f, "malformed document.xml: {e}"),
            Self::NoDocument => write!(f, "archive has no word/document.xml"),
        }
    }
}

impl std::error::Error for DocxError {}

/// Parse a `.docx` file's bytes into document elements in reading order.
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, DocxError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| DocxError::Zip(e.to_string()))?;
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .map_err(|_| DocxError::NoDocument)?
        .read_to_string(&mut xml)
        .map_err(|e| DocxError::Xml(e.to_string()))?;
    // Styles only refine what a table's first row is; a package without
    // them, or with ones that do not parse, still reads.
    let mut styles = String::new();
    let read = zip
        .by_name("word/styles.xml")
        .is_ok_and(|mut f| f.read_to_string(&mut styles).is_ok());
    let header_styles = if read {
        header_row_styles(&styles)
    } else {
        HashSet::new()
    };
    parse_with_styles(&xml, &header_styles)
}

/// The table styles that set a table's first row apart — bold or shaded
/// through a `firstRow` conditional format — directly or through the style
/// they are based on.
///
/// Word applies that formatting only where the table's `w:tblLook` turns
/// the first-row condition on. python-docx turns it on for every table it
/// writes, so the look alone says nothing; what says the first row is a
/// header is a style that draws it as one. `Table Grid`, the style most
/// generated tables use, has no conditional formats at all.
fn header_row_styles(xml: &str) -> HashSet<String> {
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut marked: HashSet<String> = HashSet::new();
    let mut based_on: HashMap<String, String> = HashMap::new();
    let mut style: Option<String> = None;
    let mut in_first_row = false;
    loop {
        match r.read_event_into(&mut buf) {
            Err(_) | Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.name();
                match local(name.as_ref()) {
                    "style" => {
                        style = (attr(&e, "type").as_deref() == Some("table"))
                            .then(|| attr(&e, "styleId"))
                            .flatten();
                    }
                    "basedOn" => {
                        if let (Some(s), Some(v)) = (&style, attr(&e, "val")) {
                            based_on.insert(s.clone(), v);
                        }
                    }
                    "tblStylePr" => {
                        in_first_row = attr(&e, "type").as_deref() == Some("firstRow");
                    }
                    "b" if in_first_row && !is_off(&e) => {
                        marked.extend(style.clone());
                    }
                    "shd" if in_first_row => {
                        let fill = attr(&e, "fill").unwrap_or_default().to_ascii_lowercase();
                        if !fill.is_empty() && fill != "auto" && fill != "ffffff" {
                            marked.extend(style.clone());
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                "style" => style = None,
                "tblStylePr" => in_first_row = false,
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    // A style inherits its parent's first-row format. The walk is bounded
    // so a cycle in a damaged file cannot hang it.
    let ids: Vec<String> = based_on.keys().cloned().collect();
    for id in ids {
        let mut at = id.clone();
        for _ in 0..16 {
            if marked.contains(&at) {
                marked.insert(id.clone());
                break;
            }
            match based_on.get(&at) {
                Some(parent) => at = parent.clone(),
                None => break,
            }
        }
    }
    marked
}

/// A toggle property (`w:b`, `w:tblHeader`) switched off by its value.
fn is_off(e: &quick_xml::events::BytesStart<'_>) -> bool {
    attr(e, "val").is_some_and(|v| matches!(v.as_str(), "0" | "false" | "off"))
}

/// A `w:pStyle` value as a heading level, if it names one.
///
/// Word's built-in styles are `Heading1`..`Heading9`; localised templates and
/// hand-built ones vary, so the digit suffix is what carries the meaning.
fn heading_level(style: &str) -> Option<usize> {
    let s = style.trim();
    let rest = s
        .strip_prefix("Heading")
        .or_else(|| s.strip_prefix("heading"))
        .or_else(|| s.strip_prefix("berschrift"))?; // German "Überschrift"
    rest.trim_start_matches('-')
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=9).contains(n))
        .map(|n| n.min(6))
}

#[derive(Default)]
struct Cell {
    text: String,
    /// `w:gridSpan` — how many grid columns this cell occupies.
    span: usize,
    /// `w:vMerge` with no `val`, i.e. a continuation of the cell above.
    v_continue: bool,
    /// Some of the cell's text is set in a bold run, and some is not.
    bold_text: bool,
    plain_text: bool,
}

impl Cell {
    /// All of the cell's text is bold — and it has some.
    fn bold(&self) -> bool {
        self.bold_text && !self.plain_text
    }
}

#[derive(Default)]
struct Row {
    cells: Vec<Cell>,
    /// `w:tblHeader`: Word repeats this row atop every page the table
    /// runs onto, which is what a header row is.
    repeats: bool,
}

/// What a table's own properties say about its first row: its style draws
/// it as a header and its `w:tblLook` turns that formatting on.
#[derive(Default, Clone)]
struct TableLook {
    style: Option<String>,
    first_row: bool,
}

#[derive(Default)]
struct Para {
    text: String,
    style: String,
    numbered: bool,
}

pub fn parse_document_xml(xml: &str) -> Result<Vec<Element>, DocxError> {
    parse_with_styles(xml, &HashSet::new())
}

/// [`parse_document_xml`], knowing which table styles set their first row
/// apart as a header (see [`header_row_styles`]).
fn parse_with_styles(
    xml: &str,
    header_styles: &HashSet<String>,
) -> Result<Vec<Element>, DocxError> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);

    let mut out: Vec<Element> = Vec::new();
    let mut buf = Vec::new();

    let mut para = Para::default();
    let mut in_text = false;
    // Table state. `depth` tracks nesting so an inner table's rows are not
    // stolen by the outer one; nested tables are flattened in reading order,
    // which is what a flat cell grid can express.
    let mut table_depth = 0usize;
    let mut rows: Vec<Row> = Vec::new();
    let mut row = Row::default();
    let mut cell = Cell::default();
    let mut in_cell = false;
    let mut look = TableLook::default();
    // Run formatting: whether the run being read is bold. Only a run's own
    // `w:rPr` counts; the one inside `w:pPr` formats the paragraph mark.
    let mut in_ppr = false;
    let mut run_bold = false;

    loop {
        match r.read_event_into(&mut buf) {
            Err(e) => return Err(DocxError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.name();
                let tag = local(name.as_ref());
                match tag {
                    "t" => in_text = true,
                    "pPr" => in_ppr = true,
                    "r" => run_bold = false,
                    "b" if !in_ppr => run_bold = !is_off(&e),
                    "rStyle" if !in_ppr => {
                        if attr(&e, "val").is_some_and(|v| v == "Strong") {
                            run_bold = true;
                        }
                    }
                    "tblHeader" => row.repeats = !is_off(&e),
                    "tblStyle" if table_depth == 1 => look.style = attr(&e, "val"),
                    "tblLook" if table_depth == 1 => {
                        // Either the explicit attribute or, in older files,
                        // the 0x0020 bit of the hex bitmask.
                        look.first_row = match attr(&e, "firstRow") {
                            Some(v) => matches!(v.as_str(), "1" | "true" | "on"),
                            None => attr(&e, "val")
                                .and_then(|v| u32::from_str_radix(&v, 16).ok())
                                .is_some_and(|v| v & 0x0020 != 0),
                        };
                    }
                    "tab" => push(&mut para, &mut cell, in_cell, "\t"),
                    "br" | "cr" => push(&mut para, &mut cell, in_cell, " "),
                    "pStyle" => {
                        if let Some(v) = attr(&e, "val") {
                            para.style = v;
                        }
                    }
                    "numPr" => para.numbered = true,
                    "gridSpan" => {
                        if let Some(v) = attr(&e, "val") {
                            cell.span = v.parse().unwrap_or(1);
                        }
                    }
                    "vMerge" => {
                        // `val="restart"` begins a merge; absent means continue.
                        cell.v_continue = !matches!(attr(&e, "val").as_deref(), Some("restart"));
                    }
                    "tbl" => {
                        table_depth += 1;
                        if table_depth == 1 {
                            rows.clear();
                            look = TableLook::default();
                        }
                    }
                    "tc" => {
                        in_cell = true;
                        cell = Cell {
                            span: 1,
                            ..Default::default()
                        };
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if in_text {
                    let s = t.unescape().unwrap_or_default().to_string();
                    if in_cell && !s.trim().is_empty() {
                        if run_bold {
                            cell.bold_text = true;
                        } else {
                            cell.plain_text = true;
                        }
                    }
                    push(&mut para, &mut cell, in_cell, &s);
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                match local(name.as_ref()) {
                    "t" => in_text = false,
                    "pPr" => in_ppr = false,
                    "p" => {
                        if in_cell {
                            // Paragraphs inside a cell are one cell's content.
                            if !cell.text.is_empty() && !cell.text.ends_with(' ') {
                                cell.text.push(' ');
                            }
                        } else {
                            flush_para(&mut out, &mut para);
                        }
                    }
                    "tc" => {
                        in_cell = false;
                        row.cells.push(std::mem::take(&mut cell));
                    }
                    "tr" => rows.push(std::mem::take(&mut row)),
                    "tbl" => {
                        table_depth = table_depth.saturating_sub(1);
                        if table_depth == 0 {
                            let styled = look.first_row
                                && look
                                    .style
                                    .as_ref()
                                    .is_some_and(|s| header_styles.contains(s));
                            emit_table(&mut out, std::mem::take(&mut rows), styled);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }
    flush_para(&mut out, &mut para);
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    Ok(out)
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

fn push(para: &mut Para, cell: &mut Cell, in_cell: bool, s: &str) {
    if in_cell {
        cell.text.push_str(s);
    } else {
        para.text.push_str(s);
    }
}

fn element(kind: &str, text: String, level: Option<usize>) -> Element {
    Element {
        id: String::new(),
        kind: kind.into(),
        page: 0,
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
        provenance: "docx",
        evidence: "docx",
    }
}

/// Word marks a list two ways and uses both: `w:numPr` attaches real
/// numbering, while the built-in `List Bullet` / `List Number` /
/// `List Paragraph` styles carry list formatting with no numbering
/// properties at all. Reading only `numPr` leaves those as paragraphs.
fn is_list_style(style: &str) -> bool {
    let s = style.trim().to_ascii_lowercase().replace(['-', ' '], "");
    s.starts_with("list") || s.starts_with("bullet")
}

fn flush_para(out: &mut Vec<Element>, para: &mut Para) {
    let p = std::mem::take(para);
    let text = p.text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return;
    }
    if let Some(level) = heading_level(&p.style) {
        out.push(element("doco:SectionTitle", text, Some(level)));
    } else if p.numbered || is_list_style(&p.style) {
        out.push(element("doco:ListItem", text, None));
    } else {
        out.push(element("doco:Paragraph", text, None));
    }
}

/// Turn Word's row/cell tree into the flat grid the model uses, carrying the
/// declared merges across as `merged_left` / `merged_down`.
///
/// A `w:gridSpan` of n occupies n grid columns, so the cell is emitted once
/// and the columns it covers are marked as continuing it — exactly the
/// convention the PDF engine derives from ruling. `w:vMerge` without
/// `val="restart"` continues the cell above.
///
/// `styled` says the table's style draws its first row as a header.
fn emit_table(out: &mut Vec<Element>, rows: Vec<Row>, styled: bool) {
    let rows: Vec<Row> = rows.into_iter().filter(|r| !r.cells.is_empty()).collect();
    if rows.is_empty() {
        return;
    }
    let width = rows
        .iter()
        .map(|r| r.cells.iter().map(|c| c.span.max(1)).sum::<usize>())
        .max()
        .unwrap_or(0);
    if width == 0 {
        return;
    }
    let n_rows = rows.len();
    let mut grid = vec![String::new(); n_rows * width];
    let mut m_left = vec![false; n_rows * width];
    let mut m_down = vec![false; n_rows * width];
    // Rows that are one cell across the whole grid.
    let mut full_width = vec![false; n_rows];

    for (r, row) in rows.iter().enumerate() {
        let mut c = 0usize;
        for cell in &row.cells {
            if c >= width {
                break;
            }
            let span = cell.span.max(1).min(width - c);
            grid[r * width + c] = cell.text.split_whitespace().collect::<Vec<_>>().join(" ");
            for k in 1..span {
                m_left[r * width + c + k] = true;
            }
            if cell.v_continue && r > 0 {
                m_down[r * width + c] = true;
            }
            c += span;
        }
        full_width[r] = row.cells.len() == 1 && width > 1 && row.cells[0].span.max(1) >= width;
    }

    let cells: Vec<Vec<String>> = (0..n_rows)
        .map(|r| grid[r * width..(r + 1) * width].to_vec())
        .collect();
    let header_rows = header_rows(&rows, &cells, &m_left, &m_down, styled);
    // A body row that is one cell across the whole table labels the rows
    // beneath it, as a section band; the last row has nothing to label.
    let sub_headers: Vec<usize> = (header_rows..n_rows.saturating_sub(1))
        .filter(|&r| full_width[r] && !cells[r][0].is_empty() && !m_down[r * width])
        .collect();
    let text = cells
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = element("doco:Table", text, None);
    e.header_rows = Some(header_rows);
    e.sub_headers = (!sub_headers.is_empty()).then_some(sub_headers);
    e.cells = Some(cells);
    e.merged_left = m_left.iter().any(|x| *x).then_some(m_left);
    e.merged_down = m_down.iter().any(|x| *x).then_some(m_down);
    out.push(e);
}

/// How many leading rows of a table are its header.
///
/// Word does not require a table to have one, and a key/value block — a
/// bold label column beside amounts — has none: taking its first row as
/// the header names the columns `Subtotal` and `38.60` and leaves the
/// subtotal in no cell. So a row is a header only where the document says
/// so:
///
/// * rows marked `w:tblHeader`, which Word repeats on every page;
/// * else a first row set apart — bold while the row under it is not, or
///   drawn as a header by the table's style, unless it is typed like the
///   data under it;
/// * else a first row that reads as labels: every column named, none of
///   the names a number or a sentence, over at least one column of numbers
///   or across three columns or more.
///
/// A header found the second or third way runs on through the rows its
/// cells reach down into (`w:vMerge`): `Item` merged over two rows beside
/// `Price` spanning `Unit` and `Line` is a two-row header.
fn header_rows(
    rows: &[Row],
    cells: &[Vec<String>],
    m_left: &[bool],
    m_down: &[bool],
    styled: bool,
) -> usize {
    let n_rows = rows.len();
    if n_rows < 2 {
        return 0;
    }
    let repeated = rows.iter().take_while(|r| r.repeats).count();
    if repeated > 0 {
        return repeated.min(n_rows - 1);
    }
    let width = cells[0].len();
    let all_bold = |r: &Row| {
        r.cells.iter().any(|c| c.bold()) && r.cells.iter().all(|c| c.bold() || !c.plain_text)
    };
    // A label column — the first cell bold on every row, no other cell
    // bold anywhere — makes the table a list of fields, `Description |
    // Cinnamon bun` over `Code | BK-215`: its first row is a field like
    // the rest, not a header.
    let labels_down = rows.iter().any(|r| r.cells.first().is_some_and(Cell::bold))
        && rows
            .iter()
            .filter(|r| r.cells.first().is_some_and(|c| c.bold_text || c.plain_text))
            .all(|r| r.cells[0].bold())
        && rows
            .iter()
            .all(|r| r.cells.iter().skip(1).all(|c| !c.bold()));
    if width >= 2 && labels_down {
        return 0;
    }
    // Columns whose body is mostly numbers — all numbers in a table of two
    // columns, where a field list's values (`Cinnamon bun`, `BK-215`, `1`,
    // `£7.90`) are as often numbers as not, and a line table's amounts
    // are numbers throughout.
    let numeric_cols: Vec<usize> = (0..width)
        .filter(|&c| {
            let body: Vec<&str> = cells[1..]
                .iter()
                .map(|r| r[c].as_str())
                .filter(|t| !t.is_empty())
                .collect();
            let numbers = body.iter().filter(|t| is_numeric(t)).count();
            !body.is_empty()
                && if width == 2 {
                    numbers == body.len()
                } else {
                    numbers * 2 > body.len()
                }
        })
        .collect();
    let labelled = if all_bold(&rows[0]) && !all_bold(&rows[1]) {
        true
    } else if styled {
        // A style formats whatever row comes first, so its say is weighed
        // against the text: a first row that is numbers where the body is
        // numbers (`Subtotal | 38.60` in a shaded key/value table) is data.
        let typed_as_data = numeric_cols
            .iter()
            .filter(|&&c| is_numeric(&cells[0][c]))
            .count();
        numeric_cols.is_empty() || typed_as_data * 2 <= numeric_cols.len()
    } else {
        let named = (0..width).all(|c| !cells[0][c].is_empty() || m_left[c]);
        let labels = cells[0]
            .iter()
            .all(|t| !is_numeric(t) && t.split_whitespace().count() <= MAX_LABEL_WORDS);
        // Over numbers a row of labels is a header. Over text it is one
        // only in a table wider than a key/value pair: two columns of text
        // are as often `Date | 11 August 2026` as `Name | Role`, while a
        // row naming three or more columns is a header row.
        named && labels && (!numeric_cols.is_empty() || width >= 3)
    };
    if !labelled {
        return 0;
    }
    let mut n = 1;
    while n + 1 < n_rows && (0..width).any(|c| m_down[n * width + c]) {
        n += 1;
    }
    n
}

/// Longest text a column label runs to, in words; longer is prose.
const MAX_LABEL_WORDS: usize = 6;

/// A cell that reads as a quantity: digits dressed with a sign, grouping,
/// a decimal point, a currency symbol or a percent sign, and nothing else.
/// A bare four-digit year is a label (`2023 | 2024` over amounts is a
/// header), and a date's separators make it no number.
fn is_numeric(s: &str) -> bool {
    let s = s.trim();
    if !s.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    if s.len() == 4 && s.parse::<u32>().is_ok_and(|y| (1900..2100).contains(&y)) {
        return false;
    }
    let body = s.trim_start_matches(['-', '+', '\u{2212}', '(']);
    body.chars().all(|c| {
        c.is_ascii_digit()
            || matches!(
                c,
                '$' | '€' | '£' | '¥' | '%' | ',' | '.' | ')' | ' ' | '\u{a0}'
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

    fn doc(body: &str) -> String {
        format!("<w:document {NS}><w:body>{body}</w:body></w:document>")
    }

    fn para(style: Option<&str>, text: &str) -> String {
        let p = style
            .map(|s| format!("<w:pPr><w:pStyle w:val=\"{s}\"/></w:pPr>"))
            .unwrap_or_default();
        format!("<w:p>{p}<w:r><w:t>{text}</w:t></w:r></w:p>")
    }

    #[test]
    fn heading_styles_give_their_level_directly() {
        let x = doc(&format!(
            "{}{}{}",
            para(Some("Heading1"), "Title"),
            para(None, "Body text"),
            para(Some("Heading3"), "Deeper")
        ));
        let els = parse_document_xml(&x).unwrap();
        assert_eq!(els.len(), 3);
        assert_eq!(els[0].kind, "doco:SectionTitle");
        assert_eq!(els[0].level, Some(1));
        assert_eq!(els[1].kind, "doco:Paragraph");
        assert_eq!(els[2].level, Some(3));
    }

    #[test]
    fn heading_level_parses_localised_and_odd_styles() {
        assert_eq!(heading_level("Heading2"), Some(2));
        assert_eq!(heading_level("heading4"), Some(4));
        assert_eq!(
            heading_level("Heading9"),
            Some(6),
            "clamped to the emitters"
        );
        assert_eq!(heading_level("Normal"), None);
        assert_eq!(heading_level("HeadingChar"), None);
    }

    #[test]
    fn numbered_paragraphs_are_list_items() {
        let x = doc("<w:p><w:pPr><w:numPr><w:ilvl w:val=\"0\"/></w:numPr></w:pPr><w:r><w:t>one</w:t></w:r></w:p>");
        let els = parse_document_xml(&x).unwrap();
        assert_eq!(els[0].kind, "doco:ListItem");
        assert_eq!(els[0].text, "one");
    }

    #[test]
    fn list_styles_are_list_items_even_without_numbering() {
        // Word's List Bullet style carries no w:numPr, so numbering alone
        // misses it.
        let x = doc(&format!(
            "{}{}",
            para(Some("ListBullet"), "bulleted"),
            para(Some("ListParagraph"), "also a list item")
        ));
        let els = parse_document_xml(&x).unwrap();
        assert_eq!(els[0].kind, "doco:ListItem");
        assert_eq!(els[1].kind, "doco:ListItem");
        assert!(!is_list_style("Normal"));
        assert!(!is_list_style("Heading1"));
    }

    #[test]
    fn nothing_claims_geometry() {
        let x = doc(&para(Some("Heading1"), "Title"));
        let els = parse_document_xml(&x).unwrap();
        assert!(els.iter().all(|e| e.bbox.is_none()));
        assert!(els.iter().all(|e| e.provenance == "docx"));
    }

    #[test]
    fn declared_merges_carry_across() {
        // Row 1: one cell spanning both columns. Row 2: two cells, the first
        // beginning a vertical merge. Row 3: that merge continuing.
        let x = doc(concat!(
            "<w:tbl>",
            "<w:tr><w:tc><w:tcPr><w:gridSpan w:val=\"2\"/></w:tcPr><w:p><w:r><w:t>Banner</w:t></w:r></w:p></w:tc></w:tr>",
            "<w:tr><w:tc><w:tcPr><w:vMerge w:val=\"restart\"/></w:tcPr><w:p><w:r><w:t>Left</w:t></w:r></w:p></w:tc>",
            "<w:tc><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc></w:tr>",
            "<w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p></w:p></w:tc>",
            "<w:tc><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc></w:tr>",
            "</w:tbl>"
        ));
        let els = parse_document_xml(&x).unwrap();
        let t = els.iter().find(|e| e.kind == "doco:Table").unwrap();
        let cells = t.cells.as_ref().unwrap();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0][0], "Banner");
        let ml = t.merged_left.as_ref().expect("gridSpan recorded");
        assert!(ml[1], "the spanned column continues the banner cell");
        let md = t.merged_down.as_ref().expect("vMerge recorded");
        assert!(md[2 * 2], "row 3 column 0 continues the cell above");
        assert!(!md[2 * 2 + 1], "column 1 is its own cell");
    }

    #[test]
    fn a_table_of_plain_cells_needs_no_merge_flags() {
        let x = doc(concat!(
            "<w:tbl>",
            "<w:tr><w:tc><w:p><w:r><w:t>Year</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Total</w:t></w:r></w:p></w:tc></w:tr>",
            "<w:tr><w:tc><w:p><w:r><w:t>2024</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>12</w:t></w:r></w:p></w:tc></w:tr>",
            "</w:tbl>"
        ));
        let t = parse_document_xml(&x)
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "doco:Table")
            .unwrap();
        assert_eq!(t.header_rows, Some(1));
        assert!(t.merged_left.is_none() && t.merged_down.is_none());
        assert_eq!(t.cells.unwrap()[1], vec!["2024", "12"]);
    }

    /// A table cell: text, bold or not, and any `w:tcPr` content.
    fn tc(text: &str, bold: bool, pr: &str) -> String {
        let b = if bold { "<w:rPr><w:b/></w:rPr>" } else { "" };
        format!("<w:tc><w:tcPr>{pr}</w:tcPr><w:p><w:r>{b}<w:t>{text}</w:t></w:r></w:p></w:tc>")
    }

    fn table(rows: &[(&str, Vec<String>)]) -> Element {
        let body: String = rows
            .iter()
            .map(|(pr, cells)| format!("<w:tr><w:trPr>{pr}</w:trPr>{}</w:tr>", cells.concat()))
            .collect();
        parse_document_xml(&doc(&format!("<w:tbl>{body}</w:tbl>")))
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "doco:Table")
            .unwrap()
    }

    #[test]
    fn a_key_value_table_has_no_header_row() {
        // A bold label column beside amounts: the first row is data.
        let kv = table(&[
            ("", vec![tc("Subtotal", true, ""), tc("38.60", false, "")]),
            ("", vec![tc("Tax", true, ""), tc("3.86", false, "")]),
            ("", vec![tc("Total", true, ""), tc("42.46", false, "")]),
        ]);
        assert_eq!(kv.header_rows, Some(0));
        // Labels over a column of numbers are a header without any mark.
        let lines = table(&[
            (
                "",
                vec![
                    tc("Item", false, ""),
                    tc("Qty", false, ""),
                    tc("Unit price", false, ""),
                ],
            ),
            (
                "",
                vec![
                    tc("Fish pie", false, ""),
                    tc("2", false, ""),
                    tc("14.50", false, ""),
                ],
            ),
            (
                "",
                vec![
                    tc("Lemonade", false, ""),
                    tc("3", false, ""),
                    tc("3.20", false, ""),
                ],
            ),
        ]);
        assert_eq!(lines.header_rows, Some(1));
        // Text over text has a header only where the document marks one:
        // bold over plain, or w:tblHeader.
        let plain = |first_bold: bool, pr: &'static str| {
            table(&[
                (
                    pr,
                    vec![tc("Name", first_bold, ""), tc("Role", first_bold, "")],
                ),
                ("", vec![tc("Ada", false, ""), tc("Engineer", false, "")]),
            ])
            .header_rows
        };
        assert_eq!(plain(false, ""), Some(0));
        // Three columns of labels over text name their columns.
        let wide = table(&[
            (
                "",
                vec![
                    tc("Activity", false, ""),
                    tc("Marketing", false, ""),
                    tc("Contracts", false, ""),
                ],
            ),
            (
                "",
                vec![
                    tc("Order approval", false, ""),
                    tc("R", false, ""),
                    tc("I", false, ""),
                ],
            ),
        ]);
        assert_eq!(wide.header_rows, Some(1));
        assert_eq!(plain(true, ""), Some(1));
        assert_eq!(plain(false, "<w:tblHeader/>"), Some(1));
        assert_eq!(plain(false, "<w:tblHeader w:val=\"0\"/>"), Some(0));
    }

    #[test]
    fn a_field_list_has_no_header_row() {
        // A bold label column, the values plain, no header marked.
        let fields = table(&[
            (
                "",
                vec![tc("Description", true, ""), tc("Cinnamon bun", false, "")],
            ),
            ("", vec![tc("Code", true, ""), tc("BK-215", false, "")]),
            ("", vec![tc("Quantity", true, ""), tc("1", false, "")]),
            ("", vec![tc("Price each", true, ""), tc("£7.90", false, "")]),
            ("", vec![tc("Total paid", true, ""), tc("£9.00", false, "")]),
        ]);
        assert_eq!(fields.header_rows, Some(0));
        // The same without the bold: values of mixed type are fields.
        let plain = table(&[
            (
                "",
                vec![tc("Description", false, ""), tc("Cinnamon bun", false, "")],
            ),
            ("", vec![tc("Code", false, ""), tc("BK-215", false, "")]),
            ("", vec![tc("Quantity", false, ""), tc("1", false, "")]),
            (
                "",
                vec![tc("Total paid", false, ""), tc("£9.00", false, "")],
            ),
        ]);
        assert_eq!(plain.header_rows, Some(0));
        // A bold header over a bold first column is still a header.
        let lines = table(&[
            ("", vec![tc("Item", true, ""), tc("Amount", true, "")]),
            ("", vec![tc("Fish pie", true, ""), tc("29.00", false, "")]),
            ("", vec![tc("Lemonade", true, ""), tc("9.60", false, "")]),
        ]);
        assert_eq!(lines.header_rows, Some(1));
        // And two columns of amounts under labels need no bold.
        let lines = table(&[
            ("", vec![tc("Item", false, ""), tc("Amount", false, "")]),
            ("", vec![tc("Fish pie", false, ""), tc("29.00", false, "")]),
            ("", vec![tc("Lemonade", false, ""), tc("9.60", false, "")]),
        ]);
        assert_eq!(lines.header_rows, Some(1));
    }

    #[test]
    fn a_header_merged_down_into_a_second_row_is_a_stacked_header() {
        let restart = "<w:vMerge w:val=\"restart\"/>";
        let t = table(&[
            (
                "",
                vec![
                    tc("Item", true, restart),
                    tc("Qty", true, restart),
                    tc("Price", true, "<w:gridSpan w:val=\"2\"/>"),
                ],
            ),
            (
                "",
                vec![
                    tc("", false, "<w:vMerge/>"),
                    tc("", false, "<w:vMerge/>"),
                    tc("Unit", true, ""),
                    tc("Line", true, ""),
                ],
            ),
            (
                "",
                vec![
                    tc("Fish pie", false, ""),
                    tc("2", false, ""),
                    tc("14.50", false, ""),
                    tc("29.00", false, ""),
                ],
            ),
        ]);
        assert_eq!(t.header_rows, Some(2));
    }

    #[test]
    fn a_row_of_one_cell_across_the_table_is_a_section_band() {
        let band = |text: &str| tc(text, false, "<w:gridSpan w:val=\"2\"/>");
        let t = table(&[
            ("", vec![tc("Item", true, ""), tc("Price", true, "")]),
            ("", vec![band("Food")]),
            ("", vec![tc("Fish pie", false, ""), tc("14.50", false, "")]),
            ("", vec![tc("Bread", false, ""), tc("", false, "")]),
            ("", vec![band("Thank you")]),
        ]);
        assert_eq!(t.header_rows, Some(1));
        // A row missing a value is not a band, and the last row labels
        // nothing.
        assert_eq!(t.sub_headers, Some(vec![1]));
    }

    #[test]
    fn a_table_style_that_sets_its_first_row_apart_names_a_header() {
        let styles = format!(
            "<w:styles {NS}>\
             <w:style w:type=\"table\" w:styleId=\"TableGrid\"><w:tblPr/></w:style>\
             <w:style w:type=\"table\" w:styleId=\"LightShading\"><w:tblStylePr w:type=\"firstRow\"><w:rPr><w:b/></w:rPr></w:tblStylePr>\
             <w:tblStylePr w:type=\"firstCol\"><w:rPr><w:b/></w:rPr></w:tblStylePr></w:style>\
             <w:style w:type=\"table\" w:styleId=\"Mine\"><w:basedOn w:val=\"LightShading\"/></w:style>\
             <w:style w:type=\"table\" w:styleId=\"ColOnly\"><w:tblStylePr w:type=\"firstCol\"><w:rPr><w:b/></w:rPr></w:tblStylePr></w:style>\
             </w:styles>"
        );
        let marked = header_row_styles(&styles);
        assert!(marked.contains("LightShading") && marked.contains("Mine"));
        assert!(!marked.contains("TableGrid") && !marked.contains("ColOnly"));
        let x = |style: &str, look: &str| {
            doc(&format!(
                "<w:tbl><w:tblPr><w:tblStyle w:val=\"{style}\"/><w:tblLook {look}/></w:tblPr>\
                 <w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>",
                tc("Name", false, ""),
                tc("Role", false, ""),
                tc("Ada", false, ""),
                tc("Engineer", false, "")
            ))
        };
        let header = |xml: String| parse_with_styles(&xml, &marked).unwrap()[0].header_rows;
        assert_eq!(header(x("Mine", "w:firstRow=\"1\"")), Some(1));
        assert_eq!(header(x("Mine", "w:val=\"04A0\"")), Some(1));
        assert_eq!(header(x("Mine", "w:firstRow=\"0\"")), Some(0));
        assert_eq!(header(x("TableGrid", "w:firstRow=\"1\"")), Some(0));
        // The style shades whatever row is first; a key/value table's first
        // row is typed like its data and stays data.
        let kv = doc(&format!(
            "<w:tbl><w:tblPr><w:tblStyle w:val=\"Mine\"/><w:tblLook w:firstRow=\"1\"/></w:tblPr>\
             <w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>",
            tc("Subtotal", false, ""),
            tc("38.60", false, ""),
            tc("Total", false, ""),
            tc("42.46", false, "")
        ));
        assert_eq!(header(kv), Some(0));
    }

    #[test]
    fn runs_within_a_paragraph_join_without_spurious_breaks() {
        let x = doc("<w:p><w:r><w:t>Hello </w:t></w:r><w:r><w:t>world</w:t></w:r></w:p>");
        let els = parse_document_xml(&x).unwrap();
        assert_eq!(els.len(), 1);
        assert_eq!(els[0].text, "Hello world");
    }

    #[test]
    fn a_missing_document_part_is_an_error_not_a_panic() {
        let empty: &[u8] = b"not a zip";
        assert!(parse(empty).is_err());
    }
}
