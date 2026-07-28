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
    parse_document_xml(&xml)
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
}

#[derive(Default)]
struct Para {
    text: String,
    style: String,
    numbered: bool,
}

pub fn parse_document_xml(xml: &str) -> Result<Vec<Element>, DocxError> {
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
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut row: Vec<Cell> = Vec::new();
    let mut cell = Cell::default();
    let mut in_cell = false;

    loop {
        match r.read_event_into(&mut buf) {
            Err(e) => return Err(DocxError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.name();
                let tag = local(name.as_ref());
                match tag {
                    "t" => in_text = true,
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
                    push(&mut para, &mut cell, in_cell, &s);
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                match local(name.as_ref()) {
                    "t" => in_text = false,
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
                        row.push(std::mem::take(&mut cell));
                    }
                    "tr" => rows.push(std::mem::take(&mut row)),
                    "tbl" => {
                        table_depth = table_depth.saturating_sub(1);
                        if table_depth == 0 {
                            emit_table(&mut out, std::mem::take(&mut rows));
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
fn emit_table(out: &mut Vec<Element>, rows: Vec<Vec<Cell>>) {
    let rows: Vec<Vec<Cell>> = rows.into_iter().filter(|r| !r.is_empty()).collect();
    if rows.is_empty() {
        return;
    }
    let width = rows
        .iter()
        .map(|r| r.iter().map(|c| c.span.max(1)).sum::<usize>())
        .max()
        .unwrap_or(0);
    if width == 0 {
        return;
    }
    let n_rows = rows.len();
    let mut grid = vec![String::new(); n_rows * width];
    let mut m_left = vec![false; n_rows * width];
    let mut m_down = vec![false; n_rows * width];

    for (r, cells) in rows.iter().enumerate() {
        let mut c = 0usize;
        for cell in cells {
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
    }

    let cells: Vec<Vec<String>> = (0..n_rows)
        .map(|r| grid[r * width..(r + 1) * width].to_vec())
        .collect();
    let text = cells
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = element("doco:Table", text, None);
    // Word marks a repeating header with w:tblHeader; absent that, the first
    // row is the header by the same convention every other reader uses.
    e.header_rows = Some(1.min(n_rows));
    e.cells = Some(cells);
    e.merged_left = m_left.iter().any(|x| *x).then_some(m_left);
    e.merged_down = m_down.iter().any(|x| *x).then_some(m_down);
    out.push(e);
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
