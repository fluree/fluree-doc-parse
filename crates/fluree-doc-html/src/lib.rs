//! HTML to DoCO-typed document elements.
//!
//! HTML declares its structure, but unlike Markdown or OOXML it also carries
//! a great deal that is not document content — navigation, scripts, styling
//! wrappers — and real-world markup is frequently malformed. So the parse is
//! spec-compliant (Servo's html5ever) and the walk is selective: non-content
//! subtrees are dropped whole, and only elements that name a document role
//! are emitted. Text the page does not draw is left out with them, and a
//! page that marks its main content is read for that alone.
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

use fluree_doc_model::{DocumentInfo, Element, Link};
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};

/// Subtrees that never carry document content. A `<form>` is not one of
/// them: older pages, and every ASP.NET page, wrap the whole body in one, so
/// only its controls are dropped.
const SKIP: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "head", "nav", "iframe", "canvas", "button",
    "select", "textarea", "datalist", "dialog",
];

/// Landmark roles that are never the document's text, wherever they sit: a
/// navigation block, a search box, a dialog. The banner and contentinfo
/// roles are not among them: mail templates put the sender's name and
/// address in a `contentinfo` footer the reader sees, and a page's own
/// banner and footer already fall outside its `<main>`.
const CHROME_ROLES: &[&str] = &["navigation", "search", "dialog", "alertdialog"];

/// Class names the common style sheets (Bootstrap, WordPress, HTML5
/// Boilerplate) give text that is kept for screen readers and never drawn.
const UNDRAWN_CLASSES: &[&str] = &[
    "sr-only",
    "visually-hidden",
    "visuallyhidden",
    "screen-reader-text",
    "screen-reader-only",
];

/// Does the page leave this subtree undrawn? The `hidden` attribute (except
/// `until-found`, which a find-in-page reveals), an inline `display:none` or
/// `visibility:hidden` — email preheaders are set that way — and the
/// screen-reader-only classes, whose text sits inside a visible phrase
/// ("Read more(opens in a new window)") and would join its words.
fn undrawn(h: &Handle) -> bool {
    if attr_of(h, "hidden").is_some_and(|v| !v.trim().eq_ignore_ascii_case("until-found")) {
        return true;
    }
    if let Some(style) = attr_of(h, "style") {
        let style: String = style
            .to_ascii_lowercase()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if style.contains("display:none") || style.contains("visibility:hidden") {
            return true;
        }
    }
    attr_of(h, "class").is_some_and(|c| {
        c.split_whitespace()
            .any(|c| UNDRAWN_CLASSES.iter().any(|u| c.eq_ignore_ascii_case(u)))
    })
}

/// The ARIA role an element states, its first token lowercased.
fn role_of(h: &Handle) -> Option<String> {
    attr_of(h, "role")?
        .split_whitespace()
        .next()
        .map(|r| r.to_ascii_lowercase())
}

/// Is this element furniture? A landmark role can say so, and so does an
/// `<aside>` that belongs to the page rather than to an article or section —
/// the rule by which browsers make it a complementary landmark: a sidebar of
/// other stories, not a factbox inside the story.
fn is_chrome(h: &Handle) -> bool {
    if let Some(role) = role_of(h) {
        return CHROME_ROLES.contains(&role.as_str());
    }
    tag_of(h).as_deref() == Some("aside") && !has_ancestor(h, &["article", "section"])
}

fn has_ancestor(h: &Handle, tags: &[&str]) -> bool {
    let mut at = h.parent.take();
    h.parent.set(at.clone());
    while let Some(weak) = at {
        let Some(p) = weak.upgrade() else { break };
        if tag_of(&p).is_some_and(|t| tags.contains(&t.as_str())) {
            return true;
        }
        at = p.parent.take();
        p.parent.set(at.clone());
    }
    false
}

/// Is this subtree left out of the reading: never content, undrawn, or the
/// page's furniture?
fn skipped(h: &Handle) -> bool {
    tag_of(h).is_some_and(|t| SKIP.contains(&t.as_str())) || undrawn(h) || is_chrome(h)
}

/// Parse an HTML document into elements in reading order.
///
/// A page that declares no headings but shows them by keyword and number,
/// as a statute or a contract does (`CHAPTER IV`, `Article 30`), has them
/// read as section titles: see [`fluree_doc_model::outline`]. A mail body,
/// read through [`parse_with_quote_depth`], is not.
pub fn parse(src: &str) -> Vec<Element> {
    let mut elements = parse_with_quote_depth(src).0;
    if fluree_doc_model::outline::infer_headings(&mut elements) {
        number(&mut elements);
    }
    elements
}

fn number(elements: &mut [Element]) {
    for (i, e) in elements.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
}

