//! A part's XML as a small tree, built within bounds.
//!
//! The ODF readers walk their content as structure (a list inside a list
//! item, a table inside a frame), which a tree states plainly. Elements and
//! attributes are kept by local name: OpenDocument's prefixes are a
//! convention, not a contract.
//!
//! The tree is held while the document's elements are made from it, so it
//! is spent from the document's [`Budget`] as it is built, in cell slots
//! ([`Budget::spend_cells`]): [`NODE_CELLS`] a node, [`ATTR_CELLS`] an
//! attribute, and one for each [`CELL_BYTES`] of text and values a node
//! holds. Four bytes of XML (`<a/>`) are a node, and 9 MB of `<a b=""/>`
//! took 376 MB as a tree.

use fluree_doc_model::budget::{Budget, CELL_BYTES};
use quick_xml::events::Event;
use quick_xml::Reader;

/// What a node costs, in cell slots: about 140 bytes were measured a node
/// (its record, its name, its place in its parent's children), 160 for a
/// node of text.
pub const NODE_CELLS: u64 = 2;
/// What an attribute costs, in cell slots: about 240 bytes were measured
/// for an attribute with an empty value (its pair of strings, their
/// allocations), its value's text counted apart.
pub const ATTR_CELLS: u64 = 2;

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

/// Spend what `node` costs from `budget`, before it is kept.
fn spend(budget: &mut Budget, node: &Node) -> Result<(), String> {
    let attrs = node.attrs.len() as u64;
    let bytes = node.text.len() + node.attrs.iter().map(|(_, v)| v.len()).sum::<usize>();
    budget
        .spend_cells(NODE_CELLS + attrs * ATTR_CELLS + (bytes as u64).div_ceil(CELL_BYTES))
        .map_err(|e| e.to_string())
}

impl Tree {
    /// Parse `xml`, each node spent from `budget` before it is kept; `Err`
    /// past a bound, naming it. A part that stops parsing keeps what was
    /// read before it.
    pub fn parse(xml: &str, budget: &mut Budget) -> Result<Tree, String> {
        let mut t = Tree {
            nodes: vec![Node::default()],
            ..Tree::default()
        };
        let mut stack: Vec<usize> = vec![0];
        let mut r = Reader::from_str(xml);
        r.config_mut().trim_text(false);
        let mut buf = Vec::new();
        loop {
            let ev = r.read_event_into(&mut buf);
            if t.nodes.len() > MAX_NODES {
                return Err(format!("more than {MAX_NODES} XML nodes"));
            }
            let parent = *stack.last().unwrap_or(&0);
            match ev {
                Err(_) | Ok(Event::Eof) => break,
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    let empty = matches!(ev, Ok(Event::Empty(_)));
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
                    spend(budget, &node)?;
                    let id = t.nodes.len();
                    t.nodes.push(node);
                    t.nodes[parent].children.push(id);
                    if !empty {
                        if stack.len() > MAX_DEPTH {
                            return Err(format!("elements nested more than {MAX_DEPTH} deep"));
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
                    let s = x.unescape().map(|s| s.into_owned()).unwrap_or_default();
                    if !s.is_empty() {
                        let node = Node {
                            text: s,
                            ..Node::default()
                        };
                        spend(budget, &node)?;
                        let id = t.nodes.len();
                        t.nodes.push(node);
                        t.nodes[parent].children.push(id);
                    }
                }
                Ok(Event::CData(x)) => {
                    let node = Node {
                        text: String::from_utf8_lossy(&x).into_owned(),
                        ..Node::default()
                    };
                    spend(budget, &node)?;
                    let id = t.nodes.len();
                    t.nodes.push(node);
                    t.nodes[parent].children.push(id);
                }
                _ => {}
            }
            buf.clear();
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
