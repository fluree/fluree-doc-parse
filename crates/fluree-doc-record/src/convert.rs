//! A record and its format, read into a document.

use crate::format::{is_field, ContentKind, SourceFormat, TrackRole};
use crate::record::{Field, Record, Segment, Track};
use fluree_doc_model::{
    DocumentInfo, Element, FieldRole, Notes, Property, PropertyValue, SourceField, Turn,
};
use fluree_doc_transcript::{caption_turns, Caption};

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const DCTERMS: &str = "http://purl.org/dc/terms/";

/// A record read as a document.
#[derive(Debug, Clone)]
pub struct Converted {
    pub elements: Vec<Element>,
    /// What the record states about the document, and the record itself.
    pub notes: Notes,
    /// What in the record did not fit its declaration: a value a list does
    /// not hold, a date that is not one. The record is read all the same,
    /// with the value stated as it was written, and this is where it says
    /// so.
    pub warnings: Vec<String>,
}

/// Read `record` as its `format` declares.
pub fn convert(record: &Record, format: &SourceFormat) -> Converted {
    let tag = record.syntax.tag();
    let live: Vec<&Field> = record
        .fields
        .iter()
        .filter(|f| !format.is_null(&f.value))
        .collect();
    let mut warnings = Vec::new();
    let mut elements = Vec::new();

    // Content, in the declaration's order.
    for content in &format.content {
        for f in live.iter().filter(|f| is_field(&f.name, &content.field)) {
            match content.kind {
                ContentKind::Title => {
                    elements.push(heading(collapse(&f.value), 1, &f.path, tag));
                }
                ContentKind::Heading => {
                    elements.push(heading(collapse(&f.value), 2, &f.path, tag));
                }
                ContentKind::Text => {
                    for p in paragraphs(&f.value) {
                        elements.push(paragraph(p, None, &f.path, tag));
                    }
                }
            }
        }
    }

    // Tracks, in the recording's order.
    let titled = elements
        .iter()
        .any(|e| e.kind == "doco:SectionTitle" && e.level == Some(1));
    let read = read_tracks(record, format);
    let section_level = if titled { 2 } else { 1 };
    for part in read.parts {
        if let Some(s) = part.section {
            let mut e = heading(s.title, section_level, &s.path, tag);
            e.turn = Some(Turn {
                speaker: None,
                start_ms: s.start_ms,
                end_ms: s.end_ms,
            });
            elements.push(e);
        }
        for (turn, text) in caption_turns(&part.captions) {
            elements.push(paragraph(text, Some(turn), &read.speech_path, tag));
        }
    }
    for (i, e) in elements.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }

    // What the record states about the document.
    let mut info = DocumentInfo {
        class: format.document_class.clone(),
        ..Default::default()
    };
    let first = |field: &Option<String>| -> Option<&Field> {
        let field = field.as_deref()?;
        live.iter().find(|f| is_field(&f.name, field)).copied()
    };
    info.title = first(&format.title).map(|f| collapse(&f.value));
    for (declared, slot, what) in [
        (&format.created, &mut info.created, "created"),
        (&format.modified, &mut info.modified, "modified"),
    ] {
        let Some(f) = first(declared) else { continue };
        match moment(&f.value) {
            Some(v) => *slot = Some(v),
            None => warnings.push(format!(
                "`{}` holds `{}`, which is not a date: the document's `{what}` is left unstated",
                f.path, f.value
            )),
        }
    }

    // The record as the source held it, each field with the part it plays.
    let mut fields: Vec<SourceField> = Vec::new();
    for f in &live {
        if format.content.iter().any(|c| is_field(&f.name, &c.field)) {
            fields.push(SourceField::content(&f.path));
            continue;
        }
        if let Some(m) = format.metadata.iter().find(|m| is_field(&f.name, &m.field)) {
            let (value, datatype) = match &m.datatype {
                None => (f.value.clone(), None),
                Some(datatype) => match typed(&f.value, datatype) {
                    Some(v) => (v, Some(datatype.clone())),
                    None => {
                        warnings.push(format!(
                            "`{}` holds `{}`, which is not a `{datatype}`: stated as written",
                            f.path, f.value
                        ));
                        (f.value.clone(), None)
                    }
                },
            };
            info.properties.push(Property {
                property: m.property.clone(),
                value: PropertyValue::Literal {
                    value: value.clone(),
                    datatype,
                },
            });
            let mut shown = SourceField::new(&f.path, FieldRole::Metadata, &value);
            shown.property = Some(m.property.clone());
            fields.push(shown);
            continue;
        }
        if let Some(e) = format.enums.iter().find(|e| is_field(&f.name, &e.field)) {
            let values: Vec<&str> = match e.separator.as_deref().filter(|s| !s.is_empty()) {
                Some(separator) => f.value.split(separator).collect(),
                None => vec![f.value.as_str()],
            };
            for value in values {
                let value = value.trim();
                if format.is_null(value) {
                    continue;
                }
                let iri = e
                    .values
                    .iter()
                    .find(|(k, _)| same_value(k, value))
                    .map(|(_, iri)| iri.clone());
                if iri.is_none() {
                    warnings.push(format!(
                        "`{}` holds `{value}`, which the list of `{}` does not: stated as written",
                        f.path, e.field
                    ));
                }
                info.properties.push(Property {
                    property: e.property.clone(),
                    value: match &iri {
                        Some(iri) => PropertyValue::Iri { iri: iri.clone() },
                        None => PropertyValue::Literal {
                            value: value.to_string(),
                            datatype: None,
                        },
                    },
                });
                let mut shown = SourceField::new(&f.path, FieldRole::Enum, value);
                shown.property = Some(e.property.clone());
                shown.label = iri.as_ref().and_then(|iri| e.labels.get(iri)).cloned();
                shown.iri = iri;
                fields.push(shown);
            }
            continue;
        }
        // The fields the document's own title and dates are read from
        // state those, under the terms the emitter states them by.
        let stated = [
            (&format.title, "title"),
            (&format.created, "created"),
            (&format.modified, "modified"),
        ]
        .into_iter()
        .find(|(declared, _)| declared.as_deref().is_some_and(|d| is_field(&f.name, d)));
        if let Some((_, term)) = stated {
            // Shown as it is stated: the title on one line, a date as the
            // date it was read as.
            let value = match term {
                "title" => collapse(&f.value),
                _ => moment(&f.value).unwrap_or_else(|| f.value.clone()),
            };
            let mut shown = SourceField::new(&f.path, FieldRole::Metadata, &value);
            shown.property = Some(format!("{DCTERMS}{term}"));
            fields.push(shown);
            continue;
        }
        fields.push(SourceField::new(&f.path, FieldRole::Unmapped, &f.value));
    }
    for path in read.read_paths {
        fields.push(SourceField::content(path));
    }

    Converted {
        elements,
        notes: Notes {
            info,
            fields,
            ..Default::default()
        },
        warnings,
    }
}

