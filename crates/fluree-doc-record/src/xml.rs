//! An XML record read into fields.
//!
//! A record is a tree used as a form: elements that name a field and hold
//! its value. Every element holding text is a field and so is every
//! attribute, in document order. The structure above them is where they
//! are, and is kept in their path.

use crate::record::{Field, Record, Syntax};
use crate::RecordError;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::collections::HashMap;

struct Node {
    name: String,
    parent: Option<usize>,
    /// 1-based position among the siblings of the same name, as XPath
    /// counts them.
    nth: usize,
    attributes: Vec<(String, String)>,
    /// The element's own text: what it holds between its tags, outside any
    /// child element.
    text: Vec<String>,
}

/// Read XML text into a record.
///
/// Mixed content, text with elements in it, is read as the element's own
/// text followed by its children: an element is a field or a container of
/// fields, and markup inside a value is a document's business, which this
/// is not the reader for.
pub fn read(text: &str) -> Result<Record, RecordError> {
    let nodes = nodes(text)?;
    if nodes.is_empty() {
        return Err(RecordError::NotARecord("the document has no element"));
    }
    // An index is written only where the source has more than one: a
    // reader addresses `/record/title`, and `/record/title[1]` would make
    // every path in every record carry a number that says nothing.
    let mut siblings: HashMap<(Option<usize>, &str), usize> = HashMap::new();
    for n in &nodes {
        *siblings.entry((n.parent, n.name.as_str())).or_default() += 1;
    }
    let path_of = |mut at: usize| -> (String, String) {
        let mut steps: Vec<(String, &str)> = Vec::new();
        loop {
            let n = &nodes[at];
            let step = if siblings[&(n.parent, n.name.as_str())] > 1 {
                format!("{}[{}]", n.name, n.nth)
            } else {
                n.name.clone()
            };
            steps.push((step, n.name.as_str()));
            match n.parent {
                Some(p) => at = p,
                None => break,
            }
        }
        steps.reverse();
        let path = steps.iter().fold(String::new(), |mut acc, (step, _)| {
            acc.push('/');
            acc.push_str(step);
            acc
        });
        // The name leaves the root out: every field of a record is under
        // it, and a format that declares `title` means the record's.
        let name = steps[1..]
            .iter()
            .map(|(_, name)| *name)
            .collect::<Vec<_>>()
            .join("/");
        (path, name)
    };

    let mut fields = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        let (path, name) = path_of(i);
        for (attribute, value) in &n.attributes {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            fields.push(Field {
                path: format!("{path}/@{attribute}"),
                name: if name.is_empty() {
                    format!("@{attribute}")
                } else {
                    format!("{name}/@{attribute}")
                },
                value: value.to_string(),
            });
        }
        let value = n.text.join("\n");
        let value = value.trim();
        if !value.is_empty() {
            fields.push(Field {
                path,
                // The root's own text has no name under the root but its own.
                name: if name.is_empty() {
                    n.name.clone()
                } else {
                    name
                },
                value: value.to_string(),
            });
        }
    }
    Ok(Record {
        syntax: Syntax::Xml,
        root: nodes[0].name.clone(),
        fields,
        tracks: Vec::new(),
    })
}

