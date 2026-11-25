//! XPS and OpenXPS documents as positioned glyphs and images.
//!
//! An XPS file is an Open Packaging Conventions package, as an Office file
//! is: a zip of parts tied by relationships. The package names a fixed
//! document sequence, which lists fixed documents, which list their pages;
//! each page (`FixedPage`) is XAML that places text and pictures at
//! absolute positions. That is a PDF page's content, stated rather than
//! drawn: a `Glyphs` element is a run of text with its origin, its size and
//! each character's advance, and a `Path` filled with an `ImageBrush` is a
//! picture where it is placed. So this crate reads a page into the PDF
//! engine's own [`Document`] — glyphs with boxes, image placements — and the
//! PDF layout pipeline does the rest: lines, blocks, columns, headings,
//! tables, furniture, and the router, which sends a page that is only a
//! picture (a screen printed to XPS) to a reader as it would a scan.
//!
//! **Units.** XPS measures in 1/96 inch, y down; a PDF unit is 1/72 inch,
//! and the engine's frame is y down too, so every position is scaled by
//! 0.75 and nothing is flipped.
//!
//! **What is approximate.** A glyph's box is its advance across, and 0.8 em
//! above and 0.2 em below its baseline: the font's own metrics are in an
//! embedded (often obfuscated) font this does not read. A character whose
//! advance the `Indices` attribute does not give is taken as half an em
//! (a space as a quarter), and no advance is taken as more than
//! [`MAX_ADVANCE_EMS`] ems. Vector drawing (`Path` strokes and fills) is not
//! read, so a table ruled with lines is found as an unruled one is. Text
//! set right to left, and content split into interleaved pieces, are not
//! read as such.
//!
//! **Bounds**, as for any archive: each part at most [`MAX_PART_BYTES`]
//! uncompressed and all parts read at most [`MAX_TOTAL_BYTES`], checked as
//! they inflate; at most [`MAX_PAGES`] pages, [`MAX_PAGE_GLYPHS`] glyphs on
//! a page and [`MAX_GLYPHS`] in all (counted before a run is laid out, so a
//! run past the bound is refused before its glyphs are made) and [`MAX_DEPTH`] nested elements on a page. A page is
//! at most [`MAX_PAGE_SIZE`] units across; a run drawn larger than its page
//! is not text anyone reads and is left out, as is a glyph drawn wholly off
//! its page, so no coordinate reaching the layout pipeline is beyond a
//! page's reach.

use fluree_doc_model::budget::scaled;
use fluree_doc_model::DocumentInfo;
use fluree_doc_ooxml::{attr, local, Package};
use fluree_doc_pdf::geom::BBox;
use fluree_doc_pdf::{Document, Glyph, ImagePlacement, Page};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

/// The most bytes one part is inflated to: 64 MiB.
///
/// This bound, [`MAX_TOTAL_BYTES`], [`MAX_GLYPHS`] and [`MAX_PAGE_GLYPHS`]
/// are the memory an XPS document may take, which the document's budget
/// does not count; built for the fuzz targets, each is a sixteenth, as the
/// budget is ([`scaled`]).
pub const MAX_PART_BYTES: u64 = scaled(64 << 20);
/// The most bytes all the parts read are inflated to: 512 MiB.
pub const MAX_TOTAL_BYTES: u64 = scaled(512 << 20);
/// The most pages read.
pub const MAX_PAGES: usize = 10_000;
/// The most glyphs, and pictures, read from one document: four million.
///
/// The pages are laid out by the PDF pipeline, all held at once. Spread over
/// pages, four million glyphs were measured to peak at 1.5 GB (about 370
/// bytes a glyph); piled on few pages, at about 530 bytes a glyph, 2.1 GB:
/// within a 4 GB address space either way. A dense printed page holds a
/// few thousand, so a document of more than about 1,600 such pages is
/// refused whole.
pub const MAX_GLYPHS: usize = scaled(4_000_000) as usize;
/// The most glyphs, and pictures, read from one page: 250,000, fifty times
/// a dense page. The pipeline's passes over a page cost more than its
/// glyphs' number, a little: a page of 250,000 glyphs was measured to take
/// 0.3 s, one of two million 4.4 s.
pub const MAX_PAGE_GLYPHS: usize = scaled(250_000) as usize;
/// The deepest nesting of a page's elements followed.
pub const MAX_DEPTH: usize = 256;
/// The most parts a package may hold.
pub const MAX_PARTS: usize = 100_000;
/// The largest page side read, in PDF units: a PDF's own limit, 200 inches.
pub const MAX_PAGE_SIZE: f64 = 14_400.0;
/// The largest advance one character is taken to have, in ems.
pub const MAX_ADVANCE_EMS: f64 = 10.0;

