//! A source format: what the fields of a kind of record mean.
//!
//! A record says what it holds and not what it is for. `body` is prose to
//! read, `published_at` is a date about the document, `region` is a value
//! from a list the organisation keeps, and nothing in the notation tells
//! them apart. The format is the declaration that does: written once for a
//! kind of record, by someone who knows it, and applied to every record of
//! that kind without anything being guessed.
//!
//! It transforms nothing. A field's value is read as it is; the format only
//! says which part of the document it becomes.

use crate::record::{Record, Syntax};
use std::collections::BTreeMap;

/// How a record of this format is told from any other.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Match {
    pub syntax: Syntax,
    /// The record's root: the root element of XML, the class of an asset.
    /// Any root when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Fields a record of this format always holds a value in. A field is
    /// named as the format's other declarations name it, so a container
    /// is held when a field inside it is; a value that means there is none
    /// (see `nullValues`) is not a value.
    ///
    /// A root named `record` or `item` is shared by formats that have
    /// nothing else in common, and JSON names no root at all. The fields a
    /// record must hold are what makes the match a recognition and not a
    /// coincidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
}

/// What a content field's text is in the document.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentKind {
    /// The document's title: a `doco:SectionTitle` of level 1.
    Title,
    /// A heading inside the document: a `doco:SectionTitle` of level 2.
    Heading,
    /// Prose, one `doco:Paragraph` per paragraph of the value.
    #[default]
    Text,
}

/// A field that is text to read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Content {
    pub field: String,
    #[serde(default, rename = "as")]
    pub kind: ContentKind,
}

/// A field that is a fact about the document.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Metadata {
    pub field: String,
    /// The property the value is stated under, as an absolute IRI.
    pub property: String,
    /// The value's datatype, as an absolute IRI. A plain string when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub datatype: Option<String>,
}

/// What of a concept the source writes to name it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchOn {
    /// Its label: `Économie`.
    #[default]
    Label,
    /// Its code in the list, its `skos:notation`: `2` for television.
    Notation,
    /// Its identifier, the last step of its IRI: `7f3a…` for
    /// `https://example.org/id/7f3a…`. What a system that holds the list
    /// writes when it links to a concept.
    Id,
}

/// A field whose value is one of a controlled list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Enum {
    pub field: String,
    /// The property the value is stated under, as an absolute IRI.
    pub property: String,
    /// The lists the values belong to, as the IRIs of their concept
    /// schemes, the one to look in first coming first.
    ///
    /// Informative here, like `classes`: this reader holds no list. Whoever
    /// does resolves them into `values` before a record is read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schemes: Vec<String>,
    /// The classes a value's concept may be, as absolute IRIs. `Québec` is
    /// a city and a province, and a field of regions means the province.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub classes: Vec<String>,
    /// Whether the source names a concept by its label, by its code or by
    /// its identifier.
    /// Informative here too: it says what the keys of `values` are.
    #[serde(default, skip_serializing_if = "is_label")]
    pub match_on: MatchOn,
    /// The list: each value as the source writes it, and the concept it
    /// names. Matched without regard to case, accents or surrounding space.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, String>,
    /// What each concept of `values` is called, by its IRI. Informative,
    /// and filled by whoever resolves the list: where the source writes a
    /// code, this is what lets a reader be shown the name beside it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// What separates the values of a field that holds several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
}

fn is_label(m: &MatchOn) -> bool {
    *m == MatchOn::Label
}

/// What a track of a recording holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackRole {
    /// What was said: captions or a transcript, read into speaker turns.
    Speech,
    /// The parts of the recording, each with a title: a bulletin's stories.
    Sections,
}

/// A track to read, and the field of its segments that holds the text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrackRule {
    pub track: String,
    pub field: String,
    pub role: TrackRole,
}

/// The declaration of a kind of record.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceFormat {
    /// What the declaration is called where it is kept, for a message to
    /// name it by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(rename = "match")]
    pub matches: Match,
    /// The class a document of this format is, as an absolute IRI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_class: Option<String>,
    /// Values that mean there is none. An export writes `null` or `N/A`
    /// into a field it has nothing for, and a field holding one is read as
    /// absent. The empty value always is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub null_values: Vec<String>,
    /// The field holding the document's title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The field holding when the document was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// The field holding when the document was last modified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    /// The fields to read, in the order the document reads them. The order
    /// is the declaration's and not the record's: a record that stores its
    /// body before its title is still a document that opens with its title.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<Content>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata: Vec<Metadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enums: Vec<Enum>,
    /// The tracks to read. Of those declared as speech, the first the
    /// record holds is read and the others are not: captions and a
    /// transcript of the same recording are the same words twice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<TrackRule>,
}

