//! OpenDocument files (`.odt`, `.ods`, `.odp`) to DoCO-typed elements.
//!
//! An OpenDocument file is a zip whose `mimetype` entry says which kind it
//! is, whose `content.xml` holds its body, `styles.xml` its page headers and
//! footers and `meta.xml` what it says about itself. Like the Office
//! formats, it declares its structure, so the readers map rather than
//! measure, and carry no geometry.
//!
//! * **Text** (`.odt`, `.ott`) reads as a Word file does: a heading's
//!   outline level is its level, list items are list items, tables are
//!   tables with their declared header rows and merges; footnotes, endnotes
//!   and comments follow the body marked by their `evidence`, a note
//!   opening with its citation, a comment with its author; the page header
//!   and footer are read once, before and after.
//! * **Spreadsheets** (`.ods`, `.ots`) read as a workbook does
//!   ([`fluree_doc_xlsx::grid_elements`]): each sheet a page, its blocks of
//!   cells tables, each cell what it shows with what it stores typed beside
//!   it. Rows and cells repeated by `number-rows-repeated` and
//!   `number-columns-repeated` are counted, not copied, while they are empty;
//!   a repeated cell that holds something is a cell each time, within
//!   [`MAX_CELLS`], so a few bytes cannot ask for billions.
//! * **Presentations** (`.odp`, `.otp`) read as a deck does: each slide a
//!   page, its title frame a level-1 heading, its text, lists and tables.
//!
//! **Bounds**, as for any archive: each part at most [`MAX_PART_BYTES`]
//! inflated, checked as it inflates, and its XML at most
//! [`tree::MAX_NODES`] nodes nested [`tree::MAX_DEPTH`] deep. The tables of
//! a text or a presentation hold at most [`MAX_TEXT_CELLS`] cells in all,
//! a repeated cell counted each time and a short row padded to the
//! table's width counted too, and at most [`MAX_TEXT_CELL_BYTES`] bytes of
//! cell text, a repeated cell's text counted each time it is copied; both
//! are counted before the cells are made, and a document past either is
//! refused.

mod sheet;
mod tree;

use fluree_doc_model::{DocumentInfo, Element};
use fluree_doc_ooxml::Package;
use tree::{Role, Tree};

/// The most bytes one part is inflated to: 64 MiB.
pub const MAX_PART_BYTES: u64 = 64 << 20;

/// The most cells a spreadsheet's sheets may hold, repeats counted.
pub const MAX_CELLS: usize = 1 << 22;

/// The most cells the tables of a text or a presentation hold in all,
/// repeats and padding counted.
pub const MAX_TEXT_CELLS: usize = 1 << 22;

/// The most bytes of text the cells of a text's or a presentation's tables
/// hold in all, a repeated cell's counted each time.
pub const MAX_TEXT_CELL_BYTES: usize = 64 << 20;

/// What the tables of a document may still make: cells, and bytes of their
/// text. Spent before anything is made; past it, the document is refused
/// ([`Room::over`]), and nothing more is made.
struct Room {
    cells: usize,
    bytes: usize,
    over: Option<String>,
}

impl Room {
    fn new() -> Self {
        Room {
            cells: MAX_TEXT_CELLS,
            bytes: MAX_TEXT_CELL_BYTES,
            over: None,
        }
    }

    /// Spend `cells` cells holding `bytes` bytes of text; `false`, and the
    /// document refused, where they are not left.
    fn spend(&mut self, cells: usize, bytes: usize) -> bool {
        if self.over.is_some() {
            return false;
        }
        match (self.cells.checked_sub(cells), self.bytes.checked_sub(bytes)) {
            (Some(c), Some(b)) => {
                (self.cells, self.bytes) = (c, b);
                true
            }
            (None, _) => {
                self.over = Some(format!("its tables hold more than {MAX_TEXT_CELLS} cells"));
                false
            }
            (_, None) => {
                self.over = Some(format!(
                    "its tables hold more than {MAX_TEXT_CELL_BYTES} bytes of text"
                ));
                false
            }
        }
    }

    /// The document, or why it is refused.
    fn check<T>(self, read: T) -> Result<T, OdfError> {
        match self.over {
            Some(why) => Err(OdfError::TooLarge(why)),
            None => Ok(read),
        }
    }
}