/// XPS units (1/96 inch) to PDF units (1/72 inch).
const UNIT: f64 = 0.75;

#[derive(Debug)]
pub enum XpsError {
    /// Not a zip, or a broken one.
    Zip(String),
    /// A package with no fixed document sequence: not an XPS document.
    NotXps,
    /// Past one of the bounds.
    TooLarge(String),
}

impl std::fmt::Display for XpsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zip(e) => write!(f, "not a readable XPS package: {e}"),
            Self::NotXps => write!(
                f,
                "not an XPS document: the package names no fixed document sequence"
            ),
            Self::TooLarge(e) => write!(f, "XPS document too large: {e}"),
        }
    }
}

impl std::error::Error for XpsError {}

/// A picture a page places, for an OCR engine or a model to read.
#[derive(Debug, Clone, PartialEq)]
pub struct PageImage {
    /// 0-based page.
    pub page: usize,
    /// The package part holding it ([`read_image`]).
    pub part: String,
    /// Where it is drawn, in PDF units, y down.
    pub bbox: BBox,
}

/// An XPS document read: its pages as the PDF engine's document, the
/// pictures they place, and what the package declares about itself.
#[derive(Clone)]
pub struct Xps {
    pub doc: Document,
    pub images: Vec<PageImage>,
    pub info: DocumentInfo,
}

impl std::fmt::Debug for Xps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Xps")
            .field("pages", &self.doc.pages.len())
            .field("images", &self.images)
            .field("info", &self.info)
            .finish()
    }
}

impl Xps {
    /// The largest picture on `page`, the one a page that is only a
    /// picture is read through.
    pub fn page_image(&self, page: usize) -> Option<&PageImage> {
        self.images
            .iter()
            .filter(|i| i.page == page)
            .max_by(|a, b| area(&a.bbox).total_cmp(&area(&b.bbox)))
    }
}

fn area(b: &BBox) -> f64 {
    b.width().max(0.0) * b.height().max(0.0)
}

/// An OCR engine's reading of a picture, or of part of one, in its pixels,
/// as words on the page where it is drawn (`drawn`)
/// ([`fluree_doc_pdf::ocr::Coords::Page`]): what
/// `fluree_doc_pdf::ocr::inject` takes for the page. `width` and
/// `height` are the size of the image read; a reading already in page
/// units is returned as it is.
pub fn reading_on_page(
    mut reading: fluree_doc_pdf::ocr::WordReading,
    drawn: &BBox,
    width: u32,
    height: u32,
) -> fluree_doc_pdf::ocr::WordReading {
    use fluree_doc_pdf::ocr::Coords;
    if reading.coords == Coords::Page || width == 0 || height == 0 {
        return reading;
    }
    let b = *drawn;
    let (sx, sy) = (b.width() / f64::from(width), b.height() / f64::from(height));
    for w in &mut reading.words {
        let [x0, y0, x1, y1] = w.bbox;
        w.bbox = [
            b.x0 + x0 * sx,
            b.y0 + y0 * sy,
            b.x0 + x1 * sx,
            b.y0 + y1 * sy,
        ];
    }
    reading.coords = Coords::Page;
    (reading.width, reading.height) = (None, None);
    reading
}

/// Whether `bytes` is an XPS or OpenXPS package: a zip whose root
/// relationships name a fixed document sequence.
pub fn is_xps(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK")
        && Package::open(bytes).is_ok_and(|mut p| p.root("fixedrepresentation").is_some())
}

/// A picture's bytes, at most [`MAX_PART_BYTES`].
pub fn read_image(bytes: &[u8], part: &str) -> Result<Vec<u8>, String> {
    Package::open(bytes)?.read_bytes(part, MAX_PART_BYTES)
}

