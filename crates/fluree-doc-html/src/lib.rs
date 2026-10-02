//! HTML to DoCO-typed document elements.
//!
//! HTML declares its structure, but unlike Markdown or OOXML it also carries
//! a great deal that is not document content — navigation, scripts, styling
//! wrappers — and real-world markup is frequently malformed. So the parse is
//! spec-compliant (Servo's html5ever) and the walk is selective: non-content
//! subtrees are dropped whole, and only elements that name a document role
//! are emitted.
//!
//! Nesting is resolved by *innermost wins*. A `<p>` inside a `<td>` inside a
//! `<table>` is table content, not a paragraph, and emitting both would
//! duplicate the text — the same double-emission the PDF engine guards
//! against when a grid's glyphs would also become prose.
//!
//! html5ever is used directly rather than through a wrapper offering CSS
//! selectors. The reader needs one query — "the `tr` elements under this
//! table" — which is a descendant walk, and the selector engine behind such a
//! wrapper (`selectors`, `cssparser`) is the only copyleft that would enter
//! the dependency tree. Same parser, four fewer crates, no MPL.
//!
//! There is no geometry: HTML positions nothing until it is laid out, so
//! `bbox` is `None` rather than a zeroed box.

use fluree_doc_model::{Element, Link};
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};

/// Subtrees that never carry document content.
const SKIP: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "head", "nav", "iframe", "canvas", "form",
];

/// Parse an HTML document into elements in reading order.
pub fn parse(src: &str) -> Vec<Element> {
    let dom = html5ever::parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut src.as_bytes())
        .unwrap_or_default();
    let mut out = Vec::new();
    walk(&dom.document, &mut out);
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    out
}

/// An element node's lowercase tag name, or `None` for text and the rest.
fn tag_of(h: &Handle) -> Option<String> {
    match &h.data {
        NodeData::Element { name, .. } => Some(name.local.to_ascii_lowercase().to_string()),
        _ => None,
    }
}

fn attr_of(h: &Handle, want: &str) -> Option<String> {
    let NodeData::Element { attrs, .. } = &h.data else {
        return None;
    };
    let attrs = attrs.borrow();
    attrs.iter().find_map(|a| {
        a.name
            .local
            .as_ref()
            .eq_ignore_ascii_case(want)
            .then(|| a.value.to_string())
    })
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
        provenance: "html",
        evidence: "html",
    }
}

/// All text under a node, whitespace-collapsed, with any `<a href>` inside it
/// located. Inline markup (`<em>`, `<a>`, `<span>`) is transparent: it styles
/// a phrase, it does not divide one — but an `<a>` also says something about
/// the phrase it styles, and that survives here as a span.
fn text_of(h: &Handle) -> (String, Vec<Link>) {
    let mut raw = String::new();
    let mut anchors = Vec::new();
    collect(h, &mut raw, &mut anchors);
    squeeze(&raw, &anchors)
}

fn collect(h: &Handle, out: &mut String, anchors: &mut Vec<(usize, usize, String)>) {
    for child in h.children.borrow().iter() {
        collect_node(child, out, anchors);
    }
}

/// One node's contribution to the text [`collect`] gathers.
fn collect_node(node: &Handle, out: &mut String, anchors: &mut Vec<(usize, usize, String)>) {
    match &node.data {
        NodeData::Text { contents } => out.push_str(&contents.borrow()),
        NodeData::Element { .. } => {
            let Some(tag) = tag_of(node) else { return };
            if SKIP.contains(&tag.as_str()) {
                return;
            }
            if matches!(tag.as_str(), "br" | "td" | "th" | "li" | "p" | "div") {
                out.push(' ');
            }
            // Recorded around the recursion: an anchor's text is whatever
            // its subtree contributes, and nesting is legal markup.
            let start = out.chars().count();
            collect(node, out, anchors);
            if tag == "a" {
                if let Some(href) = attr_of(node, "href").filter(|h| !h.trim().is_empty()) {
                    anchors.push((start, out.chars().count(), href));
                }
            }
        }
        _ => {}
    }
}

/// Elements that lay out as blocks. Everything else, text included, flows
/// inline within the nearest one.
const BLOCK: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "caption",
    "center",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "html",
    "legend",
    "li",
    "main",
    "menu",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "ul",
];

fn is_block(tag: &str) -> bool {
    BLOCK.contains(&tag) || SKIP.contains(&tag)
}