/// The three kinds of OpenDocument this crate reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Spreadsheet,
    Presentation,
}

impl Format {
    /// The kind a package says it is, by its `mimetype` entry.
    pub fn sniff(bytes: &[u8]) -> Option<Format> {
        if !bytes.starts_with(b"PK") {
            return None;
        }
        let mut pkg = Package::open(bytes).ok()?;
        let m = pkg.read_bytes("mimetype", 256).ok()?;
        let m = String::from_utf8_lossy(&m);
        let kind = m
            .trim()
            .strip_prefix("application/vnd.oasis.opendocument.")?;
        match kind {
            "text" | "text-template" | "text-web" | "text-master" => Some(Format::Text),
            "spreadsheet" | "spreadsheet-template" => Some(Format::Spreadsheet),
            "presentation" | "presentation-template" => Some(Format::Presentation),
            _ => None,
        }
    }

    /// The provenance and evidence its elements carry.
    pub fn source(self) -> &'static str {
        match self {
            Format::Text => "odt",
            Format::Spreadsheet => "ods",
            Format::Presentation => "odp",
        }
    }
}

#[derive(Debug)]
pub enum OdfError {
    /// Not a zip, or a broken one.
    Zip(String),
    /// No `mimetype` naming a text, spreadsheet or presentation.
    NotOdf,
    /// No `content.xml`.
    NoContent,
    /// Past a bound.
    TooLarge(String),
}

impl std::fmt::Display for OdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zip(e) => write!(f, "not a readable OpenDocument file: {e}"),
            Self::NotOdf => write!(f, "not an OpenDocument text, spreadsheet or presentation"),
            Self::NoContent => write!(f, "the package has no content.xml"),
            Self::TooLarge(e) => write!(f, "OpenDocument file too large: {e}"),
        }
    }
}

impl std::error::Error for OdfError {}

/// Parse an OpenDocument file, whichever of the three kinds it is, with what
/// it declares about itself in `meta.xml`.
pub fn parse_with_info(bytes: &[u8]) -> Result<(Vec<Element>, DocumentInfo, Format), OdfError> {
    let format = Format::sniff(bytes).ok_or(OdfError::NotOdf)?;
    let mut pkg = Package::open(bytes).map_err(OdfError::Zip)?;
    let content = part(&mut pkg, "content.xml")?.ok_or(OdfError::NoContent)?;
    let content = Tree::parse(&content).map_err(OdfError::TooLarge)?;
    let mut out = match format {
        Format::Text => {
            let styles = part(&mut pkg, "styles.xml")?
                .map(|s| Tree::parse(&s).map_err(OdfError::TooLarge))
                .transpose()?;
            let mut content = content;
            content.roles = style_roles(&content, styles.as_ref());
            let mut room = Room::new();
            let read = text_elements(&content, styles.as_ref(), &mut room);
            room.check(read)?
        }
        Format::Spreadsheet => sheet::elements(&content)?,
        Format::Presentation => {
            let mut room = Room::new();
            let read = slide_elements(&content, &mut room);
            room.check(read)?
        }
    };
    let source = format.source();
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
        e.provenance = source;
        if e.evidence == "odf" {
            e.evidence = source;
        }
    }
    let info = part(&mut pkg, "meta.xml")?
        .and_then(|m| Tree::parse(&m).ok())
        .map(|m| info(&m))
        .unwrap_or_default();
    Ok((out, info, format))
}

/// [`parse_with_info`], the elements alone.
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, OdfError> {
    parse_with_info(bytes).map(|(e, _, _)| e)
}

/// A part's text, at most [`MAX_PART_BYTES`]; `None` where the package has
/// no such part.
fn part(pkg: &mut Package<'_>, name: &str) -> Result<Option<String>, OdfError> {
    if !pkg.part_names().iter().any(|n| n == name) {
        return Ok(None);
    }
    pkg.read_bytes(name, MAX_PART_BYTES)
        .map(|b| Some(String::from_utf8_lossy(&b).into_owned()))
        .map_err(OdfError::TooLarge)
}

