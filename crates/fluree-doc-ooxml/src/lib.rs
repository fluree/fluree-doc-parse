//! The package layer under `.docx`, `.pptx` and `.xlsx`.
//!
//! All three are Open Packaging Conventions packages: a zip of XML parts tied
//! together by relationships. A part names another by a relationship id, and
//! the part's relationships part (`dir/_rels/name.xml.rels`) says which part
//! that id is, by a path relative to the part's own directory. Which part is
//! the document, which holds its styles, which are its slides: each is a
//! relationship, and conventional paths (`word/document.xml`, `xl/`) are only
//! where most writers put them. So the readers ask the package, and fall back
//! to the convention where a package does not say.

use fluree_doc_model::budget::{Budget, TooLarge, PART_BYTES};
use fluree_doc_model::{
    xsd_date_time, Allowance, DocumentInfo, Media, MediaSource, PictureReader, PictureReads,
};
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::Read;

/// An opened package.
pub struct Package<'a> {
    zip: zip::ZipArchive<std::io::Cursor<&'a [u8]>>,
}

/// One relationship of a part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relationship {
    /// The id the source part names it by (`rId2`).
    pub id: String,
    /// The relationship type, an IRI ending in what it is (`…/slide`).
    pub kind: String,
    /// The part it points to, as an archive path, or an external URL as
    /// written.
    pub target: String,
    /// `TargetMode="External"`: `target` is a URL, not a part.
    pub external: bool,
}

impl Relationship {
    /// Whether the type is `name`, by its last segment: `is("slide")` holds
    /// for the transitional and the strict namespaces alike.
    pub fn is(&self, name: &str) -> bool {
        self.kind.rsplit('/').next() == Some(name)
    }
}

/// The most bytes one image part is read to: 64 MiB, more than any picture
/// a document pastes in. A sixteenth built for the fuzz targets, as the
/// budget is ([`fluree_doc_model::budget::scaled`]).
pub const MAX_MEDIA_BYTES: u64 = fluree_doc_model::budget::scaled(64 << 20);

/// The most bytes an XML part read beside a document's main one (its
/// relationships, its core properties, its notes, comments and settings)
/// is inflated to: 64 MiB, far more than any holds (scaled as
/// [`MAX_MEDIA_BYTES`]).
pub const MAX_XML_BYTES: u64 = fluree_doc_model::budget::scaled(64 << 20);

/// The bytes of an image part of an opened package, at most
/// [`MAX_MEDIA_BYTES`] of them. The package is opened once for all of a
/// document's pictures: opening it reads the archive's whole directory.
pub fn read_media(pkg: &mut Package<'_>, part: &str) -> Result<Vec<u8>, String> {
    pkg.read_bytes(part, MAX_MEDIA_BYTES)
}

/// Read a document's pictures from its opened package
/// ([`fluree_doc_model::read_pictures`]): each image once, however many
/// figures draw it, one at a time, within `allowance`.
pub fn read_pictures<R: PictureReader>(
    pkg: &mut Package<'_>,
    media: Vec<Media>,
    reader: &mut R,
    allowance: Allowance,
) -> PictureReads<R::Reading> {
    fluree_doc_model::read_pictures(media, &mut |part| read_media(pkg, part), reader, allowance)
}

/// The first bytes of an image part of an opened package, enough to tell
/// its format by (`fluree_doc_pdf::image::Format::sniff`); only they are
/// decompressed.
pub fn read_media_head(pkg: &mut Package<'_>, part: &str) -> Result<Vec<u8>, String> {
    let entry = pkg.zip.by_name(part).map_err(|e| format!("{part}: {e}"))?;
    let mut v = Vec::new();
    entry
        .take(MEDIA_HEAD_BYTES)
        .read_to_end(&mut v)
        .map_err(|e| format!("{part}: {e}"))?;
    Ok(v)
}

/// How many bytes [`read_media_head`] reads.
const MEDIA_HEAD_BYTES: u64 = 16;