impl SourceFormat {
    /// The name a message calls this declaration by.
    pub fn name(&self) -> &str {
        self.id
            .as_deref()
            .or(self.label.as_deref())
            .unwrap_or("(unnamed source format)")
    }

    /// Read a declaration from JSON.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let format: SourceFormat = serde_json::from_str(text).map_err(|e| e.to_string())?;
        format.validate()?;
        Ok(format)
    }

    /// What is wrong with the declaration, if anything is.
    ///
    /// Checked when it is read and not when a record meets it: a mistake
    /// in a declaration is the same mistake for every record, and the
    /// place to hear of it is where the declaration was written.
    pub fn validate(&self) -> Result<(), String> {
        let m = &self.matches;
        let root = m.root.as_deref().is_some_and(|r| !r.trim().is_empty());
        match m.syntax {
            // JSON names no root, so nothing but its fields can recognise it.
            Syntax::Json if m.required.is_empty() => {
                return Err(format!(
                    "{}: a JSON format must name the fields it requires, \
                     or it would match every JSON object",
                    self.name()
                ));
            }
            Syntax::Json if root => {
                return Err(format!(
                    "{}: a JSON record has no root to match",
                    self.name()
                ));
            }
            Syntax::Xml if !root && m.required.is_empty() => {
                return Err(format!(
                    "{}: an XML format must name its root or the fields it requires, \
                     or it would match every XML document",
                    self.name()
                ));
            }
            _ => {}
        }
        let properties = self
            .metadata
            .iter()
            .map(|m| (&m.field, &m.property))
            .chain(self.enums.iter().map(|e| (&e.field, &e.property)));
        for (field, property) in properties {
            if !absolute(property) {
                return Err(format!(
                    "{}: the property of `{field}` must be an absolute IRI, not `{property}`",
                    self.name()
                ));
            }
        }
        let iris = self
            .document_class
            .iter()
            .map(|c| ("documentClass", c))
            .chain(
                self.metadata
                    .iter()
                    .filter_map(|m| m.datatype.as_ref().map(|d| ("datatype", d))),
            )
            .chain(self.enums.iter().flat_map(|e| {
                e.values
                    .values()
                    .map(|v| ("values", v))
                    .chain(e.schemes.iter().map(|v| ("schemes", v)))
                    .chain(e.classes.iter().map(|v| ("classes", v)))
            }));
        for (what, iri) in iris {
            if !absolute(iri) {
                return Err(format!(
                    "{}: `{what}` must be an absolute IRI, not `{iri}`",
                    self.name()
                ));
            }
        }
        // A field plays one part. Declared twice, one declaration would win
        // without a word and the other would state nothing.
        let mut seen: Vec<(&str, &str)> = Vec::new();
        let parts = self
            .content
            .iter()
            .map(|c| (c.field.as_str(), "content"))
            .chain(self.metadata.iter().map(|m| (m.field.as_str(), "metadata")))
            .chain(self.enums.iter().map(|e| (e.field.as_str(), "an enum")));
        for (field, part) in parts {
            // A field inside a declared one is that one's: `body/p` is part
            // of `body`, whichever of the two was declared first.
            let clash = seen
                .iter()
                .find(|(f, _)| is_field(field, f) || is_field(f, field));
            if let Some((other, first)) = clash {
                return Err(if *other == field && *first == part {
                    format!("{}: `{field}` is declared as {part} twice", self.name())
                } else if *other == field {
                    format!(
                        "{}: `{field}` is declared as {first} and as {part}; a field plays one part",
                        self.name()
                    )
                } else {
                    format!(
                        "{}: `{other}` is declared as {first} and `{field}` as {part}, \
                         and one holds the other; a field plays one part",
                        self.name()
                    )
                });
            }
            seen.push((field, part));
        }
        Ok(())
    }

    /// Whether `record` is of this format.
    pub fn recognises(&self, record: &Record) -> bool {
        let m = &self.matches;
        m.syntax == record.syntax
            && m.root.as_deref().is_none_or(|r| r == record.root)
            && m.required.iter().all(|required| {
                record
                    .fields
                    .iter()
                    .any(|f| is_field(&f.name, required) && !self.is_null(&f.value))
            })
    }

    /// Whether `value` is one of the values that mean there is none.
    pub(crate) fn is_null(&self, value: &str) -> bool {
        let value = value.trim();
        value.is_empty() || self.null_values.iter().any(|n| n.trim() == value)
    }
}

