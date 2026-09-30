//! Avid media assets to DoCO-typed document elements.
//!
//! AXF, the Asset eXchange Format, is what Avid's asset management (Interplay
//! MAM, MediaCentral Asset Management) exports an asset as: a programme, an
//! episode, a news story, with everything the archive knows about it. It is
//! XML by notation and a small database by content:
//!
//! - an `AXFRoot` holding `MAObject`s, each with a `GUID` and a class
//!   (`mdclass`);
//! - on each object, flat `Meta` fields, a name and a value, all strings;
//! - on the asset, `StratumEx` tracks: timelines cut into `Segment`s, each
//!   with a `begin` and an `end` in milliseconds and, by `contentid`, the
//!   object that holds what the segment says;
//! - beside the objects, `MVAttribute`s: the values of an object's
//!   multi-valued attributes, one element a value, each a group of `Meta`
//!   fields naming its object by `objectid`.
//!
//! So a caption is three things in three places: a segment on the caption
//! track, the object it points at, and that object's one field. This reader
//! puts them back together and hands over a [`Record`]: the asset's fields,
//! its attributes' among them, and its tracks with each segment's fields
//! beside its timing.
//!
//! The container is Avid's and is the same everywhere. What is in it is the
//! archive's: the classes, the field names and the tracks are the data
//! model each site configures, and a track named `SITE_RUNDOWN` means something to one
//! archive only. Which field is prose and which track is speech is
//! therefore a [`SourceFormat`]'s to declare. With none, [`default_format`]
//! reads what every asset has: its title, its dates, and the tracks that
//! are plainly captions.

use fluree_doc_record::{
    decode, xml, Content, ContentKind, Field, Match, Record, Segment, SourceFormat, Syntax, Track,
    TrackRole, TrackRule,
};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::collections::{HashMap, HashSet};

/// The root element every AXF file has.
const ROOT: &str = "AXFRoot";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AxfError {
    /// The file is not an AXF export: its root is not `AXFRoot`.
    NotAxf,
    /// The file is not well-formed XML.
    Malformed(String),
    /// The export holds no object.
    NoAsset,
}

impl std::fmt::Display for AxfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAxf => write!(f, "not an AXF file: its root element is not <{ROOT}>"),
            Self::Malformed(e) => write!(f, "not well-formed: {e}"),
            Self::NoAsset => write!(f, "the AXF file holds no asset"),
        }
    }
}

impl std::error::Error for AxfError {}

/// Whether a file is an AXF export, by its content.
///
/// The exports are named `.axf`, which no system has a type for, and are
/// UTF-16, which makes them binary to anything that looks at bytes. The
/// root element is what says it.
pub fn sniff(bytes: &[u8]) -> bool {
    // UTF-16 is two bytes a character, and the root follows a declaration
    // and sometimes a comment.
    let head = decode(&bytes[..bytes.len().min(4096) & !1]);
    xml::root_name(&head).as_deref() == Some(ROOT)
}

/// An object as the file holds it.
#[derive(Default)]
struct Object {
    class: String,
    guid: String,
    fields: Vec<(String, String)>,
    strata: Vec<Stratum>,
}

struct Stratum {
    name: String,
    segments: Vec<Cut>,
}

/// One value of a multi-valued attribute as the file holds it: a group of
/// fields, written beside the object it is about.
struct Attribute {
    /// The attribute's name: `type` in the file.
    name: String,
    /// Which value of the attribute this is, as the file numbers them.
    index: Option<String>,
    /// The `GUID` of the object the value is about. Empty when the file
    /// names none.
    object: String,
    fields: Vec<(String, String)>,
}

/// A segment as the file holds it: its timing and the object it points at.
struct Cut {
    begin: u64,
    end: u64,
    content: Option<String>,
}