/// What a picture says in words: its alternative text (`descr`), or else its
/// title, on one line, without the notice Office appends to text it wrote
/// itself.
pub fn alt_text(descr: &str, title: &str) -> String {
    let alt = if descr.trim().is_empty() {
        title
    } else {
        descr
    };
    alt.lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Description automatically generated")
                && !l.starts_with("AI-generated content may be incorrect")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A part's relationships by id, each found in one lookup: a reader that
/// scanned them all for each id a part names spent their count times the
/// names', and an id no relationship has cost the whole scan for nothing.
/// Where several relationships share an id, the first is the one found, as
/// a scan in order found it.
pub struct Relations<'r> {
    first: HashMap<&'r str, &'r Relationship>,
    internal: HashMap<&'r str, &'r str>,
}

impl<'r> Relations<'r> {
    pub fn new(rels: &'r [Relationship]) -> Self {
        let mut first = HashMap::new();
        let mut internal = HashMap::new();
        for r in rels {
            first.entry(r.id.as_str()).or_insert(r);
            if !r.external {
                internal.entry(r.id.as_str()).or_insert(r.target.as_str());
            }
        }
        Relations { first, internal }
    }

    /// The first relationship of id `rid`, within the package or not.
    pub fn get(&self, rid: &str) -> Option<&'r Relationship> {
        self.first.get(rid).copied()
    }

    /// The part the first relationship of id `rid` within the package
    /// names: one outside it (`TargetMode="External"`) is passed over.
    pub fn part(&self, rid: &str) -> Option<&'r str> {
        self.internal.get(rid).copied()
    }
}

/// A package's part names, to find the part a relationship names as the
/// package format finds it: exactly, or else ignoring case, a target written
/// with backslashes, as some writers do, read with slashes.
pub struct PartNames {
    exact: std::collections::HashSet<String>,
    folded: std::collections::HashMap<String, String>,
}

impl PartNames {
    pub fn new(pkg: &Package<'_>) -> Self {
        let names = pkg.part_names();
        PartNames {
            folded: names
                .iter()
                .map(|p| (p.to_lowercase(), p.clone()))
                .collect(),
            exact: names.into_iter().collect(),
        }
    }

    /// Where the image of the relationship `rid` is, among a part's
    /// relationships `rels`. A link to a file outside the package, or an id
    /// no relationship has, is still named, by what it was given, for a
    /// reader to say which image it could not read; a part that is not
    /// there is named as given, and fails to read.
    pub fn resolve(&self, rels: &Relations<'_>, rid: &str) -> MediaSource {
        match rels.get(rid) {
            Some(r) if !r.external => MediaSource::Part(if self.exact.contains(&r.target) {
                r.target.clone()
            } else {
                self.folded
                    .get(&r.target.to_lowercase())
                    .map_or_else(|| r.target.clone(), Clone::clone)
            }),
            Some(r) => MediaSource::External(r.target.clone()),
            None => MediaSource::Missing(rid.to_string()),
        }
    }
}

impl<'a> Package<'a> {
    /// Open a package's bytes. The error is the archive's own message.
    pub fn open(bytes: &'a [u8]) -> Result<Self, String> {
        zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map(|zip| Package { zip })
            .map_err(|e| e.to_string())
    }

    /// A part's text, read whole within what is left of the document's
    /// `budget` and spent from it ([`Budget::spend_bytes`], at
    /// [`PART_BYTES`] a byte): `Ok(None)`
    /// where the package has no such part or it does not read, and refused
    /// as too large where it holds more than the budget has left. A main
    /// part has no bound of its own, and a few kilobytes of archive can
    /// inflate to gigabytes.
    pub fn read_within(
        &mut self,
        part: &str,
        budget: &mut Budget,
    ) -> Result<Option<String>, TooLarge> {
        let max = budget.memory_left() / PART_BYTES;
        match self.read_capped(part, max) {
            Ok(text) => {
                let len = text.as_ref().map_or(0, String::len);
                budget.spend_bytes(len as u64)?;
                Ok(text)
            }
            // Past what is left: spending it refuses, naming the budget.
            Err(_) => budget.spend_bytes(max + 1).map(|()| None),
        }
    }