/// What `meta.xml` declares: title, author, created and modified.
fn info(meta: &Tree) -> DocumentInfo {
    let text = |name: &str| {
        meta.find(0, name)
            .map(|n| meta.all_text(n).trim().to_string())
            .filter(|t| !t.is_empty())
    };
    DocumentInfo {
        title: text("title"),
        creators: text("initial-creator")
            .or_else(|| text("creator"))
            .into_iter()
            .collect(),
        created: text("creation-date").and_then(|d| fluree_doc_model::xsd_date_time(&d)),
        modified: text("date").and_then(|d| fluree_doc_model::xsd_date_time(&d)),
    }
}

fn element(kind: &str, text: String, level: Option<usize>, page: usize) -> Element {
    Element {
        page,
        level,
        ..Element::new(kind, text, "odf", "odf")
    }
}

/// A note or comment met inside a paragraph, read after the body.
struct Aside {
    /// `footnote`, `endnote` or `comment`.
    evidence: &'static str,
    /// Its citation (`1`, `i`) or its author, opening its first paragraph.
    label: String,
    /// The element holding its paragraphs.
    body: usize,
}

/// A paragraph's text as shown: spans and links read through, `text:s` as
/// a space (its count is spacing, which [`tidy`] makes one space anyway, and
/// a few bytes of it would otherwise be thousands), a tab as a space, a line
/// break as a line; notes and comments collected into `asides` rather than
/// read inline; paragraphs and list items (in a table cell) apart. A frame
/// is read after its paragraph ([`frames_in`]).
fn para_text(t: &Tree, id: usize, asides: &mut Vec<Aside>) -> String {
    let mut s = String::new();
    for &c in &t.node(id).children {
        let n = t.node(c);
        match n.name.as_str() {
            "" => s.push_str(&n.text),
            "s" => s.push(' '),
            "tab" => s.push(' '),
            "line-break" => s.push('\n'),
            "note" => {
                let evidence = if t.attr(c, "note-class") == Some("endnote") {
                    "endnote"
                } else {
                    "footnote"
                };
                let label = t
                    .find(c, "note-citation")
                    .map(|n| t.all_text(n).trim().to_string())
                    .unwrap_or_default();
                if let Some(body) = t.find(c, "note-body") {
                    asides.push(Aside {
                        evidence,
                        label,
                        body,
                    });
                }
            }
            "annotation" => {
                let label = t
                    .find(c, "creator")
                    .map(|n| format!("{}:", t.all_text(n).trim()))
                    .filter(|l| l.len() > 1)
                    .unwrap_or_default();
                asides.push(Aside {
                    evidence: "comment",
                    label,
                    body: c,
                });
            }
            // What carries no text of its own.
            "annotation-end" | "bookmark" | "bookmark-start" | "bookmark-end"
            | "soft-page-break" | "reference-mark" | "frame" | "change" | "change-start"
            | "change-end" => {}
            // A block in a cell: apart from its neighbours.
            "p" | "h" | "list" | "list-item" | "list-header" => {
                s.push(' ');
                s.push_str(&para_text(t, c, asides));
                s.push(' ');
            }
            _ => s.push_str(&para_text(t, c, asides)),
        }
    }
    s
}

/// The frames a paragraph holds, anchored to it or to a character in it,
/// in order: their text is read after the paragraph. Not those inside a
/// note or a comment, which are read with it, nor a frame's own frames,
/// which are read with it.
fn frames_in(t: &Tree, id: usize, out: &mut Vec<usize>) {
    for c in t.elements(id) {
        match t.node(c).name.as_str() {
            "frame" => out.push(c),
            "note" | "annotation" => {}
            _ => frames_in(t, c, out),
        }
    }
}