/// Read an XPS document's pages.
pub fn parse(bytes: &[u8]) -> Result<Xps, XpsError> {
    let mut pkg = Package::open(bytes).map_err(XpsError::Zip)?;
    if pkg.part_names().len() > MAX_PARTS {
        return Err(XpsError::TooLarge(format!("more than {MAX_PARTS} parts")));
    }
    let seq = pkg.root("fixedrepresentation").ok_or(XpsError::NotXps)?;
    let mut budget = MAX_TOTAL_BYTES;
    let mut read = |pkg: &mut Package<'_>, part: &str| -> Result<Option<String>, XpsError> {
        match pkg.read_bytes(part, MAX_PART_BYTES.min(budget)) {
            Ok(b) => {
                budget -= b.len() as u64;
                Ok(Some(decode(&b)))
            }
            Err(e) if e.contains("more than") => Err(XpsError::TooLarge(e)),
            Err(_) => Ok(None),
        }
    };
    let seq_xml = read(&mut pkg, &seq)?.ok_or(XpsError::NotXps)?;
    let mut page_parts = Vec::new();
    for doc in sources(&seq_xml, "DocumentReference", &seq) {
        let Some(doc_xml) = read(&mut pkg, &doc)? else {
            continue;
        };
        page_parts.extend(sources(&doc_xml, "PageContent", &doc));
        if page_parts.len() > MAX_PAGES {
            return Err(XpsError::TooLarge(format!("more than {MAX_PAGES} pages")));
        }
    }
    let mut pages = Vec::new();
    let mut images = Vec::new();
    let mut glyphs = 0usize;
    for part in page_parts {
        let index = pages.len();
        // A page the package does not hold is an empty page, so the pages
        // after it keep their numbers.
        let xml = read(&mut pkg, &part)?.unwrap_or_default();
        let room = MAX_PAGE_GLYPHS.min(MAX_GLYPHS - glyphs);
        let (page, imgs) = page(&xml, &part, index, room)?;
        glyphs += page.glyphs.len();
        pages.push(page);
        images.extend(imgs);
    }
    Ok(Xps {
        doc: Document { pages },
        images,
        info: pkg.info(),
    })
}