/// Does anything under this node lay out as a block?
fn holds_block(h: &Handle) -> bool {
    h.children.borrow().iter().any(|c| match tag_of(c) {
        Some(t) if SKIP.contains(&t.as_str()) => false,
        Some(t) => is_block(&t) || holds_block(c),
        None => false,
    })
}

/// Content that flows inline: text, and elements with no block inside.
fn is_inline(h: &Handle) -> bool {
    match tag_of(h) {
        Some(t) => !is_block(&t) && !holds_block(h),
        None => true,
    }
}

/// A paragraph from a run of inline siblings, when it has any text.
fn flush_run(run: &mut Vec<Handle>, out: &mut Vec<Element>) {
    let mut raw = String::new();
    let mut anchors = Vec::new();
    for node in run.drain(..) {
        collect_node(&node, &mut raw, &mut anchors);
    }
    let (text, links) = squeeze(&raw, &anchors);
    if !text.is_empty() {
        out.push(linked("doco:Paragraph", text, None, links));
    }
}

/// Walk a container's children, reading each run of inline content set
/// directly in it as a paragraph.
///
/// A page built of `<div>`s, and nearly every email, sets its text straight
/// in the container with no `<p>` around it, which a walk that emits only
/// named blocks drops whole. A browser lays each such run out in an
/// anonymous block, and so does this: runs end at a block child, and at two
/// `<br>` in a row, which is how that markup separates paragraphs.
fn walk_container(h: &Handle, out: &mut Vec<Element>) {
    let mut run: Vec<Handle> = Vec::new();
    let mut last_br = false;
    for c in children_of(h) {
        if !is_inline(&c) {
            flush_run(&mut run, out);
            last_br = false;
            walk(&c, out);
            continue;
        }
        let blank =
            matches!(&c.data, NodeData::Text { contents } if contents.borrow().trim().is_empty());
        if tag_of(&c).as_deref() == Some("br") {
            if last_br {
                flush_run(&mut run, out);
                last_br = false;
                continue;
            }
            last_br = true;
        } else if !blank {
            last_br = false;
        }
        run.push(c);
    }
    flush_run(&mut run, out);
}

/// An element's text excluding nested block containers, so a list item that
/// holds a sub-list contributes only its own label.
fn own_text(h: &Handle) -> (String, Vec<Link>) {
    let mut raw = String::new();
    let mut anchors = Vec::new();
    for child in h.children.borrow().iter() {
        match &child.data {
            NodeData::Text { contents } => raw.push_str(&contents.borrow()),
            NodeData::Element { .. } => {
                let Some(tag) = tag_of(child) else { continue };
                if SKIP.contains(&tag.as_str()) || matches!(tag.as_str(), "ul" | "ol" | "table") {
                    continue;
                }
                raw.push(' ');
                let start = raw.chars().count();
                collect(child, &mut raw, &mut anchors);
                if tag == "a" {
                    if let Some(href) = attr_of(child, "href").filter(|h| !h.trim().is_empty()) {
                        anchors.push((start, raw.chars().count(), href));
                    }
                }
            }
            _ => {}
        }
    }
    squeeze(&raw, &anchors)
}

/// Collapse whitespace the way the readers always have, carrying the anchor
/// offsets through the collapse.
///
/// The offsets are taken against the raw concatenation, and normalising it
/// deletes characters — so they cannot simply be copied across. Each raw
/// character keeps the position it lands on, and an anchor's range becomes the
/// first and last positions its characters occupy.
fn squeeze(raw: &str, anchors: &[(usize, usize, String)]) -> (String, Vec<Link>) {
    let mut out = String::new();
    let mut at: Vec<Option<usize>> = Vec::with_capacity(raw.len());
    let (mut n, mut pending) = (0usize, false);
    for c in raw.chars() {
        if c.is_whitespace() {
            pending = n > 0;
            at.push(None);
            continue;
        }
        if pending {
            out.push(' ');
            n += 1;
            pending = false;
        }
        at.push(Some(n));
        out.push(c);
        n += 1;
    }
    let mut links: Vec<Link> = anchors
        .iter()
        .filter_map(|(b, e, href)| {
            let range = at.get(*b..(*e).min(at.len()))?;
            let first = range.iter().flatten().next()?;
            let last = range.iter().flatten().next_back()?;
            Some(Link::uri(href.clone()).spanning(*first, last + 1))
        })
        .collect();
    // Emitters splice in one pass, so the spans have to arrive ordered and
    // disjoint. Nested anchors are invalid markup but do occur; the outer one
    // is the one the document drew.
    links.sort_by_key(|l| (l.begin.unwrap_or(0), std::cmp::Reverse(l.end.unwrap_or(0))));
    let mut end = 0usize;
    links.retain(|l| match l.span() {
        Some((b, e)) if b >= end => {
            end = e;
            true
        }
        _ => false,
    });
    (out, links)
}

