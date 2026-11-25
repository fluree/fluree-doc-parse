//! Direct DoCO JSON-LD emission — no XHTML round-trip.
//!
//! The downstream document workflow historically consumed XHTML and walked
//! its tags back into a DoCO (Document Components Ontology) JSON-LD graph:
//! headings were serialized to `<h2>` only so a walker could re-derive the
//! section tree from tag names, and everything the tags could not carry —
//! page, bounding box, classification evidence — was destroyed in transit.
//! This emitter produces that graph directly from the element stream, same
//! shape the XHTML walker builds, with the spatial provenance kept:
//!
//! - `@graph` of elements in reading order; hierarchy lives only in
//!   `po:contains` (coerced to `@id` in the context so ingestion links
//!   rather than storing dead strings).
//! - A `foaf:Document` root containing `doco:BodyMatter`; headings open a
//!   `doco:Section` (with `doc:sectionLevel`) holding a `doco:SectionTitle`
//!   as its header, and content attaches to the innermost open section —
//!   the same stack walk the XHTML consumer performs on `h1`-`h6`. A title
//!   the file declares for itself is a `doco:Title`, the document's header.
//!   Headers are held by `po:containsAsHeader`, as DoCO specifies, and by
//!   `po:contains` too, so everything a section holds is one hop.
//! - Consecutive list items group under a `doco:List`, each a
//!   `doco:Paragraph` it contains: DoCO has no item class, and containment
//!   is what makes a block a list's member. Tables carry
//!   `doc:TableCell` children with row/column indices, header labels,
//!   and cell values. Cells whose text appears verbatim in the projection
//!   also carry `nif:beginIndex`/`nif:endIndex`, slicing it to their
//!   `doc:cellValue`, so a
//!   consumer can scope work to a table *row* instead of the whole table;
//!   a value synthesised by merge denormalisation has no place in the
//!   projection and carries no offsets rather than wrong ones.
//! - Text rides on `nif:isString`, with a display `rdfs:label` truncated to
//!   [`LABEL_MAX_CHARS`]; `nif:beginIndex`/`nif:endIndex` are character
//!   offsets into the plain-text projection ([`to_text`]) so entity mentions
//!   located in that projection join to their element by interval lookup.
//! - Every element additionally carries `doc:pageIndex` and `doc:bbox`
//!   (`"x0,y0,x1,y1"`, PDF units, top-left origin) — the fields the XHTML
//!   round-trip could not represent. `doc:pageIndex` is the 0-based physical
//!   position in the page sequence, deliberately not the printed page number:
//!   front matter, unnumbered inserts, and roman-numeral folios make the
//!   printed label a document-authored string that may be absent or repeat,
//!   while the physical index is always defined and always unique. For
//!   paginationless sources it is the slide or sheet index.

use crate::element::{Attachment, DocumentInfo, Element, Target};
use crate::message::{Mailbox, Message};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};

/// Display-label budget: full text lives in `nif:isString`, the label is a
/// preview for graph canvases.
const LABEL_MAX_CHARS: usize = 100;

#[derive(Default)]
pub struct DocoOptions {
    /// What every minted IRI starts with: `{base_iri}-section-{n}`,
    /// `{base_iri}-element-{n}`, one shared counter in emission order.
    ///
    /// Nothing minted adds a `/`, `#` or `:`, so every node of a document
    /// falls in the namespace its base IRI does. A store that keys
    /// namespaces on the part before the last of those — Fluree does —
    /// then keeps one namespace for a whole corpus, not one per document
    /// and kind. A base that ends in one of them is the caller choosing a
    /// namespace per document, and the `-` is left off.
    pub base_iri: String,
    /// When present, every node except a table cell is stamped
    /// `doc:sourceDocument → {iri}`, the tag a re-extraction retracts by. A
    /// cell is always one `po:contains` below its stamped table, and cells
    /// are most of a table-heavy graph.
    pub doc_iri: Option<String>,
    /// The document's running header and footer text, emitted once on the
    /// document node.
    ///
    /// Furniture is removed from the body because a line repeating on every
    /// page is noise *there*. It is not noise about the document: a
    /// controlled procedure puts its owner, title and number in exactly that
    /// block, and a three-page one repeats them on all three, so stripping
    /// the repetition strips the identity. Declared here rather than kept in
    /// the text, which would put it back into the body it was removed from
    /// and shift every character offset in the graph.
    pub running_text: Vec<String>,
    /// Pages carrying content nothing transcribed. Emitted on the document
    /// node as `doc:unreadPages`, so a consumer can tell an empty page from a
    /// page that was not read.
    pub unread: Vec<crate::element::UnreadPage>,
    /// Raster regions of otherwise read pages that nothing transcribed.
    /// Emitted on the document node as `doc:unreadRegions`, beside
    /// `doc:unreadPages` and for the same reason: a page of prose whose
    /// pasted screenshot nobody read looks read.
    pub unread_regions: Vec<crate::element::UnreadRegion>,
    /// What the parser reported while reading the pages. Emitted on the
    /// document node as `doc:parseWarnings`: an image that failed to decode
    /// is otherwise indistinguishable from one that held nothing.
    pub warnings: Vec<crate::element::ParseWarning>,
    /// Images placed in the flow whose text nothing read. Emitted on the
    /// document node as `doc:unreadImages`, so a pasted screenshot nothing
    /// read is not mistaken for a picture with no text in it.
    pub unread_images: Vec<crate::element::UnreadImage>,
    /// The OCR readings taken into the output, with the engine's own
    /// confidence. Emitted on the document node as `doc:ocrReadings`, so a
    /// caller can spot a reading that failed as a whole.
    pub ocr_readings: Vec<crate::element::OcrReading>,
    /// Page sizes, in PDF user units, for sources that have pages.
    ///
    /// Emitted on the document node as `doc:pages`. A `doc:bbox` cannot be
    /// placed on a rendered page without them: the consumer needs the ratio
    /// between the page's own units and the pixels it rendered to, and this
    /// is the only place the denominator appears. Empty for sources with no
    /// geometry.
    pub pages: Vec<crate::geom::PageSize>,
    /// What the document says about itself, emitted on the document node in
    /// Dublin Core terms.
    pub info: DocumentInfo,
    /// Files the document carries. Described on the document node as
    /// `doc:attachments`; their content is theirs to convert.
    pub attachments: Vec<Attachment>,
    /// The input's bytes as lowercase hex SHA-256, emitted on the document
    /// node as `doc:sha256`: the same file found twice, under any names, is
    /// one value.
    pub sha256: Option<String>,
    /// What the caller calls the input, usually its file name, emitted on
    /// the document node as `doc:sourceName`. Only the caller knows it: a
    /// parser is handed bytes.
    pub source_name: Option<String>,
}

/// The base IRI for a document whose caller named none:
/// `urn:fluree-doc-parse:` then the file's stem as a [`slug`] and the first
/// twelve hex digits of the file's SHA-256 (`report-661511bb2b30`), or the
/// digits alone where there is no name.
///
/// The stem alone is not enough: `report.pdf` and `report.docx` share one,
/// and so does every `report.docx` on a drive, and documents minted under
/// one base merge into one set of nodes in a store. The hash keeps them
/// apart; the stem keeps the IRI readable.
pub fn default_base_iri(stem: Option<&str>, sha256: &str) -> String {
    let digest = &sha256[..sha256.len().min(12)];
    match stem.map(slug).filter(|s| !s.is_empty()) {
        Some(s) => format!("urn:fluree-doc-parse:{s}-{digest}"),
        None => format!("urn:fluree-doc-parse:{digest}"),
    }
}

/// A name as it can sit inside an IRI: letters, digits, `.`, `_` and `~`
/// kept, any run of anything else one `-`, none at either end.
///
/// A file name's space is not allowed in an IRI at all, and its `#`, `/`,
/// `:` or `?` would move where a store splits the IRI into namespace and
/// name.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in name.chars() {
        if c.is_alphanumeric() || matches!(c, '.' | '_' | '~') {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

/// The plain-text projection: each text-bearing element's trimmed text in
/// reading order, separated by blank lines. `nif:beginIndex`/`nif:endIndex`
/// in [`to_doco`] index into exactly this string (in characters), so the two
/// outputs form a contract: locate a mention in the text, look up its
/// element by interval.
pub fn to_text(elements: &[Element]) -> String {
    let mut out = String::new();
    for p in projection(elements) {
        if !out.is_empty() {
            out.push_str(SEPARATOR);
        }
        out.push_str(&p.text);
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// What the projection puts between one element's text and the next's.
const SEPARATOR: &str = "\n\n";

/// One element's place in the text projection.
#[derive(Debug, Clone)]
pub struct Projected<'a> {
    /// The element's position in the slice walked.
    pub index: usize,
    pub element: &'a Element,
    /// Its text as the projection holds it: [`projection_text`].
    pub text: String,
    /// Where that text begins and ends in the projection, in characters.
    pub begin: usize,
    pub end: usize,
}

/// The walk the text projection is built by: each element that puts text
/// into it, in order, with where that text sits.
///
/// Elements with no text are skipped and take no separator; every other is
/// separated from the one before by [`SEPARATOR`]. [`to_text`] builds the
/// string by it, [`to_doco`] takes every `nif:beginIndex` from it, and a
/// reader mapping an offset back to its element walks it too — one walk, so
/// none of them can count a separator the others do not.
pub fn projection(elements: &[Element]) -> Projection<'_> {
    Projection {
        elements: elements.iter().enumerate(),
        cursor: 0,
        first: true,
    }
}

/// The iterator [`projection`] returns.
#[derive(Debug, Clone)]
pub struct Projection<'a> {
    elements: std::iter::Enumerate<std::slice::Iter<'a, Element>>,
    cursor: usize,
    first: bool,
}

impl<'a> Iterator for Projection<'a> {
    type Item = Projected<'a>;

    fn next(&mut self) -> Option<Projected<'a>> {
        for (index, element) in self.elements.by_ref() {
            let text = projection_text(element);
            if text.is_empty() {
                continue;
            }
            if !self.first {
                self.cursor += SEPARATOR.chars().count();
            }
            self.first = false;
            let begin = self.cursor;
            self.cursor += text.chars().count();
            return Some(Projected {
                index,
                element,
                text,
                begin,
                end: self.cursor,
            });
        }
        None
    }
}

/// An element's contribution to the text projection.
///
/// Re-deriving it from `Element::text` gets tables wrong — their
/// contribution is the cells joined with tabs — and offsets drift by exactly
/// the difference, silently. Anything mapping a projection offset back to an
/// element walks [`projection`], which yields this text with where it sits.
pub fn projection_text(e: &Element) -> String {
    match (projected_cells(e), e.kind.as_str()) {
        (Some(rows), "doco:Table") => table_projection(&rows).0,
        (None, "doco:Table") => strip_tags(&e.text).trim().to_string(),
        _ => e.text.trim().to_string(),
    }
}

/// A table's cells as the text projection shows them: a value merged down
/// over the rows beneath it is shown on each of them.
///
/// The projection is what a model reads, a line per row, and a row whose
/// merged cell is blank has lost what it is about: `\tG-2` under
/// `Grain\tG-1`, with nothing to say whose code it is. The cell entities already carry the value on every row it spans;
/// the projection now says the same. Merges across columns are not
/// repeated: a label over two columns names them both once.
pub fn projected_cells(e: &Element) -> Option<Vec<Vec<String>>> {
    let mut rows = e.cells.clone()?;
    let ncols = rows.first().map_or(0, Vec::len);
    let Some(down) = e
        .merged_down
        .as_ref()
        .filter(|m| ncols > 0 && m.len() == rows.len() * ncols)
    else {
        return Some(rows);
    };
    for r in 1..rows.len() {
        for c in 0..ncols {
            if down[r * ncols + c] && rows[r][c].trim().is_empty() && c < rows[r - 1].len() {
                rows[r][c] = rows[r - 1][c].clone();
            }
        }
    }
    Some(rows)
}

/// A table's projection text together with each non-empty cell's char span
/// within it.
///
/// One function builds both because they must agree exactly: the projection
/// is rows of trimmed cells tab-joined (row trailing whitespace dropped,
/// rows newline-joined, leading empty rows and trailing whitespace
/// dropped), and a span computed by any second walk drifts from it
/// silently. Spans are keyed `(row, column)` in
/// the RAW grid — the same indices the cell-emission loop iterates — and
/// exist only for cells whose trimmed text is non-empty, i.e. the cells
/// that actually appear in the projection.
pub fn table_projection(
    rows: &[Vec<String>],
) -> (String, BTreeMap<(usize, usize), (usize, usize)>) {
    let mut out = String::new();
    let mut cursor = 0usize; // chars, not bytes
    let mut spans: BTreeMap<(usize, usize), (usize, usize)> = BTreeMap::new();
    for (r, row) in rows.iter().enumerate() {
        if r > 0 {
            out.push('\n');
            cursor += 1;
        }
        let mut row_str = String::new();
        let mut row_cursor = 0usize;
        for (c, cell) in row.iter().enumerate() {
            if c > 0 {
                row_str.push('\t');
                row_cursor += 1;
            }
            let t = cell.trim();
            if !t.is_empty() {
                let len = t.chars().count();
                spans.insert((r, c), (cursor + row_cursor, cursor + row_cursor + len));
                row_cursor += len;
            }
            row_str.push_str(t);
        }
        // Trailing empty cells left only tabs; drop them. Spans are
        // unaffected — a non-empty cell's text is never trailing
        // whitespace.
        let trimmed_row = row_str.trim_end();
        out.push_str(trimmed_row);
        cursor += trimmed_row.chars().count();
    }
    // The final trim drops leading rows that were entirely empty, and
    // nothing else at the start: a first row whose first cell is empty
    // (the corner over a column of row labels) keeps its tab, so every
    // line of the table puts its fields at the same positions. Surviving
    // spans shift left by the trimmed char count.
    let start = out.trim_start_matches('\n');
    let start = if start.trim().is_empty() { "" } else { start };
    let lead = out.chars().count() - start.chars().count();
    let text = start.trim_end().to_string();
    if lead > 0 {
        let shifted = spans
            .into_iter()
            .map(|(k, (b, e))| (k, (b - lead, e - lead)))
            .collect();
        return (text, shifted);
    }
    (text, spans)
}

/// Each column's header path, as `doc:columnHeader` carries it.
///
/// One header row names the columns by itself, and its label is the
/// header, verbatim, and under each column it spans that has no label of
/// its own: a filer sets a period over two columns, its currency sign's and
/// its value's. A stacked header
/// says more: a banner spanning several
/// columns (`Price` over `Unit` and `Line`) is part of the name of every
/// column under it, and dropping it leaves two columns that differ from
/// the neighbouring ones only by a word. So under a stacked header each
/// column's name is its labels from the top down, joined with `" / "`
/// (`Price / Unit`). A label that reaches a column by a span is read from
/// the cell that holds it — `rows` is the denormalised grid, where a
/// horizontal span's text sits at its first column and a vertical span is
/// repeated into each row — and appears once however many rows or columns
/// it covers (`Item`, not `Item / Item`).
/// The width of one record in a table wrapped into side-by-side copies of
/// its columns, or `None` for a table whose rows are records.
///
/// A cross-reference list fills the page by wrapping: `F.P. DIESEL | MAHLE
/// CLEVITE` four times across, each pair a record of its own, so a value in
/// the fourth pair belongs to the key beside it, not to the row's first
/// cell. The header says so: the copies' value columns are named alike, and
/// each copy opens with a key column named apart from them. The key's name
/// may change from copy to copy as the list moves from one competitor's
/// numbers to the next, so only the value columns are matched.
///
/// Every label of a copy names a column: letters, and no figures. A table
/// with grouped columns repeats its names too, under a banner each: `2025 |
/// 2026` under `Median` and again under `Range`. Its column names are the
/// paths through the banners and differ, and where the banners were not
/// read its labels are years. A spec table whose first row of values was
/// taken for its header (`267MB/s | 275MB/s` across models) repeats values,
/// not names, and one value repeated across models names no key at all.
fn record_width(header: &[String]) -> Option<usize> {
    let n = header.len();
    let label = |c: usize| header[c].trim();
    let name =
        |t: &str| t.chars().any(char::is_alphabetic) && !t.chars().any(|c| c.is_ascii_digit());
    (2..=n / 2).filter(|&k| n.is_multiple_of(k)).find(|&k| {
        (0..n / k).all(|j| {
            let key = label(j * k);
            name(key)
                && (1..k).all(|i| {
                    let value = label(j * k + i);
                    name(value) && value == label(i) && value != key
                })
        })
    })
}