/// A paragraph's text tidied: each line's runs of whitespace one space.
fn tidy(s: &str) -> String {
    s.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A table as a `doco:Table`: its rows, inside header rows or not, each
/// cell its paragraphs joined; a cell spanning columns or rows makes its
/// covered cells continuations; the rows `table-header-rows` holds are its
/// header.
fn table(
    t: &Tree,
    id: usize,
    page: usize,
    asides: &mut Vec<Aside>,
    room: &mut Room,
) -> Option<Element> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut spans: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut header_rows = 0usize;
    let mut cells = 0usize;
    let order = rows_in_order(t, id);
    for (r, header) in order {
        let mut row = Vec::new();
        for c in t.elements(r) {
            let name = t.node(c).name.as_str();
            if name != "table-cell" && name != "covered-table-cell" {
                continue;
            }
            let repeat = t
                .attr(c, "number-columns-repeated")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(1)
                .clamp(1, 1024);
            let text = t
                .elements(c)
                .map(|p| tidy(&para_text(t, p, asides)))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let span = |a: &str| {
                t.attr(c, a)
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(1)
            };
            let (cs, rs) = (span("number-columns-spanned"), span("number-rows-spanned"));
            if name == "table-cell" && (cs > 1 || rs > 1) {
                spans.push((rows.len(), row.len(), cs, rs));
            }
            // Each copy is a cell and its text: counted before any is made.
            cells += repeat;
            if !room.spend(repeat, text.len().saturating_mul(repeat)) {
                return None;
            }
            row.extend(std::iter::repeat_n(text, repeat));
        }
        if header {
            header_rows += 1;
        }
        rows.push(row);
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return None;
    }
    // The grid a short row is padded to: rows without cells and one long
    // row would otherwise make a grid of their product.
    let n = rows.len();
    let grid = n.saturating_mul(width);
    if !room.spend(grid - cells, 0) {
        return None;
    }
    for r in &mut rows {
        r.resize(width, String::new());
    }
    let mut left = vec![false; grid];
    let mut down = vec![false; grid];
    // Each merge marks the cells it covers, within the grid; the merges'
    // areas together are at most the grid's, as merges that do not overlap
    // are, so overlapping ones (a hostile file's) cost no more than one
    // pass over the table.
    let mut marks = grid;
    for (r0, c0, cs, rs) in spans {
        let (r1, c1) = (
            r0.saturating_add(rs).min(n),
            c0.saturating_add(cs).min(width),
        );
        let area = (r1 - r0.min(r1)) * (c1 - c0.min(c1));
        let Some(rest) = marks.checked_sub(area) else {
            break;
        };
        marks = rest;
        for r in r0..r1 {
            for c in c0..c1 {
                if c > c0 {
                    left[r * width + c] = true;
                }
                if r > r0 && c == c0 {
                    down[r * width + c] = true;
                }
            }
        }
    }
    let text = rows
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = element("doco:Table", text, None, page);
    e.cells = Some(rows);
    e.header_rows = Some(header_rows);
    e.merged_left = left.iter().any(|x| *x).then_some(left);
    e.merged_down = down.iter().any(|x| *x).then_some(down);
    // What DoCO copies of it, a merged value in each cell it covers, as
    // cells holding that text.
    let copies = fluree_doc_model::doco::table_copies(&e);
    let size = |n: u64| usize::try_from(n).unwrap_or(usize::MAX);
    if !room.spend(size(copies.cells), size(copies.text)) {
        return None;
    }
    Some(e)
}

/// A table's rows in document order, through header rows and row groups,
/// each with whether it is a header row.
fn rows_in_order(t: &Tree, id: usize) -> Vec<(usize, bool)> {
    let mut out = Vec::new();
    fn walk(t: &Tree, n: usize, header: bool, out: &mut Vec<(usize, bool)>) {
        for c in t.elements(n) {
            match t.node(c).name.as_str() {
                "table-row" => out.push((c, header)),
                "table-header-rows" => walk(t, c, true, out),
                "table-rows" | "table-row-group" => walk(t, c, header, out),
                _ => {}
            }
        }
    }
    walk(t, id, false, &mut out);
    out
}