/// Whether the field called `name` is the declared `field`, or under it.
///
/// A format declares `body` and means what `body` holds: the value, or the
/// values of the fields inside it when the source nests them.
pub(crate) fn is_field(name: &str, field: &str) -> bool {
    name == field
        || name
            .strip_prefix(field)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// An IRI with a scheme and nothing a prefixed name or a sentence has.
fn absolute(iri: &str) -> bool {
    let Some((scheme, rest)) = iri.split_once(':') else {
        return false;
    };
    matches!(scheme, "http" | "https" | "urn")
        && !rest.is_empty()
        && !iri.contains(char::is_whitespace)
}

/// What the declared formats make of a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recognition<'a> {
    /// No format is this record's. It is whatever its notation is, and
    /// read as that.
    Unknown,
    Known(&'a SourceFormat),
    /// More than one format claims the record. Reading it as either would
    /// be a guess, and a guess here decides what every field means.
    Ambiguous(Vec<&'a SourceFormat>),
}

/// Find the format of `record` among `formats`.
pub fn recognise<'a>(record: &Record, formats: &'a [SourceFormat]) -> Recognition<'a> {
    let mut claimed: Vec<&SourceFormat> = formats.iter().filter(|f| f.recognises(record)).collect();
    match claimed.len() {
        0 => Recognition::Unknown,
        1 => Recognition::Known(claimed.remove(0)),
        _ => Recognition::Ambiguous(claimed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;

    const WEB: &str = r#"{
        "id": "https://example.org/format/web-record",
        "match": { "syntax": "xml", "root": "record", "required": ["id", "title", "body"] },
        "nullValues": ["null"],
        "content": [{ "field": "title", "as": "title" }, { "field": "body" }],
        "metadata": [{ "field": "id", "property": "https://example.org/model#recordId" }]
    }"#;

    fn record(xml: &str) -> Record {
        xml::read(xml).unwrap()
    }

    #[test]
    fn a_declaration_reads_from_json_with_its_defaults() {
        let f = SourceFormat::from_json(WEB).unwrap();
        assert_eq!(f.name(), "https://example.org/format/web-record");
        assert_eq!(f.content[0].kind, ContentKind::Title);
        assert_eq!(f.content[1].kind, ContentKind::Text);
        assert!(f.enums.is_empty() && f.tracks.is_empty());
        assert!(f.is_null(" null ") && f.is_null("") && !f.is_null("nul"));
        // What it writes, it reads back.
        let again = SourceFormat::from_json(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(again, f);
    }

    #[test]
    fn a_list_names_its_concepts_by_label_code_or_identifier() {
        let with = |match_on: &str| {
            SourceFormat::from_json(&WEB.replace(
                r#""metadata": ["#,
                &format!(
                    r#""enums": [{{ "field": "author", "property": "https://example.org/model#author"{match_on} }}],
        "metadata": ["#
                ),
            ))
        };
        assert_eq!(with("").unwrap().enums[0].match_on, MatchOn::Label);
        let by = |word: &str| with(&format!(r#", "matchOn": "{word}""#));
        assert_eq!(by("notation").unwrap().enums[0].match_on, MatchOn::Notation);
        assert_eq!(by("id").unwrap().enums[0].match_on, MatchOn::Id);
        assert!(by("name").unwrap_err().contains("name"));
    }

    #[test]
    fn a_misspelt_key_is_an_error_and_not_a_silence() {
        let err = SourceFormat::from_json(&WEB.replace("nullValues", "nulValues")).unwrap_err();
        assert!(err.contains("nulValues"), "{err}");
    }

    #[test]
    fn a_declaration_that_would_match_everything_is_refused() {
        let json = r#"{ "match": { "syntax": "json" }, "content": [{ "field": "text" }] }"#;
        assert!(SourceFormat::from_json(json)
            .unwrap_err()
            .contains("every JSON object"));
        let xml = r#"{ "match": { "syntax": "xml" } }"#;
        assert!(SourceFormat::from_json(xml)
            .unwrap_err()
            .contains("every XML document"));
        // An asset's notation is its own: any asset is one.
        assert!(SourceFormat::from_json(r#"{ "match": { "syntax": "axf" } }"#).is_ok());
    }

    #[test]
    fn a_property_is_an_absolute_iri() {
        let bad = WEB.replace("https://example.org/model#recordId", "model:recordId");
        let err = SourceFormat::from_json(&bad).unwrap_err();
        assert!(
            err.contains("`id`") && err.contains("model:recordId"),
            "{err}"
        );
    }

    #[test]
    fn a_field_is_content_once() {
        let twice = WEB.replace(r#"{ "field": "body" }"#, r#"{ "field": "title" }"#);
        assert!(SourceFormat::from_json(&twice)
            .unwrap_err()
            .contains("content twice"));
    }

    #[test]
    fn a_field_plays_one_part() {
        let both = WEB.replace(
            r#"{ "field": "id", "property""#,
            r#"{ "field": "body", "property""#,
        );
        let err = SourceFormat::from_json(&both).unwrap_err();
        assert!(
            err.contains("`body`") && err.contains("content") && err.contains("metadata"),
            "{err}"
        );
    }

    #[test]
    fn a_field_declared_twice_or_inside_another_is_refused() {
        let twice = WEB.replace(
            r#""metadata": [{"#,
            r#""metadata": [{ "field": "id", "property": "https://example.org/model#other" }, {"#,
        );
        let err = SourceFormat::from_json(&twice).unwrap_err();
        assert!(err.contains("`id` is declared as metadata twice"), "{err}");

        let inside = WEB.replace(
            r#"{ "field": "id", "property""#,
            r#"{ "field": "body/p", "property""#,
        );
        let err = SourceFormat::from_json(&inside).unwrap_err();
        assert!(
            err.contains("`body`") && err.contains("`body/p`") && err.contains("holds the other"),
            "{err}"
        );
        // Two fields that only begin alike are two fields.
        let apart = WEB.replace(
            r#"{ "field": "id", "property""#,
            r#"{ "field": "bodyguard", "property""#,
        );
        assert!(SourceFormat::from_json(&apart).is_ok());
    }

    #[test]
    fn a_required_field_holds_a_value_and_may_be_a_container() {
        let formats = vec![SourceFormat::from_json(WEB).unwrap()];
        // `null` is what this format reads as absent.
        let absent = record("<record><id>1</id><title>T</title><body>null</body></record>");
        assert_eq!(recognise(&absent, &formats), Recognition::Unknown);
        let nested = record("<record><id>1</id><title>T</title><body><p>B</p></body></record>");
        assert_eq!(
            recognise(&nested, &formats),
            Recognition::Known(&formats[0])
        );
    }

    #[test]
    fn a_record_is_recognised_by_its_root_and_its_fields() {
        let formats = vec![SourceFormat::from_json(WEB).unwrap()];
        let article = record("<record><id>1</id><title>T</title><body>B</body></record>");
        assert_eq!(
            recognise(&article, &formats),
            Recognition::Known(&formats[0])
        );
        // The same root, another kind of record.
        let other = record("<record><sku>1</sku><price>2</price></record>");
        assert_eq!(recognise(&other, &formats), Recognition::Unknown);
        // The same fields, another root.
        let item = record("<item><id>1</id><title>T</title><body>B</body></item>");
        assert_eq!(recognise(&item, &formats), Recognition::Unknown);
        // Another notation.
        let json = crate::json::read(r#"{"id": 1, "title": "T", "body": "B"}"#).unwrap();
        assert_eq!(recognise(&json, &formats), Recognition::Unknown);
    }

    #[test]
    fn two_formats_claiming_one_record_is_said_and_not_settled() {
        let loose = r#"{ "id": "urn:loose", "match": { "syntax": "xml", "root": "record" } }"#;
        let formats = vec![
            SourceFormat::from_json(WEB).unwrap(),
            SourceFormat::from_json(loose).unwrap(),
        ];
        let article = record("<record><id>1</id><title>T</title><body>B</body></record>");
        let Recognition::Ambiguous(claimed) = recognise(&article, &formats) else {
            panic!("expected an ambiguity");
        };
        assert_eq!(
            claimed.iter().map(|f| f.name()).collect::<Vec<_>>(),
            ["https://example.org/format/web-record", "urn:loose"]
        );
    }
}