/// The root element's name, read from the head of the text alone.
///
/// What a reader needs to decide whether a file is a record at all, before
/// reading megabytes of it.
pub fn root_name(text: &str) -> Option<String> {
    let mut reader = Reader::from_str(text);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => return Some(name_of(&e)),
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

fn name_of(e: &BytesStart<'_>) -> String {
    String::from_utf8_lossy(e.name().as_ref()).into_owned()
}

fn nodes(text: &str) -> Result<Vec<Node>, RecordError> {
    let mut reader = Reader::from_str(text);
    let mut nodes: Vec<Node> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    let mut counts: HashMap<(Option<usize>, String), usize> = HashMap::new();
    let mut closed_root = false;
    loop {
        let event = reader
            .read_event()
            .map_err(|e| RecordError::Malformed(e.to_string()))?;
        match event {
            Event::Start(e) | Event::Empty(e) if closed_root => {
                let _ = e;
                return Err(RecordError::Malformed(
                    "more than one root element".to_string(),
                ));
            }
            Event::Start(e) => {
                let at = push(&mut nodes, &mut counts, open.last().copied(), &e);
                open.push(at);
            }
            Event::Empty(e) => {
                push(&mut nodes, &mut counts, open.last().copied(), &e);
                closed_root = open.is_empty();
            }
            Event::Text(t) => {
                if let Some(&at) = open.last() {
                    // An entity the document declares itself is left as it
                    // is written rather than failing the whole record.
                    let s = match t.unescape() {
                        Ok(s) => s.into_owned(),
                        Err(_) => String::from_utf8_lossy(&t).into_owned(),
                    };
                    if !s.trim().is_empty() {
                        nodes[at].text.push(s.trim().to_string());
                    }
                }
            }
            Event::CData(t) => {
                if let Some(&at) = open.last() {
                    let s = String::from_utf8_lossy(&t).into_owned();
                    if !s.trim().is_empty() {
                        nodes[at].text.push(s.trim().to_string());
                    }
                }
            }
            Event::End(_) => {
                open.pop();
                closed_root = open.is_empty();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !open.is_empty() {
        return Err(RecordError::Malformed(format!(
            "<{}> is never closed",
            nodes[*open.last().unwrap()].name
        )));
    }
    Ok(nodes)
}

fn push(
    nodes: &mut Vec<Node>,
    counts: &mut HashMap<(Option<usize>, String), usize>,
    parent: Option<usize>,
    e: &BytesStart<'_>,
) -> usize {
    let name = name_of(e);
    let nth = counts.entry((parent, name.clone())).or_default();
    *nth += 1;
    let attributes = e
        .attributes()
        .flatten()
        // A namespace declaration says how to read names, and is no field.
        .filter(|a| {
            let k = a.key.as_ref();
            k != b"xmlns" && !k.starts_with(b"xmlns:")
        })
        .map(|a| {
            let value = match a.unescape_value() {
                Ok(v) => v.into_owned(),
                Err(_) => String::from_utf8_lossy(&a.value).into_owned(),
            };
            (String::from_utf8_lossy(a.key.as_ref()).into_owned(), value)
        })
        .collect();
    nodes.push(Node {
        name,
        parent,
        nth: *nth,
        attributes,
        text: Vec::new(),
    });
    nodes.len() - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(xml: &str) -> Vec<(String, String, String)> {
        read(xml)
            .unwrap()
            .fields
            .into_iter()
            .map(|f| (f.path, f.name, f.value))
            .collect()
    }

    fn f(path: &str, name: &str, value: &str) -> (String, String, String) {
        (path.into(), name.into(), value.into())
    }

    #[test]
    fn a_flat_record_is_its_fields_in_order() {
        let xml = "<?xml version='1.0' encoding='utf-16'?>\n<record>\n  <id>48213</id>\n  \
                   <title>Budget adopté</title>\n  <subject_fr>null</subject_fr>\n  \
                   <empty/>\n  <blank>  </blank>\n</record>";
        let r = read(xml).unwrap();
        assert_eq!(r.root, "record");
        assert_eq!(r.syntax, Syntax::Xml);
        assert_eq!(
            fields(xml),
            [
                f("/record/id", "id", "48213"),
                f("/record/title", "title", "Budget adopté"),
                // `null` is a value until a format says it is none.
                f("/record/subject_fr", "subject_fr", "null"),
            ]
        );
        assert!(r.has("title") && !r.has("empty"));
        assert_eq!(r.first("id"), Some("48213"));
    }

    #[test]
    fn a_value_reads_as_written_not_as_escaped() {
        assert_eq!(
            fields("<r><a>Q&amp;A &lt;live&gt; &#233;t&#xE9;</a><b><![CDATA[1 < 2 & 3]]></b></r>"),
            [
                f("/r/a", "a", "Q&A <live> été"),
                f("/r/b", "b", "1 < 2 & 3"),
            ]
        );
    }

    #[test]
    fn nested_and_repeated_fields_keep_one_name_and_their_own_paths() {
        assert_eq!(
            fields(
                "<r><meta><author>Ada</author></meta>\
                 <tags><tag>a</tag><tag>b</tag></tags><tags><tag>c</tag></tags></r>"
            ),
            [
                f("/r/meta/author", "meta/author", "Ada"),
                f("/r/tags[1]/tag[1]", "tags/tag", "a"),
                f("/r/tags[1]/tag[2]", "tags/tag", "b"),
                f("/r/tags[2]/tag", "tags/tag", "c"),
            ]
        );
    }

    #[test]
    fn attributes_are_fields_and_namespace_declarations_are_not() {
        assert_eq!(
            fields(
                "<r xmlns=\"urn:x\" xmlns:d=\"urn:d\" version=\"2\">\
                 <item id=\"7\" d:lang=\"fr\">Texte</item></r>"
            ),
            [
                f("/r/@version", "@version", "2"),
                f("/r/item/@id", "item/@id", "7"),
                f("/r/item/@d:lang", "item/@d:lang", "fr"),
                f("/r/item", "item", "Texte"),
            ]
        );
    }

    #[test]
    fn mixed_content_is_the_element_s_text_then_its_children() {
        assert_eq!(
            fields("<r><body>Before <b>bold</b> after</body></r>"),
            [
                f("/r/body", "body", "Before\nafter"),
                f("/r/body/b", "body/b", "bold"),
            ]
        );
    }

    #[test]
    fn what_is_not_well_formed_is_not_read() {
        assert!(matches!(
            read("<r><a>1</b></r>"),
            Err(RecordError::Malformed(_))
        ));
        assert!(matches!(
            read("<r><a>1</a>"),
            Err(RecordError::Malformed(_))
        ));
        assert!(matches!(read("<r/><s/>"), Err(RecordError::Malformed(_))));
        assert!(matches!(read("  "), Err(RecordError::NotARecord(_))));
    }

    #[test]
    fn the_root_is_read_without_reading_the_record() {
        assert_eq!(
            root_name("<?xml version='1.0'?><!-- c --><AXFRoot><MAObject>").as_deref(),
            Some("AXFRoot")
        );
        assert_eq!(root_name("no markup"), None);
    }
}
