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
//!
//! What a page makes is spent from one [`Budget`]: the parser's tree, its
//! nodes as the parser makes them; the parser's work where it can be
//! counted, the attributes its tokenizer compares on each tag, counted from
//! the tags before it starts, the elements it walks and the siblings it
//! looks among, and what it does on each formatting element's tag; then as
//! it is made each element with its links, its text counted before it is
//! gathered, a table's grid before it is made (a `colspan` of
//! a billion asks for a billion cells), the cells its spans mark, and what
//! DoCO copies of its cells. Past it, the page is refused ([`TooLarge`]).
//! What nests past [`MAX_DEPTH`] is read beside its ancestor at that depth.
//! The walk that reads the tree is linear in it.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;

use fluree_doc_model::budget::{char_cost, Budget, TooLarge};
use fluree_doc_model::{DocumentInfo, Element, Link};
use html5ever::interface::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{
    BufferQueue, Tag, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use html5ever::tree_builder::{Tracer, TreeBuilder, TreeBuilderOpts};
use html5ever::{Attribute, ExpandedName, LocalName, QualName, TokenizerResult};
use markup5ever_rcdom::{Handle, Node, NodeData, RcDom};

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

/// Is this element, with the tag `tag`, furniture? A landmark role can say
/// so, and so does an `<aside>` that belongs to the page rather than to an
/// article or section (`in_article`, whether one is among its ancestors) —
/// the rule by which browsers make it a complementary landmark: a sidebar
/// of other stories, not a factbox inside the story.
fn is_chrome(h: &Handle, tag: &str, in_article: bool) -> bool {
    if let Some(role) = role_of(h) {
        return CHROME_ROLES.contains(&role.as_str());
    }
    tag == "aside" && !in_article
}

/// Is this element, with the tag `tag`, a subtree left out of the reading:
/// never content, undrawn, or the page's furniture? Asked once of each
/// element, by [`Facts::of`].
fn left_out(h: &Handle, tag: &str, in_article: bool) -> bool {
    SKIP.contains(&tag) || undrawn(h) || is_chrome(h, tag, in_article)
}

/// What the walk asks of an element of the tree, beyond its own tag.
#[derive(Clone, Copy, Default, PartialEq)]
struct Fact {
    /// It is left out of the reading, and what is under it ([`left_out`]).
    skipped: bool,
    /// An element under it, not left out, lays out as a block.
    holds_block: bool,
    /// A table is under it.
    holds_table: bool,
}

/// The [`Fact`] of each element of a page's tree, worked out once for the
/// whole tree before it is walked, in one pass down it and back up.
///
/// Asked as the walk went, each was a walk of its own, of a size the page
/// chooses: whether an element holds a block, down its subtree, at each
/// level of the walk above it (500 `<span>` around a million `<br>x`, a 5
/// MB page, took 54 s, and without the `x` 111 s); and whether an `<aside>`
/// is in an article, up its ancestors (a million of them 500 elements
/// deep, 18 s). Worked out here, the walk is linear in the tree, which the
/// budget counts as the parser makes it ([`Sink`]). Only an element with a
/// fact has an entry: about 70 bytes at the peak, as the table grows (a
/// million `<aside>`, each left out), within the 112 its node is counted at
/// beyond the 144 it takes ([`node_slots`]). The map of the others, most
/// of a page's elements, stays small, and a page of nothing but them reads
/// as fast as it did.
#[derive(Default)]
struct Facts(HashMap<*const Node, Fact, BuildHasherDefault<ByAddress>>);

/// An element on the way down [`Facts::of`]: what it gives the element it
/// is in, once its own children have given theirs.
struct Open {
    node: Handle,
    block: bool,
    table: bool,
    fact: Fact,
}

impl Facts {
    fn of(root: &Handle) -> Self {
        let mut facts = Facts::default();
        // The nodes still to go down to, each with whether it is in an
        // article or a section; `None` where the element last opened is
        // done.
        let mut todo: Vec<Option<(Handle, bool)>> = Vec::new();
        down_to(&mut todo, root, false);
        let mut open: Vec<Open> = Vec::new();
        while let Some(next) = todo.pop() {
            let Some((h, in_article)) = next else {
                let Some(done) = open.pop() else { break };
                if done.fact != Fact::default() {
                    facts.0.insert(Rc::as_ptr(&done.node), done.fact);
                }
                if let Some(parent) = open.last_mut() {
                    parent.fact.holds_block |=
                        !done.fact.skipped && (done.block || done.fact.holds_block);
                    parent.fact.holds_table |= done.table || done.fact.holds_table;
                }
                continue;
            };
            let Some(tag) = tag_of(&h) else { continue };
            let inside = in_article || matches!(tag.as_str(), "article" | "section");
            todo.push(None);
            down_to(&mut todo, &h, inside);
            open.push(Open {
                fact: Fact {
                    skipped: left_out(&h, &tag, in_article),
                    ..Fact::default()
                },
                block: is_block(&tag),
                table: tag == "table",
                node: h,
            });
        }
        facts
    }

    /// `h`'s facts.
    fn of_node(&self, h: &Handle) -> Fact {
        self.0.get(&Rc::as_ptr(h)).copied().unwrap_or_default()
    }

    /// Is this subtree left out of the reading?
    fn skipped(&self, h: &Handle) -> bool {
        self.of_node(h).skipped
    }

    /// Does anything under this node lay out as a block?
    fn holds_block(&self, h: &Handle) -> bool {
        self.of_node(h).holds_block
    }

    /// Is a table under this node?
    fn holds_table(&self, h: &Handle) -> bool {
        self.of_node(h).holds_table
    }

    /// Content that flows inline: text, and elements with no block inside.
    fn is_inline(&self, h: &Handle) -> bool {
        match tag_of(h) {
            Some(t) => !is_block(&t) && !self.holds_block(h),
            None => true,
        }
    }
}

/// Set `h`'s children to be gone down to next, in order, each with
/// `in_article`.
fn down_to(todo: &mut Vec<Option<(Handle, bool)>>, h: &Handle, in_article: bool) {
    let children = h.children.borrow();
    todo.extend(children.iter().rev().map(|c| Some((c.clone(), in_article))));
}

/// Hashes a node by its address, all a [`Facts`] key is: the default
/// hasher took a third of the pass.
#[derive(Default)]
struct ByAddress(u64);

impl Hasher for ByAddress {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.write_u64(u64::from(*b));
        }
    }

    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }

    fn write_u64(&mut self, n: u64) {
        // Spread over the high bits, then folded into the low ones, which
        // an address, aligned, leaves at zero.
        let x = (self.0 ^ n).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.0 = x ^ (x >> 29);
    }
}

