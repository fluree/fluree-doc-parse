//! Projection conformance: each format's writer, read back with our own
//! reader, held to the element model.
//!
//! A reader produces the element list `E`; a writer projects it into
//! Markdown or XHTML; the matching reader (`fluree-doc-markdown`,
//! `fluree-doc-html`) reads the projection back. What the read-back says is
//! compared with `E` under the contract in `docs/design/conformance.md`: an
//! element's text, the order of elements, headings and their levels, list
//! items and their depth, and a table's grid, header rows, spans, section
//! bands and restated header rows. A difference the contract does not list
//! as the format's loss is a [`Violation`].
//!
//! The check lives in a crate of its own because both readers depend on the
//! model: the model cannot depend on them.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use fluree_doc_model::{Element, MarkdownOptions, Notes};

/// A projection the check reads back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Md,
    Xhtml,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Md => "md",
            Format::Xhtml => "xhtml",
        }
    }
}

/// One place where a projection, read back, says something other than `E`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Violation {
    pub format: Format,
    /// The contract's class: `text.changed`, `table.header_rows`, ….
    pub class: &'static str,
    /// The element of `E` concerned (a table carried over pages: its first
    /// part). `None` for something only the read-back holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element: Option<usize>,
    /// What differs, with a short excerpt of the text.
    pub detail: String,
}

/// Write `elements` to every format, read each back, and hold it to the
/// contract.
pub fn conform(elements: &[Element], notes: &Notes) -> Vec<Violation> {
    let md =
        fluree_doc_model::to_markdown_with_options(elements, notes, &MarkdownOptions::default());
    let mut out = check(elements, Format::Md, &fluree_doc_markdown::parse(&md));
    let xhtml = fluree_doc_model::to_xhtml_with(elements, notes);
    out.extend(check(
        elements,
        Format::Xhtml,
        &fluree_doc_html::parse(&xhtml),
    ));
    out
}

/// Violations counted by format and class.
pub fn counts(violations: &[Violation]) -> BTreeMap<(Format, &'static str), usize> {
    let mut m = BTreeMap::new();
    for v in violations {
        *m.entry((v.format, v.class)).or_insert(0) += 1;
    }
    m
}

/// Hold `read`, the elements read back from `format`'s projection of
/// `elements`, to the contract.
pub fn check(elements: &[Element], format: Format, read: &[Element]) -> Vec<Violation> {
    let a = reference(elements, format);
    let b = read_back(read);
    let mut c = Checker {
        format,
        a: &a,
        b: &b,
        out: Vec::new(),
        lost: Vec::new(),
        extra: Vec::new(),
    };
    c.run();
    c.out
}

// ---------------------------------------------------------------------------
// Units: what is compared.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Para,
    Heading(usize),
    Item(usize),
    Table,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Para => "paragraph",
            Kind::Heading(_) => "heading",
            Kind::Item(_) => "list item",
            Kind::Table => "table",
        }
    }
}

struct Unit {
    kind: Kind,
    /// The element's index in its list.
    at: usize,
    /// The text, whitespace runs one space; a table's cells joined.
    text: String,
    /// The text without whitespace: what alignment matches on.
    key: String,
    table: Option<Table>,
    /// The first item of a run of items: the element before it in its list
    /// is no item.
    run_start: bool,
    /// Characters the format may add to this element's text: a transcript
    /// turn's clock reading in Markdown.
    may_add: &'static str,
}

impl Unit {
    fn block(kind: Kind, at: usize, text: &str) -> Option<Unit> {
        let text = squeeze(text);
        let key = key_of(&text);
        (!key.is_empty()).then_some(Unit {
            kind,
            at,
            text,
            key,
            table: None,
            run_start: false,
            may_add: "",
        })
    }

