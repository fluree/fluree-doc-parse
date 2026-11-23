//! DOCX (OOXML) to DoCO-typed document elements.
//!
//! Word declares outright what a PDF makes us infer, and the difference runs
//! the whole way through: `w:pStyle` names the heading level, `w:numPr` marks
//! a list item, `w:tbl` bounds a real table, and `w:gridSpan` / `w:vMerge`
//! state cell merges that the PDF engine has to read back out of ruling
//! geometry. So this reader measures nothing — it maps. The one thing it
//! computes is what Word computes at layout rather than storing: list
//! numbers, in the `numbering` module.
//!
//! Two consequences worth being explicit about:
//!
//! * **No geometry.** A `.docx` stores a flow, not a layout; page boundaries
//!   and coordinates only exist once something lays it out. Every element's
//!   `bbox` is `None` and `page` is 0, rather than a zeroed box that would
//!   read as a real position.
//! * **No escalation.** There is nothing for a model tier to arbitrate: the
//!   structure is not a hypothesis.

mod numbering;

use fluree_doc_model::{symbols, DocumentInfo, Element};
use fluree_doc_ooxml::{attr, local};
use numbering::Numbering;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub enum DocxError {
    Zip(String),
    Xml(String),
    NoDocument,
}

impl std::fmt::Display for DocxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zip(e) => write!(f, "not a readable .docx: {e}"),
            Self::Xml(e) => write!(f, "malformed document.xml: {e}"),
            Self::NoDocument => write!(f, "package has no main document part"),
        }
    }
}

impl std::error::Error for DocxError {}

/// Parse a `.docx` file's bytes into document elements in reading order.
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, DocxError> {
    parse_with_info(bytes).map(|(elements, _)| elements)
}

/// [`parse`], with what the file declares about itself in its core
/// properties, read from the one opening of the package.
///
/// The document part is the one the package names as its main part, and
/// its styles the part the document relates to as such; `word/document.xml`
/// and `word/styles.xml` are where a package that does not say keeps them.
pub fn parse_with_info(bytes: &[u8]) -> Result<(Vec<Element>, DocumentInfo), DocxError> {
    parse_with_media(bytes).map(|(elements, info, _)| (elements, info))
}

/// The opened package [`read_media`] and [`read_pictures`] read from.
pub use fluree_doc_ooxml::Package;

/// The images a document places in its flow and the loop that reads them,
/// shared with the other containers of pictures (a workbook, an e-mail):
/// each image read once however many figures draw it, within an allowance
/// per document.
pub use fluree_doc_model::{
    pictures, Allowance, Media, MediaSource, Picture, PictureReader, PictureReads, Unread,
};

/// The most bytes one image part is read to: 64 MiB, more than any picture
/// a document pastes in.
pub const MAX_MEDIA_BYTES: u64 = 64 << 20;

/// The bytes of a media part of an opened package, at most
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

/// What [`parse_with_ocr`] gives.
#[cfg(feature = "ocr")]
#[derive(Debug, Clone)]
pub struct OcrParse {
    pub elements: Vec<Element>,
    pub info: DocumentInfo,
    /// The pictures not read, each with why ([`Unread`]). Each is left as
    /// its figures.
    pub unread: Vec<(Picture, Unread)>,
    /// Images of more than one page (a multi-page TIFF), of which only the
    /// first was read.
    pub first_page_only: Vec<Picture>,
}

/// [`parse_with_info`], with the text in the document's pictures read by
/// `engine` and placed right after each figure that draws them
/// ([`fluree_doc_image::place_after_figures`]), each picture read once,
/// within [`Allowance::DOCUMENT`].
#[cfg(feature = "ocr")]
pub fn parse_with_ocr(
    bytes: &[u8],
    engine: &dyn fluree_doc_vision::OcrEngine,
) -> Result<OcrParse, DocxError> {
    struct Engine<'e>(&'e dyn fluree_doc_vision::OcrEngine);
    impl PictureReader for Engine<'_> {
        type Reading = fluree_doc_image::ImageElements;
        fn pixels(&mut self, image: &[u8]) -> Result<u64, String> {
            fluree_doc_image::header(image).map(|h| u64::from(h.width) * u64::from(h.height))
        }
        fn read(&mut self, _: &Picture, image: &[u8]) -> Result<Self::Reading, String> {
            fluree_doc_image::elements(image, self.0)
        }
    }
    let (mut elements, info, media) = parse_with_media(bytes)?;
    let mut pkg = Package::open(bytes).map_err(DocxError::Zip)?;
    let reads = read_pictures(&mut pkg, media, &mut Engine(engine), Allowance::DOCUMENT);
    let mut placed = Vec::new();
    let mut first_page_only = Vec::new();
    for (picture, r) in reads.read {
        placed.extend(picture.after_each_figure(r.elements));
        if r.more_pages {
            first_page_only.push(picture);
        }
    }
    fluree_doc_image::place_after_figures(&mut elements, placed);
    Ok(OcrParse {
        elements,
        info,
        unread: reads.unread,
        first_page_only,
    })
}

/// [`parse_with_info`], with the images of each figure in the body that has
/// any, in reading order: what an OCR engine or a model can read, and the
/// figure its text belongs after. Only where they are is read here; their
/// bytes are read by [`read_pictures`], each image once, or one at a time
/// by [`read_media`].
pub fn parse_with_media(
    bytes: &[u8],
) -> Result<(Vec<Element>, DocumentInfo, Vec<Media>), DocxError> {
    let mut pkg = Package::open(bytes).map_err(DocxError::Zip)?;
    let main = pkg.main_part("word/document.xml");
    let xml = pkg.read(&main).ok_or(DocxError::NoDocument)?;
    // Styles refine what a table's first row is and which paragraphs are
    // the title; a package without them, or with ones that do not parse,
    // still reads.
    let styles = pkg.related(&main, "styles", "word/styles.xml");
    let styles = pkg.read(&styles);
    let (header_styles, title_styles) = match &styles {
        Some(styles) => (header_row_styles(styles), title_styles(styles)),
        None => (HashSet::new(), HashSet::new()),
    };
    // List numbering, which Word computes rather than stores; without the
    // part the document numbers nothing.
    let numbering = pkg.related(&main, "numbering", "word/numbering.xml");
    let numbering = Numbering::new(pkg.read(&numbering).as_deref(), styles.as_deref());
    let (body, figure_rids) = parse_body(&xml, &header_styles, &title_styles, numbering)?;
    // The page header and footer: where a controlled document keeps its
    // number, revision and title, which its body never repeats. Each is
    // read once, the header before the body and the footer after it, and
    // marked as what it is.
    let settings = pkg.related(&main, "settings", "word/settings.xml");
    let even_odd = pkg
        .read(&settings)
        .is_some_and(|x| toggled_on(&x, "evenAndOddHeaders"));
    let rels = pkg.relationships(&main);
    let mut read_parts = |kind: &str, evidence: &'static str| -> Vec<Element> {
        let mut out: Vec<Element> = Vec::new();
        for rid in shown_parts(&xml, kind, even_odd) {
            let Some(part) = rels.iter().find(|r| r.id == rid && !r.external) else {
                continue;
            };
            let Some(part_xml) = pkg.read(&part.target) else {
                continue;
            };
            let Ok(found) = parse_with_styles(
                &part_xml,
                &header_styles,
                &title_styles,
                Numbering::default(),
            ) else {
                continue;
            };
            for mut e in found {
                // A header's picture is its logo.
                if e.kind == "doco:Figure" {
                    continue;
                }
                // A heading set in the header is the page's furniture, not
                // a section of the document.
                if e.kind != "doco:Table" {
                    e.kind = "doco:Paragraph".into();
                    e.level = None;
                }
                e.evidence = evidence;
                // The first page's header and the rest's often share lines.
                if !out.iter().any(|o| o.kind == e.kind && o.text == e.text) {
                    out.push(e);
                }
            }
        }
        out
    };
    let header = read_parts("header", "header");
    let footer = read_parts("footer", "footer");
    let mut elements = header;
    elements.extend(body);
    elements.extend(footer);
    for (i, e) in elements.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    // Each figure's images, by the relationships its drawing names. A part
    // name is matched as the package format matches it, ignoring case, and
    // a target written with backslashes, as some writers do, is read with
    // slashes. An image that resolves to nothing in the package (a link to a
    // file outside it, a relationship not defined) is still listed, so it
    // is counted among those not read; a part that is not there is listed
    // under the name it was given, and fails to read.
    let names = pkg.part_names();
    let exact: HashSet<&str> = names.iter().map(String::as_str).collect();
    let folded: HashMap<String, &str> = names
        .iter()
        .map(|p| (p.to_lowercase(), p.as_str()))
        .collect();
    // By id, so a document naming many images resolves each in one lookup;
    // the first definition of an id wins, as a scan would have it.
    let mut by_id: HashMap<&str, &fluree_doc_ooxml::Relationship> = HashMap::new();
    for r in &rels {
        by_id.entry(r.id.as_str()).or_insert(r);
    }
    let media = figure_rids
        .into_iter()
        .map(|(figure, rid, index)| {
            let source = match by_id.get(rid.as_str()) {
                Some(r) if !r.external => MediaSource::Part(if exact.contains(r.target.as_str()) {
                    r.target.clone()
                } else {
                    folded
                        .get(&r.target.to_lowercase())
                        .map_or_else(|| r.target.clone(), |p| p.to_string())
                }),
                Some(r) => MediaSource::External(r.target.clone()),
                None => MediaSource::Missing(rid),
            };
            Media {
                figure,
                source,
                index,
            }
        })
        .collect();
    Ok((elements, pkg.info(), media))
}

/// A toggle element in a part, present and not switched off.
fn toggled_on(xml: &str, name: &str) -> bool {
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    loop {
        match r.read_event_into(&mut buf) {
            Err(_) | Ok(Event::Eof) => return false,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(e.name().as_ref()) == name => {
                return !is_off(&e);
            }
            _ => {}
        }
        buf.clear();
    }
}