/// A part's text: UTF-16 by its byte-order mark, else UTF-8.
fn decode(bytes: &[u8]) -> String {
    match encoding_rs::Encoding::for_bom(bytes) {
        Some((enc, bom)) => enc
            .decode_without_bom_handling(&bytes[bom..])
            .0
            .into_owned(),
        None => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// The parts the `Source` attributes of `element`s in `xml` name, resolved
/// against the directory of `part`.
fn sources(xml: &str, element: &str, part: &str) -> Vec<String> {
    let dir = part.rsplit_once('/').map_or("", |(d, _)| d);
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    loop {
        match r.read_event_into(&mut buf) {
            Err(_) | Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(e.name().as_ref()) == element => {
                if let Some(src) = attr(&e, "Source") {
                    out.push(fluree_doc_ooxml::resolve_part(dir, &src));
                }
            }
            _ => {}
        }
        buf.clear();
    }
    out
}

/// An affine transform as XAML writes one, `m11,m12,m21,m22,dx,dy`: a point
/// `(x, y)` goes to `(x·m11 + y·m21 + dx, x·m12 + y·m22 + dy)`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Matrix([f64; 6]);

impl Matrix {
    const IDENTITY: Matrix = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    fn parse(s: &str) -> Option<Matrix> {
        let v: Vec<f64> = s
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .map(|t| t.parse().ok())
            .collect::<Option<_>>()?;
        let m: [f64; 6] = v.try_into().ok()?;
        m.iter().all(|x| x.is_finite()).then_some(Matrix(m))
    }

    fn apply(&self, (x, y): (f64, f64)) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.0;
        (x * a + y * c + e, x * b + y * d + f)
    }

    /// This transform, then `outer`.
    fn then(&self, outer: &Matrix) -> Matrix {
        let [a, b, c, d, e, f] = self.0;
        let [p, q, r, s, t, u] = outer.0;
        Matrix([
            a * p + b * r,
            a * q + b * s,
            c * p + d * r,
            c * q + d * s,
            e * p + f * r + t,
            e * q + f * s + u,
        ])
    }

    /// How much it scales a length, on average over its axes.
    fn scale(&self) -> f64 {
        let [a, b, c, d, _, _] = self.0;
        (a * d - b * c).abs().sqrt()
    }
}

/// An element open on a page, with the transform it sets for what it holds.
struct Frame {
    name: String,
    matrix: Matrix,
    /// A `Glyphs` or `Path` waiting for its end, whose own transform its
    /// property elements (`Glyphs.RenderTransform`) may still set.
    pending: Option<Pending>,
}

enum Pending {
    Glyphs(GlyphsRun),
    Path(PathFill),
}

/// A `Glyphs` element's attributes.
#[derive(Default)]
struct GlyphsRun {
    origin: (f64, f64),
    em: f64,
    text: String,
    indices: Option<String>,
    bold: bool,
}

/// A `Path` filled with a picture.
#[derive(Default)]
struct PathFill {
    data_box: Option<(f64, f64, f64, f64)>,
    image: Option<String>,
    viewport: Option<(f64, f64, f64, f64)>,
}

/// Read one `FixedPage`: its glyphs and image placements, and the pictures
/// it places by part; at most `room` glyphs, refused past them.
fn page(
    xml: &str,
    part: &str,
    index: usize,
    room: usize,
) -> Result<(Page, Vec<PageImage>), XpsError> {
    let dir = part.rsplit_once('/').map_or("", |(d, _)| d);
    let base = Matrix([UNIT, 0.0, 0.0, UNIT, 0.0, 0.0]);
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let (mut width, mut height) = (816.0 * UNIT, 1056.0 * UNIT);
    let mut glyphs: Vec<Glyph> = Vec::new();
    let mut images: Vec<PageImage> = Vec::new();
    let mut placements: Vec<ImagePlacement> = Vec::new();
    let mut room = room;
    loop {
        let event = r.read_event_into(&mut buf);
        let (e, empty) = match event {
            Err(_) | Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => (e.into_owned(), false),
            Ok(Event::Empty(e)) => (e.into_owned(), true),
            Ok(Event::End(_)) => {
                if let Some(frame) = stack.pop() {
                    let size = (width, height);
                    finish(
                        frame,
                        size,
                        &mut room,
                        &mut glyphs,
                        &mut images,
                        &mut placements,
                        index,
                    )?;
                }
                buf.clear();
                continue;
            }
            _ => {
                buf.clear();
                continue;
            }
        };
        buf.clear();
        if stack.len() >= MAX_DEPTH {
            return Err(XpsError::TooLarge(format!(
                "elements nested more than {MAX_DEPTH} deep"
            )));
        }
        let name = local(e.name().as_ref()).to_string();
        let parent = stack.last().map_or(base, |f| f.matrix);
        let own = attr(&e, "RenderTransform")
            .and_then(|m| Matrix::parse(&m))
            .unwrap_or(Matrix::IDENTITY);
        let mut frame = Frame {
            name: name.clone(),
            matrix: own.then(&parent),
            pending: None,
        };
        match name.as_str() {
            "FixedPage" => {
                let size = |k| attr(&e, k).and_then(|v| v.parse::<f64>().ok());
                if let (Some(w), Some(h)) = (size("Width"), size("Height")) {
                    if w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite() {
                        (width, height) =
                            ((w * UNIT).min(MAX_PAGE_SIZE), (h * UNIT).min(MAX_PAGE_SIZE));
                    }
                }
            }
            "Glyphs" => frame.pending = Some(Pending::Glyphs(glyphs_run(&e))),
            "Path" => {
                let mut fill = PathFill {
                    data_box: attr(&e, "Data").and_then(|d| bounds(&d)),
                    ..Default::default()
                };
                fill.image = None;
                frame.pending = Some(Pending::Path(fill));
            }
            // A property element's transform is its owner's: the owner is
            // the frame below it, and what it holds comes after it.
            "MatrixTransform" => {
                if let Some(m) = attr(&e, "Matrix").and_then(|m| Matrix::parse(&m)) {
                    let owner_is = |f: &Frame, prop: &str| f.name.ends_with(prop);
                    let n = stack.len();
                    if n >= 2 && owner_is(&stack[n - 1], ".RenderTransform") {
                        let grand = if n >= 3 { stack[n - 3].matrix } else { base };
                        stack[n - 2].matrix = m.then(&grand);
                    }
                }
            }
            "ImageBrush" => {
                let source = attr(&e, "ImageSource").and_then(|s| image_source(&s));
                let viewport = attr(&e, "Viewport").and_then(|v| rect(&v));
                // The path this brush fills: the nearest open one.
                if let Some(Pending::Path(fill)) = stack
                    .iter_mut()
                    .rev()
                    .find_map(|f| f.pending.as_mut().filter(|p| matches!(p, Pending::Path(_))))
                {
                    fill.image = source.map(|s| fluree_doc_ooxml::resolve_part(dir, &s));
                    fill.viewport = viewport;
                }
            }
            _ => {}
        }
        if empty {
            let size = (width, height);
            finish(
                frame,
                size,
                &mut room,
                &mut glyphs,
                &mut images,
                &mut placements,
                index,
            )?;
        } else {
            stack.push(frame);
        }
    }
    while let Some(frame) = stack.pop() {
        let size = (width, height);
        finish(
            frame,
            size,
            &mut room,
            &mut glyphs,
            &mut images,
            &mut placements,
            index,
        )?;
    }
    for (i, g) in glyphs.iter_mut().enumerate() {
        g.draw_index = i;
    }
    let page_box = BBox {
        x0: 0.0,
        y0: 0.0,
        x1: width,
        y1: height,
    };
    // Clipped to the page, as the PDF engine clips what it collects.
    placements.retain_mut(|p| {
        p.bbox = clip(p.bbox, &page_box);
        area(&p.bbox) > 0.0
    });
    for i in &mut images {
        i.bbox = clip(i.bbox, &page_box);
    }
    images.retain(|i| area(&i.bbox) > 0.0);
    Ok((
        Page {
            index,
            glyphs,
            images: placements,
            rules: Vec::new(),
            fills: Vec::new(),
            // A page's drawn bullets come from its paths' fills, which XPS
            // pages are not read for.
            dots: Vec::new(),
            routing: None,
            width,
            height,
            rotation: 0,
            ocr: None,
            warnings: Vec::new(),
        },
        images,
    ))
}

fn too_many() -> XpsError {
    XpsError::TooLarge(format!(
        "more than {MAX_PAGE_GLYPHS} glyphs and pictures on a page, or {MAX_GLYPHS} in all"
    ))
}

fn clip(b: BBox, to: &BBox) -> BBox {
    BBox {
        x0: b.x0.max(to.x0),
        y0: b.y0.max(to.y0),
        x1: b.x1.min(to.x1),
        y1: b.y1.min(to.y1),
    }
}

/// Close an element: lay out a `Glyphs` run, or place a `Path`'s picture,
/// on a page `size` across. A run is laid out within the `room` left for
/// glyphs, and refused past it.
#[allow(clippy::too_many_arguments)]
fn finish(
    frame: Frame,
    size: (f64, f64),
    room: &mut usize,
    glyphs: &mut Vec<Glyph>,
    images: &mut Vec<PageImage>,
    placements: &mut Vec<ImagePlacement>,
    page: usize,
) -> Result<(), XpsError> {
    match frame.pending {
        Some(Pending::Glyphs(run)) => {
            let before = glyphs.len();
            lay_out(&run, &frame.matrix, page, size, *room, glyphs)?;
            *room -= glyphs.len() - before;
        }
        Some(Pending::Path(fill)) => {
            let Some(part) = fill.image else {
                return Ok(());
            };
            // The picture fills the path; its viewport says where it is
            // drawn, and the path's outline bounds it.
            let (x0, y0, x1, y1) = match (fill.viewport, fill.data_box) {
                (Some(v), Some(d)) => (v.0.max(d.0), v.1.max(d.1), v.2.min(d.2), v.3.min(d.3)),
                (Some(v), None) => v,
                (None, Some(d)) => d,
                (None, None) => return Ok(()),
            };
            let m = frame.matrix;
            let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|p| m.apply(p));
            let bbox = BBox {
                x0: corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min),
                y0: corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min),
                x1: corners
                    .iter()
                    .map(|c| c.0)
                    .fold(f64::NEG_INFINITY, f64::max),
                y1: corners
                    .iter()
                    .map(|c| c.1)
                    .fold(f64::NEG_INFINITY, f64::max),
            };
            if area(&bbox).is_nan() || area(&bbox) <= 0.0 {
                return Ok(());
            }
            *room = room.checked_sub(1).ok_or_else(too_many)?;
            placements.push(ImagePlacement {
                bbox,
                // Not decoded here: whether it holds text is for a reader
                // to find out, so the router treats it as though it may.
                texty: Some(true),
                ppi: None,
                bitonal: false,
                // Nor its pixels counted: taken as enough for a line of
                // text, and as an image of its own, drawn nowhere else.
                grid: (u32::MAX, u32::MAX),
                key: None,
            });
            images.push(PageImage { page, part, bbox });
        }
        None => {}
    }
    Ok(())
}