    fn table(at: usize, t: Table) -> Option<Unit> {
        let text = t
            .cells
            .iter()
            .filter(|c| !c.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ");
        let key = key_of(&text);
        (!key.is_empty()).then_some(Unit {
            kind: Kind::Table,
            at,
            text,
            key,
            table: Some(t),
            run_start: false,
            may_add: "",
        })
    }
}

/// Whitespace runs as one space, trimmed: the normalisation every format
/// imposes, a cell's line breaks included.
fn squeeze(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn key_of(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Text opening with an ordered list's marker as Markdown reads one: up to
/// nine digits, `.` or `)`, a space. The writer makes it the item's marker.
fn strip_ordinal(text: &str) -> &str {
    let t = text.trim_start();
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=9).contains(&digits)
        && matches!(t[digits..].chars().next(), Some('.' | ')'))
        && t[digits + 1..].starts_with(' ')
    {
        &t[digits + 2..]
    } else {
        text
    }
}

/// Text without the marker that opens a list item or a heading in
/// Markdown: an ordinal, a bullet, a run of `#`.
fn strip_marker(text: &str) -> &str {
    let t = text.trim_start();
    let o = strip_ordinal(t);
    if o.len() < t.len() {
        return o;
    }
    for b in ["- ", "* ", "+ "] {
        if let Some(r) = t.strip_prefix(b) {
            return r;
        }
    }
    let hashes = t.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
        return &t[hashes + 1..];
    }
    text
}

/// `E` as every writer reads it: a table carried over a page break joined
/// to the one it continues, its repeated header rows dropped; elements with
/// nothing to say left out.
fn reference(elements: &[Element], format: Format) -> Vec<Unit> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut notes_open = false;
    while i < elements.len() {
        let e = &elements[i];
        // A deck's speaker notes, as both writers set them: a heading over
        // all of them, and each slide's name over its own.
        if let Some(slide) = e.notes_for {
            if i == 0 || elements[i - 1].notes_for != Some(slide) {
                if !notes_open {
                    notes_open = true;
                    out.extend(Unit::block(Kind::Heading(1), i, "Speaker notes"));
                }
                let name = fluree_doc_model::emit::notes_heading(elements, slide);
                out.extend(Unit::block(Kind::Heading(2), i, &name));
            }
        }
        match e.kind.as_str() {
            "doco:Table" => {
                if let Some(rows) = &e.cells {
                    let first = i;
                    let mut parts = vec![Part::of(e, rows, 0)];
                    while carries_on(elements, i + 1) {
                        i += 1;
                        let skip = repeated_header_rows(elements, i);
                        let p = &elements[i];
                        parts.push(Part::of(p, p.cells.as_ref().unwrap(), skip));
                    }
                    let t = Table::joined(&parts);
                    let t = match format {
                        Format::Md => t.in_pipes(),
                        Format::Xhtml => t,
                    };
                    out.extend(Unit::table(first, t));
                } else if !e.text.trim().is_empty() {
                    // A model's table reading, already HTML: the writers
                    // pass it through, and it means what it reads as.
                    for r in fluree_doc_html::parse(&e.text) {
                        out.extend(read_unit(i, &r));
                    }
                }
            }
            "doco:Title" => out.extend(Unit::block(Kind::Heading(1), i, &e.text)),
            "doco:SectionTitle" => out.extend(Unit::block(
                Kind::Heading(e.level.unwrap_or(1).clamp(1, 6)),
                i,
                &e.text,
            )),
            "doc:ListItem" => {
                // In Markdown an item's own number is its marker.
                let text = match format {
                    Format::Md => strip_ordinal(&e.text),
                    Format::Xhtml => &e.text,
                };
                let first = i == 0 || elements[i - 1].kind != "doc:ListItem";
                out.extend(Unit::block(Kind::Item(e.depth), i, text).map(|mut u| {
                    u.run_start = first;
                    u
                }));
            }
            _ => {
                if let Some(mut u) = Unit::block(Kind::Para, i, &e.text) {
                    if e.turn.is_some() && format == Format::Md {
                        u.may_add = "()0123456789:";
                    }
                    out.push(u);
                }
            }
        }
        i += 1;
    }
    nest(&mut out);
    out
}

/// As the writers decide it: the table at `i` goes on from the one just
/// before it.
fn carries_on(elements: &[Element], i: usize) -> bool {
    let (Some(prev), Some(e)) = (i.checked_sub(1).map(|p| &elements[p]), elements.get(i)) else {
        return false;
    };
    let cols = |e: &Element| e.cells.as_ref().and_then(|r| r.first()).map(Vec::len);
    prev.kind == "doco:Table"
        && e.kind == "doco:Table"
        && e.continues.as_deref() == Some(prev.id.as_str())
        && cols(prev).is_some()
        && cols(prev) == cols(e)
}