/// The relationship ids of the headers (`kind` `header`) or footers Word
/// shows, in the order the document's sections name them: each section's
/// default one, its first page's where the section has a title page, and
/// its even pages' where the document sets odd and even apart. A part
/// a section names but Word does not show is left out.
fn shown_parts(document_xml: &str, kind: &str, even_odd: bool) -> Vec<String> {
    let mut r = Reader::from_str(document_xml);
    let mut buf = Vec::new();
    let want = format!("{kind}Reference");
    let mut out: Vec<String> = Vec::new();
    // The section being read: its references by type, and its title page.
    let mut refs: Vec<(String, String)> = Vec::new();
    let mut title_page = false;
    let mut in_sect = false;
    let flush = |refs: &mut Vec<(String, String)>, title_page: bool, out: &mut Vec<String>| {
        for want_type in ["first", "default", "even"] {
            let shown = match want_type {
                "first" => title_page,
                "even" => even_odd,
                _ => true,
            };
            if !shown {
                continue;
            }
            for (t, id) in refs.iter() {
                if t == want_type && !out.contains(id) {
                    out.push(id.clone());
                }
            }
        }
        refs.clear();
    };
    loop {
        match r.read_event_into(&mut buf) {
            Err(_) | Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                "sectPr" => {
                    in_sect = true;
                    title_page = false;
                    refs.clear();
                }
                "titlePg" if in_sect => title_page = !is_off(&e),
                n if in_sect && n == want => {
                    let t = attr(&e, "type").unwrap_or_else(|| "default".into());
                    if let Some(id) = attr(&e, "id") {
                        refs.push((t, id));
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) if local(e.name().as_ref()) == "sectPr" => {
                in_sect = false;
                flush(&mut refs, title_page, &mut out);
            }
            _ => {}
        }
        buf.clear();
    }
    out
}

/// The paragraph styles that are Word's built-in Title.
///
/// A localised Word writes its own id for the style (`Titel`) but keeps the
/// built-in name, `Title`, so the name is what says which style it is.
fn title_styles(xml: &str) -> HashSet<String> {
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut out = HashSet::new();
    let mut style: Option<String> = None;
    loop {
        match r.read_event_into(&mut buf) {
            Err(_) | Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                "style" => {
                    style = (attr(&e, "type").as_deref() == Some("paragraph"))
                        .then(|| attr(&e, "styleId"))
                        .flatten();
                }
                "name" if attr(&e, "val").is_some_and(|v| v.eq_ignore_ascii_case("title")) => {
                    out.extend(style.clone());
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    out
}

/// The table styles that set a table's first row apart — bold or shaded
/// through a `firstRow` conditional format — directly or through the style
/// they are based on.
///
/// Word applies that formatting only where the table's `w:tblLook` turns
/// the first-row condition on. python-docx turns it on for every table it
/// writes, so the look alone says nothing; what says the first row is a
/// header is a style that draws it as one. `Table Grid`, the style most
/// generated tables use, has no conditional formats at all.
fn header_row_styles(xml: &str) -> HashSet<String> {
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut marked: HashSet<String> = HashSet::new();
    let mut based_on: HashMap<String, String> = HashMap::new();
    let mut style: Option<String> = None;
    let mut in_first_row = false;
    loop {
        match r.read_event_into(&mut buf) {
            Err(_) | Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.name();
                match local(name.as_ref()) {
                    "style" => {
                        style = (attr(&e, "type").as_deref() == Some("table"))
                            .then(|| attr(&e, "styleId"))
                            .flatten();
                    }
                    "basedOn" => {
                        if let (Some(s), Some(v)) = (&style, attr(&e, "val")) {
                            based_on.insert(s.clone(), v);
                        }
                    }
                    "tblStylePr" => {
                        in_first_row = attr(&e, "type").as_deref() == Some("firstRow");
                    }
                    "b" if in_first_row && !is_off(&e) => {
                        marked.extend(style.clone());
                    }
                    "shd" if in_first_row => {
                        let fill = attr(&e, "fill").unwrap_or_default().to_ascii_lowercase();
                        if !fill.is_empty() && fill != "auto" && fill != "ffffff" {
                            marked.extend(style.clone());
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                "style" => style = None,
                "tblStylePr" => in_first_row = false,
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    // A style inherits its parent's first-row format. The walk is bounded
    // so a cycle in a damaged file cannot hang it.
    let ids: Vec<String> = based_on.keys().cloned().collect();
    for id in ids {
        let mut at = id.clone();
        for _ in 0..16 {
            if marked.contains(&at) {
                marked.insert(id.clone());
                break;
            }
            match based_on.get(&at) {
                Some(parent) => at = parent.clone(),
                None => break,
            }
        }
    }
    marked
}

/// A toggle property (`w:b`, `w:tblHeader`) switched off by its value.
fn is_off(e: &quick_xml::events::BytesStart<'_>) -> bool {
    attr(e, "val").is_some_and(|v| matches!(v.as_str(), "0" | "false" | "off"))
}

/// A `w:pStyle` value as a heading level, if it names one.
///
/// Word's built-in styles are `Heading1`..`Heading9`; localised templates and
/// hand-built ones vary, so the digit suffix is what carries the meaning.
fn heading_level(style: &str) -> Option<usize> {
    let s = style.trim();
    let rest = s
        .strip_prefix("Heading")
        .or_else(|| s.strip_prefix("heading"))
        .or_else(|| s.strip_prefix("berschrift"))?; // German "Überschrift"
    rest.trim_start_matches('-')
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=9).contains(n))
        .map(|n| n.min(6))
}

/// A picture, chart or embedded object, as the document describes it.
#[derive(Default)]
struct Pic {
    /// Its alternative text, written for a reader who cannot see it.
    alt: String,
    title: String,
    /// It holds an image or a chart, not only a shape or a text box.
    image: bool,
    /// It is an object embedded from another program (`w:object`).
    object: bool,
    /// The relationships naming its image parts (`a:blip r:embed`, or VML's
    /// `v:imagedata r:id`), in order: one for a picture, several for a group
    /// of them.
    embed: Vec<String>,
    /// The same ids, to name each once in one lookup: a picture may name
    /// tens of thousands.
    named: HashSet<String>,
}

impl Pic {
    /// Name an image of it, once however often it is named.
    fn name_image(&mut self, rid: String) {
        if self.named.insert(rid.clone()) {
            self.embed.push(rid);
        }
    }

    /// What the figure says in words: its alternative text, or its title,
    /// without the notice Word appends to text it wrote itself.
    fn text(&self) -> String {
        let alt = if self.alt.trim().is_empty() {
            &self.title
        } else {
            &self.alt
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
}

/// Whether an element was read as an opening tag or a self-closing one.
#[derive(Clone, Copy)]
enum Kind {
    Start,
    Empty,
}

#[derive(Default)]
struct Cell {
    text: String,
    /// `w:gridSpan` — how many grid columns this cell occupies.
    span: usize,
    /// `w:vMerge` with no `val`, i.e. a continuation of the cell above.
    v_continue: bool,
    /// Some of the cell's text is set in a bold run, and some is not.
    bold_text: bool,
    plain_text: bool,
}

impl Cell {
    /// All of the cell's text is bold — and it has some.
    fn bold(&self) -> bool {
        self.bold_text && !self.plain_text
    }
}

#[derive(Default)]
struct Row {
    cells: Vec<Cell>,
    /// `w:tblHeader`: Word repeats this row atop every page the table
    /// runs onto, which is what a header row is.
    repeats: bool,
}

/// What a table's own properties say about its first row: its style draws
/// it as a header and its `w:tblLook` turns that formatting on.
#[derive(Default, Clone)]
struct TableLook {
    style: Option<String>,
    first_row: bool,
}

#[derive(Default)]
struct Para {
    text: String,
    style: String,
    /// `w:numPr` on the paragraph itself, with the list and level it names
    /// (either may be left to the style).
    numbered: bool,
    num_id: Option<String>,
    ilvl: Option<usize>,
    /// The number Word shows at the head of the paragraph, with what
    /// follows it: `9.1.5. `.
    label: Option<String>,
    /// The list level that number is counted at, from 0.
    level: Option<usize>,
    /// The list it is counted in.
    list: Option<String>,
}

/// What the file declares about itself in its core properties: title,
/// author, and when it was made and last saved. Empty where there are none
/// or the file is not a package.
pub fn info(bytes: &[u8]) -> DocumentInfo {
    fluree_doc_ooxml::info(bytes)
}

pub fn parse_document_xml(xml: &str) -> Result<Vec<Element>, DocxError> {
    parse_with_styles(xml, &HashSet::new(), &HashSet::new(), Numbering::default())
}

/// [`parse_document_xml`], knowing which table styles set their first row
/// apart as a header (see [`header_row_styles`]) and which paragraph styles
/// are the document's title (see [`title_styles`]), and numbering each
/// numbered paragraph by `numbering`.
fn parse_with_styles(
    xml: &str,
    header_styles: &HashSet<String>,
    title_styles: &HashSet<String>,
    numbering: Numbering,
) -> Result<Vec<Element>, DocxError> {
    parse_body(xml, header_styles, title_styles, numbering).map(|(elements, _)| elements)
}

/// A figure's id (`Element::figure`) and the relationship id naming its
/// image part, and its place among the figure's images.
type FigureImage = (String, String, usize);

/// [`parse_with_styles`], with each figure's image relationship, in reading
/// order.
fn parse_body(
    xml: &str,
    header_styles: &HashSet<String>,
    title_styles: &HashSet<String>,
    mut numbering: Numbering,
) -> Result<(Vec<Element>, Vec<FigureImage>), DocxError> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);

    let mut out: Vec<Element> = Vec::new();
    let mut buf = Vec::new();

    let mut para = Para::default();
    let mut in_text = false;
    // Table state. `depth` tracks nesting so an inner table's rows are not
    // stolen by the outer one; nested tables are flattened in reading order,
    // which is what a flat cell grid can express.
    let mut table_depth = 0usize;
    let mut rows: Vec<Row> = Vec::new();
    let mut row = Row::default();
    let mut cell = Cell::default();
    let mut in_cell = false;
    let mut look = TableLook::default();
    // Run formatting: whether the run being read is bold. Only a run's own
    // `w:rPr` counts; the one inside `w:pPr` formats the paragraph mark.
    let mut in_ppr = false;
    let mut run_bold = false;
    // The font a run sets for itself, where it is a symbol font whose text
    // is written in the font's own codes.
    let mut run_font: Option<String> = None;
    // A tracked change keeps the paragraph's properties as they were, inside
    // `w:pPrChange`: its style and numbering are not the paragraph's.
    let mut in_ppr_change = false;
    // Where the paragraph being read starts in its cell's text, so a
    // numbered one's label goes at its head.
    let mut cell_para_start = 0usize;
    // Fields being read: each one's instruction, and whether its result
    // (the text Word last computed for it) is being read. A page number's
    // result is the page Word last laid out, `Page 3 of 7` whatever page
    // document, and is no part of the text.
    let mut fields: Vec<(String, bool)> = Vec::new();
    let mut in_instr = false;
    // Inside `mc:Fallback`: what Word reads in place of content it cannot,
    // a second copy of what `mc:Choice` already holds.
    let mut fallback = 0usize;
    // A picture, chart or embedded object being read, and the figures read
    // in the current paragraph or table, which follow it.
    let mut pic_depth = 0usize;
    let mut pic = Pic::default();
    // Inside a picture's `pic:pic`, the one place its image is: a shape's
    // image fill (`a:blipFill` in its properties) is a texture, not a
    // picture.
    let mut in_pic = 0usize;
    // Inside a tracked deletion (`w:del`, `w:moveFrom`): what the document
    // no longer holds, and a picture there is none of its content.
    let mut deleted = 0usize;
    // Drawings opened inside a deletion, which are no figure: their ends
    // close nothing.
    let mut skipped = 0usize;
    let mut figures: Vec<Element> = Vec::new();
    let mut figure_count = 0usize;
    let mut media: Vec<FigureImage> = Vec::new();
    // Each list item read, by its place in `out`: the level its number is
    // counted at and whether it reads as a title, where it is numbered.
    let mut outline: Vec<(usize, Option<Numbered>)> = Vec::new();
    let page_field = |instr: &str| {
        let i = instr.trim_start().to_ascii_uppercase();
        ["PAGE", "NUMPAGES", "SECTIONPAGES"]
            .iter()
            .any(|f| i.split_whitespace().next() == Some(f))
    };

    loop {
        let event = r.read_event_into(&mut buf);
        if fallback > 0 {
            match &event {
                Ok(Event::Start(e)) if local(e.name().as_ref()) == "Fallback" => fallback += 1,
                Ok(Event::End(e)) if local(e.name().as_ref()) == "Fallback" => fallback -= 1,
                Ok(Event::Eof) | Err(_) => {}
                _ => {
                    buf.clear();
                    continue;
                }
            }
            if fallback > 0 {
                buf.clear();
                continue;
            }
        }
        match event {
            Err(e) => return Err(DocxError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) if local(e.name().as_ref()) == "Fallback" => fallback = 1,
            Ok(ev @ (Event::Start(_) | Event::Empty(_))) => {
                let (e, ev_kind) = match ev {
                    Event::Start(e) => (e, Kind::Start),
                    Event::Empty(e) => (e, Kind::Empty),
                    _ => unreachable!(),
                };
                let name = e.name();
                let tag = local(name.as_ref());
                match tag {
                    "t" => in_text = true,
                    "instrText" => in_instr = true,
                    // A picture or chart (DrawingML), an image (VML), or an
                    // object embedded from another program.
                    "del" | "moveFrom" if matches!(ev_kind, Kind::Start) => deleted += 1,
                    "drawing" | "pict" | "object"
                        if matches!(ev_kind, Kind::Start) && deleted > 0 =>
                    {
                        skipped += 1;
                    }
                    "drawing" | "pict" | "object" if matches!(ev_kind, Kind::Start) => {
                        if pic_depth == 0 {
                            pic = Pic::default();
                        }
                        pic.object |= tag == "object";
                        pic_depth += 1;
                    }
                    "docPr" if pic_depth > 0 && deleted == 0 => {
                        pic.alt = attr(&e, "descr").unwrap_or_default();
                        pic.title = attr(&e, "title").unwrap_or_default();
                    }
                    "pic" | "chart" | "relIds" if pic_depth > 0 && deleted == 0 => {
                        pic.image = true;
                        if tag == "pic" && matches!(ev_kind, Kind::Start) {
                            in_pic += 1;
                        }
                    }
                    "blip" if pic_depth > 0 && in_pic > 0 && deleted == 0 => {
                        // An image linked to a file outside the package
                        // names it by `r:link`: listed, so it is counted
                        // among those not read.
                        if let Some(rid) = attr(&e, "embed").or_else(|| attr(&e, "link")) {
                            pic.name_image(rid);
                        }
                    }
                    "imagedata" if pic_depth > 0 && deleted == 0 => {
                        pic.image = true;
                        if let Some(rid) = attr(&e, "id") {
                            pic.name_image(rid);
                        }
                        if pic.title.is_empty() {
                            pic.title = attr(&e, "title").unwrap_or_default();
                        }
                    }
                    "shape" if pic_depth > 0 && deleted == 0 && pic.alt.is_empty() => {
                        pic.alt = attr(&e, "alt").unwrap_or_default();
                    }
                    "fldChar" => match attr(&e, "fldCharType").as_deref() {
                        Some("begin") => fields.push((String::new(), false)),
                        Some("separate") => {
                            if let Some(f) = fields.last_mut() {
                                f.1 = true;
                            }
                        }
                        Some("end") => {
                            fields.pop();
                        }
                        _ => {}
                    },
                    // A simple field holds its result as its runs; an
                    // empty one has none to read.
                    "fldSimple" if matches!(ev_kind, Kind::Start) => {
                        fields.push((attr(&e, "instr").unwrap_or_default(), true));
                    }
                    "p" => {
                        // A paragraph's properties are its own: a style or a
                        // list read in a cell before it does not carry on.
                        para.style.clear();
                        para.numbered = false;
                        para.num_id = None;
                        para.ilvl = None;
                        para.label = None;
                        para.level = None;
                        para.list = None;
                        if in_cell {
                            cell_para_start = cell.text.len();
                        }
                    }
                    "pPrChange" => in_ppr_change = true,
                    _ if in_ppr_change => {}
                    "pPr" => in_ppr = true,
                    "r" => {
                        run_bold = false;
                        run_font = None;
                    }
                    "rFonts" if !in_ppr => {
                        run_font = attr(&e, "ascii")
                            .or_else(|| attr(&e, "hAnsi"))
                            .filter(|f| symbols::is_symbol_font(f));
                    }
                    // A character inserted from a symbol font, stored as
                    // the font's code for it.
                    "sym" => {
                        let ch = attr(&e, "char")
                            .and_then(|c| u32::from_str_radix(&c, 16).ok())
                            .and_then(|c| {
                                symbols::char_for(&attr(&e, "font").unwrap_or_default(), c)
                            });
                        if let Some(ch) = ch {
                            if in_cell {
                                if run_bold {
                                    cell.bold_text = true;
                                } else {
                                    cell.plain_text = true;
                                }
                            }
                            push(&mut para, &mut cell, in_cell, &ch.to_string());
                        }
                    }
                    "b" if !in_ppr => run_bold = !is_off(&e),
                    "rStyle" if !in_ppr => {
                        if attr(&e, "val").is_some_and(|v| v == "Strong") {
                            run_bold = true;
                        }
                    }
                    "tblHeader" => row.repeats = !is_off(&e),
                    "tblStyle" if table_depth == 1 => look.style = attr(&e, "val"),
                    "tblLook" if table_depth == 1 => {
                        // Either the explicit attribute or, in older files,
                        // the 0x0020 bit of the hex bitmask.
                        look.first_row = match attr(&e, "firstRow") {
                            Some(v) => matches!(v.as_str(), "1" | "true" | "on"),
                            None => attr(&e, "val")
                                .and_then(|v| u32::from_str_radix(&v, 16).ok())
                                .is_some_and(|v| v & 0x0020 != 0),
                        };
                    }
                    // A positional tab, which headers align their parts
                    // with, sets text apart as a tab does.
                    "tab" | "ptab" => push(&mut para, &mut cell, in_cell, "\t"),
                    // A line break is a line's end; a cell's lines are
                    // read as one value.
                    "br" | "cr" => push(
                        &mut para,
                        &mut cell,
                        in_cell,
                        if in_cell { " " } else { "\n" },
                    ),
                    "pStyle" => {
                        if let Some(v) = attr(&e, "val") {
                            para.style = v;
                        }
                    }
                    "numPr" if in_ppr => para.numbered = true,
                    "numId" if in_ppr => para.num_id = attr(&e, "val"),
                    "ilvl" if in_ppr => para.ilvl = attr(&e, "val").and_then(|v| v.parse().ok()),
                    "gridSpan" => {
                        if let Some(v) = attr(&e, "val") {
                            cell.span = v.parse().unwrap_or(1);
                        }
                    }
                    "vMerge" => {
                        // `val="restart"` begins a merge; absent means continue.
                        cell.v_continue = !matches!(attr(&e, "val").as_deref(), Some("restart"));
                    }
                    "tbl" => {
                        table_depth += 1;
                        if table_depth == 1 {
                            rows.clear();
                            look = TableLook::default();
                        }
                    }
                    "tc" => {
                        in_cell = true;
                        cell = Cell {
                            span: 1,
                            ..Default::default()
                        };
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if in_instr {
                    if let Some(f) = fields.last_mut() {
                        f.0.push_str(&t.unescape().unwrap_or_default());
                    }
                }
                let page_number = fields
                    .iter()
                    .any(|(instr, result)| *result && page_field(instr));
                if in_text && !page_number {
                    let mut s = t.unescape().unwrap_or_default().to_string();
                    // Text in a symbol font is the font's codes: `m` in
                    // Symbol shows `μ`. A code the font maps to nothing
                    // a reader knows is dropped, as Word's own text export
                    // drops it, rather than shown as the letter it is not.
                    if let Some(font) = &run_font {
                        s = s
                            .chars()
                            .filter_map(|c| {
                                if c.is_whitespace() {
                                    Some(c)
                                } else {
                                    symbols::char_for(font, c as u32)
                                }
                            })
                            .collect();
                    }
                    if in_cell && !s.trim().is_empty() {
                        if run_bold {
                            cell.bold_text = true;
                        } else {
                            cell.plain_text = true;
                        }
                    }
                    push(&mut para, &mut cell, in_cell, &s);
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                match local(name.as_ref()) {
                    "t" => in_text = false,
                    "instrText" => in_instr = false,
                    "del" | "moveFrom" if deleted > 0 => deleted -= 1,
                    "pic" if in_pic > 0 && deleted == 0 => in_pic -= 1,
                    "drawing" | "pict" | "object" if skipped > 0 => skipped -= 1,
                    "drawing" | "pict" | "object" if pic_depth > 0 => {
                        pic_depth -= 1;
                        if pic_depth == 0 && (pic.image || pic.object) {
                            figure_count += 1;
                            let mut f = element("doco:Figure", pic.text(), None);
                            let id = format!("figure-0-{figure_count}");
                            for (k, rid) in std::mem::take(&mut pic.embed).into_iter().enumerate() {
                                media.push((id.clone(), rid, k));
                            }
                            f.figure = Some(id);
                            figures.push(f);
                        }
                    }
                    "fldSimple" => {
                        fields.pop();
                    }
                    "pPrChange" => in_ppr_change = false,
                    _ if in_ppr_change => {}
                    "pPr" => {
                        in_ppr = false;
                        // The paragraph's list and level are known once its
                        // properties are; its number is counted here, shown
                        // or not, as Word counts it.
                        let style = (!para.style.is_empty()).then_some(para.style.as_str());
                        if let Some((id, level)) =
                            numbering.resolve(para.num_id.as_deref(), para.ilvl, style)
                        {
                            para.label = numbering.next(&id, level);
                            para.level = para.label.is_some().then_some(level);
                            para.list = numbering.list_of(&id).map(str::to_string);
                        }
                    }
                    "p" => {
                        if in_cell {
                            // A numbered paragraph's label heads its text in
                            // the cell. Word shows it on an empty paragraph
                            // too, and a step column's cells hold nothing
                            // else: `1.`, `2.` beside each step.
                            if let Some(label) = para.label.take() {
                                let start = cell_para_start.min(cell.text.len());
                                cell.text.insert_str(start, &label);
                            }
                            // Paragraphs inside a cell are one cell's content.
                            if !cell.text.is_empty() && !cell.text.ends_with(' ') {
                                cell.text.push(' ');
                            }
                        } else {
                            let at = out.len();
                            let (level, label) = (para.level, para.label.clone());
                            let list = para.list.clone().unwrap_or_default();
                            flush_para(&mut out, &mut para, title_styles);
                            // Whether it reads as a title is a question about
                            // the element it became: a paragraph's lines after
                            // a blank one are paragraphs of their own.
                            let numbered_at = level.zip(out.get(at)).map(|(level, e)| {
                                let label = label.as_deref().unwrap_or("").trim_end();
                                let text = e.text.strip_prefix(label).unwrap_or(&e.text);
                                Numbered {
                                    level,
                                    list,
                                    titled: titled(text),
                                    capitals: titled(text) && !text.chars().any(char::is_lowercase),
                                }
                            });
                            // Numbered on the paragraph or by its style, a
                            // list item or not.
                            if out.len() > at
                                && matches!(
                                    out[at].kind.as_str(),
                                    "doc:ListItem" | "doco:Paragraph"
                                )
                            {
                                outline.push((at, numbered_at));
                            }
                            // The paragraph's pictures follow its text.
                            if table_depth == 0 {
                                out.append(&mut figures);
                            }
                        }
                    }
                    "tc" => {
                        in_cell = false;
                        row.cells.push(std::mem::take(&mut cell));
                    }
                    "tr" => rows.push(std::mem::take(&mut row)),
                    "tbl" => {
                        table_depth = table_depth.saturating_sub(1);
                        if table_depth == 0 {
                            let styled = look.first_row
                                && look
                                    .style
                                    .as_ref()
                                    .is_some_and(|s| header_styles.contains(s));
                            emit_table(&mut out, std::mem::take(&mut rows), styled);
                            // A picture in a cell has no place in a grid of
                            // text; it follows the table.
                            out.append(&mut figures);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }
    flush_para(&mut out, &mut para, title_styles);
    out.append(&mut figures);
    promote_numbered_titles(&mut out, &outline);
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    Ok((out, media))
}

fn push(para: &mut Para, cell: &mut Cell, in_cell: bool, s: &str) {
    if in_cell {
        cell.text.push_str(s);
    } else {
        para.text.push_str(s);
    }
}

fn element(kind: &str, text: String, level: Option<usize>) -> Element {
    Element {
        level,
        ..Element::new(kind, text, "docx", "docx")
    }
}

/// Word marks a list two ways and uses both: `w:numPr` attaches real
/// numbering, while the built-in `List Bullet` / `List Number` /
/// `List Paragraph` styles carry list formatting with no numbering
/// properties at all. Reading only `numPr` leaves those as paragraphs.
fn is_list_style(style: &str) -> bool {
    let s = style.trim().to_ascii_lowercase().replace(['-', ' '], "");
    s.starts_with("list") || s.starts_with("bullet")
}

/// Longest a numbered title runs, in words.
const MAX_TITLE_WORDS: usize = 12;

/// The words a title leaves in lowercase: articles, conjunctions and
/// prepositions. Any other word set in lowercase is a sentence's.
const MINOR_WORDS: &[&str] = &[
    "a", "about", "after", "against", "among", "an", "and", "as", "at", "before", "between", "but",
    "by", "during", "for", "from", "in", "into", "nor", "of", "on", "or", "over", "per", "than",
    "the", "through", "to", "under", "until", "upon", "versus", "via", "vs", "with", "within",
    "without",
];

/// Whether a numbered paragraph's text reads as a title: short, not a
/// sentence, and set in capitals or in title case. `SCOPE`,
/// `PUMP CALIBRATION:`, `SAMPLE HANDLING and STORAGE`,
/// `Storage Conditions and Limits`, `REVIEWS - General Requirements`.
/// A parenthesis is an aside and may say anything:
/// `Report Approval and Filing (see form 5)`. A word counts as capitalized
/// if it has a capital anywhere, as `100 mL` and `pH` do.
fn titled(text: &str) -> bool {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let letters = t.chars().filter(|c| c.is_alphabetic()).count();
    if letters < 2 || t.split_whitespace().count() > MAX_TITLE_WORDS || t.ends_with(['.', ';', ','])
    {
        return false;
    }
    let mut depth = 0usize;
    let mut first = true;
    for w in t.split_whitespace() {
        let aside = depth > 0 || w.starts_with('(');
        depth += w.matches('(').count();
        depth = depth.saturating_sub(w.matches(')').count());
        if aside || !w.chars().any(char::is_alphabetic) {
            continue;
        }
        let capital = w.chars().any(char::is_uppercase);
        let bare = w
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        if !capital && (first || !MINOR_WORDS.contains(&bare.as_str())) {
            return false;
        }
        first = false;
    }
    !first
}

/// Read the outline a document keeps in its numbering.
///
/// A procedure numbered `1. SCOPE`, `4.2. SAMPLE HANDLING`,
/// `4.2.3. Label each sample` often uses no heading style at all: its
/// titles are list items like its steps, set apart only by being short and
/// in capitals, and its sections exist only in the numbers. A numbered item
/// that reads as a title and heads something — deeper-numbered items after
/// it, or text before its next sibling — is a section title, at the level
/// its number is counted at. One that heads nothing is a step written in
/// capitals for emphasis (`DO NOT PROCEED`), and stays one.
///
/// Capitals are a title's own mark. Title case is weaker: a list of
/// deliverables or of people is in title case too, and one of its entries
/// with a note under it heads something. So an item in title case is a
/// title only among siblings that are mostly titles heading something —
/// the same list, at the same level, under the same parent — which is what
/// an outline's sections are and a list's entries are not.
fn promote_numbered_titles(out: &mut [Element], outline: &[(usize, Option<Numbered>)]) {
    let numbered: Vec<(usize, &Numbered)> = outline
        .iter()
        .filter_map(|(i, n)| n.as_ref().map(|n| (*i, n)))
        .collect();
    // Levels are counted per list: the next item of another list is no
    // deeper than this one whatever its level.
    let heads: Vec<bool> = numbered
        .iter()
        .enumerate()
        .map(|(k, &(i, n))| match numbered.get(k + 1) {
            Some(&(j, next)) => (next.list == n.list && next.level > n.level) || j > i + 1,
            None => i + 1 < out.len(),
        })
        .collect();
    // Each item's siblings, as a run of the items in its list at its level,
    // broken where the list goes back to a shallower one.
    let mut group = vec![0usize; numbered.len()];
    let mut open: HashMap<(&str, usize), usize> = HashMap::new();
    let mut groups = 0usize;
    for (k, &(_, n)) in numbered.iter().enumerate() {
        open.retain(|&(list, level), _| list != n.list || level <= n.level);
        group[k] = *open.entry((n.list.as_str(), n.level)).or_insert_with(|| {
            groups += 1;
            groups - 1
        });
    }
    let mut sections = vec![(0usize, 0usize); groups];
    for (k, &(_, n)) in numbered.iter().enumerate() {
        let g = &mut sections[group[k]];
        g.0 += usize::from(n.titled && heads[k]);
        g.1 += 1;
    }
    for (k, &(i, n)) in numbered.iter().enumerate() {
        let (titles, of) = sections[group[k]];
        let outline = n.capitals || titles * 2 > of;
        if n.titled && heads[k] && outline {
            out[i].kind = "doco:SectionTitle".into();
            out[i].level = Some((n.level + 1).min(6));
        }
    }
}

/// A numbered paragraph, as the outline sees it.
struct Numbered {
    /// The level its number is counted at, from 0.
    level: usize,
    /// The list it is counted in.
    list: String,
    /// Reads as a title: see [`titled`].
    titled: bool,
    /// A title set in capitals.
    capitals: bool,
}

fn flush_para(out: &mut Vec<Element>, para: &mut Para, title_styles: &HashSet<String>) {
    let p = std::mem::take(para);
    // The number Word shows at the head of a numbered paragraph is part of
    // what it says: `9.1.5. Record the reading` is how it is cited.
    // Word shows it on an empty paragraph as well.
    let text = format!("{}{}", p.label.as_deref().unwrap_or(""), p.text);
    // A paragraph's lines are where its line breaks put them, and a blank
    // line between two of them parts the paragraph as an empty paragraph
    // would: a numbered `METHOD`, a blank line, and the section's
    // contents one entry to a line are a list item and what follows it.
    let mut parts: Vec<Vec<String>> = vec![Vec::new()];
    for line in text.split('\n') {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            if parts.last().is_some_and(|p| !p.is_empty()) {
                parts.push(Vec::new());
            }
        } else {
            parts.last_mut().unwrap().push(line);
        }
    }
    parts.retain(|p| !p.is_empty());
    let Some((first, rest)) = parts.split_first() else {
        return;
    };
    // `numId` 0 is a paragraph taken out of its list.
    let numbered = p.numbered && p.num_id.as_deref() != Some("0");
    // A title is one line however Word breaks it.
    let title = first.join(" ");
    let lines = first.join("\n");
    // The title Word's Title style declares: the document's own, which
    // DoCO holds as the document's header rather than as a section's.
    if p.style == "Title" || title_styles.contains(&p.style) {
        out.push(element("doco:Title", title, None));
    } else if let Some(level) = heading_level(&p.style) {
        out.push(element("doco:SectionTitle", title, Some(level)));
    } else if numbered || is_list_style(&p.style) {
        out.push(element("doc:ListItem", lines, None));
    } else {
        out.push(element("doco:Paragraph", lines, None));
    }
    for part in rest {
        out.push(element("doco:Paragraph", part.join("\n"), None));
    }
}

/// Turn Word's row/cell tree into the flat grid the model uses, carrying the
/// declared merges across as `merged_left` / `merged_down`.
///
/// A `w:gridSpan` of n occupies n grid columns, so the cell is emitted once
/// and the columns it covers are marked as continuing it — exactly the
/// convention the PDF engine derives from ruling. `w:vMerge` without
/// `val="restart"` continues the cell above.
///
/// `styled` says the table's style draws its first row as a header.
fn emit_table(out: &mut Vec<Element>, rows: Vec<Row>, styled: bool) {
    let rows: Vec<Row> = rows.into_iter().filter(|r| !r.cells.is_empty()).collect();
    if rows.is_empty() {
        return;
    }
    let width = rows
        .iter()
        .map(|r| r.cells.iter().map(|c| c.span.max(1)).sum::<usize>())
        .max()
        .unwrap_or(0);
    if width == 0 {
        return;
    }
    let n_rows = rows.len();
    let mut grid = vec![String::new(); n_rows * width];
    let mut m_left = vec![false; n_rows * width];
    let mut m_down = vec![false; n_rows * width];
    // Rows that are one cell across the whole grid.
    let mut full_width = vec![false; n_rows];

    for (r, row) in rows.iter().enumerate() {
        let mut c = 0usize;
        for cell in &row.cells {
            if c >= width {
                break;
            }
            let span = cell.span.max(1).min(width - c);
            grid[r * width + c] = cell.text.split_whitespace().collect::<Vec<_>>().join(" ");
            for k in 1..span {
                m_left[r * width + c + k] = true;
            }
            if cell.v_continue && r > 0 {
                m_down[r * width + c] = true;
            }
            c += span;
        }
        full_width[r] = row.cells.len() == 1 && width > 1 && row.cells[0].span.max(1) >= width;
    }

    let cells: Vec<Vec<String>> = (0..n_rows)
        .map(|r| grid[r * width..(r + 1) * width].to_vec())
        .collect();
    let header_rows = header_rows(&rows, &cells, &m_left, &m_down, styled);
    // A list set two or more to a page width: each group is a table of its
    // own, read one after the other.
    if let Some(groups) = side_by_side(&cells, &m_left, header_rows) {
        // The labels repeated over each group are the header, whether or
        // not its look said so: the empty column between the groups is
        // one no header names.
        let header_rows = header_rows.max(1);
        for cols in groups {
            let pick = |m: &[bool]| -> Vec<bool> {
                (0..n_rows)
                    .flat_map(|r| cols.clone().map(move |c| r * width + c))
                    .map(|i| m[i])
                    .collect()
            };
            let part: Vec<Vec<String>> = cells.iter().map(|r| r[cols.clone()].to_vec()).collect();
            let (left, down) = (pick(&m_left), pick(&m_down));
            // Rows the group leaves empty below its header: a shorter
            // list's tail beside a longer one's.
            let w = cols.len();
            let keep: Vec<usize> = (0..n_rows)
                .filter(|&r| r < header_rows || part[r].iter().any(|t| !t.is_empty()))
                .collect();
            let part: Vec<Vec<String>> = keep.iter().map(|&r| part[r].clone()).collect();
            let take = |m: &[bool]| -> Vec<bool> {
                keep.iter()
                    .flat_map(|&r| m[r * w..(r + 1) * w].iter().copied())
                    .collect()
            };
            let (left, mut down) = (take(&left), take(&down));
            // A merge that ran down from a dropped row starts its run here.
            for (k, &r) in keep.iter().enumerate() {
                if k == 0 || keep[k - 1] + 1 != r {
                    for c in 0..w {
                        down[k * w + c] = false;
                    }
                }
            }
            push_table(out, part, left, down, vec![false; keep.len()], header_rows);
        }
        return;
    }
    push_table(out, cells, m_left, m_down, full_width, header_rows);
}

/// One table element from its grid.
fn push_table(
    out: &mut Vec<Element>,
    cells: Vec<Vec<String>>,
    m_left: Vec<bool>,
    m_down: Vec<bool>,
    full_width: Vec<bool>,
    header_rows: usize,
) {
    let n_rows = cells.len();
    let width = cells.first().map_or(0, Vec::len);
    // A body row that is one cell across the whole table labels the rows
    // beneath it, as a section band; the last row has nothing to label.
    let sub_headers: Vec<usize> = (header_rows..n_rows.saturating_sub(1))
        .filter(|&r| full_width[r] && !cells[r][0].is_empty() && !m_down[r * width])
        .collect();
    let text = cells
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = element("doco:Table", text, None);
    e.header_rows = Some(header_rows);
    e.sub_headers = (!sub_headers.is_empty()).then_some(sub_headers);
    e.cells = Some(cells);
    e.merged_left = m_left.iter().any(|x| *x).then_some(m_left);
    e.merged_down = m_down.iter().any(|x| *x).then_some(m_down);
    out.push(e);
}

/// The column ranges of a table that sets one list side by side with
/// itself, two or more up, to fill the page's width.
///
/// A long list of codes under `Group | Code` is set as `Group | Code | |
/// Group | Code`: the list runs down the left pair and carries on down the
/// right. Read as one table, each row joins two entries that have nothing
/// to do with each other, and the right one takes the left one's first cell
/// as its row's name: `G-1` filed under `Fruit`. The header says which kind of table it is: the same
/// labels again, after a column left empty all the way down or at least
/// two labels long, so that no single repeated name splits a table.
fn side_by_side(
    cells: &[Vec<String>],
    m_left: &[bool],
    header_rows: usize,
) -> Option<Vec<std::ops::Range<usize>>> {
    if header_rows > 1 || cells.len() < 3 {
        return None;
    }
    let width = cells[0].len();
    let head = &cells[0];
    let empty_col = |c: usize| cells.iter().all(|r| r[c].trim().is_empty());
    for k in 1..=width / 2 {
        for gap in [1usize, 0] {
            // `n` groups of `k`, `gap` empty columns between each.
            if !(width + gap).is_multiple_of(k + gap) {
                continue;
            }
            let n = (width + gap) / (k + gap);
            if n < 2 || (gap == 0 && k < 2) {
                continue;
            }
            let label = |c: usize| head[c].split_whitespace().collect::<Vec<_>>().join(" ");
            // Labels, not values: a row of figures repeating is data.
            if (0..k).any(|c| label(c).is_empty() || is_numeric(&label(c))) {
                continue;
            }
            let groups: Vec<std::ops::Range<usize>> =
                (0..n).map(|g| g * (k + gap)..g * (k + gap) + k).collect();
            let same = groups
                .iter()
                .all(|g| (0..k).all(|c| label(g.start + c) == label(c)));
            let spacers_empty =
                (1..n).all(|g| (0..gap).all(|j| empty_col(g * (k + gap) - gap + j)));
            // No cell crosses from one group into the next.
            let apart = groups.iter().skip(1).all(|g| {
                (0..cells.len()).all(|r| !m_left[r * width + g.start])
                    && (0..gap)
                        .all(|j| (0..cells.len()).all(|r| !m_left[r * width + g.start - gap + j]))
            });
            if same && spacers_empty && apart {
                return Some(groups);
            }
        }
    }
    None
}

/// How many leading rows of a table are its header.
///
/// Word does not require a table to have one, and a key/value block — a
/// bold label column beside amounts — has none: taking its first row as
/// the header names the columns `Subtotal` and `38.60` and leaves the
/// subtotal in no cell. So a row is a header only where the document says
/// so:
///
/// * rows marked `w:tblHeader`, which Word repeats on every page;
/// * else a first row set apart — bold while the row under it is not, or
///   drawn as a header by the table's style, unless it is typed like the
///   data under it;
/// * else a first row that reads as labels: every column named, none of
///   the names a number or a sentence, over at least one column of numbers
///   or across three columns or more.
///
/// A header found the second or third way runs on through the rows its
/// cells reach down into (`w:vMerge`): `Item` merged over two rows beside
/// `Price` spanning `Unit` and `Line` is a two-row header.
fn header_rows(
    rows: &[Row],
    cells: &[Vec<String>],
    m_left: &[bool],
    m_down: &[bool],
    styled: bool,
) -> usize {
    let n_rows = rows.len();
    if n_rows < 2 {
        return 0;
    }
    let repeated = rows.iter().take_while(|r| r.repeats).count();
    if repeated > 0 {
        return repeated.min(n_rows - 1);
    }
    let width = cells[0].len();
    let all_bold = |r: &Row| {
        r.cells.iter().any(|c| c.bold()) && r.cells.iter().all(|c| c.bold() || !c.plain_text)
    };
    // A label column — the first cell bold on every row, no other cell
    // bold anywhere — makes the table a list of fields, `Description |
    // Cinnamon bun` over `Code | BK-215`: its first row is a field like
    // the rest, not a header.
    let labels_down = rows.iter().any(|r| r.cells.first().is_some_and(Cell::bold))
        && rows
            .iter()
            .filter(|r| r.cells.first().is_some_and(|c| c.bold_text || c.plain_text))
            .all(|r| r.cells[0].bold())
        && rows
            .iter()
            .all(|r| r.cells.iter().skip(1).all(|c| !c.bold()));
    if width >= 2 && labels_down {
        return 0;
    }
    // Columns whose body is mostly numbers — all numbers in a table of two
    // columns, where a field list's values (`Cinnamon bun`, `BK-215`, `1`,
    // `£7.90`) are as often numbers as not, and a line table's amounts
    // are numbers throughout.
    let numeric_cols: Vec<usize> = (0..width)
        .filter(|&c| {
            let body: Vec<&str> = cells[1..]
                .iter()
                .map(|r| r[c].as_str())
                .filter(|t| !t.is_empty())
                .collect();
            let numbers = body.iter().filter(|t| is_numeric(t)).count();
            !body.is_empty()
                && if width == 2 {
                    numbers == body.len()
                } else {
                    numbers * 2 > body.len()
                }
        })
        .collect();
    let labelled = if all_bold(&rows[0]) && !all_bold(&rows[1]) {
        true
    } else if styled {
        // A style formats whatever row comes first, so its say is weighed
        // against the text: a first row that is numbers where the body is
        // numbers (`Subtotal | 38.60` in a shaded key/value table) is data.
        let typed_as_data = numeric_cols
            .iter()
            .filter(|&&c| is_numeric(&cells[0][c]))
            .count();
        numeric_cols.is_empty() || typed_as_data * 2 <= numeric_cols.len()
    } else {
        let named = (0..width).all(|c| !cells[0][c].is_empty() || m_left[c]);
        let labels = cells[0]
            .iter()
            .all(|t| !is_numeric(t) && t.split_whitespace().count() <= MAX_LABEL_WORDS);
        // Over numbers a row of labels is a header. Over text it is one
        // only in a table wider than a key/value pair: two columns of text
        // are as often `Date | 11 August 2026` as `Name | Role`, while a
        // row naming three or more columns is a header row.
        named && labels && (!numeric_cols.is_empty() || width >= 3)
    };
    if !labelled {
        return 0;
    }
    let mut n = 1;
    while n + 1 < n_rows && (0..width).any(|c| m_down[n * width + c]) {
        n += 1;
    }
    n
}

/// Longest text a column label runs to, in words; longer is prose.
const MAX_LABEL_WORDS: usize = 6;

/// A cell that reads as a quantity: digits dressed with a sign, grouping,
/// a decimal point, a currency symbol or a percent sign, and nothing else.
/// A bare four-digit year is a label (`2023 | 2024` over amounts is a
/// header), and a date's separators make it no number.
fn is_numeric(s: &str) -> bool {
    let s = s.trim();
    if !s.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    if s.len() == 4 && s.parse::<u32>().is_ok_and(|y| (1900..2100).contains(&y)) {
        return false;
    }
    let body = s.trim_start_matches(['-', '+', '\u{2212}', '(']);
    body.chars().all(|c| {
        c.is_ascii_digit()
            || matches!(
                c,
                '$' | '€' | '£' | '¥' | '%' | ',' | '.' | ')' | ' ' | '\u{a0}'
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

    fn doc(body: &str) -> String {
        format!("<w:document {NS}><w:body>{body}</w:body></w:document>")
    }

    fn para(style: Option<&str>, text: &str) -> String {
        let p = style
            .map(|s| format!("<w:pPr><w:pStyle w:val=\"{s}\"/></w:pPr>"))
            .unwrap_or_default();
        format!("<w:p>{p}<w:r><w:t>{text}</w:t></w:r></w:p>")
    }

    /// A level as `(numFmt, lvlText, extra level XML)`.
    type Lvl<'a> = (&'a str, &'a str, &'a str);

    /// `numbering.xml` with one abstract numbering per `(id, levels)`, and
    /// `w:num`s as given.
    fn numbering_xml(abstracts: &[(&str, &[Lvl])], nums: &str) -> String {
        let mut x = format!("<w:numbering {NS}>");
        for (id, levels) in abstracts {
            x.push_str(&format!("<w:abstractNum w:abstractNumId=\"{id}\">"));
            for (i, (fmt, text, extra)) in levels.iter().enumerate() {
                x.push_str(&format!(
                    "<w:lvl w:ilvl=\"{i}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{fmt}\"/>\
                     <w:lvlText w:val=\"{text}\"/>{extra}</w:lvl>"
                ));
            }
            x.push_str("</w:abstractNum>");
        }
        x.push_str(nums);
        x.push_str("</w:numbering>");
        x
    }

    /// A paragraph in list `num` at `lvl`.
    fn item(num: &str, lvl: usize, text: &str) -> String {
        format!(
            "<w:p><w:pPr><w:numPr><w:ilvl w:val=\"{lvl}\"/><w:numId w:val=\"{num}\"/></w:numPr></w:pPr>\
             <w:r><w:t>{text}</w:t></w:r></w:p>"
        )
    }

    fn numbered(body: &str, numbering: &str, styles: Option<&str>) -> Vec<String> {
        let n = Numbering::new(Some(numbering), styles);
        parse_with_styles(&doc(body), &HashSet::new(), &HashSet::new(), n)
            .unwrap()
            .into_iter()
            .map(|e| e.text)
            .collect()
    }

    fn read(body: &str) -> Vec<Element> {
        parse_document_xml(&doc(body)).unwrap()
    }

    #[test]
    fn a_line_break_ends_a_line_and_a_blank_line_ends_a_paragraph() {
        let els = read(
            "<w:p><w:r><w:t>Scope</w:t><w:br/><w:br/><w:t>Contents:</w:t><w:br/><w:br/>\
             <w:t>1 First part</w:t><w:br/><w:t>2 Second part</w:t></w:r></w:p>",
        );
        let texts: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["Scope", "Contents:", "1 First part\n2 Second part"]);
        // A heading broken over two lines is one heading.
        let h = read("<w:p><w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr><w:r><w:t>Part</w:t><w:br/><w:t>One</w:t></w:r></w:p>");
        assert_eq!(h[0].text, "Part One");
    }

    #[test]
    fn symbol_font_characters_are_the_characters_they_show() {
        let els = read(
            "<w:p><w:r><w:t xml:space=\"preserve\">Low </w:t></w:r>\
             <w:r><w:sym w:font=\"Wingdings\" w:char=\"F0E0\"/></w:r>\
             <w:r><w:t xml:space=\"preserve\"> High, 25 </w:t></w:r>\
             <w:r><w:sym w:font=\"Symbol\" w:char=\"F0B1\"/></w:r>\
             <w:r><w:t xml:space=\"preserve\"> 2 </w:t></w:r>\
             <w:r><w:rPr><w:rFonts w:ascii=\"Symbol\" w:hAnsi=\"Symbol\"/></w:rPr><w:t>m</w:t></w:r>\
             <w:r><w:t>g</w:t></w:r></w:p>",
        );
        assert_eq!(els[0].text, "Low → High, 25 ± 2 μg");
    }

    #[test]
    fn a_page_number_field_shows_no_stale_page() {
        let els = read(
            "<w:p><w:r><w:t xml:space=\"preserve\">Page </w:t></w:r>\
             <w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
             <w:r><w:instrText xml:space=\"preserve\"> PAGE </w:instrText></w:r>\
             <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>2</w:t></w:r>\
             <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>\
             <w:r><w:t xml:space=\"preserve\"> of the record, see </w:t></w:r>\
             <w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
             <w:r><w:instrText xml:space=\"preserve\"> REF step4 </w:instrText></w:r>\
             <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>step 4</w:t></w:r>\
             <w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>",
        );
        assert_eq!(els[0].text, "Page of the record, see step 4");
    }

    #[cfg(feature = "ocr")]
    #[test]
    fn a_pictures_text_is_read_after_its_figure() {
        use std::io::Write;
        /// Reads one line across whatever it is shown.
        struct OneLine;
        impl fluree_doc_vision::OcrEngine for OneLine {
            fn name(&self) -> &str {
                "one-line"
            }
            fn read(
                &self,
                image: &fluree_doc_vision::Pixels,
            ) -> Result<fluree_doc_vision::WordReading, String> {
                let (w, h) = (image.width as f64, image.height as f64);
                Ok(fluree_doc_vision::WordReading {
                    coords: fluree_doc_vision::Coords::Px,
                    width: Some(w),
                    height: Some(h),
                    engine: None,
                    words: vec![fluree_doc_vision::OcrWord {
                        text: "Reference 4417-0093-21".into(),
                        bbox: [w * 0.1, h * 0.4, w * 0.9, h * 0.6],
                        angle: None,
                        conf: Some(0.9),
                    }],
                })
            }
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            400,
            120,
            image::Rgb([240, 240, 240]),
        ))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let body = doc(&format!(
            "{}<w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"1\" name=\"Picture 1\"/>\
             <a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed=\"rId7\"/>\
             </pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>{}",
            para(None, "See below."),
            para(None, "After.")
        ));
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let parts: [(&str, Vec<u8>); 3] = [
            ("word/document.xml", body.into_bytes()),
            (
                "word/_rels/document.xml.rels",
                format!("<Relationships><Relationship Id=\"rId7\" Type=\"{rel}/image\" Target=\"media/image1.png\"/></Relationships>")
                    .into_bytes(),
            ),
            ("word/media/image1.png", png),
        ];
        for (name, bytes) in parts {
            z.start_file(name, opts).unwrap();
            z.write_all(&bytes).unwrap();
        }
        z.finish().unwrap();
        let read = parse_with_ocr(&buf.into_inner(), &OneLine).unwrap();
        assert!(read.unread.is_empty(), "{:?}", read.unread);
        let els = read.elements;
        let got: Vec<(&str, &str)> = els
            .iter()
            .map(|e| (e.kind.as_str(), e.text.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("doco:Paragraph", "See below."),
                ("doco:Figure", ""),
                ("doco:Paragraph", "Reference 4417-0093-21"),
                ("doco:Paragraph", "After."),
            ]
        );
        assert_eq!(els[2].provenance, "ocr");
    }

    #[test]
    fn a_pictures_image_comes_with_its_figure() {
        use std::io::Write;
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let pic = |rid: &str| {
            format!(
                "<w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"1\" name=\"Picture 1\"/>\
                 <a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed=\"{rid}\"/>\
                 </pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"
            )
        };
        // A picture in the package, one whose image is elsewhere, and text.
        let body = doc(&format!(
            "{}{}{}{}",
            para(None, "See the screenshot below."),
            pic("rId7"),
            pic("rId8"),
            para(None, "After")
        ));
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let parts: [(&str, Vec<u8>); 3] = [
            ("word/document.xml", body.into_bytes()),
            (
                "word/_rels/document.xml.rels",
                format!(
                    "<Relationships><Relationship Id=\"rId7\" Type=\"{rel}/image\" Target=\"media/image1.png\"/>\
                     <Relationship Id=\"rId8\" Type=\"{rel}/image\" Target=\"https://example.org/x.png\" TargetMode=\"External\"/>\
                     </Relationships>"
                )
                .into_bytes(),
            ),
            ("word/media/image1.png", b"\x89PNG not really".to_vec()),
        ];
        for (name, bytes) in parts {
            z.start_file(name, opts).unwrap();
            z.write_all(&bytes).unwrap();
        }
        z.finish().unwrap();
        let docx = buf.into_inner();
        let (els, _, media) = parse_with_media(&docx).unwrap();
        let figures: Vec<&Element> = els.iter().filter(|e| e.kind == "doco:Figure").collect();
        assert_eq!(figures.len(), 2);
        // Both are listed; only the image the package holds can be read.
        assert_eq!(media.len(), 2, "{media:?}");
        assert_eq!(
            media[0].source,
            MediaSource::Part("word/media/image1.png".into())
        );
        assert_eq!(media[0].tag(), "m1");
        assert_eq!(
            read_source(&docx, &media[0].source).unwrap(),
            b"\x89PNG not really"
        );
        assert_eq!(Some(&media[0].figure), figures[0].figure.as_ref());
        assert_eq!(
            media[1].source,
            MediaSource::External("https://example.org/x.png".into())
        );
        assert!(read_source(&docx, &media[1].source).is_err());
    }

    #[test]
    fn a_figures_images_are_named_by_its_number() {
        let m = |figure: &str, index| Media {
            figure: figure.into(),
            source: MediaSource::Missing(String::new()),
            index,
        };
        assert_eq!(m("figure-0-3", 0).tag(), "m3");
        assert_eq!(m("figure-0-12", 0).tag(), "m12");
        assert_eq!(m("figure-0-12", 2).tag(), "m12_2");
    }

    #[test]
    fn a_group_of_pictures_is_one_figure_with_each_image() {
        // A group shape holding two pictures, and a picture after it.
        let pic = |rid: &str| {
            format!("<pic:pic><pic:blipFill><a:blip r:embed=\"{rid}\"/></pic:blipFill></pic:pic>")
        };
        let body = format!(
            "<w:document><w:body><w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"1\" name=\"Group\"/>\
             <a:graphic><a:graphicData><wpg:wgp>{}{}</wpg:wgp></a:graphicData></a:graphic></wp:inline>\
             </w:drawing></w:r></w:p><w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"2\" name=\"P\"/>\
             <a:graphic><a:graphicData>{}</a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>\
             </w:body></w:document>",
            pic("rId1"),
            pic("rId2"),
            pic("rId3")
        );
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let rels: String = (1..=3)
            .map(|i| {
                format!("<Relationship Id=\"rId{i}\" Type=\"{rel}/image\" Target=\"media/image{i}.png\"/>")
            })
            .collect();
        let mut parts = vec![
            ("word/document.xml".to_string(), body.into_bytes()),
            (
                "word/_rels/document.xml.rels".to_string(),
                format!("<Relationships>{rels}</Relationships>").into_bytes(),
            ),
        ];
        for i in 1..=3 {
            parts.push((format!("word/media/image{i}.png"), vec![i as u8]));
        }
        let docx = zipped(&parts);
        let (els, _, media) = parse_with_media(&docx).unwrap();
        let figures = els.iter().filter(|e| e.kind == "doco:Figure").count();
        assert_eq!(figures, 2);
        let tags: Vec<String> = media.iter().map(Media::tag).collect();
        assert_eq!(tags, ["m1", "m1_1", "m2"]);
        assert_eq!(read_source(&docx, &media[1].source).unwrap(), [2]);
    }

    /// A package of `body` (inside `w:body`), the image relationships
    /// `rels` (id, target) and the parts `parts`.
    fn package(body: &str, rels: &[(&str, &str)], parts: &[(&str, &[u8])]) -> Vec<u8> {
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let rels: String = rels
            .iter()
            .map(|(id, target)| {
                format!("<Relationship Id=\"{id}\" Type=\"{rel}/image\" Target=\"{target}\"/>")
            })
            .collect();
        let mut all = vec![
            (
                "word/document.xml".to_string(),
                format!("<w:document><w:body>{body}</w:body></w:document>").into_bytes(),
            ),
            (
                "word/_rels/document.xml.rels".to_string(),
                format!("<Relationships>{rels}</Relationships>").into_bytes(),
            ),
        ];
        all.extend(parts.iter().map(|(n, b)| (n.to_string(), b.to_vec())));
        zipped(&all)
    }

    /// A paragraph holding one DrawingML picture of `rid`.
    fn picture(rid: &str) -> String {
        format!(
            "<w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"1\" name=\"P\"/><a:graphic><a:graphicData>\
             <pic:pic><pic:blipFill><a:blip r:embed=\"{rid}\"/></pic:blipFill></pic:pic>\
             </a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"
        )
    }

    #[test]
    fn a_picture_deleted_under_tracked_changes_is_no_figure() {
        let body = format!(
            "<w:p><w:r><w:t>Before.</w:t></w:r></w:p>\
             <w:p><w:del w:id=\"1\" w:author=\"A\"><w:r><w:drawing><wp:inline><wp:docPr id=\"1\" name=\"P\"/>\
             <a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed=\"rId1\"/></pic:blipFill></pic:pic>\
             </a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:del></w:p>\
             <w:p><w:moveFrom w:id=\"2\" w:author=\"A\"><w:r><w:pict><v:shape><v:imagedata r:id=\"rId1\"/></v:shape></w:pict></w:r></w:moveFrom></w:p>\
             {}",
            picture("rId2")
        );
        let docx = package(
            &body,
            &[("rId1", "media/image1.png"), ("rId2", "media/image2.png")],
            &[
                ("word/media/image1.png", b"1"),
                ("word/media/image2.png", b"2"),
            ],
        );
        let (els, _, media) = parse_with_media(&docx).unwrap();
        let figures: Vec<&Element> = els.iter().filter(|e| e.kind == "doco:Figure").collect();
        assert_eq!(figures.len(), 1, "only the picture the document holds");
        assert_eq!(media.len(), 1, "{media:?}");
        assert_eq!(
            media[0].source,
            MediaSource::Part("word/media/image2.png".into())
        );
        assert_eq!(
            media[0].tag(),
            "m1",
            "numbered as if the deleted ones were not there"
        );
    }

    #[test]
    fn a_picture_deleted_inside_a_text_box_is_no_figure() {
        // A group holding a text box with a deleted picture in it, then a
        // live picture: one figure, of the live picture.
        let body = "<w:p><w:r><w:drawing><wp:anchor><wp:docPr id=\"1\" name=\"Group\"/><a:graphic><a:graphicData>\
             <wpg:wgp><wps:wsp><wps:txbx><w:txbxContent><w:p><w:del w:id=\"1\" w:author=\"A\"><w:r><w:drawing>\
             <wp:inline><wp:docPr id=\"2\" name=\"P\"/><a:graphic><a:graphicData><pic:pic><pic:blipFill>\
             <a:blip r:embed=\"rId1\"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline>\
             </w:drawing></w:r></w:del></w:p><w:p><w:r><w:t>Live text</w:t></w:r></w:p></w:txbxContent></wps:txbx>\
             </wps:wsp><pic:pic><pic:blipFill><a:blip r:embed=\"rId2\"/></pic:blipFill></pic:pic></wpg:wgp>\
             </a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p>";
        let docx = package(
            body,
            &[("rId1", "media/image1.png"), ("rId2", "media/image2.png")],
            &[
                ("word/media/image1.png", b"1"),
                ("word/media/image2.png", b"2"),
            ],
        );
        let (els, _, media) = parse_with_media(&docx).unwrap();
        let figures = els.iter().filter(|e| e.kind == "doco:Figure").count();
        assert_eq!(figures, 1, "{els:?}");
        let parts: Vec<String> = media.iter().map(|m| m.source.to_string()).collect();
        assert_eq!(parts, ["word/media/image2.png"]);
    }

    #[test]
    fn a_linked_picture_and_a_missing_relationship_are_listed_unreadable() {
        // Insert > Picture > Link to File writes `r:link`, no `r:embed`.
        let linked = "<w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"1\" name=\"P\"/><a:graphic><a:graphicData>\
             <pic:pic><pic:blipFill><a:blip r:link=\"rId3\"/></pic:blipFill></pic:pic>\
             </a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>";
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let body = format!(
            "<w:document><w:body>{linked}{}</w:body></w:document>",
            picture("rId9")
        );
        let docx = zipped(&[
            ("word/document.xml".to_string(), body.into_bytes()),
            (
                "word/_rels/document.xml.rels".to_string(),
                format!(
                    "<Relationships><Relationship Id=\"rId3\" Type=\"{rel}/image\" \
                     Target=\"file:///C:/pics/logo.png\" TargetMode=\"External\"/></Relationships>"
                )
                .into_bytes(),
            ),
        ]);
        let (_, _, media) = parse_with_media(&docx).unwrap();
        let sources: Vec<&MediaSource> = media.iter().map(|m| &m.source).collect();
        assert_eq!(
            sources,
            [
                &MediaSource::External("file:///C:/pics/logo.png".into()),
                &MediaSource::Missing("rId9".into())
            ]
        );
        assert!(media.iter().all(|m| read_source(&docx, &m.source).is_err()));
    }

    #[test]
    fn a_part_named_exactly_wins_over_one_differing_in_case() {
        let docx = package(
            &picture("rId1"),
            &[("rId1", "media/image1.png")],
            &[
                ("word/media/image1.png", b"lower"),
                ("word/media/IMAGE1.png", b"upper"),
            ],
        );
        let (_, _, media) = parse_with_media(&docx).unwrap();
        assert_eq!(read_source(&docx, &media[0].source).unwrap(), b"lower");
    }

    #[test]
    fn a_shapes_image_fill_is_not_one_of_its_pictures() {
        // A group holding a shape filled with a texture, then a picture.
        let body = "<w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"1\" name=\"G\"/><a:graphic><a:graphicData>\
             <wpg:wgp><wps:wsp><wps:spPr><a:blipFill><a:blip r:embed=\"rId2\"/></a:blipFill></wps:spPr></wps:wsp>\
             <pic:pic><pic:blipFill><a:blip r:embed=\"rId1\"/></pic:blipFill></pic:pic></wpg:wgp>\
             </a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>";
        let docx = package(
            body,
            &[("rId1", "media/image1.png"), ("rId2", "media/image2.png")],
            &[
                ("word/media/image1.png", b"1"),
                ("word/media/image2.png", b"2"),
            ],
        );
        let (_, _, media) = parse_with_media(&docx).unwrap();
        let parts: Vec<String> = media.iter().map(|m| m.source.to_string()).collect();
        assert_eq!(parts, ["word/media/image1.png"]);
    }

    #[test]
    fn an_image_part_is_found_whatever_the_case_or_slashes_of_its_name() {
        let body = format!("{}{}{}", picture("rId1"), picture("rId2"), picture("rId3"));
        let docx = package(
            &body,
            &[
                ("rId1", "media/Image1.PNG"),
                ("rId2", "media\\image2.png"),
                ("rId3", "media/missing.png"),
            ],
            &[
                ("word/media/image1.png", b"1"),
                ("word/media/image2.png", b"2"),
            ],
        );
        let (_, _, media) = parse_with_media(&docx).unwrap();
        assert_eq!(media.len(), 3);
        assert_eq!(read_source(&docx, &media[0].source).unwrap(), b"1");
        assert_eq!(read_source(&docx, &media[1].source).unwrap(), b"2");
        // A part that is not there is still listed, and fails to read.
        assert!(read_source(&docx, &media[2].source).is_err());
    }

    #[test]
    fn a_vml_image_and_a_repeated_blip_are_each_read_once() {
        // A VML image, and a picture naming its image twice (as a picture
        // with an alternate-content copy may).
        let body = "<w:p><w:r><w:pict><v:shape><v:imagedata r:id=\"rId1\" o:title=\"Logo\"/></v:shape></w:pict></w:r></w:p>\
             <w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"2\" name=\"P\"/><a:graphic><a:graphicData>\
             <pic:pic><pic:blipFill><a:blip r:embed=\"rId2\"/><a:blip r:embed=\"rId2\"/></pic:blipFill></pic:pic>\
             </a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>";
        let docx = package(
            body,
            &[("rId1", "media/image1.png"), ("rId2", "media/image2.png")],
            &[
                ("word/media/image1.png", b"1"),
                ("word/media/image2.png", b"2"),
            ],
        );
        let (_, _, media) = parse_with_media(&docx).unwrap();
        let tags: Vec<String> = media.iter().map(Media::tag).collect();
        assert_eq!(tags, ["m1", "m2"]);
        assert_eq!(
            media[0].source,
            MediaSource::Part("word/media/image1.png".into())
        );
    }

    #[cfg(feature = "ocr")]
    #[test]
    fn pictures_not_read_are_reported_with_why() {
        struct Never;
        impl fluree_doc_vision::OcrEngine for Never {
            fn name(&self) -> &str {
                "never"
            }
            fn read(
                &self,
                _: &fluree_doc_vision::Pixels,
            ) -> Result<fluree_doc_vision::WordReading, String> {
                Err("no engine".into())
            }
        }
        let docx = package(
            &format!("{}{}", picture("rId1"), picture("rId2")),
            &[("rId1", "media/image1.emf"), ("rId2", "media/gone.png")],
            &[("word/media/image1.emf", b"\x01\x00\x00\x00 an EMF")],
        );
        let read = parse_with_ocr(&docx, &Never).unwrap();
        let why: Vec<(String, bool)> = read
            .unread
            .iter()
            .map(|(p, e)| (p.tag(), matches!(e, Unread::Failed(e) if !e.is_empty())))
            .collect();
        assert_eq!(why, [("m1".to_string(), true), ("m2".to_string(), true)]);
    }

    #[cfg(feature = "ocr")]
    #[test]
    fn an_image_drawn_by_many_figures_goes_to_the_engine_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        /// Reads one line across whatever it is shown, and counts its calls.
        struct Counting(AtomicUsize);
        impl fluree_doc_vision::OcrEngine for Counting {
            fn name(&self) -> &str {
                "counting"
            }
            fn read(
                &self,
                image: &fluree_doc_vision::Pixels,
            ) -> Result<fluree_doc_vision::WordReading, String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                let (w, h) = (image.width as f64, image.height as f64);
                Ok(fluree_doc_vision::WordReading {
                    coords: fluree_doc_vision::Coords::Px,
                    width: Some(w),
                    height: Some(h),
                    engine: None,
                    words: vec![fluree_doc_vision::OcrWord {
                        text: "Logo".into(),
                        bbox: [w * 0.1, h * 0.4, w * 0.9, h * 0.6],
                        angle: None,
                        conf: Some(0.9),
                    }],
                })
            }
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            200,
            60,
            image::Rgb([240, 240, 240]),
        ))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
        // Fifty figures of one image, by two relationships to it.
        let body: String = (0..50)
            .map(|i| picture(if i % 2 == 0 { "rId1" } else { "rId2" }))
            .collect();
        let docx = package(
            &body,
            &[("rId1", "media/image1.png"), ("rId2", "media/image1.png")],
            &[("word/media/image1.png", &png)],
        );
        let engine = Counting(AtomicUsize::new(0));
        let read = parse_with_ocr(&docx, &engine).unwrap();
        assert_eq!(engine.0.load(Ordering::SeqCst), 1);
        assert!(read.unread.is_empty(), "{:?}", read.unread);
        let kinds: Vec<&str> = read.elements.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(kinds, ["", "Logo"].repeat(50));
    }

    /// Reads a part whose bytes are a decimal number of pixels, and counts
    /// what it reads.
    struct Digits {
        reads: usize,
        skip: Option<&'static str>,
    }

    impl PictureReader for Digits {
        type Reading = String;
        fn ready(&mut self, picture: &Picture) -> bool {
            self.skip != Some(picture.tag().as_str())
        }
        fn pixels(&mut self, image: &[u8]) -> Result<u64, String> {
            std::str::from_utf8(image)
                .ok()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| "not a number".to_string())
        }
        fn read(&mut self, picture: &Picture, _: &[u8]) -> Result<String, String> {
            self.reads += 1;
            Ok(picture.tag())
        }
    }

    #[test]
    fn a_picture_is_read_once_and_placed_after_each_figure_drawing_it() {
        // Three figures of one image, the last naming it twice by two ids.
        let group = "<w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"1\" name=\"G\"/><a:graphic><a:graphicData>\
             <wpg:wgp><pic:pic><pic:blipFill><a:blip r:embed=\"rId1\"/></pic:blipFill></pic:pic>\
             <pic:pic><pic:blipFill><a:blip r:embed=\"rId2\"/></pic:blipFill></pic:pic></wpg:wgp>\
             </a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>";
        let body = format!("{}{}{group}", picture("rId1"), picture("rId2"));
        let docx = package(
            &body,
            &[("rId1", "media/image1.png"), ("rId2", "media/image1.png")],
            &[("word/media/image1.png", b"10")],
        );
        let (_, _, media) = parse_with_media(&docx).unwrap();
        assert_eq!(media.len(), 4);
        let mut reader = Digits {
            reads: 0,
            skip: None,
        };
        let mut pkg = Package::open(&docx).unwrap();
        let reads = read_pictures(&mut pkg, media, &mut reader, Allowance::DOCUMENT);
        assert_eq!(reader.reads, 1);
        assert!(reads.unread.is_empty());
        let (picture, tag) = &reads.read[0];
        assert_eq!(tag, "m1");
        let placed = picture.after_each_figure(vec![1]);
        assert_eq!(
            placed,
            [
                ("figure-0-1".to_string(), vec![1]),
                ("figure-0-2".to_string(), vec![1]),
                ("figure-0-3".to_string(), vec![1]),
            ]
        );
    }

    #[test]
    fn pictures_past_the_documents_allowance_are_left_unread() {
        let body: String = (1..=5).map(|i| picture(&format!("rId{i}"))).collect();
        let rels: Vec<(String, String)> = (1..=5)
            .map(|i| (format!("rId{i}"), format!("media/image{i}.png")))
            .collect();
        let rels: Vec<(&str, &str)> = rels.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        // Pixels: 40, 50, 30, 20, 10; the second is not asked for.
        let docx = package(
            &body,
            &rels,
            &[
                ("word/media/image1.png", b"40"),
                ("word/media/image2.png", b"50"),
                ("word/media/image3.png", b"30"),
                ("word/media/image4.png", b"20"),
                ("word/media/image5.png", b"10"),
            ],
        );
        let run = |allowance| {
            let (_, _, media) = parse_with_media(&docx).unwrap();
            let mut reader = Digits {
                reads: 0,
                skip: Some("m2"),
            };
            let reads = read_pictures(
                &mut Package::open(&docx).unwrap(),
                media,
                &mut reader,
                allowance,
            );
            let read: Vec<String> = reads.read.into_iter().map(|(_, t)| t).collect();
            let unread: Vec<(String, Unread)> = reads
                .unread
                .into_iter()
                .map(|(p, e)| (p.tag(), e))
                .collect();
            (reader.reads, read, unread)
        };
        // Three pictures: the fourth and fifth are past it; the one not
        // asked for is not charged.
        let (reads, read, unread) = run(Allowance {
            pictures: 3,
            pixels: u64::MAX,
        });
        assert_eq!(reads, 3);
        assert_eq!(read, ["m1", "m3", "m4"]);
        assert_eq!(
            unread,
            [
                ("m2".to_string(), Unread::NotAsked),
                ("m5".to_string(), Unread::PastAllowance)
            ]
        );
        // Eighty pixels: 40 and 30 fit, 20 would not, 10 does.
        let (reads, read, unread) = run(Allowance {
            pictures: usize::MAX,
            pixels: 80,
        });
        assert_eq!(reads, 3);
        assert_eq!(read, ["m1", "m3", "m5"]);
        assert_eq!(
            unread,
            [
                ("m2".to_string(), Unread::NotAsked),
                ("m4".to_string(), Unread::PastAllowance)
            ]
        );
    }

    #[test]
    fn an_external_image_named_like_a_part_is_never_read() {
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let body = format!(
            "<w:document><w:body>{}</w:body></w:document>",
            picture("rId1")
        );
        let docx = zipped(&[
            ("word/document.xml".to_string(), body.into_bytes()),
            (
                "word/_rels/document.xml.rels".to_string(),
                format!(
                    "<Relationships><Relationship Id=\"rId1\" Type=\"{rel}/image\" \
                     Target=\"word/media/image1.png\" TargetMode=\"External\"/></Relationships>"
                )
                .into_bytes(),
            ),
            ("word/media/image1.png".to_string(), b"10".to_vec()),
        ]);
        let (_, _, media) = parse_with_media(&docx).unwrap();
        assert_eq!(
            media[0].source,
            MediaSource::External("word/media/image1.png".into())
        );
        let mut reader = Digits {
            reads: 0,
            skip: None,
        };
        let reads = read_pictures(
            &mut Package::open(&docx).unwrap(),
            media,
            &mut reader,
            Allowance::DOCUMENT,
        );
        assert_eq!(reader.reads, 0);
        assert_eq!(reads.unread[0].1, Unread::NotInPackage);
    }

    #[test]
    fn an_image_part_past_the_cap_is_not_read() {
        // A part that inflates to more than the cap: refused as it is read,
        // whatever size the archive declares for it.
        let big = vec![0u8; (MAX_MEDIA_BYTES + 1) as usize];
        let docx = zipped(&[("word/media/image1.png".to_string(), big)]);
        let e = read_source(&docx, &MediaSource::Part("word/media/image1.png".into())).unwrap_err();
        assert!(e.contains("image1.png"), "{e}");
    }

    /// The bytes of a media source of the package `docx`: read only from
    /// a part the package holds.
    fn read_source(docx: &[u8], source: &MediaSource) -> Result<Vec<u8>, String> {
        let part = source.part().ok_or_else(|| source.to_string())?;
        read_media(&mut Package::open(docx)?, part)
    }

    /// A deflated archive of `parts`.
    fn zipped(parts: &[(String, Vec<u8>)]) -> Vec<u8> {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in parts {
            z.start_file(name.as_str(), opts).unwrap();
            z.write_all(bytes).unwrap();
        }
        z.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn a_picture_leaves_a_figure_with_its_alternative_text() {
        let pic = |descr: &str| {
            format!(
                "<w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"1\" name=\"Picture 1\" descr=\"{descr}\"/>\
                 <a:graphic><a:graphicData><pic:pic/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"
            )
        };
        let els = read(&format!(
            "{}{}{}",
            para(None, "Before"),
            pic("A flow chart of the steps&#xA;&#xA;AI-generated content may be incorrect."),
            pic("")
        ));
        let kinds: Vec<&str> = els.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, ["doco:Paragraph", "doco:Figure", "doco:Figure"]);
        assert_eq!(els[1].text, "A flow chart of the steps");
        assert_eq!(els[2].text, "");
        assert_ne!(els[1].figure, els[2].figure);
        // An embedded object is a figure too; the fallback Word keeps for
        // readers that cannot show the drawing is not a second one.
        let obj = read(
            "<w:p><w:r><w:object><v:shape><v:imagedata r:id=\"rId9\" o:title=\"\"/></v:shape>\
             <o:OLEObject Type=\"Embed\" ProgID=\"Excel.Sheet.12\"/></w:object></w:r>\
             <mc:AlternateContent><mc:Choice><w:r><w:t>Shown</w:t></w:r></mc:Choice>\
             <mc:Fallback><w:r><w:t>Shown</w:t></w:r></mc:Fallback></mc:AlternateContent></w:p>",
        );
        assert_eq!(obj.len(), 2);
        assert_eq!(obj[0].text, "Shown");
        assert_eq!(obj[1].kind, "doco:Figure");
    }

    #[test]
    fn a_list_set_two_up_is_two_tables() {
        let row = |cells: &[&str]| {
            let tcs: String = cells
                .iter()
                .map(|c| format!("<w:tc><w:p><w:r><w:t>{c}</w:t></w:r></w:p></w:tc>"))
                .collect();
            format!("<w:tr>{tcs}</w:tr>")
        };
        let merged = |c: &str| {
            format!("<w:tc><w:tcPr><w:vMerge w:val=\"restart\"/></w:tcPr><w:p><w:r><w:t>{c}</w:t></w:r></w:p></w:tc>")
        };
        let cont = "<w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc>";
        let cell = |c: &str| format!("<w:tc><w:p><w:r><w:t>{c}</w:t></w:r></w:p></w:tc>");
        let empty = "<w:tc><w:p/></w:tc>";
        let tbl = format!(
            "<w:tbl>{}<w:tr>{}{}{empty}{}{}</w:tr><w:tr>{cont}{}{empty}{cont}{}</w:tr>\
             <w:tr>{}{}{empty}{empty}{empty}</w:tr></w:tbl>",
            row(&["Kind", "Code", "", "Kind", "Code"]),
            merged("Fruit"),
            cell("F-1"),
            merged("Grain"),
            cell("G-1"),
            cell("F-2"),
            cell("G-2"),
            cell("Root"),
            cell("R-1"),
        );
        let els = read(&tbl);
        let tables: Vec<&Element> = els.iter().filter(|e| e.kind == "doco:Table").collect();
        assert_eq!(tables.len(), 2);
        let right = tables[1].cells.as_ref().unwrap();
        assert_eq!(right[0], ["Kind", "Code"]);
        assert_eq!(right[1], ["Grain", "G-1"]);
        assert_eq!(right.len(), 3, "the shorter list's empty tail is dropped");
        // A table whose header only repeats a single label is not split.
        let one = read(&format!(
            "<w:tbl>{}{}{}</w:tbl>",
            row(&["Value", "Value"]),
            row(&["1", "2"]),
            row(&["3", "4"])
        ));
        assert_eq!(one.iter().filter(|e| e.kind == "doco:Table").count(), 1);
    }

    #[test]
    fn headers_and_footers_are_read_once_as_what_they_are() {
        use std::io::Write;
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let body = doc(&format!(
            "{}<w:sectPr><w:headerReference w:type=\"default\" r:id=\"rId1\"/>\
             <w:headerReference w:type=\"first\" r:id=\"rId3\"/>\
             <w:footerReference w:type=\"default\" r:id=\"rId2\"/></w:sectPr>",
            para(None, "Body text")
        ));
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, xml) in [
            ("word/document.xml", body),
            (
                "word/_rels/document.xml.rels",
                format!(
                    "<Relationships><Relationship Id=\"rId1\" Type=\"{rel}/header\" Target=\"header1.xml\"/>\
                     <Relationship Id=\"rId3\" Type=\"{rel}/header\" Target=\"header2.xml\"/>\
                     <Relationship Id=\"rId2\" Type=\"{rel}/footer\" Target=\"footer1.xml\"/></Relationships>"
                ),
            ),
            (
                "word/header1.xml",
                format!(
                    "<w:hdr {NS}>{}{}</w:hdr>",
                    para(Some("Heading1"), "Ref: QA-7"),
                    para(None, "Revision 3")
                ),
            ),
            // A first page's header, which no title page shows.
            ("word/header2.xml", format!("<w:hdr {NS}>{}</w:hdr>", para(None, "Cover only"))),
            ("word/footer1.xml", format!("<w:ftr {NS}>{}</w:ftr>", para(None, "Uncontrolled when printed"))),
        ] {
            z.start_file(name, opts).unwrap();
            z.write_all(xml.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        let bytes = buf.into_inner();
        let els = parse(&bytes).unwrap();
        let got: Vec<(&str, &str, &str)> = els
            .iter()
            .map(|e| (e.kind.as_str(), e.text.as_str(), e.evidence))
            .collect();
        assert_eq!(
            got,
            [
                ("doco:Paragraph", "Ref: QA-7", "header"),
                ("doco:Paragraph", "Revision 3", "header"),
                ("doco:Paragraph", "Body text", "docx"),
                ("doco:Paragraph", "Uncontrolled when printed", "footer"),
            ]
        );
        let ids: Vec<&str> = els.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            ["elem-00001", "elem-00002", "elem-00003", "elem-00004"]
        );
    }

    #[test]
    fn numbered_titles_in_capitals_are_the_outline() {
        let numbering = numbering_xml(
            &[("0", OUTLINE)],
            "<w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num>",
        );
        let n = Numbering::new(Some(&numbering), None);
        let body = [
            item("1", 0, "OVERVIEW"),
            para(None, "What this covers."),
            item("1", 0, "METHOD"),
            item("1", 1, "PREPARATION and STORAGE"),
            item("1", 2, "Weigh the sample."),
            item("1", 2, "DO NOT PROCEED"),
            item("1", 2, "Record the weight."),
            item("1", 1, "Sample Handling (see appendix)"),
            item("1", 2, "Label each sample."),
            item("1", 1, "Check the label"),
            item("1", 2, "Compare the lot number."),
        ]
        .concat();
        let els = parse_with_styles(&doc(&body), &HashSet::new(), &HashSet::new(), n).unwrap();
        let got: Vec<(&str, Option<usize>, &str)> = els
            .iter()
            .map(|e| (e.kind.as_str(), e.level, e.text.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("doco:SectionTitle", Some(1), "1. OVERVIEW"),
                ("doco:Paragraph", None, "What this covers."),
                ("doco:SectionTitle", Some(1), "2. METHOD"),
                ("doco:SectionTitle", Some(2), "2.1. PREPARATION and STORAGE"),
                ("doc:ListItem", None, "2.1.1. Weigh the sample."),
                // In capitals, but heading nothing: a step.
                ("doc:ListItem", None, "2.1.2. DO NOT PROCEED"),
                ("doc:ListItem", None, "2.1.3. Record the weight."),
                (
                    "doco:SectionTitle",
                    Some(2),
                    "2.2. Sample Handling (see appendix)"
                ),
                ("doc:ListItem", None, "2.2.1. Label each sample."),
                // Heading steps, but worded as one.
                ("doc:ListItem", None, "2.3. Check the label"),
                ("doc:ListItem", None, "2.3.1. Compare the lot number."),
            ]
        );
    }

    #[test]
    fn a_list_in_title_case_is_not_an_outline() {
        let numbering = numbering_xml(
            &[("0", OUTLINE), ("1", &[("lowerLetter", "%1.", "")])],
            "<w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num>\
             <w:num w:numId=\"2\"><w:abstractNumId w:val=\"1\"/></w:num>",
        );
        let n = Numbering::new(Some(&numbering), None);
        let body = [
            para(None, "Deliverables:"),
            // Entries in title case, one with a note under it.
            item("1", 0, "Design Package"),
            item("1", 0, "Test Templates"),
            para(None, "Templates may arrive in stages."),
            item("1", 0, "Rollout Plan"),
            item("1", 0, "Release Notes"),
            // One entry of another list, then a deeper item of the first:
            // levels are not compared across lists.
            item("2", 0, "The Review Board"),
            item("1", 1, "Appendix Notes"),
        ]
        .concat();
        let els = parse_with_styles(&doc(&body), &HashSet::new(), &HashSet::new(), n).unwrap();
        let titles: Vec<&str> = els
            .iter()
            .filter(|e| e.kind == "doco:SectionTitle")
            .map(|e| e.text.as_str())
            .collect();
        assert!(titles.is_empty(), "{titles:?}");
    }

    #[test]
    fn a_title_is_in_capitals_or_title_case() {
        for t in [
            "SCOPE",
            "PUMP CALIBRATION:",
            "SAMPLE HANDLING and STORAGE",
            "REVIEWS - General Requirements",
            "Storage Conditions and Limits",
            "Preparing 100 mL Buffer for Use",
            "Report Approval and Filing (see form 5)",
        ] {
            assert!(titled(t), "{t}");
        }
        for t in [
            "Weigh the sample.",
            "Check the label",
            "Area Supervisor or designee",
            "and Other Items",
            "(see below)",
            "Record the weight; then sign",
        ] {
            assert!(!titled(t), "{t}");
        }
    }

    const OUTLINE: &[Lvl] = &[
        ("decimal", "%1.", ""),
        ("decimal", "%1.%2.", ""),
        ("decimal", "%1.%2.%3.", ""),
    ];

    #[test]
    fn clause_numbers_word_computes_head_their_paragraphs() {
        let nx = numbering_xml(
            &[("0", OUTLINE)],
            r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
        );
        let body = [
            item("1", 0, "Purpose"),
            item("1", 0, "Procedure"),
            item("1", 1, "Calibration"),
            item("1", 2, "Zero the balance."),
            item("1", 2, "Weigh the standard."),
            item("1", 1, "Cleaning"),
            item("1", 2, "Wipe the pan."),
            item("1", 0, "Records"),
            item("1", 1, "Retention"),
        ]
        .concat();
        assert_eq!(
            numbered(&body, &nx, None),
            [
                "1. Purpose",
                "2. Procedure",
                "2.1. Calibration",
                "2.1.1. Zero the balance.",
                "2.1.2. Weigh the standard.",
                "2.2. Cleaning",
                "2.2.1. Wipe the pan.",
                "3. Records",
                "3.1. Retention",
            ]
        );
    }

    #[test]
    fn letters_romans_bullets_and_legal_numbers() {
        let nx = numbering_xml(
            &[
                (
                    "0",
                    &[
                        ("decimal", "%1.", ""),
                        ("lowerLetter", "%2)", ""),
                        ("lowerRoman", "(%3)", ""),
                    ],
                ),
                ("1", &[("bullet", "\u{F0B7}", "")]),
                // Legal numbering: the level shows every number in decimal.
                (
                    "2",
                    &[
                        ("upperRoman", "%1.", ""),
                        ("decimal", "%1.%2", "<w:isLgl/>"),
                    ],
                ),
            ],
            r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
               <w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>
               <w:num w:numId="3"><w:abstractNumId w:val="2"/></w:num>"#,
        );
        let body = [
            item("1", 0, "Scope"),
            item("1", 1, "first"),
            item("1", 1, "second"),
            item("1", 2, "detail"),
            item("1", 2, "more"),
            item("2", 0, "a bullet"),
            item("3", 0, "Article"),
            item("3", 0, "Article"),
            item("3", 1, "Section"),
        ]
        .concat();
        assert_eq!(
            numbered(&body, &nx, None),
            [
                "1. Scope",
                "a) first",
                "b) second",
                "(i) detail",
                "(ii) more",
                "a bullet",
                "I. Article",
                "II. Article",
                "2.1 Section",
            ]
        );
    }

    #[test]
    fn a_style_numbers_its_paragraphs_at_the_level_linked_to_it() {
        // Clause styles carry the list; the levels name the style each is
        // for, as an outline built on heading styles does.
        let nx = numbering_xml(
            &[(
                "0",
                &[
                    ("decimal", "%1.", "<w:pStyle w:val=\"Clause1\"/>"),
                    ("decimal", "%1.%2.", "<w:pStyle w:val=\"Clause2\"/>"),
                ],
            )],
            r#"<w:num w:numId="7"><w:abstractNumId w:val="0"/></w:num>"#,
        );
        let styles = format!(
            "<w:styles {NS}>\
             <w:style w:type=\"paragraph\" w:styleId=\"Clause1\"><w:pPr><w:numPr><w:numId w:val=\"7\"/></w:numPr></w:pPr></w:style>\
             <w:style w:type=\"paragraph\" w:styleId=\"Clause2\"><w:basedOn w:val=\"Clause1\"/></w:style>\
             </w:styles>"
        );
        let body = [
            para(Some("Clause1"), "Responsibilities"),
            para(Some("Clause2"), "Operators"),
            para(Some("Clause2"), "Reviewers"),
            para(Some("Clause1"), "Procedure"),
            para(None, "Plain text is not numbered."),
        ]
        .concat();
        assert_eq!(
            numbered(&body, &nx, Some(&styles)),
            [
                "1. Responsibilities",
                "1.1. Operators",
                "1.2. Reviewers",
                "2. Procedure",
                "Plain text is not numbered.",
            ]
        );
    }

    #[test]
    fn lists_drawn_from_one_numbering_count_on_unless_one_restarts() {
        let nx = numbering_xml(
            &[("0", OUTLINE)],
            r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
               <w:num w:numId="2"><w:abstractNumId w:val="0"/></w:num>
               <w:num w:numId="3"><w:abstractNumId w:val="0"/>
                 <w:lvlOverride w:ilvl="0"><w:startOverride w:val="5"/></w:lvlOverride></w:num>"#,
        );
        let body = [
            item("1", 0, "one"),
            item("2", 0, "carries on"),
            item("3", 0, "starts at five"),
            item("3", 0, "six"),
        ]
        .concat();
        assert_eq!(
            numbered(&body, &nx, None),
            ["1. one", "2. carries on", "5. starts at five", "6. six"]
        );
    }

    #[test]
    fn a_paragraph_taken_out_of_its_list_is_neither_numbered_nor_an_item() {
        let nx = numbering_xml(
            &[("0", OUTLINE)],
            r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
        );
        let body = [
            item("1", 0, "first"),
            item("0", 0, "no longer listed"),
            item("1", 0, "second"),
        ]
        .concat();
        let n = Numbering::new(Some(&nx), None);
        let els = parse_with_styles(&doc(&body), &HashSet::new(), &HashSet::new(), n).unwrap();
        assert_eq!(els[1].text, "no longer listed");
        assert_eq!(els[1].kind, "doco:Paragraph");
        assert_eq!(els[2].text, "2. second");
    }

    #[test]
    fn a_step_column_keeps_its_numbers() {
        // A form's step column: each cell holds an empty numbered
        // paragraph, and Word shows only its number.
        let nx = numbering_xml(
            &[("0", OUTLINE)],
            r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
        );
        let step = |text: &str| {
            format!(
                "<w:tr><w:tc><w:p><w:pPr><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr></w:pPr></w:p></w:tc>\
                 <w:tc><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc></w:tr>"
            )
        };
        let body = format!(
            "<w:tbl>{}{}</w:tbl>{}",
            step("Weigh the sample."),
            step("Label the vial."),
            item("1", 1, "after the table")
        );
        let n = Numbering::new(Some(&nx), None);
        let els = parse_with_styles(&doc(&body), &HashSet::new(), &HashSet::new(), n).unwrap();
        let cells = els[0].cells.as_ref().unwrap();
        assert_eq!(cells[0][0].trim(), "1.");
        assert_eq!(cells[1][0].trim(), "2.");
        assert_eq!(els[1].text, "2.1. after the table");
    }

    #[test]
    fn a_tracked_changes_old_numbering_is_not_the_paragraphs() {
        let nx = numbering_xml(
            &[("0", OUTLINE)],
            r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
        );
        let body = "<w:p><w:pPr><w:pStyle w:val=\"Body\"/><w:pPrChange><w:pPr><w:pStyle w:val=\"Heading1\"/>\
                    <w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr></w:pPr></w:pPrChange></w:pPr>\
                    <w:r><w:t>Was a numbered heading</w:t></w:r></w:p>";
        let n = Numbering::new(Some(&nx), None);
        let els = parse_with_styles(&doc(body), &HashSet::new(), &HashSet::new(), n).unwrap();
        assert_eq!(els[0].text, "Was a numbered heading");
        assert_eq!(els[0].kind, "doco:Paragraph");
    }

    #[test]
    fn the_title_style_is_the_documents_title() {
        let x = doc(&format!(
            "{}{}",
            para(Some("Title"), "Annual Report"),
            para(Some("Heading1"), "Overview")
        ));
        let els = parse_document_xml(&x).unwrap();
        assert_eq!(els[0].kind, "doco:Title");
        assert_eq!(els[0].text, "Annual Report");
        assert_eq!(els[1].kind, "doco:SectionTitle");
        // A localised Word names the style by its own id and keeps the
        // built-in name.
        let styles = format!(
            "<w:styles {NS}><w:style w:type=\"paragraph\" w:styleId=\"Titel\">\
             <w:name w:val=\"Title\"/></w:style></w:styles>"
        );
        let localised = title_styles(&styles);
        let els = parse_with_styles(
            &doc(&para(Some("Titel"), "Jahresbericht")),
            &HashSet::new(),
            &localised,
            Numbering::default(),
        )
        .unwrap();
        assert_eq!(els[0].kind, "doco:Title");
    }

    #[test]
    fn heading_styles_give_their_level_directly() {
        let x = doc(&format!(
            "{}{}{}",
            para(Some("Heading1"), "Title"),
            para(None, "Body text"),
            para(Some("Heading3"), "Deeper")
        ));
        let els = parse_document_xml(&x).unwrap();
        assert_eq!(els.len(), 3);
        assert_eq!(els[0].kind, "doco:SectionTitle");
        assert_eq!(els[0].level, Some(1));
        assert_eq!(els[1].kind, "doco:Paragraph");
        assert_eq!(els[2].level, Some(3));
    }

    #[test]
    fn heading_level_parses_localised_and_odd_styles() {
        assert_eq!(heading_level("Heading2"), Some(2));
        assert_eq!(heading_level("heading4"), Some(4));
        assert_eq!(
            heading_level("Heading9"),
            Some(6),
            "clamped to the emitters"
        );
        assert_eq!(heading_level("Normal"), None);
        assert_eq!(heading_level("HeadingChar"), None);
    }

    #[test]
    fn numbered_paragraphs_are_list_items() {
        let x = doc("<w:p><w:pPr><w:numPr><w:ilvl w:val=\"0\"/></w:numPr></w:pPr><w:r><w:t>one</w:t></w:r></w:p>");
        let els = parse_document_xml(&x).unwrap();
        assert_eq!(els[0].kind, "doc:ListItem");
        assert_eq!(els[0].text, "one");
    }

    #[test]
    fn list_styles_are_list_items_even_without_numbering() {
        // Word's List Bullet style carries no w:numPr, so numbering alone
        // misses it.
        let x = doc(&format!(
            "{}{}",
            para(Some("ListBullet"), "bulleted"),
            para(Some("ListParagraph"), "also a list item")
        ));
        let els = parse_document_xml(&x).unwrap();
        assert_eq!(els[0].kind, "doc:ListItem");
        assert_eq!(els[1].kind, "doc:ListItem");
        assert!(!is_list_style("Normal"));
        assert!(!is_list_style("Heading1"));
    }

    #[test]
    fn nothing_claims_geometry() {
        let x = doc(&para(Some("Heading1"), "Title"));
        let els = parse_document_xml(&x).unwrap();
        assert!(els.iter().all(|e| e.bbox.is_none()));
        assert!(els.iter().all(|e| e.provenance == "docx"));
    }

    #[test]
    fn declared_merges_carry_across() {
        // Row 1: one cell spanning both columns. Row 2: two cells, the first
        // beginning a vertical merge. Row 3: that merge continuing.
        let x = doc(concat!(
            "<w:tbl>",
            "<w:tr><w:tc><w:tcPr><w:gridSpan w:val=\"2\"/></w:tcPr><w:p><w:r><w:t>Banner</w:t></w:r></w:p></w:tc></w:tr>",
            "<w:tr><w:tc><w:tcPr><w:vMerge w:val=\"restart\"/></w:tcPr><w:p><w:r><w:t>Left</w:t></w:r></w:p></w:tc>",
            "<w:tc><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc></w:tr>",
            "<w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p></w:p></w:tc>",
            "<w:tc><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc></w:tr>",
            "</w:tbl>"
        ));
        let els = parse_document_xml(&x).unwrap();
        let t = els.iter().find(|e| e.kind == "doco:Table").unwrap();
        let cells = t.cells.as_ref().unwrap();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0][0], "Banner");
        let ml = t.merged_left.as_ref().expect("gridSpan recorded");
        assert!(ml[1], "the spanned column continues the banner cell");
        let md = t.merged_down.as_ref().expect("vMerge recorded");
        assert!(md[2 * 2], "row 3 column 0 continues the cell above");
        assert!(!md[2 * 2 + 1], "column 1 is its own cell");
    }

    #[test]
    fn a_table_of_plain_cells_needs_no_merge_flags() {
        let x = doc(concat!(
            "<w:tbl>",
            "<w:tr><w:tc><w:p><w:r><w:t>Year</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Total</w:t></w:r></w:p></w:tc></w:tr>",
            "<w:tr><w:tc><w:p><w:r><w:t>2024</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>12</w:t></w:r></w:p></w:tc></w:tr>",
            "</w:tbl>"
        ));
        let t = parse_document_xml(&x)
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "doco:Table")
            .unwrap();
        assert_eq!(t.header_rows, Some(1));
        assert!(t.merged_left.is_none() && t.merged_down.is_none());
        assert_eq!(t.cells.unwrap()[1], vec!["2024", "12"]);
    }

    /// A table cell: text, bold or not, and any `w:tcPr` content.
    fn tc(text: &str, bold: bool, pr: &str) -> String {
        let b = if bold { "<w:rPr><w:b/></w:rPr>" } else { "" };
        format!("<w:tc><w:tcPr>{pr}</w:tcPr><w:p><w:r>{b}<w:t>{text}</w:t></w:r></w:p></w:tc>")
    }

    fn table(rows: &[(&str, Vec<String>)]) -> Element {
        let body: String = rows
            .iter()
            .map(|(pr, cells)| format!("<w:tr><w:trPr>{pr}</w:trPr>{}</w:tr>", cells.concat()))
            .collect();
        parse_document_xml(&doc(&format!("<w:tbl>{body}</w:tbl>")))
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "doco:Table")
            .unwrap()
    }

    #[test]
    fn a_key_value_table_has_no_header_row() {
        // A bold label column beside amounts: the first row is data.
        let kv = table(&[
            ("", vec![tc("Subtotal", true, ""), tc("38.60", false, "")]),
            ("", vec![tc("Tax", true, ""), tc("3.86", false, "")]),
            ("", vec![tc("Total", true, ""), tc("42.46", false, "")]),
        ]);
        assert_eq!(kv.header_rows, Some(0));
        // Labels over a column of numbers are a header without any mark.
        let lines = table(&[
            (
                "",
                vec![
                    tc("Item", false, ""),
                    tc("Qty", false, ""),
                    tc("Unit price", false, ""),
                ],
            ),
            (
                "",
                vec![
                    tc("Fish pie", false, ""),
                    tc("2", false, ""),
                    tc("14.50", false, ""),
                ],
            ),
            (
                "",
                vec![
                    tc("Lemonade", false, ""),
                    tc("3", false, ""),
                    tc("3.20", false, ""),
                ],
            ),
        ]);
        assert_eq!(lines.header_rows, Some(1));
        // Text over text has a header only where the document marks one:
        // bold over plain, or w:tblHeader.
        let plain = |first_bold: bool, pr: &'static str| {
            table(&[
                (
                    pr,
                    vec![tc("Name", first_bold, ""), tc("Role", first_bold, "")],
                ),
                ("", vec![tc("Ada", false, ""), tc("Engineer", false, "")]),
            ])
            .header_rows
        };
        assert_eq!(plain(false, ""), Some(0));
        // Three columns of labels over text name their columns.
        let wide = table(&[
            (
                "",
                vec![
                    tc("Activity", false, ""),
                    tc("Marketing", false, ""),
                    tc("Contracts", false, ""),
                ],
            ),
            (
                "",
                vec![
                    tc("Order approval", false, ""),
                    tc("R", false, ""),
                    tc("I", false, ""),
                ],
            ),
        ]);
        assert_eq!(wide.header_rows, Some(1));
        assert_eq!(plain(true, ""), Some(1));
        assert_eq!(plain(false, "<w:tblHeader/>"), Some(1));
        assert_eq!(plain(false, "<w:tblHeader w:val=\"0\"/>"), Some(0));
    }

    #[test]
    fn a_field_list_has_no_header_row() {
        // A bold label column, the values plain, no header marked.
        let fields = table(&[
            (
                "",
                vec![tc("Description", true, ""), tc("Cinnamon bun", false, "")],
            ),
            ("", vec![tc("Code", true, ""), tc("BK-215", false, "")]),
            ("", vec![tc("Quantity", true, ""), tc("1", false, "")]),
            ("", vec![tc("Price each", true, ""), tc("£7.90", false, "")]),
            ("", vec![tc("Total paid", true, ""), tc("£9.00", false, "")]),
        ]);
        assert_eq!(fields.header_rows, Some(0));
        // The same without the bold: values of mixed type are fields.
        let plain = table(&[
            (
                "",
                vec![tc("Description", false, ""), tc("Cinnamon bun", false, "")],
            ),
            ("", vec![tc("Code", false, ""), tc("BK-215", false, "")]),
            ("", vec![tc("Quantity", false, ""), tc("1", false, "")]),
            (
                "",
                vec![tc("Total paid", false, ""), tc("£9.00", false, "")],
            ),
        ]);
        assert_eq!(plain.header_rows, Some(0));
        // A bold header over a bold first column is still a header.
        let lines = table(&[
            ("", vec![tc("Item", true, ""), tc("Amount", true, "")]),
            ("", vec![tc("Fish pie", true, ""), tc("29.00", false, "")]),
            ("", vec![tc("Lemonade", true, ""), tc("9.60", false, "")]),
        ]);
        assert_eq!(lines.header_rows, Some(1));
        // And two columns of amounts under labels need no bold.
        let lines = table(&[
            ("", vec![tc("Item", false, ""), tc("Amount", false, "")]),
            ("", vec![tc("Fish pie", false, ""), tc("29.00", false, "")]),
            ("", vec![tc("Lemonade", false, ""), tc("9.60", false, "")]),
        ]);
        assert_eq!(lines.header_rows, Some(1));
    }

    #[test]
    fn a_header_merged_down_into_a_second_row_is_a_stacked_header() {
        let restart = "<w:vMerge w:val=\"restart\"/>";
        let t = table(&[
            (
                "",
                vec![
                    tc("Item", true, restart),
                    tc("Qty", true, restart),
                    tc("Price", true, "<w:gridSpan w:val=\"2\"/>"),
                ],
            ),
            (
                "",
                vec![
                    tc("", false, "<w:vMerge/>"),
                    tc("", false, "<w:vMerge/>"),
                    tc("Unit", true, ""),
                    tc("Line", true, ""),
                ],
            ),
            (
                "",
                vec![
                    tc("Fish pie", false, ""),
                    tc("2", false, ""),
                    tc("14.50", false, ""),
                    tc("29.00", false, ""),
                ],
            ),
        ]);
        assert_eq!(t.header_rows, Some(2));
    }

    #[test]
    fn a_row_of_one_cell_across_the_table_is_a_section_band() {
        let band = |text: &str| tc(text, false, "<w:gridSpan w:val=\"2\"/>");
        let t = table(&[
            ("", vec![tc("Item", true, ""), tc("Price", true, "")]),
            ("", vec![band("Food")]),
            ("", vec![tc("Fish pie", false, ""), tc("14.50", false, "")]),
            ("", vec![tc("Bread", false, ""), tc("", false, "")]),
            ("", vec![band("Thank you")]),
        ]);
        assert_eq!(t.header_rows, Some(1));
        // A row missing a value is not a band, and the last row labels
        // nothing.
        assert_eq!(t.sub_headers, Some(vec![1]));
    }

    #[test]
    fn a_table_style_that_sets_its_first_row_apart_names_a_header() {
        let styles = format!(
            "<w:styles {NS}>\
             <w:style w:type=\"table\" w:styleId=\"TableGrid\"><w:tblPr/></w:style>\
             <w:style w:type=\"table\" w:styleId=\"LightShading\"><w:tblStylePr w:type=\"firstRow\"><w:rPr><w:b/></w:rPr></w:tblStylePr>\
             <w:tblStylePr w:type=\"firstCol\"><w:rPr><w:b/></w:rPr></w:tblStylePr></w:style>\
             <w:style w:type=\"table\" w:styleId=\"Mine\"><w:basedOn w:val=\"LightShading\"/></w:style>\
             <w:style w:type=\"table\" w:styleId=\"ColOnly\"><w:tblStylePr w:type=\"firstCol\"><w:rPr><w:b/></w:rPr></w:tblStylePr></w:style>\
             </w:styles>"
        );
        let marked = header_row_styles(&styles);
        assert!(marked.contains("LightShading") && marked.contains("Mine"));
        assert!(!marked.contains("TableGrid") && !marked.contains("ColOnly"));
        let x = |style: &str, look: &str| {
            doc(&format!(
                "<w:tbl><w:tblPr><w:tblStyle w:val=\"{style}\"/><w:tblLook {look}/></w:tblPr>\
                 <w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>",
                tc("Name", false, ""),
                tc("Role", false, ""),
                tc("Ada", false, ""),
                tc("Engineer", false, "")
            ))
        };
        let header = |xml: String| {
            parse_with_styles(&xml, &marked, &HashSet::new(), Numbering::default()).unwrap()[0]
                .header_rows
        };
        assert_eq!(header(x("Mine", "w:firstRow=\"1\"")), Some(1));
        assert_eq!(header(x("Mine", "w:val=\"04A0\"")), Some(1));
        assert_eq!(header(x("Mine", "w:firstRow=\"0\"")), Some(0));
        assert_eq!(header(x("TableGrid", "w:firstRow=\"1\"")), Some(0));
        // The style shades whatever row is first; a key/value table's first
        // row is typed like its data and stays data.
        let kv = doc(&format!(
            "<w:tbl><w:tblPr><w:tblStyle w:val=\"Mine\"/><w:tblLook w:firstRow=\"1\"/></w:tblPr>\
             <w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>",
            tc("Subtotal", false, ""),
            tc("38.60", false, ""),
            tc("Total", false, ""),
            tc("42.46", false, "")
        ));
        assert_eq!(header(kv), Some(0));
    }

    #[test]
    fn runs_within_a_paragraph_join_without_spurious_breaks() {
        let x = doc("<w:p><w:r><w:t>Hello </w:t></w:r><w:r><w:t>world</w:t></w:r></w:p>");
        let els = parse_document_xml(&x).unwrap();
        assert_eq!(els.len(), 1);
        assert_eq!(els[0].text, "Hello world");
    }

    #[test]
    fn a_missing_document_part_is_an_error_not_a_panic() {
        let empty: &[u8] = b"not a zip";
        assert!(parse(empty).is_err());
    }
}
