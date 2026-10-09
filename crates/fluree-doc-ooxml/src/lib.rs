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

use fluree_doc_model::{xsd_date_time, DocumentInfo};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
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

impl<'a> Package<'a> {
    /// Open a package's bytes. The error is the archive's own message.
    pub fn open(bytes: &'a [u8]) -> Result<Self, String> {
        zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map(|zip| Package { zip })
            .map_err(|e| e.to_string())
    }

    /// A part's text, or `None` where the package has no such part or it is
    /// not UTF-8.
    pub fn read(&mut self, part: &str) -> Option<String> {
        let mut s = String::new();
        self.zip.by_name(part).ok()?.read_to_string(&mut s).ok()?;
        Some(s)
    }

    /// Every part's name, in archive order.
    pub fn part_names(&self) -> Vec<String> {
        self.zip.file_names().map(str::to_string).collect()
    }

    /// A part's relationships, each internal target resolved to a part name.
    /// Empty where the part has none.
    pub fn relationships(&mut self, part: &str) -> Vec<Relationship> {
        self.read(&rels_part(part))
            .map(|xml| relationships(&xml, part))
            .unwrap_or_default()
    }

    /// The package's own relationship of type `name` (`officeDocument`,
    /// `core-properties`), from its root relationships part.
    fn root(&mut self, name: &str) -> Option<String> {
        self.read("_rels/.rels")
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

    /// The part `source` relates to by type `name`, or `fallback`.
    pub fn related(&mut self, source: &str, name: &str, fallback: &str) -> String {
        self.relationships(source)
            .into_iter()
            .find(|r| r.is(name) && !r.external)
            .map(|r| r.target)
            .unwrap_or_else(|| fallback.to_string())
    }

    /// What the file declares about itself in its core properties: title,
    /// author, and when it was made and last saved.
    pub fn info(&mut self) -> DocumentInfo {
        let part = self
            .root("core-properties")
            .unwrap_or_else(|| "docProps/core.xml".to_string());
        self.read(&part)
            .map(|xml| core_properties(&xml))
            .unwrap_or_default()
    }
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
                        resolve_part(dir, &target)
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

/// An attribute's value by local name, its entities unescaped: a sheet
/// named `R&D` is written `name="R&amp;D"`, and is `R&D`.
pub fn attr(e: &BytesStart<'_>, want: &str) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
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