/// An element carrying whatever links its text contained.
fn linked(kind: &str, text: String, level: Option<usize>, links: Vec<Link>) -> Element {
    let mut e = element(kind, text, level);
    if !links.is_empty() {
        e.links = Some(links);
    }
    e
}

fn heading_level(tag: &str) -> Option<usize> {
    let b = tag.as_bytes();
    (b.len() == 2 && b[0] == b'h' && (b'1'..=b'6').contains(&b[1])).then(|| (b[1] - b'0') as usize)
}

fn children_of(h: &Handle) -> Vec<Handle> {
    h.children.borrow().iter().cloned().collect()
}

fn walk(h: &Handle, out: &mut Vec<Element>) {
    if let Some(tag) = tag_of(h) {
        let t = tag.as_str();
        if SKIP.contains(&t) {
            return;
        }
        if t == "table" {
            if is_layout(h) {
                walk_container(h, out);
            } else {
                emit_table(h, out); // innermost wins: cells own their text
            }
            return;
        }
        if let Some(level) = heading_level(t) {
            let (text, links) = text_of(h);
            if !text.is_empty() {
                out.push(linked("doco:SectionTitle", text, Some(level), links));
            }
            return;
        }
        match t {
            "li" => {
                // A nested list inside an item is walked separately; the
                // item's own text stops at it.
                let (own, links) = own_text(h);
                if !own.is_empty() {
                    out.push(linked("doco:ListItem", own, None, links));
                }
                for c in children_of(h) {
                    if matches!(tag_of(&c).as_deref(), Some("ul" | "ol" | "table")) {
                        walk(&c, out);
                    }
                }
                return;
            }
            // A quotation or a caption that holds paragraphs of its own is a
            // container of them, not one paragraph run together.
            "blockquote" | "figcaption" | "dd" | "dt" if holds_block(h) => {}
            "p" | "pre" | "blockquote" | "figcaption" | "dd" | "dt" => {
                let (text, links) = text_of(h);
                if !text.is_empty() {
                    out.push(linked("doco:Paragraph", text, None, links));
                }
                return;
            }
            _ => {}
        }
    }
    walk_container(h, out);
}

/// Every `tr` under a node, at any depth — `thead`/`tbody`/`tfoot` are
/// transparent, and html5ever inserts a `tbody` even where the source had
/// none. This is the one query the reader needs, as a descendant walk.
fn rows_under(h: &Handle, out: &mut Vec<Handle>) {
    for c in h.children.borrow().iter() {
        match tag_of(c).as_deref() {
            Some("tr") => out.push(c.clone()),
            // Do not descend into a nested table: its rows are its own.
            Some("table") => {}
            _ => rows_under(c, out),
        }
    }
}

/// Is this table laid out rather than tabulated?
///
/// Email is set almost entirely in tables, and older pages often are: a
/// column of boxes, a logo beside a banner, tables inside tables to centre
/// a column. Read as data, a whole message becomes one cell. The test
/// follows the one a browser makes before it tells a screen reader there is
/// a table (Gecko's): the author's word, a header, the shape. Except that a
/// table holding a table, or of one row, is laid out whatever its cells are
/// called, because email frameworks set their columns in `<th>`. A layout
/// table's cells are read as the containers they are.
fn is_layout(table: &Handle) -> bool {
    let role = attr_of(table, "role").map(|r| r.trim().to_ascii_lowercase());
    if matches!(role.as_deref(), Some("presentation" | "none")) {
        return true;
    }
    if holds_table(table) {
        return true;
    }
    let mut trs = Vec::new();
    rows_under(table, &mut trs);
    let widths: Vec<usize> = trs
        .iter()
        .map(|tr| {
            children_of(tr)
                .iter()
                .filter(|c| matches!(tag_of(c).as_deref(), Some("td" | "th")))
                .map(|c| num_attr(c, "colspan"))
                .sum::<usize>()
        })
        .filter(|w| *w > 0)
        .collect();
    if widths.len() <= 1 {
        return true;
    }
    let headed = children_of(table)
        .iter()
        .any(|c| matches!(tag_of(c).as_deref(), Some("caption" | "thead" | "tfoot")))
        || trs.iter().any(|tr| {
            children_of(tr)
                .iter()
                .any(|c| tag_of(c).as_deref() == Some("th"))
        });
    // One column is a stack of boxes, unless a header says it is a list of
    // values.
    !headed && widths.iter().all(|w| *w <= 1)
}