/// A `Glyphs` element's attributes.
fn glyphs_run(e: &BytesStart<'_>) -> GlyphsRun {
    let num = |k| attr(e, k).and_then(|v| v.trim().parse::<f64>().ok());
    let text = attr(e, "UnicodeString").unwrap_or_default();
    // `{}` opens a string that itself starts with `{`.
    let text = text.strip_prefix("{}").map(str::to_string).unwrap_or(text);
    GlyphsRun {
        origin: (num("OriginX").unwrap_or(0.0), num("OriginY").unwrap_or(0.0)),
        em: num("FontRenderingEmSize").unwrap_or(0.0),
        text,
        indices: attr(e, "Indices"),
        bold: attr(e, "StyleSimulations").is_some_and(|s| s.contains("Bold")),
    }
}

/// Each character's advance, in ems, from an `Indices` attribute:
/// `;`-separated entries `[(chars:glyphs)]index[,advance[,…]]`, the
/// advance in hundredths of an em. A cluster's advances are shared out
/// over its characters. A character no entry covers, or whose entry gives
/// no advance, takes `None`.
fn advances(indices: Option<&str>, chars: usize) -> Vec<Option<f64>> {
    let mut out: Vec<Option<f64>> = Vec::with_capacity(chars);
    let Some(indices) = indices else {
        return vec![None; chars];
    };
    let mut entries = indices.split(';').peekable();
    while out.len() < chars {
        let Some(entry) = entries.next() else {
            break;
        };
        let (cluster, rest) = match entry.trim().strip_prefix('(') {
            Some(r) => match r.split_once(')') {
                Some((c, rest)) => {
                    let (n, g) = c.split_once(':').unwrap_or((c, "1"));
                    let n = n.trim().parse::<usize>().unwrap_or(1).max(1);
                    let g = g.trim().parse::<usize>().unwrap_or(1).max(1);
                    (Some((n, g)), rest)
                }
                None => (None, entry),
            },
            None => (None, entry),
        };
        let advance = |e: &str| {
            e.split(',')
                .nth(1)
                .and_then(|a| a.trim().parse::<f64>().ok())
        };
        match cluster {
            Some((n, g)) => {
                // The counts are the file's: a cluster gives advances to at
                // most the characters left, and takes at most the entries
                // left, so nothing is made or walked past either.
                let mut total = advance(rest);
                for _ in 1..g {
                    let Some(next) = entries.next() else {
                        total = None;
                        break;
                    };
                    total = match (total, advance(next)) {
                        (Some(a), Some(b)) => Some(a + b),
                        _ => None,
                    };
                }
                let each = total.map(|t| t / 100.0 / n as f64);
                out.extend(std::iter::repeat_n(each, n.min(chars - out.len())));
            }
            None => out.push(advance(rest).map(|a| a / 100.0)),
        }
    }
    out.resize(chars, None);
    out
}