/// Decode an HTML file's bytes. A byte-order mark decides; then UTF-8, when
/// the bytes are valid UTF-8; then the charset the page declares; then
/// Windows-1252.
///
/// Valid UTF-8 is taken before the declaration because a saved page is often
/// re-encoded by whatever fetched it while its `<meta>` still names the old
/// charset, and text in a legacy encoding that uses any byte above ASCII is
/// almost never valid UTF-8 by accident. Labels resolve as browsers resolve
/// them, so `iso-8859-1` is read as Windows-1252, which has the curly quotes
/// pages declared as Latin-1 actually use.
pub fn decode(bytes: &[u8]) -> String {
    if let Some((enc, bom)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc
            .decode_without_bom_handling(&bytes[bom..])
            .0
            .into_owned();
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    let enc = declared_charset(bytes)
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        // A page cannot declare itself UTF-16 from inside its own bytes.
        .filter(|e| *e != encoding_rs::UTF_16LE && *e != encoding_rs::UTF_16BE)
        .unwrap_or(encoding_rs::WINDOWS_1252);
    enc.decode_without_bom_handling(bytes).0.into_owned()
}

/// The `charset=` a page declares near its top, as written.
fn declared_charset(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(4096)];
    let lower: Vec<u8> = head.iter().map(|b| b.to_ascii_lowercase()).collect();
    let at = lower.windows(8).position(|w| w == b"charset=")? + 8;
    let label: String = lower[at..]
        .iter()
        .skip_while(|b| matches!(b, b'"' | b'\'' | b' '))
        .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.'))
        .map(|b| *b as char)
        .collect();
    (!label.is_empty()).then_some(label)
}

/// The Windows-1252 character a C1 control stands for. Text that holds one
/// was decoded as Latin-1 when it was Windows-1252, the commonest mislabelling
/// on the web: `l\u{92}art` is `l’art`. HTML never means a C1 control in
/// text, and the five codes Windows-1252 leaves undefined stay as they are.
fn repair_c1(c: char) -> char {
    let n = c as u32;
    if !(0x80..=0x9f).contains(&n) {
        return c;
    }
    let byte = [n as u8];
    let (s, _, _) = encoding_rs::WINDOWS_1252.decode(&byte);
    s.chars().next().filter(|r| *r != '\u{fffd}').unwrap_or(c)
}

/// Parse an HTML document into elements in reading order, with how many
/// quotations of another text enclose each one.
///
/// A quotation here is one the markup marks as cited, as mail clients mark
/// a reply's quoted history: a `<blockquote>` with `type="cite"`, a `cite`
/// address, or a class naming a quote (`gmail_quote`), and the containers
/// Gmail, Yahoo and Proton put around one. A plain `<blockquote>` is left
/// out, since Gmail's indent button writes one with no quotation in it.
pub fn parse_with_quote_depth(src: &str) -> (Vec<Element>, Vec<usize>) {
    let dom = html5ever::parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut src.as_bytes())
        .unwrap_or_default();
    let mut out = Out::default();
    // A page that marks its main content is read for that alone: what lies
    // outside it is the site around the document. One whose main holds no
    // text has marked it wrongly, and is read whole.
    let mut mains = Vec::new();
    find_mains(&dom.document, &mut mains);
    mains.retain(|m| !text_of(m).0.is_empty());
    if mains.is_empty() {
        walk(&dom.document, &mut out);
    } else {
        for m in &mains {
            walk(m, &mut out);
        }
    }
    number(&mut out.elements);
    (out.elements, out.depths)
}

/// The outermost `<main>` elements, or elements in the `main` role, that the
/// page draws.
fn find_mains(h: &Handle, out: &mut Vec<Handle>) {
    for c in h.children.borrow().iter() {
        if tag_of(c).is_none() || skipped(c) {
            continue;
        }
        if tag_of(c).as_deref() == Some("main") || role_of(c).as_deref() == Some("main") {
            out.push(c.clone());
        } else {
            find_mains(c, out);
        }
    }
}

/// What the walk emits: the elements, and beside each the number of cited
/// quotations open where it was read.
#[derive(Default)]
struct Out {
    elements: Vec<Element>,
    depths: Vec<usize>,
    depth: usize,
}

impl Out {
    fn push(&mut self, e: Element) {
        self.elements.push(e);
        self.depths.push(self.depth);
    }
}

/// Does this element mark its content as quoted from another text?
fn is_citation(h: &Handle) -> bool {
    let class = attr_of(h, "class").unwrap_or_default().to_ascii_lowercase();
    match tag_of(h).as_deref() {
        Some("blockquote") => {
            attr_of(h, "type").is_some_and(|t| t.eq_ignore_ascii_case("cite"))
                || attr_of(h, "cite").is_some()
                || class.contains("quote")
        }
        Some("div") => class.split_whitespace().any(|c| {
            matches!(
                c,
                "gmail_quote" | "gmail_quote_container" | "yahoo_quoted" | "protonmail_quote"
            )
        }),
        _ => false,
    }
}

/// An element node's lowercase tag name, or `None` for text and the rest.
/// What a page declares about itself in its `<head>`: its `<title>` and a
/// `<meta name="author">`. Read from the head alone, because a `<title>`
/// in the body is an SVG drawing's.
pub fn info(src: &str) -> DocumentInfo {
    let dom = html5ever::parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut src.as_bytes())
        .unwrap_or_default();
    let mut info = DocumentInfo::default();
    let html = children_of(&dom.document)
        .into_iter()
        .find(|h| tag_of(h).as_deref() == Some("html"));
    let head = html
        .iter()
        .flat_map(children_of)
        .find(|h| tag_of(h).as_deref() == Some("head"));
    for h in head.iter().flat_map(children_of) {
        match tag_of(&h).as_deref() {
            Some("title") if info.title.is_none() => {
                info.title = Some(text_of(&h).0).filter(|t| !t.is_empty());
            }
            Some("meta") => {
                let named_author =
                    attr_of(&h, "name").is_some_and(|n| n.eq_ignore_ascii_case("author"));
                let content = attr_of(&h, "content")
                    .map(|c| c.split_whitespace().collect::<Vec<_>>().join(" "))
                    .filter(|c| !c.is_empty());
                if let (true, Some(c)) = (named_author, content) {
                    info.creators.push(c);
                }
            }
            _ => {}
        }
    }
    info
}

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
        level,
        ..Element::new(kind, text, "html", "html")
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