fn holds_table(h: &Handle) -> bool {
    h.children
        .borrow()
        .iter()
        .any(|c| tag_of(c).as_deref() == Some("table") || holds_table(c))
}

fn num_attr(h: &Handle, name: &str) -> usize {
    attr_of(h, name)
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(1)
}

/// Build the flat grid, carrying `colspan` / `rowspan` into the model's merge
/// flags — the same convention DOCX's gridSpan/vMerge map to and the PDF
/// engine derives from ruling.
fn emit_table(table: &Handle, out: &mut Vec<Element>) {
    let mut trs = Vec::new();
    rows_under(table, &mut trs);

    let mut rows: Vec<Vec<(String, usize, usize)>> = Vec::new();
    let mut header_rows = 0usize;
    let mut saw_body = false;
    for tr in &trs {
        let mut cells = Vec::new();
        let mut all_th = true;
        for cell in children_of(tr) {
            match tag_of(&cell).as_deref() {
                Some("th") => {}
                Some("td") => all_th = false,
                _ => continue,
            }
            cells.push((
                // A table cell's text becomes a grid position, which has no
                // room for an anchor span; the link is dropped with it.
                text_of(&cell).0,
                num_attr(&cell, "colspan"),
                num_attr(&cell, "rowspan"),
            ));
        }
        if cells.is_empty() {
            continue;
        }
        // A leading run of all-`th` rows is the header — HTML states it,
        // unlike a PDF where it has to be measured.
        if all_th && !saw_body {
            header_rows += 1;
        } else {
            saw_body = true;
        }
        rows.push(cells);
    }
    if rows.is_empty() {
        return;
    }
    let width = rows
        .iter()
        .map(|r| r.iter().map(|c| c.1).sum::<usize>())
        .max()
        .unwrap_or(0)
        .max(1);
    let n = rows.len();
    let mut grid = vec![String::new(); n * width];
    let mut m_left = vec![false; n * width];
    let mut m_down = vec![false; n * width];
    // Occupancy, so a rowspan from an earlier row displaces later cells the
    // way a browser lays them out.
    let mut taken = vec![false; n * width];

    for (r, cells) in rows.iter().enumerate() {
        let mut c = 0usize;
        for (text, colspan, rowspan) in cells {
            while c < width && taken[r * width + c] {
                c += 1;
            }
            if c >= width {
                break;
            }
            let cs = (*colspan).min(width - c);
            let rs = (*rowspan).min(n - r);
            grid[r * width + c] = text.clone();
            for dr in 0..rs {
                for dc in 0..cs {
                    taken[(r + dr) * width + c + dc] = true;
                    if dc > 0 {
                        m_left[(r + dr) * width + c + dc] = true;
                    }
                    if dr > 0 {
                        m_down[(r + dr) * width + c + dc] = true;
                    }
                }
            }
            c += cs;
        }
    }
    let cells: Vec<Vec<String>> = (0..n)
        .map(|r| grid[r * width..(r + 1) * width].to_vec())
        .collect();
    let text = cells
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = element("doco:Table", text, None);
    e.header_rows = Some(header_rows.min(n));
    e.cells = Some(cells);
    e.merged_left = m_left.iter().any(|x| *x).then_some(m_left);
    e.merged_down = m_down.iter().any(|x| *x).then_some(m_down);
    out.push(e);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(e: &[Element]) -> Vec<&str> {
        e.iter().map(|x| x.kind.as_str()).collect()
    }

    #[test]
    fn headings_paragraphs_and_items() {
        let els =
            parse("<h1>Title</h1><p>Body <em>text</em> here.</p><ul><li>one</li><li>two</li></ul>");
        assert_eq!(
            kinds(&els),
            [
                "doco:SectionTitle",
                "doco:Paragraph",
                "doco:ListItem",
                "doco:ListItem"
            ]
        );
        assert_eq!(els[0].level, Some(1));
        // Inline markup styles a phrase; it does not divide one.
        assert_eq!(els[1].text, "Body text here.");
    }

    fn texts(e: &[Element]) -> Vec<&str> {
        e.iter().map(|x| x.text.as_str()).collect()
    }

    #[test]
    fn text_set_straight_in_a_container_is_read() {
        // How a div-built page, and nearly every email, sets its text.
        let els = parse(
            "<body><div>Hi Ada,<div><br></div><div>The pilot starts <b>in May</b>.</div>\
             <div>Thanks<br>Ben</div>Loose text<p>A paragraph</p>after it</div></body>",
        );
        assert_eq!(
            texts(&els),
            vec![
                "Hi Ada,",
                "The pilot starts in May.",
                "Thanks Ben",
                "Loose text",
                "A paragraph",
                "after it"
            ]
        );
        assert!(kinds(&els).iter().all(|k| *k == "doco:Paragraph"));
    }

    #[test]
    fn two_line_breaks_in_a_row_end_a_paragraph() {
        let els = parse("<div>One line<br>still one<br><br>Two</div>");
        assert_eq!(texts(&els), vec!["One line still one", "Two"]);
    }

    #[test]
    fn a_quotation_holding_paragraphs_is_their_container() {
        let els = parse("<blockquote><p>First.</p><div>Second.</div></blockquote><blockquote>Short one.</blockquote>");
        assert_eq!(texts(&els), vec!["First.", "Second.", "Short one."]);
    }

    #[test]
    fn a_link_in_loose_text_keeps_its_anchor() {
        let els = parse("<div>See <a href=\"https://example.org/q\">the quote</a> today.</div>");
        let l = &els[0].links.as_ref().unwrap()[0];
        assert_eq!(l.span(), Some((4, 13)));
        assert_eq!(l.href(), "https://example.org/q");
    }

    #[test]
    fn non_content_subtrees_are_dropped() {
        let els = parse(
            "<head><title>t</title></head><body><script>var x=1;</script>\
             <style>p{}</style><nav>Home About</nav><p>real</p></body>",
        );
        assert_eq!(els.len(), 1);
        assert_eq!(els[0].text, "real");
    }

    #[test]
    fn table_cells_own_their_text() {
        // A <p> inside a cell must not also emit as a paragraph, or the text
        // appears twice.
        let els = parse(
            "<table><tr><td><p>cell</p></td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>",
        );
        assert_eq!(kinds(&els), ["doco:Table"]);
        assert_eq!(els[0].cells.as_ref().unwrap()[0][0], "cell");
    }

    #[test]
    fn a_layout_table_reads_as_its_cells_content() {
        // How an email sets a message: a centred column inside a frame,
        // with the one data table it carries inside that.
        let els = parse(
            "<table width=\"100%\"><tr><td align=\"center\">\
               <table><tr><td><h2>Your bill</h2></td></tr>\
                 <tr><td>Your balance is ready.<br><br>Details below.</td></tr>\
                 <tr><td><table>\
                   <tr><td>Invoice Date</td><td>Amount Due</td></tr>\
                   <tr><td>03/14/2026</td><td>$1,200.00</td></tr>\
                 </table></td></tr>\
               </table>\
             </td></tr></table>",
        );
        assert_eq!(
            kinds(&els),
            [
                "doco:SectionTitle",
                "doco:Paragraph",
                "doco:Paragraph",
                "doco:Table"
            ]
        );
        assert_eq!(
            texts(&els)[..3],
            ["Your bill", "Your balance is ready.", "Details below."]
        );
        assert_eq!(
            els[3].cells.as_ref().unwrap()[1],
            vec!["03/14/2026", "$1,200.00"]
        );
    }

    #[test]
    fn a_table_is_laid_out_by_its_authors_word_or_its_shape() {
        let one_row = parse("<table><tr><td>Logo</td><td>Banner</td></tr></table>");
        assert_eq!(texts(&one_row), ["Logo", "Banner"]);
        let one_column = parse("<table><tr><td>Top</td></tr><tr><td>Bottom</td></tr></table>");
        assert_eq!(texts(&one_column), ["Top", "Bottom"]);
        let said = parse(
            "<table role=\"presentation\"><tr><td>a</td><td>b</td></tr>\
             <tr><td>c</td><td>d</td></tr></table>",
        );
        assert!(kinds(&said).iter().all(|k| *k == "doco:Paragraph"));
        // A header over a column says it is a list of values.
        let headed = parse("<table><tr><th>Total</th></tr><tr><td>12</td></tr></table>");
        assert_eq!(kinds(&headed), ["doco:Table"]);
    }

    #[test]
    fn columns_set_in_th_are_still_laid_out() {
        // How Foundation for Emails sets a message: each column a `<th>`,
        // each holding a table of its own.
        let els = parse(
            "<table class=\"row\"><tr>\
               <th class=\"columns\"><table><tr><th><p>Hello Ada,</p></th></tr></table></th>\
               <th class=\"columns\"><table><tr><th><p>Your statement is ready.</p></th></tr></table></th>\
             </tr></table>",
        );
        assert_eq!(texts(&els), ["Hello Ada,", "Your statement is ready."]);
        assert!(kinds(&els).iter().all(|k| *k == "doco:Paragraph"));
    }

    #[test]
    fn header_rows_come_from_th() {
        let els = parse(
            "<table><thead><tr><th>Year</th><th>Total</th></tr></thead>\
             <tbody><tr><td>2024</td><td>12</td></tr></tbody></table>",
        );
        let t = &els[0];
        assert_eq!(t.header_rows, Some(1));
        assert_eq!(t.cells.as_ref().unwrap()[1], vec!["2024", "12"]);
    }

    #[test]
    fn colspan_and_rowspan_become_merge_flags() {
        let els = parse(
            "<table>\
               <tr><td colspan=\"2\">Banner</td></tr>\
               <tr><td rowspan=\"2\">Left</td><td>A</td></tr>\
               <tr><td>B</td></tr>\
             </table>",
        );
        let t = &els[0];
        let cells = t.cells.as_ref().unwrap();
        assert_eq!(cells[0][0], "Banner");
        let ml = t.merged_left.as_ref().expect("colspan recorded");
        assert!(ml[1], "second column continues the banner");
        let md = t.merged_down.as_ref().expect("rowspan recorded");
        assert!(md[2 * 2], "row 3 col 0 continues the rowspan cell");
        // The rowspan displaces B into column 1, as a browser would.
        assert_eq!(cells[2][1], "B");
    }

    #[test]
    fn nothing_claims_geometry() {
        let els = parse("<h1>T</h1><p>p</p>");
        assert!(els.iter().all(|e| e.bbox.is_none()));
        assert!(els.iter().all(|e| e.provenance == "html"));
    }

    #[test]
    fn malformed_markup_still_parses() {
        // Unclosed tags are what a spec-compliant parser is here for.
        let els = parse("<p>one<p>two<ul><li>a<li>b</ul>");
        assert!(els.len() >= 4, "got {:?}", kinds(&els));
    }

    #[test]
    fn a_nested_list_does_not_duplicate_its_parent_item() {
        let els = parse("<ul><li>outer<ul><li>inner</li></ul></li></ul>");
        let texts: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["outer", "inner"]);
    }

    #[test]
    fn a_nested_table_keeps_its_own_rows() {
        // A table that holds a table is laid out; the inner one is the grid,
        // and its rows are read once, by it.
        let els = parse(
            "<table><tr><th>Item</th></tr><tr><td>outer\
               <table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>\
             </td></tr></table>",
        );
        assert_eq!(
            kinds(&els),
            ["doco:Paragraph", "doco:Paragraph", "doco:Table"]
        );
        assert_eq!(els[2].cells.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn an_anchor_survives_whitespace_collapse() {
        // The newline and the run of spaces both collapse; the span has to
        // move with them.
        let e = parse("<p>See\n   <a href=\"https://sec.example/x\">the\n filing</a> now.</p>");
        let links = e[0].links.as_ref().expect("links");
        let (b, end) = links[0].span().unwrap();
        let anchor: String = e[0].text.chars().skip(b).take(end - b).collect();
        assert_eq!(e[0].text, "See the filing now.");
        assert_eq!(anchor, "the filing");
        assert_eq!(links[0].href(), "https://sec.example/x");
    }

    #[test]
    fn an_anchor_without_a_destination_is_not_a_link() {
        let e = parse("<p>See <a>the filing</a> now.</p>");
        assert!(e[0].links.is_none());
    }

    #[test]
    fn spans_arrive_ordered_and_disjoint() {
        // Nesting anchors is invalid, and html5ever repairs it by closing the
        // first — so this is two links, and the emitters need them in order
        // and not overlapping to splice in one pass.
        let e = parse(
            "<p><a href=\"https://outer.example\">a <a href=\"https://inner.example\">b</a></a></p>",
        );
        let links = e[0].links.as_ref().expect("links");
        let mut end = 0;
        for l in links {
            let (b, e) = l.span().expect("span");
            assert!(b >= end, "{links:?}");
            end = e;
        }
    }
}
