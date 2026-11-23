//! A part's XML as a small tree, built within bounds.
//!
//! The ODF readers walk their content as structure (a list inside a list
//! item, a table inside a frame), which a tree states plainly. Elements and
//! attributes are kept by local name: OpenDocument's prefixes are a
//! convention, not a contract.
//!
//! The tree is held while the document's elements are made from it, so it
//! is spent from the document's [`Budget`] as it is built, each node before
//! it is copied out of the part, in cell slots ([`Budget::spend_cells`]):
//! [`NODE_CELLS`] a node, [`ATTR_CELLS`] an attribute and once more for a
//! node's list of them, and one for each [`CELL_BYTES`] of the XML it is
//! made from (a tag's name and attributes, a text), which is no shorter
//! than what it holds. Four bytes of XML
//! (`<a/>`) are a node, and 9 MB of `<a b=""/>` took 376 MB as a tree.

use fluree_doc_model::budget::{Budget, TooLarge, CELL_BYTES};
use quick_xml::events::Event;
use quick_xml::Reader;

/// What a node costs, in cell slots, beside the one or more its XML costs:
/// about 140 bytes were measured a node (its record, its name, its place in
/// its parent's children), 160 for a node of text, 240 at most in address
/// space, its record in a vector that reserves up to twice what it holds.
pub const NODE_CELLS: u64 = 1;
/// What an attribute costs, in cell slots, and once more for a node's
/// first, which makes its list of them: about 90 bytes were measured an
/// attribute (its pair of strings, their allocations), its value's text
/// counted apart, and 260 the first. A million `<a b=""/>` took 370 bytes
/// a node, counted at 512. A table cell of an `.odt` with three
/// (`<table:table-cell table:style-name="T.A1" office:value-type="string"
/// calcext:value-type="string">`), its paragraph and its text, a million of
/// them, took 1.03 GB from the part through Markdown, counted at 1.98 GB
/// with this and the cell's own cost.
pub const ATTR_CELLS: u64 = 1;

/// The most nodes a part's tree holds.
pub const MAX_NODES: usize = 4 << 20;
/// The deepest nesting followed.
pub const MAX_DEPTH: usize = 256;

#[derive(Debug, Default)]
pub struct Node {
    /// Local name; empty for a text node.
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<usize>,
    /// A text node's text.
    pub text: String,
}

#[derive(Debug, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    /// What each paragraph style makes a paragraph: a heading at a level,
    /// or the document's title ([`crate::style_roles`]).
    pub roles: std::collections::HashMap<String, Role>,
}

/// What a paragraph style makes a paragraph set in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Heading(usize),
    Title,
}

fn local(q: &[u8]) -> String {
    let s = String::from_utf8_lossy(q);
    s.rsplit(':').next().unwrap_or(&s).to_string()
}

/// Spend what a node of `attrs` attributes costs from `budget`, `bytes` the
/// XML it is made from (a tag's name and attributes, a text's escaped
/// text), which is no shorter than what it holds: before any of it is
/// copied out of the part.
fn spend(budget: &mut Budget, attrs: u64, bytes: usize) -> Result<(), TooLarge> {
    let attrs = (attrs + u64::from(attrs > 0)) * ATTR_CELLS;
    budget.spend_cells(NODE_CELLS + attrs + (bytes as u64).div_ceil(CELL_BYTES))
}

impl Tree {
    /// Parse `xml`, each node spent from `budget` before it is copied out
    /// of it; `Err` past a bound, naming it. A part that stops parsing
    /// keeps what was read before it. Its events are read in place, not
    /// copied: a run of text can be as long as the part.
    pub fn parse(xml: &str, budget: &mut Budget) -> Result<Tree, TooLarge> {
        let mut t = Tree {
            nodes: vec![Node::default()],
            ..Tree::default()
        };
        let mut stack: Vec<usize> = vec![0];
        let mut r = Reader::from_str(xml);
        r.config_mut().trim_text(false);
        loop {
            let ev = r.read_event();
            if t.nodes.len() > MAX_NODES {
                return Err(TooLarge::fixed(format!("more than {MAX_NODES} XML nodes")));
            }
            let parent = *stack.last().unwrap_or(&0);
            match ev {
                Err(_) | Ok(Event::Eof) => break,
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    let empty = matches!(ev, Ok(Event::Empty(_)));
                    let n = fluree_doc_ooxml::attributes(e).count() as u64;
                    spend(budget, n, e.len())?;
                    let attrs = fluree_doc_ooxml::attributes(e)
                        .map(|a| {
                            let v = a
                                .unescape_value()
                                .map(|v| v.into_owned())
                                .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned());
                            (local(a.key.as_ref()), v)
                        })
                        .collect();
                    let node = Node {
                        name: local(e.name().as_ref()),
                        attrs,
                        ..Node::default()
                    };
                    let id = t.nodes.len();
                    t.nodes.push(node);
                    t.nodes[parent].children.push(id);
                    if !empty {
                        if stack.len() > MAX_DEPTH {
                            return Err(TooLarge::fixed(format!(
                                "elements nested more than {MAX_DEPTH} deep"
                            )));
                        }
                        stack.push(id);
                    }
                }
                Ok(Event::End(_)) => {
                    if stack.len() > 1 {
                        stack.pop();
                    }
                }
                Ok(Event::Text(x)) => {
                    if x.is_empty() {
                        continue;
                    }
                    spend(budget, 0, x.len())?;
                    let s = x.unescape().map(|s| s.into_owned()).unwrap_or_default();
                    if !s.is_empty() {
                        let node = Node {
                            text: s,
                            ..Node::default()
                        };
                        let id = t.nodes.len();
                        t.nodes.push(node);
                        t.nodes[parent].children.push(id);
                    }
                }
                Ok(Event::CData(x)) => {
                    spend(budget, 0, x.len())?;
                    let node = Node {
                        text: String::from_utf8_lossy(&x).into_owned(),
                        ..Node::default()
                    };
                    let id = t.nodes.len();
                    t.nodes.push(node);
                    t.nodes[parent].children.push(id);
                }
                _ => {}
            }
        }
        Ok(t)
    }

    pub fn node(&self, id: usize) -> &Node {
        &self.nodes[id]
    }

    pub fn attr(&self, id: usize, name: &str) -> Option<&str> {
        self.nodes[id]
            .attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The element children of `id`.
    pub fn elements(&self, id: usize) -> impl Iterator<Item = usize> + '_ {
        self.nodes[id]
            .children
            .iter()
            .copied()
            .filter(|&c| !self.nodes[c].name.is_empty())
    }

    /// The first descendant of `id` named `name`, depth first.
    pub fn find(&self, id: usize, name: &str) -> Option<usize> {
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if n != id && self.nodes[n].name == name {
                return Some(n);
            }
            stack.extend(self.nodes[n].children.iter().rev().copied());
        }
        None
    }

    /// The characters of the text under `id`, as is, read in place.
    pub fn text_chars(&self, id: usize) -> impl Iterator<Item = char> + '_ {
        let mut stack = vec![id];
        std::iter::from_fn(move || {
            let n = stack.pop()?;
            stack.extend(self.nodes[n].children.iter().rev().copied());
            Some(self.nodes[n].text.chars())
        })
        .flatten()
    }

    /// All the text under `id`, as is.
    pub fn all_text(&self, id: usize) -> String {
        let mut s = String::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            s.push_str(&self.nodes[n].text);
            stack.extend(self.nodes[n].children.iter().rev().copied());
        }
        s
    }
}