/// The leading rows of a continued part that repeat the header before it.
fn repeated_header_rows(elements: &[Element], i: usize) -> usize {
    if elements[i - 1].header_only() {
        0
    } else {
        elements[i].header_rows.unwrap_or(0)
    }
}

fn read_back(read: &[Element]) -> Vec<Unit> {
    let mut out: Vec<Unit> = read
        .iter()
        .enumerate()
        .flat_map(|(i, e)| {
            let first = i == 0 || read[i - 1].kind != "doc:ListItem";
            read_unit(i, e).map(|mut u| {
                u.run_start = first;
                u
            })
        })
        .collect();
    nest(&mut out);
    out
}

fn read_unit(i: usize, e: &Element) -> Option<Unit> {
    match e.kind.as_str() {
        "doco:Table" => {
            let rows = e.cells.as_ref()?;
            Unit::table(i, Table::joined(&[Part::of(e, rows, 0)]))
        }
        "doco:Title" => Unit::block(Kind::Heading(1), i, &e.text),
        "doco:SectionTitle" => Unit::block(Kind::Heading(e.level.unwrap_or(1)), i, &e.text),
        "doc:ListItem" => Unit::block(Kind::Item(e.depth), i, &e.text),
        _ => Unit::block(Kind::Para, i, &e.text),
    }
}

// ---------------------------------------------------------------------------
// Tables in canonical form.

/// One element's part of a table: its rows from `skip` on.
struct Part<'a> {
    e: &'a Element,
    rows: &'a [Vec<String>],
    skip: usize,
}

impl<'a> Part<'a> {
    fn of(e: &'a Element, rows: &'a [Vec<String>], skip: usize) -> Self {
        Part {
            e,
            rows,
            skip: skip.min(rows.len()),
        }
    }
}

/// A table as the model means it: each span written once, at its first
/// cell, with the text of every cell it covers; a band row one cell across.
#[derive(Debug, Clone, PartialEq)]
struct Table {
    rows: usize,
    cols: usize,
    /// Row-major: a span's text at its first cell, the cells it covers empty.
    cells: Vec<String>,
    /// The spans that carry a value: first cell → (rows, columns).
    spans: BTreeMap<(usize, usize), (usize, usize)>,
    /// `None` where the reader measured nothing: no claim to keep.
    header_rows: Option<usize>,
    sub_headers: BTreeSet<usize>,
    restated: BTreeSet<usize>,
}

impl Table {
    fn joined(parts: &[Part]) -> Table {
        let cols = parts
            .iter()
            .flat_map(|p| p.rows.iter().map(Vec::len))
            .max()
            .unwrap_or(0);
        let mut grid: Vec<Vec<String>> = Vec::new();
        let mut down: Vec<bool> = Vec::new();
        let mut left: Vec<bool> = Vec::new();
        let mut header: Vec<bool> = Vec::new();
        let mut sub_headers = BTreeSet::new();
        let mut restated = BTreeSet::new();
        for p in parts {
            let n = p.rows.len();
            let w = p.rows.first().map_or(0, Vec::len);
            let flags = |m: &Option<Vec<bool>>| {
                m.as_ref()
                    .filter(|m| w > 0 && m.len() == n * w && w == cols)
                    .cloned()
            };
            let (md, ml) = (flags(&p.e.merged_down), flags(&p.e.merged_left));
            let base = grid.len();
            let shift = |r: &usize| (*r >= p.skip && *r < n).then(|| base + r - p.skip);
            sub_headers.extend(p.e.sub_headers.iter().flatten().filter_map(shift));
            restated.extend(p.e.restated_headers.iter().flatten().filter_map(shift));
            let h = p.e.header_rows.unwrap_or(0);
            for r in p.skip..n {
                let mut row = p.rows[r].clone();
                row.resize(cols, String::new());
                grid.push(row);
                header.push(r < h);
                for c in 0..cols {
                    let k = r * w + c;
                    down.push(c < w && md.as_ref().is_some_and(|m| m[k]));
                    left.push(c < w && ml.as_ref().is_some_and(|m| m[k]));
                }
            }
        }
        let header_rows = parts[0]
            .e
            .header_rows
            .map(|_| header.iter().take_while(|h| **h).count());
        canonical(grid, down, left, header_rows, sub_headers, restated)
    }