fn column_headers(
    rows: &[Vec<String>],
    n_header: usize,
    merged_left: Option<&[bool]>,
) -> Vec<String> {
    let ncols = rows.first().map(Vec::len).unwrap_or(0);
    if n_header == 0 {
        return Vec::new();
    }
    let continues_left = |r: usize, c: usize| {
        merged_left
            .filter(|m| m.len() == rows.len() * ncols)
            .is_some_and(|m| m[r * ncols + c])
    };
    // One header row: a column the row leaves empty, inside a label's span,
    // is under that label. A column with text of its own keeps it, since a
    // PDF's merged run can set its words across the columns it spans. The
    // owner is carried along the row, as below.
    if n_header == 1 {
        let mut owner = 0;
        let mut names = Vec::with_capacity(ncols);
        for c in 0..ncols {
            if c == 0
                || !continues_left(0, c)
                || !rows[0].get(c).is_some_and(|t| t.trim().is_empty())
            {
                owner = c;
            }
            names.push(
                rows[0]
                    .get(owner)
                    .map(|t| t.trim())
                    .unwrap_or("")
                    .to_string(),
            );
        }
        return names;
    }
    let mut paths: Vec<Vec<&str>> = vec![Vec::new(); ncols];
    for (r, row) in rows.iter().enumerate().take(n_header) {
        // The cell holding the label over column `c`, carried along the
        // row rather than walked back to from each column.
        let mut owner = 0;
        for (c, path) in paths.iter_mut().enumerate() {
            if c == 0 || !continues_left(r, c) {
                owner = c;
            }
            let label = row.get(owner).map(|t| t.trim()).unwrap_or("");
            if !label.is_empty() && !path.contains(&label) {
                path.push(label);
            }
        }
    }
    paths.into_iter().map(|path| path.join(" / ")).collect()
}

/// What the DoCO output copies of a table's cells, beyond the cells a
/// reader made: see [`table_copies`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Copies {
    /// Cells left empty by a merge that a copy fills, each a `doc:TableCell`
    /// node of its own: the nodes the output makes past the cells that hold
    /// text of their own.
    pub cells: u64,
    /// Text copied, at [`text_cost`](crate::budget::text_cost): a merged
    /// value repeated into each cell it covers, and the labels each cell
    /// node carries (`doc:columnHeader`, `doc:rowHeader`,
    /// `doc:sectionLabel`).
    pub text: u64,
    /// The part of `text` the merges copy, the labels left out: what the
    /// text projection copies too, a value merged down shown on each row
    /// ([`projected_cells`]), where DoCO copies the labels as well.
    pub merged: u64,
}

/// What [`to_doco`] copies of `e`'s cells, sized without making a copy.
///
/// A cell DoCO writes must describe itself, so a value merged down is
/// repeated into each row of its span (the fragments of a merged cell joined
/// first, `merges::denormalize`), the projection shows it on each row too
/// ([`projected_cells`]), and every cell node carries its column's header,
/// its row's first cell and its section's band. A file states none of these
/// copies: a few kilobytes of merges, or one long label over many rows, make
/// gigabytes of output. A reader spends them from the document's budget
/// ([`Budget::spend_copies`](crate::budget::Budget::spend_copies)) before
/// the table is kept.
///
/// This walks the table as the writer does, with each cell's text replaced
/// by its cost: the vertical runs, the horizontal runs and the full-width
/// bands of `denormalize`, then the labels of each cell that becomes a node.
/// A stacked header's path is counted at every label on it, the repeats
/// the writer drops included, so it is never under the writer's. A table
/// that carries on another (`continues`, a PDF's page break) takes that
/// one's header, which is not counted here.
pub fn table_copies(e: &Element) -> Copies {
    use crate::budget::text_cost;
    let Some(raw) = &e.cells else {
        return Copies::default();
    };
    let n = raw.len();
    let cols = raw.first().map_or(0, Vec::len);
    if n == 0 || cols == 0 {
        return Copies::default();
    }
    // Each cell's trimmed text by its cost; 0 is an empty cell.
    let own: Vec<u64> = raw
        .iter()
        .flat_map(|row| (0..cols).map(move |c| row.get(c).map_or(0, |t| text_cost(t.trim()))))
        .collect();
    // Texts joined by one space, as `denormalize` joins them.
    let joined = |parts: &mut dyn Iterator<Item = u64>| {
        let (sum, k) = parts
            .filter(|&p| p > 0)
            .fold((0u64, 0u64), |(s, k), p| (s.saturating_add(p), k + 1));
        sum.saturating_add(k.saturating_sub(1))
    };
    let mut text = 0u64;
    let mut d = own.clone();
    let subs = e.sub_header_rows(n);
    if e.merged_down.is_some() || e.merged_left.is_some() || e.sub_headers.is_some() {
        let flag = |m: &Option<Vec<bool>>, i: usize| {
            m.as_ref().and_then(|m| m.get(i)).copied().unwrap_or(false)
        };
        for c in 0..cols {
            let mut r = 0;
            while r < n {
                let mut end = r + 1;
                while end < n && flag(&e.merged_down, end * cols + c) {
                    end += 1;
                }
                if end > r + 1 {
                    let v = joined(&mut (r..end).map(|i| d[i * cols + c]));
                    for i in r..end {
                        text = text.saturating_add(v.saturating_sub(own[i * cols + c]));
                        d[i * cols + c] = v;
                    }
                }
                r = end;
            }
        }
        for r in 0..n {
            let row = r * cols;
            let mut c = 0;
            while c < cols {
                let mut end = c + 1;
                while end < cols && flag(&e.merged_left, row + end) {
                    end += 1;
                }
                if end > c + 1 {
                    // The text moves to the first cell; the spaces it is
                    // joined by are new.
                    let v = joined(&mut (c..end).map(|k| d[row + k]));
                    let parts = d[row + c..row + end].iter().sum::<u64>();
                    text = text.saturating_add(v.saturating_sub(parts));
                    d[row + c] = v;
                    d[row + c + 1..row + end].fill(0);
                }
                c = end;
            }
            if subs[r] {
                // The band is the whole row, cells past the first row's
                // width included.
                let past = raw[r].iter().skip(cols).map(|t| text_cost(t.trim()));
                let past = past.collect::<Vec<_>>();
                let v = joined(&mut d[row..row + cols].iter().chain(&past).copied());
                let parts = d[row..row + cols].iter().chain(&past).sum::<u64>();
                text = text.saturating_add(v.saturating_sub(parts));
                d[row] = v;
                d[row + 1..row + cols].fill(0);
            }
        }
    }
    let merged = text;
    // Each column's name as the writer gives it ([`column_headers`]), and
    // the record a wrapped table's cell belongs to ([`record_width`]): its
    // row label is the record's first cell, not the row's.
    let n_header = e.header_rows.unwrap_or(1).min(n);
    let names = header_names(e, raw, n_header, &subs);
    let header: Vec<u64> = names.iter().map(|h| text_cost(h)).collect();
    let width = record_width(&names).unwrap_or(cols).max(1);
    // A row restated partway down names the columns beneath it, each label
    // under the header's upper rows' (`K25W951 / By wt`): at most that
    // much, or the header's own name where the row leaves it empty.
    let slash = text_cost(" / ");
    let upper: Vec<u64> = if n_header > 1 {
        header_names(e, raw, n_header - 1, &subs)
            .iter()
            .map(|h| text_cost(h))
            .collect()
    } else {
        Vec::new()
    };
    let mut restated_rows = vec![false; n];
    for &s in e
        .restated_headers
        .iter()
        .flatten()
        .filter(|&&s| s >= n_header && s < n)
    {
        restated_rows[s] = true;
    }
    let mut restated: Option<usize> = None;
    let column_label = |c: usize, restated: Option<usize>| {
        let own = header.get(c).copied().unwrap_or(0);
        match restated.map(|s| d.get(s * cols + c).copied().unwrap_or(0)) {
            Some(label) if label > 0 => own.max(
                upper
                    .get(c)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(slash)
                    .saturating_add(label),
            ),
            _ => own,
        }
    };
    let flag = |m: &Option<Vec<bool>>, i: usize| {
        m.as_ref().and_then(|m| m.get(i)).copied().unwrap_or(false)
    };
    // A band no data row follows is a row of its own, written as one.
    let last_data = (0..n).rposition(|r| !subs[r]);
    // The nodes: each cell below the header, outside a band, that holds
    // text once the merges are read; each with its labels. The bands over a
    // row are joined from the top down, a group's over its sub-group's.
    let mut nodes = 0u64;
    let mut section = 0u64;
    for r in 0..n {
        if r < n_header {
            continue;
        }
        if subs[r] {
            let above = if r > n_header && subs[r - 1] {
                section
            } else {
                0
            };
            let own = d[r * cols];
            section = match (above, own) {
                (0, x) | (x, 0) => x,
                (a, x) => a.saturating_add(slash).saturating_add(x),
            };
            if last_data.is_some_and(|l| l > r) {
                continue;
            }
        }
        if restated_rows[r] {
            restated = Some(r);
            continue;
        }
        let section_here = if subs[r] { 0 } else { section };
        let cost = |c: usize| {
            if c < cols {
                d[r * cols + c]
            } else {
                raw[r].get(c).map_or(0, |t| text_cost(t.trim()))
            }
        };
        // A row longer than the first keeps its cells past it as they are.
        // A cell's row is named by the groups set down the columns before
        // it in its record and by the row's own label beside them: at most
        // every column from the record's first to the last one spanning
        // down, and the one after it, joined by ` / `.
        let spans_down = |k: usize| {
            (r > n_header && flag(&e.merged_down, r * cols + k))
                || (r + 1 < n && flag(&e.merged_down, (r + 1) * cols + k))
        };
        let mut record = usize::MAX;
        let mut last_group = 0;
        let (mut names, mut joins) = (0u64, 0u64);
        for c in 0..raw[r].len() {
            let first = c - c % width;
            if first != record {
                record = first;
                (names, joins) = (0, 0);
                last_group = first;
                while last_group + 1 < first + width && spans_down(last_group) {
                    last_group += 1;
                }
            }
            if c > first && c - 1 <= last_group {
                if c - 1 > first {
                    joins += 1;
                }
                names = names.saturating_add(cost(c - 1));
            }
            let value = cost(c);
            if value == 0 {
                continue;
            }
            nodes += 1;
            let row_header = if c > first {
                names.saturating_add(slash.saturating_mul(joins))
            } else {
                0
            };
            let labels = column_label(c, restated)
                .saturating_add(row_header)
                .saturating_add(section_here);
            text = text.saturating_add(labels);
        }
    }
    let filled = raw
        .iter()
        .flatten()
        .filter(|t| !t.trim().is_empty())
        .count() as u64;
    Copies {
        cells: nodes.saturating_sub(filled),
        text,
        merged,
    }
}

/// The column names [`to_doco`] gives a table of its own header: its
/// header rows, their merges read as the writer reads them (`subs`, each
/// row's band), named by [`column_headers`]. Only the header rows are
/// copied, and the rows a value merged down from them runs into.
fn header_names(e: &Element, raw: &[Vec<String>], n_header: usize, subs: &[bool]) -> Vec<String> {
    let Some(cols) = raw.first().map(Vec::len).filter(|_| n_header > 0) else {
        return Vec::new();
    };
    let flag = |m: &Option<Vec<bool>>, i: usize| m.as_ref().and_then(|m| m.get(i)) == Some(&true);
    // Past the header, to the first row no value runs down into: a run
    // ends above it, so the rows above read as in the whole table.
    let mut rows = n_header;
    while rows < raw.len() && (0..cols).any(|c| flag(&e.merged_down, rows * cols + c)) {
        rows += 1;
    }
    let k = rows * cols;
    let mut head = raw[..rows].to_vec();
    if e.merged_down.is_some() || e.merged_left.is_some() || e.sub_headers.is_some() {
        let first = |m: &Option<Vec<bool>>| (0..k).map(|i| flag(m, i)).collect();
        let m = crate::merges::Merges {
            continues_above: first(&e.merged_down),
            continues_left: first(&e.merged_left),
            full_width_row: subs[..rows].to_vec(),
        };
        crate::merges::denormalize(&mut head, &m);
    }
    let left = e
        .merged_left
        .as_deref()
        .filter(|m| m.len() == raw.len() * cols)
        .map(|m| &m[..k]);
    column_headers(&head, n_header, left)
}

/// Minimal tag stripper for model-arbitrated tables whose text is HTML.
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                if !out.ends_with(' ') && !out.is_empty() {
                    out.push(' ');
                }
            }
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Emitter<'a> {
    opts: &'a DocoOptions,
    nodes: Vec<Map<String, Value>>,
    /// node index → ordered child IRIs (kept separate so `po:contains` can be
    /// attached in one pass at the end).
    children: Vec<Vec<String>>,
    /// node index → the child IRI it holds as its header.
    headers: Vec<Option<String>>,
    counter: usize,
    /// (id-index of open section, its level)
    open_sections: Vec<(usize, usize)>,
    /// The `doc:Message` node the current email message's elements go into.
    open_message: Option<usize>,
    /// `doc:Message` nodes by the id of the element that opened them, for
    /// an element that goes back to one.
    messages: HashMap<String, usize>,
    /// The `doc:Signature` the current elements go into, with its text so
    /// far and where that starts and ends in the projection.
    open_signature: Option<OpenSignature>,
    /// Mailbox IRIs by address (lower-cased), or by name for a mailbox
    /// written without one.
    mailboxes: HashMap<String, String>,
    /// Each table emitted so far, by element id, for a part that carries it
    /// on across a page break.
    tables: HashMap<String, TablePart>,
    body_idx: usize,
}

/// What a table's continuation takes from it: the node it links to, the
/// names of its columns, its header rows with which of their cells continue
/// the one to the left, its last row read whole, and how many data rows it
/// and the parts before it hold.
struct TablePart {
    iri: String,
    header: Vec<String>,
    head: Vec<Vec<String>>,
    head_left: Vec<bool>,
    header_only: bool,
    last: Vec<String>,
    rows: usize,
}

impl<'a> Emitter<'a> {
    fn mint(&mut self, kind: &str) -> String {
        let base = &self.opts.base_iri;
        let sep = if base.is_empty() || base.ends_with(['/', '#', ':']) {
            ""
        } else {
            "-"
        };
        let iri = format!("{base}{sep}{kind}-{}", self.counter);
        self.counter += 1;
        iri
    }

    /// Create a node stamped with its source document, returning its index.
    fn node(&mut self, kind: &str, doco_type: &str, tag: Option<&str>) -> usize {
        let idx = self.unstamped(kind, doco_type, tag);
        if let Some(doc_iri) = &self.opts.doc_iri {
            self.nodes[idx].insert("doc:sourceDocument".into(), json!({ "@id": doc_iri }));
        }
        idx
    }

    /// Create a node without the `doc:sourceDocument` stamp, for a node
    /// always reached through a stamped parent.
    fn unstamped(&mut self, kind: &str, doco_type: &str, tag: Option<&str>) -> usize {
        let iri = self.mint(kind);
        let mut m = Map::new();
        m.insert("@id".into(), Value::String(iri));
        m.insert("@type".into(), Value::String(doco_type.into()));
        if let Some(tag) = tag {
            m.insert("doc:xhtmlTag".into(), Value::String(tag.into()));
        }
        self.nodes.push(m);
        self.children.push(Vec::new());
        self.headers.push(None);
        self.nodes.len() - 1
    }