    /// A part's text, at most `max` bytes of it, checked as it inflates:
    /// `Ok(None)` where the package has no such part, or it does not read
    /// (a broken entry, text that is not UTF-8), and refused, naming the
    /// part, where it holds more, or more than memory can be found for.
    pub fn read_capped(&mut self, part: &str, max: u64) -> Result<Option<String>, String> {
        let Ok(entry) = self.zip.by_name(part) else {
            return Ok(None);
        };
        let too_large = || format!("{part}: more than the {max} bytes read");
        if entry.size() > max {
            return Err(too_large());
        }
        let declared = entry.size();
        match read_declared(entry, declared, max) {
            Ok(v) => Ok(String::from_utf8(v).ok()),
            Err(Unread::TooLarge) => Err(too_large()),
            Err(Unread::Broken) => Ok(None),
        }
    }

    /// A part's bytes, for a binary part such as an image, at most `max` of
    /// them. Refused, naming the part, where the package has no such part or
    /// it holds more: a few kilobytes of archive can inflate to gigabytes, and
    /// the size an archive declares is checked as it is read, not believed.
    pub fn read_bytes(&mut self, part: &str, max: u64) -> Result<Vec<u8>, String> {
        let entry = self.zip.by_name(part).map_err(|e| format!("{part}: {e}"))?;
        if entry.size() > max {
            return Err(format!(
                "{part}: {} bytes uncompressed, more than the {max} read",
                entry.size()
            ));
        }
        let mut v = Vec::new();
        entry
            .take(max + 1)
            .read_to_end(&mut v)
            .map_err(|e| format!("{part}: {e}"))?;
        if v.len() as u64 > max {
            return Err(format!("{part}: more than the {max} bytes read"));
        }
        Ok(v)
    }

    /// Every part's name, in archive order.
    pub fn part_names(&self) -> Vec<String> {
        // A name the archive cannot decode names no part a reader can ask for.
        self.zip
            .file_names()
            .filter_map(|n| n.ok().map(|n| n.into_owned()))
            .collect()
    }

    /// A part's relationships, each internal target resolved to a part name.
    /// Empty where the part has none; refused, naming its relationships
    /// part, where that holds more than [`MAX_XML_BYTES`]. Read as none, a
    /// document past it lost its headers, notes and pictures without a
    /// word.
    pub fn relationships(&mut self, part: &str) -> Result<Vec<Relationship>, String> {
        Ok(self
            .read_capped(&rels_part(part), MAX_XML_BYTES)?
            .map(|xml| relationships(&xml, part))
            .unwrap_or_default())
    }

    /// The package's own relationship of type `name` (`officeDocument`,
    /// `core-properties`), from its root relationships part; none where that
    /// part holds more than [`MAX_XML_BYTES`].
    pub fn root(&mut self, name: &str) -> Option<String> {
        self.read_capped("_rels/.rels", MAX_XML_BYTES)
            .ok()
            .flatten()
            .map(|xml| relationships(&xml, ""))
            .unwrap_or_default()
            .into_iter()
            .find(|r| r.is(name) && !r.external)
            .map(|r| r.target)
    }

    /// The document's main part, as the package's root relationship names
    /// it, or `fallback` — where writers conventionally put it — when the
    /// package does not say.
    pub fn main_part(&mut self, fallback: &str) -> String {
        self.root("officeDocument")
            .filter(|p| self.zip.index_for_name(p).is_some())
            .unwrap_or_else(|| fallback.to_string())
    }

    /// The part `source` relates to by type `name`, or `fallback`; refused
    /// as [`Package::relationships`] is.
    pub fn related(&mut self, source: &str, name: &str, fallback: &str) -> Result<String, String> {
        Ok(self
            .relationships(source)?
            .into_iter()
            .find(|r| r.is(name) && !r.external)
            .map(|r| r.target)
            .unwrap_or_else(|| fallback.to_string()))
    }

    /// What the file declares about itself in its core properties: title,
    /// author, and when it was made and last saved; nothing where the part
    /// holds more than [`MAX_XML_BYTES`].
    pub fn info(&mut self) -> DocumentInfo {
        let part = self
            .root("core-properties")
            .unwrap_or_else(|| "docProps/core.xml".to_string());
        self.read_capped(&part, MAX_XML_BYTES)
            .ok()
            .flatten()
            .map(|xml| core_properties(&xml))
            .unwrap_or_default()
    }
}

/// Why a part was not read.
#[derive(Debug, PartialEq, Eq)]
enum Unread {
    /// More than the bound, or more than memory can be found for.
    TooLarge,
    /// A broken entry.
    Broken,
}

