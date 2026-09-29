//! A JSON record read into fields.
//!
//! An object used as a form: every string, number and boolean in it is a
//! field, addressed by its JSON Pointer and named by the keys that lead to
//! it. `null` is the notation's own way of saying there is no value, so it
//! is no field.

use crate::record::{Field, Record, Syntax};
use crate::RecordError;
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// A JSON value with its members in the order the source wrote them.
///
/// `serde_json::Value` sorts an object's keys unless the whole build turns
/// a feature on, and a record shown to a person is shown in its own order.
enum Node {
    Null,
    Leaf(String),
    Object(Vec<(String, Node)>),
    Array(Vec<Node>),
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Node;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_unit<E>(self) -> Result<Node, E> {
                Ok(Node::Null)
            }
            fn visit_bool<E>(self, v: bool) -> Result<Node, E> {
                Ok(Node::Leaf(v.to_string()))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Node, E> {
                Ok(Node::Leaf(v.to_string()))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Node, E> {
                Ok(Node::Leaf(v.to_string()))
            }
            fn visit_f64<E>(self, v: f64) -> Result<Node, E> {
                Ok(Node::Leaf(v.to_string()))
            }
            fn visit_str<E>(self, v: &str) -> Result<Node, E> {
                Ok(Node::Leaf(v.to_string()))
            }
            fn visit_string<E>(self, v: String) -> Result<Node, E> {
                Ok(Node::Leaf(v))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
                let mut out = Vec::new();
                while let Some(n) = seq.next_element()? {
                    out.push(n);
                }
                Ok(Node::Array(out))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
                let mut out = Vec::new();
                while let Some(entry) = map.next_entry::<String, Node>()? {
                    out.push(entry);
                }
                Ok(Node::Object(out))
            }
        }
        d.deserialize_any(V)
    }
}

/// Read JSON text into a record.
///
/// One record is one object. A file that is a list of them is a batch, and
/// which of its records a document is about is not something a reader can
/// decide.
pub fn read(text: &str) -> Result<Record, RecordError> {
    let node: Node =
        serde_json::from_str(text).map_err(|e| RecordError::Malformed(e.to_string()))?;
    let Node::Object(members) = node else {
        return Err(RecordError::NotARecord("the document is not a JSON object"));
    };
    let mut fields = Vec::new();
    for (key, value) in &members {
        walk(value, &format!("/{}", escape(key)), key, &mut fields);
    }
    Ok(Record {
        syntax: Syntax::Json,
        root: String::new(),
        fields,
        tracks: Vec::new(),
    })
}

fn walk(node: &Node, path: &str, name: &str, out: &mut Vec<Field>) {
    match node {
        Node::Null => {}
        Node::Leaf(value) => {
            let value = value.trim();
            if !value.is_empty() {
                out.push(Field {
                    path: path.to_string(),
                    name: name.to_string(),
                    value: value.to_string(),
                });
            }
        }
        Node::Object(members) => {
            for (key, value) in members {
                walk(
                    value,
                    &format!("{path}/{}", escape(key)),
                    &format!("{name}/{key}"),
                    out,
                );
            }
        }
        // An index is a position, not a name: every item of `tags` is a
        // value of the field `tags`.
        Node::Array(items) => {
            for (i, value) in items.iter().enumerate() {
                walk(value, &format!("{path}/{i}"), name, out);
            }
        }
    }
}

/// A key as a JSON Pointer reference token (RFC 6901).
fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(json: &str) -> Vec<(String, String, String)> {
        read(json)
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
    fn a_record_is_its_fields_in_the_order_written() {
        let json = r#"{"title": "Budget adopté", "id": 48213, "live": false,
                       "summary": null, "blank": " ", "score": 0.5}"#;
        let r = read(json).unwrap();
        assert_eq!((r.syntax, r.root.as_str()), (Syntax::Json, ""));
        assert_eq!(
            fields(json),
            [
                f("/title", "title", "Budget adopté"),
                f("/id", "id", "48213"),
                f("/live", "live", "false"),
                f("/score", "score", "0.5"),
            ]
        );
    }

    #[test]
    fn nested_and_listed_values_keep_one_name_and_their_own_paths() {
        assert_eq!(
            fields(
                r#"{"meta": {"author": "Ada"}, "tags": ["a", "b"],
                    "parts": [{"text": "un"}, {"text": "deux"}], "a/b": {"c~d": "x"}}"#
            ),
            [
                f("/meta/author", "meta/author", "Ada"),
                f("/tags/0", "tags", "a"),
                f("/tags/1", "tags", "b"),
                f("/parts/0/text", "parts/text", "un"),
                f("/parts/1/text", "parts/text", "deux"),
                f("/a~1b/c~0d", "a/b/c~d", "x"),
            ]
        );
    }

    #[test]
    fn a_value_reads_as_written_not_as_escaped() {
        assert_eq!(
            fields(r#"{"a": "Q&A \"live\" été\nligne"}"#),
            [f("/a", "a", "Q&A \"live\" été\nligne")]
        );
    }

    #[test]
    fn what_is_not_one_object_is_not_a_record() {
        assert!(matches!(
            read("[{\"a\": 1}]"),
            Err(RecordError::NotARecord(_))
        ));
        assert!(matches!(read("\"text\""), Err(RecordError::NotARecord(_))));
        assert!(matches!(read("{\"a\": "), Err(RecordError::Malformed(_))));
    }
}