    /// As a Markdown pipe table says it. An empty header row is how a pipe
    /// table says it has none, so a header row with nothing in it reads back
    /// as no row and no header: there is nothing in it to lose.
    fn in_pipes(mut self) -> Table {
        let pipes = self.spans.is_empty() && self.sub_headers.is_empty();
        if !pipes
            || self.header_rows != Some(1)
            || self.rows == 0
            || self.cells[..self.cols].iter().any(|c| !c.is_empty())
        {
            return self;
        }
        self.cells.drain(..self.cols);
        self.rows -= 1;
        self.header_rows = Some(0);
        self.restated = self
            .restated
            .iter()
            .filter_map(|r| r.checked_sub(1))
            .collect();
        self
    }

    fn excerpt(&self) -> String {
        excerpt(
            &self
                .cells
                .iter()
                .filter(|c| !c.is_empty())
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | "),
        )
    }
}

/// The canonical form of a row-major grid and its merge flags.
fn canonical(
    grid: Vec<Vec<String>>,
    mut down: Vec<bool>,
    mut left: Vec<bool>,
    header_rows: Option<usize>,
    sub_headers: BTreeSet<usize>,
    restated: BTreeSet<usize>,
) -> Table {
    let rows = grid.len();
    let cols = grid.first().map_or(0, Vec::len);
    let filled = |r: usize, c: usize| !grid[r][c].trim().is_empty();
    let th = header_rows.unwrap_or(0);
    // A band: a section band, or a header row holding its first cell alone.
    let band = |r: usize| sub_headers.contains(&r) || (r < th && (1..cols).all(|c| !filled(r, c)));
    // A cell continued from one above or to its left belongs to that cell's
    // span; one flagged with no span to take it in, holding text, is a cell
    // of its own, as the model reads it.
    loop {
        let mut covered = vec![false; rows * cols];
        for r in 0..rows {
            if band(r) {
                covered[r * cols..(r + 1) * cols].fill(true);
                continue;
            }
            for c in 0..cols {
                let k = r * cols + c;
                if down[k] || left[k] {
                    continue;
                }
                let (rs, cs) = extent(&down, &left, rows, cols, r, c);
                for rr in r..r + rs {
                    covered[rr * cols + c..rr * cols + c + cs].fill(true);
                }
            }
        }
        let lost: Vec<usize> = (0..rows * cols)
            .filter(|&k| !covered[k] && filled(k / cols, k % cols))
            .collect();
        if lost.is_empty() {
            break;
        }
        for k in lost {
            down[k] = false;
            left[k] = false;
        }
    }
    let mut cells = vec![String::new(); rows * cols];
    let mut spans = BTreeMap::new();
    for r in 0..rows {
        if band(r) {
            let text = squeeze(&grid[r].join(" "));
            if cols > 1 && !text.is_empty() {
                spans.insert((r, 0), (1, cols));
            }
            if cols > 0 {
                cells[r * cols] = text;
            }
            continue;
        }
        for c in 0..cols {
            let k = r * cols + c;
            if down[k] || left[k] {
                continue;
            }
            let (rs, cs) = extent(&down, &left, rows, cols, r, c);
            let text = squeeze(
                &(r..r + rs)
                    .flat_map(|rr| grid[rr][c..c + cs].iter())
                    .map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            if (rs > 1 || cs > 1) && !text.is_empty() {
                spans.insert((r, c), (rs, cs));
            }
            cells[k] = text;
        }
    }
    Table {
        rows,
        cols,
        cells,
        spans,
        header_rows,
        sub_headers,
        restated,
    }
}

/// How many rows and columns the span starting at `(r, c)` covers.
fn extent(
    down: &[bool],
    left: &[bool],
    rows: usize,
    cols: usize,
    r: usize,
    c: usize,
) -> (usize, usize) {
    let rs = (r + 1..rows).take_while(|&rr| down[rr * cols + c]).count() + 1;
    let cs = (c + 1..cols).take_while(|&cc| left[r * cols + cc]).count() + 1;
    (rs, cs)
}

// ---------------------------------------------------------------------------
// Alignment and comparison.

/// How far ahead alignment looks for an element's partner.
const WINDOW: usize = 64;
/// The share of two elements' characters they hold in common for one to be
/// read as the other, changed, rather than as two elements.
const SAME_ELEMENT: f64 = 0.5;

struct Checker<'a> {
    format: Format,
    a: &'a [Unit],
    b: &'a [Unit],
    out: Vec<Violation>,
    /// Units of `E`, and of the read-back, that found no partner.
    lost: Vec<usize>,
    extra: Vec<usize>,
}

