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
//! **Bounds**, as for any archive: a part read whole (the content, the
//! styles, the metadata) is read within what is left of the document's
//! [`Budget`], checked as it inflates, and spent from it, at two bytes a
//! byte as an Office part is, and so is its tree as it is built
//! ([`Tree::parse`]): four bytes of XML (`<a/>`) are a node of about 140
//! bytes. Its XML is at most [`tree::MAX_NODES`] nodes nested
//! [`tree::MAX_DEPTH`] deep. What a document makes of it is spent from the
//! document's [`Budget`] before it is made: each paragraph and its text;
//! each table cell, a repeated cell each time it is copied, its text too,
//! and a short row's padding to the table's width; and the copies the
//! output makes of a table's cells (a merged value in each cell it covers,
//! each cell's labels). A document past it is refused.

mod sheet;
mod tree;

use fluree_doc_model::budget::{Budget, TooLarge, PART_BYTES};
use fluree_doc_model::{DocumentInfo, Element};
use fluree_doc_ooxml::Package;
use tree::{Role, Tree};

/// The most cells a spreadsheet's sheets may hold, repeats counted.
pub const MAX_CELLS: usize = 1 << 22;

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

impl From<TooLarge> for OdfError {
    fn from(e: TooLarge) -> Self {
        Self::TooLarge(e.to_string())
    }
}

/// Parse an OpenDocument file, whichever of the three kinds it is, with what
/// it declares about itself in `meta.xml`.
pub fn parse_with_info(bytes: &[u8]) -> Result<(Vec<Element>, DocumentInfo, Format), OdfError> {
    parse_within(bytes, &mut Budget::default())
}

/// [`parse_with_info`], against `budget`.
fn parse_within(
    bytes: &[u8],
    budget: &mut Budget,
) -> Result<(Vec<Element>, DocumentInfo, Format), OdfError> {
    let format = Format::sniff(bytes).ok_or(OdfError::NotOdf)?;
    let mut pkg = Package::open(bytes).map_err(OdfError::Zip)?;
    let content = tree(&mut pkg, "content.xml", budget)?.ok_or(OdfError::NoContent)?;
    let mut out = match format {
        Format::Text => {
            let styles = tree(&mut pkg, "styles.xml", budget)?;
            let mut content = content;
            content.roles = style_roles(&content, styles.as_ref());
            text_elements(&content, styles.as_ref(), budget)?
        }
        Format::Spreadsheet => sheet::elements(&content, budget)?,
        Format::Presentation => slide_elements(&content, budget)?,
    };
    let source = format.source();
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
        e.provenance = source;
        if e.evidence == "odf" {
            e.evidence = source;
        }
    }
    // What the package says of itself is not worth refusing it over.
    let info = tree(&mut pkg, "meta.xml", budget)
        .ok()
        .flatten()
        .map(|m| info(&m))
        .unwrap_or_default();
    Ok((out, info, format))
}

/// [`parse_with_info`], the elements alone.
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, OdfError> {
    parse_with_info(bytes).map(|(e, _, _)| e)
}

/// A part's XML as a tree, its text read within what is left of `budget`
/// and spent from it as an Office part is ([`Budget::spend_bytes`]), then
/// its tree ([`Tree::parse`]); `None` where the package has no such part.
/// The text is let go once the tree holds it.
fn tree(pkg: &mut Package<'_>, name: &str, budget: &mut Budget) -> Result<Option<Tree>, OdfError> {
    if !pkg.part_names().iter().any(|n| n == name) {
        return Ok(None);
    }
    let left = budget.memory_left() / PART_BYTES;
    let bytes = match pkg.read_bytes(name, left) {
        Ok(b) => b,
        // Past what the budget has left: spending it refuses, naming the
        // budget.
        Err(e) if e.contains("more than") => {
            let refused = budget.spend_bytes(left + 1).err();
            return Err(refused.map_or(OdfError::TooLarge(e), OdfError::from));
        }
        Err(e) => return Err(OdfError::TooLarge(e)),
    };
    budget.spend_bytes(bytes.len() as u64)?;
    let xml = match String::from_utf8(bytes) {
        Ok(xml) => xml,
        Err(e) => {
            // Read with each byte that is not UTF-8 replaced, a copy made
            // beside the bytes and up to three times as long (U+FFFD is
            // three bytes): spent at its length before it is made. A
            // `content.xml` of 1.2 GiB of `0xFF`, a megabyte zipped, made
            // 3.6 GB of replacements past the 2.4 its bytes were spent at.
            let bytes = e.as_bytes();
            let lossy = bytes.utf8_chunks().fold(0u64, |n, chunk| {
                let bad = if chunk.invalid().is_empty() { 0 } else { 3 };
                n.saturating_add(chunk.valid().len() as u64 + bad)
            });
            budget.spend_bytes(lossy)?;
            String::from_utf8_lossy(bytes).into_owned()
        }
    };
    Tree::parse(&xml, budget)
        .map(Some)
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
    para_text_into(t, id, asides, &mut s);
    s
}