/// Endings an ordinal sets raised after its number: `1st`, `2e`, `1er`, `3º`.
const ORDINALS: &[&str] = &[
    "st", "nd", "rd", "th", "e", "er", "re", "ère", "eme", "ème", "o", "a", "º", "ª",
];

/// Is a `<sup>` holding `mark`, set after the character `before`, a note's
/// mark rather than part of the word?
///
/// Read into the word, a note's mark changes it: `April 2026` revised
/// becomes `April 2026r`, and a footnoted `Acrylonitrile` becomes
/// `Acrylonitrilea`. Raised text that belongs to the word stays in it: an
/// exponent or a unit's power (`10⁶`, `m²`) and an ordinal's ending (`1st`,
/// `1er`). So a mark is a symbol (`*`, `†`), letters after a word or after a
/// number they are not the ordinal of, or a number after a word too long to
/// be a unit (`Revenue¹`, not `km²`).
fn footnote_mark(text_before: &str, mark: &str) -> bool {
    let mark = mark.trim();
    let Some(before) = text_before.chars().last().filter(|c| !c.is_whitespace()) else {
        return false;
    };
    if mark.is_empty() || mark.chars().count() > 4 {
        return false;
    }
    if mark.chars().all(|c| "*†‡§¶#".contains(c)) {
        return true;
    }
    if mark.chars().all(char::is_alphabetic) {
        return !(before.is_ascii_digit() && ORDINALS.contains(&mark.to_lowercase().as_str()));
    }
    let word = text_before.rsplit(char::is_whitespace).next().unwrap_or("");
    mark.chars().all(|c| c.is_ascii_digit() || c == ',')
        && word.chars().all(char::is_alphabetic)
        && word.chars().count() >= 4
}