/// A run's characters as glyphs, along its baseline, on a page `size`
/// across: at most `room` of them, the run refused past it. A run drawn
/// larger than its page is left out, and so is a glyph drawn wholly off it.
fn lay_out(
    run: &GlyphsRun,
    m: &Matrix,
    page: usize,
    size: (f64, f64),
    room: usize,
    out: &mut Vec<Glyph>,
) -> Result<(), XpsError> {
    if run.em <= 0.0 || !run.em.is_finite() || run.text.is_empty() {
        return Ok(());
    }
    let scale = m.scale();
    // Its size as drawn: past the page's larger side (or not a number, a
    // transform that overflowed), no reader sees it as text.
    let drawn = run.em * scale;
    if drawn.is_nan() || drawn > size.0.max(size.1) {
        return Ok(());
    }
    // Counted before anything is made of it.
    let count = run.text.chars().count();
    if count > room {
        return Err(too_many());
    }
    let adv = advances(run.indices.as_deref(), count);
    let [a, b, _, _, _, _] = m.0;
    // The baseline's direction on the page, y down: counter-clockwise from
    // horizontal as the page is viewed.
    let rotation = (-b).atan2(a).to_degrees() as f32;
    let on_page = |p: (f64, f64)| {
        // Within a page's breadth of it: a glyph whose box overhangs the
        // edge still counts, one far beyond it does not.
        let (w, h) = size;
        p.0 >= -w && p.0 <= 2.0 * w && p.1 >= -h && p.1 <= 2.0 * h
    };
    let y = run.origin.1;
    let mut next_x = run.origin.0;
    for (c, adv) in run.text.chars().zip(adv) {
        let ems = adv
            .unwrap_or(if c.is_whitespace() { 0.25 } else { 0.5 })
            .clamp(-MAX_ADVANCE_EMS, MAX_ADVANCE_EMS);
        let step = ems * run.em;
        let x = next_x;
        next_x += step;
        let (ox, oy) = m.apply((x, y));
        if !on_page((ox, oy)) {
            continue;
        }
        let bbox = (!c.is_whitespace()).then(|| {
            let pts = [
                (x, y - 0.8 * run.em),
                (x + step, y - 0.8 * run.em),
                (x, y + 0.2 * run.em),
                (x + step, y + 0.2 * run.em),
            ]
            .map(|p| m.apply(p));
            BBox {
                x0: pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min),
                y0: pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min),
                x1: pts.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max),
                y1: pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max),
            }
        });
        out.push(Glyph {
            text: c.to_string(),
            bbox,
            page,
            origin: (ox, oy),
            rotation_deg: rotation,
            font_size: (run.em * scale) as f32,
            advance: Some(step * scale),
            weight: run.bold.then_some(700),
            draw_index: 0,
        });
    }
    Ok(())
}