/// Read an AXF file's bytes into a record.
pub fn read(bytes: &[u8]) -> Result<Record, AxfError> {
    let text = decode(bytes);
    if xml::root_name(&text).as_deref() != Some(ROOT) {
        return Err(AxfError::NotAxf);
    }
    let (objects, attributes) = parse(&text)?;

    // The asset is the object the others are about: the one no segment
    // points at. An export lists it first, and that is not relied on.
    let pointed: HashSet<&str> = objects
        .iter()
        .flat_map(|o| &o.strata)
        .flat_map(|s| &s.segments)
        .filter_map(|c| c.content.as_deref())
        .collect();
    let asset = objects
        .iter()
        .find(|o| !pointed.contains(o.guid.as_str()))
        .or(objects.first())
        .ok_or(AxfError::NoAsset)?;
    let by_guid: HashMap<&str, &Object> = objects
        .iter()
        .filter(|o| !o.guid.is_empty())
        .map(|o| (o.guid.as_str(), o))
        .collect();

    let mut fields = Vec::new();
    if !asset.guid.is_empty() {
        fields.push(Field {
            path: "GUID".into(),
            name: "GUID".into(),
            value: asset.guid.clone(),
        });
    }
    fields.extend(fields_of(asset, &attributes, "", true));

    let tracks = asset
        .strata
        .iter()
        .filter(|s| !s.segments.is_empty())
        .map(|s| {
            let path = format!("Stratum:{}", s.name);
            let segments = s
                .segments
                .iter()
                .map(|c| Segment {
                    start_ms: c.begin,
                    end_ms: c.end.max(c.begin),
                    // A segment pointing at an object the export left out
                    // keeps its place in time and says nothing.
                    fields: c
                        .content
                        .as_deref()
                        .and_then(|guid| by_guid.get(guid))
                        .map(|o| fields_of(o, &attributes, &format!("{path}/"), false))
                        .unwrap_or_default(),
                })
                .collect();
            Track {
                name: s.name.clone(),
                path,
                segments,
            }
        })
        .collect();

    Ok(Record {
        syntax: Syntax::Axf,
        root: asset.class.clone(),
        fields,
        tracks,
    })
}

/// The fields of an object, each path starting with `under`: its own, then
/// those of its multi-valued attributes in the file's order.
///
/// A field of an attribute is named by the attribute and the field,
/// `CONTRIBUTORS/NAME`: an attribute's field is often called what a field
/// of the object is, a title among them, and is not a value of that one.
/// An attribute naming no object is the asset's.
fn fields_of(object: &Object, attributes: &[Attribute], under: &str, asset: bool) -> Vec<Field> {
    let own = object.fields.iter().map(|(name, value)| Field {
        path: format!("{under}Meta:{name}"),
        name: name.clone(),
        value: value.clone(),
    });
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let held = attributes
        .iter()
        .filter(|a| match a.object.as_str() {
            "" => asset,
            guid => guid == object.guid,
        })
        .flat_map(|a| {
            // The file numbers the values. One that does not is numbered by
            // its place among the values of the same attribute.
            let nth = seen.entry(a.name.as_str()).or_default();
            let index = a.index.clone().unwrap_or_else(|| nth.to_string());
            *nth += 1;
            a.fields.iter().map(move |(name, value)| Field {
                path: format!("{under}{ATTRIBUTE}{}[{index}]/Meta:{name}", a.name),
                name: format!("{}/{name}", a.name),
                value: value.clone(),
            })
        });
    own.chain(held).collect()
}

/// What the path of an attribute's field starts with.
const ATTRIBUTE: &str = "Attribute:";

fn attribute(e: &BytesStart<'_>, want: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == want.as_bytes())
        .map(|a| match a.unescape_value() {
            Ok(v) => v.into_owned(),
            Err(_) => String::from_utf8_lossy(&a.value).into_owned(),
        })
}