/// [`para_text`], written at the end of `s`. Each element's text is written
/// where it goes, not gathered into a string of its own and copied into
/// its parent's: a run of text under 5,000 nested spans was copied once
/// for each.
fn para_text_into(t: &Tree, id: usize, asides: &mut Vec<Aside>, s: &mut String) {
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
                // Its citation and body are its own children: looked for
                // through the whole note, a note in a note in a note was
                // walked again at each.
                let label = child(t, c, "note-citation")
                    .map(|n| t.all_text(n).trim().to_string())
                    .unwrap_or_default();
                if let Some(body) = child(t, c, "note-body") {
                    asides.push(Aside {
                        evidence,
                        label,
                        body,
                    });
                }
            }
            "annotation" => {
                let label = child(t, c, "creator")
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
                para_text_into(t, c, asides, s);
                s.push(' ');
            }
            _ => para_text_into(t, c, asides, s),
        }
    }
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
/// Written as it is read: a list of a line's words is many times the line.
fn tidy(s: &str) -> String {
    let mut out = String::new();
    for line in s.lines() {
        let mut words = line.split_whitespace();
        let Some(first) = words.next() else {
            continue;
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(first);
        for w in words {
            out.push(' ');
            out.push_str(w);
        }
    }
    out
}

/// The element child of `id` named `name`.
fn child(t: &Tree, id: usize, name: &str) -> Option<usize> {
    t.elements(id).find(|&c| t.node(c).name == name)
}

/// Refuse the block `id` where what is left of `budget` could not hold its
/// text, before the copies it is made through (its runs joined, then
/// tidied) are made: each as long as the text under it, which one run of a
/// part can make a gigabyte. Checked, not spent: the text is spent once
/// made, tidied, and no longer than this.
fn room_for_text(t: &Tree, id: usize, budget: &Budget) -> Result<(), TooLarge> {
    budget.clone().spend_chars(t.text_chars(id))
}

/// [`room_for_text`] for a block [`para_text`] reads: its text, not a
/// note's, a comment's or a frame's, each read as a block of its own,
/// which checks its own; and a space for each element, the most it adds
/// (`text:s`, a tab, or a block in a cell set apart by two). Checked with
/// the text of the notes or frames it holds, nested in one another, the
/// text was walked again at each level.
///
/// A table cell's text is each of its children's [`para_text`]: `cell`
/// says `id` is one, whose children are read whole whatever they are.
fn room_for_para(t: &Tree, id: usize, cell: bool, budget: &Budget) -> Result<(), TooLarge> {
    // Each node with whether it is read whole.
    let mut stack = vec![(id, true)];
    let mut spaces = 0usize;
    let mut cost = budget.clone();
    while let Some((n, whole)) = stack.pop() {
        let node = t.node(n);
        if !whole && matches!(node.name.as_str(), "note" | "annotation" | "frame") {
            continue;
        }
        if !node.name.is_empty() {
            spaces += 2;
        }
        cost.spend_chars(node.text.chars())?;
        let whole = cell && n == id;
        stack.extend(node.children.iter().map(|&c| (c, whole)));
    }
    cost.spend_chars(std::iter::repeat_n(' ', spaces))
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
    budget: &mut Budget,
) -> Result<Option<Element>, TooLarge> {
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
            room_for_para(t, c, true, budget)?;
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
            // Each copy is a cell, and one that holds text a cell of the
            // output with that text: counted before any is made.
            cells += repeat;
            budget.spend_cells(repeat as u64)?;
            if !text.is_empty() {
                budget.spend_text_cells(repeat as u64)?;
                for _ in 0..repeat {
                    budget.spend_text(&text)?;
                }
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
        return Ok(None);
    }
    // The grid a short row is padded to: rows without cells and one long
    // row would otherwise make a grid of their product.
    let n = rows.len();
    let grid = n.saturating_mul(width);
    budget.spend_cells((grid - cells) as u64)?;
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
    budget.spend_elements(1)?;
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
    // What the output copies of it: a merged value in each cell it covers,
    // each cell's labels.
    budget.spend_copies(&e)?;
    Ok(Some(e))
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

/// A paragraph's element, its text spent before it is kept.
fn paragraph(
    kind: &str,
    text: String,
    level: Option<usize>,
    page: usize,
    budget: &mut Budget,
) -> Result<Element, TooLarge> {
    budget.spend_elements(1)?;
    budget.spend_text(&text)?;
    Ok(element(kind, text, level, page))
}

/// The deepest a list item is nested, as its `depth`: OpenDocument styles
/// ten levels.
const MAX_LIST_DEPTH: usize = 9;

/// The flow of a text body, a text box or a note: headings, paragraphs,
/// lists, tables, sections. `list` is the depth of the list it is in, if
/// it is in one: a paragraph there is a list item at that depth.
fn flow(
    t: &Tree,
    id: usize,
    page: usize,
    list: Option<usize>,
    out: &mut Vec<Element>,
    asides: &mut Vec<Aside>,
    budget: &mut Budget,
) -> Result<(), TooLarge> {
    // A paragraph's frames, read after it: a text box anchored to it.
    let frames_after = |c: usize,
                        out: &mut Vec<Element>,
                        asides: &mut Vec<Aside>,
                        budget: &mut Budget|
     -> Result<(), TooLarge> {
        let mut frames = Vec::new();
        frames_in(t, c, &mut frames);
        for f in frames {
            flow(t, f, page, None, out, asides, budget)?;
        }
        Ok(())
    };
    for c in t.elements(id) {
        match t.node(c).name.as_str() {
            "h" => {
                let level = t
                    .attr(c, "outline-level")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 6);
                room_for_para(t, c, false, budget)?;
                let text = tidy(&para_text(t, c, asides));
                if !text.is_empty() {
                    out.push(paragraph(
                        "doco:SectionTitle",
                        text,
                        Some(level),
                        page,
                        budget,
                    )?);
                }
                frames_after(c, out, asides, budget)?;
            }
            "p" => {
                room_for_para(t, c, false, budget)?;
                let text = tidy(&para_text(t, c, asides));
                if text.is_empty() {
                    frames_after(c, out, asides, budget)?;
                    continue;
                }
                let style = t.attr(c, "style-name");
                // A style named `Title` is the built-in one, defined or not.
                let role = style
                    .and_then(|s| t.roles.get(s))
                    .or((style == Some("Title")).then_some(&Role::Title));
                let (kind, level) = match role {
                    _ if list.is_some() => ("doc:ListItem", None),
                    Some(Role::Title) => ("doco:Title", None),
                    Some(Role::Heading(l)) => ("doco:SectionTitle", Some(*l)),
                    None => ("doco:Paragraph", None),
                };
                let mut e = paragraph(kind, text, level, page, budget)?;
                e.depth = list.unwrap_or(0);
                out.push(e);
                frames_after(c, out, asides, budget)?;
            }
            // A list in a list's item is a level deeper.
            "list" => {
                let depth = list.map_or(0, |d| d + 1).min(MAX_LIST_DEPTH);
                flow(t, c, page, Some(depth), out, asides, budget)?
            }
            "list-item" | "list-header" => {
                flow(t, c, page, Some(list.unwrap_or(0)), out, asides, budget)?
            }
            "table" => {
                if let Some(e) = table(t, c, page, asides, budget)? {
                    out.push(e);
                }
            }
            "section" | "index-body" | "table-of-content" | "alphabetical-index"
            | "illustration-index" | "table-index" | "bibliography" | "text-box" | "frame" => {
                flow(t, c, page, list, out, asides, budget)?
            }
            _ => {}
        }
    }
    Ok(())
}

/// Notes and comments, after what cited them: each a run of paragraphs
/// marked by its `evidence`, its first opening with its label.
fn read_asides(
    t: &Tree,
    asides: Vec<Aside>,
    page: usize,
    out: &mut Vec<Element>,
    budget: &mut Budget,
) -> Result<(), TooLarge> {
    let mut notes = Vec::new();
    let mut comments = Vec::new();
    for a in asides {
        let mut els = Vec::new();
        let mut nested = Vec::new();
        flow(t, a.body, page, None, &mut els, &mut nested, budget)?;
        for (i, e) in els.iter_mut().enumerate() {
            if e.kind != "doco:Table" {
                e.kind = "doco:Paragraph".into();
                e.level = None;
                if i == 0 && !a.label.is_empty() {
                    budget.spend_text(&a.label)?;
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
    Ok(())
}

/// A text document: the header, the body, its notes, the footer, then its
/// comments.
fn text_elements(
    content: &Tree,
    styles: Option<&Tree>,
    budget: &mut Budget,
) -> Result<Vec<Element>, TooLarge> {
    let mut body = Vec::new();
    let mut asides = Vec::new();
    if let Some(text) = content.find(0, "text") {
        flow(content, text, 0, None, &mut body, &mut asides, budget)?;
    }
    let (notes, comments): (Vec<Aside>, Vec<Aside>) =
        asides.into_iter().partition(|a| a.evidence != "comment");
    let mut after = Vec::new();
    read_asides(content, notes, 0, &mut after, budget)?;
    let mut furniture = |name: &str, evidence: &'static str| -> Result<Vec<Element>, TooLarge> {
        let mut out: Vec<Element> = Vec::new();
        // What a master page's header says that another's said already.
        let mut seen = std::collections::HashSet::new();
        let Some(styles) = styles else {
            return Ok(out);
        };
        let Some(masters) = styles.find(0, "master-styles") else {
            return Ok(out);
        };
        for m in styles.elements(masters) {
            for h in styles.elements(m).filter(|&h| styles.node(h).name == name) {
                let mut els = Vec::new();
                flow(styles, h, 0, None, &mut els, &mut Vec::new(), budget)?;
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
        Ok(out)
    };
    let mut out = furniture("header", "header")?;
    let footer = furniture("footer", "footer")?;
    out.extend(body);
    out.extend(after);
    out.extend(footer);
    read_asides(content, comments, 0, &mut out, budget)?;
    Ok(out)
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
fn slide_elements(content: &Tree, budget: &mut Budget) -> Result<Vec<Element>, TooLarge> {
    let mut out = Vec::new();
    let Some(pres) = content.find(0, "presentation") else {
        return Ok(out);
    };
    let pages: Vec<usize> = content
        .elements(pres)
        .filter(|&p| content.node(p).name == "page")
        .collect();
    for (page, p) in pages.into_iter().enumerate() {
        let mut asides = Vec::new();
        slide_shapes(content, p, page, &mut out, &mut asides, budget)?;
        read_asides(content, asides, page, &mut out, budget)?;
    }
    Ok(out)
}

fn slide_shapes(
    t: &Tree,
    id: usize,
    page: usize,
    out: &mut Vec<Element>,
    asides: &mut Vec<Aside>,
    budget: &mut Budget,
) -> Result<(), TooLarge> {
    for c in t.elements(id) {
        match t.node(c).name.as_str() {
            "notes" => {}
            "frame" | "custom-shape" | "rect" | "ellipse" | "text-box" => {
                let class = t.attr(c, "class").unwrap_or("");
                if class == "title" {
                    // Its paragraphs, each spent as it is read, joined.
                    let mut parts = Vec::new();
                    for b in t.elements(c) {
                        flow(t, b, page, None, &mut parts, asides, budget)?;
                    }
                    let text = tidy(
                        &parts
                            .into_iter()
                            .map(|e| e.text)
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
                flow(t, c, page, None, out, asides, budget)?;
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
            "g" => slide_shapes(t, c, page, out, asides, budget)?,
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
