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
        match &child.data {
            NodeData::Text { contents } => out.push_str(&contents.borrow()),
            NodeData::Element { .. } => {
                let Some(tag) = tag_of(child) else { continue };
                if SKIP.contains(&tag.as_str()) {
                    continue;
                }
                if matches!(tag.as_str(), "br" | "td" | "th" | "li" | "p" | "div") {
                    out.push(' ');
                }
                // Recorded around the recursion: an anchor's text is whatever
                // its subtree contributes, and nesting is legal markup.
                let start = out.chars().count();
                collect(child, out, anchors);
                if tag == "a" {
                    if let Some(href) = attr_of(child, "href").filter(|h| !h.trim().is_empty()) {
                        anchors.push((start, out.chars().count(), href));
                    }
                }
            }
            _ => {}
        }
    }
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
            emit_table(h, out);
            return; // innermost wins: cells own their text
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
    for c in children_of(h) {
        walk(&c, out);
    }
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
        let els = parse("<table><tr><td><p>cell</p></td></tr></table>");
        assert_eq!(kinds(&els), ["doco:Table"]);
        assert_eq!(els[0].cells.as_ref().unwrap()[0][0], "cell");
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
        // rows_under must not steal the inner table's rows for the outer one.
        let els = parse(
            "<table><tr><td>outer\
               <table><tr><td>inner</td></tr></table>\
             </td></tr></table>",
        );
        let tables: Vec<&Element> = els.iter().filter(|e| e.kind == "doco:Table").collect();
        assert_eq!(tables.len(), 1, "the outer table owns the cell");
        assert_eq!(tables[0].cells.as_ref().unwrap().len(), 1);
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