/// One node's contribution to the text [`collect`] gathers.
fn collect_node(node: &Handle, out: &mut String, anchors: &mut Vec<(usize, usize, String)>) {
    match &node.data {
        NodeData::Text { contents } => out.push_str(&contents.borrow()),
        NodeData::Element { .. } => {
            let Some(tag) = tag_of(node) else { return };
            if skipped(node) {
                return;
            }
            if matches!(tag.as_str(), "br" | "td" | "th" | "li" | "p" | "div") {
                out.push(' ');
            }
            if tag == "sup" && footnote_mark(out, &text_of(node).0) {
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
    "form",
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
        Some(_) if skipped(c) => false,
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
fn flush_run(run: &mut Vec<Handle>, out: &mut Out) {
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
fn walk_container(h: &Handle, out: &mut Out) {
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
                if skipped(child) || matches!(tag.as_str(), "ul" | "ol" | "table") {
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
    for c in raw.chars().map(repair_c1) {
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
    let links = spans(&at, anchors);
    (out, links)
}

/// The anchors located in the text a raw concatenation became, given where
/// each raw character landed (`None` for one the text left out).
fn spans(at: &[Option<usize>], anchors: &[(usize, usize, String)]) -> Vec<Link> {
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
    links
}

/// A `<pre>`'s text as it is drawn, with any `<a href>` inside it located.
///
/// In a `<pre>` the line breaks, the blank lines and the runs of spaces are
/// the content (HTML, the `pre` element): a hearing transcript or a public
/// law is published as one, its letterhead centred and its witness list set
/// in columns by spaces alone. So nothing is collapsed. A `<br>` is a line
/// break, and a block set inside one starts a line of its own.
fn pre_text(h: &Handle) -> (String, Vec<Link>) {
    let mut raw = Raw::default();
    let mut anchors = Vec::new();
    collect_pre(h, &mut raw, &mut anchors);
    verbatim(&raw.text, &anchors)
}

/// Text gathered in order, with its length in characters kept as it grows:
/// a `<pre>` can run to hundreds of thousands of characters and hold an
/// anchor on every line.
#[derive(Default)]
struct Raw {
    text: String,
    chars: usize,
}

impl Raw {
    fn push_str(&mut self, s: &str) {
        self.text.push_str(s);
        self.chars += s.chars().count();
    }

    /// Start a new line unless one has just begun.
    fn break_line(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.push_str("\n");
        }
    }
}

fn collect_pre(h: &Handle, out: &mut Raw, anchors: &mut Vec<(usize, usize, String)>) {
    for node in h.children.borrow().iter() {
        match &node.data {
            NodeData::Text { contents } => out.push_str(&contents.borrow()),
            NodeData::Element { .. } => {
                let Some(tag) = tag_of(node) else { continue };
                if skipped(node) {
                    continue;
                }
                if tag == "br" {
                    out.push_str("\n");
                    continue;
                }
                let block = is_block(&tag);
                if block {
                    out.break_line();
                }
                let start = out.chars;
                collect_pre(node, out, anchors);
                if tag == "a" {
                    if let Some(href) = attr_of(node, "href").filter(|h| !h.trim().is_empty()) {
                        anchors.push((start, out.chars, href));
                    }
                }
                if block {
                    out.break_line();
                }
            }
            _ => {}
        }
    }
}

/// Preformatted text as written, carrying the anchor offsets through.
///
/// Only what is never drawn goes: the whitespace before the first character
/// and after the last, as every element's text is trimmed, and the spaces a
/// line ends in. A tab stays a tab, since it sets a column; any other space
/// character, a no-break space among them, is a space.
fn verbatim(raw: &str, anchors: &[(usize, usize, String)]) -> (String, Vec<Link>) {
    let chars: Vec<char> = raw
        .chars()
        .map(repair_c1)
        .map(|c| match c {
            '\n' | '\t' => c,
            c if c.is_whitespace() => ' ',
            c => c,
        })
        .collect();
    let Some(first) = chars.iter().position(|c| !c.is_whitespace()) else {
        return (String::new(), Vec::new());
    };
    let last = chars
        .iter()
        .rposition(|c| !c.is_whitespace())
        .unwrap_or(first);
    let mut keep = vec![false; chars.len()];
    // Walked backwards, so a run of spaces knows whether a line break or
    // the end follows it.
    let mut line_end = true;
    for i in (first..=last).rev() {
        match chars[i] {
            '\n' => {
                keep[i] = true;
                line_end = true;
            }
            ' ' | '\t' if line_end => {}
            _ => {
                keep[i] = true;
                line_end = false;
            }
        }
    }
    let mut out = String::with_capacity(raw.len());
    let mut at = Vec::with_capacity(chars.len());
    let mut n = 0usize;
    for (c, k) in chars.iter().zip(&keep) {
        if *k {
            out.push(*c);
            at.push(Some(n));
            n += 1;
        } else {
            at.push(None);
        }
    }
    (out, spans(&at, anchors))
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

fn walk(h: &Handle, out: &mut Out) {
    if is_citation(h) {
        out.depth += 1;
        walk_node(h, out);
        out.depth -= 1;
    } else {
        walk_node(h, out);
    }
}

fn walk_node(h: &Handle, out: &mut Out) {
    if let Some(tag) = tag_of(h) {
        let t = tag.as_str();
        if skipped(h) {
            return;
        }
        if t == "table" {
            if is_layout(h) {
                walk_container(h, out);
            } else {
                // The caption is where a table states its subject or unit
                // (`Price list, prices in EUR`); it reads before the table.
                for c in children_of(h) {
                    if tag_of(&c).as_deref() == Some("caption") {
                        let (text, links) = text_of(&c);
                        if !text.is_empty() {
                            out.push(linked("doco:Paragraph", text, None, links));
                        }
                    }
                }
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
                    out.push(linked("doc:ListItem", own, None, links));
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
            // One paragraph, its lines and spacing kept, as a Markdown code
            // block is read.
            "pre" => {
                let (text, links) = pre_text(h);
                if !text.is_empty() {
                    out.push(linked("doco:Paragraph", text, None, links));
                }
                return;
            }
            "p" | "blockquote" | "figcaption" | "dd" | "dt" => {
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
            Some(_) if skipped(c) => {}
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

/// What one cell of a row says about the row.
struct CellMark {
    /// It holds text.
    filled: bool,
    th: bool,
    /// A `<td>` set as a header is: bold throughout, or ruled beneath.
    styled: bool,
}

/// What a table row is, for finding where the header ends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowKind {
    /// No text in any cell: a spacer.
    Blank,
    /// Its text is all in `<th>` cells. An empty `<td>` may stand before
    /// them, the corner over the row labels.
    Th { cells: usize },
    /// Its text is in two cells or more, each a `<th>` or a `<td>` set as a
    /// header: the header row a filer's table, an SEC inline-XBRL
    /// statement among them, writes in `<td>`.
    Styled,
    /// Values: text in a `<td>` not set as a header.
    Data,
    /// Anything else: a row of one label, set bold (`Revenues`), or a
    /// `<th>` label beside empty `<td>` values.
    Other,
}

fn row_kind(marks: &[CellMark]) -> RowKind {
    let filled: Vec<&CellMark> = marks.iter().filter(|m| m.filled).collect();
    if filled.is_empty() {
        return RowKind::Blank;
    }
    if filled.iter().any(|m| !m.th && !m.styled) {
        return RowKind::Data;
    }
    let first_th = marks.iter().position(|m| m.th);
    let empty_td_after_th = first_th.is_some_and(|f| marks[f..].iter().any(|m| !m.th));
    if filled.iter().all(|m| m.th) && !(filled.len() == 1 && empty_td_after_th) {
        return RowKind::Th {
            cells: filled.len(),
        };
    }
    if filled.len() >= 2 {
        return RowKind::Styled;
    }
    RowKind::Other
}

/// How many rows from the top are the header.
///
/// HTML states its header, in `<th>` and `<thead>`, but not always both, and
/// not always all of it:
/// - With no `<thead>`, the header is the leading rows whose text is all in
///   `<th>`, an empty `<td>` corner over the row labels notwithstanding.
/// - A `<thead>` ends the header, except that rows of several `<th>` at the
///   top of the body go on with it: a statistical table sets its units
///   (`thousands of units`, `% change`) there, under the periods. A single
///   `<th>` across the table there is a band over the rows beneath it, not
///   a header row.
/// - A filer's table sets its header in `<td>` cells, bold or ruled
///   beneath, and nothing else so. Such leading rows are the header when
///   rows of plain values follow.
///
/// Blank spacer rows inside the header are part of it; the header ends at
/// its last row of labels.
fn header_rows(kinds: &[RowKind], thead: usize) -> usize {
    let values_follow = |from: usize| kinds[from..].contains(&RowKind::Data);
    let mut end = thead;
    for (i, kind) in kinds.iter().enumerate().skip(thead) {
        let header = match kind {
            RowKind::Blank => continue,
            RowKind::Th { cells } => thead == 0 || *cells >= 2,
            RowKind::Styled => values_follow(i + 1),
            RowKind::Data | RowKind::Other => false,
        };
        if !header {
            break;
        }
        end = i + 1;
    }
    end
}

/// Is this cell set as a header: ruled beneath, or bold throughout?
fn header_styled(cell: &Handle) -> bool {
    ruled_below(cell) || bold_throughout(cell, false) == Some(true)
}

/// An element's inline style, lowercase and without whitespace.
fn style_of(h: &Handle) -> String {
    attr_of(h, "style")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// Does the cell draw a rule along its bottom edge: `border-bottom: 1px
/// solid`, or `border-bottom-style: solid`? A rule on every side is a
/// grid's, and says nothing of a header.
fn ruled_below(cell: &Handle) -> bool {
    const LINES: &[&str] = &[
        "solid", "double", "dotted", "dashed", "groove", "ridge", "inset", "outset",
    ];
    style_of(cell).split(';').any(|d| match d.split_once(':') {
        Some(("border-bottom", v)) => {
            !v.is_empty() && !v.contains("none") && !v.contains("hidden") && !v.starts_with('0')
        }
        Some(("border-bottom-style", v)) => LINES.contains(&v),
        _ => false,
    })
}

fn bold_style(h: &Handle) -> bool {
    let style = style_of(h);
    style.split(';').any(|d| {
        d.strip_prefix("font-weight:").is_some_and(|w| {
            matches!(w, "bold" | "bolder") || w.parse::<u32>().is_ok_and(|n| n >= 600)
        })
    })
}

/// Is every word under this node set bold? `None` where it holds no text.
fn bold_throughout(h: &Handle, inherited: bool) -> Option<bool> {
    let bold = inherited || bold_style(h) || matches!(tag_of(h).as_deref(), Some("b" | "strong"));
    let mut seen = None;
    for c in h.children.borrow().iter() {
        let here = match &c.data {
            NodeData::Text { contents } if !contents.borrow().trim().is_empty() => Some(bold),
            NodeData::Element { .. } if !skipped(c) => bold_throughout(c, bold),
            _ => None,
        };
        match here {
            Some(false) => return Some(false),
            Some(true) => seen = Some(true),
            None => {}
        }
    }
    seen
}

/// Build the flat grid, carrying `colspan` / `rowspan` into the model's merge
/// flags — the same convention DOCX's gridSpan/vMerge map to and the PDF
/// engine derives from ruling.
fn emit_table(table: &Handle, out: &mut Out) {
    let mut trs = Vec::new();
    rows_under(table, &mut trs);

    let mut rows: Vec<Vec<(String, usize, usize)>> = Vec::new();
    let mut kinds: Vec<RowKind> = Vec::new();
    for tr in &trs {
        let mut cells = Vec::new();
        let mut marks = Vec::new();
        for cell in children_of(tr) {
            let th = match tag_of(&cell).as_deref() {
                Some("th") => true,
                Some("td") => false,
                _ => continue,
            };
            // A table cell's text becomes a grid position, which has no
            // room for an anchor span; the link is dropped with it.
            let text = text_of(&cell).0;
            marks.push(CellMark {
                filled: !text.is_empty(),
                th,
                styled: !th && !text.is_empty() && header_styled(&cell),
            });
            cells.push((text, num_attr(&cell, "colspan"), num_attr(&cell, "rowspan")));
        }
        if cells.is_empty() {
            continue;
        }
        kinds.push(row_kind(&marks));
        rows.push(cells);
    }
    if rows.is_empty() {
        return;
    }
    let mut thead_trs = Vec::new();
    for c in children_of(table) {
        if tag_of(&c).as_deref() == Some("thead") {
            rows_under(&c, &mut thead_trs);
        }
    }
    let thead = thead_trs
        .iter()
        .filter(|tr| {
            children_of(tr)
                .iter()
                .any(|c| matches!(tag_of(c).as_deref(), Some("td" | "th")))
        })
        .count()
        .min(rows.len());
    let header_rows = header_rows(&kinds, thead);
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
    // Rows that are one cell across the whole width, as laid out.
    let mut full_width = vec![false; n];

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
            full_width[r] = cells.len() == 1 && c == 0 && cs == width && width > 1;
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
    // A body row that is one cell across the whole table labels the rows
    // beneath it — `<td colspan="4">Food</td>` over the dishes — and is not
    // itself a row of data. The last row has nothing beneath it to label.
    let header_rows = header_rows.min(n);
    let sub_headers: Vec<usize> = (header_rows..n.saturating_sub(1))
        .filter(|&r| full_width[r] && !cells[r][0].trim().is_empty())
        .collect();
    let mut e = element("doco:Table", text, None);
    e.header_rows = Some(header_rows);
    e.sub_headers = (!sub_headers.is_empty()).then_some(sub_headers);
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
    fn a_page_declares_its_title_and_author_in_its_head() {
        let info = info(
            "<html><head><title>Quarterly\n  report</title>\
             <meta name=\"Author\" content=\"Ada Park\"></head>\
             <body><svg><title>a chart</title></svg><p>Text</p></body></html>",
        );
        assert_eq!(info.title.as_deref(), Some("Quarterly report"));
        assert_eq!(info.creators, ["Ada Park"]);
        // A drawing's title is not the page's.
        let bare = super::info("<p>Text</p><svg><title>a chart</title></svg>");
        assert_eq!(bare.title, None);
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
                "doc:ListItem",
                "doc:ListItem"
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
    fn cited_quotations_are_counted_and_an_indent_is_not() {
        let (els, depths) = parse_with_quote_depth(
            "<div>Reply</div><blockquote type=\"cite\"><div>Quoted</div>\
             <div class=\"gmail_quote\"><div>Attribution</div>\
             <blockquote class=\"gmail_quote\"><p>Deeper</p></blockquote></div>\
             <div>Back</div></blockquote>\
             <blockquote style=\"margin:0 0 0 40px\"><p>Indented</p></blockquote>",
        );
        assert_eq!(
            texts(&els),
            [
                "Reply",
                "Quoted",
                "Attribution",
                "Deeper",
                "Back",
                "Indented"
            ]
        );
        assert_eq!(depths, [0, 1, 2, 3, 1, 0]);
    }

    #[test]
    fn a_pre_keeps_its_lines_blank_lines_and_spacing() {
        // How a hearing transcript or a public law is published: the whole
        // document one `<pre>`, its letterhead and addresses set by spaces.
        let els = parse(
            "<body><pre>\nCOMMITTEE ON ENERGY AND COMMERCE\n\nWashington, DC 20515\n\n\
             \x20   Dear Chairman Example:\n\n    Line with   three   spaces.\n</pre></body>",
        );
        assert_eq!(kinds(&els), ["doco:Paragraph"]);
        assert_eq!(
            els[0].text,
            "COMMITTEE ON ENERGY AND COMMERCE\n\nWashington, DC 20515\n\n\
             \x20   Dear Chairman Example:\n\n    Line with   three   spaces."
        );
        // Text outside a `<pre>` still flows.
        let p = parse("<p>one\n\n   two</p><pre>a\n  b</pre><p>three</p>");
        assert_eq!(texts(&p), ["one two", "a\n  b", "three"]);
    }

    #[test]
    fn a_pre_keeps_its_links_columns_and_breaks() {
        let e = parse(
            "<pre>Name\tRoom   \nAda Park\t214\nwrite to <a href=\"mailto:a@example.org\">the\n  clerk</a>\
             <br>l\u{92}art\u{a0}x  \n\n\n</pre>",
        );
        // A tab sets a column and stays; the spaces a line ends in are not
        // drawn; a `<br>` breaks the line; a no-break space is a space.
        assert_eq!(
            e[0].text,
            "Name\tRoom\nAda Park\t214\nwrite to the\n  clerk\nl\u{2019}art x"
        );
        let l = &e[0].links.as_ref().expect("links")[0];
        let (b, end) = l.span().unwrap();
        let anchor: String = e[0].text.chars().skip(b).take(end - b).collect();
        assert_eq!(anchor, "the\n  clerk");
        assert_eq!(l.href(), "mailto:a@example.org");
        // A block set inside one starts a line of its own.
        let d = parse("<pre><div>a</div><div>b</div>c</pre>");
        assert_eq!(d[0].text, "a\nb\nc");
    }

    #[test]
    fn a_statute_without_heading_markup_reads_as_sections() {
        // How the Official Journal publishes a regulation: its outline is in
        // classes and identifiers of its own, with no heading element.
        let els = parse(
            "<div id=\"cpt_IV\"><p class=\"oj-ti-section-1\"><span class=\"oj-italic\">CHAPTER IV</span></p>\
             <div class=\"eli-title\"><p class=\"oj-ti-section-2\">Controller and processor</p></div>\
             <div class=\"eli-subdivision\" id=\"art_30\"><p class=\"oj-ti-art\">Article\u{a0}30</p>\
             <div class=\"eli-title\"><p class=\"oj-sti-art\">Records of processing activities</p></div>\
             <p class=\"oj-normal\">1.\u{a0}\u{a0}\u{a0}Each controller shall maintain a record.</p></div></div>",
        );
        assert_eq!(
            kinds(&els),
            ["doco:SectionTitle", "doco:SectionTitle", "doco:Paragraph"]
        );
        assert_eq!(
            texts(&els)[..2],
            [
                "CHAPTER IV Controller and processor",
                "Article 30 Records of processing activities"
            ]
        );
        assert_eq!((els[0].level, els[1].level), (Some(1), Some(2)));
        let ids: Vec<&str> = els.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["elem-00001", "elem-00002", "elem-00003"]);
        // A page that declares its headings keeps them as declared.
        let declared = parse("<h1>Terms</h1><p>Article 1</p><p>Scope</p>");
        assert_eq!(
            kinds(&declared),
            ["doco:SectionTitle", "doco:Paragraph", "doco:Paragraph"]
        );
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

    fn header(html: &str) -> (Option<usize>, Vec<Vec<String>>) {
        let els = parse(html);
        let t = els
            .iter()
            .find(|e| e.kind == "doco:Table")
            .expect("a table");
        (t.header_rows, t.cells.clone().unwrap())
    }

    #[test]
    fn a_units_row_of_th_under_the_thead_goes_on_with_the_header() {
        let (n, cells) = header(
            "<table><thead><tr><th></th><th>March<br/>2026</th><th>April<br/>2026<sup>r</sup></th>\
             <th>March to April 2026</th></tr></thead><tbody>\
             <tr><th></th><th>thousands of units</th><th>thousands of units</th><th>% change</th></tr>\
             <tr><th>North</th><td>1,204</td><td>1,250</td><td>3.8</td></tr>\
             <tr><th>South</th><td>980</td><td>1,011</td><td>3.2</td></tr></tbody></table>",
        );
        assert_eq!(n, Some(2));
        // A revised mark is a note on the header, not part of its year.
        assert_eq!(cells[0][2], "April 2026 r");
    }

    #[test]
    fn an_empty_td_corner_does_not_make_a_header_row_data() {
        let (n, _) = header(
            "<table><tr><td></td><th colspan=\"2\">12-month rate (%)</th><th colspan=\"2\">1-month rate (%)</th></tr>\
             <tr><td></td><th>May 2026</th><th>June 2026</th><th>May 2026</th><th>June 2026</th></tr>\
             <tr><th>All items</th><td>3.6</td><td>3.7</td><td>0.4</td><td>0.5</td></tr>\
             <tr><th>Food</th><td>2.1</td><td>2.4</td><td>0.2</td><td>0.3</td></tr></table>",
        );
        assert_eq!(n, Some(2));
        // A label beside empty values is a row, not a header.
        let (n, _) = header(
            "<table><tr><th>Region</th><td></td><td></td></tr>\
             <tr><th>North</th><td>1</td><td>2</td></tr><tr><th>South</th><td>3</td><td>4</td></tr></table>",
        );
        assert_eq!(n, Some(0));
    }

    #[test]
    fn a_filers_header_in_bold_or_ruled_td_cells_is_the_header() {
        let ruled = "font-weight:bold;border-bottom:1px solid #000";
        let (n, _) = header(&format!(
            "<table><tr><td style=\"{ruled}\">Region</td><td style=\"{ruled}\">2024</td>\
             <td style=\"{ruled}\">2023</td></tr>\
             <tr><td>North</td><td>120</td><td>110</td></tr><tr><td>South</td><td>95</td><td>90</td></tr></table>"
        ));
        assert_eq!(n, Some(1));
        // As an SEC statement sets it: two rows of bold labels, spans and a
        // spacer row, then a bold band over the values, which is not header.
        let (n, cells) = header(
            "<table><tr><td></td><td colspan=\"2\" style=\"font-weight:bold\">Three months ended</td>\
               <td colspan=\"2\" style=\"font-weight:bold\">Nine months ended</td></tr>\
             <tr><td></td><td></td><td></td><td></td><td></td></tr>\
             <tr><td><b>(in millions)</b></td><td><b>2018</b></td><td><b>2017</b></td>\
               <td style=\"border-bottom: 1px solid\">2018</td><td style=\"border-bottom-style:solid\">2017</td></tr>\
             <tr><td><b>Revenues</b></td><td></td><td></td><td></td><td></td></tr>\
             <tr><td>Premiums</td><td>321.1</td><td>282.8</td><td>914.5</td><td>750.6</td></tr></table>",
        );
        assert_eq!(n, Some(3));
        assert_eq!(cells[3][0], "Revenues");
        // Bold everywhere is emphasis, not a header; so is a grid's rule on
        // every cell.
        for html in [
            "<table><tr><td><b>A</b></td><td><b>1</b></td></tr><tr><td><b>B</b></td><td><b>2</b></td></tr></table>",
            "<table><tr><td style=\"border-bottom:1px solid\">A</td><td style=\"border-bottom:1px solid\">1</td></tr>\
             <tr><td style=\"border-bottom:1px solid\">B</td><td style=\"border-bottom:1px solid\">2</td></tr></table>",
            "<table><tr><td style=\"border-bottom:none\">A</td><td style=\"border-bottom-color:#000\">1</td></tr>\
             <tr><td>B</td><td>2</td></tr></table>",
        ] {
            assert_eq!(header(html).0, Some(0), "{html}");
        }
    }

    #[test]
    fn a_raised_note_mark_stands_apart_and_a_power_does_not() {
        let t = |html: &str| parse(html)[0].text.clone();
        assert_eq!(
            t("<p>Acrylonitrile<sup>a</sup> and Revenue<sup>1</sup></p>"),
            "Acrylonitrile a and Revenue 1"
        );
        assert_eq!(t("<p>Net income<sup>*</sup></p>"), "Net income *");
        assert_eq!(
            t("<p>10 m<sup>2</sup>, 10<sup>6</sup>, the 1<sup>st</sup> and 2<sup>e</sup></p>"),
            "10 m2, 106, the 1st and 2e"
        );
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
    fn a_full_width_body_row_is_a_section_band() {
        let els = parse(
            "<table><tr><th>Item</th><th>Qty</th><th>Price</th></tr>\
               <tr><td colspan=\"3\">Food</td></tr>\
               <tr><td>Fish pie</td><td>2</td><td>14.50</td></tr>\
               <tr><th colspan=\"3\">Drinks</th></tr>\
               <tr><td>Lemonade</td><td></td><td>3.20</td></tr>\
               <tr><td colspan=\"3\">Thank you</td></tr>\
             </table>",
        );
        let t = &els[0];
        assert_eq!(t.header_rows, Some(1));
        // A row with a missing value is data; the last row labels nothing.
        assert_eq!(t.sub_headers, Some(vec![1, 3]));
    }

    #[test]
    fn a_th_band_in_the_body_is_a_band_not_a_header_row() {
        // `<th colspan scope=rowgroup>` at the top of `<tbody>`: the header
        // ends where `<thead>` does.
        let els = parse(
            "<table><thead><tr><th>Name</th><th>Ext.</th></tr></thead><tbody>\
               <tr><th colspan=\"2\" scope=\"rowgroup\">Finance</th></tr>\
               <tr><td>Ana Ruiz</td><td>214</td></tr>\
               <tr><th colspan=\"2\" scope=\"rowgroup\">Logistics</th></tr>\
               <tr><td>Bo Lind</td><td>220</td></tr>\
             </tbody></table>",
        );
        let t = &els[0];
        assert_eq!(t.header_rows, Some(1));
        assert_eq!(t.sub_headers, Some(vec![1, 3]));
    }

    #[test]
    fn a_caption_reads_before_its_table() {
        let els = parse(
            "<table><caption>Price list, prices in EUR</caption>\
               <tr><th>Code</th><th>Price</th></tr><tr><td>PX-210</td><td>1,250.00</td></tr>\
             </table>",
        );
        assert_eq!(els[0].kind, "doco:Paragraph");
        assert_eq!(els[0].text, "Price list, prices in EUR");
        assert_eq!(els[1].kind, "doco:Table");
        let cells = els[1].cells.as_ref().unwrap();
        assert!(cells.iter().flatten().all(|c| !c.contains("Price list")));
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

    #[test]
    fn a_form_around_the_page_is_read_and_its_controls_are_not() {
        // ASP.NET and older pages wrap the whole body in one form.
        let els = parse(
            "<body><form action=\"/x\"><p>The story.</p><input name=\"q\">\
             <button>Search</button><select><option>One</option></select>\
             <textarea>draft</textarea></form></body>",
        );
        assert_eq!(texts(&els), vec!["The story."]);
    }

    #[test]
    fn undrawn_text_is_left_out() {
        let els = parse(
            "<div style=\"display: none; max-height:0\">Preheader</div>\
             <p>Read <a href=\"/a\">the report<span class=\"sr-only\">(opens in a new window)</span></a> now.</p>\
             <p hidden>Gone</p><p style=\"VISIBILITY:hidden\">Gone too</p>\
             <p hidden=\"until-found\">Folded</p><p aria-hidden=\"true\">Drawn</p>",
        );
        assert_eq!(texts(&els), vec!["Read the report now.", "Folded", "Drawn"]);
        assert_eq!(els[0].links.as_ref().unwrap()[0].span(), Some((5, 15)));
    }

    #[test]
    fn a_page_with_a_main_is_read_for_it_alone() {
        let els = parse(
            "<body><header><p>Site name</p></header><div>Menu</div>\
             <main><h1>Story</h1><p>Body.</p></main><footer><p>Copyright</p></footer></body>",
        );
        assert_eq!(texts(&els), vec!["Story", "Body."]);
        // A main with no text in it was marked wrongly; the page is read whole.
        let els = parse("<body><main> </main><p>Everything</p></body>");
        assert_eq!(texts(&els), vec!["Everything"]);
        let els = parse("<body><div>Nav</div><div role=\"main\"><p>Here</p></div></body>");
        assert_eq!(texts(&els), vec!["Here"]);
    }

    #[test]
    fn a_page_sidebar_is_furniture_and_a_factbox_is_not() {
        let els = parse(
            "<main><article><p>Story.</p><aside><p>Factbox.</p></aside></article>\
             <aside><p>Other stories</p></aside></main>",
        );
        assert_eq!(texts(&els), vec!["Story.", "Factbox."]);
    }

    #[test]
    fn landmark_roles_drop_navigation_and_dialogs_but_keep_a_mail_footer() {
        // Mail templates mark their visible footer, with the sender's address,
        // as contentinfo.
        let els = parse(
            "<div role=\"navigation\">Home</div><div role=\"dialog\">Accept cookies</div>\
             <dialog open>Modal</dialog><p>Text.</p>\
             <div role=\"contentinfo\"><p>Acme Ltd, 1 High St</p></div>",
        );
        assert_eq!(texts(&els), vec!["Text.", "Acme Ltd, 1 High St"]);
    }

    #[test]
    fn windows_1252_read_as_latin_1_is_repaired() {
        let els = parse("<p>l\u{92}art \u{93}cit\u{e9}\u{94} \u{81}</p>");
        assert_eq!(els[0].text, "l\u{2019}art \u{201c}cit\u{e9}\u{201d} \u{81}");
    }

    #[test]
    fn bytes_decode_by_mark_then_utf8_then_the_declared_charset() {
        let latin = b"<meta charset=\"iso-8859-1\"><p>l\x92\xe9t\xe9</p>";
        assert_eq!(
            decode(latin),
            "<meta charset=\"iso-8859-1\"><p>l\u{2019}\u{e9}t\u{e9}</p>"
        );
        // Re-saved as UTF-8 under its old declaration: the bytes win.
        let resaved = "<meta http-equiv=\"Content-Type\" content=\"text/html; charset=iso-8859-1\"><p>été</p>";
        assert_eq!(decode(resaved.as_bytes()), resaved);
        assert_eq!(decode(b"\xef\xbb\xbf<p>x</p>"), "<p>x</p>");
        // No declaration and not UTF-8: Windows-1252.
        assert_eq!(decode(b"<p>\x93q\x94</p>"), "<p>\u{201c}q\u{201d}</p>");
    }
}