/// The flow of a text body, a text box or a note: headings, paragraphs,
/// lists, tables, sections.
fn flow(
    t: &Tree,
    id: usize,
    page: usize,
    list: bool,
    out: &mut Vec<Element>,
    asides: &mut Vec<Aside>,
    room: &mut Room,
) {
    // A paragraph's frames, read after it: a text box anchored to it.
    let frames_after =
        |c: usize, out: &mut Vec<Element>, asides: &mut Vec<Aside>, room: &mut Room| {
            let mut frames = Vec::new();
            frames_in(t, c, &mut frames);
            for f in frames {
                flow(t, f, page, false, out, asides, room);
            }
        };
    for c in t.elements(id) {
        match t.node(c).name.as_str() {
            "h" => {
                let level = t
                    .attr(c, "outline-level")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 6);
                let text = tidy(&para_text(t, c, asides));
                if !text.is_empty() {
                    out.push(element("doco:SectionTitle", text, Some(level), page));
                }
                frames_after(c, out, asides, room);
            }
            "p" => {
                let text = tidy(&para_text(t, c, asides));
                if text.is_empty() {
                    frames_after(c, out, asides, room);
                    continue;
                }
                let style = t.attr(c, "style-name");
                // A style named `Title` is the built-in one, defined or not.
                let role = style
                    .and_then(|s| t.roles.get(s))
                    .or((style == Some("Title")).then_some(&Role::Title));
                let (kind, level) = match role {
                    _ if list => ("doc:ListItem", None),
                    Some(Role::Title) => ("doco:Title", None),
                    Some(Role::Heading(l)) => ("doco:SectionTitle", Some(*l)),
                    None => ("doco:Paragraph", None),
                };
                out.push(element(kind, text, level, page));
                frames_after(c, out, asides, room);
            }
            "list" | "list-item" | "list-header" => flow(t, c, page, true, out, asides, room),
            "table" => {
                if let Some(e) = table(t, c, page, asides, room) {
                    out.push(e);
                }
            }
            "section" | "index-body" | "table-of-content" | "alphabetical-index"
            | "illustration-index" | "table-index" | "bibliography" | "text-box" | "frame" => {
                flow(t, c, page, list, out, asides, room)
            }
            _ => {}
        }
    }
}

/// Notes and comments, after what cited them: each a run of paragraphs
/// marked by its `evidence`, its first opening with its label.
fn read_asides(t: &Tree, asides: Vec<Aside>, page: usize, out: &mut Vec<Element>, room: &mut Room) {
    let mut notes = Vec::new();
    let mut comments = Vec::new();
    for a in asides {
        let mut els = Vec::new();
        let mut nested = Vec::new();
        flow(t, a.body, page, false, &mut els, &mut nested, room);
        for (i, e) in els.iter_mut().enumerate() {
            if e.kind != "doco:Table" {
                e.kind = "doco:Paragraph".into();
                e.level = None;
                if i == 0 && !a.label.is_empty() {
                    e.text = format!("{} {}", a.label, e.text);
                }
            }
            e.evidence = a.evidence;
        }
        if a.evidence == "comment" {
            comments.extend(els);
        } else {
            notes.extend(els);
        }
    }
    out.extend(notes);
    out.extend(comments);
}

/// A text document: the header, the body, its notes, the footer, then its
/// comments.
fn text_elements(content: &Tree, styles: Option<&Tree>, room: &mut Room) -> Vec<Element> {
    let mut body = Vec::new();
    let mut asides = Vec::new();
    if let Some(text) = content.find(0, "text") {
        flow(content, text, 0, false, &mut body, &mut asides, room);
    }
    let (notes, comments): (Vec<Aside>, Vec<Aside>) =
        asides.into_iter().partition(|a| a.evidence != "comment");
    let mut after = Vec::new();
    read_asides(content, notes, 0, &mut after, room);
    let mut furniture = |name: &str, evidence: &'static str| -> Vec<Element> {
        let mut out: Vec<Element> = Vec::new();
        // What a master page's header says that another's said already.
        let mut seen = std::collections::HashSet::new();
        let Some(styles) = styles else {
            return out;
        };
        let Some(masters) = styles.find(0, "master-styles") else {
            return out;
        };
        for m in styles.elements(masters) {
            for h in styles.elements(m).filter(|&h| styles.node(h).name == name) {
                let mut els = Vec::new();
                flow(styles, h, 0, false, &mut els, &mut Vec::new(), room);
                for mut e in els {
                    if e.kind != "doco:Table" {
                        e.kind = "doco:Paragraph".into();
                        e.level = None;
                    }
                    e.evidence = evidence;
                    if seen.insert(e.text.clone()) {
                        out.push(e);
                    }
                }
            }
        }
        out
    };
    let mut out = furniture("header", "header");
    let footer = furniture("footer", "footer");
    out.extend(body);
    out.extend(after);
    out.extend(footer);
    read_asides(content, comments, 0, &mut out, room);
    out
}