impl Checker<'_> {
    fn push(&mut self, class: &'static str, element: Option<usize>, detail: String) {
        self.out.push(Violation {
            format: self.format,
            class,
            element,
            detail,
        });
    }

    fn run(&mut self) {
        let (a, b) = (self.a, self.b);
        let (mut gi, mut gj) = (0, 0);
        for (i, j) in anchors(a, b) {
            self.gap(gi..i, gj..j);
            self.compare(i, j);
            gi = i + 1;
            gj = j + 1;
        }
        self.gap(gi..a.len(), gj..b.len());
        self.settle();
    }

    /// Pair the units between two anchors: an element split in two, two run
    /// together, one changed, or one lost.
    fn gap(&mut self, ga: std::ops::Range<usize>, gb: std::ops::Range<usize>) {
        let (a, b) = (self.a, self.b);
        let (mut x, mut y) = (ga.start, gb.start);
        while x < ga.end && y < gb.end {
            let (ua, ub) = (&a[x], &b[y]);
            // One element read as several.
            if ub.key.len() < ua.key.len() && ua.key.starts_with(&ub.key) {
                if let Some(z) = runs_to(&ua.key, b, y, gb.end) {
                    self.push(
                        "block.split",
                        Some(ua.at),
                        format!("read as {} elements: {}", z - y, excerpt(&ua.text)),
                    );
                    x += 1;
                    y = z;
                    continue;
                }
            }
            // Several read as one.
            if ua.key.len() < ub.key.len() && ub.key.starts_with(&ua.key) {
                if let Some(z) = runs_to(&ub.key, a, x, ga.end) {
                    self.push(
                        "block.merged",
                        Some(ua.at),
                        format!("{} elements read as one: {}", z - x, excerpt(&ub.text)),
                    );
                    x = z;
                    y += 1;
                    continue;
                }
            }
            if (ua.kind == Kind::Table) == (ub.kind == Kind::Table)
                && similarity(&ua.key, &ub.key) >= SAME_ELEMENT
            {
                self.compare(x, y);
                x += 1;
                y += 1;
                continue;
            }
            // A table read as the blocks of its cells' text, or blocks read
            // as a table.
            if ua.kind == Kind::Table && ub.kind != Kind::Table {
                let z = absorbed(&ua.key, b, y, gb.end);
                if z > y {
                    self.push(
                        "table.lost",
                        Some(ua.at),
                        format!("read as {} blocks: {}", z - y, excerpt(&ua.text)),
                    );
                    x += 1;
                    y = z;
                    continue;
                }
            }
            if ub.kind == Kind::Table && ua.kind != Kind::Table {
                let z = absorbed(&ub.key, a, x, ga.end);
                if z > x {
                    self.push(
                        "table.extra",
                        Some(ua.at),
                        format!("{} blocks read as a table: {}", z - x, excerpt(&ub.text)),
                    );
                    x = z;
                    y += 1;
                    continue;
                }
            }
            // The read-back's unit has a partner a little further on: the
            // ones before it are extra. Otherwise this one of `E` is lost.
            let ahead = (y + 1..gb.end.min(y + 4)).find(|&k| {
                (b[k].kind == Kind::Table) == (ua.kind == Kind::Table)
                    && similarity(&ua.key, &b[k].key) >= SAME_ELEMENT
            });
            match ahead {
                Some(k) => {
                    self.extra.extend(y..k);
                    y = k;
                }
                None => {
                    self.lost.push(x);
                    x += 1;
                }
            }
        }
        self.lost.extend(x..ga.end);
        self.extra.extend(y..gb.end);
    }

    /// Lost and extra units that are the same text are one element moved.
    fn settle(&mut self) {
        let (a, b) = (self.a, self.b);
        let mut extra_by_key: HashMap<&str, Vec<usize>> = HashMap::new();
        for &k in &self.extra {
            extra_by_key.entry(b[k].key.as_str()).or_default().push(k);
        }
        let mut moved = BTreeSet::new();
        let lost = std::mem::take(&mut self.lost);
        for x in lost {
            let ua = &a[x];
            if let Some(k) = extra_by_key.get_mut(ua.key.as_str()).and_then(|v| v.pop()) {
                moved.insert(k);
                self.push(
                    "order",
                    Some(ua.at),
                    format!("moved: {}", excerpt(&ua.text)),
                );
                continue;
            }
            let class = match ua.kind {
                Kind::Table => "table.lost",
                _ => "text.lost",
            };
            self.push(class, Some(ua.at), excerpt(&ua.text));
        }
        let extra = std::mem::take(&mut self.extra);
        for y in extra {
            if moved.contains(&y) {
                continue;
            }
            let ub = &b[y];
            let class = match ub.kind {
                Kind::Table => "table.extra",
                _ => "text.extra",
            };
            self.push(class, None, excerpt(&ub.text));
        }
    }

    /// Hold a pair, `E`'s unit `x` and the read-back's `y`, to the contract.
    fn compare(&mut self, x: usize, y: usize) {
        let (ua, ub) = (&self.a[x], &self.b[y]);
        let at = Some(ua.at);
        match (&ua.table, &ub.table) {
            (Some(ta), Some(tb)) => return self.tables(ua.at, ta, tb),
            (Some(_), None) => {
                return self.push(
                    "table.lost",
                    at,
                    format!("read as a {}: {}", ub.kind.name(), excerpt(&ua.text)),
                )
            }
            (None, Some(_)) => {
                return self.push(
                    "table.extra",
                    at,
                    format!(
                        "a {} read as a table: {}",
                        ua.kind.name(),
                        excerpt(&ua.text)
                    ),
                )
            }
            (None, None) => {}
        }
        // Text a format read as markup, a paragraph's `1. ` read as a list's
        // number or its `# ` as a heading's, is the kind's violation alone.
        let markup = matches!(
            (ua.kind, ub.kind),
            (Kind::Para | Kind::Heading(_), Kind::Item(_))
                | (Kind::Para | Kind::Item(_), Kind::Heading(_))
        );
        let expected = if markup {
            key_of(strip_marker(&ua.text))
        } else {
            ua.key.clone()
        };
        if expected != ub.key {
            let (missing, added) = difference(&expected, &ub.key);
            let added: String = added.chars().filter(|c| !ua.may_add.contains(*c)).collect();
            if !missing.is_empty() || !added.is_empty() {
                self.push(
                    "text.changed",
                    at,
                    format!(
                        "{}{}{}: {}",
                        if missing.is_empty() {
                            String::new()
                        } else {
                            format!("lost {}", clip(&missing))
                        },
                        if !missing.is_empty() && !added.is_empty() {
                            ", "
                        } else {
                            ""
                        },
                        if added.is_empty() {
                            String::new()
                        } else {
                            format!("gained {}", clip(&added))
                        },
                        excerpt(&ua.text)
                    ),
                );
            }
        }
        match (ua.kind, ub.kind) {
            (Kind::Heading(l), Kind::Heading(m)) if l != m => self.push(
                "heading.level",
                at,
                format!("{l} → {m}: {}", excerpt(&ua.text)),
            ),
            (Kind::Heading(_), Kind::Heading(_)) => {}
            (Kind::Heading(_), k) => self.push(
                "heading.lost",
                at,
                format!("read as a {}: {}", k.name(), excerpt(&ua.text)),
            ),
            (k, Kind::Heading(_)) => self.push(
                "heading.extra",
                at,
                format!("a {} read as a heading: {}", k.name(), excerpt(&ua.text)),
            ),
            (Kind::Item(d), Kind::Item(e)) if d != e => self.push(
                "list.depth",
                at,
                format!("{d} → {e}: {}", excerpt(&ua.text)),
            ),
            (Kind::Item(_), Kind::Item(_)) => {}
            (Kind::Item(_), k) => self.push(
                "list.lost",
                at,
                format!("read as a {}: {}", k.name(), excerpt(&ua.text)),
            ),
            (k, Kind::Item(_)) => self.push(
                "list.extra",
                at,
                format!("a {} read as a list item: {}", k.name(), excerpt(&ua.text)),
            ),
            _ => {}
        }
    }

    fn tables(&mut self, at: usize, a: &Table, b: &Table) {
        let at_ = Some(at);
        if (a.rows, a.cols) != (b.rows, b.cols) {
            return self.push(
                "table.shape",
                at_,
                format!(
                    "{}×{} → {}×{}: {}",
                    a.rows,
                    a.cols,
                    b.rows,
                    b.cols,
                    a.excerpt()
                ),
            );
        }
        let differ: Vec<usize> = (0..a.cells.len())
            .filter(|&k| key_of(&a.cells[k]) != key_of(&b.cells[k]))
            .collect();
        if let Some(&k) = differ.first() {
            self.push(
                "table.grid",
                at_,
                format!(
                    "{} cells differ; ({}, {}) {:?} → {:?}: {}",
                    differ.len(),
                    k / a.cols,
                    k % a.cols,
                    excerpt(&a.cells[k]),
                    excerpt(&b.cells[k]),
                    a.excerpt()
                ),
            );
        }
        if let Some(h) = a.header_rows {
            if b.header_rows != Some(h) {
                self.push(
                    "table.header_rows",
                    at_,
                    format!("{h} → {}: {}", b.header_rows.unwrap_or(0), a.excerpt()),
                );
            }
        }
        if a.spans != b.spans {
            let lost = a
                .spans
                .iter()
                .filter(|(k, v)| b.spans.get(k) != Some(v))
                .count();
            let gained = b
                .spans
                .iter()
                .filter(|(k, v)| a.spans.get(k) != Some(v))
                .count();
            self.push(
                "table.spans",
                at_,
                format!("{lost} lost, {gained} gained: {}", a.excerpt()),
            );
        }
        if a.sub_headers != b.sub_headers {
            self.push(
                "table.sub_headers",
                at_,
                format!("{:?} → {:?}: {}", a.sub_headers, b.sub_headers, a.excerpt()),
            );
        }
        if a.restated != b.restated {
            self.push(
                "table.restated_headers",
                at_,
                format!("{:?} → {:?}: {}", a.restated, b.restated, a.excerpt()),
            );
        }
    }
}