    /// Attach `child` as `parent`'s header: DoCO's `po:containsAsHeader`,
    /// and `po:contains` too, so a query for everything a section holds
    /// needs no second property.
    fn attach_header(&mut self, parent: usize, child: usize) {
        self.headers[parent] = Some(self.nodes[child]["@id"].as_str().unwrap().to_string());
        self.attach(parent, child);
    }

    fn attach(&mut self, parent: usize, child: usize) {
        let child_iri = self.nodes[child]["@id"].as_str().unwrap().to_string();
        self.children[parent].push(child_iri);
    }

    /// The open signature, else the innermost open section, else the open
    /// message, else the body.
    fn current_parent(&self) -> usize {
        self.open_signature
            .as_ref()
            .map(|s| s.node)
            .or(self.open_sections.last().map(|(i, _)| *i))
            .or(self.open_message)
            .unwrap_or(self.body_idx)
    }

    /// Open a `doc:Signature` in the current message, signed by its sender.
    ///
    /// A node of its own, holding the signature's elements, so a consumer
    /// can tell a place named in a signature from a place the message talks
    /// about, and keep it on `doc:signer`: the address under a name is that
    /// sender's office, not the office of the company the thread discusses.
    fn open_signature(&mut self) {
        let parent = self.open_message.unwrap_or_else(|| self.current_parent());
        let idx = self.node("signature", "doc:Signature", None);
        if let Some(from) = self
            .open_message
            .and_then(|m| self.nodes[m].get("doc:from").cloned())
        {
            self.nodes[idx].insert("doc:signer".into(), from);
        }
        self.attach(parent, idx);
        self.open_signature = Some(OpenSignature {
            node: idx,
            text: String::new(),
            span: None,
        });
    }

    /// Close the open signature, giving it the span its elements cover.
    fn close_signature(&mut self) {
        let Some(sig) = self.open_signature.take() else {
            return;
        };
        if let Some((start, end)) = sig.span {
            self.set_text(sig.node, &sig.text);
            self.set_offsets(sig.node, start, end);
        }
    }

    /// Go on with the `doc:Message` the element `opener` opened.
    fn resume_message(&mut self, opener: &str) {
        if let Some(&idx) = self.messages.get(opener) {
            self.open_sections.clear();
            self.open_message = Some(idx);
        }
    }

    /// Mint a `doc:Mailbox` node for every sender and recipient in the
    /// document, one per address.
    ///
    /// A node rather than a string on the message, so that a person can be
    /// found by address: every message they sent or received in this
    /// document points at the same node, and `doc:address` is the value a
    /// contact record joins on. Minted per document, like every other node
    /// here, so re-extracting one document never touches another's. A
    /// mailbox written with a name alone, as a quoted Outlook header often
    /// is, joins the addressed mailbox of the same name when there is one.
    fn register_mailboxes(&mut self, elements: &[Element]) {
        let all: Vec<Mailbox> = elements
            .iter()
            .filter_map(|e| e.message.as_deref())
            .flat_map(|m| m.from.iter().chain(&m.to).chain(&m.cc).chain(&m.bcc))
            .cloned()
            .collect();
        let (addressed, named): (Vec<&Mailbox>, Vec<&Mailbox>) =
            all.iter().partition(|m| m.address.is_some());
        for m in addressed.into_iter().chain(named) {
            let key = mailbox_key(m);
            if key.is_empty() || self.mailboxes.contains_key(&key) {
                continue;
            }
            if m.address.is_none() {
                if let Some(iri) = m.name.as_deref().and_then(|n| self.mailbox_by_name(n)) {
                    self.mailboxes.insert(key, iri);
                    continue;
                }
            }
            let idx = self.node("mailbox", "doc:Mailbox", None);
            if let Some(a) = &m.address {
                self.nodes[idx].insert("doc:address".into(), Value::String(a.clone()));
            }
            if let Some(n) = &m.name {
                self.nodes[idx].insert("doc:name".into(), Value::String(n.clone()));
            }
            let iri = self.nodes[idx]["@id"].as_str().unwrap().to_string();
            self.mailboxes.insert(key, iri);
        }
    }

    fn mailbox_by_name(&self, name: &str) -> Option<String> {
        self.nodes
            .iter()
            .find(|n| {
                n["@type"] == "doc:Mailbox"
                    && n.get("doc:address").is_some()
                    && n.get("doc:name")
                        .and_then(Value::as_str)
                        .is_some_and(|v| v.eq_ignore_ascii_case(name))
            })
            .map(|n| n["@id"].as_str().unwrap().to_string())
    }

    /// Open a `doc:Message` for an element that starts one, and describe it.
    ///
    /// Messages sit side by side under the body, in reading order, rather
    /// than nested the way the quoting nests them: a thread is a sequence
    /// of messages, each by its own sender, and "everything Ada wrote" is
    /// then one step from each of them. Sections a message's own headings
    /// open stay inside it.
    fn open_message(&mut self, m: &Message, opener: &str) {
        self.open_sections.clear();
        let idx = self.node("message", "doc:Message", None);
        self.messages.insert(opener.to_string(), idx);
        for (key, boxes) in [
            ("doc:from", &m.from),
            ("doc:to", &m.to),
            ("doc:cc", &m.cc),
            ("doc:bcc", &m.bcc),
        ] {
            let iris: Vec<Value> = boxes
                .iter()
                .filter_map(|b| self.mailboxes.get(&mailbox_key(b)).cloned())
                .map(Value::String)
                .collect();
            if !iris.is_empty() {
                self.nodes[idx].insert(key.into(), Value::Array(iris));
            }
        }
        if let Some(d) = &m.date {
            if let Some(d) = date_time(d) {
                self.nodes[idx].insert("doc:sentAt".into(), d);
            }
        }
        if let Some(s) = &m.subject {
            self.nodes[idx].insert("doc:subject".into(), Value::String(s.clone()));
        }
        if let Some(id) = &m.message_id {
            self.nodes[idx].insert("doc:messageId".into(), Value::String(id.clone()));
        }
        for (key, ids) in [
            ("doc:inReplyTo", &m.in_reply_to),
            ("doc:references", &m.references),
        ] {
            if !ids.is_empty() {
                self.nodes[idx].insert(key.into(), json!(ids));
            }
        }
        if m.quoted {
            self.nodes[idx].insert("doc:quoted".into(), Value::Bool(true));
        }
        let body = self.body_idx;
        self.attach(body, idx);
        self.open_message = Some(idx);
    }

    /// Give a node its text: `text` is its slice of the projection, and
    /// `nif:isString` carries it verbatim, so it equals what the node's
    /// offsets cut from [`to_text`]. A table whose first cell is empty
    /// starts with a tab there, and trimming it would put the string one
    /// character off its own offsets. Only the display label is trimmed.
    fn set_text(&mut self, idx: usize, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        self.nodes[idx].insert("nif:isString".into(), Value::String(text.into()));
        let label: String = if trimmed.chars().count() > LABEL_MAX_CHARS {
            let mut l: String = trimmed.chars().take(LABEL_MAX_CHARS).collect();
            l.push('\u{2026}');
            l
        } else {
            trimmed.into()
        };
        self.nodes[idx].insert("rdfs:label".into(), Value::String(label));
    }

    fn set_provenance(&mut self, idx: usize, e: &Element) {
        self.nodes[idx].insert("doc:pageIndex".into(), json!(e.page));
        // Only sources with layout carry a box. Emitting zeros for the rest
        // would hand every consumer a real-looking position.
        if let Some(b) = e.bbox {
            self.nodes[idx].insert(
                "doc:bbox".into(),
                Value::String(format!("{:.2},{:.2},{:.2},{:.2}", b.x0, b.y0, b.x1, b.y1)),
            );
        }
        self.nodes[idx].insert("doc:evidence".into(), Value::String(e.evidence.into()));
        // A recording places an element in time rather than on a page.
        // Plain integers, so "what was said between minute 4 and 5" is a
        // numeric comparison in any query language.
        if let Some(t) = &e.turn {
            if let Some(s) = &t.speaker {
                self.nodes[idx].insert("doc:speaker".into(), Value::String(s.clone()));
            }
            self.nodes[idx].insert("doc:startMs".into(), json!(t.start_ms));
            self.nodes[idx].insert("doc:endMs".into(), json!(t.end_ms));
        }
    }

    fn set_offsets(&mut self, idx: usize, start: usize, end: usize) {
        self.nodes[idx].insert("nif:beginIndex".into(), json!(start));
        self.nodes[idx].insert("nif:endIndex".into(), json!(end));
    }

    /// Emit a `doc:Link` node per link on this element and reference them
    /// from it.
    ///
    /// A node rather than a property on the element, because an element can
    /// carry several links and each one has its own anchor: flattening them
    /// onto the element would leave a consumer with a set of targets and no
    /// way to tell which words point where.
    ///
    /// Anchor offsets land in the same space as every other offset in the
    /// graph — characters into the text projection — so the interval lookup
    /// that finds an entity mention finds a link anchor too. `text_start` is
    /// where this element's text begins in that projection; where the
    /// projection is not the element's own text (a table, whose cells are
    /// joined with tabs) it is `None`, and the link is emitted without
    /// offsets rather than with wrong ones.
    fn set_links(&mut self, idx: usize, e: &Element, text_start: Option<usize>) {
        let Some(links) = e.links.clone() else {
            return;
        };
        // The projection trims the element's text, so offsets into it have
        // to trim with it.
        let lead = e.text.chars().take_while(|c| c.is_whitespace()).count();
        // Where each character starts, and the text's end: an anchor is cut
        // by its offsets in one step, where skipping to each anchor is
        // quadratic in an element of many links (600,000 on one line).
        let starts: Vec<usize> = e
            .text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(e.text.len()))
            .collect();
        let at = |c: usize| starts[c.min(starts.len() - 1)];
        let mut iris = Vec::new();
        for l in &links {
            let node = self.node("link", "doc:Link", None);
            match &l.target {
                // A literal, not a node: every directory of every address
                // a corpus links to would otherwise become a namespace in
                // the store, and which ones is up to the documents.
                Target::Uri { uri } => {
                    self.nodes[node].insert(
                        "doc:linkTarget".into(),
                        json!({ "@value": uri, "@type": "xsd:anyURI" }),
                    );
                }
                Target::Page { page } => {
                    self.nodes[node].insert("doc:linkPage".into(), json!(page));
                }
            }
            if let (Some(start), Some((b, end))) = (text_start, l.span()) {
                if b >= lead {
                    self.set_offsets(node, start + b - lead, start + end - lead);
                    let anchor = e.text[at(b)..at(end.max(b))].to_string();
                    self.nodes[node].insert("nif:isString".into(), Value::String(anchor));
                }
            }
            iris.push(self.nodes[node]["@id"].as_str().unwrap().to_string());
        }
        if iris.is_empty() {
            return;
        }
        self.nodes[idx].insert(
            "doc:link".into(),
            Value::Array(iris.into_iter().map(Value::String).collect()),
        );
    }
}

/// A signature being emitted: its node, the projection text of its elements
/// so far, and the span that text covers.
struct OpenSignature {
    node: usize,
    text: String,
    span: Option<(usize, usize)>,
}

/// The key a mailbox is minted under: its address, case folded, or its name
/// where it has no address.
fn mailbox_key(m: &Mailbox) -> String {
    match (&m.address, &m.name) {
        (Some(a), _) => a.to_lowercase(),
        (None, Some(n)) => format!("name:{}", n.to_lowercase()),
        (None, None) => String::new(),
    }
}

/// An ISO 8601 timestamp as a typed literal, so a store compares it as a
/// time rather than a string. A bare date is an `xsd:date`. `None` for one
/// that is not a real date: typed, it would fail the whole insert.
fn date_time(v: &str) -> Option<Value> {
    let (ty, v) = crate::xsd_temporal(v).filter(|(ty, _)| *ty != "xsd:time")?;
    Some(json!({ "@value": v, "@type": ty }))
}