/// Parse an HTML document into elements in reading order, or say why it
/// makes more than one document may.
///
/// A page that declares no headings but shows them by keyword and number,
/// as a statute or a contract does (`CHAPTER IV`, `Article 30`), has them
/// read as section titles: see [`fluree_doc_model::outline`]. A mail body,
/// read through [`parse_with_quote_depth`], is not.
pub fn parse(src: &str) -> Result<Vec<Element>, TooLarge> {
    let mut elements = read(src, false)?.0;
    if fluree_doc_model::outline::infer_headings(&mut elements) {
        number(&mut elements);
    }
    Ok(elements)
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
///
/// Read as a message's body, an image the message shows from one of its
/// own parts (`<img src="cid:…">`) is a `doco:Figure` where it is drawn,
/// after the paragraph or table holding it, its text the image's
/// alternative text and its `figure` the `cid:` URL as the message writes
/// it, for the e-mail reader to resolve to the part. An image on the web is
/// not in the message, and is left out as [`parse`] leaves every image out.
pub fn parse_with_quote_depth(src: &str) -> Result<(Vec<Element>, Vec<usize>), TooLarge> {
    read(src, true)
}

/// The content ids of the images a message's HTML shows from its own parts
/// (`<img src="cid:…">`), as [`parse_with_quote_depth`] reads them: the
/// scheme in any case, percent signs decoded, an image the page does not
/// draw left out. What tells which of a message's files its HTML shows. A
/// page too large to read shows none: reading it is refused.
pub fn cid_ids(src: &str) -> Vec<String> {
    dom(src).map(|(dom, _)| ids_in(&dom)).unwrap_or_default()
}

/// What [`parse_message`] reads in a message's HTML body: its elements,
/// how many quotations enclose each, and the content ids it shows.
pub type MessageBody = (Vec<Element>, Vec<usize>, Vec<String>);

/// [`parse_with_quote_depth`] and [`cid_ids`] from one parse of the page:
/// what a message's HTML body is read for.
pub fn parse_message(src: &str) -> Result<MessageBody, TooLarge> {
    let (dom, budget) = dom(src)?;
    let (elements, depths) = read_dom(&dom, budget, true)?;
    Ok((elements, depths, ids_in(&dom)))
}

fn ids_in(dom: &RcDom) -> Vec<String> {
    let facts = Facts::of(&dom.document);
    let mut found = Vec::new();
    for c in dom.document.children.borrow().iter() {
        cid_images(c, &facts, &mut found);
    }
    found.into_iter().map(|(id, _)| id).collect()
}

/// The page's tree, and the budget what is read of it is spent from
/// ([`build`]).
fn dom(src: &str) -> Result<(RcDom, Budget), TooLarge> {
    build(src, Budget::default())
}

/// The walk, with or without a message's own images.
fn read(src: &str, cid_images: bool) -> Result<(Vec<Element>, Vec<usize>), TooLarge> {
    read_within(src, cid_images, Budget::default())
}

/// [`read`], against `budget`.
fn read_within(
    src: &str,
    cid_images: bool,
    budget: Budget,
) -> Result<(Vec<Element>, Vec<usize>), TooLarge> {
    let (dom, budget) = build(src, budget)?;
    read_dom(&dom, budget, cid_images)
}

fn read_dom(
    dom: &RcDom,
    budget: Budget,
    cid_images: bool,
) -> Result<(Vec<Element>, Vec<usize>), TooLarge> {
    let facts = Facts::of(&dom.document);
    let mut out = Out {
        cid_images,
        budget,
        facts,
        ..Out::default()
    };
    // A page that marks its main content is read for that alone: what lies
    // outside it is the site around the document. One whose main holds no
    // text has marked it wrongly, and is read whole.
    let mut mains = Vec::new();
    find_mains(&dom.document, &out.facts, &mut mains);
    mains.retain(|m| has_text(m, &out.facts));
    if mains.is_empty() {
        walk(&dom.document, &mut out);
    } else {
        for m in &mains {
            walk(m, &mut out);
        }
    }
    if let Some(e) = out.over {
        return Err(e);
    }
    number(&mut out.elements);
    Ok((out.elements, out.depths))
}

/// Parse `src` into a tree, spending `budget` on what the parser does: its
/// tokenizer's work the tree does not show, counted before it starts
/// ([`attribute_steps`]); the tree builder's, as it does it, on each
/// formatting element's tag ([`Tokens`]) and through the sink ([`Sink`]);
/// and the tree as the parser makes it ([`Sink`]). Then set what nests past
/// [`MAX_DEPTH`] beside its ancestor at that depth. The budget comes back
/// with what is left of it.
fn build(src: &str, mut budget: Budget) -> Result<(RcDom, Budget), TooLarge> {
    budget.spend_work(attribute_steps(src))?;
    let sink = Sink {
        dom: RcDom::default(),
        budget: RefCell::new(budget),
        over: RefCell::new(None),
    };
    // What `html5ever::parse_document` puts together, with the tree builder
    // behind [`Tokens`].
    let builder = TreeBuilder::new(sink, TreeBuilderOpts::default());
    let tokenizer = Tokenizer::new(Tokens { builder }, TokenizerOpts::default());
    let input = BufferQueue::default();
    // Fed a little at a time, so that a page past its budget stops being
    // parsed close to where it passed it: the tokenizer goes on to the end
    // of what it was given, and a run of short blocks under many formatting
    // elements re-makes them all at each block.
    let mut rest = src;
    while !rest.is_empty() && tokenizer.sink.builder.sink.live() {
        let mut at = rest.len().min(FEED_BYTES);
        while !rest.is_char_boundary(at) {
            at += 1;
        }
        input.push_back(StrTendril::from_slice(&rest[..at]));
        while let TokenizerResult::Script(_) = tokenizer.feed(&input) {}
        rest = &rest[at..];
    }
    tokenizer.end();
    let sink = tokenizer.sink.builder.sink;
    if let Some(e) = sink.over.into_inner() {
        return Err(e);
    }
    flatten(&sink.dom.document);
    Ok((sink.dom, sink.budget.into_inner()))
}

/// How much of a page the parser is given at once: the most it does past
/// the page's budget, before it is stopped.
const FEED_BYTES: usize = 256;

/// The deepest a page's elements nest as it is read: a browser stops
/// nesting at 512 (Chromium sets deeper elements beside their parent), and
/// so does the reader, once the page is parsed. The walk recurses down the
/// tree, each level a few hundred bytes of stack, and a table four levels
/// (`table`, `tbody`, `tr`, `td`): 1,000 tables nested, a 30 KB page,
/// overflowed a 2 MiB stack.
pub const MAX_DEPTH: usize = 512;

/// Set each node nested past [`MAX_DEPTH`] beside its ancestor at that
/// depth, in the order the page has them, as a browser does: what is
/// inside them is read, only no longer inside them.
fn flatten(root: &Handle) {
    let mut stack = vec![(root.clone(), 0usize)];
    while let Some((h, d)) = stack.pop() {
        if d + 1 < MAX_DEPTH {
            stack.extend(h.children.borrow().iter().map(|c| (c.clone(), d + 1)));
            continue;
        }
        // Its children are at the deepest a page nests: their descendants
        // become their siblings, each after the node it was inside.
        let children = std::mem::take(&mut *h.children.borrow_mut());
        let mut flat = Vec::with_capacity(children.len());
        let mut pending: Vec<Handle> = children.into_iter().rev().collect();
        while let Some(n) = pending.pop() {
            let below = std::mem::take(&mut *n.children.borrow_mut());
            pending.extend(below.into_iter().rev());
            n.parent.set(Some(Rc::downgrade(&h)));
            flat.push(n);
        }
        *h.children.borrow_mut() = flat;
    }
}

/// What the parser makes of a page, spent from the page's budget as it is
/// made: each node at two cell slots (a node is 144 bytes, an element's,
/// a text's or a comment's, and its place among its parent's children),
/// and one for each 128 bytes of its text; an element's attributes at one
/// slot each and one for each 128 bytes of its value. Counted here rather
/// than from the page's tags, because the parser makes elements the page
/// does not write: a formatting element left open (`<b>`) is made again at
/// each block after it, so 400 of them over N short paragraphs are 400 × N
/// elements, a 91 KB page that aborted the process at 4 GB.
///
/// The tree builder's own work is spent here too, as steps, where it asks
/// the sink: the name of each open element it looks at and each pair of
/// nodes it compares, one step each. That is how it walks the elements
/// open around a node, to know what is in scope (40,000 nested `<div>`
/// took 3 s, 200,000 over two minutes), to find what an end tag closes
/// (20,000 `<span>` left open, then 200,000 `</zz>`, ran past a minute),
/// or to find a formatting element among those open; and finding a node
/// among its siblings, to set a node before another (a table's misplaced
/// content, set before it) or to take one out, costs a step for each of
/// them. What the tree builder does on a formatting element's tag without
/// asking the sink is spent by [`Tokens`].
///
/// Once the budget is spent, the sink keeps nothing more: it makes what the
/// parser asks for bare, attaches nothing, and changes nothing in the tree,
/// so that the parser runs on to where it is stopped ([`build`]) and the
/// page is refused.
struct Sink {
    dom: RcDom,
    budget: RefCell<Budget>,
    /// Why the page is refused, once its budget is spent.
    over: RefCell<Option<TooLarge>>,
}

/// What finding `h` among its parent's children takes: a step for each
/// [`SIBLINGS_A_STEP`] of them.
fn sibling_steps(h: &Handle) -> u64 {
    let parent = h.parent.take();
    h.parent.set(parent.clone());
    let siblings = parent
        .and_then(|w| w.upgrade())
        .map_or(0, |p| p.children.borrow().len() as u64);
    siblings.div_ceil(SIBLINGS_A_STEP)
}

/// Siblings looked at for a step: each is a pointer compared, half a
/// nanosecond, where a step of the parser's is several. 500,000 nodes set
/// before a table, among 200,000 siblings and those set before, took 105
/// s; an old layout page sets a few hundred, among a few thousand.
const SIBLINGS_A_STEP: u64 = 32;

/// Cell slots a node of the tree takes with `bytes` of its own text.
fn node_slots(bytes: usize) -> u64 {
    2 + bytes as u64 / 128
}

/// Cell slots an attribute takes with a value of `bytes`.
fn attribute_slots(bytes: usize) -> u64 {
    1 + bytes as u64 / 128
}

impl Sink {
    /// Spend from the budget; `false`, and the page refused, where it is
    /// spent.
    fn spend(&self, f: impl FnOnce(&mut Budget) -> Result<(), TooLarge>) -> bool {
        let mut over = self.over.borrow_mut();
        if over.is_some() {
            return false;
        }
        match f(&mut self.budget.borrow_mut()) {
            Ok(()) => true,
            Err(e) => {
                *over = Some(e);
                false
            }
        }
    }

    fn live(&self) -> bool {
        self.over.borrow().is_none()
    }

    /// Spend a step of the tree builder's.
    fn step(&self) {
        self.spend(|b| b.spend_work(1));
    }

    /// Spend what setting `child` in the tree costs: `steps`, and a text
    /// node, where it is text.
    fn spend_child(&self, steps: u64, child: &NodeOrText<Handle>) -> bool {
        if !self.spend(|b| b.spend_work(steps)) {
            return false;
        }
        match child {
            NodeOrText::AppendText(t) => self.spend(|b| b.spend_cells(node_slots(t.len()))),
            NodeOrText::AppendNode(_) => true,
        }
    }

    fn spend_attrs(&self, attrs: &[Attribute]) -> bool {
        let slots = attrs.iter().fold(0u64, |n, a| {
            n.saturating_add(attribute_slots(a.value.len()))
        });
        self.spend(|b| b.spend_cells(slots))
    }
}

impl TreeSink for Sink {
    type Handle = Handle;
    type Output = Self;
    type ElemName<'a> = ExpandedName<'a>;

    fn finish(self) -> Self {
        self
    }

    // A page's errors are not reported, and a page can make one at each
    // byte.
    fn parse_error(&self, _: Cow<'static, str>) {}

    fn get_document(&self) -> Handle {
        self.dom.get_document()
    }

    fn elem_name<'a>(&'a self, target: &'a Handle) -> ExpandedName<'a> {
        self.step();
        self.dom.elem_name(target)
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> Handle {
        if self.spend(|b| b.spend_cells(node_slots(0))) && self.spend_attrs(&attrs) {
            self.dom.create_element(name, attrs, flags)
        } else {
            self.dom.create_element(name, Vec::new(), flags)
        }
    }

    fn create_comment(&self, text: StrTendril) -> Handle {
        if self.spend(|b| b.spend_cells(node_slots(text.len()))) {
            self.dom.create_comment(text)
        } else {
            self.dom.create_comment(StrTendril::new())
        }
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> Handle {
        if self.spend(|b| b.spend_cells(node_slots(target.len() + data.len()))) {
            self.dom.create_pi(target, data)
        } else {
            self.dom.create_pi(StrTendril::new(), StrTendril::new())
        }
    }

    fn append(&self, parent: &Handle, child: NodeOrText<Handle>) {
        if self.spend_child(1, &child) {
            self.dom.append(parent, child);
        }
    }

    fn append_based_on_parent_node(
        &self,
        element: &Handle,
        prev_element: &Handle,
        child: NodeOrText<Handle>,
    ) {
        // Set before `element` where it has a parent, found among its
        // children.
        if self.spend_child(sibling_steps(element).saturating_add(1), &child) {
            self.dom
                .append_based_on_parent_node(element, prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        let bytes = name.len() + public_id.len() + system_id.len();
        if self.spend(|b| b.spend_cells(node_slots(bytes))) {
            self.dom
                .append_doctype_to_document(name, public_id, system_id);
        }
    }

    fn get_template_contents(&self, target: &Handle) -> Handle {
        self.dom.get_template_contents(target)
    }

    fn same_node(&self, x: &Handle, y: &Handle) -> bool {
        self.step();
        self.dom.same_node(x, y)
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.dom.set_quirks_mode(mode);
    }

    fn append_before_sibling(&self, sibling: &Handle, new_node: NodeOrText<Handle>) {
        if self.spend_child(sibling_steps(sibling).saturating_add(1), &new_node) {
            self.dom.append_before_sibling(sibling, new_node);
        }
    }

    fn add_attrs_if_missing(&self, target: &Handle, attrs: Vec<Attribute>) {
        // Each is looked for among the element's own: a page that repeats
        // `<html>` or `<body>`, each time with an attribute of its own,
        // costs the square of their number.
        let NodeData::Element { attrs: own, .. } = &target.data else {
            return;
        };
        let held = own.borrow().len() as u64;
        let n = attrs.len() as u64;
        if self.spend(|b| b.spend_work(held.saturating_add(n))) && self.spend_attrs(&attrs) {
            self.dom.add_attrs_if_missing(target, attrs);
        }
    }

    fn remove_from_parent(&self, target: &Handle) {
        if self.spend(|b| b.spend_work(sibling_steps(target))) {
            self.dom.remove_from_parent(target);
        }
    }

    fn reparent_children(&self, node: &Handle, new_parent: &Handle) {
        let moved = node.children.borrow().len() as u64;
        if self.spend(|b| b.spend_work(moved)) {
            self.dom.reparent_children(node, new_parent);
        }
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &Handle) -> bool {
        self.dom.is_mathml_annotation_xml_integration_point(handle)
    }

    // A `<select>` is never read, and copying its chosen option into it
    // would make a copy the budget does not see.
    fn maybe_clone_an_option_into_selectedcontent(&self, _: &Handle) {}
}

/// Formatting elements: those the tree builder keeps in its list of active
/// formatting elements, to make again where they are left open.
const FORMATTING: &[&str] = &[
    "a", "b", "big", "code", "em", "font", "i", "nobr", "s", "small", "strike", "strong", "tt", "u",
];

/// The tree builder, given the page's tokens through this, which spends
/// what it does on a formatting element's tag without asking the sink,
/// before it does it. A start tag is compared with each formatting element
/// of its name in the list, attributes and all: each comparison copies and
/// sorts the attributes of both (5,000 `<b>` of 100 attributes each, 2 MB,
/// ran past two minutes; without attributes, 0.1 s). An end tag looks for
/// its formatting element down the list by name alone, which may hold many
/// no longer open: 20,000 `<i>` closed by a `</p>`, then a million `</b>`,
/// took 55 s. The list is not in reach, so all the tree builder holds is
/// counted, its open elements with it: a step each, and for a start tag the
/// comparison with each of its name. Once the page is refused the tree
/// builder is given nothing more.
struct Tokens {
    builder: TreeBuilder<Handle, Sink>,
}

/// Steps to copy and sort `n` attributes.
fn sorting(n: u64) -> u64 {
    n.saturating_mul(1 + u64::from(u64::BITS - n.leading_zeros()))
}

/// What a tag of a formatting element costs, from what the tree builder
/// holds: a step each, and for a start tag, comparing its attributes with
/// those of each element of its name, once each (one both open and in the
/// list is held twice).
struct Held<'a> {
    name: &'a LocalName,
    /// The start tag's attributes, sorted at each comparison; `None` for
    /// an end tag, which compares none.
    attrs: Option<u64>,
    compared: RefCell<HashSet<*const Node>>,
    steps: Cell<u64>,
}

impl Tracer for Held<'_> {
    type Handle = Handle;

    fn trace_handle(&self, h: &Handle) {
        let mut n = 1u64;
        if let (Some(k), NodeData::Element { name, attrs, .. }) = (self.attrs, &h.data) {
            let compare = sorting(k).saturating_add(sorting(attrs.borrow().len() as u64));
            if compare > 0
                && name.local == *self.name
                && self.compared.borrow_mut().insert(Rc::as_ptr(h))
            {
                n = n.saturating_add(compare);
            }
        }
        self.steps.set(self.steps.get().saturating_add(n));
    }
}

impl Tokens {
    fn spend_formatting(&self, tag: &Tag) {
        if !FORMATTING.contains(&&*tag.name) {
            return;
        }
        let held = Held {
            name: &tag.name,
            attrs: (tag.kind == html5ever::tokenizer::StartTag).then_some(tag.attrs.len() as u64),
            compared: RefCell::default(),
            steps: Cell::new(0),
        };
        self.builder.trace_handles(&held);
        let steps = held.steps.get();
        self.builder.sink.spend(|b| b.spend_work(steps));
    }
}

impl TokenSink for Tokens {
    type Handle = Handle;

    fn process_token(&self, token: Token, line: u64) -> TokenSinkResult<Handle> {
        if let Token::TagToken(tag) = &token {
            self.spend_formatting(tag);
        }
        if !self.builder.sink.live() {
            return TokenSinkResult::Continue;
        }
        self.builder.process_token(token, line)
    }

    fn end(&self) {
        self.builder.end();
    }

    fn adjusted_current_node_present_but_not_in_html_namespace(&self) -> bool {
        self.builder
            .adjusted_current_node_present_but_not_in_html_namespace()
    }
}

/// Elements whose content is text up to their end tag, not markup.
const RAW_TEXT: &[&str] = &[
    "iframe", "noembed", "noframes", "noscript", "script", "style", "textarea", "title", "xmp",
];

/// What the parser's tokenizer costs that the tree does not show, in steps,
/// counted before the page is parsed: for each tag, the square of its
/// attributes, halved, since each is looked for among those before it (two
/// million on one tag ran past five minutes; an end tag's are looked for
/// too, then dropped). Read from a scan of the tags, comments and the text
/// of a `<script>` passed over.
///
/// The scan passes over no more than the tokenizer does, so as never to
/// count less: a comment ends at the first place it may (`-->`, `--!>`, or
/// at once, `<!-->` and `<!--->`), and an element's text up to its end tag
/// (`<script>`, `<title>`) is passed over only until the page opens an
/// `<svg>`, a `<math>` or a `<frameset>`, where such a tag may open no
/// such text. Passing over too little counts tags that are not, which only
/// counts more: 100,000 attributes behind `<!-->` or `<svg><title>` took
/// 15 to 19 s.
///
/// The rest of what parsing costs is counted as the tree is built
/// ([`Sink`], [`Tokens`]), where the parser makes it.
fn attribute_steps(src: &str) -> u64 {
    let b = src.as_bytes();
    let find = |from: usize, pat: &[u8]| {
        b.get(from..)
            .and_then(|s| s.windows(pat.len()).position(|w| w == pat))
            .map(|k| from + k)
    };
    let name_end = |from: usize| {
        from + b[from..]
            .iter()
            .take_while(|c| !c.is_ascii_whitespace() && !matches!(c, b'/' | b'>'))
            .count()
    };
    // A tag's attributes from `i`, its name's end, to the tag's end: each
    // name, then its value, quoted or not. Where the tag ends, and how many
    // attributes it has.
    let attributes = |mut i: usize| {
        let mut attrs = 0u64;
        while i < b.len() {
            match b[i] {
                b'>' => return (i + 1, attrs),
                c if c.is_ascii_whitespace() || c == b'/' => i += 1,
                _ => {
                    attrs += 1;
                    i += b[i..]
                        .iter()
                        .take_while(|c| {
                            !c.is_ascii_whitespace() && !matches!(c, b'=' | b'>' | b'/')
                        })
                        .count()
                        .max(1);
                    while b.get(i).is_some_and(u8::is_ascii_whitespace) {
                        i += 1;
                    }
                    if b.get(i) != Some(&b'=') {
                        continue;
                    }
                    i += 1;
                    while b.get(i).is_some_and(u8::is_ascii_whitespace) {
                        i += 1;
                    }
                    match b.get(i) {
                        Some(&q @ (b'"' | b'\'')) => {
                            i = b[i + 1..]
                                .iter()
                                .position(|&c| c == q)
                                .map_or(b.len(), |k| i + 1 + k + 1);
                        }
                        _ => {
                            i += b[i..]
                                .iter()
                                .take_while(|c| !c.is_ascii_whitespace() && **c != b'>')
                                .count();
                        }
                    }
                }
            }
        }
        (b.len(), attrs)
    };
    let pairs = |n: u64| n.saturating_mul(n.saturating_sub(1)) / 2;
    let mut steps = 0u64;
    // Whether a tag of an element of text may no longer open its text: in
    // SVG or MathML it is markup, and a frameset ignores the tag.
    let mut foreign = false;
    let mut i = 0;
    while let Some(k) = b[i..].iter().position(|&c| c == b'<') {
        i += k + 1;
        match b.get(i) {
            Some(b'!') if b[i..].starts_with(b"!--") => {
                // From the opening's own dashes, which `<!-->` and `<!--->`
                // end on.
                let end = [&b"-->"[..], b"--!>"]
                    .iter()
                    .filter_map(|pat| find(i + 1, pat).map(|e| e + pat.len()))
                    .min();
                i = end.unwrap_or(b.len());
            }
            Some(b'!' | b'?') => {
                i = find(i, b">").map_or(b.len(), |e| e + 1);
            }
            Some(b'/') if b.get(i + 1).is_some_and(u8::is_ascii_alphabetic) => {
                let (to, attrs) = attributes(name_end(i + 1));
                steps = steps.saturating_add(pairs(attrs));
                i = to;
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let end = name_end(i);
                let tag = &b[i..end];
                let (to, attrs) = attributes(end);
                steps = steps.saturating_add(pairs(attrs));
                i = to;
                foreign |= [&b"svg"[..], b"math", b"frameset"]
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(tag));
                if foreign {
                    continue;
                }
                if tag.eq_ignore_ascii_case(b"plaintext") {
                    break;
                }
                if RAW_TEXT
                    .iter()
                    .any(|t| t.as_bytes().eq_ignore_ascii_case(tag))
                {
                    // Its text runs to the first end tag of its name.
                    let mut at = i;
                    i = loop {
                        let Some(e) = find(at, b"</") else {
                            break b.len();
                        };
                        let close = b.get(e + 2..e + 2 + tag.len());
                        if close.is_some_and(|c| c.eq_ignore_ascii_case(tag)) {
                            break e;
                        }
                        at = e + 2;
                    };
                }
            }
            _ => {}
        }
    }
    steps
}

/// The outermost `<main>` elements, or elements in the `main` role, that the
/// page draws.
fn find_mains(h: &Handle, facts: &Facts, out: &mut Vec<Handle>) {
    for c in h.children.borrow().iter() {
        if tag_of(c).is_none() || facts.skipped(c) {
            continue;
        }
        if tag_of(c).as_deref() == Some("main") || role_of(c).as_deref() == Some("main") {
            out.push(c.clone());
        } else {
            find_mains(c, facts, out);
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
    /// Read a message's own images (`<img src="cid:…">`) as figures.
    cid_images: bool,
    /// What the page may still make.
    budget: Budget,
    /// Why the page is refused, once its budget is spent: nothing more is
    /// kept.
    over: Option<TooLarge>,
    /// What the walk asks of each node of the page.
    facts: Facts,
    /// How many list items the walk is inside: a list item's depth.
    items: usize,
    /// Whether the walk is directly in a list, not in one of its items.
    in_list: bool,
}

impl Out {
    /// Keep `e`, it, its text and its links spent first.
    fn push(&mut self, e: Element) {
        if self.spend(|b| b.spend_text(&e.text)) {
            self.push_gathered(e);
        }
    }

    /// Keep `e`, whose text was spent as it was gathered ([`Out::text`]),
    /// it and its links spent first.
    fn push_gathered(&mut self, e: Element) {
        let links = e.links.as_deref().unwrap_or_default();
        if self.spend(|b| {
            b.spend_elements(1)?;
            b.spend_links(links)
        }) {
            self.keep(e);
        }
    }

    /// The text `gather` gathers, with its links, spent before it is made:
    /// gathered once to count it, copying nothing, then made. `None` once
    /// the page is refused.
    ///
    /// Made first and spent after, with a map of each of its bytes to
    /// place its links by, a paragraph took 20 bytes of memory for each
    /// byte of it before it was spent: one of 193 MB, which the budget
    /// refuses, aborted the process under 4 GB.
    fn text(&mut self, gather: impl Fn(&mut Gather)) -> Option<(String, Vec<Link>)> {
        if self.over.is_some() {
            return None;
        }
        let mut count = Gather::new(&self.facts, false);
        gather(&mut count);
        let cost = count.cost;
        if !self.spend(|b| b.spend_text_cost(cost)) {
            return None;
        }
        let mut made = Gather::new(&self.facts, true);
        gather(&mut made);
        Some(made.finish())
    }

    /// An element of `kind` of the text `gather` gathers, where it has any.
    fn paragraph(&mut self, kind: &str, level: Option<usize>, gather: impl Fn(&mut Gather)) {
        if let Some((text, links)) = self.text(gather).filter(|(text, _)| !text.is_empty()) {
            self.push_gathered(linked(kind, text, level, links));
        }
    }

    /// A paragraph of a `<pre>`'s text as it is drawn ([`pre_text`]),
    /// spent at [`pre_cost`] before it is made.
    fn pre(&mut self, h: &Handle) {
        let cost = pre_cost(h, &self.facts);
        if !self.spend(|b| b.spend_text_cost(cost)) {
            return;
        }
        let (text, links) = pre_text(h, &self.facts);
        if !text.is_empty() {
            self.push_gathered(linked("doco:Paragraph", text, None, links));
        }
    }

    fn keep(&mut self, mut e: Element) {
        if e.kind == "doc:ListItem" {
            e.depth = self.items;
        }
        self.elements.push(e);
        self.depths.push(self.depth);
    }

    /// Spend from the page's budget; `false`, and the page refused, where
    /// it is spent.
    fn spend(&mut self, f: impl FnOnce(&mut Budget) -> Result<(), TooLarge>) -> bool {
        if self.over.is_some() {
            return false;
        }
        match f(&mut self.budget) {
            Ok(()) => true,
            Err(e) => {
                self.over = Some(e);
                false
            }
        }
    }

    /// A figure for each of a message's own images drawn under `nodes`
    /// (each node included), in order, where they are read as such.
    fn figures<'a>(&mut self, nodes: impl IntoIterator<Item = &'a Handle>) {
        if !self.cid_images {
            return;
        }
        let mut found = Vec::new();
        for n in nodes {
            cid_images(n, &self.facts, &mut found);
        }
        for (cid, alt) in found {
            let mut f = element("doco:Figure", alt, None);
            f.figure = Some(format!("cid:{cid}"));
            self.push(f);
        }
    }
}

/// The images under a node, the node included, that a message shows from
/// its own parts: `<img src="cid:…">`, by content id (RFC 2392, percent
/// signs decoded), with their alternative text. An image the page does not
/// draw is not shown.
fn cid_images(h: &Handle, facts: &Facts, out: &mut Vec<(String, String)>) {
    let Some(tag) = tag_of(h) else { return };
    if facts.skipped(h) {
        return;
    }
    if tag == "img" {
        let src = attr_of(h, "src").unwrap_or_default();
        let src = src.trim();
        // By bytes: a source is any text, and its fourth byte can fall
        // inside a character (`日本.png`).
        let id = src
            .get(..4)
            .filter(|scheme| scheme.eq_ignore_ascii_case("cid:"))
            .map(|_| &src[4..]);
        if let Some(id) = id.filter(|id| !id.is_empty()) {
            let alt = attr_of(h, "alt").unwrap_or_default();
            let alt = alt.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push((percent_decoded(id), alt));
        }
        return;
    }
    for c in h.children.borrow().iter() {
        cid_images(c, facts, out);
    }
}

/// `%XX` escapes decoded, as a URL writes the bytes it cannot hold; a `%`
/// that escapes nothing is kept.
fn percent_decoded(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (
            b[i],
            b.get(i + 1).copied().and_then(hex),
            b.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
/// in the body is an SVG drawing's. A page too large to read declares
/// nothing.
pub fn info(src: &str) -> DocumentInfo {
    let mut info = DocumentInfo::default();
    let Ok((dom, _)) = build(src, Budget::default()) else {
        return info;
    };
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
                // Its own facts: a title's text is all it holds.
                let facts = Facts::of(&h);
                info.title = Some(text_of(&h, &facts).0).filter(|t| !t.is_empty());
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
/// the phrase it styles, and that survives here as a span. Not spent: the
/// text the walk keeps is gathered by [`Out::text`].
fn text_of(h: &Handle, facts: &Facts) -> (String, Vec<Link>) {
    let mut g = Gather::new(facts, true);
    g.children(h);
    g.finish()
}

/// Is there text under this node, once its whitespace is collapsed?
/// Counted, not copied.
fn has_text(h: &Handle, facts: &Facts) -> bool {
    let mut g = Gather::new(facts, false);
    g.children(h);
    g.n > 0
}

/// Text gathered from a page's nodes, its whitespace collapsed as it is
/// gathered, the way the readers always have, and each `<a href>` in it
/// located by the characters it spans; or only counted, at [`char_cost`],
/// to be spent before it is made ([`Out::text`]).
///
/// One copy, made as the nodes are read. The text was gathered whole, then
/// collapsed with a map from each of its bytes to the character it lands
/// on, to place the links: 18 bytes for each byte of it.
struct Gather<'f> {
    facts: &'f Facts,
    /// The text, or `None` where it is only counted.
    text: Option<String>,
    /// What the text costs, each character at [`char_cost`].
    cost: u64,
    /// The characters of the text.
    n: usize,
    /// Whitespace met since the last character: a space before the next.
    pending: bool,
    /// Where the last character is.
    last: usize,
    /// Each anchor met.
    anchors: Vec<Anchor>,
    /// The anchors no character has started yet.
    awaiting: Vec<usize>,
    /// The anchors, in the order they end.
    ended: Vec<usize>,
    /// The last of the text, at least [`MARK_CONTEXT`] bytes of it where
    /// there is as much, to tell a note's mark by.
    tail: String,
}

/// An `<a href>` met as text is gathered.
struct Anchor {
    href: String,
    /// Where its first and last characters are, once it has one.
    span: Option<(usize, usize)>,
    /// Its end is not met yet.
    open: bool,
}

impl<'f> Gather<'f> {
    /// Text to `make`, or only to count.
    fn new(facts: &'f Facts, make: bool) -> Self {
        Gather {
            facts,
            text: make.then(String::new),
            cost: 0,
            n: 0,
            pending: false,
            last: 0,
            anchors: Vec::new(),
            awaiting: Vec::new(),
            ended: Vec::new(),
            tail: String::new(),
        }
    }

    fn push_str(&mut self, s: &str) {
        for c in s.chars() {
            self.push(c);
        }
    }

    fn push(&mut self, raw: char) {
        let c = repair_c1(raw);
        if c.is_whitespace() {
            self.pending = self.n > 0;
            return;
        }
        if std::mem::take(&mut self.pending) {
            self.put(' ');
        }
        for i in self.awaiting.drain(..) {
            let a = &mut self.anchors[i];
            if a.open {
                a.span = Some((self.n, self.n));
            }
        }
        self.last = self.n;
        self.put(c);
    }

    fn put(&mut self, c: char) {
        self.tail.push(c);
        if self.tail.len() >= 2 * MARK_CONTEXT {
            let mut cut = self.tail.len() - MARK_CONTEXT;
            while !self.tail.is_char_boundary(cut) {
                cut -= 1;
            }
            self.tail.drain(..cut);
        }
        self.cost = self.cost.saturating_add(char_cost(c));
        if let Some(t) = &mut self.text {
            t.push(c);
        }
        self.n += 1;
    }

    /// The text of `node`'s children, located as an anchor's where `node`
    /// is one: an anchor's text is whatever its subtree contributes, and
    /// nesting is legal markup. Counted text locates none.
    fn around(&mut self, node: &Handle, tag: &str) {
        let href = (tag == "a" && self.text.is_some())
            .then(|| attr_of(node, "href"))
            .flatten()
            .filter(|h| !h.trim().is_empty());
        let Some(href) = href else {
            self.children(node);
            return;
        };
        let at = self.anchors.len();
        self.anchors.push(Anchor {
            href,
            span: None,
            open: true,
        });
        self.awaiting.push(at);
        self.children(node);
        let last = self.last;
        let a = &mut self.anchors[at];
        a.open = false;
        if let Some((_, end)) = &mut a.span {
            *end = last;
        }
        self.ended.push(at);
    }

    fn children(&mut self, h: &Handle) {
        for child in h.children.borrow().iter() {
            self.node(child);
        }
    }

    /// One node's contribution to the text.
    fn node(&mut self, node: &Handle) {
        match &node.data {
            NodeData::Text { contents } => self.push_str(&contents.borrow()),
            NodeData::Element { .. } => {
                let Some(tag) = tag_of(node) else { return };
                if self.facts.skipped(node) {
                    return;
                }
                if matches!(tag.as_str(), "br" | "td" | "th" | "li" | "p" | "div") {
                    self.push(' ');
                }
                // A note's mark is read apart from the word it marks.
                let raised = tag == "sup" || (tag == "a" && note_link(node));
                if raised && self.n > 0 && !self.pending {
                    let mark = mark_text(node, self.facts);
                    if mark.is_some_and(|m| fluree_doc_model::marks::note_mark(&self.tail, &m)) {
                        self.push(' ');
                    }
                }
                self.around(node, &tag);
            }
            _ => {}
        }
    }

    /// An element's text excluding nested block containers, so a list item
    /// that holds a sub-list contributes only its own label.
    fn own(&mut self, h: &Handle) {
        for child in h.children.borrow().iter() {
            match &child.data {
                NodeData::Text { contents } => self.push_str(&contents.borrow()),
                NodeData::Element { .. } => {
                    let Some(tag) = tag_of(child) else { continue };
                    if self.facts.skipped(child) || matches!(tag.as_str(), "ul" | "ol" | "table") {
                        continue;
                    }
                    self.push(' ');
                    self.around(child, &tag);
                }
                _ => {}
            }
        }
    }

    /// The text, and its links [`ordered`].
    fn finish(self) -> (String, Vec<Link>) {
        let mut anchors = self.anchors;
        let links: Vec<Link> = self
            .ended
            .iter()
            .filter_map(|&i| {
                let a = &mut anchors[i];
                let (first, last) = a.span?;
                Some(Link::uri(std::mem::take(&mut a.href)).spanning(first, last + 1))
            })
            .collect();
        (self.text.unwrap_or_default(), ordered(links))
    }
}

/// A link to a note on the same page: a note's mark written as a link
/// rather than in a `<sup>` (Statistics Canada's tables: `Provincial
/// governments<a href="#fn01" class="sd-tablefnref">1</a>`). Its target is
/// a fragment of the page, and the link says it is a note's: by its role
/// (`doc-noteref`), or a class, id, target or label that names a note
/// (`fn`, `note`, `foot`).
fn note_link(node: &Handle) -> bool {
    let Some(href) = attr_of(node, "href").filter(|h| h.trim_start().starts_with('#')) else {
        return false;
    };
    if attr_of(node, "role").is_some_and(|r| r.eq_ignore_ascii_case("doc-noteref")) {
        return true;
    }
    [
        Some(href),
        attr_of(node, "class"),
        attr_of(node, "id"),
        attr_of(node, "aria-label"),
    ]
    .into_iter()
    .flatten()
    .any(|v| {
        let v = v.to_ascii_lowercase();
        v.contains("fn") || v.contains("note") || v.contains("foot")
    })
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

/// A paragraph from a run of inline siblings, when it has any text.
fn flush_run(run: &mut Vec<Handle>, out: &mut Out) {
    let gathered = out.text(|g| {
        for node in run.iter() {
            g.node(node);
        }
    });
    if let Some((text, links)) = gathered.filter(|(text, _)| !text.is_empty()) {
        out.push_gathered(linked("doco:Paragraph", text, None, links));
    }
    out.figures(run.iter());
    run.clear();
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
        if !out.facts.is_inline(&c) {
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

/// A `<pre>`'s text as it is drawn, with any `<a href>` inside it located.
///
/// In a `<pre>` the line breaks, the blank lines and the runs of spaces are
/// the content (HTML, the `pre` element): a hearing transcript or a public
/// law is published as one, its letterhead centred and its witness list set
/// in columns by spaces alone. So nothing is collapsed. A `<br>` is a line
/// break, and a block set inside one starts a line of its own. Not spent:
/// [`Out::pre`] spends it, at [`pre_cost`], before it is made.
fn pre_text(h: &Handle, facts: &Facts) -> (String, Vec<Link>) {
    let mut raw = Raw::default();
    let mut anchors = Vec::new();
    collect_pre(h, facts, &mut raw, &mut anchors);
    verbatim(&raw.text, &anchors)
}

/// What a `<pre>`'s text can cost at most, each character at
/// [`char_cost`] as it is drawn, counted down the tree without a copy: no
/// less than [`pre_text`] makes, which leaves out some of the whitespace
/// counted here, and adds no line break beyond the one counted for each
/// `<br>` and each side of a block.
fn pre_cost(h: &Handle, facts: &Facts) -> u64 {
    let mut cost = 0u64;
    let mut todo: Vec<Handle> = h.children.borrow().iter().rev().cloned().collect();
    while let Some(node) = todo.pop() {
        match &node.data {
            NodeData::Text { contents } => {
                for c in contents.borrow().chars() {
                    cost = cost.saturating_add(char_cost(drawn(c)));
                }
            }
            NodeData::Element { .. } => {
                let Some(tag) = tag_of(&node) else { continue };
                if facts.skipped(&node) {
                    continue;
                }
                let breaks = if tag == "br" {
                    1
                } else if is_block(&tag) {
                    2
                } else {
                    0
                };
                cost = cost.saturating_add(breaks * char_cost('\n'));
                if tag != "br" {
                    todo.extend(node.children.borrow().iter().rev().cloned());
                }
            }
            _ => {}
        }
    }
    cost
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

fn collect_pre(
    h: &Handle,
    facts: &Facts,
    out: &mut Raw,
    anchors: &mut Vec<(usize, usize, String)>,
) {
    for node in h.children.borrow().iter() {
        match &node.data {
            NodeData::Text { contents } => out.push_str(&contents.borrow()),
            NodeData::Element { .. } => {
                let Some(tag) = tag_of(node) else { continue };
                if facts.skipped(node) {
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
                collect_pre(node, facts, out, anchors);
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

/// A character of preformatted text as it is drawn: a tab stays a tab,
/// since it sets a column, and any other space character, a no-break space
/// among them, is a space.
fn drawn(c: char) -> char {
    match repair_c1(c) {
        '\n' => '\n',
        '\t' => '\t',
        c if c.is_whitespace() => ' ',
        c => c,
    }
}

/// Preformatted text as written, carrying the anchor offsets through.
///
/// Only what is never drawn goes: the whitespace before the first character
/// and after the last, as every element's text is trimmed, and the spaces a
/// line ends in. Each anchor, a range of `raw`'s characters, spans the
/// characters kept from it. A flag for each character of `raw` says whether
/// it is kept, and the anchors are placed in one pass over it, in the order
/// they start and end: a map from each character to where it lands took 16
/// bytes for each.
fn verbatim(raw: &str, anchors: &[(usize, usize, String)]) -> (String, Vec<Link>) {
    let n = raw.chars().count();
    let mut keep = vec![false; n];
    // Walked backwards, so a run of spaces knows whether a line break or
    // the end follows it. Nothing after the last character is kept.
    let (mut line_end, mut first) = (true, None);
    for (back, c) in raw.chars().rev().map(drawn).enumerate() {
        let i = n - 1 - back;
        match c {
            '\n' if first.is_some() => {
                keep[i] = true;
                line_end = true;
            }
            '\n' | ' ' | '\t' if first.is_none() || line_end => {}
            c if c.is_whitespace() => keep[i] = true,
            _ => {
                keep[i] = true;
                line_end = false;
                first = Some(i);
            }
        }
    }
    // Nor anything before the first.
    if let Some(first) = first {
        keep[..first].fill(false);
    }
    let mut by_start: Vec<usize> = (0..anchors.len()).collect();
    by_start.sort_by_key(|&a| anchors[a].0);
    let mut by_end = by_start.clone();
    by_end.sort_by_key(|&a| anchors[a].1);
    let mut placed: Vec<Option<(usize, usize)>> = vec![None; anchors.len()];
    let (mut started, mut ended) = (0, 0);
    let mut awaiting = Vec::new();
    let mut last_kept = 0usize;
    let mut out = String::with_capacity(raw.len());
    let mut at = 0usize;
    for (i, c) in raw.chars().map(drawn).enumerate() {
        while let Some(&a) = by_end.get(ended).filter(|&&a| anchors[a].1 <= i) {
            if let Some((_, last)) = &mut placed[a] {
                *last = last_kept;
            }
            ended += 1;
        }
        while let Some(&a) = by_start.get(started).filter(|&&a| anchors[a].0 <= i) {
            awaiting.push(a);
            started += 1;
        }
        if keep[i] {
            for a in awaiting.drain(..) {
                if anchors[a].1 > i {
                    placed[a] = Some((at, at));
                }
            }
            out.push(c);
            last_kept = at;
            at += 1;
        }
    }
    for &a in &by_end[ended..] {
        if let Some((_, last)) = &mut placed[a] {
            *last = last_kept;
        }
    }
    let links = anchors
        .iter()
        .zip(placed)
        .filter_map(|((_, _, href), at)| {
            let (first, last) = at?;
            Some(Link::uri(href.clone()).spanning(first, last + 1))
        })
        .collect();
    (out, ordered(links))
}

/// Links ordered and disjoint: emitters splice in one pass. Nested anchors
/// are invalid markup but do occur; the outer one is the one the document
/// drew.
fn ordered(mut links: Vec<Link>) -> Vec<Link> {
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

/// The bytes of text before a raised mark [`Gather`] keeps to tell a note's
/// mark from raised text that belongs to its word
/// ([`note_mark`](fluree_doc_model::marks::note_mark)), which looks at the
/// word before the mark and the character before that word. Asked of all
/// the text gathered so far, a run of a million raised digits after one
/// unbroken word looked at the word again at each: kept to its last
/// stretch, a word longer than any a mark follows is told by its end.
const MARK_CONTEXT: usize = 256;

/// The text of a raised element read as a note's mark, whitespace
/// collapsed, as [`text_of`] would gather it; `None` where it holds more
/// than a mark could ([`note_mark`](fluree_doc_model::marks::note_mark)
/// takes four characters at most), and the walk stops there. Gathered
/// whole at each raised element, nested ones each walked again what the
/// one inside them holds.
fn mark_text(h: &Handle, facts: &Facts) -> Option<String> {
    /// Nodes looked at, and characters other than whitespace, past which
    /// it is no mark.
    const NODES: usize = 64;
    const CHARS: usize = 8;
    let mut raw = String::new();
    let (mut nodes, mut chars) = (0usize, 0usize);
    let mut todo: Vec<Handle> = h.children.borrow().iter().rev().cloned().collect();
    while let Some(node) = todo.pop() {
        nodes += 1;
        if nodes > NODES {
            return None;
        }
        match &node.data {
            NodeData::Text { contents } => {
                for c in contents.borrow().chars().map(repair_c1) {
                    if !c.is_whitespace() {
                        chars += 1;
                        if chars > CHARS {
                            return None;
                        }
                    }
                    raw.push(c);
                }
            }
            NodeData::Element { .. } => {
                let Some(tag) = tag_of(&node) else { continue };
                if facts.skipped(&node) {
                    continue;
                }
                if matches!(tag.as_str(), "br" | "td" | "th" | "li" | "p" | "div") {
                    raw.push(' ');
                }
                todo.extend(node.children.borrow().iter().rev().cloned());
            }
            _ => {}
        }
    }
    Some(raw.split_whitespace().collect::<Vec<_>>().join(" "))
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
    // A list set straight in a list rather than in one of its items, as
    // hand-written markup often has it, shows inside the item before it:
    // one deeper.
    let list = matches!(tag_of(h).as_deref(), Some("ul" | "ol"));
    let nested = list && out.in_list;
    out.items += usize::from(nested);
    let in_list = std::mem::replace(&mut out.in_list, list);
    if is_citation(h) {
        out.depth += 1;
        walk_node(h, out);
        out.depth -= 1;
    } else {
        walk_node(h, out);
    }
    out.in_list = in_list;
    out.items -= usize::from(nested);
}

fn walk_node(h: &Handle, out: &mut Out) {
    if let Some(tag) = tag_of(h) {
        let t = tag.as_str();
        if out.facts.skipped(h) {
            return;
        }
        if t == "table" {
            if is_layout(h, &out.facts) {
                walk_container(h, out);
            } else {
                // The caption is where a table states its subject or unit
                // (`Price list, prices in EUR`); it reads before the table.
                for c in children_of(h) {
                    if tag_of(&c).as_deref() == Some("caption") {
                        out.paragraph("doco:Paragraph", None, |g| g.children(&c));
                    }
                }
                emit_table(h, out); // innermost wins: cells own their text
                out.figures([h]);
            }
            return;
        }
        if let Some(level) = heading_level(t) {
            out.paragraph("doco:SectionTitle", Some(level), |g| g.children(h));
            out.figures([h]);
            return;
        }
        match t {
            "li" => {
                // A nested list inside an item is walked separately; the
                // item's own text stops at it.
                out.paragraph("doc:ListItem", None, |g| g.own(h));
                let children = children_of(h);
                out.figures(
                    children
                        .iter()
                        .filter(|c| !matches!(tag_of(c).as_deref(), Some("ul" | "ol" | "table"))),
                );
                for c in children {
                    match tag_of(&c).as_deref() {
                        // A list in an item is one deeper than the item.
                        Some("ul" | "ol") => {
                            out.items += 1;
                            walk(&c, out);
                            out.items -= 1;
                        }
                        // A table's cells start afresh.
                        Some("table") => {
                            let items = std::mem::take(&mut out.items);
                            walk(&c, out);
                            out.items = items;
                        }
                        _ => {}
                    }
                }
                return;
            }
            // A quotation or a caption that holds paragraphs of its own is a
            // container of them, not one paragraph run together.
            "blockquote" | "figcaption" | "dd" | "dt" if out.facts.holds_block(h) => {}
            // One paragraph, its lines and spacing kept, as a Markdown code
            // block is read.
            "pre" => {
                out.pre(h);
                out.figures([h]);
                return;
            }
            "p" | "blockquote" | "figcaption" | "dd" | "dt" => {
                out.paragraph("doco:Paragraph", None, |g| g.children(h));
                out.figures([h]);
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
fn rows_under(h: &Handle, facts: &Facts, out: &mut Vec<Handle>) {
    for c in h.children.borrow().iter() {
        match tag_of(c).as_deref() {
            Some(_) if facts.skipped(c) => {}
            Some("tr") => out.push(c.clone()),
            // Do not descend into a nested table: its rows are its own.
            Some("table") => {}
            _ => rows_under(c, facts, out),
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
fn is_layout(table: &Handle, facts: &Facts) -> bool {
    let role = attr_of(table, "role").map(|r| r.trim().to_ascii_lowercase());
    if matches!(role.as_deref(), Some("presentation" | "none")) {
        return true;
    }
    if facts.holds_table(table) {
        return true;
    }
    let mut trs = Vec::new();
    rows_under(table, facts, &mut trs);
    let widths: Vec<usize> = trs
        .iter()
        .map(|tr| {
            children_of(tr)
                .iter()
                .filter(|c| matches!(tag_of(c).as_deref(), Some("td" | "th")))
                .map(|c| num_attr(c, "colspan"))
                .fold(0usize, usize::saturating_add)
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
    /// Its text is a caption in brackets, the units a filer sets in the
    /// label column of its header row: `(in millions)`.
    caption: bool,
    /// Its text is a bare figure, set neither bold nor in `<th>`: a value
    /// however it is ruled.
    figure: bool,
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
    /// statement among them, writes in `<td>`. A units caption in the
    /// label column may stand plain beside them. `labelled` where the label
    /// column holds text other than such a caption.
    Styled { labelled: bool },
    /// One `<td>` set as a header over a value column, the label column
    /// left empty or given to the units: `Annual Output`, or a contents
    /// page's `Page`.
    StyledOne,
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
    // The heads: every filled cell but a units caption in the label column,
    // which a filer sets plain beside heads that are bold or ruled.
    let heads: Vec<(usize, &CellMark)> = marks
        .iter()
        .enumerate()
        .filter(|(i, m)| m.filled && !(*i == 0 && m.caption && !m.th))
        .collect();
    if heads.iter().any(|(_, m)| !m.th && !m.styled) {
        return RowKind::Data;
    }
    let first_th = marks.iter().position(|m| m.th);
    let empty_td_after_th = first_th.is_some_and(|f| marks[f..].iter().any(|m| !m.th));
    if filled.iter().all(|m| m.th) && !(filled.len() == 1 && empty_td_after_th) {
        return RowKind::Th {
            cells: filled.len(),
        };
    }
    // Two heads or more; or one over a value column, the label cell left
    // empty or given to the units. One label alone in the label column is
    // a band (`Revenues`).
    if heads.len() >= 2 {
        return RowKind::Styled {
            labelled: heads[0].0 == 0,
        };
    }
    // A lone figure ruled beneath is the line under the values before a
    // subtotal, not a head.
    if heads.first().is_some_and(|(i, m)| *i > 0 && !m.figure) {
        return RowKind::StyledOne;
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
    // The next row with text after `from`.
    let next = |from: usize| kinds[from..].iter().find(|k| **k != RowKind::Blank);
    let mut end = thead;
    for (i, kind) in kinds.iter().enumerate().skip(thead) {
        let header = match kind {
            RowKind::Blank => continue,
            RowKind::Th { cells } => thead == 0 || *cells >= 2,
            RowKind::Styled { .. } => values_follow(i + 1),
            // One head over a column heads it when what follows is the
            // body or more of the header. A styled row with a label of its
            // own after it is a contents page's entry under its `Page`.
            RowKind::StyledOne => {
                values_follow(i + 1)
                    && !matches!(next(i + 1), Some(RowKind::Styled { labelled: true }))
            }
            RowKind::Data | RowKind::Other => false,
        };
        if !header {
            break;
        }
        end = i + 1;
    }
    end
}

/// A row of one head names a column of figures: `Annual Output` over
/// output, a contents page's `Page` over page numbers. One over names and
/// titles is a signature's line (`/s/ Jane Q. Signer` ruled above the
/// signer), and is no header. Columns are counted by `colspan`.
fn heads_over_figures(rows: &[Vec<(String, usize, usize)>], kinds: &mut [RowKind]) {
    let spans = |row: &[(String, usize, usize)]| {
        let mut c = 0usize;
        row.iter()
            .map(|(text, colspan, _)| {
                let span = (c, c + (*colspan).max(1));
                c = span.1;
                (text.trim().to_string(), span)
            })
            .collect::<Vec<_>>()
    };
    for r in 0..rows.len() {
        if kinds[r] != RowKind::StyledOne {
            continue;
        }
        let Some((_, (a, b))) = spans(&rows[r])
            .into_iter()
            .skip(1)
            .find(|(t, _)| !t.is_empty())
        else {
            continue;
        };
        let figures = rows[r + 1..].iter().any(|row| {
            spans(row)
                .iter()
                .any(|(t, (x, y))| *x < b && a < *y && t.chars().any(|c| c.is_ascii_digit()))
        });
        if !figures {
            kinds[r] = RowKind::Other;
        }
    }
}

/// Fewest rows of a list of fields that set a label beside a value.
const MIN_FIELDS: usize = 2;

/// Do the rows list fields: every row that sets a label in its first filled
/// cell beside a value in a later one ends the label on a colon (`FOR:`,
/// `CONTACT:`), [`MIN_FIELDS`] of them or more? A row with no value (`News
/// Release`), or a value with no label (a contact's second line), says
/// nothing either way.
fn lists_fields(rows: &[Vec<(String, usize, usize)>]) -> bool {
    let mut fields = 0usize;
    for row in rows {
        let filled: Vec<&str> = row
            .iter()
            .map(|(t, ..)| t.trim())
            .filter(|t| !t.is_empty())
            .collect();
        let starts_on_label = row.first().is_some_and(|(t, ..)| !t.trim().is_empty());
        if filled.len() < 2 || !starts_on_label {
            continue;
        }
        if !filled[0].ends_with(':') {
            return false;
        }
        fields += 1;
    }
    fields >= MIN_FIELDS
}

/// Is this cell set as a header: ruled beneath, or bold throughout?
fn header_styled(cell: &Handle, facts: &Facts) -> bool {
    ruled_below(cell) || bold_throughout(cell, false, facts) == Some(true)
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
///
/// Any width above zero is a rule. Filings draw theirs at half a point
/// (`border-bottom: 0.5pt solid #000`), which reading every width that
/// began with a `0` as none took for no rule at all.
fn ruled_below(cell: &Handle) -> bool {
    const LINES: &[&str] = &[
        "solid", "double", "dotted", "dashed", "groove", "ridge", "inset", "outset",
    ];
    style_of(cell).split(';').any(|d| match d.split_once(':') {
        Some(("border-bottom", v)) => {
            // The width leads the shorthand when it is given first; the
            // declaration's spaces are already gone (`0.5ptsolid#000`).
            let width: String = v
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let zero = width.parse::<f64>().is_ok_and(|w| w <= 0.0);
            !v.is_empty() && !v.contains("none") && !v.contains("hidden") && !zero
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
fn bold_throughout(h: &Handle, inherited: bool, facts: &Facts) -> Option<bool> {
    let bold = inherited || bold_style(h) || matches!(tag_of(h).as_deref(), Some("b" | "strong"));
    let mut seen = None;
    for c in h.children.borrow().iter() {
        let here = match &c.data {
            NodeData::Text { contents } if !contents.borrow().trim().is_empty() => Some(bold),
            NodeData::Element { .. } if !facts.skipped(c) => bold_throughout(c, bold, facts),
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
    if out.over.is_some() {
        return;
    }
    let mut trs = Vec::new();
    rows_under(table, &out.facts, &mut trs);

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
            let text = text_of(&cell, &out.facts).0;
            let caption = {
                let t = text.trim();
                t.len() > 2 && t.starts_with('(') && t.ends_with(')')
            };
            marks.push(CellMark {
                filled: !text.is_empty(),
                th,
                styled: !th && !text.is_empty() && header_styled(&cell, &out.facts),
                caption,
                figure: !th
                    && text.chars().any(|c| c.is_ascii_digit())
                    && !text.chars().any(char::is_alphabetic)
                    && bold_throughout(&cell, false, &out.facts) != Some(true),
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
    heads_over_figures(&rows, &mut kinds);
    let mut thead_trs = Vec::new();
    for c in children_of(table) {
        if tag_of(&c).as_deref() == Some("thead") {
            rows_under(&c, &out.facts, &mut thead_trs);
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
    let mut header_rows = header_rows(&kinds, thead);
    // A list of fields has no header: under bold `<td>` cells at its top (a
    // press release's letterhead, `FOR IMMEDIATE RELEASE` beside `EXHIBIT
    // 99.1`), each row that sets a label beside a value labels it with a
    // colon (`FOR:`, `CONTACT:`). Read as a header, the letterhead named
    // every field's value. A header a `<thead>` or `<th>` states stands.
    if thead == 0
        && kinds[..header_rows]
            .iter()
            .all(|k| matches!(k, RowKind::Styled { .. } | RowKind::Blank))
        && lists_fields(&rows[header_rows..])
    {
        header_rows = 0;
    }
    let width = rows
        .iter()
        .map(|r| r.iter().fold(0usize, |w, c| w.saturating_add(c.1)))
        .max()
        .unwrap_or(0)
        .max(1);
    let n = rows.len();
    // The grid, as wide as the widest row's spans make it, before it is
    // made.
    if !out.spend(|b| b.spend_cells((n as u64).saturating_mul(width as u64))) {
        return;
    }
    let mut grid = vec![String::new(); n * width];
    let mut m_left = vec![false; n * width];
    let mut m_down = vec![false; n * width];
    // Occupancy, so a rowspan from an earlier row displaces later cells the
    // way a browser lays them out.
    let mut taken = vec![false; n * width];
    // Rows that are one cell across the whole width, as laid out.
    let mut full_width = vec![false; n];

    // Each cell's text is moved into the grid, then into its row: a copy
    // of a table's text is as large as its tree.
    for (r, cells) in rows.iter_mut().enumerate() {
        let lone = cells.len() == 1;
        let mut c = 0usize;
        for (text, colspan, rowspan) in cells.iter_mut() {
            while c < width && taken[r * width + c] {
                c += 1;
            }
            if c >= width {
                break;
            }
            let cs = (*colspan).min(width - c);
            let rs = (*rowspan).min(n - r);
            // Each cell the span marks, overlapping spans each time.
            if !out.spend(|b| b.spend_work((cs as u64).saturating_mul(rs as u64))) {
                return;
            }
            full_width[r] = lone && c == 0 && cs == width && width > 1;
            grid[r * width + c] = std::mem::take(text);
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
    // A row holding nothing of its own, every cell either one above it
    // running down into it or an empty spacer, is the second line of the
    // record over it, not a row: a page laying each record over two rows
    // (`rowspan="2"` on every column) sets the second row as a lone spacer
    // cell, and read as a row it repeated the record. Not where a cell
    // starting in it runs on down: it is that cell's first row.
    let hollow: Vec<bool> = (0..n)
        .map(|r| {
            let row = r * width..(r + 1) * width;
            r > 0
                && row.clone().any(|i| m_down[i])
                && row.clone().all(|i| m_down[i] || grid[i].trim().is_empty())
                && (r + 1 >= n
                    || (0..width).all(|c| !m_down[(r + 1) * width + c] || m_down[r * width + c]))
        })
        .collect();
    let (grid, m_left, m_down, full_width, header_rows, n) = if hollow.iter().any(|h| *h) {
        let keep = |r: usize| !hollow[r];
        let rows_of = |v: &[bool]| -> Vec<bool> {
            (0..n)
                .filter(|&r| keep(r))
                .flat_map(|r| v[r * width..(r + 1) * width].iter().copied())
                .collect()
        };
        let grid: Vec<String> = (0..n)
            .filter(|&r| keep(r))
            .flat_map(|r| grid[r * width..(r + 1) * width].iter().cloned())
            .collect();
        let full: Vec<bool> = (0..n).filter(|&r| keep(r)).map(|r| full_width[r]).collect();
        let header = header_rows - (0..header_rows.min(n)).filter(|&r| hollow[r]).count();
        let kept = full.len();
        (grid, rows_of(&m_left), rows_of(&m_down), full, header, kept)
    } else {
        (grid, m_left, m_down, full_width, header_rows, n)
    };
    drop(rows);
    let mut grid = grid.into_iter();
    let cells: Vec<Vec<String>> = (0..n)
        .map(|_| grid.by_ref().take(width).collect())
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
    let spent = out.spend(|b| spend_table(b, &e));
    if spent {
        out.keep(e);
    }
}

/// Spend the table `e`, its cells that hold text and their text, as every
/// reader spends a table's cells ([`Budget::spend_cell_texts`]), and what
/// DoCO copies of them. About 510 bytes a cell were measured from the
/// parser's tree to the Markdown written, the tree spent apart.
fn spend_table(b: &mut Budget, e: &Element) -> Result<(), TooLarge> {
    b.spend_elements(1)?;
    b.spend_cell_texts(e.cells.iter().flatten().flatten().map(String::as_str))?;
    b.spend_copies(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluree_doc_model::budget::{
        converting_to, output, Output, CELL_BYTES, ELEMENT_BYTES, FLAT_CELL_BYTES, MAX_MEMORY,
        MAX_WORK, TEXT_BYTES,
    };
    use fluree_doc_model::scaling::assert_linear;

    fn parse(src: &str) -> Vec<Element> {
        super::parse(src).unwrap()
    }

    fn parse_with_quote_depth(src: &str) -> (Vec<Element>, Vec<usize>) {
        super::parse_with_quote_depth(src).unwrap()
    }

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
    fn a_messages_own_image_is_a_figure_where_it_is_drawn() {
        let src = "<p>See below.</p><p><img src=\"cid:shot1%40example.org\" alt=\"  Record \n screen \"></p>\
                   <table><tr><td>Name</td><td>Logo <img src=\"CID:logo@x\"></td></tr>\
                   <tr><td>a</td><td>b</td></tr></table>\
                   <ul><li>Item <img src=\"cid:i@x\"><ul><li>Inner <img src=\"cid:j@x\"></li></ul></li></ul>\
                   <div>Loose <img src=\"cid:k@x\"> text<img src=\"https://example.org/web.png\"><img src=\"日本.png\"><img src=\"aéé.png\"></div>\
                   <p style=\"display:none\"><img src=\"cid:hidden@x\"></p>";
        let (els, depths) = parse_with_quote_depth(src);
        assert_eq!(els.len(), depths.len());
        let got: Vec<(&str, Option<&str>)> = els
            .iter()
            .map(|e| (e.kind.as_str(), e.figure.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("doco:Paragraph", None),
                ("doco:Figure", Some("cid:shot1@example.org")),
                ("doco:Table", None),
                ("doco:Figure", Some("cid:logo@x")),
                ("doc:ListItem", None),
                ("doco:Figure", Some("cid:i@x")),
                ("doc:ListItem", None),
                ("doco:Figure", Some("cid:j@x")),
                ("doco:Paragraph", None),
                ("doco:Figure", Some("cid:k@x")),
            ]
        );
        assert_eq!(els[1].text, "Record screen");
        assert_eq!(els[8].text, "Loose text");
        // One parse for both, as a message's body is read.
        let (body, _, ids) = parse_message(src).unwrap();
        assert_eq!(body.len(), els.len());
        assert_eq!(ids, ["shot1@example.org", "logo@x", "i@x", "j@x", "k@x"]);
        assert_eq!(ids, cid_ids(src));
        // Read as a page, not a message, no image is anything.
        assert!(parse(src).iter().all(|e| e.kind != "doco:Figure"));
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
    fn a_records_second_row_of_spanned_cells_and_a_spacer_is_no_row() {
        // Each record over two rows, every column spanning both, the second
        // row a lone spacer cell; a group label spanning the records.
        let (heads, cells) = header(
            "<table><tr><th>Brand</th><th>Model</th><th>Price</th><td>&nbsp;</td></tr>\
             <tr><td rowspan=\"4\">Acme</td><td rowspan=\"2\">A-1</td>\
                 <td rowspan=\"2\">$5</td><td>&nbsp;</td></tr><tr><td>&nbsp;</td></tr>\
             <tr><td rowspan=\"2\">A-2</td><td rowspan=\"2\">$7</td><td>&nbsp;</td></tr>\
                 <tr><td>&nbsp;</td></tr></table>",
        );
        assert_eq!(heads, Some(1));
        let rows: Vec<Vec<&str>> = cells
            .iter()
            .map(|r| r.iter().map(|c| c.trim()).collect())
            .collect();
        assert_eq!(
            rows,
            vec![
                vec!["Brand", "Model", "Price", ""],
                vec!["Acme", "A-1", "$5", ""],
                vec!["", "A-2", "$7", ""],
            ]
        );
        // A row whose cell starts a span of its own is that cell's first row.
        let (_, cells) = header(
            "<table><tr><th>A</th><th>B</th></tr>\
             <tr><td rowspan=\"2\">x</td><td>1</td></tr>\
             <tr><td rowspan=\"2\"></td></tr><tr><td>y</td></tr></table>",
        );
        assert_eq!(cells.len(), 4);
    }

    #[test]
    fn a_letterhead_over_a_list_of_fields_is_no_header() {
        let letterhead = "<table><tr><td style=\"font-weight:700\">FOR IMMEDIATE RELEASE</td>\
             <td></td><td style=\"font-weight:700\">EXHIBIT 99.1</td></tr>\
             <tr><td>FOR:</td><td>&#160;</td><td>EMCOR GROUP, INC.</td></tr>\
             <tr><td>News Release</td><td></td><td></td></tr>\
             <tr><td>CONTACT:</td><td></td><td>Andy Backman</td></tr></table>";
        assert_eq!(header(letterhead).0, Some(0));
        // Bold heads over values that are not fields stay the header.
        let heads = "<table><tr><td style=\"font-weight:700\">Item</td>\
             <td style=\"font-weight:700\">Amount</td></tr>\
             <tr><td>Rent:</td><td>100</td></tr><tr><td>Heat</td><td>20</td></tr></table>";
        assert_eq!(header(heads).0, Some(1));
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
    fn a_hairline_under_the_heads_is_a_rule() {
        // A filing's unit caption over bold heads, each cell ruled beneath
        // at half a point or less; the caption is plain.
        for width in ["0.5pt", "0.75pt", ".5pt", "0.4px", "1px", "thin"] {
            let r = format!("border-bottom:{width} solid #000");
            let (n, _) = header(&format!(
                "<table><tr><td style=\"{r}\">(in thousand tonnes)</td>\
                 <td style=\"{r}\"><b>2024</b></td><td style=\"{r}\"><b>2023</b></td></tr>\
                 <tr><td>North plant</td><td>1,240</td><td>1,180</td></tr>\
                 <tr><td>South plant</td><td>615</td><td>590</td></tr></table>"
            ));
            assert_eq!(n, Some(1), "{width}");
        }
        // A width of zero draws nothing.
        for width in ["0", "0px", "0.0pt"] {
            let r = format!("border-bottom:{width} solid #000");
            let (n, _) = header(&format!(
                "<table><tr><td style=\"{r}\">A</td><td style=\"{r}\">1</td></tr>\
                 <tr><td>B</td><td>2</td></tr><tr><td>C</td><td>3</td></tr></table>"
            ));
            assert_eq!(n, Some(0), "{width}");
        }
    }

    #[test]
    fn a_units_caption_or_an_empty_label_cell_leaves_the_heads_a_header() {
        let r = "border-bottom:0.5pt solid #000";
        // The units in the label column, plain, beside ruled year heads.
        let (n, _) = header(&format!(
            "<table><tr><td>(in millions)</td><td style=\"{r}\">2024</td><td style=\"{r}\">2023</td></tr>\
             <tr><td>Northfield Power</td><td>214</td><td>198</td></tr>\
             <tr><td>Lakeside Gas</td><td>11</td><td>12</td></tr></table>"
        ));
        assert_eq!(n, Some(1));
        // One head over the value column, the label cell left empty.
        let (n, _) = header(&format!(
            "<table><tr><td style=\"{r}\">&#160;</td><td style=\"{r}\"><b>Annual Output</b></td></tr>\
             <tr><td>North plant</td><td>1,240</td></tr><tr><td>South plant</td><td>615</td></tr></table>"
        ));
        assert_eq!(n, Some(1));
        // A contents page's `Page` over its numbers, then a bold article
        // with its page: the article is an entry, not a second header row.
        let (n, _) = header(
            "<table><tr><td></td><td><b>Page</b></td></tr>\
             <tr><td><b>ARTICLE I DEFINITIONS</b></td><td><b>1</b></td></tr>\
             <tr><td>Section 1.1 Definitions</td><td>1</td></tr>\
             <tr><td><b>ARTICLE II TERM</b></td><td><b>5</b></td></tr></table>",
        );
        assert_eq!(n, Some(0));
        // A signature ruled above the signer's name and title heads nothing.
        let (n, _) = header(&format!(
            "<table><tr><td></td><td style=\"{r}\">/s/ Jane Q. Signer</td></tr>\
             <tr><td>By:</td><td>Jane Q. Signer</td></tr>\
             <tr><td>Its:</td><td>President</td></tr></table>"
        ));
        assert_eq!(n, Some(0));
        // A plain label that is no caption is a row's, and the row is data;
        // a bold label alone in the label column is a band.
        for html in [
            format!(
                "<table><tr><td>Net income</td><td style=\"{r}\">221</td><td style=\"{r}\">204</td></tr>\
                 <tr><td>North</td><td>1</td><td>2</td></tr></table>"
            ),
            "<table><tr><td><b>Revenues</b></td><td></td></tr>\
             <tr><td>North</td><td>1</td></tr></table>"
                .to_string(),
        ] {
            assert_eq!(header(&html).0, Some(0), "{html}");
        }
    }

    #[test]
    fn one_head_over_every_value_column_beside_a_blank_corner_is_the_header() {
        let r = "border-bottom:1px solid";
        let (n, _) = header(&format!(
            "<table><tr><td></td><td colspan=\"2\"><b>Year Ended December 31,</b></td></tr>\
             <tr><td></td><td style=\"{r}\"><b>2024</b></td><td style=\"{r}\"><b>2023</b></td></tr>\
             <tr><td>Revenue</td><td>120</td><td>110</td></tr>\
             <tr><td>Costs</td><td>80</td><td>75</td></tr></table>"
        ));
        assert_eq!(n, Some(2));
        // A year set bold over a single column of values heads it.
        let (n, _) = header(
            "<table><tr><td></td><td><b>2024</b></td></tr>\
             <tr><td>Revenue</td><td>120</td></tr><tr><td>Costs</td><td>80</td></tr></table>",
        );
        assert_eq!(n, Some(1));
    }

    #[test]
    fn emphasis_or_a_rule_beside_a_label_is_not_a_header() {
        for html in [
            // A key-value table's bold value.
            "<table><tr><td>Borrower:</td><td><b>Acme Corp</b></td></tr>\
             <tr><td>Amount:</td><td>$100</td></tr></table>",
            // The figures underlined before a subtotal, with or without a
            // label beside them.
            "<table><tr><td>Cost of sales</td><td style=\"border-bottom:1px solid\">(60)</td>\
             <td style=\"border-bottom:1px solid\">(50)</td></tr>\
             <tr><td>Gross profit</td><td>40</td><td>40</td></tr></table>",
            "<table><tr><td></td><td style=\"border-bottom:1px solid\">(60)</td></tr>\
             <tr><td>Gross profit</td><td>40</td></tr></table>",
        ] {
            assert_eq!(header(html).0, Some(0), "{html}");
        }
    }

    #[test]
    fn a_note_mark_written_as_a_link_stands_apart() {
        let t = |html: &str| parse(html)[0].text.clone();
        assert_eq!(
            t(
                "<p>Provincial governments<a href=\"#fn01\" class=\"btn sd-tablefnref\" \
               aria-label=\"Table note 1\" role=\"button\">1</a> rose</p>"
            ),
            "Provincial governments 1 rose"
        );
        assert_eq!(
            t("<p>Revenue<a role=\"doc-noteref\" href=\"#n3\">3</a></p>"),
            "Revenue 3"
        );
        // A link that is no note's, or goes elsewhere, is read as written.
        assert_eq!(t("<p>Part<a href=\"#part2\">2</a></p>"), "Part2");
        assert_eq!(
            t("<p>Table<a href=\"https://example.org/fn\">5</a></p>"),
            "Table5"
        );
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
        // After a quantity or a bracket of words, a number is a note's; a
        // unit of length takes a power whatever its name's length.
        assert_eq!(
            t("<p>up to 26TB<sup>1</sup>, 11 lbf (49 N)<sup>5</sup>, 1000 PIES<sup>2</sup></p>"),
            "up to 26TB 1, 11 lbf (49 N) 5, 1000 PIES2"
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
    fn a_list_in_an_item_is_one_deeper() {
        let els = parse(
            "<ul><li>Fruit<ol><li>Apples<ul><li>Seedless</li></ul></li><li>Pears</li></ol></li>\
             <li>Vegetables<table><tr><td><ul><li>In a cell</li></ul></td></tr></table></li></ul>",
        );
        let got: Vec<(&str, usize)> = els
            .iter()
            .filter(|e| e.kind == "doc:ListItem")
            .map(|e| (e.text.as_str(), e.depth))
            .collect();
        assert_eq!(
            got,
            [
                ("Fruit", 0),
                ("Apples", 1),
                ("Seedless", 2),
                ("Pears", 1),
                ("Vegetables", 0),
                // A list in a table's cell starts afresh.
                ("In a cell", 0)
            ]
        );
        // A list set straight in a list, as a browser shows it.
        let els = parse("<ul><li>Fruit</li><ul><li>Apples</li></ul><li>Vegetables</li></ul>");
        let got: Vec<usize> = els.iter().map(|e| e.depth).collect();
        assert_eq!(got, [0, 1, 0]);
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

    #[test]
    fn a_long_value_spanning_rows_is_refused_rather_than_copied_into_each() {
        let table = |rows: usize| {
            format!(
                "<table><tr><td rowspan=\"{rows}\">{}</td><td>a</td></tr>{}</table>",
                "y".repeat(200_000),
                "<tr><td>b</td></tr>".repeat(rows - 1)
            )
        };
        assert!(super::parse(&table(3)).is_ok());
        let err = super::parse(&table(3_000)).unwrap_err();
        assert!(err.to_string().contains("copies"), "{err}");
    }

    #[test]
    fn a_span_of_a_billion_columns_is_refused_before_its_grid_is_made() {
        let table = |span: &str| {
            format!(
                "<table>{}</table>",
                format!("<tr><td colspan=\"{span}\">x</td></tr><tr><td>a</td></tr>").repeat(50)
            )
        };
        assert!(super::parse(&table("3")).is_ok());
        assert!(super::parse(&table("1000000000")).is_err());
        assert!(super::parse(&table(&usize::MAX.to_string())).is_err());
        // Spans past what a number holds add up without wrapping: the
        // greatest and two more are not one column.
        let wrapped = format!(
            "<table><tr><td colspan=\"{}\">x</td><td colspan=\"2\">y</td></tr><tr><td>a</td></tr></table>",
            usize::MAX
        );
        assert!(super::parse(&wrapped).is_err());
        // Read as a message's body, the same.
        assert!(super::parse_with_quote_depth(&table("1000000000")).is_err());
    }

    #[test]
    fn each_element_spends_the_pages_budget() {
        // Each paragraph is an element: 2 KiB of the budget, whatever the
        // output, DoCO's included.
        let page = |n: usize| "<p>word</p>".repeat(n);
        for output in [Output::Flat, Output::Doco] {
            let read = |n| converting_to(output, || within(&page(n), 4 << 20));
            assert_eq!(read(1_000).unwrap().len(), 1_000);
            let err = read(2_048).unwrap_err().to_string();
            assert!(err.starts_with("elements past"), "{err}");
        }
    }

    #[test]
    fn the_attributes_of_each_tag_are_counted_from_the_tags() {
        // Each attribute is looked for among those before it on its tag,
        // an end tag's too; quoted values hold what would be markup, and
        // comments and a script's text hold no tags.
        let steps = attribute_steps(
            "<!doctype html><div class=\"a>b\" hidden data-x=1><p a b>one</p c d e>\
             <!-- <p a b c> --><script>if (a<b c d) x('<p a b c>')</script>",
        );
        assert_eq!(steps, 3 + 1 + 3);
        assert_eq!(attribute_steps("<p>x</p>"), 0);
    }

    #[test]
    fn the_scan_passes_over_no_text_the_tokenizer_reads_as_tags() {
        // A comment ends at the first place it may, and an element's text
        // up to its end tag is markup in SVG and MathML, and in a frameset,
        // which ignores the tag. Passed over, 100,000 attributes behind
        // `<!-->` or `<svg><title>` took 15 to 19 s.
        let tag = "<p a b c>";
        for before in [
            "<!-->",
            "<!--->",
            "<!-- x --!>",
            "<svg><title>",
            "<svg><plaintext>",
            "<math><style>",
            "<frameset><script>",
            "<SVG><Script>",
        ] {
            assert_eq!(attribute_steps(&format!("{before}{tag}")), 3, "{before}");
        }
        // Before any of them, a script's text is passed over.
        assert_eq!(attribute_steps(&format!("<script>{tag}</script><svg>")), 0);
    }

    #[test]
    fn a_tag_of_many_thousand_attributes_is_refused_before_it_is_parsed() {
        // The parser looks for each among those before it: two million on
        // one tag ran past five minutes.
        let tag = |n: usize| {
            let attrs: Vec<String> = (0..n).map(|i| format!("a{i}=1")).collect();
            format!("<p {}>x</p>", attrs.join(" "))
        };
        // 200,000 took 37 s, and as long behind what the scan of the tags
        // passed over and the tokenizer did not.
        for before in ["", "<!-->", "<svg><title>"] {
            let err = super::parse(&format!("{before}{}", tag(200_000)))
                .unwrap_err()
                .to_string();
            assert!(err.contains("steps"), "{err}");
            assert_linear(
                &format!("refusing a tag of many attributes after {before:?}"),
                50_000,
                |n| format!("{before}{}", tag(n)),
                |page| assert!(super::parse(page).is_err()),
            );
        }
        assert_eq!(super::parse(&tag(1_000)).unwrap().len(), 1);
    }

    /// [`read_within`] a budget of `memory`, its work unbounded, at the
    /// costs of the output converted to.
    fn within(src: &str, memory: u64) -> Result<Vec<Element>, TooLarge> {
        let budget = Budget::new(memory, u64::MAX).for_output(output());
        read_within(src, false, budget).map(|(e, _)| e)
    }

    #[test]
    fn the_tree_is_spent_as_the_parser_builds_it() {
        // Empty tags make a tree twenty times the page: 24 million made
        // 3.9 GB of it from 168 MB.
        let page = "<i></i>".repeat(10_000);
        // Two slots an element, `<html>`, `<head>` and `<body>` among them.
        let need = (10_000 + 3) * 2 * 128;
        assert!(within(&page, need).is_ok());
        let err = within(&page, need - 1).unwrap_err().to_string();
        assert!(err.starts_with("cells past"), "{err}");
    }

    #[test]
    fn a_text_node_spends_its_node_as_the_parser_makes_it() {
        // Each `x` between two `<br>` is a node of the tree, as an element
        // is, spent as the parser makes it.
        let n = 10_000;
        let memory_to_read = |page: &str| {
            let (mut refused, mut read) = (0, MAX_MEMORY);
            while refused + 1 < read {
                let mid = refused + (read - refused) / 2;
                match within(page, mid) {
                    Ok(_) => read = mid,
                    Err(_) => refused = mid,
                }
            }
            read
        };
        let more = memory_to_read(&"<br>x".repeat(n)) - memory_to_read(&"<br>".repeat(n));
        let nodes = (n as u64) * node_slots(1) * fluree_doc_model::budget::CELL_BYTES;
        assert!(more >= nodes, "{more} < {nodes}");
    }

    #[test]
    fn formatting_elements_the_parser_makes_again_are_spent() {
        // Each `<p>` closes the `<b>` left open, and the text after it
        // makes them all again: 400 of them over 30,000 paragraphs, a
        // 124 KB page, made 12 million elements and 2.8 GB, and 1,000 over
        // 20,000 aborted the process.
        let page = |n: usize| {
            let open: String = (0..200).map(|i| format!("<b id=b{i}>")).collect();
            format!("<p>{open}{}", "<p>x".repeat(n))
        };
        // Two slots a node: 200 elements made again a paragraph.
        let memory = 2_000 * 200 * 2 * 128;
        assert!(within(&page(100), memory).is_ok());
        let err = within(&page(2_000), memory).unwrap_err().to_string();
        assert!(err.starts_with("cells past"), "{err}");
    }

    #[test]
    fn nesting_the_parser_walks_is_spent_as_steps() {
        // The parser checks what is in scope by walking the elements open
        // around each one, so nested ones cost the square of their depth:
        // 40,000 took 3 s, 200,000 over two minutes. So do formatting
        // elements it re-makes inside each other, the page closing each
        // one around another left open (`<b><i></b>`), which nest none in
        // the page.
        // Within a sixty-fourth of the steps, as a test is built
        // unoptimized: 5,000 deep is past them, as 23,000 are past all.
        let steps = |page: &str| {
            let budget = Budget::new(MAX_MEMORY, MAX_WORK >> 6);
            read_within(page, false, budget).map(|(e, _)| e)
        };
        let nested = format!("{}x{}", "<div>".repeat(5_000), "</div>".repeat(5_000));
        let misnested: String = (0..5_000)
            .map(|k| format!("<b id={k}><i id={k}></b>"))
            .collect();
        // A repeated `<html>` gives the first its attributes, each looked
        // for among those it has.
        let repeated: String = (0..5_000).map(|k| format!("<html a{k}=1>")).collect();
        for page in [nested, misnested, repeated] {
            let err = steps(&page).unwrap_err().to_string();
            assert!(err.contains("steps"), "{err}");
        }
        assert!(steps(&format!("{}x", "<div>".repeat(1_000))).is_ok());
        // A page of ordinary depth and size is read.
        let ordinary = format!(
            "<div>{}</div>",
            "<section><div><p>Some <b>text</b> and <a href=\"/x\">a link</a>.</p></div></section>"
                .repeat(20_000)
        );
        assert_eq!(super::parse(&ordinary).unwrap().len(), 20_000);
    }

    /// [`read_within`] `work` steps, as [`nesting_the_parser_walks_is_spent_as_steps`].
    fn within_steps(page: &str, work: u64) -> Result<Vec<Element>, TooLarge> {
        read_within(page, false, Budget::new(MAX_MEMORY, work)).map(|(e, _)| e)
    }

    /// The fewest steps `page` is read within.
    fn steps_to_read(page: &str) -> u64 {
        let (mut refused, mut read) = (0, MAX_WORK);
        assert!(within_steps(page, read).is_ok());
        while refused + 1 < read {
            let mid = refused + (read - refused) / 2;
            match within_steps(page, mid) {
                Ok(_) => read = mid,
                Err(_) => refused = mid,
            }
        }
        read
    }

    #[test]
    fn end_tags_that_close_nothing_are_spent_as_steps() {
        // Each looks for an open element of its name down all those open:
        // 20,000 `<span>`, then 200,000 `</zz>`, ran past a minute.
        let page = |close: &str| format!("{}{}", "<span>".repeat(2_000), close.repeat(5_000));
        let err = within_steps(&page("</zz>"), MAX_WORK >> 6)
            .unwrap_err()
            .to_string();
        assert!(err.contains("steps"), "{err}");
        // Refused as soon, however many there are of each.
        assert_linear(
            "end tags that close nothing",
            2_000,
            |n| format!("{}{}", "<span>".repeat(n), "</zz>".repeat(n * 5 / 2)),
            |page| assert!(within_steps(page, MAX_WORK >> 6).is_err()),
        );
        // Closing the innermost each time is read.
        assert!(within_steps(&page("</span>"), MAX_WORK >> 6).is_ok());
    }

    #[test]
    fn formatting_elements_made_again_are_each_looked_for_among_those_open() {
        // Before making again the formatting elements a paragraph closed,
        // the parser looks for each among the elements open, a comparison
        // of two nodes each.
        let page = |depth: usize| {
            let held: String = (0..50).map(|i| format!("<i id={i}>")).collect();
            format!("{}<p>{held}x{}", "<span>".repeat(depth), "<p>x".repeat(100))
        };
        let err = within_steps(&page(2_000), MAX_WORK >> 6)
            .unwrap_err()
            .to_string();
        assert!(err.contains("steps"), "{err}");
        assert!(within_steps(&page(20), MAX_WORK >> 6).is_ok());
    }

    #[test]
    fn a_node_set_before_its_sibling_spends_the_siblings_looked_at() {
        // What a table holds out of place is set before it, found among
        // its siblings: 500,000 among 200,000 took 105 s.
        let (before, inputs) = (6_400, 1_000);
        let paragraphs = "<p>x</p>".repeat(before);
        let set_before = format!("{paragraphs}<table>{}", "<input>".repeat(inputs));
        let after = format!("{paragraphs}<table></table>{}", "<input>".repeat(inputs));
        let looked_at = (before / SIBLINGS_A_STEP as usize * inputs) as u64;
        let more = steps_to_read(&set_before) - steps_to_read(&after);
        assert!(more >= looked_at, "{more} < {looked_at}");
    }

    #[test]
    fn the_children_a_formatting_element_takes_are_spent() {
        // Closing the `<b>` around a block gives the block's children to a
        // new `<b>` inside it, each moved.
        let moved = 10_000;
        let page = |close: &str| format!("<b><div>{}{close}", "<br>".repeat(moved));
        let more = steps_to_read(&page("</b>")) - steps_to_read(&page("</i>"));
        assert!(more >= moved as u64, "{more} < {moved}");
    }

    #[test]
    fn formatting_elements_compared_by_their_attributes_are_spent() {
        // Each start tag is compared with each formatting element of its
        // name the parser holds, attributes copied and sorted: 5,000 `<b>`
        // of 100 attributes took over two minutes, as did a million end
        // tags looking past 20,000 `<i>` the parser holds, closed.
        let attrs: String = (0..40).map(|k| format!(" a{k}")).collect();
        let compared =
            |n: usize| -> String { (0..n).map(|i| format!("<b id={i}{attrs}>")).collect() };
        let held = |n: usize| -> String { (0..n).map(|i| format!("<i id={i}>")).collect() };
        let looked_past = |held: &str, n: usize| format!("<p>{held}</p>{}", "</b>".repeat(n));
        for page in [compared(1_000), looked_past(&held(500), 10_000)] {
            let err = within_steps(&page, MAX_WORK >> 6).unwrap_err().to_string();
            assert!(err.contains("steps"), "{err}");
        }
        assert!(within_steps(&looked_past(&held(500), 1_000), MAX_WORK >> 6).is_ok());
        // Refused as soon, however many there are.
        assert_linear("start tags compared", 1_000, compared, |page| {
            assert!(within_steps(page, MAX_WORK >> 6).is_err())
        });
        assert_linear(
            "end tags looking past those held",
            10_000,
            |n| looked_past(&held(n / 20), n),
            |page| assert!(within_steps(page, MAX_WORK >> 6).is_err()),
        );
        // Formatting elements closed, or left open around a mail's
        // paragraphs, are read.
        let closed = "<p><font face=Arial size=2 color=#1F497D><b class=x>a</b> b</font></p>";
        let open: String = (0..50)
            .map(|i| format!("<font face=Arial size=2 color=#{i:06x}><p>a"))
            .collect();
        let ordinary = format!("{}{open}{}", closed.repeat(5_000), "<p>b".repeat(5_000));
        assert!(within_steps(&ordinary, MAX_WORK >> 6).is_ok());
    }

    #[test]
    fn a_page_nested_past_the_depth_a_browser_keeps_is_read_flat() {
        // A batch's worker has a 2 MiB stack: 1,000 tables nested, 33 KB,
        // overflowed it and aborted the batch. Past 512 levels the page's
        // elements are set beside their ancestor, as a browser sets them,
        // and the walk goes no deeper.
        let tables = |n: usize| {
            format!(
                "{}x{}",
                "<table><tr><td>".repeat(n),
                "</td></tr></table>".repeat(n)
            )
        };
        let divs = |n: usize| format!("{}deep{}", "<div>".repeat(n), "</div>".repeat(n));
        let read = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                let (dom, _) = build(&tables(1_000), Budget::default()).unwrap();
                let deepest = {
                    let mut stack = vec![(dom.document.clone(), 0)];
                    let mut deepest = 0;
                    while let Some((h, d)) = stack.pop() {
                        deepest = deepest.max(d);
                        stack.extend(h.children.borrow().iter().map(|c| (c.clone(), d + 1)));
                    }
                    deepest
                };
                (
                    deepest,
                    super::parse(&tables(1_000)).unwrap(),
                    super::parse(&divs(520)).unwrap(),
                    super::parse(&tables(100)).unwrap(),
                )
            })
            .unwrap()
            .join()
            .unwrap();
        let (deepest, tables, divs, shallow) = read;
        assert_eq!(deepest, MAX_DEPTH);
        assert!(tables.iter().any(|e| e.text.contains('x')));
        assert_eq!(texts(&divs), ["deep"]);
        assert!(shallow.iter().any(|e| e.text.contains('x')));
    }

    #[test]
    fn each_link_is_spent_before_it_is_kept() {
        // Four cell slots a link, and its target's text, in every output:
        // DoCO writes each as a node of its own, a link at a time, and
        // holds no more of it. Against the same tags with an attribute
        // that makes no link, 1,000 links cost exactly that more.
        let page = |attribute: &str| {
            format!(
                "<p>{}</p>",
                format!("<a {attribute}=b>a</a> ").repeat(1_000)
            )
        };
        let (linked, titled) = (page("href"), page("title"));
        for output in [Output::Flat, Output::Doco] {
            let least = |page: &str| {
                let (mut lo, mut hi) = (0u64, 64 << 20);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    match converting_to(output, || within(page, mid)) {
                        Ok(_) => hi = mid,
                        Err(_) => lo = mid + 1,
                    }
                }
                lo
            };
            let links = converting_to(output, || within(&linked, 64 << 20)).unwrap();
            assert_eq!(links[0].links.as_ref().unwrap().len(), 1_000);
            assert_eq!(
                least(&linked) - least(&titled),
                1_000 * (4 * CELL_BYTES + TEXT_BYTES)
            );
        }
    }

    #[test]
    fn a_table_written_once_spends_its_cells_not_an_element_each() {
        // Markdown writes a cell that holds text once, and DoCO as a node of
        // its own, a node at a time: neither holds an element's worth of
        // it. 50,000 rows of 30 cells, 1.5 million, were refused as
        // Markdown, which takes about 510 bytes a cell.
        let row = format!("<tr>{}</tr>", "<td>12.50</td>".repeat(10));
        let page = format!("<table>{}</table>", row.repeat(2_000));
        let read = |output| converting_to(output, || within(&page, 32 << 20));
        assert_eq!(read(Output::Flat).unwrap().len(), 1);
        assert_eq!(read(Output::Doco).unwrap().len(), 1);
        // Written once, a cell that holds text costs what a sheet's does, at
        // the output its budget counts at, whatever the thread converts to.
        let mut e = element("doco:Table", String::new(), None);
        e.cells = Some(vec![vec!["a".into(), String::new()]]);
        let need = ELEMENT_BYTES + FLAT_CELL_BYTES + TEXT_BYTES;
        let spend = |memory| spend_table(&mut Budget::new(memory, 0).for_output(Output::Flat), &e);
        assert!(spend(need).is_ok());
        assert!(spend(need - 1).is_err());
    }

    #[test]
    fn a_long_run_of_inline_elements_is_read_in_linear_time() {
        // Each element counted the characters of the run before it, for a
        // link's offsets: a million `<span>` took 12 s.
        let word = "é".repeat(100);
        let page = |n: usize| {
            format!(
                "<div>{}<a href=\"/é\">çà</a> ü</div>",
                format!("<span>{word}</span>").repeat(n)
            )
        };
        assert_linear("a run of spans", 5_000, page, |page| {
            assert_eq!(super::parse(page).unwrap().len(), 1);
        });
        let n = 5_000;
        let els = super::parse(&page(n)).unwrap();
        let n = n * 100;
        assert_eq!(els.len(), 1);
        // The link is where its characters are, past the multi-byte ones.
        let l = &els[0].links.as_ref().unwrap()[0];
        assert_eq!(l.span(), Some((n, n + 2)));
        assert_eq!(
            els[0].text.chars().skip(n).take(2).collect::<String>(),
            "çà"
        );
    }
    #[test]
    fn the_walk_asks_each_element_once_what_is_under_and_above_it() {
        // Whether an element held a block was a walk down it at each level
        // of the walk above it, and whether an `<aside>` was in an article
        // a walk up its ancestors: 500 `<span>` around a million `<br>x`
        // took 54 s, a million `<aside>` 500 elements deep 18 s.
        // Deeper and longer together: either walk alone grows with their
        // product.
        assert_linear(
            "inline elements around many lines",
            10_000,
            |n| {
                format!(
                    "{}{}<div>y</div>",
                    "<span>".repeat(n / 100),
                    "<br>x".repeat(n)
                )
            },
            |page| assert!(super::parse(page).is_ok()),
        );
        assert_linear(
            "asides deep down",
            40_000,
            |n| {
                format!(
                    "{}<table><tr><td>{}",
                    "<div>".repeat(n / 400),
                    "<aside></aside>".repeat(n)
                )
            },
            |page| assert!(super::parse(page).is_ok()),
        );
        // What is under and above is read as it was.
        let els = parse(
            "<section><div><span><aside><p>Factbox.</p></aside></span></div></section>\
             <div><span><span><aside><p>Sidebar.</p></aside></span></span></div>\
             <span><b>Loose <i>run</i></b></span><span><em><div>Block</div></em></span>\
             <div>One <span>run<script>x</script> with</span> a script</div>",
        );
        assert_eq!(
            texts(&els),
            ["Factbox.", "Loose run", "Block", "One run with a script"]
        );
    }

    #[test]
    fn a_text_past_the_budget_is_refused_before_it_is_made() {
        // Made, then spent, with a map of each of its bytes to place its
        // links by, a paragraph took 20 bytes for each of its own before it
        // was spent: one of 193 MB aborted the process under 4 GB.
        let word = "abcdefgh ";
        let page = format!("<p>{}</p>", word.repeat(10_000));
        let (dom, budget) = build(&page, Budget::default()).unwrap();
        let p = {
            let mut stack = vec![dom.document.clone()];
            loop {
                let h = stack.pop().unwrap();
                if tag_of(&h).as_deref() == Some("p") {
                    break h;
                }
                stack.extend(children_of(&h));
            }
        };
        // Its text, collapsed: the last space is dropped.
        let cost = (word.len() * 10_000 - 1) as u64 * TEXT_BYTES;
        let spent = MAX_MEMORY - budget.memory_left();
        let gather = |memory: u64| {
            let mut out = Out {
                budget: Budget::new(memory, u64::MAX),
                facts: Facts::of(&dom.document),
                ..Out::default()
            };
            let made = Cell::new(false);
            let text = out.text(|g| {
                made.set(made.get() || g.text.is_some());
                g.children(&p);
            });
            (text.map(|(t, _)| t.len()), made.get())
        };
        assert_eq!(gather(cost), (Some(word.len() * 10_000 - 1), true));
        assert_eq!(gather(cost - 1), (None, false));
        // And the page with it, as a page.
        let err = within(&page, spent + cost - 1).unwrap_err().to_string();
        assert!(err.starts_with("text past"), "{err}");
    }

    #[test]
    fn anchors_are_placed_as_their_text_is_gathered() {
        // An anchor of whitespace alone spans nothing; one whose text ends
        // in whitespace ends at its last character; anchors in a list
        // item's own text are its own.
        let els = parse(
            "<p>a <a href=\"/w\"> </a> b <a href=\"/x\">c d </a> e</p>\
             <ul><li>Go <a href=\"/y\">here</a><ul><li><a href=\"/z\">in</a></li></ul></li></ul>",
        );
        assert_eq!(els[0].text, "a b c d e");
        let spans = |e: &Element| -> Vec<(Option<(usize, usize)>, String)> {
            e.links
                .iter()
                .flatten()
                .map(|l| (l.span(), l.href().to_string()))
                .collect()
        };
        assert_eq!(spans(&els[0]), [(Some((4, 7)), "/x".to_string())]);
        assert_eq!(els[1].text, "Go here");
        assert_eq!(spans(&els[1]), [(Some((3, 7)), "/y".to_string())]);
        assert_eq!(spans(&els[2]), [(Some((0, 2)), "/z".to_string())]);
    }

    #[test]
    fn raised_marks_are_told_in_time_linear_in_the_page() {
        // Each raised element's text was gathered whole, and nested ones
        // each gathered again what the one inside them holds; and each was
        // told by all the text before it, a word of a million raised
        // digits looked at again at each.
        // Deeper and longer together, the nested ones.
        assert_linear(
            "nested raised elements",
            4_000,
            |n| {
                format!(
                    "<p>Revenue{}{}{}</p>",
                    "<sup>".repeat(n / 40),
                    "<br>x".repeat(n),
                    "</sup>".repeat(n / 40)
                )
            },
            |page| assert_eq!(parse(page).len(), 1),
        );
        assert_linear(
            "a word of raised digits",
            5_000,
            |n| format!("<p>\u{3c1}w{}</p>", "<sup>2</sup>".repeat(n)),
            |page| assert_eq!(parse(page).len(), 1),
        );
        // A mark is still told apart, and a power kept with its value.
        assert_eq!(
            texts(&parse(
                "<p>Revenue<sup>1</sup> rose; 10<sup>6</sup> m<sup>2</sup></p>"
            )),
            ["Revenue 1 rose; 106 m2"]
        );
    }

    #[test]
    fn a_pre_past_the_budget_is_refused_before_it_is_made() {
        let page = format!("<pre>{}</pre>", "abcdefgh\n".repeat(10_000));
        let (dom, _) = build(&page, Budget::default()).unwrap();
        let pre = {
            let mut stack = vec![dom.document.clone()];
            loop {
                let h = stack.pop().unwrap();
                if tag_of(&h).as_deref() == Some("pre") {
                    break h;
                }
                stack.extend(children_of(&h));
            }
        };
        let facts = Facts::of(&dom.document);
        // Counted as it is drawn, a line break at the cost of a control
        // character: no less than what is made, the last line break left
        // out of it.
        let cost = pre_cost(&pre, &facts);
        assert_eq!(cost, 80_000 + 10_000 * 6);
        assert_eq!(pre_text(&pre, &facts).0.len(), 89_999);
        let read = |memory: u64| {
            let mut out = Out {
                budget: Budget::new(memory, u64::MAX),
                facts: Facts::of(&dom.document),
                ..Out::default()
            };
            out.pre(&pre);
            (out.elements.len(), out.over.is_some())
        };
        assert_eq!(read(2 * cost * TEXT_BYTES), (1, false));
        assert_eq!(read(cost * TEXT_BYTES - 1), (0, true));
    }
}
