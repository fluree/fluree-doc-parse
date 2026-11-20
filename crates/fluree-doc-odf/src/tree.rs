//! A part's XML as a small tree, built within bounds.
//!
//! The ODF readers walk their content as structure (a list inside a list
//! item, a table inside a frame), which a tree states plainly. Elements and
//! attributes are kept by local name: OpenDocument's prefixes are a
//! convention, not a contract.

use quick_xml::events::Event;
use quick_xml::Reader;

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

impl Tree {
    /// Parse `xml`; `Err` past a bound, naming it. A part that stops
    /// parsing keeps what was read before it.
    pub fn parse(xml: &str) -> Result<Tree, String> {
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
                    let id = t.nodes.len();
                    t.nodes.push(Node {
                        name: local(e.name().as_ref()),
                        attrs,
                        ..Node::default()
                    });
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
                        let id = t.nodes.len();
                        t.nodes.push(Node {
                            text: s,
                            ..Node::default()
                        });
                        t.nodes[parent].children.push(id);
                    }
                }
                Ok(Event::CData(x)) => {
                    let id = t.nodes.len();
                    t.nodes.push(Node {
                        text: String::from_utf8_lossy(&x).into_owned(),
                        ..Node::default()
                    });
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