/// A part of the recording: the section that opens it, when one does, and
/// what was said until the next.
struct Part {
    section: Option<Section>,
    captions: Vec<Caption>,
}

struct Section {
    start_ms: u64,
    end_ms: u64,
    title: String,
    path: String,
}

struct ReadTracks {
    parts: Vec<Part>,
    /// The path of the speech track that was read.
    speech_path: String,
    /// The paths of every track that was read, speech and sections.
    read_paths: Vec<String>,
}

/// The value of `field` in a segment, when it holds one.
fn value_of<'a>(segment: &'a Segment, field: &str, format: &SourceFormat) -> Option<&'a str> {
    segment
        .fields
        .iter()
        .find(|f| f.name == field && !format.is_null(&f.value))
        .map(|f| f.value.as_str())
}

/// The first track declared for `role` that the record holds text in.
fn track_for<'a>(
    record: &'a Record,
    format: &'a SourceFormat,
    role: TrackRole,
) -> Option<(&'a Track, &'a str)> {
    format
        .tracks
        .iter()
        .filter(|rule| rule.role == role)
        .find_map(|rule| {
            let track = record.tracks.iter().find(|t| t.name == rule.track)?;
            track
                .segments
                .iter()
                .any(|s| value_of(s, &rule.field, format).is_some())
                .then_some((track, rule.field.as_str()))
        })
}