/// The pairs of units whose text matches exactly, in order on both sides.
///
/// First the texts each side holds once, the longest run of them in the same
/// order on both (a moved element breaks the run and is no anchor); then,
/// between those, matches found looking a little ahead on either side.
fn anchors(a: &[Unit], b: &[Unit]) -> Vec<(usize, usize)> {
    let mut seen: HashMap<&str, (usize, usize, usize)> = HashMap::new();
    for (i, u) in a.iter().enumerate() {
        let e = seen.entry(u.key.as_str()).or_insert((0, 0, i));
        e.0 += 1;
        e.2 = i;
    }
    for u in b {
        if let Some(e) = seen.get_mut(u.key.as_str()) {
            e.1 += 1;
        }
    }
    let unique: Vec<(usize, usize)> = b
        .iter()
        .enumerate()
        .filter_map(|(j, u)| {
            let (na, nb, i) = *seen.get(u.key.as_str())?;
            (na == 1 && nb == 1).then_some((i, j))
        })
        .collect();
    let spine = increasing(&unique);
    let mut out = Vec::new();
    let (mut i0, mut j0) = (0, 0);
    for &(i, j) in spine.iter().chain(std::iter::once(&(a.len(), b.len()))) {
        out.extend(nearby(a, b, i0..i, j0..j));
        if i < a.len() {
            out.push((i, j));
        }
        i0 = i + 1;
        j0 = j + 1;
    }
    out
}