/// What `entry` holds, at most `max` bytes, checked as it inflates. The size
/// the archive declares, `declared`, is reserved first where memory is found
/// for it, so a large part does not double its buffer on the way; but it is
/// not believed: where the reservation is refused the buffer grows as the
/// part is read instead, and what a part declares past what it holds is let
/// go once it is read. A growth refused is a part too large, not an abort.
fn read_declared(entry: impl Read, declared: u64, max: u64) -> Result<Vec<u8>, Unread> {
    let mut v = Vec::new();
    let declared = usize::try_from(declared.min(max)).unwrap_or(usize::MAX);
    // Refused, it may well be a lie: grown as read.
    let _ = v.try_reserve_exact(declared);
    match entry.take(max.saturating_add(1)).read_to_end(&mut v) {
        Err(e) if e.kind() == std::io::ErrorKind::OutOfMemory => return Err(Unread::TooLarge),
        Err(_) => return Err(Unread::Broken),
        Ok(_) => {}
    }
    if v.len() as u64 > max {
        return Err(Unread::TooLarge);
    }
    v.shrink_to_fit();
    Ok(v)
}

/// The core properties of a package's bytes; empty where they are not a
/// package or declare none.
pub fn info(bytes: &[u8]) -> DocumentInfo {
    Package::open(bytes)
        .map(|mut p| p.info())
        .unwrap_or_default()
}

/// A part's relationships part: `dir/name.xml` → `dir/_rels/name.xml.rels`;
/// the package's own, for the root, is `_rels/.rels`.
pub fn rels_part(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, name)) => format!("{dir}/_rels/{name}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

/// A relationships part read, each internal target resolved against the
/// directory of `source`, the part they belong to.
pub fn relationships(rels_xml: &str, source: &str) -> Vec<Relationship> {
    let mut out = Vec::new();
    let mut r = Reader::from_str(rels_xml);
    let mut buf = Vec::new();
    let dir = source.rsplit_once('/').map_or("", |(d, _)| d);
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if local(e.name().as_ref()) == "Relationship" => {
                let (Some(id), Some(target)) = (attr(&e, "Id"), attr(&e, "Target")) else {
                    buf.clear();
                    continue;
                };
                let external = attr(&e, "TargetMode").as_deref() == Some("External");
                out.push(Relationship {
                    id,
                    kind: attr(&e, "Type").unwrap_or_default(),
                    target: if external {
                        target
                    } else {
                        // Some writers separate segments with backslashes.
                        resolve_part(dir, &target.replace('\\', "/"))
                    },
                    external,
                });
            }
            _ => {}
        }
        buf.clear();
    }
    out
}