fn parse(text: &str) -> Result<(Vec<Object>, Vec<Attribute>), AxfError> {
    /// The element whose text is being read.
    enum Reading {
        Nothing,
        Guid,
        Meta(String),
    }

    let mut reader = Reader::from_str(text);
    let mut out: Vec<Object> = Vec::new();
    let mut attributes: Vec<Attribute> = Vec::new();
    let mut open: Option<Object> = None;
    let mut held: Option<Attribute> = None;
    let mut reading = Reading::Nothing;
    let mut value = String::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|e| AxfError::Malformed(e.to_string()))?;
        let (e, empty) = match event {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::Text(t) => {
                if !matches!(reading, Reading::Nothing) {
                    match t.unescape() {
                        Ok(s) => value.push_str(&s),
                        Err(_) => value.push_str(&String::from_utf8_lossy(&t)),
                    }
                }
                continue;
            }
            Event::CData(t) => {
                if !matches!(reading, Reading::Nothing) {
                    value.push_str(&String::from_utf8_lossy(&t));
                }
                continue;
            }
            Event::End(e) => {
                match e.name().as_ref() {
                    b"MAObject" => out.extend(open.take()),
                    b"MVAttribute" => {
                        if let Some(mut a) = held.take().filter(|a| !a.fields.is_empty()) {
                            // Written inside an object and naming none, it
                            // is about that object.
                            if let (true, Some(o)) = (a.object.is_empty(), open.as_ref()) {
                                a.object = o.guid.clone();
                            }
                            attributes.push(a);
                        }
                    }
                    b"GUID" | b"Meta" => {
                        let read = std::mem::replace(&mut reading, Reading::Nothing);
                        let v = std::mem::take(&mut value);
                        let v = v.trim();
                        if v.is_empty() {
                            continue;
                        }
                        match read {
                            Reading::Guid => {
                                if let Some(o) = open.as_mut() {
                                    o.guid = v.to_string();
                                }
                            }
                            // A field inside an attribute is the
                            // attribute's, wherever the attribute is written.
                            Reading::Meta(name) => {
                                let fields = match (held.as_mut(), open.as_mut()) {
                                    (Some(a), _) => Some(&mut a.fields),
                                    (None, Some(o)) => Some(&mut o.fields),
                                    (None, None) => None,
                                };
                                if let Some(fields) = fields {
                                    fields.push((name, v.to_string()));
                                }
                            }
                            Reading::Nothing => {}
                        }
                    }
                    _ => {}
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        match e.name().as_ref() {
            b"MAObject" => {
                // An object written inside another is its own object.
                out.extend(open.take());
                let o = Object {
                    class: attribute(&e, "mdclass").unwrap_or_default(),
                    ..Default::default()
                };
                if empty {
                    out.push(o);
                } else {
                    open = Some(o);
                }
            }
            b"MVAttribute" if !empty => {
                held = Some(Attribute {
                    name: attribute(&e, "type")
                        .or_else(|| attribute(&e, "attribute"))
                        .unwrap_or_default(),
                    index: attribute(&e, "index").filter(|i| !i.trim().is_empty()),
                    object: attribute(&e, "objectid").unwrap_or_default(),
                    fields: Vec::new(),
                });
            }
            b"GUID" if !empty => {
                reading = Reading::Guid;
                value.clear();
            }
            b"Meta" if !empty => {
                if let Some(name) = attribute(&e, "name") {
                    reading = Reading::Meta(name);
                    value.clear();
                }
            }
            b"StratumEx" => {
                if let (Some(o), Some(name)) = (open.as_mut(), attribute(&e, "name")) {
                    o.strata.push(Stratum {
                        name,
                        segments: Vec::new(),
                    });
                }
            }
            b"Segment" => {
                let time = |name: &str| attribute(&e, name).and_then(|v| v.trim().parse().ok());
                if let (Some(stratum), Some(begin), Some(end)) = (
                    open.as_mut().and_then(|o| o.strata.last_mut()),
                    time("begin"),
                    time("end"),
                ) {
                    stratum.segments.push(Cut {
                        begin,
                        end,
                        content: attribute(&e, "contentid").filter(|c| !c.trim().is_empty()),
                    });
                }
            }
            _ => {}
        }
    }
    out.extend(open);
    Ok((out, attributes))
}

/// The fields every asset has, whatever the archive's data model: Avid's
/// own, which a site does not rename.
const TITLE: &str = "MAINTITLE";
const REGISTERED: &str = "REGISTRATION_DATETIME";
const MODIFIED: &str = "MODIFICATION_DATETIME";

/// The format an asset is read by when none is declared for it.
///
/// Its title, when it was registered and last changed, and what was said:
/// the tracks whose segments each hold one field and nothing else, which is
/// what a caption or a transcript line is. A track of stories has a number
/// and a title, a track of locators a colour and a user, and neither is
/// taken for speech. The longest such track comes first, so it is the one
/// read. What an attribute says of a segment is beside the point: a caption
/// with a note on it is a caption.
pub fn default_format(record: &Record) -> SourceFormat {
    let held = |name: &str| record.has(name).then(|| name.to_string());
    fn own(s: &Segment) -> Vec<&str> {
        s.fields
            .iter()
            .filter(|f| !f.path.contains(ATTRIBUTE))
            .map(|f| f.name.as_str())
            .collect()
    }
    let mut speech: Vec<(usize, TrackRule)> = record
        .tracks
        .iter()
        .filter_map(|t| {
            let said: Vec<Vec<&str>> = t.segments.iter().map(own).collect();
            let field = *said.iter().flatten().next()?;
            let saying = said.iter().filter(|s| !s.is_empty());
            saying.clone().all(|s| s[..] == [field]).then(|| {
                (
                    saying.count(),
                    TrackRule {
                        track: t.name.clone(),
                        field: field.to_string(),
                        role: TrackRole::Speech,
                    },
                )
            })
        })
        .collect();
    speech.sort_by_key(|(said, _)| std::cmp::Reverse(*said));

    SourceFormat {
        id: None,
        label: Some("AXF asset (undeclared)".into()),
        matches: Match {
            syntax: Syntax::Axf,
            root: None,
            required: Vec::new(),
        },
        document_class: None,
        null_values: Vec::new(),
        title: held(TITLE),
        created: held(REGISTERED),
        modified: held(MODIFIED),
        content: held(TITLE)
            .into_iter()
            .map(|field| Content {
                field,
                kind: ContentKind::Title,
            })
            .collect(),
        metadata: Vec::new(),
        enums: Vec::new(),
        tracks: speech.into_iter().map(|(_, rule)| rule).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluree_doc_record::convert;

    fn utf16(text: &str) -> Vec<u8> {
        [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect()
    }

    const EPISODE: &str = r#"<?xml version='1.0' encoding='utf-16'?>
<AXFRoot>
  <MAObject type="default" mdclass="EPISODE">
    <GUID dmname="">a-0001</GUID>
    <Meta name="MAINTITLE" format="string">Le bulletin du soir &amp; suite</Meta>
    <Meta name="COMMENT" format="string" />
    <Meta name="DESCRIPTION" format="string">  </Meta>
    <Meta name="REGISTRATION_DATETIME" format="string">20250424153338</Meta>
    <Meta name="MODIFICATION_DATETIME" format="string">20250511010425</Meta>
    <StratumEx name="RESTRICTION" />
    <StratumEx name="CLOSED_CAPTION">
      <Group orderidx="0" id="0" lastchanged="00010101000000">
        <Segment id="0" contentid="c-1" begin="4805" end="9805" />
        <Segment id="1" contentid="c-2" begin="10744" end="11744" />
      </Group>
    </StratumEx>
    <StratumEx name="SEGMENTATION">
      <Group orderidx="0" id="0">
        <Segment id="0" contentid="s-1" begin="0" end="600000" />
      </Group>
    </StratumEx>
    <StratumEx name="SITE_RUNDOWN">
      <Group orderidx="0" id="0">
        <Segment id="1" contentid="not-in-the-file" begin="0" end="6999" />
      </Group>
    </StratumEx>
    <StratumEx name="DEFAULT">
      <Group orderidx="0" id="0">
        <Segment id="0" begin="0" end="1302" />
      </Group>
    </StratumEx>
  </MAObject>
  <MAObject type="default" mdclass="S_CLOSED_CAPTION">
    <GUID dmname="">c-1</GUID>
    <Meta name="CLOSED_CAPTION" format="string">Bonsoir, dans la prochaine</Meta>
  </MAObject>
  <MAObject type="default" mdclass="S_CLOSED_CAPTION">
    <GUID dmname="">c-2</GUID>
    <Meta name="CLOSED_CAPTION" format="string">demi-heure du bulletin :</Meta>
  </MAObject>
  <MAObject type="default" mdclass="S_SEGMENTATION">
    <GUID dmname="">s-1</GUID>
    <Meta name="NUMBER" format="string">ST_1</Meta>
    <Meta name="TITLE" format="string">Budget municipal</Meta>
  </MAObject>
</AXFRoot>"#;

    #[test]
    fn an_export_is_known_by_its_root_in_either_encoding() {
        assert!(sniff(&utf16(EPISODE)));
        assert!(sniff(EPISODE.as_bytes()));
        assert!(!sniff(&utf16("<record><title>T</title></record>")));
        assert!(!sniff(b"AXFRoot in plain text"));
        assert!(!sniff(b""));
        assert_eq!(read(b"<record/>"), Err(AxfError::NotAxf));
    }

    #[test]
    fn an_asset_is_its_fields_and_its_tracks_put_back_together() {
        let r = read(&utf16(EPISODE)).unwrap();
        assert_eq!((r.syntax, r.root.as_str()), (Syntax::Axf, "EPISODE"));
        let fields: Vec<(&str, &str, &str)> = r
            .fields
            .iter()
            .map(|f| (f.path.as_str(), f.name.as_str(), f.value.as_str()))
            .collect();
        assert_eq!(
            fields,
            [
                ("GUID", "GUID", "a-0001"),
                ("Meta:MAINTITLE", "MAINTITLE", "Le bulletin du soir & suite"),
                (
                    "Meta:REGISTRATION_DATETIME",
                    "REGISTRATION_DATETIME",
                    "20250424153338"
                ),
                (
                    "Meta:MODIFICATION_DATETIME",
                    "MODIFICATION_DATETIME",
                    "20250511010425"
                ),
            ]
        );
        let tracks: Vec<(&str, usize)> = r
            .tracks
            .iter()
            .map(|t| (t.name.as_str(), t.segments.len()))
            .collect();
        // A track with no segment is not one the asset has.
        assert_eq!(
            tracks,
            [
                ("CLOSED_CAPTION", 2),
                ("SEGMENTATION", 1),
                ("SITE_RUNDOWN", 1),
                ("DEFAULT", 1),
            ]
        );
        let captions = &r.tracks[0];
        assert_eq!(captions.path, "Stratum:CLOSED_CAPTION");
        assert_eq!(
            (captions.segments[0].start_ms, captions.segments[0].end_ms),
            (4805, 9805)
        );
        assert_eq!(
            captions.segments[1].fields,
            [Field {
                path: "Stratum:CLOSED_CAPTION/Meta:CLOSED_CAPTION".into(),
                name: "CLOSED_CAPTION".into(),
                value: "demi-heure du bulletin :".into(),
            }]
        );
        assert_eq!(r.tracks[1].segments[0].fields.len(), 2);
        // Pointing at nothing, or at no object: a place in time, no words.
        assert!(r.tracks[2].segments[0].fields.is_empty());
        assert!(r.tracks[3].segments[0].fields.is_empty());
    }

    /// Multi-valued attributes, written after the objects as an export
    /// writes them.
    const ATTRIBUTES: &str = r#"  <MVAttribute type="CONTRIBUTORS" index="0" attribute="CONTRIBUTORS" mdclass="EPISODE" objectid="a-0001">
    <Meta name="FUNCTION" format="string">35</Meta>
    <Meta name="NAME" format="string">Claire Fontaine</Meta>
    <Meta name="COMMENTS" format="string" />
  </MVAttribute>
  <MVAttribute type="CONTRIBUTORS" index="1" attribute="CONTRIBUTORS" mdclass="EPISODE" objectid="a-0001">
    <Meta name="NAME" format="string">Marc Tremblay</Meta>
  </MVAttribute>
  <MVAttribute type="COPIES" attribute="COPIES" mdclass="EPISODE" objectid="a-0001">
    <Meta name="MAINTITLE" format="string">Copie de diffusion</Meta>
  </MVAttribute>
  <MVAttribute type="LEGACY" index="0" attribute="LEGACY" mdclass="EPISODE" objectid="a-0001">
    <Meta name="SYSTEM" format="string" />
  </MVAttribute>
  <MVAttribute type="NOTES" index="0" attribute="NOTES" mdclass="S_CLOSED_CAPTION" objectid="c-1">
    <Meta name="NOTE" format="string">Inaudible</Meta>
  </MVAttribute>
</AXFRoot>"#;

    fn with_attributes() -> String {
        EPISODE.replace("</AXFRoot>", ATTRIBUTES)
    }

    #[test]
    fn a_multi_valued_attribute_is_fields_of_the_object_it_names() {
        let r = read(&utf16(&with_attributes())).unwrap();
        let held: Vec<(&str, &str, &str)> = r
            .fields
            .iter()
            .filter(|f| f.path.starts_with(ATTRIBUTE))
            .map(|f| (f.path.as_str(), f.name.as_str(), f.value.as_str()))
            .collect();
        // One path a value, one name an attribute's field. A field holding
        // nothing is not one, and neither is an attribute of such fields.
        assert_eq!(
            held,
            [
                (
                    "Attribute:CONTRIBUTORS[0]/Meta:FUNCTION",
                    "CONTRIBUTORS/FUNCTION",
                    "35"
                ),
                (
                    "Attribute:CONTRIBUTORS[0]/Meta:NAME",
                    "CONTRIBUTORS/NAME",
                    "Claire Fontaine"
                ),
                (
                    "Attribute:CONTRIBUTORS[1]/Meta:NAME",
                    "CONTRIBUTORS/NAME",
                    "Marc Tremblay"
                ),
                (
                    "Attribute:COPIES[0]/Meta:MAINTITLE",
                    "COPIES/MAINTITLE",
                    "Copie de diffusion"
                ),
            ]
        );
        // The asset's own fields come first and are what they were.
        let own = read(&utf16(EPISODE)).unwrap();
        assert_eq!(r.fields[..own.fields.len()], own.fields[..]);
        // The title of a copy is not a title of the asset.
        let titles = r.fields.iter().filter(|f| f.name == "MAINTITLE");
        assert_eq!(titles.count(), 1);

        assert_eq!(
            r.tracks[0].segments[0].fields[1],
            Field {
                path: "Stratum:CLOSED_CAPTION/Attribute:NOTES[0]/Meta:NOTE".into(),
                name: "NOTES/NOTE".into(),
                value: "Inaudible".into(),
            }
        );
        assert_eq!(r.tracks[0].segments[1].fields.len(), 1);
    }

    #[test]
    fn an_attribute_is_read_wherever_it_is_written() {
        // Inside the object and naming none: about that object.
        let inside = r#"<AXFRoot><MAObject mdclass="STORY">
            <GUID>b-0002</GUID>
            <Meta name="MAINTITLE">Le budget est adopté</Meta>
            <MVAttribute type="GENRES" index="0"><Meta name="GENRE">4</Meta></MVAttribute>
            <Meta name="SYNOPSIS">Le budget passe.</Meta>
        </MAObject></AXFRoot>"#;
        let names = |text: &str| -> Vec<String> {
            let r = read(text.as_bytes()).unwrap();
            r.fields.into_iter().map(|f| f.name).collect()
        };
        assert_eq!(
            names(inside),
            ["GUID", "MAINTITLE", "SYNOPSIS", "GENRES/GENRE"]
        );
        // Beside the objects and naming none: the asset's.
        let beside = r#"<AXFRoot><MAObject mdclass="STORY"><GUID>b-0002</GUID></MAObject>
            <MVAttribute type="GENRES"><Meta name="GENRE">4</Meta></MVAttribute></AXFRoot>"#;
        assert_eq!(names(beside), ["GUID", "GENRES/GENRE"]);
        // Naming an object the export left out: nobody's.
        let orphan = beside.replace("<MVAttribute ", "<MVAttribute objectid=\"z-9\" ");
        assert_eq!(names(&orphan), ["GUID"]);
    }

    #[test]
    fn a_caption_with_a_note_on_it_is_still_a_caption() {
        let r = read(&utf16(&with_attributes())).unwrap();
        let format = default_format(&r);
        assert_eq!(format, default_format(&read(&utf16(EPISODE)).unwrap()));
        let c = convert(&r, &format);
        assert_eq!(
            c.elements[1].text,
            "Bonsoir, dans la prochaine demi-heure du bulletin :"
        );
        // Undeclared, what the attributes hold is kept and not read.
        assert_eq!(c.elements.len(), 2);
        assert!(c.notes.fields.iter().any(|f| {
            f.path == "Attribute:CONTRIBUTORS[1]/Meta:NAME"
                && f.value.as_deref() == Some("Marc Tremblay")
        }));
    }

    #[test]
    fn the_asset_is_the_object_nothing_points_at_wherever_it_is_listed() {
        let (head, rest) = EPISODE.split_once("  <MAObject").unwrap();
        let (asset, linked) = rest.split_once("  <MAObject").unwrap();
        let (linked, tail) = linked.rsplit_once("</AXFRoot>").unwrap();
        let reordered = format!("{head}  <MAObject{linked}  <MAObject{asset}</AXFRoot>{tail}");
        assert_eq!(read(reordered.as_bytes()).unwrap().root, "EPISODE");
    }

    #[test]
    fn undeclared_an_asset_reads_as_its_title_and_what_was_said() {
        let r = read(&utf16(EPISODE)).unwrap();
        let format = default_format(&r);
        assert_eq!(format.title.as_deref(), Some("MAINTITLE"));
        // Captions hold one field a segment. A story holds a number and a
        // title, and is not speech.
        assert_eq!(
            format.tracks,
            [TrackRule {
                track: "CLOSED_CAPTION".into(),
                field: "CLOSED_CAPTION".into(),
                role: TrackRole::Speech,
            }]
        );
        assert!(format.validate().is_ok());

        let c = convert(&r, &format);
        /// Kind, text, and the turn's start and end.
        type Read<'a> = (&'a str, &'a str, Option<(u64, u64)>);
        let read: Vec<Read> = c
            .elements
            .iter()
            .map(|e| {
                (
                    e.kind.as_str(),
                    e.text.as_str(),
                    e.turn.as_ref().map(|t| (t.start_ms, t.end_ms)),
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                ("doco:SectionTitle", "Le bulletin du soir & suite", None),
                (
                    "doco:Paragraph",
                    "Bonsoir, dans la prochaine demi-heure du bulletin :",
                    Some((4805, 11744))
                ),
            ]
        );
        assert!(c.elements.iter().all(|e| e.provenance == "axf"));
        assert_eq!(c.notes.info.created.as_deref(), Some("2025-04-24T15:33:38"));
        assert_eq!(
            c.notes.info.modified.as_deref(),
            Some("2025-05-11T01:04:25")
        );
    }

    #[test]
    fn a_story_with_no_track_is_its_fields() {
        let story = r#"<AXFRoot><MAObject type="default" mdclass="STORY">
            <GUID>b-0002</GUID>
            <Meta name="MAINTITLE" format="string">Le budget est adopté</Meta>
            <Meta name="SYNOPSIS" format="string">Le budget passe.</Meta>
        </MAObject></AXFRoot>"#;
        let r = read(story.as_bytes()).unwrap();
        assert_eq!(r.root, "STORY");
        assert!(r.tracks.is_empty());
        let c = convert(&r, &default_format(&r));
        // The synopsis is the archive's field, and undeclared it is kept
        // and not read.
        assert_eq!(c.elements.len(), 1);
        assert!(c
            .notes
            .fields
            .iter()
            .any(|f| f.path == "Meta:SYNOPSIS" && f.value.as_deref() == Some("Le budget passe.")));
    }

    #[test]
    fn a_file_with_no_object_or_not_well_formed_is_an_error() {
        assert_eq!(read(b"<AXFRoot></AXFRoot>"), Err(AxfError::NoAsset));
        assert!(matches!(
            read(b"<AXFRoot><MAObject mdclass=\"X\"><GUID>1</Meta></MAObject></AXFRoot>"),
            Err(AxfError::Malformed(_))
        ));
    }
}