fn read_tracks(record: &Record, format: &SourceFormat) -> ReadTracks {
    let mut read_paths = Vec::new();

    let mut sections: Vec<Section> = Vec::new();
    if let Some((track, field)) = track_for(record, format, TrackRole::Sections) {
        read_paths.push(track.path.clone());
        sections = track
            .segments
            .iter()
            .filter_map(|s| {
                Some(Section {
                    start_ms: s.start_ms,
                    end_ms: s.end_ms,
                    title: collapse(value_of(s, field, format)?),
                    path: track.path.clone(),
                })
            })
            .collect();
        // A track is kept in the order its segments were logged, which is
        // not always the order they play in.
        sections.sort_by_key(|s| s.start_ms);
    }

    let mut captions: Vec<Caption> = Vec::new();
    let mut speech_path = String::new();
    if let Some((track, field)) = track_for(record, format, TrackRole::Speech) {
        read_paths.push(track.path.clone());
        speech_path = track.path.clone();
        captions = track
            .segments
            .iter()
            .filter_map(|s| {
                Some(Caption {
                    start_ms: s.start_ms,
                    end_ms: s.end_ms,
                    text: value_of(s, field, format)?.to_string(),
                })
            })
            .collect();
        captions.sort_by_key(|c| c.start_ms);
    }

    // What is said belongs to the section that was open when it began. A
    // turn never runs across the start of one: the first words of a story
    // are not the last of the one before.
    let mut parts: Vec<Part> = Vec::new();
    let mut captions = captions.into_iter().peekable();
    let mut before = Vec::new();
    let first_start = sections.first().map_or(u64::MAX, |s| s.start_ms);
    while let Some(c) = captions.next_if(|c| c.start_ms < first_start) {
        before.push(c);
    }
    if !before.is_empty() {
        parts.push(Part {
            section: None,
            captions: before,
        });
    }
    let starts: Vec<u64> = sections.iter().map(|s| s.start_ms).collect();
    for (i, section) in sections.into_iter().enumerate() {
        let next_start = starts.get(i + 1).copied().unwrap_or(u64::MAX);
        let mut said = Vec::new();
        while let Some(c) = captions.next_if(|c| c.start_ms < next_start) {
            said.push(c);
        }
        parts.push(Part {
            section: Some(section),
            captions: said,
        });
    }

    ReadTracks {
        parts,
        speech_path,
        read_paths,
    }
}

fn element(
    kind: &str,
    text: String,
    level: Option<usize>,
    path: &str,
    tag: &'static str,
) -> Element {
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
        source_path: Some(path.to_string()),
        provenance: tag,
        // The class of every element is what the format declares its field
        // to be. Nothing about it was inferred.
        evidence: "declared",
    }
}

fn heading(text: String, level: usize, path: &str, tag: &'static str) -> Element {
    element("doco:SectionTitle", text, Some(level), path, tag)
}

fn paragraph(text: String, turn: Option<Turn>, path: &str, tag: &'static str) -> Element {
    let mut e = element("doco:Paragraph", text, None, path, tag);
    e.turn = turn;
    e
}

/// Text on one line, its runs of whitespace single spaces.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The paragraphs of a value.
///
/// Where the value separates them with blank lines, a line break inside
/// one is a wrap and the lines are one paragraph. Where it has none, every
/// line is a paragraph: that is how an export writes prose it holds in one
/// field.
fn paragraphs(value: &str) -> Vec<String> {
    let value = value.replace("\r\n", "\n").replace('\r', "\n");
    let blank = value
        .split('\n')
        .collect::<Vec<_>>()
        .windows(2)
        .any(|w| w[0].trim().is_empty() && !w[1].trim().is_empty());
    let mut out = Vec::new();
    if blank {
        let mut open = String::new();
        for line in value.split('\n').chain(std::iter::once("")) {
            let line = line.trim();
            if line.is_empty() {
                if !open.is_empty() {
                    out.push(std::mem::take(&mut open));
                }
                continue;
            }
            if !open.is_empty() {
                open.push(' ');
            }
            open.push_str(line);
        }
    } else {
        out.extend(
            value
                .split('\n')
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string),
        );
    }
    out
}

/// Whether a value of the source is this value of the list.
///
/// Case and accents are how a value was typed, not which value it is: one
/// archive holds `Montréal`, `Montreal` and `MONTREAL`, and means one city.
fn same_value(listed: &str, value: &str) -> bool {
    fold(listed) == fold(value)
}