/// A relationship target as an archive path: absolute from the package
/// root when it starts with `/`, otherwise relative to `dir`, with `.` and
/// `..` segments applied.
pub fn resolve_part(dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = match target.strip_prefix('/') {
        Some(_) => Vec::new(),
        None => dir.split('/').filter(|s| !s.is_empty()).collect(),
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// What an Office file declares in its core properties part: `dc:title`,
/// `dc:creator`, and `dcterms:created` / `dcterms:modified`.
///
/// Each is a child of the root, read by local name, so any prefix a writer
/// chose works, and its text is all of its text: CDATA included, comments
/// left out. A date that is not a real one is left out rather than passed on
/// as one. A part that stops parsing keeps what was read before it.
pub fn core_properties(xml: &str) -> DocumentInfo {
    let mut info = DocumentInfo::default();
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut depth = 0usize;
    // The root's child being read, by local name, and its text so far.
    let mut field: Option<(String, String)> = None;
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                depth += 1;
                if depth == 2 {
                    field = Some((local(e.name().as_ref()).to_string(), String::new()));
                }
            }
            Ok(Event::Text(t)) => {
                if let Some((_, text)) = field.as_mut() {
                    text.push_str(&t.unescape().unwrap_or_default());
                }
            }
            Ok(Event::CData(c)) => {
                if let Some((_, text)) = field.as_mut() {
                    text.push_str(&String::from_utf8_lossy(&c));
                }
            }
            Ok(Event::End(_)) => {
                if depth == 2 {
                    if let Some((name, text)) = field.take() {
                        set_core_property(&mut info, &name, &text);
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    info
}

fn set_core_property(info: &mut DocumentInfo, name: &str, text: &str) {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return;
    }
    match name {
        "title" => info.title = Some(text),
        "creator" => info.creators.push(text),
        "created" => info.created = xsd_date_time(&text),
        "modified" => info.modified = xsd_date_time(&text),
        _ => {}
    }
}

/// An element or attribute name without its prefix: `w:pStyle` → `pStyle`.
pub fn local(qname: &[u8]) -> &str {
    let s = std::str::from_utf8(qname).unwrap_or("");
    s.rsplit(':').next().unwrap_or(s)
}

/// The most attributes of one element read. An element carries a few; a
/// part's root, a few dozen namespace declarations.
pub const MAX_ATTRIBUTES: usize = 1024;

/// An element's attributes, the first [`MAX_ATTRIBUTES`] of them, those that
/// do not parse left out. Read without quick-xml's check for a name given
/// twice, which compares each attribute with every one before it: an
/// element of 300,000 attributes would cost their square before a reader
/// saw the first. A name given twice is found at its first.
pub fn attributes<'a>(e: &'a BytesStart<'_>) -> impl Iterator<Item = Attribute<'a>> {
    let mut all = e.attributes();
    all.with_checks(false);
    all.take(MAX_ATTRIBUTES).flatten()
}

/// An attribute's value by local name, its entities unescaped: a sheet
/// named `R&D` is written `name="R&amp;D"`, and is `R&D`.
pub fn attr(e: &BytesStart<'_>, want: &str) -> Option<String> {
    attributes(e).find_map(|a| {
        (local(a.key.as_ref()) == want).then(|| {
            a.unescape_value()
                .map(|v| v.into_owned())
                .unwrap_or_else(|_| String::from_utf8_lossy(a.value.as_ref()).into_owned())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn package(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, body) in parts {
            z.start_file(*name, opts).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        buf.into_inner()
    }

    const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

    /// The first element of `xml`, and what `f` makes of it.
    fn first<T>(xml: &str, f: impl Fn(&BytesStart<'_>) -> T) -> T {
        let mut r = Reader::from_str(xml);
        loop {
            match r.read_event().unwrap() {
                Event::Start(e) | Event::Empty(e) => return f(&e),
                Event::Eof => panic!("no element"),
                _ => {}
            }
        }
    }

    #[test]
    fn attributes_are_read_unchecked_and_at_most_max() {
        // Checked, the name given twice would be an error, and the
        // attributes after it would cost a comparison with each before.
        let twice = r#"<a x="1" x="2" y="3"/>"#;
        assert_eq!(first(twice, |e| attributes(e).count()), 3);
        assert_eq!(first(twice, |e| attr(e, "x")).as_deref(), Some("1"));
        let many: String = (0..MAX_ATTRIBUTES + 10)
            .map(|i| format!(" a{i}=\"{i}\""))
            .collect();
        let many = format!("<a{many}/>");
        assert_eq!(first(&many, |e| attributes(e).count()), MAX_ATTRIBUTES);
        assert_eq!(first(&many, |e| attr(e, "a0")).as_deref(), Some("0"));
        let past = format!("a{MAX_ATTRIBUTES}");
        assert_eq!(first(&many, |e| attr(e, &past)), None);
    }

    #[test]
    fn only_the_head_of_an_image_part_is_read_for_its_format() {
        let body = format!("GIF89a{}", "x".repeat(200));
        let bytes = package(&[("word/media/image1.gif", &body)]);
        let mut pkg = Package::open(&bytes).unwrap();
        let head = read_media_head(&mut pkg, "word/media/image1.gif").unwrap();
        assert_eq!(head, body.as_bytes()[..16]);
        // A part shorter than the head is read whole.
        let bytes = package(&[("word/media/image1.gif", "GIF8")]);
        let mut pkg = Package::open(&bytes).unwrap();
        assert_eq!(
            read_media_head(&mut pkg, "word/media/image1.gif").unwrap(),
            b"GIF8"
        );
        assert!(read_media_head(&mut pkg, "word/media/image2.png").is_err());
    }

    #[test]
    fn targets_resolve_against_their_parts_directory() {
        assert_eq!(
            resolve_part("ppt/slides", "../charts/chart1.xml"),
            "ppt/charts/chart1.xml"
        );
        assert_eq!(
            resolve_part("ppt", "/ppt/slides/slide3.xml"),
            "ppt/slides/slide3.xml"
        );
        assert_eq!(
            resolve_part("ppt", "./slides/slide3.xml"),
            "ppt/slides/slide3.xml"
        );
        assert_eq!(resolve_part("", "word/document.xml"), "word/document.xml");
        assert_eq!(
            rels_part("ppt/slides/slide3.xml"),
            "ppt/slides/_rels/slide3.xml.rels"
        );
        assert_eq!(rels_part(""), "_rels/.rels");
    }

    #[test]
    fn a_target_written_with_backslashes_resolves_as_with_slashes() {
        let rels = r#"<Relationships>
            <Relationship Id="a" Target="media\image1.png"/>
            <Relationship Id="b" Target="..\word\media\image2.png"/>
            <Relationship Id="c" Target="\word\media\image3.png"/>
        </Relationships>"#;
        let r = relationships(rels, "word/document.xml");
        let t: Vec<&str> = r.iter().map(|r| r.target.as_str()).collect();
        assert_eq!(
            t,
            [
                "word/media/image1.png",
                "word/media/image2.png",
                "word/media/image3.png"
            ]
        );
    }

    #[test]
    fn an_id_is_found_as_a_scan_in_order_found_it() {
        let rels = format!(
            "<Relationships><Relationship Id=\"a\" Target=\"https://example.org/\" TargetMode=\"External\"/>\
             <Relationship Id=\"a\" Type=\"{REL}/image\" Target=\"media/one.png\"/>\
             <Relationship Id=\"a\" Type=\"{REL}/image\" Target=\"media/two.png\"/>\
             <Relationship Id=\"b\" Type=\"{REL}/image\" Target=\"media/Three.png\"/></Relationships>"
        );
        let rels = relationships(&rels, "word/document.xml");
        let by_id = Relations::new(&rels);
        // The first of an id, whichever it is; its first within the package.
        assert!(by_id.get("a").unwrap().external);
        assert_eq!(by_id.part("a"), Some("word/media/one.png"));
        assert_eq!(by_id.part("c"), None);
        let bytes = package(&[("word/media/three.png", "")]);
        let names = PartNames::new(&Package::open(&bytes).unwrap());
        assert_eq!(
            names.resolve(&by_id, "a"),
            MediaSource::External("https://example.org/".to_string())
        );
        assert_eq!(
            names.resolve(&by_id, "b"),
            MediaSource::Part("word/media/three.png".to_string())
        );
        assert_eq!(
            names.resolve(&by_id, "c"),
            MediaSource::Missing("c".to_string())
        );
    }

    #[test]
    fn relationships_resolve_internal_targets_and_keep_external_ones() {
        let rels = format!(
            "<Relationships><Relationship Id=\"rId1\" Type=\"{REL}/chart\" Target=\"../charts/chart1.xml\"/>\
             <Relationship Id=\"rId2\" Type=\"{REL}/hyperlink\" Target=\"https://example.org/?a=1&amp;b=2\" TargetMode=\"External\"/></Relationships>"
        );
        let r = relationships(&rels, "ppt/slides/slide1.xml");
        assert_eq!(r[0].target, "ppt/charts/chart1.xml");
        assert!(r[0].is("chart") && !r[0].external);
        assert_eq!(r[1].target, "https://example.org/?a=1&b=2");
        assert!(r[1].external);
    }

    #[test]
    fn the_main_part_and_the_core_properties_are_where_the_package_says() {
        let bytes = package(&[
            (
                "_rels/.rels",
                &format!(
                    "<Relationships><Relationship Id=\"rId1\" Type=\"{REL}/officeDocument\" Target=\"word/document2.xml\"/>\
                     <Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"props/core.xml\"/></Relationships>"
                ),
            ),
            ("word/document2.xml", "<w:document/>"),
            ("props/core.xml", "<cp:coreProperties><dc:title>Moved</dc:title></cp:coreProperties>"),
        ]);
        let mut p = Package::open(&bytes).unwrap();
        assert_eq!(p.main_part("word/document.xml"), "word/document2.xml");
        assert_eq!(p.info().title.as_deref(), Some("Moved"));
        // A package that does not say is read where writers put it.
        let bare = package(&[(
            "docProps/core.xml",
            "<cp:coreProperties><dc:creator>Ada</dc:creator></cp:coreProperties>",
        )]);
        let mut p = Package::open(&bare).unwrap();
        assert_eq!(p.main_part("word/document.xml"), "word/document.xml");
        assert_eq!(p.info().creators, ["Ada"]);
        assert!(info(b"not a zip").is_empty());
    }

    /// A package of these parts, deflated, as a hostile one holds a part
    /// of gigabytes in a few kilobytes.
    fn deflated(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        for (name, body) in parts {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(body).unwrap();
        }
        z.finish().unwrap();
        buf.into_inner()
    }

    /// `head`, padded with spaces to one byte past `max`.
    fn past(head: &str, max: u64) -> Vec<u8> {
        let mut v = head.as_bytes().to_vec();
        v.resize(max as usize + 1, b' ');
        v
    }

    #[test]
    fn a_part_past_its_bound_is_refused_as_it_inflates() {
        let notes = "<w:notes/>";
        let bytes = package(&[("word/notes.xml", notes)]);
        let mut p = Package::open(&bytes).unwrap();
        assert!(p.read_capped("word/notes.xml", 9).is_err());
        assert_eq!(
            p.read_capped("word/notes.xml", 10).unwrap().as_deref(),
            Some(notes)
        );
        assert_eq!(p.read_capped("word/none.xml", 9), Ok(None));
        // A part's relationships and the core properties, each past the
        // bound: refused, and the package reads as one that does not say.
        let rels = format!(
            "<Relationships><Relationship Id=\"rId1\" Type=\"{REL}/styles\" Target=\"styles.xml\"/></Relationships>"
        );
        let bytes = deflated(&[
            ("word/_rels/document.xml.rels", &past(&rels, MAX_XML_BYTES)),
            (
                "docProps/core.xml",
                &past(
                    "<cp:coreProperties><dc:title>Unread</dc:title>",
                    MAX_XML_BYTES,
                ),
            ),
        ]);
        let mut p = Package::open(&bytes).unwrap();
        assert!(p.read_capped("docProps/core.xml", MAX_XML_BYTES).is_err());
        assert!(p.relationships("word/document.xml").is_err());
        assert!(p.related("word/document.xml", "styles", "x").is_err());
        assert!(p.info().is_empty());
        // Its size declared as a few bytes: no more than those are read.
        let mut bytes = deflated(&[("big.xml", &past("<a/>", 1 << 20))]);
        for (sig, at) in [(b"PK\x03\x04", 22), (b"PK\x01\x02", 24)] {
            let h = bytes.windows(4).position(|w| w == sig).unwrap();
            bytes[h + at..h + at + 4].copy_from_slice(&16u32.to_le_bytes());
        }
        let mut p = Package::open(&bytes).unwrap();
        let read = p.read_capped("big.xml", 1 << 20);
        assert!(read.is_err() || read.unwrap().is_none_or(|s| s.len() <= 16));
    }

    #[test]
    fn a_parts_declared_size_is_reserved_but_not_believed() {
        let text = b"<w:document/>";
        // Declared as a gigabyte: read, and holding no more than it is.
        let v = read_declared(&text[..], 1 << 30, 2 << 30).unwrap();
        assert_eq!(v, text);
        assert!(v.capacity() < 1 << 20, "{}", v.capacity());
        // Declared as more than memory can be found for: read all the same.
        let v = read_declared(&text[..], 1 << 62, 1 << 63).unwrap();
        assert_eq!(v, text);
        // More than the bound, whatever it declares: refused.
        assert_eq!(read_declared(&text[..], 4, 4), Err(Unread::TooLarge));
    }

    #[test]
    fn a_part_read_whole_is_read_within_the_budget_left_and_spent_from_it() {
        let body = "<w:document/>";
        let len = body.len() as u64;
        let bytes = deflated(&[("word/document.xml", &past(body, 4 * len))]);
        let mut p = Package::open(&bytes).unwrap();
        // Room for it: read, and spent.
        let mut b = Budget::new((4 * len + 1) * PART_BYTES + 7, 0);
        let read = p.read_within("word/document.xml", &mut b).unwrap();
        assert_eq!(read.map(|s| s.len() as u64), Some(4 * len + 1));
        assert_eq!(b.memory_left(), 7);
        // No room for it: refused as too large, not read as no part, and
        // the budget stays spent.
        let mut b = Budget::new((4 * len + 1) * PART_BYTES - 1, 0);
        let e = p.read_within("word/document.xml", &mut b).unwrap_err();
        assert!(e.to_string().starts_with("bytes of parts inflated"), "{e}");
        assert_eq!(b.memory_left(), 0);
        // A part the package does not hold costs nothing.
        let mut b = Budget::new(10, 0);
        assert_eq!(p.read_within("word/none.xml", &mut b), Ok(None));
        assert_eq!(b.memory_left(), 10);
        // Its size declared as a few bytes is not believed.
        let mut bytes = bytes;
        for (sig, at) in [(b"PK\x03\x04", 22), (b"PK\x01\x02", 24)] {
            let h = bytes.windows(4).position(|w| w == sig).unwrap();
            bytes[h + at..h + at + 4].copy_from_slice(&4u32.to_le_bytes());
        }
        let mut p = Package::open(&bytes).unwrap();
        let mut b = Budget::new(2 * len * PART_BYTES, 0);
        let read = p.read_within("word/document.xml", &mut b);
        assert!(read.is_err() || read.unwrap().is_none_or(|s| s.len() <= 4));
    }

    #[test]
    fn the_root_relationships_are_read_within_their_bound() {
        // A root relationships part past the bound, naming the main part
        // in its first bytes: refused, so it names none.
        let rels = format!(
            "<Relationships><Relationship Id=\"rId1\" Type=\"{REL}/officeDocument\" \
             Target=\"word/document.xml\"/></Relationships>"
        );
        let bytes = deflated(&[("_rels/.rels", &past(&rels, MAX_XML_BYTES))]);
        let mut p = Package::open(&bytes).unwrap();
        assert_eq!(p.root("officeDocument"), None);
        let bytes = package(&[("_rels/.rels", &rels)]);
        let mut p = Package::open(&bytes).unwrap();
        assert_eq!(
            p.root("officeDocument").as_deref(),
            Some("word/document.xml")
        );
    }

    #[test]
    fn core_properties_are_read_by_local_name() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
<dc:title>Q3 &amp; Q4 Plan &#8212; Draft</dc:title><dc:subject/><dc:creator>Ada Park</dc:creator>
<cp:lastModifiedBy>Kai Moreno</cp:lastModifiedBy>
<dcterms:created xsi:type="dcterms:W3CDTF">2026-07-17T13:48:00Z</dcterms:created>
<dcterms:modified xsi:type="dcterms:W3CDTF">sometime</dcterms:modified>
</cp:coreProperties>"#;
        let info = core_properties(xml);
        assert_eq!(info.title.as_deref(), Some("Q3 & Q4 Plan \u{2014} Draft"));
        assert_eq!(info.creators, ["Ada Park"]);
        assert_eq!(info.created.as_deref(), Some("2026-07-17T13:48:00Z"));
        assert_eq!(info.modified, None, "not a date, so not passed on as one");
        assert!(core_properties("<cp:coreProperties/>").is_empty());
    }

    #[test]
    fn a_core_property_is_all_of_its_text() {
        let info = core_properties(
            "<cp:coreProperties xmlns:cp=\"c\" xmlns:dc=\"d\">\
             <dc:title><![CDATA[R&D <draft>]]></dc:title>\
             <dc:creator>Ada<!-- the lead --> Park</dc:creator></cp:coreProperties>",
        );
        assert_eq!(info.title.as_deref(), Some("R&D <draft>"));
        assert_eq!(info.creators, ["Ada Park"]);
        // A part that stops parsing keeps what came before.
        let cut = core_properties("<cp:coreProperties><dc:title>Kept</dc:title><dc:creator>Ada");
        assert_eq!(cut.title.as_deref(), Some("Kept"));
    }

    #[test]
    fn an_attribute_is_its_unescaped_value() {
        let mut r = Reader::from_str("<sheet name=\"R&amp;D\"/>");
        let Ok(Event::Empty(e)) = r.read_event() else {
            panic!()
        };
        assert_eq!(attr(&e, "name").as_deref(), Some("R&D"));
    }
}