pub fn to_doco(elements: &[Element], opts: &DocoOptions) -> String {
    let mut em = Emitter {
        opts,
        nodes: Vec::new(),
        children: Vec::new(),
        headers: Vec::new(),
        counter: 0,
        open_sections: Vec::new(),
        open_message: None,
        messages: HashMap::new(),
        open_signature: None,
        mailboxes: HashMap::new(),
        tables: HashMap::new(),
        body_idx: 0,
    };

    // FOAF's document class: DoCO roots its own examples in a FaBiO
    // bibliographic class, which a spreadsheet or an email is not.
    let doc_idx = em.node("element", "foaf:Document", Some("html"));
    if let Some(h) = &opts.sha256 {
        em.nodes[doc_idx].insert("doc:sha256".into(), Value::String(h.clone()));
    }
    if let Some(n) = &opts.source_name {
        em.nodes[doc_idx].insert("doc:sourceName".into(), Value::String(n.clone()));
    }
    if !opts.running_text.is_empty() {
        em.nodes[doc_idx].insert(
            "doc:runningText".into(),
            json!({ "@type": "@json", "@value": opts.running_text }),
        );
    }
    if !opts.unread.is_empty() {
        em.nodes[doc_idx].insert(
            "doc:unreadPages".into(),
            json!({ "@type": "@json", "@value": opts.unread }),
        );
    }
    if !opts.warnings.is_empty() {
        em.nodes[doc_idx].insert(
            "doc:parseWarnings".into(),
            json!({ "@type": "@json", "@value": opts.warnings }),
        );
    }
    if !opts.unread_regions.is_empty() {
        em.nodes[doc_idx].insert(
            "doc:unreadRegions".into(),
            json!({ "@type": "@json", "@value": opts.unread_regions }),
        );
    }
    if !opts.unread_images.is_empty() {
        em.nodes[doc_idx].insert(
            "doc:unreadImages".into(),
            json!({ "@type": "@json", "@value": opts.unread_images }),
        );
    }
    if !opts.ocr_readings.is_empty() {
        // A JSON literal, as the unread pages are: facts about how the
        // document was read, not things a graph should grow edges to.
        em.nodes[doc_idx].insert(
            "doc:ocrReadings".into(),
            json!({ "@type": "@json", "@value": opts.ocr_readings }),
        );
    }
    if !opts.pages.is_empty() {
        // A JSON literal rather than a node per page: these are the page's
        // measurements, not things a graph should grow edges to.
        em.nodes[doc_idx].insert(
            "doc:pages".into(),
            json!({ "@type": "@json", "@value": opts.pages }),
        );
    }
    let info = &opts.info;
    if let Some(t) = &info.title {
        em.nodes[doc_idx].insert("dcterms:title".into(), Value::String(t.clone()));
    }
    if !info.creators.is_empty() {
        em.nodes[doc_idx].insert("dcterms:creator".into(), json!(info.creators));
    }
    for (key, value) in [
        ("dcterms:created", &info.created),
        ("dcterms:modified", &info.modified),
    ] {
        if let Some(v) = value {
            if let Some(v) = date_time(v) {
                em.nodes[doc_idx].insert(key.into(), v);
            }
        }
    }
    if !opts.attachments.is_empty() {
        em.nodes[doc_idx].insert(
            "doc:attachments".into(),
            json!({ "@type": "@json", "@value": opts.attachments }),
        );
    }
    let body_idx = em.node("element", "doco:BodyMatter", Some("body"));
    em.body_idx = body_idx;
    em.attach(doc_idx, body_idx);
    em.register_mailboxes(elements);

    // Where each element's text sits in the `to_text` projection.
    let mut spans: Vec<Option<(usize, usize)>> = vec![None; elements.len()];
    for p in projection(elements) {
        spans[p.index] = Some((p.begin, p.end));
    }

    let mut open_list: Option<usize> = None;
    let mut titled = false;
    for (e, &span) in elements.iter().zip(&spans) {
        if open_list.is_some() && e.kind != "doc:ListItem" {
            open_list = None;
        }
        if let Some(m) = &e.message {
            open_list = None;
            em.close_signature();
            em.open_message(m, &e.id);
        } else if let Some(opener) = &e.resumes {
            open_list = None;
            em.close_signature();
            em.resume_message(opener);
        }
        if e.signature != em.open_signature.is_some() {
            open_list = None;
            if e.signature {
                em.open_signature();
            } else {
                em.close_signature();
            }
        }
        match e.kind.as_str() {
            // The title the file declares for itself is the document's
            // header. A document has one; a second reads as a top-level
            // heading.
            "doco:Title" if !titled => {
                titled = true;
                let title = em.node("element", "doco:Title", Some("h1"));
                let text = projection_text(e);
                em.set_text(title, &text);
                em.set_provenance(title, e);
                if let Some((s, t)) = span {
                    em.set_offsets(title, s, t);
                }
                let start = span.map(|(s, _)| s);
                em.set_links(title, e, start);
                em.attach_header(doc_idx, title);
            }
            "doco:SectionTitle" | "doco:Title" => {
                let level = match e.kind.as_str() {
                    "doco:Title" => 1,
                    _ => e.level.unwrap_or(1).clamp(1, 6),
                };
                while em.open_sections.last().is_some_and(|(_, l)| *l >= level) {
                    em.open_sections.pop();
                }
                let parent = em.current_parent();
                let section = em.node("section", "doco:Section", None);
                em.nodes[section].insert("doc:sectionLevel".into(), json!(level));
                em.attach(parent, section);

                let title = em.node("element", "doco:SectionTitle", Some(&format!("h{level}")));
                let text = projection_text(e);
                em.set_text(title, &text);
                em.set_provenance(title, e);
                if let Some((s, t)) = span {
                    em.set_offsets(title, s, t);
                }
                let start = span.map(|(s, _)| s);
                em.set_links(title, e, start);
                em.attach_header(section, title);
                em.open_sections.push((section, level));
            }
            "doc:ListItem" => {
                let list = match open_list {
                    Some(l) => l,
                    None => {
                        let parent = em.current_parent();
                        let l = em.node("element", "doco:List", Some("ul"));
                        em.attach(parent, l);
                        open_list = Some(l);
                        l
                    }
                };
                // A list item is the block it holds, a paragraph; being
                // contained by the list is what makes it a member.
                let item = em.node("element", "doco:Paragraph", Some("li"));
                let text = projection_text(e);
                em.set_text(item, &text);
                em.set_provenance(item, e);
                if let Some((s, t)) = span {
                    em.set_offsets(item, s, t);
                }
                let start = span.map(|(s, _)| s);
                em.set_links(item, e, start);
                em.attach(list, item);
            }
            "doco:Table" => {
                let parent = em.current_parent();
                let table = em.node("table", "doco:Table", Some("table"));
                let text = projection_text(e);
                em.set_text(table, &text);
                em.set_provenance(table, e);
                if let Some((s, t)) = span {
                    em.set_offsets(table, s, t);
                }
                let table_start = span.map(|(s, _)| s);
                // A table's projection joins cells with tabs, so an anchor
                // offset into the element's own text indexes nothing here.
                em.set_links(table, e, None);
                em.attach(parent, table);
                // A part carried on from the page before is that table's
                // rows: it links to the part it carries on, its columns are
                // the ones that part's header names, and its rows number on
                // from that part's. Only where the columns line up.
                let ncols = e.cells.as_ref().and_then(|r| r.first()).map(Vec::len);
                let earlier = e
                    .continues
                    .as_ref()
                    .and_then(|id| em.tables.get(id))
                    .filter(|p| Some(p.last.len()) == ncols);
                let (header_before, last_before, rows_before, head_before) = match earlier {
                    Some(p) => {
                        em.nodes[table]
                            .insert("doc:continues".into(), Value::String(p.iri.clone()));
                        (
                            Some(p.header.clone()),
                            Some(p.last.clone()),
                            p.rows,
                            Some((p.head.clone(), p.head_left.clone(), p.header_only)),
                        )
                    }
                    None => (None, None, 0, None),
                };

                // Cell entities: data rows index from 0 below the measured
                // header (`header_rows` — absent means undetected, treated
                // as the presumed single header row). A column's name is its
                // header path (`column_headers`): the one header row's label,
                // or, under a stacked header, the banner over it and the
                // label under that. Empty cells are not materialised.
                if let Some(raw_rows) = &e.cells {
                    // Where each raw cell's text sits inside the table's
                    // projection block — the anchor for per-cell offsets.
                    let projected = projected_cells(e).unwrap_or_else(|| raw_rows.clone());
                    let cell_spans = table_projection(&projected).1;
                    // Each cell becomes an entity that must describe itself,
                    // so merged values are filled down here — a cell blanked
                    // by the rowspan convention would otherwise lose its row
                    // context entirely.
                    let mut rows = raw_rows.clone();
                    // A cell the page break cut: empty at the top of a part
                    // with no header of its own, and spanning down or
                    // labelling the row (its first cell, beside values),
                    // its value is above the break.
                    if let (Some(last), Some(0)) = (&last_before, e.header_rows) {
                        let n = last.len();
                        let spans = |c: usize| {
                            e.merged_down.as_ref().and_then(|m| m.get(n + c)) == Some(&true)
                        };
                        let labels = rows[0].iter().skip(1).any(|t| !t.trim().is_empty());
                        for (c, value) in last.iter().enumerate() {
                            if rows[0][c].trim().is_empty() && (spans(c) || (c == 0 && labels)) {
                                rows[0][c] = value.clone();
                            }
                        }
                    }
                    if e.merged_down.is_some() || e.merged_left.is_some() || e.sub_headers.is_some()
                    {
                        let ncols = rows.first().map(Vec::len).unwrap_or(0);
                        let m = crate::merges::Merges {
                            continues_above: e
                                .merged_down
                                .clone()
                                .unwrap_or_else(|| vec![false; rows.len() * ncols]),
                            continues_left: e
                                .merged_left
                                .clone()
                                .unwrap_or_else(|| vec![false; rows.len() * ncols]),
                            full_width_row: e.sub_header_rows(rows.len()),
                        };
                        crate::merges::denormalize(&mut rows, &m);
                    }
                    let rows = &rows;
                    let n_header = e.header_rows.unwrap_or(1).min(rows.len());
                    let ncols = rows.first().map(Vec::len).unwrap_or(0);
                    let own_left = e
                        .merged_left
                        .clone()
                        .filter(|m| m.len() == rows.len() * ncols)
                        .unwrap_or_else(|| vec![false; rows.len() * ncols]);
                    let own_head = (
                        rows[..n_header].to_vec(),
                        own_left[..n_header * ncols].to_vec(),
                    );
                    // The header block: this part's own, the earlier part's
                    // where this one has none, or both where the break fell
                    // inside it (the earlier part is nothing but header).
                    let (head, head_left, header) = match head_before {
                        Some((mut h, mut l, true)) if n_header > 0 => {
                            h.extend(own_head.0);
                            l.extend(own_head.1);
                            let header = column_headers(&h, h.len(), Some(&l));
                            (h, l, header)
                        }
                        Some((h, l, _)) if n_header == 0 => {
                            (h, l, header_before.unwrap_or_default())
                        }
                        _ => (
                            own_head.0,
                            own_head.1,
                            column_headers(rows, n_header, e.merged_left.as_deref()),
                        ),
                    };
                    let subs = e.sub_header_rows(rows.len());
                    // A row set like the header, partway down, names the
                    // columns of the rows beneath it in the header's place:
                    // its labels stand for the header's lowest row, under
                    // any rows above that one (`K25W951 / By wt`).
                    let restated = e.restated_headers.clone().unwrap_or_default();
                    let upper = if head.len() > 1 {
                        let n = head.len() - 1;
                        let left = head_left.get(..n * ncols).unwrap_or(&[]);
                        column_headers(&head[..n], n, Some(left))
                    } else {
                        Vec::new()
                    };
                    let restated_header = |s: usize| -> Vec<String> {
                        (0..ncols)
                            .map(|c| {
                                let label = rows[s].get(c).map_or("", |t| t.trim());
                                if label.is_empty() {
                                    return header.get(c).cloned().unwrap_or_default();
                                }
                                match upper.get(c).map(|u| u.trim()).filter(|u| !u.is_empty()) {
                                    Some(u) if u != label => format!("{u} / {label}"),
                                    _ => label.to_string(),
                                }
                            })
                            .collect()
                    };
                    let mut is_restated = vec![false; rows.len()];
                    for &s in restated
                        .iter()
                        .filter(|&&s| s >= n_header && s < rows.len())
                    {
                        is_restated[s] = true;
                    }
                    // A table wrapped into side-by-side copies of its columns
                    // holds one record per copy on each row.
                    let width = record_width(&header).unwrap_or(ncols).max(1);
                    // A band labels the rows beneath it: no data row follows
                    // the last band, or the band is a row of its own.
                    let last_data = (0..rows.len()).rposition(|r| !subs[r]);
                    let data_after = |s: usize| last_data.is_some_and(|d| d > s);
                    // Carried down the table rather than searched for at each
                    // row: the header restated last, and the bands over the
                    // rows below the last band, joined once per band.
                    let mut restated_now: Option<Vec<String>> = None;
                    let mut section_now: Option<String> = None;
                    // The run a value merged down makes in each column: for
                    // each cell, the first and last rows of the run it is
                    // in, below the header. Found once for the table, not
                    // walked again for each cell beside a group.
                    let merged_at = |r: usize, c: usize| {
                        e.merged_down
                            .as_ref()
                            .and_then(|m| m.get(r * ncols + c))
                            .copied()
                            .unwrap_or(false)
                    };
                    let mut run_top = vec![0usize; rows.len() * ncols];
                    let mut run_end = vec![0usize; rows.len() * ncols];
                    for c in 0..ncols {
                        for r in 0..rows.len() {
                            run_top[r * ncols + c] = if r > n_header && merged_at(r, c) {
                                run_top[(r - 1) * ncols + c]
                            } else {
                                r
                            };
                        }
                        for r in (0..rows.len()).rev() {
                            run_end[r * ncols + c] = if r + 1 < rows.len() && merged_at(r + 1, c) {
                                run_end[(r + 1) * ncols + c]
                            } else {
                                r
                            };
                        }
                    }
                    for (r, row) in rows.iter().enumerate().skip(n_header) {
                        // A sub-header band labels the rows beneath it; it is
                        // not itself data. Its text rides on those rows as
                        // `doc:sectionLabel` so every cell entity carries
                        // the section it belongs to. A band no data row
                        // follows labels nothing, and is a cell of its own:
                        // a table's closing notes.
                        if subs[r] {
                            // The bands over a row: the nearest above it,
                            // and each band set directly over that one, a
                            // group over its sub-group (`Power Management`
                            // over `Average power requirements (W)`), joined
                            // from the top down as a stacked header's labels
                            // are.
                            let mut joined = if r > n_header && subs[r - 1] {
                                section_now.take().unwrap_or_default()
                            } else {
                                String::new()
                            };
                            if let Some(label) =
                                rows[r].first().map(|t| t.trim()).filter(|t| !t.is_empty())
                            {
                                if !joined.is_empty() {
                                    joined.push_str(" / ");
                                }
                                joined.push_str(label);
                            }
                            section_now = Some(joined);
                            if data_after(r) {
                                continue;
                            }
                        }
                        if is_restated[r] {
                            restated_now = Some(restated_header(r));
                            continue;
                        }
                        let header = restated_now.as_ref().unwrap_or(&header);
                        let section = (!subs[r])
                            .then_some(section_now.as_deref())
                            .flatten()
                            .filter(|t| !t.is_empty());
                        // A group label set down the first column, a span
                        // over the rows it groups, beside each row's own
                        // label: the row is named by both, group first
                        // (`Vertical Angle / 55°`).
                        // The span lies in the body: a first cell carrying
                        // on the header's is no group.
                        let spans_down = |r: usize, c: usize| {
                            let at = |r: usize| {
                                e.merged_down
                                    .as_ref()
                                    .and_then(|m| m.get(r * ncols + c))
                                    .copied()
                                    .unwrap_or(false)
                            };
                            (r > n_header && at(r)) || (r + 1 < rows.len() && at(r + 1))
                        };
                        // The rows the cell at `r`, `c` spans: its first and
                        // last, as found for the whole table above.
                        let span = |r: usize, c: usize| {
                            if c < ncols {
                                return (run_top[r * ncols + c], run_end[r * ncols + c]);
                            }
                            let at = |r: usize| {
                                e.merged_down
                                    .as_ref()
                                    .and_then(|m| m.get(r * ncols + c))
                                    .copied()
                                    .unwrap_or(false)
                            };
                            let mut a = r;
                            while a > n_header && at(a) {
                                a -= 1;
                            }
                            let mut b = r;
                            while b + 1 < rows.len() && at(b + 1) {
                                b += 1;
                            }
                            (a, b)
                        };
                        // A row's own label, not a figure (`55°` is one,
                        // `158,571` is not, nor are two figures a slash
                        // parts, `0 / 0`, or a bound, `<5%`).
                        let figure = |t: &str| {
                            t.chars().any(|c| c.is_ascii_digit())
                                && t.chars().all(|c| {
                                    c.is_ascii_digit() || ",.$€£¥%()-+\u{2212}/<>≤≥± ".contains(c)
                                })
                        };
                        // The groups of the record a cell is in, found once
                        // for the record.
                        let mut record = usize::MAX;
                        let mut groups: Vec<(usize, &str)> = Vec::new();
                        let mut k = 0;
                        for (c, cell) in row.iter().enumerate() {
                            let first = c - c % width;
                            let group = row.get(first).map(|t| t.trim()).unwrap_or("");
                            // Groups over sub-groups, each set down a column
                            // of its own (`FP2 Backplane` over `Conventional
                            // type`): a leading column spanning down is a
                            // group, and so is each after it spanning fewer
                            // rows than the one before, a part of its group.
                            // A column spanning the group's rows exactly says
                            // more of the group (its colour, its volumes
                            // carried down), and is read as the row's own, as
                            // is the first column spanning nothing.
                            if first != record {
                                record = first;
                                groups.clear();
                                k = first;
                                let mut outer: Option<(usize, usize)> = None;
                                while k + 1 < first + width && spans_down(r, k) {
                                    let t = row.get(k).map(|t| t.trim()).unwrap_or("");
                                    let rows_spanned = span(r, k);
                                    if t.is_empty()
                                        || outer.is_some_and(|o| {
                                            rows_spanned.1 - rows_spanned.0 >= o.1 - o.0
                                        })
                                    {
                                        break;
                                    }
                                    if groups.last().map(|g| g.1) != Some(t) {
                                        groups.push((k, t));
                                    }
                                    outer = Some(rows_spanned);
                                    k += 1;
                                }
                            }
                            // The groups set left of column `c`.
                            let left_of = |c: usize| {
                                groups
                                    .iter()
                                    .filter(|(at, _)| *at < c)
                                    .map(|(_, t)| *t)
                                    .collect::<Vec<_>>()
                                    .join(" / ")
                            };
                            let own = row.get(k).map(|t| t.trim()).unwrap_or("");
                            let stacked = (c > k
                                && !groups.is_empty()
                                && !own.is_empty()
                                && !figure(own)
                                && groups.last().map(|g| g.1) != Some(own))
                            .then(|| format!("{} / {own}", left_of(k)));
                            // A cell among the groups is named by those left
                            // of it.
                            let row_header = stacked.unwrap_or_else(|| {
                                if groups.is_empty() {
                                    group.to_string()
                                } else {
                                    left_of(c)
                                }
                            });
                            let value = cell.trim();
                            if value.is_empty() {
                                continue;
                            }
                            let cell_idx = em.unstamped("cell", "doc:TableCell", None);
                            em.nodes[cell_idx]
                                .insert("doc:rowIndex".into(), json!(rows_before + r - n_header));
                            em.nodes[cell_idx].insert("doc:columnIndex".into(), json!(c));
                            if let Some(h) = header.get(c).filter(|h| !h.is_empty()) {
                                em.nodes[cell_idx]
                                    .insert("doc:columnHeader".into(), Value::String(h.clone()));
                            }
                            if let Some(s) = section {
                                em.nodes[cell_idx]
                                    .insert("doc:sectionLabel".into(), Value::String(s.into()));
                            }
                            if c > first && !row_header.is_empty() {
                                em.nodes[cell_idx]
                                    .insert("doc:rowHeader".into(), Value::String(row_header));
                            }
                            em.nodes[cell_idx]
                                .insert("doc:cellValue".into(), Value::String(value.into()));
                            // Whether the value is this cell's own: merge
                            // denormalisation can synthesise or rejoin
                            // values (fill-down copies, fragment joins).
                            let own = raw_rows.get(r).and_then(|row| row.get(c)).map(|c| c.trim())
                                == Some(value);
                            // Offsets only when the projection really shows
                            // this value at (r, c): the cell's own, or one
                            // merged down from above, which the projection
                            // repeats. A value copied otherwise has no span
                            // to point at. A cell's span slices the
                            // projection to its `doc:cellValue`, which is
                            // its text, so it carries no `nif:isString`
                            // repeating it.
                            let shown = projected
                                .get(r)
                                .and_then(|row| row.get(c))
                                .map(|c| c.trim())
                                == Some(value);
                            if let (true, Some(s), Some(&(b, t))) =
                                (shown, table_start, cell_spans.get(&(r, c)))
                            {
                                em.set_offsets(cell_idx, s + b, s + t);
                            }
                            // What the source stores under the text it
                            // shows, typed, where it declares a type.
                            if let Some(d) = own
                                .then(|| e.datums.as_ref()?.get(r)?.get(c)?.as_ref())
                                .flatten()
                            {
                                em.nodes[cell_idx].insert(
                                    "doc:cellDatum".into(),
                                    json!({ "@value": d.value, "@type": d.datatype }),
                                );
                            }
                            em.attach(table, cell_idx);
                        }
                    }
                    if let Some(last) = rows.last() {
                        let iri = em.nodes[table]["@id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string();
                        em.tables.insert(
                            e.id.clone(),
                            TablePart {
                                iri,
                                header,
                                head,
                                head_left,
                                header_only: e.header_only(),
                                last: last.clone(),
                                rows: rows_before + rows.len() - n_header,
                            },
                        );
                    }
                }
            }
            // A figure carries its fragments as an unordered set. The
            // sequence they were drawn in is not a reading of the chart --
            // pairing J&J's donut by that order attaches two of four regions
            // to the wrong percentage -- so `doc:figureContent` states what
            // is inside the region without asserting how the parts relate.
            // A consumer wanting the pairing needs the drawing, which is why
            // the bbox travels with it.
            "doco:Figure" => {
                let parent = em.current_parent();
                let f = em.node("element", "doco:Figure", Some("figure"));
                let text = projection_text(e);
                em.set_text(f, &text);
                em.set_provenance(f, e);
                if let Some((s, t)) = span {
                    em.set_offsets(f, s, t);
                }
                let start = span.map(|(s, _)| s);
                em.set_links(f, e, start);
                if let Some(id) = &e.figure {
                    em.nodes[f].insert("doc:figure".into(), Value::String(id.clone()));
                }
                em.attach(parent, f);
            }
            // doco:Paragraph and anything else text-bearing.
            _ => {
                let parent = em.current_parent();
                let p = em.node("element", &e.kind, Some("p"));
                let text = projection_text(e);
                em.set_text(p, &text);
                em.set_provenance(p, e);
                if let Some((s, t)) = span {
                    em.set_offsets(p, s, t);
                }
                let start = span.map(|(s, _)| s);
                em.set_links(p, e, start);
                em.attach(parent, p);
            }
        }
        if let (Some(sig), Some((start, end))) = (em.open_signature.as_mut(), span) {
            if !sig.text.is_empty() {
                sig.text.push_str("\n\n");
            }
            sig.text.push_str(&projection_text(e));
            sig.span = Some((sig.span.map_or(start, |s| s.0), end));
        }
    }
    em.close_signature();

    // Attach po:contains and build the graph.
    let Emitter {
        nodes,
        children,
        headers,
        ..
    } = em;
    let graph: Vec<Value> = nodes
        .into_iter()
        .zip(children)
        .zip(headers)
        .map(|((mut n, kids), header)| {
            if let Some(h) = header {
                n.insert("po:containsAsHeader".into(), Value::String(h));
            }
            if !kids.is_empty() {
                n.insert(
                    "po:contains".into(),
                    Value::Array(kids.into_iter().map(Value::String).collect()),
                );
            }
            Value::Object(n)
        })
        .collect();

    let doc = json!({
        "@context": {
            "doco": "http://purl.org/spar/doco/",
            "po": "http://www.essepuntato.it/2008/12/pattern#",
            "rdfs": "http://www.w3.org/2000/01/rdf-schema#",
            "nif": "http://persistence.uni-leipzig.org/nlp2rdf/ontologies/nif-core#",
            "doc": "https://ns.flur.ee/doc#",
            "dcterms": "http://purl.org/dc/terms/",
            "foaf": "http://xmlns.com/foaf/0.1/",
            "xsd": "http://www.w3.org/2001/XMLSchema#",
            // Containment edges must ingest as IRI references, not literals.
            "po:contains": { "@type": "@id" },
            "po:containsAsHeader": { "@type": "@id" },
            // A link's anchor is a node of the graph. Its target is an
            // `xsd:anyURI` literal on that node.
            "doc:link": { "@type": "@id" },
            // A message's sender and recipients, and a signature's signer,
            // are the graph's mailbox nodes.
            "doc:from": { "@type": "@id" },
            "doc:to": { "@type": "@id" },
            "doc:cc": { "@type": "@id" },
            "doc:bcc": { "@type": "@id" },
            "doc:signer": { "@type": "@id" },
            // A table carried on from the page before names the part it
            // carries on.
            "doc:continues": { "@type": "@id" },
        },
        "@graph": graph,
    });
    serde_json::to_string_pretty(&doc).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::BBox;

    fn el(kind: &str, text: &str, level: Option<usize>) -> Element {
        Element {
            bbox: Some(BBox {
                x0: 10.0,
                y0: 20.0,
                x1: 110.0,
                y1: 40.0,
            }),
            level,
            ..Element::new(kind, text, "rust", "layout")
        }
    }

    fn opts() -> DocoOptions {
        DocoOptions {
            base_iri: "urn:test".into(),
            doc_iri: Some("urn:test:doc".into()),
            pages: Vec::new(),
            unread: Vec::new(),
            running_text: Vec::new(),
            ..Default::default()
        }
    }

    fn graph(elements: &[Element]) -> Vec<Value> {
        let v: Value = serde_json::from_str(&to_doco(elements, &opts())).unwrap();
        v["@graph"].as_array().unwrap().clone()
    }

    fn find<'a>(g: &'a [Value], ty: &str) -> Vec<&'a Value> {
        g.iter().filter(|n| n["@type"] == ty).collect()
    }

    #[test]
    fn sections_nest_by_level_and_content_attaches_to_innermost() {
        let els = vec![
            el("doco:SectionTitle", "Chapter", Some(1)),
            el("doco:Paragraph", "In chapter.", None),
            el("doco:SectionTitle", "Part", Some(2)),
            el("doco:Paragraph", "In part.", None),
            el("doco:SectionTitle", "Next chapter", Some(1)),
            el("doco:Paragraph", "After.", None),
        ];
        let g = graph(&els);
        let sections = find(&g, "doco:Section");
        assert_eq!(sections.len(), 3);
        // Chapter contains its title, its paragraph, and the nested Part.
        let chapter = &sections[0];
        let kids = chapter["po:contains"].as_array().unwrap();
        assert_eq!(kids.len(), 3);
        // The level-2 section is inside the level-1, not the body.
        let part_id = sections[1]["@id"].as_str().unwrap();
        assert!(kids.iter().any(|k| k == part_id));
        // "Next chapter" closed both and attached to the body.
        let body = &find(&g, "doco:BodyMatter")[0];
        let body_kids = body["po:contains"].as_array().unwrap();
        let next_id = sections[2]["@id"].as_str().unwrap();
        assert!(body_kids.iter().any(|k| k == next_id));
        assert_eq!(body_kids.len(), 2, "chapter + next chapter");
    }

    #[test]
    fn a_section_holds_its_title_as_its_header() {
        let g = graph(&[
            el("doco:SectionTitle", "Revenue", Some(1)),
            el("doco:Paragraph", "Up.", None),
        ]);
        let section = &find(&g, "doco:Section")[0];
        let title = &find(&g, "doco:SectionTitle")[0];
        assert_eq!(section["po:containsAsHeader"], title["@id"]);
        // Still contained, so everything a section holds is one hop.
        assert!(section["po:contains"]
            .as_array()
            .unwrap()
            .contains(&title["@id"]));
    }

    #[test]
    fn the_documents_own_title_is_the_documents_header() {
        let g = graph(&[
            el("doco:Title", "Annual Report", None),
            el("doco:Paragraph", "Prepared for the board.", None),
            el("doco:SectionTitle", "Overview", Some(1)),
            el("doco:Title", "Appendix", None),
        ]);
        let doc = &find(&g, "foaf:Document")[0];
        let titles = find(&g, "doco:Title");
        assert_eq!(titles.len(), 1, "a document has one title");
        assert_eq!(titles[0]["nif:isString"], "Annual Report");
        assert_eq!(doc["po:containsAsHeader"], titles[0]["@id"]);
        // It opens no section: the paragraph under it is the body's.
        let body = &find(&g, "doco:BodyMatter")[0];
        let para = &find(&g, "doco:Paragraph")[0];
        assert!(body["po:contains"]
            .as_array()
            .unwrap()
            .contains(&para["@id"]));
        // A second title reads as a top-level heading.
        let sections = find(&g, "doco:Section");
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[1]["doc:sectionLevel"], json!(1));
    }

    #[test]
    fn offsets_index_into_the_text_projection() {
        let els = vec![
            el("doco:SectionTitle", "Title", Some(1)),
            el("doco:Paragraph", "Body text here.", None),
        ];
        let projection = to_text(&els);
        let g = graph(&els);
        for n in g.iter().filter(|n| n.get("nif:beginIndex").is_some()) {
            let s = n["nif:beginIndex"].as_u64().unwrap() as usize;
            let e = n["nif:endIndex"].as_u64().unwrap() as usize;
            let slice: String = projection.chars().skip(s).take(e - s).collect();
            assert_eq!(&slice, n["nif:isString"].as_str().unwrap());
        }
    }

    #[test]
    fn consecutive_list_items_group_under_one_list() {
        let els = vec![
            el("doc:ListItem", "one", None),
            el("doc:ListItem", "two", None),
            el("doco:Paragraph", "break", None),
            el("doc:ListItem", "three", None),
        ];
        let g = graph(&els);
        let lists = find(&g, "doco:List");
        assert_eq!(lists.len(), 2);
        assert_eq!(lists[0]["po:contains"].as_array().unwrap().len(), 2);
        assert_eq!(lists[1]["po:contains"].as_array().unwrap().len(), 1);
        // Each item is the paragraph it holds, a member by containment.
        let item = |id: &Value| g.iter().find(|n| &n["@id"] == id).unwrap();
        for id in lists[0]["po:contains"].as_array().unwrap() {
            assert_eq!(item(id)["@type"], "doco:Paragraph");
            assert_eq!(item(id)["doc:xhtmlTag"], "li");
        }
        assert!(g.iter().all(|n| n["@type"] != "doc:ListItem"));
    }

    #[test]
    fn table_cells_carry_headers_indices_and_values() {
        let mut t = el("doco:Table", String::new().as_str(), None);
        t.cells = Some(vec![
            vec!["".into(), "2023".into(), "2024".into()],
            vec!["Revenue".into(), "10".into(), "".into()],
        ]);
        let g = graph(&[t]);
        let cells = find(&g, "doc:TableCell");
        // "Revenue" (no column header), "10" (both headers); empty cell dropped.
        assert_eq!(cells.len(), 2);
        let ten = cells.iter().find(|c| c["doc:cellValue"] == "10").unwrap();
        assert_eq!(ten["doc:rowIndex"], json!(0));
        assert_eq!(ten["doc:columnIndex"], json!(1));
        assert_eq!(ten["doc:columnHeader"], "2023");
        assert_eq!(ten["doc:rowHeader"], "Revenue");
        // The table's projection text is row-major.
        let table = &find(&g, "doco:Table")[0];
        assert!(table["nif:isString"]
            .as_str()
            .unwrap()
            .contains("Revenue\t10"));
    }

    /// Cells of a one-header-row table, keyed by value.
    fn cells_of(rows: &[&[&str]]) -> Vec<Value> {
        let mut t = el("doco:Table", "", None);
        t.cells = Some(
            rows.iter()
                .map(|r| r.iter().map(|c| c.to_string()).collect())
                .collect(),
        );
        t.header_rows = Some(1);
        find(&graph(&[t]), "doc:TableCell")
            .into_iter()
            .cloned()
            .collect()
    }

    #[test]
    fn a_band_over_a_band_or_closing_the_table_reaches_a_cell() {
        // A group band over its sub-band over the rows they label, and a
        // notes band no data row follows.
        let mut t = el("doco:Table", "", None);
        t.cells = Some(
            [
                ["Model", "26TB", "24TB"],
                ["Power Management", "", ""],
                ["Average power requirements (W)", "", ""],
                ["Read/Write", "8.4", "8.0"],
                ["Idle", "5.8", "5.6"],
                ["Notes: helium drive.", "", ""],
            ]
            .iter()
            .map(|r| r.iter().map(|c| c.to_string()).collect())
            .collect(),
        );
        t.header_rows = Some(1);
        t.sub_headers = Some(vec![1, 2, 5]);
        let g = graph(&[t]);
        let cells = find(&g, "doc:TableCell");
        let idle = cells.iter().find(|c| c["doc:cellValue"] == "5.8").unwrap();
        assert_eq!(
            idle["doc:sectionLabel"],
            "Power Management / Average power requirements (W)"
        );
        let notes = cells
            .iter()
            .find(|c| c["doc:cellValue"] == "Notes: helium drive.")
            .expect("the closing band is a cell");
        assert_eq!(notes["doc:columnIndex"], json!(0));
        assert!(notes.get("doc:sectionLabel").is_none());
        // A band with data under it is no cell of its own.
        assert!(!cells
            .iter()
            .any(|c| c["doc:cellValue"] == "Power Management"));
    }

    fn row_header_of<'a>(cells: &'a [Value], value: &str) -> &'a Value {
        &cells.iter().find(|c| c["doc:cellValue"] == value).unwrap()["doc:rowHeader"]
    }

    #[test]
    fn a_table_wrapped_into_copies_of_its_columns_holds_a_record_per_copy() {
        // An interchange list wrapped four times across the page.
        let cells = cells_of(&[
            &[
                "F.P. DIESEL",
                "MAHLE CLEVITE",
                "F.P. DIESEL",
                "MAHLE CLEVITE",
                "FEL-PRO INC.",
                "MAHLE CLEVITE",
                "FEL-PRO INC.",
                "MAHLE CLEVITE",
            ],
            &[
                "FP-1804353",
                "213-1725",
                "FP3802711",
                "225-3523",
                "11853",
                "P18562",
                "55350",
                "J17829",
            ],
        ]);
        assert_eq!(row_header_of(&cells, "J17829"), "55350");
        assert_eq!(row_header_of(&cells, "225-3523"), "FP3802711");
        assert_eq!(row_header_of(&cells, "213-1725"), "FP-1804353");
        // A key opens its own record and has no row header.
        assert_eq!(row_header_of(&cells, "55350"), &Value::Null);
        // Three columns to a record.
        let cells = cells_of(&[
            &[
                "COMP NO.",
                "COMPETITOR",
                "MAHLE CLEVITE",
                "COMP NO.",
                "COMPETITOR",
                "MAHLE CLEVITE",
            ],
            &[
                "14079426",
                "GMC",
                "601-8027",
                "1811191-C3",
                "IHC",
                "211-2562",
            ],
        ]);
        assert_eq!(row_header_of(&cells, "IHC"), "1811191-C3");
        assert_eq!(row_header_of(&cells, "211-2562"), "1811191-C3");
    }

    #[test]
    fn columns_named_again_are_not_always_copies_of_a_record() {
        // Years repeated under banners that were not read: the row is one
        // record, labelled by its first cell.
        let cells = cells_of(&[
            &["2025", "2026", "Longer run", "2025", "2026", "Longer run"],
            &["1.6", "1.8", "1.8", "1.4–1.7", "1.7–2.1", "1.7–2.0"],
        ]);
        assert_eq!(row_header_of(&cells, "1.7–2.1"), "1.6");
        // One value repeated across models names no key.
        let cells = cells_of(&[
            &[
                "Non-operating temperature",
                "-40 to 70",
                "-40 to 70",
                "-40 to 70",
            ],
            &["Shock", "300G", "300G", "300G"],
        ]);
        assert_eq!(row_header_of(&cells, "300G"), "Shock");
        // A first row of values taken for a header, repeating across models.
        let cells = cells_of(&[
            &[
                "Data transfer rate",
                "267MB/s",
                "275MB/s",
                "267MB/s",
                "275MB/s",
                "267MB/s",
            ],
            &["Cache", "256MB", "512MB", "256MB", "512MB", "256MB"],
        ]);
        assert_eq!(row_header_of(&cells, "512MB"), "Cache");
    }

    #[test]
    fn table_cells_carry_offsets_into_the_projection() {
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec!["".into(), "2023".into(), "2024".into()],
            vec!["Revenue".into(), "10".into(), "".into()],
            vec!["Cost".into(), "7".into(), "8".into()],
        ]);
        // A paragraph before the table so cell offsets must include the
        // table's own start in the document projection, not just the
        // in-table position.
        let els = vec![el("doco:Paragraph", "Intro paragraph.", None), t];
        let projection = to_text(&els);
        let g = graph(&els);
        let cells = find(&g, "doc:TableCell");
        assert_eq!(cells.len(), 5, "five non-empty data cells");
        for cell in &cells {
            let s = cell["nif:beginIndex"].as_u64().unwrap() as usize;
            let e = cell["nif:endIndex"].as_u64().unwrap() as usize;
            let slice: String = projection.chars().skip(s).take(e - s).collect();
            assert_eq!(&slice, cell["doc:cellValue"].as_str().unwrap());
            assert!(cell.get("nif:isString").is_none(), "the value is the text");
        }
        // Same-row cells sit on one projection line: no newline between
        // the row header's end and its neighbour's start.
        let rev = cells
            .iter()
            .find(|c| c["doc:cellValue"] == "Revenue")
            .unwrap();
        let ten = cells.iter().find(|c| c["doc:cellValue"] == "10").unwrap();
        let between: String = {
            let from = rev["nif:endIndex"].as_u64().unwrap() as usize;
            let to = ten["nif:beginIndex"].as_u64().unwrap() as usize;
            projection.chars().skip(from).take(to - from).collect()
        };
        assert_eq!(between, "\t");
    }

    #[test]
    fn an_empty_corner_keeps_its_field_in_the_projection() {
        // A transposed table: the header row's first cell is empty, the
        // rows below are labelled in it.
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec!["".into(), "Paracetamol".into(), "Ibuprofen".into()],
            vec!["Code".into(), "MD-0152".into(), "MD-0140".into()],
        ]);
        t.header_rows = Some(1);
        let els = vec![el("doco:Paragraph", "Intro.", None), t];
        let projection = to_text(&els);
        assert_eq!(
            projection,
            "Intro.\n\n\tParacetamol\tIbuprofen\nCode\tMD-0152\tMD-0140\n"
        );
        let g = graph(&els);
        let table = &find(&g, "doco:Table")[0];
        assert_eq!(
            table["nif:beginIndex"],
            json!(8),
            "the table starts at its line"
        );
        // Its string is what its offsets cut, corner tab included.
        let (s, e) = (
            table["nif:beginIndex"].as_u64().unwrap() as usize,
            table["nif:endIndex"].as_u64().unwrap() as usize,
        );
        let slice: String = projection.chars().skip(s).take(e - s).collect();
        assert_eq!(table["nif:isString"].as_str().unwrap(), slice);
        assert_eq!(
            table["rdfs:label"],
            "Paracetamol\tIbuprofen\nCode\tMD-0152\tMD-0140"
        );
        for cell in find(&g, "doc:TableCell") {
            let s = cell["nif:beginIndex"].as_u64().unwrap() as usize;
            let e = cell["nif:endIndex"].as_u64().unwrap() as usize;
            let slice: String = projection.chars().skip(s).take(e - s).collect();
            assert_eq!(&slice, cell["doc:cellValue"].as_str().unwrap());
        }
        // Leading rows with nothing in them are still dropped.
        let (text, _) =
            table_projection(&[vec!["".into(), "".into()], vec!["".into(), "x".into()]]);
        assert_eq!(text, "\tx");
    }

    /// A table split by a page break: its first part with a header, the
    /// second carrying it on with a cell the break cut at its top.
    fn split_table() -> Vec<Element> {
        let mut a = el("doco:Table", "", None);
        a.id = "elem-00001".into();
        a.cells = Some(vec![
            vec!["Problem".into(), "Cause".into(), "Resolution".into()],
            vec![
                "Baseline noise".into(),
                "Dirty cell".into(),
                "Clean the cell".into(),
            ],
        ]);
        a.header_rows = Some(1);
        let mut b = el("doco:Table", "", None);
        b.id = "elem-00002".into();
        b.page = 1;
        b.cells = Some(vec![
            vec!["".into(), "Dirty system".into(), "Clean the system".into()],
            vec![
                "".into(),
                "Detector needs purging".into(),
                "Purge it".into(),
            ],
            vec![
                "Broadening".into(),
                "Old column".into(),
                "Replace it".into(),
            ],
        ]);
        b.header_rows = Some(0);
        b.continues = Some("elem-00001".into());
        let mut md = vec![false; 9];
        md[3] = true; // the cut cell spans down into the second row
        b.merged_down = Some(md);
        vec![a, b]
    }

    #[test]
    fn a_continued_table_names_its_columns_and_rows_as_one_table() {
        let els = split_table();
        let projection = to_text(&els);
        let g = graph(&els);
        let tables = find(&g, "doco:Table");
        assert_eq!(tables[1]["doc:continues"], tables[0]["@id"]);
        let row_of = |table: &Value, value: &str| -> Value {
            let ids = table["po:contains"].as_array().unwrap();
            g.iter()
                .find(|n| ids.contains(&n["@id"]) && n["doc:cellValue"] == value)
                .cloned()
                .unwrap()
        };
        let system = row_of(tables[1], "Dirty system");
        assert_eq!(system["doc:columnHeader"], "Cause");
        assert_eq!(
            system["doc:rowIndex"],
            json!(1),
            "rows number on from the first part"
        );
        assert_eq!(
            system["doc:rowHeader"], "Baseline noise",
            "the cut cell is above the break"
        );
        // A value beyond the cause is named by the problem spanning its
        // rows and its own cause, group first.
        assert_eq!(
            row_of(tables[1], "Purge it")["doc:rowHeader"],
            "Baseline noise / Detector needs purging"
        );
        assert_eq!(
            row_of(tables[1], "Clean the system")["doc:rowHeader"],
            "Baseline noise / Dirty system"
        );
        assert_eq!(
            row_of(tables[1], "Replace it")["doc:rowHeader"],
            "Broadening"
        );
        // The carried value is a copy: no offsets, and every offset the
        // parts do carry still cuts its own text.
        let carried = row_of(tables[1], "Baseline noise");
        assert!(carried.get("nif:beginIndex").is_none());
        for n in g.iter().filter(|n| n.get("nif:beginIndex").is_some()) {
            let s = n["nif:beginIndex"].as_u64().unwrap() as usize;
            let e = n["nif:endIndex"].as_u64().unwrap() as usize;
            let slice: String = projection.chars().skip(s).take(e - s).collect();
            let want = n.get("nif:isString").or(n.get("doc:cellValue")).unwrap();
            assert_eq!(&slice, want.as_str().unwrap());
        }
    }

    #[test]
    fn a_row_restating_the_header_names_the_rows_under_it() {
        let mut t = el("doco:Table", "", None);
        t.id = "elem-00001".into();
        t.cells = Some(vec![
            vec!["".into(), "K25W951".into(), "".into()],
            vec!["".into(), "LB/Gal".into(), "g/L".into()],
            vec!["Coating Density".into(), "11.95".into(), "1432".into()],
            vec!["".into(), "By wt".into(), "By vol".into()],
            vec!["Total Volatiles".into(), "39.0%".into(), "55.7%".into()],
            vec!["Water".into(), "39.0%".into(), "55.6%".into()],
        ]);
        t.header_rows = Some(2);
        let mut ml = vec![false; 18];
        ml[2] = true; // `K25W951` spans both columns
        t.merged_left = Some(ml);
        t.restated_headers = Some(vec![3]);
        let g = graph(&[t]);
        let cell = |value: &str| -> Vec<Value> {
            g.iter()
                .filter(|n| n["doc:cellValue"] == value)
                .cloned()
                .collect()
        };
        assert_eq!(cell("11.95")[0]["doc:columnHeader"], "K25W951 / LB/Gal");
        assert_eq!(cell("55.7%")[0]["doc:columnHeader"], "K25W951 / By vol");
        assert!(
            cell("39.0%")
                .iter()
                .all(|c| c["doc:columnHeader"] == "K25W951 / By wt"),
            "every row under it until the next"
        );
        assert!(cell("By wt").is_empty(), "the row itself is not data");
    }

    #[test]
    fn a_label_cut_by_the_break_is_carried_beside_its_values() {
        // The break cuts a row: its label stays above, the rest of its
        // values open the next page beside an empty first cell that spans
        // nothing.
        let mut els = split_table();
        els[1].merged_down = None;
        els[1].cells = Some(vec![
            vec!["".into(), "Dirty system".into(), "Clean the system".into()],
            vec![
                "Broadening".into(),
                "Old column".into(),
                "Replace it".into(),
            ],
        ]);
        let g = graph(&els);
        let system = g
            .iter()
            .find(|n| n["doc:cellValue"] == "Dirty system")
            .unwrap();
        assert_eq!(system["doc:rowHeader"], "Baseline noise");
        let old = g
            .iter()
            .find(|n| n["doc:cellValue"] == "Old column")
            .unwrap();
        assert_eq!(old["doc:rowHeader"], "Broadening");
    }

    #[test]
    fn a_header_split_by_the_break_names_columns_by_both_parts() {
        let mut a = el("doco:Table", "", None);
        a.id = "elem-00001".into();
        a.cells = Some(vec![
            vec!["Reaction".into(), "Drug".into(), "".into()],
            vec!["".into(), "*".into(), "".into()],
        ]);
        a.header_rows = Some(1);
        let mut left = vec![false; 6];
        left[2] = true; // `Drug` spans both of its grade columns
        a.merged_left = Some(left);
        let mut b = el("doco:Table", "", None);
        b.id = "elem-00002".into();
        b.page = 1;
        b.cells = Some(vec![
            vec!["".into(), "All grades".into(), "Grades 3-4".into()],
            vec!["Diarrhea".into(), "56".into(), "11".into()],
        ]);
        b.header_rows = Some(1);
        b.continues = Some("elem-00001".into());
        let g = graph(&[a, b]);
        let eleven = g.iter().find(|n| n["doc:cellValue"] == "11").unwrap();
        assert_eq!(eleven["doc:columnHeader"], "Drug / Grades 3-4");
        assert_eq!(eleven["doc:rowHeader"], "Diarrhea");
    }

    #[test]
    fn a_value_beside_a_group_label_is_no_part_of_the_rows_name() {
        // A property wrapped down the rows of its two test methods, each row
        // with its value: `0 / 0` and `<5%` are figures, not labels that
        // tell the rows apart. A label beside it still does (`Field`).
        let mut t = el("doco:Table", "", None);
        t.cells = Some(
            [
                ["Property", "Value", "Test"],
                ["Surface burning", "0 / 0", "Method A"],
                ["", "<5%", "Method B"],
                ["Hardness", "Field", "Method C"],
                ["", "Edge", "Method D"],
            ]
            .iter()
            .map(|r| r.iter().map(|c| c.to_string()).collect())
            .collect(),
        );
        t.header_rows = Some(1);
        let mut down = vec![false; 15];
        down[6] = true;
        down[12] = true;
        t.merged_down = Some(down);
        let g = graph(&[t]);
        let cells = find(&g, "doc:TableCell");
        let header = |v: &str| {
            cells.iter().find(|c| c["doc:cellValue"] == v).unwrap()["doc:rowHeader"].clone()
        };
        assert_eq!(header("Method A"), "Surface burning");
        assert_eq!(header("Method B"), "Surface burning");
        assert_eq!(header("Method D"), "Hardness / Edge");
    }

    #[test]
    fn a_value_merged_down_is_on_every_row_of_the_text() {
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec!["H1".into(), "H2".into()],
            vec!["Alpha".into(), "1".into()],
            vec!["".into(), "2".into()],
        ]);
        // Row 2's first cell continues Alpha from the row above.
        t.merged_down = Some(vec![false, false, false, false, true, false]);
        let text = to_text(std::slice::from_ref(&t));
        assert_eq!(text.trim(), "H1\tH2\nAlpha\t1\nAlpha\t2");
        let g = graph(&[t]);
        let cells = find(&g, "doc:TableCell");
        let filled = cells
            .iter()
            .find(|c| c["doc:rowIndex"] == json!(1) && c["doc:columnIndex"] == json!(0))
            .expect("filled-down cell");
        assert_eq!(filled["doc:cellValue"], "Alpha");
        // Its offsets cut its own copy out of the projection, on its row.
        let chars: Vec<char> = text.chars().collect();
        let (b, e) = (
            filled["nif:beginIndex"].as_u64().unwrap() as usize,
            filled["nif:endIndex"].as_u64().unwrap() as usize,
        );
        assert_eq!(chars[b..e].iter().collect::<String>(), "Alpha");
        assert!(chars[..b].iter().filter(|c| **c == '\n').count() == 2);
    }

    #[test]
    fn a_row_under_a_group_and_its_sub_group_is_named_by_all_three() {
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec![
                "Product".into(),
                "Type".into(),
                "Specifications".into(),
                "Part No.".into(),
            ],
            vec![
                "Backplane".into(),
                "Conventional".into(),
                "5-module".into(),
                "BP05".into(),
            ],
            vec!["".into(), "".into(), "7-module".into(), "BP07".into()],
            vec!["".into(), "H type".into(), "8 slots".into(), "BP11".into()],
            vec![
                "".into(),
                "".into(),
                "8 slots, expansion".into(),
                "BP10".into(),
            ],
        ]);
        // The product spans its four rows, each type its two.
        let mut md = vec![false; 20];
        md[8] = true;
        md[9] = true;
        md[12] = true;
        md[16] = true;
        md[17] = true;
        t.merged_down = Some(md);
        t.header_rows = Some(1);
        let g = graph(&[t]);
        let cells = find(&g, "doc:TableCell");
        let row_of = |value: &str| {
            cells.iter().find(|c| c["doc:cellValue"] == value).unwrap()["doc:rowHeader"].clone()
        };
        assert_eq!(row_of("BP07"), "Backplane / Conventional / 7-module");
        assert_eq!(row_of("BP11"), "Backplane / H type / 8 slots");
        assert_eq!(row_of("BP10"), "Backplane / H type / 8 slots, expansion");
        // A cell among the groups is named by those left of it.
        assert_eq!(row_of("7-module"), "Backplane / Conventional");
        assert_eq!(row_of("H type"), "Backplane");
    }

    #[test]
    fn a_column_spanning_the_groups_rows_says_more_of_the_group() {
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec![
                "Tip".into(),
                "Colour".into(),
                "Max. volume".into(),
                "Volume".into(),
                "Error".into(),
            ],
            vec![
                "0.1 mL".into(),
                "white".into(),
                "100 µL".into(),
                "10 µL".into(),
                "±1.6 %".into(),
            ],
            vec![
                "".into(),
                "".into(),
                "".into(),
                "50 µL".into(),
                "±0.8 %".into(),
            ],
        ]);
        // The tip, its colour and its volume all span the group's two rows.
        let mut md = vec![false; 15];
        md[10] = true;
        md[11] = true;
        md[12] = true;
        t.merged_down = Some(md);
        t.header_rows = Some(1);
        let g = graph(&[t]);
        let cells = find(&g, "doc:TableCell");
        let error = cells
            .iter()
            .find(|c| c["doc:cellValue"] == "±0.8 %")
            .unwrap();
        assert_eq!(error["doc:rowHeader"], "0.1 mL / white");
    }

    #[test]
    fn a_cell_carries_its_stored_value_typed_beside_its_text() {
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec!["Region".into(), "Share".into()],
            vec!["North".into(), "12%".into()],
            vec!["".into(), "7%".into()],
        ]);
        let dec = |v: &str| Some(crate::Datum::new("xsd:decimal", v));
        t.datums = Some(vec![
            vec![None, None],
            vec![None, dec("0.12345")],
            vec![None, dec("0.0701")],
        ]);
        // Row 2's region continues North from the row above.
        t.merged_down = Some(vec![false, false, false, false, true, false]);
        let g = graph(&[t]);
        let cells = find(&g, "doc:TableCell");
        let cell = |row: u64, col: u64| {
            cells
                .iter()
                .find(|c| c["doc:rowIndex"] == json!(row) && c["doc:columnIndex"] == json!(col))
                .unwrap()
        };
        assert_eq!(cell(0, 1)["doc:cellValue"], "12%");
        assert_eq!(
            cell(0, 1)["doc:cellDatum"],
            json!({ "@value": "0.12345", "@type": "xsd:decimal" })
        );
        assert_eq!(cell(1, 1)["doc:cellDatum"]["@value"], "0.0701");
        assert!(cell(0, 0).get("doc:cellDatum").is_none(), "text is text");
        assert_eq!(cell(1, 0)["doc:cellValue"], "North");
        assert!(cell(1, 0).get("doc:cellDatum").is_none());
    }

    #[test]
    fn a_stacked_header_names_each_column_by_its_path() {
        // Item and Qty span both header rows; Price spans Unit and Line.
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec!["Item".into(), "Qty".into(), "Price".into(), "".into()],
            vec!["".into(), "".into(), "Unit".into(), "Line".into()],
            vec![
                "Fish pie".into(),
                "2".into(),
                "14.50".into(),
                "29.00".into(),
            ],
        ]);
        t.header_rows = Some(2);
        let mut ml = vec![false; 12];
        ml[3] = true;
        let mut md = vec![false; 12];
        md[4] = true;
        md[5] = true;
        t.merged_left = Some(ml);
        t.merged_down = Some(md);
        let g = graph(&[t]);
        let cells = find(&g, "doc:TableCell");
        let header = |v: &str| {
            cells.iter().find(|c| c["doc:cellValue"] == v).unwrap()["doc:columnHeader"].clone()
        };
        assert_eq!(header("Fish pie"), "Item", "a rowspan label appears once");
        assert_eq!(header("2"), "Qty");
        assert_eq!(header("14.50"), "Price / Unit");
        assert_eq!(
            header("29.00"),
            "Price / Line",
            "the banner reaches every column it spans"
        );
        assert!(cells.iter().all(|c| c["doc:rowIndex"] == json!(0)));
    }

    #[test]
    fn a_single_header_row_names_every_column_it_spans() {
        // One header row: its label, verbatim, under each column it spans,
        // as a filer sets a period over its currency sign and its value.
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec!["Item".into(), "Price".into(), "".into()],
            vec!["Fish pie".into(), "14.50".into(), "29.00".into()],
        ]);
        t.header_rows = Some(1);
        t.merged_left = Some(vec![false, false, true, false, false, false]);
        let g = graph(&[t]);
        let cells = find(&g, "doc:TableCell");
        let line = cells
            .iter()
            .find(|c| c["doc:cellValue"] == "29.00")
            .unwrap();
        assert_eq!(line["doc:columnHeader"], "Price");
        let unit = cells
            .iter()
            .find(|c| c["doc:cellValue"] == "14.50")
            .unwrap();
        assert_eq!(unit["doc:columnHeader"], "Price");
    }

    #[test]
    fn headerless_table_emits_all_rows_as_data_without_column_headers() {
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec!["".into(), "6,785".into()],
            vec!["Gold".into(), "11,037".into()],
        ]);
        t.header_rows = Some(0);
        let g = graph(&[t]);
        let cells = find(&g, "doc:TableCell");
        // Both non-empty cells of row 0 and row 1 materialise as data.
        assert_eq!(cells.len(), 3);
        let first = cells
            .iter()
            .find(|c| c["doc:cellValue"] == "6,785")
            .unwrap();
        assert_eq!(first["doc:rowIndex"], json!(0));
        assert!(first.get("doc:columnHeader").is_none());
    }

    #[test]
    fn every_element_is_stamped_with_source_doc_page_and_bbox() {
        let g = graph(&[el("doco:Paragraph", "x", None)]);
        let p = &find(&g, "doco:Paragraph")[0];
        assert_eq!(p["doc:sourceDocument"]["@id"], "urn:test:doc");
        assert_eq!(p["doc:pageIndex"], json!(0));
        assert_eq!(p["doc:bbox"], "10.00,20.00,110.00,40.00");
        // Structural wrappers are stamped too (retract-on-rerun must catch them).
        let doc = &find(&g, "foaf:Document")[0];
        assert_eq!(doc["doc:sourceDocument"]["@id"], "urn:test:doc");
    }

    #[test]
    fn parse_warnings_are_declared_on_the_document_node() {
        use crate::element::ParseWarning;
        let els = vec![el("doco:Paragraph", "Review memo", None)];
        let o = DocoOptions {
            warnings: vec![ParseWarning {
                page: 2,
                kind: "ImageDecodeFailure".into(),
                count: 3,
            }],
            ..opts()
        };
        let v: Value = serde_json::from_str(&to_doco(&els, &o)).unwrap();
        let g = v["@graph"].as_array().unwrap();
        assert_eq!(
            find(g, "foaf:Document")[0]["doc:parseWarnings"],
            json!({ "@type": "@json", "@value": [
                { "pageIndex": 2, "kind": "ImageDecodeFailure", "count": 3 }
            ] })
        );
        assert!(find(&graph(&els), "foaf:Document")[0]
            .get("doc:parseWarnings")
            .is_none());
    }

    #[test]
    fn unread_regions_are_declared_on_the_document_node() {
        use crate::element::UnreadRegion;
        let els = vec![el("doco:Paragraph", "Review memo", None)];
        let o = DocoOptions {
            unread_regions: vec![UnreadRegion {
                page: 0,
                bbox: BBox {
                    x0: 56.0,
                    y0: 582.0,
                    x1: 556.0,
                    y1: 732.0,
                },
                reason: "Region".into(),
            }],
            ..opts()
        };
        let v: Value = serde_json::from_str(&to_doco(&els, &o)).unwrap();
        let g = v["@graph"].as_array().unwrap();
        let doc = find(g, "foaf:Document")[0];
        // A JSON literal, the box written as `doc:bbox` writes one.
        assert_eq!(
            doc["doc:unreadRegions"],
            json!({ "@type": "@json", "@value": [
                { "pageIndex": 0, "bbox": "56.00,582.00,556.00,732.00", "reason": "Region" }
            ] })
        );
        // None, no key: a document read whole says nothing.
        let quiet = &graph(&els);
        assert!(find(quiet, "foaf:Document")[0]
            .get("doc:unreadRegions")
            .is_none());
    }

    #[test]
    fn an_unread_image_is_listed_on_the_document_by_its_figure() {
        let o = DocoOptions {
            base_iri: "urn:test".into(),
            unread_images: vec![crate::element::UnreadImage {
                figure: "figure-0-1".into(),
                image: "m1".into(),
                part: "word/media/image1.png".into(),
                reason: "NotRead".into(),
            }],
            ..Default::default()
        };
        let v: Value = serde_json::from_str(&to_doco(&[], &o)).unwrap();
        let doc = &v["@graph"][0];
        assert_eq!(doc["doc:unreadImages"]["@type"], "@json");
        assert_eq!(
            doc["doc:unreadImages"]["@value"],
            serde_json::json!([{
                "figure": "figure-0-1", "image": "m1",
                "part": "word/media/image1.png", "reason": "NotRead"
            }])
        );
        // Nothing unread, nothing said.
        let v: Value = serde_json::from_str(&to_doco(&[], &opts())).unwrap();
        assert!(v["@graph"][0].get("doc:unreadImages").is_none());
    }

    #[test]
    fn the_document_node_says_which_bytes_it_was_read_from() {
        let sha = "661511bb2b30c4e8a9f2d71b05e3c6a48f90d2b17e5a3c8f4b6d1e09a7c25f3e";
        let o = DocoOptions {
            base_iri: "urn:test".into(),
            sha256: Some(sha.into()),
            source_name: Some("Q3 report.pdf".into()),
            attachments: vec![Attachment {
                filename: Some("quote.pdf".into()),
                content_type: "application/pdf".into(),
                size: 3,
                sha256: crate::sha256_hex(b"pdf"),
                inline: false,
            }],
            ..Default::default()
        };
        let v: Value = serde_json::from_str(&to_doco(&[], &o)).unwrap();
        let doc = &v["@graph"][0];
        assert_eq!(doc["@type"], "foaf:Document");
        assert_eq!(doc["doc:sha256"], sha);
        assert_eq!(doc["doc:sourceName"], "Q3 report.pdf");
        assert_eq!(
            doc["doc:attachments"]["@value"][0]["sha256"],
            "c35b21d6ca39aa7cc3b79a705d989f1a6e88b99ab43988d74048799e3db926a3"
        );
    }

    #[test]
    fn the_document_node_lists_its_ocr_readings_with_their_confidence() {
        use crate::element::OcrReading;
        // None read: nothing is said.
        let v: Value = serde_json::from_str(&to_doco(&[], &opts())).unwrap();
        assert!(v["@graph"][0].get("doc:ocrReadings").is_none());
        let o = DocoOptions {
            ocr_readings: vec![
                OcrReading {
                    page: Some(0),
                    image: None,
                    crop: "r1".into(),
                    engine: Some("oar-ocr".into()),
                    words: 12,
                    mean_confidence: Some(0.9812),
                    min_confidence: Some(0.4),
                },
                OcrReading {
                    page: None,
                    image: Some("m2".into()),
                    crop: "full".into(),
                    engine: None,
                    words: 3,
                    mean_confidence: None,
                    min_confidence: None,
                },
            ],
            ..opts()
        };
        let v: Value = serde_json::from_str(&to_doco(&[], &o)).unwrap();
        let r = &v["@graph"][0]["doc:ocrReadings"];
        assert_eq!(r["@type"], "@json");
        assert_eq!(
            r["@value"],
            json!([
                {"pageIndex": 0, "crop": "r1", "engine": "oar-ocr", "words": 12,
                 "meanConfidence": 0.9812, "minConfidence": 0.4},
                {"image": "m2", "crop": "full", "words": 3}
            ])
        );
    }

    #[test]
    fn a_table_is_stamped_and_its_cells_are_reached_through_it() {
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec!["Item".into(), "Price".into()],
            vec!["Fish pie".into(), "14.50".into()],
        ]);
        let g = graph(&[t]);
        let table = &find(&g, "doco:Table")[0];
        assert_eq!(table["doc:sourceDocument"]["@id"], "urn:test:doc");
        let cells = find(&g, "doc:TableCell");
        assert_eq!(cells.len(), 2);
        for cell in cells {
            assert!(cell.get("doc:sourceDocument").is_none());
            assert!(table["po:contains"]
                .as_array()
                .unwrap()
                .contains(&cell["@id"]));
        }
    }

    #[test]
    fn minted_iris_stay_in_the_base_iris_namespace() {
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec!["A".into(), "B".into()],
            vec!["1".into(), "2".into()],
        ]);
        let els = vec![el("doco:SectionTitle", "Prices", Some(1)), t];
        let g = graph(&els);
        let ids: Vec<&str> = g.iter().map(|n| n["@id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            [
                "urn:test-element-0",
                "urn:test-element-1",
                "urn:test-section-2",
                "urn:test-element-3",
                "urn:test-table-4",
                "urn:test-cell-5",
                "urn:test-cell-6",
            ]
        );
        // A base ending in a delimiter is a namespace of its own, and the
        // caller's choice.
        let o = DocoOptions {
            base_iri: "https://example.org/doc/7f3a/".into(),
            ..Default::default()
        };
        let v: Value = serde_json::from_str(&to_doco(&els, &o)).unwrap();
        assert_eq!(
            v["@graph"][0]["@id"],
            "https://example.org/doc/7f3a/element-0"
        );
    }

    #[test]
    fn a_default_base_is_the_stem_as_a_slug_and_the_hash() {
        let sha = "661511bb2b30c4e8a9f2d71b05e3c6a48f90d2b17e5a3c8f4b6d1e09a7c25f3e";
        assert_eq!(
            default_base_iri(Some("report"), sha),
            "urn:fluree-doc-parse:report-661511bb2b30"
        );
        // Nothing that is not allowed in an IRI, and nothing that moves
        // where a store splits one.
        assert_eq!(
            default_base_iri(Some("Invoice #12 final"), sha),
            "urn:fluree-doc-parse:Invoice-12-final-661511bb2b30"
        );
        assert_eq!(slug("Résumé 2024"), "Résumé-2024");
        assert_eq!(slug("a/b:c?d"), "a-b-c-d");
        assert_eq!(slug("  --report.v2--  "), "report.v2");
        // Standard input has no name, only bytes.
        assert_eq!(
            default_base_iri(None, sha),
            "urn:fluree-doc-parse:661511bb2b30"
        );
        assert_eq!(
            default_base_iri(Some("###"), sha),
            "urn:fluree-doc-parse:661511bb2b30"
        );
    }

    #[test]
    fn label_truncates_but_is_string_survives() {
        let long = "x".repeat(150);
        let g = graph(&[el("doco:Paragraph", &long, None)]);
        let p = &find(&g, "doco:Paragraph")[0];
        assert_eq!(p["nif:isString"].as_str().unwrap().chars().count(), 150);
        assert_eq!(p["rdfs:label"].as_str().unwrap().chars().count(), 101);
    }

    #[test]
    fn a_link_becomes_a_node_the_element_references() {
        let mut e = el("doco:Paragraph", "see the filing now", None);
        e.links = Some(vec![
            crate::element::Link::uri("https://sec.example/x").spanning(4, 14)
        ]);
        let g = graph(&[e]);
        let link = find(&g, "doc:Link");
        assert_eq!(link.len(), 1);
        assert_eq!(
            link[0]["doc:linkTarget"],
            json!({ "@value": "https://sec.example/x", "@type": "xsd:anyURI" })
        );
        assert_eq!(link[0]["nif:isString"], "the filing");

        // The anchor's offsets index the text projection, exactly as an
        // element's do.
        let para = find(&g, "doco:Paragraph");
        let s = link[0]["nif:beginIndex"].as_u64().unwrap() as usize;
        let t = link[0]["nif:endIndex"].as_u64().unwrap() as usize;
        assert_eq!(s, para[0]["nif:beginIndex"].as_u64().unwrap() as usize + 4);
        assert_eq!(t - s, 10);

        let id = link[0]["@id"].as_str().unwrap();
        assert_eq!(para[0]["doc:link"][0], id);
    }

    #[test]
    fn a_turn_carries_its_speaker_and_times_and_its_text_carries_no_time() {
        let mut e = el("doco:Paragraph", "Ada Park: Renewal is at risk.", None);
        e.bbox = None;
        e.turn = Some(crate::element::Turn {
            speaker: Some("Ada Park".into()),
            start_ms: 272_404,
            end_ms: 279_000,
        });
        let g = graph(std::slice::from_ref(&e));
        let p = &find(&g, "doco:Paragraph")[0];
        assert_eq!(p["doc:speaker"], "Ada Park");
        assert_eq!(p["doc:startMs"], json!(272_404));
        assert_eq!(p["doc:endMs"], json!(279_000));
        assert!(p.get("doc:bbox").is_none());
        assert_eq!(to_text(&[e]), "Ada Park: Renewal is at risk.\n");
    }

    #[test]
    fn anchors_are_cut_by_characters_past_multi_byte_ones() {
        let mut e = el("doco:Paragraph", "é ç ü €", None);
        let uri = crate::element::Link::uri;
        // The last one runs past the text, and is cut at its end.
        e.links = Some(vec![uri("a").spanning(2, 3), uri("b").spanning(6, 9)]);
        let g = graph(&[e]);
        let link = find(&g, "doc:Link");
        assert_eq!(link[0]["nif:isString"], "ç");
        assert_eq!(link[1]["nif:isString"], "€");
    }

    #[test]
    fn an_internal_jump_carries_a_page_rather_than_an_address() {
        let mut e = el("doco:Paragraph", "see Chapter 4", None);
        e.links = Some(vec![crate::element::Link::page(11).spanning(4, 13)]);
        let g = graph(&[e]);
        let link = find(&g, "doc:Link");
        assert_eq!(link[0]["doc:linkPage"], 11);
        assert!(link[0].get("doc:linkTarget").is_none());
    }

    #[test]
    fn a_signature_is_its_senders_and_a_message_goes_on_after_its_quote() {
        let mailbox = |name: &str, address: &str| Mailbox {
            name: Some(name.into()),
            address: Some(address.into()),
        };
        let para = |id: &str, text: &str| {
            let mut e = el("doco:Paragraph", text, None);
            e.id = id.into();
            e
        };
        let opener = |id: &str, text: &str, from: Mailbox, quoted: bool| {
            let mut e = para(id, text);
            e.message = Some(Box::new(Message {
                from: vec![from],
                quoted,
                ..Default::default()
            }));
            e
        };
        let mut sign_off = para("e5", "--");
        sign_off.resumes = Some("e1".into());
        sign_off.signature = true;
        let mut block = para("e6", "Kai Moreno\n1 Main Street, Springfield");
        block.signature = true;
        let els = vec![
            opener(
                "e1",
                "From: Kai Moreno <kai@example.net>",
                mailbox("Kai Moreno", "kai@example.net"),
                false,
            ),
            para("e2", "Done."),
            opener(
                "e3",
                "On Fri, Jul 17, 2026 at 10:05 AM Lena Holt <lena@example.com> wrote:",
                mailbox("Lena Holt", "lena@example.com"),
                true,
            ),
            para("e4", "Ready?"),
            sign_off,
            block,
        ];
        let g = graph(&els);
        let messages = find(&g, "doc:Message");
        let signatures = find(&g, "doc:Signature");
        assert_eq!((messages.len(), signatures.len()), (2, 1));
        let sig = signatures[0];
        // Signed by the sender's mailbox, inside the message it went back
        // to, not the quoted one before it.
        let kai = find(&g, "doc:Mailbox")
            .into_iter()
            .find(|m| m["doc:address"] == "kai@example.net")
            .unwrap();
        assert_eq!(sig["doc:signer"], json!([kai["@id"]]));
        let holds = |n: &Value, id: &Value| {
            n["po:contains"]
                .as_array()
                .is_some_and(|kids| kids.contains(id))
        };
        assert!(holds(messages[0], &sig["@id"]));
        assert!(!holds(messages[1], &sig["@id"]));
        assert_eq!(sig["po:contains"].as_array().unwrap().len(), 2);
        // Its span is the text of its elements in the projection.
        let projection = to_text(&els);
        let (b, e) = (
            sig["nif:beginIndex"].as_u64().unwrap() as usize,
            sig["nif:endIndex"].as_u64().unwrap() as usize,
        );
        let slice: String = projection.chars().skip(b).take(e - b).collect();
        assert_eq!(slice, "--\n\nKai Moreno\n1 Main Street, Springfield");
        assert_eq!(sig["nif:isString"], slice.as_str());
    }

    /// A one-column table of `rows`, no header, each row continuing the
    /// one above but the first.
    fn merged_column(rows: Vec<String>) -> Element {
        let n = rows.len();
        let mut t = el("doco:Table", "", None);
        t.cells = Some(rows.into_iter().map(|r| vec![r]).collect());
        t.header_rows = Some(0);
        t.merged_down = Some((0..n).map(|r| r > 0).collect());
        t
    }

    #[test]
    fn a_long_value_merged_down_is_counted_once_a_row() {
        let n = 50;
        let long = "v".repeat(1000);
        let mut rows = vec![long.clone()];
        rows.resize(n, String::new());
        let t = merged_column(rows);
        let copies = table_copies(&t);
        // Each covered row is a node of its own, holding the whole value.
        assert_eq!(
            copies,
            Copies {
                cells: (n - 1) as u64,
                text: (n as u64 - 1) * 1000,
                merged: (n as u64 - 1) * 1000
            }
        );
        let g = graph(std::slice::from_ref(&t));
        let cells = find(&g, "doc:TableCell");
        assert_eq!(cells.len(), n);
        assert!(cells.iter().all(|c| c["doc:cellValue"] == long.as_str()));
    }

    #[test]
    fn fragments_merged_down_are_joined_then_counted_on_each_row() {
        // A run of n fragments is one value of 2n - 1 characters, on each
        // of its n rows: the square of the run, not the run.
        let n = 1000u64;
        let t = merged_column(vec!["x".into(); n as usize]);
        let copies = table_copies(&t);
        assert_eq!(
            copies,
            Copies {
                cells: 0,
                text: n * (2 * n - 1) - n,
                merged: n * (2 * n - 1) - n
            }
        );
        let small = merged_column(vec!["x".into(); 4]);
        let g = graph(&[small]);
        assert!(find(&g, "doc:TableCell")
            .iter()
            .all(|c| c["doc:cellValue"] == "x x x x"));
    }

    #[test]
    fn each_cell_counts_the_labels_it_carries() {
        let long = "w".repeat(100);
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec!["Name".into(), "Code".into()],
            vec![long.clone(), "".into()],
            vec![long.clone(), "".into()],
            vec!["Alpha".into(), "1".into()],
            vec!["Beta".into(), "2".into()],
        ]);
        t.header_rows = Some(1);
        // Row 1 is a band; rows 3 and 4 fall under it.
        t.sub_headers = Some(vec![1]);
        t.merged_left = Some(vec![
            false, false, false, true, false, false, false, false, false, false,
        ]);
        let copies = table_copies(&t);
        // Each cell under the band carries its column's header and the
        // band's text, and each but the first its row's first cell.
        let row2 = 4 + 100;
        let row3 = (4 + 100) + (4 + 5 + 100);
        let row4 = (4 + 100) + (4 + 4 + 100);
        assert_eq!(
            copies,
            Copies {
                cells: 0,
                text: row2 + row3 + row4,
                // The band is one cell's text already: nothing is joined.
                merged: 0
            }
        );
    }

    #[test]
    fn a_table_of_many_bands_is_written_in_linear_time() {
        // Each row searched the list of bands, twice, for whether it is one
        // and for the last above it: 170,000 bands took 28 s.
        let n = 120_000;
        let mut t = el("doco:Table", "", None);
        let mut rows = vec![vec!["Name".to_string(), "Code".to_string()]];
        for i in 0..n {
            rows.push(vec![format!("Band {i}"), String::new()]);
            rows.push(vec![format!("Item {i}"), i.to_string()]);
        }
        t.cells = Some(rows);
        t.header_rows = Some(1);
        t.sub_headers = Some((0..n).map(|i| 1 + 2 * i).collect());
        let start = std::time::Instant::now();
        let g = graph(std::slice::from_ref(&t));
        let xhtml = crate::emit::to_xhtml(std::slice::from_ref(&t));
        let took = start.elapsed();
        let cells = find(&g, "doc:TableCell");
        assert_eq!(cells.len(), 2 * n);
        let last = cells.last().unwrap();
        assert_eq!(last["doc:sectionLabel"], format!("Band {}", n - 1));
        assert_eq!(last["doc:cellValue"], (n - 1).to_string());
        assert_eq!(xhtml.matches("<th colspan=\"2\">Band").count(), n);
        assert!(took.as_secs() < 30, "{took:?}");
    }

    #[test]
    fn a_group_merged_down_many_rows_is_named_in_linear_time() {
        // Each cell beside a group walked the group's run up and down to
        // find the rows it spans: a group over 100,000 rows walked them
        // for each of them.
        let n = 100_000;
        let mut t = el("doco:Table", "", None);
        let mut rows = vec![vec![
            "Group".to_string(),
            "Item".to_string(),
            "Code".to_string(),
        ]];
        for i in 0..n {
            let group = if i == 0 { "All" } else { "" };
            rows.push(vec![group.to_string(), format!("Item {i}"), i.to_string()]);
        }
        t.cells = Some(rows);
        t.header_rows = Some(1);
        let mut down = vec![false; (n + 1) * 3];
        for r in 2..=n {
            down[r * 3] = true;
        }
        t.merged_down = Some(down);
        let start = std::time::Instant::now();
        let g = graph(std::slice::from_ref(&t));
        let took = start.elapsed();
        let cells = find(&g, "doc:TableCell");
        let last = cells.last().unwrap();
        assert_eq!(last["doc:cellValue"], (n - 1).to_string());
        assert_eq!(last["doc:rowHeader"], format!("All / Item {}", n - 1));
        assert!(took.as_secs() < 30, "{took:?}");
    }

    #[test]
    fn a_tall_stacked_header_is_named_in_time_linear_in_its_cells() {
        // Each column walked back along its row to the cell holding its
        // label: 128 rows merged across 16,384 columns took 4 s.
        let (h, cols) = (64, 16_384);
        let mut t = el("doco:Table", "", None);
        let mut rows: Vec<Vec<String>> = (0..h)
            .map(|r| {
                let mut row = vec![String::new(); cols];
                row[0] = format!("Level {r}");
                row
            })
            .collect();
        rows.push((0..cols).map(|c| c.to_string()).collect());
        t.cells = Some(rows);
        t.header_rows = Some(h);
        let mut left = vec![true; (h + 1) * cols];
        for r in 0..=h {
            left[r * cols] = false;
        }
        left[h * cols..].fill(false);
        t.merged_left = Some(left);
        let start = std::time::Instant::now();
        let g = graph(std::slice::from_ref(&t));
        let took = start.elapsed();
        let cells = find(&g, "doc:TableCell");
        assert_eq!(cells.len(), cols);
        let path = (0..h).map(|r| format!("Level {r}")).collect::<Vec<_>>();
        assert_eq!(cells[cols - 1]["doc:columnHeader"], path.join(" / "));
        assert!(took.as_secs() < 10, "{took:?}");
    }

    /// What the DoCO output writes for a table's cells, at the budget's
    /// costs: the nodes, and the text of their values and labels.
    fn written(t: &Element) -> (u64, u64) {
        let g = graph(std::slice::from_ref(t));
        let cells = find(&g, "doc:TableCell");
        let text = cells
            .iter()
            .flat_map(|c| {
                [
                    "doc:cellValue",
                    "doc:columnHeader",
                    "doc:rowHeader",
                    "doc:sectionLabel",
                ]
                .map(|k| c[k].as_str().map_or(0, crate::budget::text_cost))
            })
            .sum();
        (cells.len() as u64, text)
    }

    #[test]
    fn a_wrapped_table_is_counted_by_its_records_and_its_spanned_header() {
        // Two records a row: each cell after a record's first is labelled
        // by that first cell, which the row's own first does not bound.
        let long = "a label far longer than the row's own first cell";
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec!["Name".into(), "Value".into(), "Name".into(), "Value".into()],
            vec!["".into(), "1".into(), long.into(), "2".into()],
        ]);
        t.header_rows = Some(1);
        let raw = 4 + 5 + 4 + 5 + 1 + long.len() as u64 + 1;
        let (_, text) = written(&t);
        assert!(text <= raw + table_copies(&t).text, "{text} written");
        // One header row: an empty cell a label spans is named by it.
        let mut s = el("doco:Table", "", None);
        s.cells = Some(vec![
            vec!["Quarterly revenue by region".into(), "".into(), "".into()],
            vec!["x".into(), "y".into(), "z".into()],
        ]);
        s.header_rows = Some(1);
        s.merged_left = Some(vec![false, true, true, false, false, false]);
        let (_, text) = written(&s);
        assert!(text <= 27 + 3 + table_copies(&s).text, "{text} written");
    }

    #[test]
    fn the_copies_counted_are_never_fewer_than_those_written() {
        // Tables of every shape a small grid takes: merges each way, bands
        // and stacks of them, headers stacked or restated partway down,
        // texts with spaces and escapes.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % m
        };
        let texts = ["", "", "a", "bb", " c d ", "\"&<", "é"];
        for _ in 0..3000 {
            let (n, cols) = (1 + next(6) as usize, 1 + next(4) as usize);
            let mut t = el("doco:Table", "", None);
            t.cells = Some(
                (0..n)
                    .map(|_| {
                        (0..cols)
                            .map(|_| texts[next(7) as usize].to_string())
                            .collect()
                    })
                    .collect(),
            );
            let flags = |p: u64, next: &mut dyn FnMut(u64) -> u64| {
                (next(2) == 0).then(|| (0..n * cols).map(|_| next(100) < p).collect())
            };
            t.merged_down = flags(40, &mut next);
            t.merged_left = flags(30, &mut next);
            t.sub_headers = (next(3) == 0).then(|| (0..n).filter(|_| next(3) == 0).collect());
            t.restated_headers = (next(4) == 0).then(|| (0..n).filter(|_| next(4) == 0).collect());
            t.header_rows = [None, Some(0), Some(1), Some(2), Some(3)][next(5) as usize];
            let raw: u64 = t
                .cells
                .iter()
                .flatten()
                .flatten()
                .map(|c| crate::budget::text_cost(c.trim()))
                .sum();
            let filled = t
                .cells
                .iter()
                .flatten()
                .flatten()
                .filter(|c| !c.trim().is_empty())
                .count();
            let copies = table_copies(&t);
            let (nodes, text) = written(&t);
            assert!(
                nodes <= filled as u64 + copies.cells,
                "{t:?}: {nodes} nodes, {copies:?}"
            );
            assert!(
                text <= raw + copies.text,
                "{t:?}: {text} written, {raw} + {copies:?}"
            );
            // The text projection copies the merges' text, not the labels.
            let shown: u64 = projected_cells(&t)
                .unwrap()
                .iter()
                .flatten()
                .map(|c| crate::budget::text_cost(c.trim()))
                .sum();
            assert!(
                shown <= raw + copies.merged,
                "{t:?}: {shown} shown, {raw} + {copies:?}"
            );
            assert!(copies.merged <= copies.text, "{copies:?}");
        }
    }
}