/// A value without its case, its accents or its surrounding space.
fn fold(value: &str) -> String {
    use unicode_normalization::char::is_combining_mark;
    use unicode_normalization::UnicodeNormalization;
    value
        .trim()
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// A date or a date and time in ISO 8601, from the forms exports write.
///
/// `20250424153338` and `20250424` are how a media asset manager stores
/// them, and `2025-04-24 15:33:38` how a database prints them.
fn moment(value: &str) -> Option<String> {
    let v = value.trim();
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let date = |y: &str, m: &str, d: &str| -> Option<String> {
        let (mm, dd): (u32, u32) = (m.parse().ok()?, d.parse().ok()?);
        (digits(y) && y.len() == 4 && (1..=12).contains(&mm) && (1..=31).contains(&dd))
            .then(|| format!("{y}-{m}-{d}"))
    };
    let time = |h: &str, m: &str, s: &str| -> Option<String> {
        let (hh, mm, ss): (u32, u32, u32) = (h.parse().ok()?, m.parse().ok()?, s.parse().ok()?);
        (hh < 24 && mm < 60 && ss < 61).then(|| format!("{h}:{m}:{s}"))
    };
    if digits(v) && v.len() == 8 {
        return date(&v[..4], &v[4..6], &v[6..8]);
    }
    if digits(v) && v.len() == 14 {
        return Some(format!(
            "{}T{}",
            date(&v[..4], &v[4..6], &v[6..8])?,
            time(&v[8..10], &v[10..12], &v[12..14])?
        ));
    }
    // ISO already, or ISO with a space where the `T` goes.
    let b = v.as_bytes();
    if b.len() >= 10 && v.is_char_boundary(10) && b[4] == b'-' && b[7] == b'-' {
        let day = date(&v[..4], &v[5..7], &v[8..10])?;
        let rest = &v[10..];
        if rest.is_empty() {
            return Some(day);
        }
        let clock = rest.strip_prefix(['T', ' '])?;
        let cb = clock.as_bytes();
        if cb.len() >= 8 && clock.is_char_boundary(8) && cb[2] == b':' && cb[5] == b':' {
            time(&clock[..2], &clock[3..5], &clock[6..8])?;
            return Some(format!("{day}T{clock}"));
        }
    }
    None
}

/// A time of day in ISO 8601, from `183000` or from ISO itself.
fn clock(value: &str) -> Option<String> {
    let v = value.trim();
    let (h, m, s) = match v.as_bytes() {
        b if b.len() == 6 && b.iter().all(u8::is_ascii_digit) => (&v[..2], &v[2..4], &v[4..6]),
        b if b.len() >= 8
            && v.is_char_boundary(8)
            && b[2] == b':'
            && b[5] == b':'
            && b[..8].iter().filter(|c| c.is_ascii_digit()).count() == 6 =>
        {
            (&v[..2], &v[3..5], &v[6..8])
        }
        _ => return None,
    };
    let (hh, mm, ss): (u32, u32, u32) = (h.parse().ok()?, m.parse().ok()?, s.parse().ok()?);
    // What follows the seconds, a fraction or a zone, is kept as written.
    let rest = if v.len() > 8 && v.as_bytes()[2] == b':' {
        &v[8..]
    } else {
        ""
    };
    (hh < 24 && mm < 60 && ss < 61).then(|| format!("{h}:{m}:{s}{rest}"))
}

/// `value` as a literal of `datatype`, or nothing when it is not one.
///
/// Only the datatypes a store compares by value are checked: a date that
/// is not a date sorts as a string among dates and is found by no range.
/// Any other datatype is the declaration's to know.
fn typed(value: &str, datatype: &str) -> Option<String> {
    let v = value.trim();
    let Some(local) = datatype.strip_prefix(XSD) else {
        return Some(v.to_string());
    };
    match local {
        "dateTime" => moment(v).filter(|m| m.contains('T')),
        "date" => moment(v).map(|m| m[..10].to_string()),
        "time" => clock(v),
        "integer" | "int" | "long" | "short" => v.parse::<i64>().ok().map(|n| n.to_string()),
        "decimal" | "double" | "float" => v
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .map(|_| v.to_string()),
        "boolean" => match v {
            "true" | "1" => Some("true".into()),
            "false" | "0" => Some("false".into()),
            _ => None,
        },
        _ => Some(v.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Syntax, Track};
    use crate::{json, xml};

    const MODEL: &str = "https://example.org/model#";

    fn web() -> SourceFormat {
        SourceFormat::from_json(
            r#"{
            "id": "https://example.org/format/web-record",
            "match": { "syntax": "xml", "root": "record", "required": ["id", "title", "body"] },
            "documentClass": "https://example.org/model#WebArticle",
            "nullValues": ["null"],
            "title": "title",
            "created": "published",
            "content": [
                { "field": "title", "as": "title" },
                { "field": "lead" },
                { "field": "body" }
            ],
            "metadata": [
                { "field": "id", "property": "https://example.org/model#recordId" },
                { "field": "published", "property": "https://example.org/model#published",
                  "datatype": "http://www.w3.org/2001/XMLSchema#dateTime" }
            ],
            "enums": [
                { "field": "theme", "property": "https://example.org/model#theme",
                  "schemes": ["https://example.org/id/themes"],
                  "classes": ["https://example.org/model#Theme"],
                  "values": { "Économie": "https://example.org/id/economy" } }
            ]
        }"#,
        )
        .unwrap()
    }

    const ARTICLE: &str = "<record>\
        <id>48213</id>\
        <body>Le conseil explique sa décision.\nLes taxes restent stables.</body>\
        <title>Le conseil municipal\n  adopte son budget</title>\
        <lead>La séance a duré trois heures.</lead>\
        <published>2025-04-12T06:26:14.821Z</published>\
        <theme>économie</theme>\
        <subject>null</subject>\
        <region>Bretagne</region>\
        </record>";

    #[test]
    fn content_reads_in_the_declaration_s_order_and_says_where_it_was() {
        let c = convert(&xml::read(ARTICLE).unwrap(), &web());
        let read: Vec<(&str, Option<usize>, &str, Option<&str>)> = c
            .elements
            .iter()
            .map(|e| {
                (
                    e.kind.as_str(),
                    e.level,
                    e.text.as_str(),
                    e.source_path.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                (
                    "doco:SectionTitle",
                    Some(1),
                    "Le conseil municipal adopte son budget",
                    Some("/record/title")
                ),
                (
                    "doco:Paragraph",
                    None,
                    "La séance a duré trois heures.",
                    Some("/record/lead")
                ),
                (
                    "doco:Paragraph",
                    None,
                    "Le conseil explique sa décision.",
                    Some("/record/body")
                ),
                (
                    "doco:Paragraph",
                    None,
                    "Les taxes restent stables.",
                    Some("/record/body")
                ),
            ]
        );
        assert_eq!(c.elements[0].id, "elem-00001");
        assert_eq!(c.elements[3].id, "elem-00004");
        assert!(c
            .elements
            .iter()
            .all(|e| e.provenance == "xml" && e.evidence == "declared" && e.bbox.is_none()));
    }

    #[test]
    fn what_the_record_states_is_stated_under_the_model_s_properties() {
        let c = convert(&xml::read(ARTICLE).unwrap(), &web());
        let info = &c.notes.info;
        assert_eq!(
            info.title.as_deref(),
            Some("Le conseil municipal adopte son budget")
        );
        assert_eq!(info.created.as_deref(), Some("2025-04-12T06:26:14.821Z"));
        assert_eq!(
            info.class.as_deref(),
            Some("https://example.org/model#WebArticle")
        );
        let lit = |property: &str, value: &str, datatype: Option<&str>| Property {
            property: format!("{MODEL}{property}"),
            value: PropertyValue::Literal {
                value: value.into(),
                datatype: datatype.map(|d| format!("{XSD}{d}")),
            },
        };
        assert_eq!(
            info.properties,
            [
                lit("recordId", "48213", None),
                lit("published", "2025-04-12T06:26:14.821Z", Some("dateTime")),
                // Found in the list whatever its case.
                Property {
                    property: format!("{MODEL}theme"),
                    value: PropertyValue::Iri {
                        iri: "https://example.org/id/economy".into()
                    },
                },
            ]
        );
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    }

    #[test]
    fn the_record_is_kept_as_the_source_held_it() {
        let c = convert(&xml::read(ARTICLE).unwrap(), &web());
        let shown: Vec<(&str, FieldRole, Option<&str>)> = c
            .notes
            .fields
            .iter()
            .map(|f| (f.path.as_str(), f.role, f.value.as_deref()))
            .collect();
        assert_eq!(
            shown,
            [
                ("/record/id", FieldRole::Metadata, Some("48213")),
                ("/record/body", FieldRole::Content, None),
                ("/record/title", FieldRole::Content, None),
                ("/record/lead", FieldRole::Content, None),
                (
                    "/record/published",
                    FieldRole::Metadata,
                    Some("2025-04-12T06:26:14.821Z")
                ),
                ("/record/theme", FieldRole::Enum, Some("économie")),
                // `subject` held `null`, which this format reads as absent.
                ("/record/region", FieldRole::Unmapped, Some("Bretagne")),
            ]
        );
        let theme = &c.notes.fields[5];
        assert_eq!(theme.iri.as_deref(), Some("https://example.org/id/economy"));
        assert_eq!(
            theme.property.as_deref(),
            Some("https://example.org/model#theme")
        );
    }

    #[test]
    fn the_fields_the_title_and_dates_are_read_from_are_shown_as_stated() {
        let format = SourceFormat::from_json(
            r#"{ "match": { "syntax": "xml", "root": "r" },
                 "title": "name", "created": "made", "modified": "changed" }"#,
        )
        .unwrap();
        let c = convert(
            &xml::read(
                "<r><name>Un  titre\n sur deux lignes</name><made>20260914063000</made>\
                 <changed>bientôt</changed></r>",
            )
            .unwrap(),
            &format,
        );
        let shown: Vec<(&str, FieldRole, Option<&str>, Option<&str>)> = c
            .notes
            .fields
            .iter()
            .map(|f| {
                (
                    f.path.as_str(),
                    f.role,
                    f.value.as_deref(),
                    f.property.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "/r/name",
                    FieldRole::Metadata,
                    Some("Un titre sur deux lignes"),
                    Some("http://purl.org/dc/terms/title")
                ),
                (
                    "/r/made",
                    FieldRole::Metadata,
                    Some("2026-09-14T06:30:00"),
                    Some("http://purl.org/dc/terms/created")
                ),
                // Not a date: shown as written, and the document's
                // `modified` is left unstated.
                (
                    "/r/changed",
                    FieldRole::Metadata,
                    Some("bientôt"),
                    Some("http://purl.org/dc/terms/modified")
                ),
            ]
        );
        assert_eq!(c.notes.info.created.as_deref(), Some("2026-09-14T06:30:00"));
        assert_eq!(c.notes.info.modified, None);
    }

    #[test]
    fn a_value_that_does_not_fit_is_stated_as_written_and_said() {
        let odd = ARTICLE
            .replace("économie", "Sports")
            .replace("2025-04-12T06:26:14.821Z", "hier");
        let c = convert(&xml::read(&odd).unwrap(), &web());
        let values: Vec<&PropertyValue> =
            c.notes.info.properties.iter().map(|p| &p.value).collect();
        assert_eq!(
            values[1..],
            [
                &PropertyValue::Literal {
                    value: "hier".into(),
                    datatype: None
                },
                &PropertyValue::Literal {
                    value: "Sports".into(),
                    datatype: None
                },
            ]
        );
        assert_eq!(c.notes.info.created, None);
        assert_eq!(c.warnings.len(), 3, "{:?}", c.warnings);
        assert!(c.warnings.iter().any(|w| w.contains("`Sports`")));
        assert!(c.warnings.iter().any(|w| w.contains("not a date")));
    }

    #[test]
    fn a_json_record_reads_the_same_way() {
        let format = SourceFormat::from_json(
            r#"{
            "match": { "syntax": "json", "required": ["headline", "text"] },
            "content": [{ "field": "headline", "as": "title" }, { "field": "text" }],
            "enums": [{ "field": "tags", "property": "https://example.org/model#tag",
                        "separator": ";",
                        "values": { "a": "https://example.org/id/a", "b": "https://example.org/id/b" } }]
        }"#,
        )
        .unwrap();
        let record = json::read(
            r#"{"text": ["Un.\n\nDeux,\nsuite."], "headline": "Titre", "tags": ["a; b", "c"]}"#,
        )
        .unwrap();
        let c = convert(&record, &format);
        let read: Vec<(&str, &str)> = c
            .elements
            .iter()
            .map(|e| (e.text.as_str(), e.source_path.as_deref().unwrap()))
            .collect();
        assert_eq!(
            read,
            [
                ("Titre", "/headline"),
                ("Un.", "/text/0"),
                ("Deux, suite.", "/text/0")
            ]
        );
        assert!(c.elements.iter().all(|e| e.provenance == "json"));
        let tags: Vec<(&str, Option<&str>)> = c
            .notes
            .fields
            .iter()
            .filter(|f| f.role == FieldRole::Enum)
            .map(|f| (f.path.as_str(), f.iri.as_deref()))
            .collect();
        assert_eq!(
            tags,
            [
                ("/tags/0", Some("https://example.org/id/a")),
                ("/tags/0", Some("https://example.org/id/b")),
                ("/tags/1", None),
            ]
        );
    }

    #[test]
    fn a_code_is_shown_with_what_its_concept_is_called() {
        let format = SourceFormat::from_json(
            r#"{
            "match": { "syntax": "xml", "root": "r" },
            "enums": [{ "field": "platform", "property": "https://example.org/model#platform",
                        "matchOn": "notation",
                        "values": { "2": "https://example.org/id/platform/2" },
                        "labels": { "https://example.org/id/platform/2": "Television" } }]
        }"#,
        )
        .unwrap();
        let c = convert(
            &xml::read("<r><platform>2</platform><platform>9</platform></r>").unwrap(),
            &format,
        );
        let shown: Vec<(Option<&str>, Option<&str>, Option<&str>)> = c
            .notes
            .fields
            .iter()
            .map(|f| (f.value.as_deref(), f.iri.as_deref(), f.label.as_deref()))
            .collect();
        assert_eq!(
            shown,
            [
                (
                    Some("2"),
                    Some("https://example.org/id/platform/2"),
                    Some("Television")
                ),
                (Some("9"), None, None),
            ]
        );
    }

    #[test]
    fn a_declared_container_is_the_fields_inside_it() {
        let format = SourceFormat::from_json(
            r#"{ "match": { "syntax": "xml", "root": "doc" }, "content": [{ "field": "body" }] }"#,
        )
        .unwrap();
        let c = convert(
            &xml::read("<doc><body><p>Un.</p><p>Deux.</p></body><bodyguard>x</bodyguard></doc>")
                .unwrap(),
            &format,
        );
        let read: Vec<(&str, &str)> = c
            .elements
            .iter()
            .map(|e| (e.text.as_str(), e.source_path.as_deref().unwrap()))
            .collect();
        assert_eq!(
            read,
            [("Un.", "/doc/body/p[1]"), ("Deux.", "/doc/body/p[2]")]
        );
        assert_eq!(c.notes.fields[2].role, FieldRole::Unmapped);
    }

    fn segment(start_ms: u64, end_ms: u64, field: &str, value: &str) -> Segment {
        Segment {
            start_ms,
            end_ms,
            fields: vec![Field {
                path: field.into(),
                name: field.into(),
                value: value.into(),
            }],
        }
    }

    fn asset() -> Record {
        Record {
            syntax: Syntax::Axf,
            root: "EPISODE".into(),
            fields: vec![Field {
                path: "Meta:MAINTITLE".into(),
                name: "MAINTITLE".into(),
                value: "Le bulletin du soir".into(),
            }],
            tracks: vec![
                Track {
                    name: "CAPTIONS".into(),
                    path: "Stratum:CAPTIONS".into(),
                    segments: vec![
                        segment(1_000, 2_000, "TEXT", "Bonsoir."),
                        segment(10_000, 11_000, "TEXT", "Le budget"),
                        segment(11_000, 12_000, "TEXT", "passe."),
                        // Logged late, said early.
                        segment(20_500, 21_500, "TEXT", "- Du soleil."),
                        segment(20_000, 20_500, "TEXT", "Autre sujet :"),
                    ],
                },
                Track {
                    name: "STORIES".into(),
                    path: "Stratum:STORIES".into(),
                    segments: vec![
                        segment(20_000, 30_000, "TITLE", "La météo de demain"),
                        segment(10_000, 20_000, "TITLE", "Le budget est adopté"),
                    ],
                },
                Track {
                    name: "TRANSCRIPT".into(),
                    path: "Stratum:TRANSCRIPT".into(),
                    segments: vec![segment(1_000, 2_000, "TEXT", "Bonsoir (transcrit).")],
                },
            ],
        }
    }

    fn asset_format() -> SourceFormat {
        SourceFormat::from_json(
            r#"{
            "match": { "syntax": "axf", "root": "EPISODE" },
            "title": "MAINTITLE",
            "content": [{ "field": "MAINTITLE", "as": "title" }],
            "tracks": [
                { "track": "MISSING", "field": "TEXT", "role": "speech" },
                { "track": "CAPTIONS", "field": "TEXT", "role": "speech" },
                { "track": "TRANSCRIPT", "field": "TEXT", "role": "speech" },
                { "track": "STORIES", "field": "TITLE", "role": "sections" }
            ]
        }"#,
        )
        .unwrap()
    }

    #[test]
    fn a_recording_reads_as_its_sections_and_what_was_said_in_each() {
        let c = convert(&asset(), &asset_format());
        /// Kind, level, text, and the turn's start and end.
        type Read<'a> = (&'a str, Option<usize>, &'a str, Option<(u64, u64)>);
        let read: Vec<Read> = c
            .elements
            .iter()
            .map(|e| {
                (
                    e.kind.as_str(),
                    e.level,
                    e.text.as_str(),
                    e.turn.as_ref().map(|t| (t.start_ms, t.end_ms)),
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                ("doco:SectionTitle", Some(1), "Le bulletin du soir", None),
                // Said before the first section began.
                ("doco:Paragraph", None, "Bonsoir.", Some((1_000, 2_000))),
                (
                    "doco:SectionTitle",
                    Some(2),
                    "Le budget est adopté",
                    Some((10_000, 20_000))
                ),
                (
                    "doco:Paragraph",
                    None,
                    "Le budget passe.",
                    Some((10_000, 12_000))
                ),
                (
                    "doco:SectionTitle",
                    Some(2),
                    "La météo de demain",
                    Some((20_000, 30_000))
                ),
                (
                    "doco:Paragraph",
                    None,
                    "Autre sujet :",
                    Some((20_000, 20_500))
                ),
                ("doco:Paragraph", None, "Du soleil.", Some((20_500, 21_500))),
            ]
        );
        let paths: Vec<&str> = c
            .elements
            .iter()
            .map(|e| e.source_path.as_deref().unwrap())
            .collect();
        assert_eq!(paths[0], "Meta:MAINTITLE");
        assert_eq!(paths[1], "Stratum:CAPTIONS");
        assert_eq!(paths[2], "Stratum:STORIES");
    }

    #[test]
    fn of_two_speech_tracks_the_first_held_is_the_one_read() {
        let c = convert(&asset(), &asset_format());
        assert!(!c.elements.iter().any(|e| e.text.contains("transcrit")));
        let tracks: Vec<&str> = c
            .notes
            .fields
            .iter()
            .filter(|f| f.path.starts_with("Stratum:"))
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(tracks, ["Stratum:STORIES", "Stratum:CAPTIONS"]);
    }

    #[test]
    fn a_turn_does_not_run_across_the_start_of_a_section() {
        let mut record = asset();
        // No pause between the last words of one story and the first of the
        // next: only the section's start separates them.
        record.tracks[0].segments = vec![
            segment(18_000, 19_900, "TEXT", "Fin du premier sujet."),
            segment(20_000, 21_000, "TEXT", "Début du second."),
        ];
        let c = convert(&record, &asset_format());
        let said: Vec<&str> = c
            .elements
            .iter()
            .filter(|e| e.kind == "doco:Paragraph")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(said, ["Fin du premier sujet.", "Début du second."]);
    }

    #[test]
    fn a_listed_value_is_found_whatever_its_case_or_accents() {
        assert!(same_value("Montréal", "montreal "));
        assert!(same_value("Trois-Rivières", "TROIS-RIVIERES"));
        assert!(same_value("Sept-Îles", "Sept-îles"));
        assert!(!same_value("Québec", "Quebec City"));
    }

    #[test]
    fn dates_are_read_from_the_forms_exports_write() {
        assert_eq!(
            moment("20250424153338").as_deref(),
            Some("2025-04-24T15:33:38")
        );
        assert_eq!(moment("20250424").as_deref(), Some("2025-04-24"));
        assert_eq!(
            moment("2025-04-24 15:33:38").as_deref(),
            Some("2025-04-24T15:33:38")
        );
        assert_eq!(
            moment("2025-04-12T06:26:14.821Z").as_deref(),
            Some("2025-04-12T06:26:14.821Z")
        );
        for not in [
            "20251324",
            "20250424253338",
            "24/04/2025",
            "hier",
            "2025",
            "",
        ] {
            assert_eq!(moment(not), None, "{not}");
        }
    }

    #[test]
    fn a_typed_value_is_checked_for_the_types_a_store_compares() {
        let xsd = |t: &str| format!("{XSD}{t}");
        assert_eq!(
            typed("20250424", &xsd("date")).as_deref(),
            Some("2025-04-24")
        );
        assert_eq!(
            typed("20250424153338", &xsd("date")).as_deref(),
            Some("2025-04-24")
        );
        assert_eq!(typed("20250424", &xsd("dateTime")), None);
        assert_eq!(typed("183000", &xsd("time")).as_deref(), Some("18:30:00"));
        assert_eq!(
            typed("18:30:00.5Z", &xsd("time")).as_deref(),
            Some("18:30:00.5Z")
        );
        assert_eq!(typed("253000", &xsd("time")), None);
        assert_eq!(typed("18h30", &xsd("time")), None);
        assert_eq!(typed(" 042 ", &xsd("integer")).as_deref(), Some("42"));
        assert_eq!(typed("4.5", &xsd("integer")), None);
        assert_eq!(typed("29.97", &xsd("decimal")).as_deref(), Some("29.97"));
        assert_eq!(typed("1", &xsd("boolean")).as_deref(), Some("true"));
        assert_eq!(typed("oui", &xsd("boolean")), None);
        assert_eq!(typed("x", &xsd("anyURI")).as_deref(), Some("x"));
        assert_eq!(typed("x", "https://example.org/type").as_deref(), Some("x"));
    }

    #[test]
    fn paragraphs_follow_blank_lines_where_there_are_any() {
        assert_eq!(
            paragraphs("Un.\nDeux.\r\nTrois."),
            ["Un.", "Deux.", "Trois."]
        );
        assert_eq!(
            paragraphs("Un,\nsuite.\n\n\nDeux.\n"),
            ["Un, suite.", "Deux."]
        );
        assert!(paragraphs(" \n ").is_empty());
    }
}