/// The longest run of `pairs` (in order of their second index) whose first
/// indices increase too.
fn increasing(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    // Patience sorting: `tops[k]` ends the best run of length k + 1 found.
    let mut tops: Vec<usize> = Vec::new();
    let mut back: Vec<Option<usize>> = vec![None; pairs.len()];
    for (n, &(i, _)) in pairs.iter().enumerate() {
        let k = tops.partition_point(|&t| pairs[t].0 < i);
        back[n] = k.checked_sub(1).map(|p| tops[p]);
        if k == tops.len() {
            tops.push(n);
        } else {
            tops[k] = n;
        }
    }
    let mut run = Vec::new();
    let mut at = tops.last().copied();
    while let Some(n) = at {
        run.push(pairs[n]);
        at = back[n];
    }
    run.reverse();
    run
}

/// Matches between two stretches, each unit's partner looked for a little
/// ahead on the other side.
fn nearby(
    a: &[Unit],
    b: &[Unit],
    ra: std::ops::Range<usize>,
    rb: std::ops::Range<usize>,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut i, mut j) = (ra.start, rb.start);
    // A text repeated nearby is matched to one of its own kind.
    let same = |x: &Unit, y: &Unit| x.key == y.key && x.kind == y.kind;
    while i < ra.end && j < rb.end {
        if same(&a[i], &b[j]) {
            out.push((i, j));
            i += 1;
            j += 1;
            continue;
        }
        let later_b = (j + 1..rb.end.min(j + WINDOW)).find(|&k| same(&a[i], &b[k]));
        let later_a = (i + 1..ra.end.min(i + WINDOW)).find(|&k| same(&a[k], &b[j]));
        match (later_a, later_b) {
            (Some(ka), Some(kb)) if ka - i <= kb - j => i = ka,
            (Some(_), Some(kb)) => j = kb,
            (Some(ka), None) => i = ka,
            (None, Some(kb)) => j = kb,
            (None, None) => {
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// Each list item's depth read as its nesting: the number of items before
/// it, in its run of items, that are shallower and still open. An item one
/// deeper than the one before opens a list inside it and a shallower one
/// closes the lists deeper than it, so a list opening deeper than any item
/// before it is nested in nothing: no format can set it deeper.
fn nest(units: &mut [Unit]) {
    let mut open: Vec<usize> = Vec::new();
    let mut prev: Option<usize> = None;
    for (n, u) in units.iter_mut().enumerate() {
        let Kind::Item(d) = u.kind else {
            continue;
        };
        // A run of items is broken by anything between them.
        if prev.is_none_or(|p| p + 1 != n) || u.run_start {
            open.clear();
        }
        while open.last().is_some_and(|&o| o >= d) {
            open.pop();
        }
        u.kind = Kind::Item(open.len());
        open.push(d);
        prev = Some(n);
    }
}

/// Where a run of `units` from `from` spells `key` exactly, the index after
/// it; it takes two units at least.
fn runs_to(key: &str, units: &[Unit], from: usize, end: usize) -> Option<usize> {
    let mut at = 0;
    let mut z = from;
    while z < end && at < key.len() {
        let k = &units[z].key;
        if !key[at..].starts_with(k.as_str()) {
            return None;
        }
        at += k.len();
        z += 1;
    }
    (at == key.len() && z - from >= 2).then_some(z)
}

/// How far a run of blocks from `from` holds nothing but characters of
/// `key`, a table's cells: the index after the run.
fn absorbed(key: &str, units: &[Unit], from: usize, end: usize) -> usize {
    let mut left = multiset(key);
    let mut z = from;
    while z < end && units[z].kind != Kind::Table {
        let m = multiset(&units[z].key);
        if m.iter()
            .any(|(c, n)| left.get(c).copied().unwrap_or(0) < *n)
        {
            break;
        }
        for (c, n) in m {
            *left.get_mut(&c).unwrap() -= n;
        }
        z += 1;
    }
    z
}

fn multiset(s: &str) -> HashMap<char, usize> {
    let mut m = HashMap::new();
    for c in s.chars() {
        *m.entry(c).or_insert(0) += 1;
    }
    m
}

/// The share of characters two texts hold in common.
fn similarity(a: &str, b: &str) -> f64 {
    let (ma, mb) = (multiset(a), multiset(b));
    let common: usize = ma
        .iter()
        .map(|(c, n)| (*n).min(mb.get(c).copied().unwrap_or(0)))
        .sum();
    let most = a.chars().count().max(b.chars().count()).max(1);
    common as f64 / most as f64
}

/// The characters of `a` not in `b`, and of `b` not in `a`, each in the
/// order its text has them.
fn difference(a: &str, b: &str) -> (String, String) {
    let one_way = |x: &str, y: &str| {
        let mut have = multiset(y);
        x.chars()
            .filter(|c| match have.get_mut(c) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    false
                }
                _ => true,
            })
            .collect::<String>()
    };
    (one_way(a, b), one_way(b, a))
}

fn clip(s: &str) -> String {
    let n = s.chars().count();
    if n <= 24 {
        format!("{s:?}")
    } else {
        format!("{:?}… ({n})", s.chars().take(24).collect::<String>())
    }
}

fn excerpt(s: &str) -> String {
    if s.chars().count() <= 60 {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(60).collect::<String>())
    }
}

#[cfg(test)]
mod tests;