/// What each paragraph style makes a paragraph set in it, through the styles
/// it is based on: a heading where a style says its outline level
/// (`default-outline-level`) or is one of the built-in headings (`Heading 2`,
/// written `Heading_20_2`), the title where it is the built-in `Title`. A
/// paragraph LibreOffice imports as a heading is often a plain `text:p` in
/// an automatic style based on one.
fn style_roles(content: &Tree, styles: Option<&Tree>) -> std::collections::HashMap<String, Role> {
    use std::collections::HashMap;
    // name -> (parent, outline level, display name)
    let mut defs: HashMap<String, (Option<String>, Option<usize>, String)> = HashMap::new();
    for t in [Some(content), styles].into_iter().flatten() {
        for (i, n) in t.nodes.iter().enumerate() {
            if n.name != "style" || t.attr(i, "family") != Some("paragraph") {
                continue;
            }
            let Some(name) = t.attr(i, "name") else {
                continue;
            };
            defs.entry(name.to_string()).or_insert((
                t.attr(i, "parent-style-name").map(str::to_string),
                t.attr(i, "default-outline-level")
                    .and_then(|v| v.parse().ok()),
                t.attr(i, "display-name")
                    .unwrap_or(name)
                    .replace("_20_", " "),
            ));
        }
    }
    let own = |name: &str, def: &(Option<String>, Option<usize>, String)| -> Option<Role> {
        if let Some(l) = def.1.filter(|l| *l > 0) {
            return Some(Role::Heading(l.min(6)));
        }
        let shown = def.2.as_str();
        if shown == "Title" || name == "Title" {
            return Some(Role::Title);
        }
        shown
            .strip_prefix("Heading ")
            .and_then(|n| n.trim().parse::<usize>().ok())
            .filter(|l| (1..=10).contains(l))
            .map(|l| Role::Heading(l.min(6)))
    };
    let mut out = HashMap::new();
    for name in defs.keys() {
        let mut at = Some(name.clone());
        // A chain of styles based on styles, cut short where it loops.
        for _ in 0..32 {
            let Some(n) = at.take() else {
                break;
            };
            let Some(def) = defs.get(&n) else {
                break;
            };
            if let Some(role) = own(&n, def) {
                out.insert(name.clone(), role);
                break;
            }
            at = def.0.clone();
        }
    }
    out
}

/// A presentation: each slide a page; its title frame a level-1 heading,
/// the rest of its frames' text, lists and tables in order. Speaker notes
/// are not read.
fn slide_elements(content: &Tree, room: &mut Room) -> Vec<Element> {
    let mut out = Vec::new();
    let Some(pres) = content.find(0, "presentation") else {
        return out;
    };
    let pages: Vec<usize> = content
        .elements(pres)
        .filter(|&p| content.node(p).name == "page")
        .collect();
    for (page, p) in pages.into_iter().enumerate() {
        let mut asides = Vec::new();
        slide_shapes(content, p, page, &mut out, &mut asides, room);
        read_asides(content, asides, page, &mut out, room);
    }
    out
}

fn slide_shapes(
    t: &Tree,
    id: usize,
    page: usize,
    out: &mut Vec<Element>,
    asides: &mut Vec<Aside>,
    room: &mut Room,
) {
    for c in t.elements(id) {
        match t.node(c).name.as_str() {
            "notes" => {}
            "frame" | "custom-shape" | "rect" | "ellipse" | "text-box" => {
                let class = t.attr(c, "class").unwrap_or("");
                if class == "title" {
                    let text = tidy(
                        &t.elements(c)
                            .flat_map(|b| {
                                let mut v = Vec::new();
                                flow(t, b, page, false, &mut v, asides, room);
                                v.into_iter().map(|e| e.text)
                            })
                            .collect::<Vec<_>>()
                            .join(" "),
                    );
                    if !text.is_empty() {
                        out.push(element("doco:SectionTitle", text, Some(1), page));
                    }
                    continue;
                }
                let before = out.len();
                // A shape's own paragraphs (a custom shape holds them), or
                // the text box or table inside a frame.
                flow(t, c, page, false, out, asides, room);
                // A bulleted text box: its paragraphs are list items when
                // the frame is an outline.
                if class == "outline" {
                    for e in &mut out[before..] {
                        if e.kind == "doco:Paragraph" {
                            e.kind = "doc:ListItem".into();
                        }
                    }
                }
            }
            "g" => slide_shapes(t, c, page, out, asides, room),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