/// The bounding box of a path's data: every coordinate pair it names. Good
/// enough for the rectangles pictures are drawn in.
fn bounds(data: &str) -> Option<(f64, f64, f64, f64)> {
    let nums = data
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e' || c == 'E'))
        .filter(|t| !t.is_empty() && *t != "e" && *t != "E")
        .filter_map(|t| t.parse::<f64>().ok());
    // Folded as they come, pairs only: a path's data is as long as its part.
    let mut b = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    let (mut n, mut x) = (0usize, 0.0);
    for v in nums {
        if n % 2 == 0 {
            x = v;
        } else {
            b = (b.0.min(x), b.1.min(v), b.2.max(x), b.3.max(v));
        }
        n += 1;
    }
    (n >= 4).then_some(b)
}

/// `x,y,w,h` as `(x0, y0, x1, y1)`.
fn rect(s: &str) -> Option<(f64, f64, f64, f64)> {
    let v: Vec<f64> = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .map(|t| t.parse().ok())
        .collect::<Option<_>>()?;
    let [x, y, w, h]: [f64; 4] = v.try_into().ok()?;
    (w > 0.0 && h > 0.0).then_some((x, y, x + w, y + h))
}

/// The part an `ImageSource` names: a URI, or `{ColorConvertedBitmap image
/// profile}`, whose first operand is the image.
fn image_source(s: &str) -> Option<String> {
    let s = s.trim();
    let s = match s.strip_prefix('{') {
        Some(inner) => inner.trim_end_matches('}').split_whitespace().nth(1)?,
        None => s,
    };
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(test)]
mod tests;
