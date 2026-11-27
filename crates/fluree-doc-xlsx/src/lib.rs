//! XLSX (OOXML) to DoCO-typed document elements.
//!
//! A workbook declares its geometry and nothing else. Every cell has an exact
//! row and column — the one thing the PDF engine has to measure — and no
//! cell says what it is. A sheet is a grid on which someone laid out a
//! title, a few labelled fields, a table and a note, and reading it as one
//! table per sheet returns a mostly-empty rectangle with the structure
//! flattened out of it. So this reader measures after all, but only shape:
//! which cells sit together, and what a row of one cell above a block of
//! many is.
//!
//! * **A sheet is a page.** `page` carries the sheet's index in the
//!   workbook's own order, and the sheet's name opens it as a level-1
//!   heading.
//! * **Islands are elements.** Occupied cells that touch along an edge form
//!   an island; an empty row or column between two blocks separates them,
//!   which is how a spreadsheet's author separates them. Each island is a
//!   table, except that a single cell alone on its row at the top is the
//!   island's title and one at the bottom is its note.
//! * **Merges are read as Excel shows them.** A merged range displays its
//!   top-left cell and hides the rest, stale values included; the hidden
//!   cells become `merged_left` / `merged_down` continuations exactly as the
//!   docx reader and the PDF engine express them.
//! * **Values are the cached ones.** A formula's `<v>` is what the file
//!   last computed and what Excel shows; nothing is recalculated. Numbers
//!   render as the shortest decimal that round-trips, dates and times as
//!   ISO where the cell's number format says the number is one, and
//!   percentages as percentages.
//! * **What a cell stores rides beside what it shows.** A table's `datums`
//!   hold each number, date, time and boolean as the file stores it, typed,
//!   at full precision: `0.12345` under a `12%`.
//!
//! * **A picture is a figure where it is anchored.** A picture a sheet's
//!   drawing anchors to its cells (`xl/drawings`, its image in `xl/media`)
//!   is a `doco:Figure` placed among the sheet's elements by the cell its
//!   top-left corner is anchored to, as an island is by its first cell. Its
//!   images are listed ([`parse_with_media`]) for an OCR engine or a model
//!   to read, and their text placed after the figure.
//!
//! No `bbox`: a cell address is not a position on a page, and a consumer
//! wanting the address has the table's row and column.
//!
//! **Bounds.** A cell past Excel's grid ([`MAX_ROWS`], [`MAX_COLUMNS`]) is
//! not read. Every part read whole, every shared string, cell style and
//! sheet listed, every cell read and every copy the layout makes of it,
//! every cell a merge covers or a table's grid holds, every element made
//! and the text of each cell are spent from the workbook's [`Budget`]
//! before they are allocated, and the passes that mend islands into blocks,
//! which compare them pairwise, spend its work; past either, the workbook
//! is refused. A number format is kept once, however many cell styles name
//! it, and a sheet's part read once, however many times the workbook lists
//! it. A sheet's merges past about a million cells are read unmerged,
//! its tables past about four million cells of grid as their cells' text,
//! and past 2,048 islands no island is mended.

use fluree_doc_model::budget::{Budget, TooLarge};
use fluree_doc_model::{Datum, DocumentInfo, Element};
pub use fluree_doc_ooxml::Package;
use fluree_doc_ooxml::{attr, local, PartNames, Relations};

/// A picture anchored to a sheet's cells: the figure it is drawn as and
/// where each of its images is. Its bytes are read on demand, each image
/// once however many figures draw it ([`read_pictures`]), within what one
/// document may have read ([`Allowance`]).
pub use fluree_doc_model::{Allowance, Media, MediaSource, PictureReader, Unread};

/// The opened package [`read_media`] and [`read_pictures`] read from; the
/// bytes of one image part, at most [`MAX_MEDIA_BYTES`] (64 MiB) of them, or
/// only its first, to tell its format by; and the loop a document's pictures
/// are read through.
pub use fluree_doc_ooxml::{read_media, read_media_head, read_pictures, MAX_MEDIA_BYTES};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

#[derive(Debug)]
pub enum XlsxError {
    Zip(String),
    Xml(String),
    NoWorkbook,
    /// Past the workbook's budget.
    TooLarge(TooLarge),
}

impl std::fmt::Display for XlsxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zip(e) => write!(f, "not a readable .xlsx: {e}"),
            Self::Xml(e) => write!(f, "malformed workbook XML: {e}"),
            Self::NoWorkbook => write!(f, "package has no workbook part"),
            Self::TooLarge(e) => write!(f, "workbook too large: {e}"),
        }
    }
}

impl From<TooLarge> for XlsxError {
    fn from(e: TooLarge) -> Self {
        Self::TooLarge(e)
    }
}

/// A bound passed is the error's source: [`TooLarge::limit`] says which.
impl std::error::Error for XlsxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TooLarge(e) => Some(e),
            _ => None,
        }
    }
}

/// Parse a `.xlsx` file's bytes into elements, in sheet order.
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, XlsxError> {
    parse_with_info(bytes).map(|(elements, _)| elements)
}

/// [`parse`], with what the file declares about itself in its core
/// properties, read from the one opening of the package.
///
/// The workbook is the package's main part, and its sheets, shared strings
/// and styles are the parts it relates to as such; `xl/` is where a package
/// that does not say keeps them.
pub fn parse_with_info(bytes: &[u8]) -> Result<(Vec<Element>, DocumentInfo), XlsxError> {
    parse_with_media(bytes).map(|(elements, info, _)| (elements, info))
}

/// What [`parse_with_ocr`] gives.
#[cfg(feature = "ocr")]
#[derive(Debug, Clone)]
pub struct OcrParse {
    pub elements: Vec<Element>,
    pub info: DocumentInfo,
    /// The images not read, as `doc:unreadImages` lists them, once for each
    /// figure drawing them, with why: `Missing` (not in the package),
    /// `UnsupportedFormat` (a drawing kept as EMF or WMF, a GIF),
    /// `PastAllowance` (past [`Allowance::DOCUMENT`]), `PastMemoryLimit`
    /// (its reading past what the document left of its budget) or
    /// `Failed`. Each is
    /// left as its figures.
    pub unread: Vec<fluree_doc_model::UnreadImage>,
    /// What went wrong with each image `UnsupportedFormat` or `Failed`, by
    /// the name it goes by (`m1`).
    pub errors: Vec<(String, String)>,
    /// Images of more than one page (a multi-page TIFF), of which only the
    /// first was read.
    pub first_page_only: Vec<fluree_doc_model::Picture>,
    /// The readings taken, with the engine's confidence, as
    /// `doc:ocrReadings` lists them: by the name each picture goes by (`m1`).
    pub ocr: Vec<fluree_doc_model::OcrReading>,
}

/// [`parse_with_info`], with the text in the sheets' pictures read by
/// `engine` and placed right after each picture's figure
/// ([`fluree_doc_image::place_after_figures`]).
#[cfg(feature = "ocr")]
pub fn parse_with_ocr(
    bytes: &[u8],
    engine: &dyn fluree_doc_vision::OcrEngine,
) -> Result<OcrParse, XlsxError> {
    let mut pkg = Package::open(bytes).map_err(XlsxError::Zip)?;
    // The pictures' readings are spent from what the document left of its
    // budget: together, they take no more than one document may.
    let mut budget = Budget::default();
    let (mut elements, info, media) = parse_package_within(&mut pkg, &mut budget)?;
    let r = fluree_doc_image::read_placed(
        media,
        &mut |part| read_media(&mut pkg, part),
        engine,
        &mut budget,
    );
    fluree_doc_image::place_after_figures(&mut elements, r.read);
    Ok(OcrParse {
        elements,
        info,
        unread: r.unread,
        errors: r.errors,
        first_page_only: r.first_page_only,
        ocr: r.ocr,
    })
}

/// [`parse_with_info`], with the pictures each sheet's drawing anchors to
/// its cells, in workbook order: each a figure (`figure-<sheet>-<n>`,
/// numbered across the workbook) placed by its anchor, and the images it
/// holds, for an OCR engine or a model to read. Only where they are is read
/// here; their bytes are read by [`read_media`].
pub fn parse_with_media(
    bytes: &[u8],
) -> Result<(Vec<Element>, DocumentInfo, Vec<Media>), XlsxError> {
    parse_package(&mut Package::open(bytes).map_err(XlsxError::Zip)?)
}

/// [`parse_with_media`] from a package already opened, for a caller that
/// goes on to read the images' bytes from it ([`read_media`]): opening it
/// reads the archive's whole directory, once.
pub fn parse_package(
    pkg: &mut Package<'_>,
) -> Result<(Vec<Element>, DocumentInfo, Vec<Media>), XlsxError> {
    parse_package_within(pkg, &mut Budget::default())
}

/// [`parse_package`], spending from `budget`, the workbook's, and leaving
/// it spent: a caller that goes on to read the workbook's pictures spends
/// what their readings make from what the workbook left of it
/// (`fluree_doc_image::read_placed`, as `parse_with_ocr` does), so a
/// workbook and its pictures are bounded together.
pub fn parse_package_within(
    pkg: &mut Package<'_>,
    budget: &mut Budget,
) -> Result<(Vec<Element>, DocumentInfo, Vec<Media>), XlsxError> {
    let main = pkg.main_part("xl/workbook.xml");
    // The parts the workbook is read from, each inflated whole, are spent
    // from its budget before what it makes.
    let workbook = pkg
        .read_within(&main, budget)?
        .ok_or(XlsxError::NoWorkbook)?;
    let rels = pkg.relationships(&main).map_err(XlsxError::Zip)?;
    let part = |name: &str, fallback: &str| {
        rels.iter()
            .find(|r| r.is(name) && !r.external)
            .map_or_else(|| fallback.to_string(), |r| r.target.clone())
    };
    let (shared, styles) = (
        part("sharedStrings", "xl/sharedStrings.xml"),
        part("styles", "xl/styles.xml"),
    );
    let shared = match pkg.read_within(&shared, budget)? {
        Some(x) => read_shared_strings(&x, budget)?,
        None => Vec::new(),
    };
    let styles = match pkg.read_within(&styles, budget)? {
        Some(x) => read_styles(&x, budget)?,
        None => Styles::default(),
    };
    let (sheets, date1904) = parse_workbook_xml(&workbook, budget)?;
    let names = PartNames::new(pkg);
    // Each sheet's part by its relationship id, the first internal one
    // that has it: found in one lookup per sheet listed, where a scan of
    // every relationship for each made the listing's cost its square.
    let targets = Relations::new(&rels);
    // A sheet part is read once, at its first listing: listed again, under
    // any name, its part, its drawing and their relationships would be
    // read and laid out again for each.
    let mut read = HashSet::new();

    let mut out = Vec::new();
    let mut media = Vec::new();
    let mut figure_count = 0usize;
    for (page, sheet_ref) in sheets.iter().enumerate() {
        let Some(target) = targets.part(&sheet_ref.rid) else {
            continue;
        };
        if !read.insert(target) {
            continue;
        }
        let Some(xml) = pkg.read_within(target, budget)? else {
            continue;
        };
        let ctx = Context {
            shared: &shared,
            styles: &styles,
            date1904,
        };
        let sheet = read_sheet(&xml, &ctx, budget)?;
        // The sheet's drawing, by the relationship it names: the pictures
        // anchored to its cells, each a figure where it is anchored.
        let mut figures = Vec::new();
        if let Some(rid) = &sheet.drawing {
            let drawing = pkg
                .relationships(target)
                .map_err(XlsxError::Zip)?
                .into_iter()
                .find(|r| r.id == *rid && !r.external)
                .map(|r| r.target);
            let xml = match &drawing {
                Some(d) => pkg.read_within(d, budget)?,
                None => None,
            };
            if let Some((xml, part)) = xml.zip(drawing) {
                let drawing_rels = pkg.relationships(&part).map_err(XlsxError::Zip)?;
                let drawing_rels = Relations::new(&drawing_rels);
                for pic in pictures(&xml, budget)? {
                    budget.spend_elements(1)?;
                    budget.spend_text(&pic.alt)?;
                    figure_count += 1;
                    let id = format!("figure-{page}-{figure_count}");
                    for (index, rid) in pic.rids.iter().enumerate() {
                        media.push(Media {
                            figure: id.clone(),
                            source: names.resolve(&drawing_rels, rid),
                            index,
                        });
                    }
                    let mut f = element("doco:Figure", pic.alt, None, page);
                    f.figure = Some(id);
                    figures.push((pic.at, f));
                }
            }
        }
        out.extend(sheet_elements(
            &sheet_ref.name,
            &sheet,
            page,
            figures,
            budget,
        )?);
    }
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    Ok((out, pkg.info(), media))
}

/// One cell of a sheet another workbook format's reader read (`.xls`,
/// `.xlsb`): where it is, 0-based, the text Excel shows, and what it stores.
#[derive(Debug, Clone, PartialEq)]
pub struct GridCell {
    pub row: u32,
    pub col: u32,
    /// What the cell shows. Runs of whitespace, line breaks among them,
    /// become one space, as a sheet cell's do here; a cell with no text is
    /// no cell.
    pub text: String,
    /// What it stores, typed, where its type is not text ([`cell_datum`]).
    pub datum: Option<Datum>,
    pub bold: bool,
}

/// A sheet another workbook format's reader read, for [`grid_elements`]
/// to lay out as an XLSX sheet is laid out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GridSheet {
    pub name: String,
    pub cells: Vec<GridCell>,
    /// Merged ranges, each `(first row, first column, last row, last
    /// column)`, inclusive, 0-based.
    pub merges: Vec<(u32, u32, u32, u32)>,
    /// Rows frozen at the top, which name the columns.
    pub frozen_rows: u32,
}

/// Sheets read by another workbook format's reader, laid out exactly as an
/// XLSX workbook's are: each a page opened by its name, its islands of
/// cells tables with their titles and notes peeled off, merges as
/// continuations. Every element's provenance and evidence is `source`
/// (`xls`, `xlsb`). A cell set larger than the rest is not known here, so
/// only weight (`bold`) sets a title apart. A cell past Excel's grid
/// ([`MAX_ROWS`], [`MAX_COLUMNS`]) is no cell, as in an XLSX sheet, and a
/// merge is cut at the grid's edge.
///
/// The cells and their text were spent from `budget` by the reader that
/// made them; what the layout makes of them is spent here, before it is
/// made, and past `budget` the workbook is refused.
pub fn grid_elements(
    sheets: Vec<GridSheet>,
    source: &'static str,
    budget: &mut Budget,
) -> Result<Vec<Element>, TooLarge> {
    let mut out = Vec::new();
    for (page, g) in sheets.into_iter().enumerate() {
        let mut sheet = Sheet {
            frozen_rows: g.frozen_rows,
            ..Sheet::default()
        };
        // The sheet's own copy of its cells.
        budget.spend_cells(g.cells.len() as u64)?;
        for c in g.cells {
            if c.row >= MAX_ROWS || c.col >= MAX_COLUMNS {
                continue;
            }
            let text = c.text.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.is_empty() {
                continue;
            }
            sheet.cells.insert(
                (c.row, c.col),
                Cell {
                    text,
                    datum: c.datum,
                    bold: c.bold,
                    size: CellStyle::default().size,
                },
            );
        }
        sheet.merges = g
            .merges
            .into_iter()
            .map(|(r0, c0, r1, c1)| Range {
                r0: r0.min(r1),
                c0: c0.min(c1),
                r1: r0.max(r1).min(MAX_ROWS - 1),
                c1: c0.max(c1).min(MAX_COLUMNS - 1),
            })
            .filter(|m| m.r0 < MAX_ROWS && m.c0 < MAX_COLUMNS)
            .collect();
        out.extend(sheet_elements(&g.name, &sheet, page, Vec::new(), budget)?);
    }
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
        e.provenance = source;
        e.evidence = source;
    }
    Ok(out)
}

/// A picture a drawing anchors to a sheet's cells.
#[derive(Debug, PartialEq, Eq)]
struct Picture {
    /// The cell its top-left corner is anchored to (row, column), 0-based;
    /// a picture placed by an absolute position, which names no cell, is
    /// set after everything on its sheet.
    at: (u32, u32),
    /// Its alternative text, or its title.
    alt: String,
    /// The relationships naming its images (`a:blip r:embed`, or `r:link`
    /// for one linked to a file outside the package), in order: one for a
    /// picture, several for a group of them.
    rids: Vec<String>,
}

/// An anchor of a drawing, as it is read.
#[derive(Default)]
struct Anchor {
    /// The cell its `from` corner is in, 0-based.
    row: Option<u32>,
    col: Option<u32>,
    /// Placed by a position on the sheet, not by a cell.
    absolute: bool,
    descr: String,
    title: String,
    rids: Vec<String>,
    /// The ids in `rids`, each kept once: a scan of `rids` for each made
    /// an anchor's cost the square of the images it names.
    named: HashSet<String>,
}

/// The pictures of a drawing part (`xl/drawings/drawing1.xml`), in the
/// order it anchors them. Only a picture (`xdr:pic`) has an image to read:
/// a chart has data, and a shape's image fill is a texture. The fallback
/// copy of an alternate content is not read twice. A drawing that does not
/// parse anchors nothing: the cells are read without it. Each image a
/// picture names is spent from `budget` before it is kept: one may name
/// millions.
fn pictures(xml: &str, budget: &mut Budget) -> Result<Vec<Picture>, TooLarge> {
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    // The anchor being read.
    let mut anchor: Option<Anchor> = None;
    let (mut in_from, mut in_pic, mut fallback) = (false, 0usize, 0usize);
    // Which of the anchor's `from` coordinates is being read.
    let mut reading: Option<&'static str> = None;
    loop {
        let (e, start) = match r.read_event_into(&mut buf) {
            Err(_) => return Ok(Vec::new()),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => (e.into_owned(), true),
            Ok(Event::Empty(e)) => (e.into_owned(), false),
            Ok(Event::Text(t)) => {
                if let (Some(which), Some(a), 0) = (reading, anchor.as_mut(), fallback) {
                    let v = t.unescape().ok().and_then(|v| v.trim().parse::<u32>().ok());
                    match which {
                        "row" => a.row = a.row.or(v),
                        _ => a.col = a.col.or(v),
                    }
                }
                buf.clear();
                continue;
            }
            Ok(Event::End(e)) => {
                match local(e.name().as_ref()) {
                    "Fallback" if fallback > 0 => fallback -= 1,
                    _ if fallback > 0 => {}
                    "from" => in_from = false,
                    "row" | "col" => reading = None,
                    "pic" if in_pic > 0 => in_pic -= 1,
                    "twoCellAnchor" | "oneCellAnchor" | "absoluteAnchor" => {
                        if let Some(a) = anchor.take().filter(|a| !a.rids.is_empty()) {
                            let at = if a.absolute {
                                (u32::MAX, u32::MAX)
                            } else {
                                (a.row.unwrap_or(0), a.col.unwrap_or(0))
                            };
                            out.push(Picture {
                                at,
                                alt: fluree_doc_ooxml::alt_text(&a.descr, &a.title),
                                rids: a.rids,
                            });
                        }
                    }
                    _ => {}
                }
                buf.clear();
                continue;
            }
            Ok(_) => {
                buf.clear();
                continue;
            }
        };
        let tag = local(e.name().as_ref()).to_string();
        if fallback > 0 {
            if tag == "Fallback" && start {
                fallback += 1;
            }
            buf.clear();
            continue;
        }
        match tag.as_str() {
            "Fallback" if start => fallback += 1,
            "twoCellAnchor" | "oneCellAnchor" | "absoluteAnchor" if start => {
                anchor = Some(Anchor {
                    absolute: tag == "absoluteAnchor",
                    ..Anchor::default()
                });
            }
            "from" if start && anchor.is_some() => in_from = true,
            "row" if start && in_from => reading = Some("row"),
            "col" if start && in_from => reading = Some("col"),
            "pic" if start && anchor.is_some() => in_pic += 1,
            "cNvPr" if in_pic > 0 => {
                // A group's first picture speaks for it.
                if let Some(a) = anchor
                    .as_mut()
                    .filter(|a| a.descr.is_empty() && a.title.is_empty())
                {
                    a.descr = attr(&e, "descr").unwrap_or_default();
                    a.title = attr(&e, "title").unwrap_or_default();
                }
            }
            "blip" if in_pic > 0 => {
                if let (Some(a), Some(rid)) = (
                    anchor.as_mut(),
                    attr(&e, "embed").or_else(|| attr(&e, "link")),
                ) {
                    if !a.named.contains(&rid) {
                        budget.spend_media(&rid)?;
                        a.named.insert(rid.clone());
                        a.rids.push(rid);
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// What the file declares about itself in its core properties: title,
/// author, and when it was made and last saved. Empty where there are none
/// or the file is not a package.
pub fn info(bytes: &[u8]) -> DocumentInfo {
    fluree_doc_ooxml::info(bytes)
}

/// What a sheet needs from the rest of the package.
pub struct Context<'a> {
    pub shared: &'a [String],
    pub styles: &'a Styles,
    /// The workbook counts days from 1904-01-01 rather than 1900.
    pub date1904: bool,
}

struct SheetRef {
    name: String,
    rid: String,
}

/// The sheets the workbook lists, each and its name and relationship id
/// spent from `budget` before it is kept, and whether it counts dates from
/// 1904.
fn parse_workbook_xml(xml: &str, budget: &mut Budget) -> Result<(Vec<SheetRef>, bool), XlsxError> {
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut sheets = Vec::new();
    let mut date1904 = false;
    loop {
        match r.read_event_into(&mut buf) {
            Err(e) => return Err(XlsxError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                "sheet" => {
                    let name = attr(&e, "name").unwrap_or_default();
                    let rid = attr(&e, "id").unwrap_or_default();
                    budget.spend_cells(1)?;
                    budget.spend_text(&name)?;
                    budget.spend_text(&rid)?;
                    sheets.push(SheetRef { name, rid });
                }
                "workbookPr" => {
                    date1904 =
                        attr(&e, "date1904").is_some_and(|v| matches!(v.as_str(), "1" | "true"));
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    Ok((sheets, date1904))
}

/// The shared string table, one entry per `<si>`, rich-text runs joined and
/// phonetic guides (`<rPh>`) left out.
pub fn parse_shared_strings(xml: &str) -> Result<Vec<String>, XlsxError> {
    read_shared_strings(xml, &mut Budget::default())
}

/// [`parse_shared_strings`], each entry spent from `budget` as a cell slot
/// before it is kept: `<si/>` is five bytes of the part, and a string in
/// the table. Their text is no more than the part's, which is spent as it
/// is read; a copy of one is spent where a cell names it.
fn read_shared_strings(xml: &str, budget: &mut Budget) -> Result<Vec<String>, XlsxError> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    let mut in_t = false;
    let mut phonetic = 0usize;
    loop {
        match r.read_event_into(&mut buf) {
            Err(e) => return Err(XlsxError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => match local(e.name().as_ref()) {
                "si" => cur = Some(String::new()),
                "rPh" => phonetic += 1,
                "t" if phonetic == 0 => in_t = true,
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                if local(e.name().as_ref()) == "si" {
                    budget.spend_cells(1)?;
                    out.push(String::new());
                }
            }
            Ok(Event::Text(t)) => {
                if in_t {
                    if let Some(s) = cur.as_mut() {
                        s.push_str(&t.unescape().unwrap_or_default());
                    }
                }
            }
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                "si" => {
                    budget.spend_cells(1)?;
                    // Runs joined leave room behind them; the table keeps
                    // the text only.
                    let mut s = cur.take().unwrap_or_default();
                    s.shrink_to_fit();
                    out.push(s);
                }
                "rPh" => phonetic = phonetic.saturating_sub(1),
                "t" => in_t = false,
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// What a cell's style index resolves to: how its number displays, and
/// whether its font is bold and how large.
///
/// The format is shared: every cell style that names a number format holds
/// the one the workbook declares, not a copy of it with its literals.
#[derive(Debug, Clone, PartialEq)]
pub struct CellStyle {
    pub format: Arc<NumFmt>,
    pub bold: bool,
    pub size: f32,
}

impl Default for CellStyle {
    fn default() -> Self {
        Self {
            format: Arc::new(NumFmt::General),
            bold: false,
            size: 11.0,
        }
    }
}

/// The cell styles a workbook declares, by `s` index.
#[derive(Debug, Default, Clone)]
pub struct Styles {
    pub xfs: Vec<CellStyle>,
}

impl Styles {
    fn get(&self, s: Option<usize>) -> CellStyle {
        s.and_then(|i| self.xfs.get(i).cloned()).unwrap_or_default()
    }
}

/// How a numeric cell displays. Excel's format language is large; this
/// reads the part that changes what a number *means* — a date is not a
/// count of days, a percentage is not a fraction — and the part that
/// changes how it reads: `14.50` where the format fixes two decimals,
/// `1,234.50` grouped, `$` or `EUR` beside it. A cell is read for the text
/// Excel shows, and an amount in an invoice is shown as one. What this
/// does not read (conditions, scientific notation, fractions) falls back
/// to the shortest decimal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumFmt {
    General,
    Date,
    Time,
    DateTime,
    /// Decimal places shown.
    Percent(usize),
    /// Digits, grouping and the literal text around them.
    Number(NumberFormat),
}

/// Built-in number formats, by id. Ids 14–22 are dates and times; 27–36
/// and 50–58 are locale dates; 45–47 are elapsed times; 9 and 10 are
/// percentages.
///
/// Ids 1–4 are fixed decimals and grouping. 5–8 and 37–44 are the locale's
/// currency and accounting formats, which the file does not spell out:
/// read with grouping, two decimals where they have them, and negatives in
/// parentheses, but no currency sign, because which sign is the reader's
/// locale and not the file's.
fn builtin_fmt(id: u32) -> NumFmt {
    let number = |code: &str| NumberFormat::parse(code).map_or(NumFmt::General, NumFmt::Number);
    match id {
        1 => number("0"),
        2 => number("0.00"),
        3 => number("#,##0"),
        4 => number("#,##0.00"),
        5 | 6 | 37 | 38 => number("#,##0_);(#,##0)"),
        7 | 8 | 39 | 40 => number("#,##0.00_);(#,##0.00)"),
        41 | 42 => number("_(* #,##0_);_(* \\(#,##0\\);_(* \"-\"_);_(@_)"),
        43 | 44 => number("_(* #,##0.00_);_(* \\(#,##0.00\\);_(* \"-\"??_);_(@_)"),
        9 => NumFmt::Percent(0),
        10 => NumFmt::Percent(2),
        14..=17 | 27..=36 | 50..=58 => NumFmt::Date,
        18..=21 | 45..=47 => NumFmt::Time,
        22 => NumFmt::DateTime,
        _ => NumFmt::General,
    }
}

/// The last id [`builtin_fmt`] knows: every id past it is General.
const LAST_BUILTIN_FMT: u32 = 58;

/// Classify a custom format code. Quoted literals, bracketed conditions and
/// locale tags, and escaped characters say nothing about the value, so they
/// are dropped before the date letters are looked for.
pub fn custom_fmt(code: &str) -> NumFmt {
    let mut bare = String::new();
    let mut chars = code.chars();
    let mut quoted = false;
    let mut bracket = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            _ if quoted => {}
            '[' => bracket = true,
            ']' => bracket = false,
            _ if bracket => {}
            '\\' | '_' | '*' => {
                chars.next();
            }
            ';' => break, // the positive section decides
            c => bare.push(c.to_ascii_lowercase()),
        }
    }
    if bare.contains('%') {
        let decimals = bare
            .split_once('.')
            .map(|(_, tail)| tail.chars().take_while(|c| *c == '0').count())
            .unwrap_or(0);
        return NumFmt::Percent(decimals);
    }
    let date = bare.contains('y') || bare.contains('d');
    let time = bare.contains('h') || bare.contains('s');
    let month = bare.contains('m');
    match (date, time, month) {
        (true, true, _) => NumFmt::DateTime,
        (true, false, _) => NumFmt::Date,
        (false, true, _) => NumFmt::Time,
        (false, false, true) => NumFmt::Date,
        _ => NumberFormat::parse(code).map_or(NumFmt::General, NumFmt::Number),
    }
}

/// A number format read section by section: positive; negative; zero.
///
/// Each section is literal text, one run of digit placeholders, and
/// literal text — which covers fixed decimals (`0.00`), grouping
/// (`#,##0`), currency on either side (`"$"#,##0.00`, `[$€-407] #,##0.00`,
/// `#,##0.00 [$EUR]`), accounting padding (`_(` and `* ` take space and
/// show nothing), parenthesised negatives, and scaling by trailing commas.
/// A format outside that — a condition, an exponent, a fraction, digits on
/// both sides of a literal, text placeholders — is not read at all, and
/// the cell shows its General form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberFormat {
    sections: Vec<NumSection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct NumSection {
    prefix: String,
    suffix: String,
    /// The section shows a number at all; a zero section can be a dash.
    digits: bool,
    /// Integer digits always shown (`0` placeholders before the point).
    int_zeros: usize,
    /// Decimals always shown, and at most shown.
    dec_min: usize,
    dec_max: usize,
    point: bool,
    grouping: bool,
    /// Thousands divided out by trailing commas.
    scale: usize,
    /// Each `%` multiplies by a hundred.
    percent: usize,
}

impl NumberFormat {
    pub fn parse(code: &str) -> Option<NumberFormat> {
        let sections = split_sections(code)
            .into_iter()
            .take(3)
            .map(|s| NumSection::parse(&s))
            .collect::<Option<Vec<_>>>()?;
        // A format with no digits anywhere is not a number format.
        sections
            .first()
            .is_some_and(|s| s.digits)
            .then_some(NumberFormat { sections })
    }

    /// `v` as this format displays it.
    pub fn format(&self, v: f64) -> String {
        if !v.is_finite() {
            return format_general(v);
        }
        let s = &self.sections;
        let (section, signed) = match (v < 0.0, v == 0.0) {
            (true, _) if s.len() >= 2 => (&s[1], false),
            (true, _) => (&s[0], true),
            (false, true) if s.len() >= 3 => (&s[2], false),
            _ => (&s[0], false),
        };
        let shown = section.render(v.abs());
        // One section serves negatives with a minus in front of everything,
        // unless the value rounds away to nothing.
        if signed && shown.chars().any(|c| c.is_ascii_digit() && c != '0') {
            format!("-{shown}")
        } else {
            shown
        }
    }
}

/// A format code's `;`-separated sections, ignoring any `;` that is quoted
/// or escaped.
fn split_sections(code: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut quoted = false;
    let mut chars = code.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted => {
                out.push(String::new());
                continue;
            }
            '\\' if !quoted => {
                let last = out.last_mut().unwrap();
                last.push(c);
                if let Some(n) = chars.next() {
                    last.push(n);
                }
                continue;
            }
            _ => {}
        }
        out.last_mut().unwrap().push(c);
    }
    out
}

impl NumSection {
    fn parse(code: &str) -> Option<NumSection> {
        let mut s = NumSection::default();
        // 0: before the digits, 1: in them, 2: after them.
        let mut phase = 0;
        let chars: Vec<char> = code.chars().collect();
        let is_digit = |c: Option<&char>| matches!(c, Some('0' | '#' | '?'));
        let literal = |s: &mut NumSection, phase: &mut u8, text: &str| {
            if *phase == 0 {
                s.prefix.push_str(text);
            } else {
                *phase = 2;
                s.suffix.push_str(text);
            }
        };
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            i += 1;
            match c {
                '"' => {
                    let end = chars[i..]
                        .iter()
                        .position(|c| *c == '"')
                        .map_or(chars.len(), |p| i + p);
                    let text: String = chars[i..end].iter().collect();
                    literal(&mut s, &mut phase, &text);
                    i = end + 1;
                }
                '\\' => {
                    if let Some(n) = chars.get(i) {
                        literal(&mut s, &mut phase, &n.to_string());
                    }
                    i += 1;
                }
                // Padding to the width of a character, and a fill: both
                // lay the cell out and show nothing in text.
                '_' | '*' => i += 1,
                '[' => {
                    let end = chars[i..]
                        .iter()
                        .position(|c| *c == ']')
                        .map_or(chars.len(), |p| i + p);
                    let tag: String = chars[i..end].iter().collect();
                    i = end + 1;
                    if let Some(currency) = tag.strip_prefix('$') {
                        // `[$€-407]`: the symbol, then the locale it is for.
                        let symbol = currency.split('-').next().unwrap_or("");
                        literal(&mut s, &mut phase, symbol);
                    } else if tag.starts_with(['<', '>', '=']) {
                        return None;
                    }
                    // Anything else is a colour or a calendar.
                }
                '0' | '#' | '?' => {
                    if phase == 2 {
                        return None;
                    }
                    phase = 1;
                    s.digits = true;
                    if s.point {
                        s.dec_max += 1;
                        if c == '0' {
                            s.dec_min = s.dec_max;
                        }
                    } else if c == '0' {
                        s.int_zeros += 1;
                    }
                }
                '.' if phase == 1 || phase == 0 && is_digit(chars.get(i)) => {
                    if s.point {
                        return None;
                    }
                    phase = 1;
                    s.point = true;
                }
                ',' if phase == 1 => {
                    if !s.point && is_digit(chars.get(i)) {
                        s.grouping = true;
                    } else {
                        s.scale += 1;
                    }
                }
                '%' => {
                    s.percent += 1;
                    literal(&mut s, &mut phase, "%");
                }
                'E' | 'e' if phase == 1 => return None,
                '/' if phase == 1 => return None,
                '@' => return None,
                c if c.is_ascii_alphanumeric() => return None,
                c => literal(&mut s, &mut phase, &c.to_string()),
            }
        }
        Some(s)
    }

    fn render(&self, v: f64) -> String {
        if !self.digits {
            return format!("{}{}", self.prefix, self.suffix);
        }
        let v = v * 100f64.powi(self.percent as i32) / 1000f64.powi(self.scale as i32);
        let (int, mut frac) = round_decimal(v, self.dec_max);
        while frac.len() > self.dec_min && frac.ends_with('0') {
            frac.pop();
        }
        let mut int = int.trim_start_matches('0').to_string();
        while int.len() < self.int_zeros {
            int.insert(0, '0');
        }
        if self.grouping {
            let digits: Vec<char> = int.chars().collect();
            int = digits
                .iter()
                .enumerate()
                .flat_map(|(k, d)| {
                    let sep = k > 0 && (digits.len() - k).is_multiple_of(3);
                    sep.then_some(',').into_iter().chain(std::iter::once(*d))
                })
                .collect();
        }
        let point = if self.point { "." } else { "" };
        format!("{}{int}{point}{frac}{}", self.prefix, self.suffix)
    }
}

/// A non-negative value's integer and fractional digits, rounded to
/// `decimals` places the way Excel shows them: to the fifteen significant
/// digits it keeps, then half away from zero. Binary rounding alone shows
/// 2.675 at two places as 2.67, because the double is 2.67499999…; Excel
/// shows 2.68.
fn round_decimal(v: f64, decimals: usize) -> (String, String) {
    let sci = format!("{:.14e}", v);
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i64 = exp.parse().unwrap_or(0);
    let mut digits: Vec<u8> = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect();
    let mut int_len = exp + 1;
    if int_len <= 0 {
        let pad = (1 - int_len) as usize;
        digits.splice(0..0, std::iter::repeat_n(0, pad));
        int_len = 1;
    }
    let mut int_len = int_len as usize;
    let cut = int_len + decimals;
    digits.resize(digits.len().max(cut + 1), 0);
    let up = digits[cut] >= 5;
    digits.truncate(cut);
    if up {
        let mut k = cut;
        loop {
            if k == 0 {
                digits.insert(0, 1);
                int_len += 1;
                break;
            }
            k -= 1;
            if digits[k] == 9 {
                digits[k] = 0;
            } else {
                digits[k] += 1;
                break;
            }
        }
    }
    let text = |d: &[u8]| d.iter().map(|d| char::from(b'0' + d)).collect::<String>();
    (text(&digits[..int_len]), text(&digits[int_len..]))
}

/// `xl/styles.xml`: custom number formats, fonts, and the cell style table
/// that indexes both.
pub fn parse_styles(xml: &str) -> Result<Styles, XlsxError> {
    read_styles(xml, &mut Budget::default())
}

/// [`parse_styles`], each number format (its literals as text) and each
/// cell style spent from `budget` before it is kept. A cell style holds
/// its number format shared, so one format's literals are kept once
/// however many styles name it.
fn read_styles(xml: &str, budget: &mut Budget) -> Result<Styles, XlsxError> {
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    // The number formats the file declares, by id, and the built-in ones
    // named so far, each made once.
    let mut custom: HashMap<u32, Arc<NumFmt>> = HashMap::new();
    let mut builtin: HashMap<u32, Arc<NumFmt>> = HashMap::new();
    let mut fonts: Vec<(bool, f32)> = Vec::new();
    let mut xfs: Vec<CellStyle> = Vec::new();
    let mut in_fonts = false;
    let mut in_cell_xfs = false;
    let mut font: Option<(bool, f32)> = None;
    loop {
        let event = r.read_event_into(&mut buf);
        let (e, empty) = match event {
            Err(e) => return Err(XlsxError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => (e, false),
            Ok(Event::Empty(e)) => (e, true),
            Ok(Event::End(e)) => {
                match local(e.name().as_ref()) {
                    "fonts" => in_fonts = false,
                    "font" if in_fonts => fonts.push(font.take().unwrap_or((false, 11.0))),
                    "cellXfs" => in_cell_xfs = false,
                    _ => {}
                }
                buf.clear();
                continue;
            }
            Ok(_) => {
                buf.clear();
                continue;
            }
        };
        let name = e.name();
        let tag = local(name.as_ref());
        match tag {
            "numFmt" => {
                // The code is unescaped: a currency in quotes arrives as
                // `&quot;$&quot;#,##0.00`, and read raw its entity names
                // are letters that make it no number format.
                let code = fluree_doc_ooxml::attributes(&e).find_map(|a| {
                    (local(a.key.as_ref()) == "formatCode")
                        .then(|| a.unescape_value().ok().map(|v| v.into_owned()))
                        .flatten()
                });
                if let (Some(id), Some(code)) = (attr(&e, "numFmtId"), code) {
                    if let Ok(id) = id.parse::<u32>() {
                        budget.spend_cells(1)?;
                        budget.spend_text(&code)?;
                        custom.insert(id, Arc::new(custom_fmt(&code)));
                    }
                }
            }
            "fonts" => in_fonts = true,
            "font" if in_fonts => {
                if empty {
                    fonts.push((false, 11.0));
                } else {
                    font = Some((false, 11.0));
                }
            }
            "b" if in_fonts => {
                if let Some(f) = font.as_mut() {
                    f.0 = !attr(&e, "val").is_some_and(|v| matches!(v.as_str(), "0" | "false"));
                }
            }
            "sz" if in_fonts => {
                if let (Some(f), Some(v)) = (font.as_mut(), attr(&e, "val")) {
                    if let Ok(sz) = v.parse::<f32>() {
                        f.1 = sz;
                    }
                }
            }
            "cellXfs" => in_cell_xfs = true,
            "xf" if in_cell_xfs => {
                let num = attr(&e, "numFmtId")
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(0);
                budget.spend_cells(1)?;
                let format = match custom.get(&num) {
                    Some(f) => f.clone(),
                    // Every id past the last built-in is General, one of them.
                    None => {
                        let id = num.min(LAST_BUILTIN_FMT + 1);
                        builtin
                            .entry(id)
                            .or_insert_with(|| Arc::new(builtin_fmt(id)))
                            .clone()
                    }
                };
                let (bold, size) = attr(&e, "fontId")
                    .and_then(|v| v.parse::<usize>().ok())
                    .and_then(|i| fonts.get(i).copied())
                    .unwrap_or((false, 11.0));
                xfs.push(CellStyle { format, bold, size });
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(Styles { xfs })
}

/// One occupied cell of a sheet, in grid coordinates.
#[derive(Debug, Clone, Default, PartialEq)]
struct Cell {
    text: String,
    /// What the cell stores, where its type is not text.
    datum: Option<Datum>,
    bold: bool,
    size: f32,
}

/// The rows of Excel's grid: no cell past the last is read.
pub const MAX_ROWS: u32 = 1 << 20;
/// The columns of Excel's grid: no cell past the last is read.
pub const MAX_COLUMNS: u32 = 1 << 14;

/// A rectangle of cells, inclusive, 0-based, within the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Range {
    r0: u32,
    c0: u32,
    r1: u32,
    c1: u32,
}

impl Range {
    fn width(&self) -> u64 {
        u64::from(self.c1) - u64::from(self.c0) + 1
    }
    fn height(&self) -> u64 {
        u64::from(self.r1) - u64::from(self.r0) + 1
    }
}

/// Column letters to a 0-based index: `A` → 0, `Z` → 25, `AA` → 26.
pub fn column_index(letters: &str) -> Option<u32> {
    let mut n: u32 = 0;
    let mut any = false;
    for c in letters.chars() {
        if !c.is_ascii_alphabetic() {
            return None;
        }
        n = n
            .checked_mul(26)?
            .checked_add(c.to_ascii_uppercase() as u32 - 'A' as u32 + 1)?;
        any = true;
    }
    any.then(|| n - 1)
}

/// `B7` → (row 6, col 1).
fn cell_ref(r: &str) -> Option<(u32, u32)> {
    let split = r.find(|c: char| c.is_ascii_digit())?;
    let col = column_index(&r[..split])?;
    let row = r[split..].parse::<u32>().ok()?.checked_sub(1)?;
    Some((row, col))
}

/// `A1:C4` → the rectangle; a bare `A1` is a one-cell range. A range is cut
/// at the grid's last row and column ([`MAX_ROWS`], [`MAX_COLUMNS`]), and
/// one that starts past them is none.
fn parse_range(s: &str) -> Option<Range> {
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    let (r0, c0) = cell_ref(a)?;
    let (r1, c1) = cell_ref(b)?;
    let range = Range {
        r0: r0.min(r1),
        c0: c0.min(c1),
        r1: r0.max(r1).min(MAX_ROWS - 1),
        c1: c0.max(c1).min(MAX_COLUMNS - 1),
    };
    (range.r0 < MAX_ROWS && range.c0 < MAX_COLUMNS).then_some(range)
}

/// A number as Excel's General format shows it: an integer when it is one,
/// otherwise the shortest decimal that reads back to the same value — which
/// is how `6.7400000000000002` in the file is `6.74` on screen. Fifteen
/// significant figures at most, as the spreadsheet holds and shows a
/// number: a computed value's last binary digits are not on screen, and
/// `0.062432000000000015` (`6.2432000000000015E-2` in the file) is
/// `0.062432`.
pub fn format_general(v: f64) -> String {
    let v = if v.is_finite() && v != 0.0 {
        format!("{v:.14e}").parse().unwrap_or(v)
    } else {
        v
    };
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Civil date from days since 1970-01-01 (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The serial of 9999-12-31, the last day Excel shows a date for, in the
/// 1900 system and in the 1904 one, which counts 1,462 days fewer to it.
const LAST_SERIAL: i64 = 2_958_465;
const LAST_SERIAL_1904: i64 = LAST_SERIAL - 1_462;

/// A serial date as the civil date it counts to and how far into that day
/// it is, in `per_day` parts, rounded. Excel counts days from 1899-12-30 in
/// the 1900 system — the day before 1900-01-01, so that serial 1 is New
/// Year's Day — and keeps a day that never happened, 1900-02-29, at serial
/// 60; a serial below 61 is one day off from that. The 1904 system counts
/// from 1904-01-01. `None` for a serial before the epoch, or past the
/// system's [`LAST_SERIAL`]: Excel shows no date past 9999-12-31, and a
/// serial near `i64::MAX` days would overflow the count.
fn civil_from_serial(v: f64, per_day: i64, date1904: bool) -> Option<((i64, u32, u32), i64)> {
    let last = if date1904 {
        LAST_SERIAL_1904
    } else {
        LAST_SERIAL
    };
    if v < 0.0 || !v.is_finite() || v >= (last + 1) as f64 {
        return None;
    }
    let days = v.floor() as i64;
    let part = ((v - v.floor()) * per_day as f64).round() as i64;
    let (days, part) = if part >= per_day {
        (days + 1, 0)
    } else {
        (days, part)
    };
    // Rounded up into the day after the last.
    if days > last {
        return None;
    }
    // Days between 1970-01-01 and each epoch.
    let epoch = if date1904 {
        -24_107
    } else if days >= 61 {
        -25_569
    } else {
        -25_568
    };
    Some((civil_from_days(days + epoch), part))
}

/// A serial date as ISO text, to the second.
pub fn format_serial(v: f64, fmt: NumFmt, date1904: bool) -> String {
    let Some(((y, m, d), secs)) = civil_from_serial(v, 86_400, date1904) else {
        return format_general(v);
    };
    let date = format!("{y:04}-{m:02}-{d:02}");
    let time = format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    );
    match fmt {
        NumFmt::Date => date,
        NumFmt::Time => time,
        _ => format!("{date} {time}"),
    }
}

/// Render a numeric cell's cached value through its style.
pub fn format_number(raw: &str, style: &CellStyle, date1904: bool) -> String {
    let Ok(v) = raw.trim().parse::<f64>() else {
        return raw.trim().to_string();
    };
    match &*style.format {
        NumFmt::General => format_general(v),
        NumFmt::Percent(decimals) => format!("{:.*}%", decimals, v * 100.0),
        NumFmt::Number(f) => f.format(v),
        fmt => format_serial(v, fmt.clone(), date1904),
    }
}

/// What a cell stores, typed, where its type is not text: a number at full
/// precision whatever its format shows, a date or time as the moment its
/// serial counts to, a boolean. Strings and errors have none.
pub fn cell_datum(kind: &str, value: &str, style: &CellStyle, date1904: bool) -> Option<Datum> {
    let v = value.trim();
    match kind {
        "b" => match v {
            "1" | "true" => Some(Datum::new("xsd:boolean", "true")),
            "0" | "false" => Some(Datum::new("xsd:boolean", "false")),
            _ => None,
        },
        // An ISO 8601 date, time or both, as the file writes it, typed by
        // what it is and written as XML Schema writes it. One that is not
        // a real date has none: typed, it would fail a store's insert.
        "d" => fluree_doc_model::xsd_temporal(v).map(|(ty, v)| Datum::new(ty, v)),
        "" | "n" => {
            let n = v.parse::<f64>().ok().filter(|n| n.is_finite())?;
            let serial = match *style.format {
                NumFmt::Date | NumFmt::Time | NumFmt::DateTime => {
                    serial_datum(n, &style.format, date1904)
                }
                _ => None,
            };
            // Rust writes a float as the shortest decimal that reads back
            // to it, never in exponent form: an `xsd:decimal` as it is.
            Some(serial.unwrap_or_else(|| Datum::new("xsd:decimal", format!("{n}"))))
        }
        _ => None,
    }
}

/// A serial as the moment it counts to, to the millisecond, whatever part
/// of it the format shows: a date format over a serial with a time of day
/// keeps the time. A time format over a day or more says the file does not
/// say which it is — a timestamp showing its time, or an elapsed duration —
/// and has none, so the number stands.
fn serial_datum(v: f64, fmt: &NumFmt, date1904: bool) -> Option<Datum> {
    let ((y, m, d), ms) = civil_from_serial(v, 86_400_000, date1904)?;
    let date = format!("{y:04}-{m:02}-{d:02}");
    let secs = ms / 1000;
    let mut time = format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    );
    if ms % 1000 != 0 {
        time.push_str(&format!(".{:03}", ms % 1000));
    }
    match fmt {
        NumFmt::Time if v < 1.0 => Some(Datum::new("xsd:time", time)),
        NumFmt::Time => None,
        NumFmt::Date if ms == 0 => Some(Datum::new("xsd:date", date)),
        _ => Some(Datum::new("xsd:dateTime", format!("{date}T{time}"))),
    }
}

/// Everything the sheet XML says that the layout needs.
#[derive(Debug, Default)]
struct Sheet {
    cells: BTreeMap<(u32, u32), Cell>,
    merges: Vec<Range>,
    /// Rows frozen at the top by a `pane`, which is how a sheet declares
    /// its header rows.
    frozen_rows: u32,
    /// `autoFilter` and table-part ranges: the header row of each is its
    /// first row.
    filtered: Vec<Range>,
    /// The relationship naming the sheet's drawing, where its pictures
    /// are anchored.
    drawing: Option<String>,
}

/// Parse one sheet's XML into elements tagged with `page`.
pub fn parse_sheet_xml(
    name: &str,
    xml: &str,
    page: usize,
    ctx: &Context<'_>,
) -> Result<Vec<Element>, XlsxError> {
    let mut budget = Budget::default();
    let sheet = read_sheet(xml, ctx, &mut budget)?;
    Ok(sheet_elements(name, &sheet, page, Vec::new(), &mut budget)?)
}

/// A sheet's elements: its name, then its cells laid out with the figures
/// anchored among them, each by the cell it is anchored to.
fn sheet_elements(
    name: &str,
    sheet: &Sheet,
    page: usize,
    figures: Vec<((u32, u32), Element)>,
    budget: &mut Budget,
) -> Result<Vec<Element>, TooLarge> {
    budget.spend_elements(1)?;
    budget.spend_text(name)?;
    let mut out = vec![element(
        "doco:SectionTitle",
        name.to_string(),
        Some(1),
        page,
    )];
    out.extend(layout(sheet, page, figures, budget)?);
    Ok(out)
}

/// A sheet's cells, each and its text spent from `budget` before it is
/// kept: a shared string is copied into every cell that names it.
fn read_sheet(xml: &str, ctx: &Context<'_>, budget: &mut Budget) -> Result<Sheet, XlsxError> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut sheet = Sheet::default();

    let mut row: u32 = 0;
    let mut next_row: u32 = 0;
    let mut col: u32 = 0;
    // The cell being read: position, type, style, and the text of its
    // value or inline string as it arrives.
    let mut cur: Option<((u32, u32), String, Option<usize>)> = None;
    let mut value = String::new();
    let mut in_v = false;
    let mut in_is_t = false;
    let mut in_is = false;

    loop {
        let event = r.read_event_into(&mut buf);
        let (e, empty) = match event {
            Err(e) => return Err(XlsxError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => (e, false),
            Ok(Event::Empty(e)) => (e, true),
            Ok(Event::Text(t)) => {
                if in_v || in_is_t {
                    value.push_str(&t.unescape().unwrap_or_default());
                }
                buf.clear();
                continue;
            }
            Ok(Event::End(e)) => {
                match local(e.name().as_ref()) {
                    "v" => in_v = false,
                    "is" => in_is = false,
                    "t" => in_is_t = false,
                    "c" => {
                        // A cell past the grid, which Excel would not
                        // open, is no cell.
                        if let Some((pos, kind, style)) = cur
                            .take()
                            .filter(|(p, ..)| p.0 < MAX_ROWS && p.1 < MAX_COLUMNS)
                        {
                            let st = ctx.styles.get(style);
                            // A shared string before it is copied; any other
                            // text, at most a format's few literals over the
                            // value, once made.
                            if let Some(s) = (kind == "s")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                                .and_then(|i| ctx.shared.get(i))
                            {
                                budget.spend_text(s)?;
                            }
                            let text = cell_text(&kind, &value, &st, ctx);
                            if kind != "s" {
                                budget.spend_text(&text)?;
                            }
                            if !text.is_empty() {
                                budget.spend_cells(1)?;
                                sheet.cells.insert(
                                    pos,
                                    Cell {
                                        text,
                                        datum: cell_datum(&kind, &value, &st, ctx.date1904),
                                        bold: st.bold,
                                        size: st.size,
                                    },
                                );
                            }
                        }
                        value.clear();
                    }
                    _ => {}
                }
                buf.clear();
                continue;
            }
            Ok(_) => {
                buf.clear();
                continue;
            }
        };
        let tag = local(e.name().as_ref()).to_string();
        match tag.as_str() {
            "row" => {
                row = attr(&e, "r")
                    .and_then(|v| v.parse::<u32>().ok())
                    .and_then(|v| v.checked_sub(1))
                    .unwrap_or(next_row);
                next_row = row.saturating_add(1);
                col = 0;
            }
            "c" => {
                let pos = attr(&e, "r")
                    .and_then(|v| cell_ref(&v))
                    .unwrap_or((row, col));
                col = pos.1.saturating_add(1);
                // `<c r="B1" s="19"/>` is a styled empty cell: no
                // End event will follow, and it holds nothing.
                if !empty {
                    let kind = attr(&e, "t").unwrap_or_default();
                    let style = attr(&e, "s").and_then(|v| v.parse::<usize>().ok());
                    cur = Some((pos, kind, style));
                    value.clear();
                }
            }
            "v" => in_v = true,
            "is" => in_is = true,
            "t" if in_is => in_is_t = true,
            "mergeCell" => {
                if let Some(range) = attr(&e, "ref").and_then(|v| parse_range(&v)) {
                    sheet.merges.push(range);
                }
            }
            "pane" => {
                let frozen = attr(&e, "state").is_some_and(|s| s == "frozen" || s == "frozenSplit");
                if frozen {
                    sheet.frozen_rows = attr(&e, "ySplit")
                        .and_then(|v| v.parse::<f64>().ok())
                        .map(|v| v as u32)
                        .unwrap_or(0);
                }
            }
            "autoFilter" => {
                if let Some(range) = attr(&e, "ref").and_then(|v| parse_range(&v)) {
                    sheet.filtered.push(range);
                }
            }
            "drawing" => sheet.drawing = attr(&e, "id"),
            _ => {}
        }
        buf.clear();
    }
    Ok(sheet)
}

/// A cell's display text from its type, cached value and style.
fn cell_text(kind: &str, value: &str, st: &CellStyle, ctx: &Context<'_>) -> String {
    let text = match kind {
        "s" => value
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|i| ctx.shared.get(i).cloned())
            .unwrap_or_default(),
        "inlineStr" | "str" | "d" => value.to_string(),
        "b" => match value.trim() {
            "1" | "true" => "TRUE".into(),
            "0" | "false" => "FALSE".into(),
            other => other.to_string(),
        },
        "e" => value.trim().to_string(),
        _ => format_number(value, st, ctx.date1904),
    };
    // A cell's hard line breaks become spaces, as a Word cell's do: the
    // text projection joins a row's cells with tabs and its rows with
    // newlines, and a newline inside a cell would break that row.
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lay a sheet's cells out as elements: islands of touching cells, each a
/// table with its title peeled off the top and its note off the bottom.
///
/// `figures` are set among them by the cell each is anchored to: after the
/// elements whose first cell comes before it, in rows then columns, as an
/// island is placed by its first cell.
fn layout(
    sheet: &Sheet,
    page: usize,
    figures: Vec<((u32, u32), Element)>,
    budget: &mut Budget,
) -> Result<Vec<Element>, TooLarge> {
    // Merged ranges show their anchor and hide the rest. The hidden cells
    // still occupy their positions — they are part of what the range
    // covers — but carry no text of their own.
    let merges = bounded_merges(sheet);
    // Each cell a merge covers is an entry of two maps; and the cells are
    // copied once.
    let covered = merges
        .iter()
        .fold(0u64, |n, m| n.saturating_add(m.height() * m.width()));
    budget.spend_cells(covered.saturating_mul(2))?;
    budget.spend_cells(sheet.cells.len() as u64)?;
    let mut cells: BTreeMap<(u32, u32), Cell> = sheet.cells.clone();
    let mut merge_of: HashMap<(u32, u32), Range> = HashMap::new();
    for m in &merges {
        for r in m.r0..=m.r1 {
            for c in m.c0..=m.c1 {
                if (r, c) != (m.r0, m.c0) {
                    cells.remove(&(r, c));
                }
                merge_of.insert((r, c), *m);
            }
        }
    }
    let mut occupied: HashSet<(u32, u32)> = cells.keys().copied().collect();
    for m in &merges {
        if cells.contains_key(&(m.r0, m.c0)) {
            for r in m.r0..=m.r1 {
                for c in m.c0..=m.c1 {
                    occupied.insert((r, c));
                }
            }
        }
    }

    // The sheet's body size: what most text is set in, so a larger cell
    // reads as display type.
    let modal_size = {
        let mut hist: HashMap<u32, usize> = HashMap::new();
        for c in cells.values() {
            *hist.entry((c.size * 2.0).round() as u32).or_default() += c.text.len();
        }
        // Of two sizes setting as much text, the smaller, as for a PDF's
        // body; taken in the map's order, the choice changed between runs.
        hist.into_iter()
            .max_by_key(|&(k, n)| (n, std::cmp::Reverse(k)))
            .map(|(k, _)| k as f32 / 2.0)
            .unwrap_or(11.0)
    };

    let mut islands = islands(&occupied, budget)?;
    // Both compare islands pairwise, each comparison counted as work: past
    // a few thousand, a sheet is no invoice whose totals they mend.
    if islands.len() <= MAX_MENDED_ISLANDS {
        absorb_fragments(&mut islands, budget)?;
        join_labels_to_amounts(&mut islands, &cells, modal_size, budget)?;
    }
    islands.sort_by_key(|isl| (isl.range.r0, isl.range.c0));

    let mut placed: Vec<((u32, u32), Element)> = Vec::new();
    let mut grids = 0u64;
    for isl in islands {
        // A table is laid out as a grid over its range: an island whose
        // range is mostly empty (cells set down a diagonal) would ask for
        // its square. Past the bound, its cells are read as paragraphs.
        let area = isl.range.height() * isl.range.width();
        grids = grids.saturating_add(area);
        if grids > MAX_LAID_OUT_CELLS {
            placed.extend(island_paragraphs(&isl, &cells, page, budget)?);
            continue;
        }
        // Its rows are read across its range, a cell at a time.
        budget.spend_work(area)?;
        placed.extend(island_elements(
            &isl, &cells, &merge_of, sheet, modal_size, page, budget,
        )?);
    }
    // After the islands: a figure anchored to an island's first cell
    // follows it.
    placed.extend(figures);
    placed.sort_by_key(|(at, _)| *at);
    Ok(placed.into_iter().map(|(_, e)| e).collect())
}

/// The most cells a sheet's merged ranges may cover, each clipped to the
/// cells the sheet holds; merges past it are read as unmerged.
const MAX_MERGED_CELLS: u64 = 1 << 20;

/// The most islands the pairwise mending of a sheet's blocks looks at: past
/// it, a sheet is no invoice whose totals need mending, and its islands are
/// left as found rather than the workbook refused for the work.
const MAX_MENDED_ISLANDS: usize = 2_048;

/// The most cells the grids of a sheet's tables may span, summed; an island
/// past it is read as its cells' text.
const MAX_LAID_OUT_CELLS: u64 = 1 << 22;

/// A sheet's merged ranges, each cut at the last row and column its cells
/// reach (what a merge covers past them holds nothing to read; its anchor
/// stays where it is), as many as fit in [`MAX_MERGED_CELLS`]: a range a
/// few bytes declare can otherwise cover every cell of the grid, each one
/// a cell laid out.
fn bounded_merges(sheet: &Sheet) -> Vec<Range> {
    let Some((r1, c1)) = sheet
        .cells
        .keys()
        .fold(None, |e: Option<(u32, u32)>, &(r, c)| {
            Some(e.map_or((r, c), |(r1, c1)| (r1.max(r), c1.max(c))))
        })
    else {
        return Vec::new();
    };
    let mut covered = 0u64;
    let mut out = Vec::new();
    for m in &sheet.merges {
        if m.r0 > r1 || m.c0 > c1 {
            continue;
        }
        let m = Range {
            r1: m.r1.min(r1),
            c1: m.c1.min(c1),
            ..*m
        };
        covered = covered.saturating_add(m.height() * m.width());
        if covered > MAX_MERGED_CELLS {
            break;
        }
        out.push(m);
    }
    out
}

/// An element and the cell it is placed by.
type Placed = ((u32, u32), Element);

/// An island too large to lay out as a grid: its cells' text, a paragraph
/// each, in rows then columns.
fn island_paragraphs(
    isl: &Island,
    cells: &BTreeMap<(u32, u32), Cell>,
    page: usize,
    budget: &mut Budget,
) -> Result<Vec<Placed>, TooLarge> {
    budget.spend_elements(isl.members.len() as u64)?;
    let mut at: Vec<(u32, u32)> = isl.members.iter().copied().collect();
    at.sort_unstable();
    Ok(at
        .into_iter()
        .filter_map(|p| cells.get(&p).map(|c| (p, c)))
        .map(|(p, c)| (p, element("doco:Paragraph", c.text.clone(), None, page)))
        .collect())
}

struct Island {
    members: HashSet<(u32, u32)>,
    range: Range,
    /// Empty columns inside `range` that are not part of the island: the
    /// gap between labels and the amounts they were joined to.
    gaps: Vec<u32>,
}

/// Connected components of occupied cells, four-connected: an empty row or
/// column between two blocks separates them. Each island is spent as two
/// cells of `budget` (its set of cells and its rows, measured at about
/// 250 bytes for a lone cell): a sheet of cells apart from each other is as
/// many islands.
fn islands(occupied: &HashSet<(u32, u32)>, budget: &mut Budget) -> Result<Vec<Island>, TooLarge> {
    let mut seen: HashSet<(u32, u32)> = HashSet::new();
    let mut out = Vec::new();
    let mut seeds: Vec<(u32, u32)> = occupied.iter().copied().collect();
    seeds.sort_unstable();
    for seed in seeds {
        if seen.contains(&seed) {
            continue;
        }
        budget.spend_cells(2)?;
        let mut members = HashSet::new();
        let mut queue = VecDeque::from([seed]);
        seen.insert(seed);
        let mut range = Range {
            r0: seed.0,
            c0: seed.1,
            r1: seed.0,
            c1: seed.1,
        };
        while let Some((r, c)) = queue.pop_front() {
            members.insert((r, c));
            range.r0 = range.r0.min(r);
            range.r1 = range.r1.max(r);
            range.c0 = range.c0.min(c);
            range.c1 = range.c1.max(c);
            // Checked: the first row and column have no neighbour before
            // them, nor the last one after.
            let neighbours = [
                r.checked_sub(1).map(|r| (r, c)),
                r.checked_add(1).map(|r| (r, c)),
                c.checked_sub(1).map(|c| (r, c)),
                c.checked_add(1).map(|c| (r, c)),
            ];
            for n in neighbours.into_iter().flatten() {
                if occupied.contains(&n) && seen.insert(n) {
                    queue.push_back(n);
                }
            }
        }
        out.push(Island {
            members,
            range,
            gaps: Vec::new(),
        });
    }
    Ok(out)
}

/// Fold into a block the pieces of it that only a missing cell set apart.
///
/// Rows of totals under a line table carry a label in column A and an
/// amount in the last column with nothing between. While every row has its
/// amount, the amount column joins them into the table; a row whose amount
/// is missing (`TOTAL` left blank) breaks that column, and the amounts
/// below it become an island of their own, cut from their labels. An
/// island inside another's columns, on rows where the other has cells, is
/// part of it.
///
/// Each island's rows are a set made once and grown with it, and the
/// islands are passed over in order until a pass mends none, each test of
/// one island against another spent from `budget` as work: no pass starts
/// over after a mend, which made a sheet of a thousand fragments under a
/// thousand pairs minutes of work.
fn absorb_fragments(islands: &mut Vec<Island>, budget: &mut Budget) -> Result<(), TooLarge> {
    let n = islands.len();
    budget.spend_work(islands.iter().map(|isl| isl.members.len() as u64).sum())?;
    let mut rows: Vec<HashSet<u32>> = islands
        .iter()
        .map(|isl| isl.members.iter().map(|(r, _)| *r).collect())
        .collect();
    let mut gone = vec![false; n];
    loop {
        let mut mended = false;
        for i in 0..n {
            if gone[i] {
                continue;
            }
            let mut host = None;
            for j in 0..n {
                budget.spend_work(1)?;
                let (inner, outer) = (&islands[i], &islands[j]);
                if j == i
                    || gone[j]
                    || outer.members.len() <= inner.members.len()
                    || inner.range.c0 < outer.range.c0
                    || inner.range.c1 > outer.range.c1
                    || rows[i].len() > rows[j].len()
                {
                    continue;
                }
                budget.spend_work(rows[i].len() as u64)?;
                if rows[i].iter().all(|r| rows[j].contains(r)) {
                    host = Some(j);
                    break;
                }
            }
            let Some(j) = host else {
                continue;
            };
            let inner = std::mem::take(&mut islands[i].members);
            let range = islands[i].range;
            let outer = &mut islands[j];
            outer.members.extend(inner);
            outer.range.r0 = outer.range.r0.min(range.r0);
            outer.range.r1 = outer.range.r1.max(range.r1);
            let moved = std::mem::take(&mut rows[i]);
            rows[j].extend(moved);
            gone[i] = true;
            mended = true;
        }
        if !mended {
            break;
        }
    }
    let mut k = 0;
    islands.retain(|_| {
        k += 1;
        !gone[k - 1]
    });
    Ok(())
}

/// Join a column of labels to the amounts set apart from it on the same
/// rows.
///
/// An invoice's totals are written as `Subtotal` in column A and its amount
/// under the line totals in column D, with nothing between: two islands,
/// each a column, and read apart they are a list of labels and a list of
/// numbers that no longer say which is which. They are one block: the same
/// rows, a label on each, an amount on each. So a single-column island of
/// text is joined to the nearest island on its right when that island
/// occupies exactly the same rows and holds nothing but amounts, and the
/// empty columns between them are dropped from the table.
///
/// Side-by-side tables, a heading beside a date, an address beside an
/// address do not join: each fails one of the shape, the alignment or the
/// type of the right-hand cells.
fn join_labels_to_amounts(
    islands: &mut Vec<Island>,
    cells: &BTreeMap<(u32, u32), Cell>,
    modal_size: f32,
    budget: &mut Budget,
) -> Result<(), TooLarge> {
    let rows_of = |isl: &Island| -> Vec<u32> {
        let mut rows: Vec<u32> = isl.members.iter().map(|(r, _)| *r).collect();
        rows.sort_unstable();
        rows.dedup();
        rows
    };
    let texts = |isl: &Island| -> Vec<&Cell> {
        let mut at: Vec<&(u32, u32)> = isl.members.iter().collect();
        at.sort_unstable();
        at.into_iter().filter_map(|p| cells.get(p)).collect()
    };
    let mut i = 0;
    while i < islands.len() {
        // A pass over the islands, and the left one's cells sorted.
        budget.spend_work(islands.len() as u64 + islands[i].members.len() as u64)?;
        let left = &islands[i];
        let labels = texts(left);
        let is_labels = left.range.width() == 1
            && !labels.is_empty()
            && labels.len() as u64 == left.range.height()
            && labels
                .iter()
                .all(|c| !is_amount(&c.text) && c.size < modal_size * DISPLAY_SIZE_RATIO);
        // The nearest island to the right that shares any of these rows.
        let right = is_labels
            .then(|| {
                islands
                    .iter()
                    .enumerate()
                    .filter(|(j, o)| {
                        *j != i
                            && o.range.c0 > left.range.c1
                            && o.range.r0 <= left.range.r1
                            && o.range.r1 >= left.range.r0
                    })
                    .min_by_key(|(_, o)| o.range.c0)
                    .map(|(j, _)| j)
            })
            .flatten()
            .filter(|&j| {
                let o = &islands[j];
                let amounts = texts(o);
                rows_of(o) == rows_of(left)
                    && !amounts.is_empty()
                    && amounts.iter().all(|c| is_amount(&c.text))
            });
        let Some(j) = right else {
            i += 1;
            continue;
        };
        budget.spend_work(islands[j].members.len() as u64)?;
        let other = islands.remove(j);
        let at = if j < i { i - 1 } else { i };
        let left = &mut islands[at];
        left.gaps.extend(left.range.c1 + 1..other.range.c0);
        left.gaps.extend(other.gaps);
        left.members.extend(other.members);
        left.range.r0 = left.range.r0.min(other.range.r0);
        left.range.r1 = left.range.r1.max(other.range.r1);
        left.range.c1 = left.range.c1.max(other.range.c1);
        i = at + 1;
    }
    Ok(())
}

/// A cell showing an amount: digits dressed as a number format dresses
/// them — sign or parentheses, grouping, a decimal point, a percent sign,
/// a currency symbol, or a three-letter currency code beside it.
fn is_amount(s: &str) -> bool {
    let mut t = s.trim();
    for part in [t.get(..4), t.get(t.len().saturating_sub(4)..)] {
        if let Some(code) = part
            .map(str::trim)
            .filter(|p| p.len() == 3 && p.chars().all(|c| c.is_ascii_uppercase()))
        {
            t = t.trim_start_matches(code).trim_end_matches(code).trim();
        }
    }
    let t = t.strip_prefix(['-', '+', '\u{2212}', '(']).unwrap_or(t);
    let t = t.strip_suffix(')').unwrap_or(t);
    t.chars().any(|c| c.is_ascii_digit())
        && t.chars().all(|c| {
            c.is_ascii_digit()
                || matches!(c, '$' | '€' | '£' | '¥' | '%' | ',' | '.' | ' ' | '\u{a0}')
        })
}

/// The anchors an island's row holds — cells with text, at their own
/// position or as the anchor of a merge — within the island.
fn row_anchors<'a>(
    isl: &Island,
    cells: &'a BTreeMap<(u32, u32), Cell>,
    r: u32,
) -> Vec<(u32, &'a Cell)> {
    (isl.range.c0..=isl.range.c1)
        .filter(|c| isl.members.contains(&(r, *c)))
        .filter_map(|c| cells.get(&(r, c)).map(|cell| (c, cell)))
        .collect()
}

/// Longest text that can be a title rather than a paragraph, in words.
const MAX_TITLE_WORDS: usize = 12;

/// Display type is set at least this much larger than the body.
const DISPLAY_SIZE_RATIO: f32 = 1.15;

fn title_like(cell: &Cell, modal_size: f32) -> bool {
    (cell.bold || cell.size >= modal_size * DISPLAY_SIZE_RATIO)
        && cell.text.split_whitespace().count() <= MAX_TITLE_WORDS
}

fn island_elements(
    isl: &Island,
    cells: &BTreeMap<(u32, u32), Cell>,
    merge_of: &HashMap<(u32, u32), Range>,
    sheet: &Sheet,
    modal_size: f32,
    page: usize,
    budget: &mut Budget,
) -> Result<Vec<Placed>, TooLarge> {
    let mut out = Vec::new();
    let mut top = isl.range.r0;
    let mut bottom = isl.range.r1;

    // A lone cell is a title when it is set as one, else a paragraph. So
    // is a column of them: lines stacked in one column — a receipt's
    // title, the shop, its address, the date — are text set down a sheet,
    // not a table of one column whose first line names the rest. The
    // first set as display type is the title; the rest are paragraphs.
    if isl.range.width() == 1 {
        let mut titled = false;
        for r in top..=bottom {
            if let Some((_, cell)) = row_anchors(isl, cells, r).first() {
                let title = !titled && title_like(cell, modal_size);
                titled |= title;
                let (kind, level) = if title {
                    ("doco:SectionTitle", Some(2))
                } else {
                    ("doco:Paragraph", None)
                };
                budget.spend_elements(1)?;
                out.push((
                    (r, isl.range.c0),
                    element(kind, cell.text.clone(), level, page),
                ));
            }
        }
        return Ok(out);
    }

    // Peel single-cell rows off the top: the first that is set as display
    // type is the island's title, the rest are paragraphs above the table.
    let mut titled = false;
    while top <= bottom {
        let anchors = row_anchors(isl, cells, top);
        match anchors.as_slice() {
            [] => top += 1,
            [(c, cell)] if isl.range.width() > 1 => {
                let title = !titled && title_like(cell, modal_size);
                titled |= title;
                let (kind, level) = if title {
                    ("doco:SectionTitle", Some(2))
                } else {
                    ("doco:Paragraph", None)
                };
                budget.spend_elements(1)?;
                out.push(((top, *c), element(kind, cell.text.clone(), level, page)));
                top += 1;
            }
            _ => break,
        }
    }
    // And off the bottom: a lone cell under a table is its note. A table
    // is at least two rows, so a two-row block keeps its second row even
    // when that row holds one cell.
    while bottom > top {
        let anchors = row_anchors(isl, cells, bottom);
        match anchors.as_slice() {
            [] => bottom -= 1,
            [(c, cell)] if isl.range.width() > 1 && bottom - top >= 2 => {
                budget.spend_elements(1)?;
                out.push((
                    (bottom, *c),
                    element("doco:Paragraph", cell.text.clone(), None, page),
                ));
                bottom -= 1;
            }
            _ => break,
        }
    }
    if top > bottom {
        return Ok(out);
    }

    // The table: trim columns the remaining rows leave empty, then read the
    // grid with merges as continuations.
    let (mut c0, mut c1) = (isl.range.c1, isl.range.c0);
    for r in top..=bottom {
        for (c, _) in row_anchors(isl, cells, r) {
            let m = merge_of.get(&(r, c)).copied();
            c0 = c0.min(c);
            c1 = c1.max(m.map_or(c, |m| m.c1.min(isl.range.c1)));
        }
    }
    if c1 < c0 {
        return Ok(out);
    }
    let range = Range {
        r0: top,
        c0,
        r1: bottom,
        c1,
    };
    // The table's columns: the range, less any gap a join left inside it.
    let gaps: HashSet<u32> = isl.gaps.iter().copied().collect();
    let cols: Vec<u32> = (range.c0..=range.c1)
        .filter(|c| !gaps.contains(c))
        .collect();
    let n_rows = range.height() as usize;
    let width = cols.len();
    // The grid, its values and flags, and its rows, the table, and the
    // cells that hold text, each a cell of the output: before any is made.
    // A cell is a node of its own in DoCO, written once in its row in the
    // others, and costs what its output makes of it.
    budget.spend_cells((n_rows * width) as u64)?;
    let filled = isl.members.iter().filter(|p| cells.contains_key(p)).count();
    budget.spend_elements(1)?;
    budget.spend_text_cells(filled as u64)?;
    let mut grid = vec![String::new(); n_rows * width];
    let mut data: Vec<Option<Datum>> = vec![None; n_rows * width];
    let mut m_left = vec![false; n_rows * width];
    let mut m_down = vec![false; n_rows * width];
    let mut bold = vec![false; n_rows * width];
    for r in range.r0..=range.r1 {
        for (k, &c) in cols.iter().enumerate() {
            if !isl.members.contains(&(r, c)) {
                continue;
            }
            let i = (r - range.r0) as usize * width + k;
            if let Some(cell) = cells.get(&(r, c)) {
                grid[i] = cell.text.clone();
                data[i] = cell.datum.clone();
                bold[i] = cell.bold;
            }
            if let Some(m) = merge_of.get(&(r, c)) {
                if c > m.c0 && c > range.c0 {
                    m_left[i] = true;
                }
                if r > m.r0 && r > range.r0 && c == m.c0.max(range.c0) {
                    m_down[i] = true;
                }
            }
        }
    }
    let rows: Vec<Vec<String>> = (0..n_rows)
        .map(|r| grid[r * width..(r + 1) * width].to_vec())
        .collect();
    if n_rows == 1 && width == 1 {
        let text = rows[0][0].clone();
        if !text.is_empty() {
            out.push((
                (range.r0, range.c0),
                element("doco:Paragraph", text, None, page),
            ));
        }
        return Ok(out);
    }

    let header_rows = header_rows(&range, &rows, &bold, (&m_left, &m_down), sheet);
    // Banner bands inside the body: one merged cell across the full width.
    let sub_headers: Vec<usize> = (header_rows..n_rows)
        .filter(|&r| {
            let row = &rows[r];
            let filled = row.iter().filter(|c| !c.is_empty()).count();
            filled == 1
                && !row[0].is_empty()
                && width > 1
                && (1..width).all(|c| m_left[r * width + c])
        })
        .collect();

    let text = rows
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = element("doco:Table", text, None, page);
    e.cells = Some(rows);
    e.datums = data
        .iter()
        .any(Option::is_some)
        .then(|| data.chunks(width).map(<[Option<Datum>]>::to_vec).collect());
    // Measured, so stated even when it is zero: absent would read as
    // undetected, and a consumer then presumes one.
    e.header_rows = Some(header_rows);
    e.sub_headers = (!sub_headers.is_empty()).then_some(sub_headers);
    e.merged_left = m_left.iter().any(|x| *x).then_some(m_left);
    e.merged_down = m_down.iter().any(|x| *x).then_some(m_down);
    // What DoCO copies of it: a merged value in each cell it covers.
    budget.spend_copies(&e)?;
    out.push(((range.r0, range.c0), e));
    Ok(out)
}

/// How many leading rows of a table are its header.
///
/// The sheet says so where it can: rows frozen by a pane, or the first row
/// of an autofilter range. Otherwise the first row is a header when it
/// names every column and is set bold, or names every column in text while
/// a later row carries numbers — a label row over data. A key/value block
/// whose first row is a label and a value has none.
///
/// A column a merged label covers is named by it, and a header found from
/// its type or weight runs on through the rows its merged cells reach
/// down into: `Item` merged over two rows beside `Price` spanning `Unit`
/// and `Line` is a two-row header.
fn header_rows(
    range: &Range,
    rows: &[Vec<String>],
    bold: &[bool],
    merged: (&[bool], &[bool]),
    sheet: &Sheet,
) -> usize {
    let n_rows = rows.len();
    let width = rows.first().map_or(0, |r| r.len());
    if n_rows < 2 || width == 0 {
        return 0;
    }
    if sheet.frozen_rows > range.r0 {
        let n = (sheet.frozen_rows - range.r0) as usize;
        if n < n_rows {
            return n;
        }
    }
    if sheet
        .filtered
        .iter()
        .any(|f| f.r0 == range.r0 && f.c0 <= range.c1 && f.c1 >= range.c0)
    {
        return 1;
    }
    let (m_left, m_down) = merged;
    let first = &rows[0];
    // The corner over a column of row labels is empty in a header that
    // names the other columns; set bold over a plain row, it is still one.
    let corner = width > 2
        && first[0].is_empty()
        && (1..width).all(|c| (!first[c].is_empty() && bold[c]) || m_left[c])
        && (0..width).any(|c| !rows[1][c].is_empty() && !bold[width + c]);
    if corner {
        return 1;
    }
    // A label column, bold down its first column and nothing else bold, is
    // a field list (`Number | R-13247` over `Date | 31.08.2026`): its first
    // row is a field like the rest, however its values are typed.
    let label_column = width >= 2
        && (0..n_rows).all(|r| bold[r * width] && !rows[r][0].is_empty())
        && (0..n_rows).all(|r| (1..width).all(|c| !bold[r * width + c]));
    if label_column {
        return 0;
    }
    if (0..width).any(|c| first[c].is_empty() && !m_left[c]) {
        return 0;
    }
    let numeric = |s: &str| s.parse::<f64>().is_ok() || is_amount(s);
    // A field list — `Description | Cinnamon bun` over `Code | BK-215`,
    // `Quantity | 1` — is two columns whose values are numbers only some
    // of the time; a label row over amounts has numbers all the way down.
    let data_below = if width == 2 {
        rows[1..]
            .iter()
            .filter(|r| !r[1].is_empty())
            .all(|r| numeric(&r[1]))
            && rows[1..].iter().any(|r| numeric(&r[1]))
    } else {
        rows[1..].iter().any(|r| r.iter().any(|c| numeric(c)))
    };
    let header = (0..width).all(|c| bold[c] || m_left[c]) || {
        let labels = first.iter().all(|c| !numeric(c));
        labels && data_below
    };
    if !header {
        return 0;
    }
    let mut n = 1;
    while n + 1 < n_rows && (0..width).any(|c| m_down[n * width + c]) {
        n += 1;
    }
    // A label merged across columns names them only as the banner of a
    // stacked header, over the row that names them one by one. A merged
    // first row with nothing reaching down beside it is a note or a title
    // set across the block.
    if n == 1 && m_left[..width].iter().any(|m| *m) {
        return 0;
    }
    n
}

fn element(kind: &str, text: String, level: Option<usize>, page: usize) -> Element {
    Element {
        page,
        level,
        ..Element::new(kind, text, "xlsx", "xlsx")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluree_doc_model::budget::{
        text_cost, Output, CELL_BYTES, ELEMENT_BYTES, FLAT_CELL_BYTES, FLAT_ELEMENT_BYTES,
        TEXT_BYTES,
    };

    /// [`grid_elements`] within the default budget.
    fn grid(sheets: Vec<GridSheet>, source: &'static str) -> Vec<Element> {
        grid_elements(sheets, source, &mut Budget::default()).unwrap()
    }

    #[test]
    fn a_grid_cell_past_excels_grid_is_no_cell() {
        let text_cell = |row, col, text: &str| GridCell {
            row,
            col,
            text: text.into(),
            datum: None,
            bold: false,
        };
        // An `.xlsb` cell record names its column in 32 bits: the last
        // column a u32 holds was the neighbour of column 0, one island over
        // every column, walked a row at a time.
        let sheets = vec![GridSheet {
            name: "S".into(),
            cells: vec![
                text_cell(0, 0, "a"),
                text_cell(0, u32::MAX, "far"),
                text_cell(1, 0, "b"),
                text_cell(1, 1, "c"),
                text_cell(MAX_ROWS, 0, "below"),
            ],
            merges: vec![(0, 0, 1, u32::MAX), (MAX_ROWS, 0, MAX_ROWS, 1)],
            frozen_rows: 0,
        }];
        let els = grid(sheets, "xlsb");
        let all: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();
        assert!(all.iter().any(|t| t.contains('a')), "{all:?}");
        assert!(
            all.iter()
                .all(|t| !t.contains("far") && !t.contains("below")),
            "{all:?}"
        );
    }

    #[test]
    fn a_grid_from_another_format_is_laid_out_as_a_sheet_is() {
        let cell = |row, col, text: &str, bold| GridCell {
            row,
            col,
            text: text.into(),
            datum: None,
            bold,
        };
        let sheets = vec![GridSheet {
            name: "Orders".into(),
            cells: vec![
                cell(0, 0, "Quarterly orders", true),
                cell(2, 0, "Customer", false),
                cell(2, 1, "Amount", false),
                cell(3, 0, "Alex  Sample", false),
                GridCell {
                    datum: Some(Datum::new("xsd:decimal", "12.5")),
                    ..cell(3, 1, "12.5", false)
                },
                cell(4, 0, "Region West", false),
                cell(5, 0, "Sam Example", false),
                cell(5, 1, "7", false),
            ],
            // `Region West` spans the table: a band inside it.
            merges: vec![(4, 0, 4, 1)],
            frozen_rows: 0,
        }];
        let els = grid(sheets, "xls");
        let kinds: Vec<(&str, &str)> = els
            .iter()
            .map(|e| (e.kind.as_str(), e.text.as_str()))
            .collect();
        assert_eq!(kinds[0], ("doco:SectionTitle", "Orders"));
        assert_eq!(kinds[1], ("doco:SectionTitle", "Quarterly orders"));
        let t = &els[2];
        assert_eq!(t.kind, "doco:Table");
        assert_eq!(t.cells.as_ref().unwrap()[1], ["Alex Sample", "12.5"]);
        assert_eq!(t.header_rows, Some(1));
        assert_eq!(t.sub_headers, Some(vec![2]));
        assert_eq!(
            t.datums.as_ref().unwrap()[1][1],
            Some(Datum::new("xsd:decimal", "12.5"))
        );
        assert!(els
            .iter()
            .all(|e| e.provenance == "xls" && e.evidence == "xls"));
        assert_eq!(els[2].id, "elem-00003");
    }

    fn text_cell(row: u32, col: u32, text: &str) -> GridCell {
        GridCell {
            row,
            col,
            text: text.into(),
            datum: None,
            bold: false,
        }
    }

    #[test]
    fn a_merge_is_laid_out_only_as_far_as_the_cells_reach() {
        // A merge cut where the cells end; one past them, dropped.
        let sheet = Sheet {
            cells: [((0, 0), "a"), ((3, 2), "b")]
                .into_iter()
                .map(|(at, t)| {
                    (
                        at,
                        Cell {
                            text: t.into(),
                            datum: None,
                            bold: false,
                            size: 11.0,
                        },
                    )
                })
                .collect(),
            merges: vec![
                Range {
                    r0: 0,
                    c0: 0,
                    r1: 1 << 20,
                    c1: 1 << 14,
                },
                Range {
                    r0: 5,
                    c0: 0,
                    r1: 9,
                    c1: 0,
                },
            ],
            ..Sheet::default()
        };
        assert_eq!(
            bounded_merges(&sheet),
            [Range {
                r0: 0,
                c0: 0,
                r1: 3,
                c1: 2
            }]
        );
        // One cell under a merge over the whole grid: four billion cells
        // to lay out, from a few bytes.
        let sheets = vec![GridSheet {
            name: "S".into(),
            cells: vec![text_cell(0, 0, "Total"), text_cell(1, 0, "7")],
            merges: vec![(0, 0, u32::MAX, u32::MAX), (5, 5, 9, 9)],
            frozen_rows: 0,
        }];
        let els = grid(sheets, "xls");
        assert!(els.iter().any(|e| e.text == "Total"), "{els:#?}");
    }

    #[test]
    fn merges_past_their_bound_are_read_unmerged() {
        // Overlapping merges, each the whole of a 1,024-square sheet: the
        // second passes the bound.
        let cells = [text_cell(0, 0, "a"), text_cell(1023, 1023, "z")];
        let sheet = Sheet {
            cells: cells
                .iter()
                .map(|c| {
                    (
                        (c.row, c.col),
                        Cell {
                            text: c.text.clone(),
                            datum: None,
                            bold: false,
                            size: 11.0,
                        },
                    )
                })
                .collect(),
            merges: vec![
                Range {
                    r0: 0,
                    c0: 0,
                    r1: 1023,
                    c1: 1023,
                };
                3
            ],
            ..Sheet::default()
        };
        assert_eq!(bounded_merges(&sheet).len(), 1);
    }

    #[test]
    fn an_island_too_wide_for_its_cells_is_read_as_paragraphs() {
        // A staircase: each cell touching the next, 3,000 rows by 3,001
        // columns of grid for 6,000 cells.
        let cells: Vec<GridCell> = (0..3_000u32)
            .flat_map(|i| [text_cell(i, i, "x"), text_cell(i, i + 1, "y")])
            .collect();
        let els = grid(
            vec![GridSheet {
                name: "S".into(),
                cells,
                ..GridSheet::default()
            }],
            "xls",
        );
        assert_eq!(els.len(), 6_001);
        assert!(els[1..].iter().all(|e| e.kind == "doco:Paragraph"));
    }

    #[test]
    fn a_sheet_of_many_islands_is_laid_out_without_mending_them() {
        // 10,000 cells alone on their rows over 10,000 pairs, every cell
        // inside a pair's columns: each is compared with every pair, a set
        // of rows built for both each time, a hundred million times.
        let cells: Vec<GridCell> = (0..10_000u32)
            .flat_map(|i| {
                [
                    text_cell(4 * i, 0, "label"),
                    text_cell(4 * i + 2, 0, "a"),
                    text_cell(4 * i + 2, 1, "b"),
                ]
            })
            .collect();
        let els = grid(
            vec![GridSheet {
                name: "S".into(),
                cells,
                ..GridSheet::default()
            }],
            "xls",
        );
        assert_eq!(els.len(), 20_001);
    }

    #[test]
    fn mending_fragments_takes_work_in_proportion_not_the_square() {
        // 500 pairs no island hosts, then a block whose rows hold 500 lone
        // cells, each a fragment it absorbs. Starting the mending over after
        // each mend tested the pairs again each time: 500 times 750,000
        // tests.
        let mut cells = Vec::new();
        for k in 0..500u32 {
            cells.extend([text_cell(2 * k, 10, "p"), text_cell(2 * k, 11, "q")]);
        }
        let base = 1_010u32;
        cells.extend((0..=100).map(|c| text_cell(base, c, "bar")));
        for r in base + 1..=base + 1_000 {
            cells.extend((0..20).map(|c| text_cell(r, c, "x")));
        }
        cells.extend((0..500).map(|k| text_cell(base + 1 + 2 * k, 80, "f")));
        let sheets = vec![GridSheet {
            name: "S".into(),
            cells,
            ..GridSheet::default()
        }];
        let mut budget = Budget::new(fluree_doc_model::budget::MAX_MEMORY, 20_000_000);
        let els = grid_elements(sheets, "xlsb", &mut budget).unwrap();
        // The title, the pairs, and the block with its fragments.
        assert_eq!(els.len(), 502);
    }

    #[test]
    fn a_layout_past_its_budget_is_refused() {
        // Cells apart from each other: an island, then an element, each.
        let apart: Vec<GridCell> = (0..1_000u32).map(|i| text_cell(2 * i, 0, "x")).collect();
        let sheets = || {
            vec![GridSheet {
                name: "S".into(),
                cells: apart.clone(),
                ..GridSheet::default()
            }]
        };
        let small = |n: u64| Budget::new(n * ELEMENT_BYTES, u64::MAX);
        assert!(grid_elements(sheets(), "xls", &mut small(2_000)).is_ok());
        let e = grid_elements(sheets(), "xls", &mut small(1_000)).unwrap_err();
        assert!(e.to_string().contains("past"), "{e}");
        // A table's grid, spent before it is made: 10,000 cells, after the
        // sheet's two copies of them.
        let table: Vec<GridCell> = (0..100u32)
            .flat_map(|r| (0..100).map(move |c| text_cell(r, c, "x")))
            .collect();
        let sheets = || {
            vec![GridSheet {
                name: "S".into(),
                cells: table.clone(),
                ..GridSheet::default()
            }]
        };
        // The sheet's title, the table and its cells, each a node of the
        // output, and the title's text, besides; and the labels DoCO copies
        // onto each cell below the header, its column's (a byte) and, past
        // the first, its row's (another).
        let labels = 99 * 100 + 99 * 99;
        let cells = |n: u64| {
            Budget::new(
                10_002 * ELEMENT_BYTES + (1 + labels) * TEXT_BYTES + n * CELL_BYTES,
                u64::MAX,
            )
        };
        assert!(grid_elements(sheets(), "xls", &mut cells(30_002)).is_ok());
        assert!(grid_elements(sheets(), "xls", &mut cells(25_000)).is_err());
        // Markdown writes each of the table's cells once, in its row: the
        // title and the table, each an element, and its cells, each a cell
        // of a row. Counted as DoCO's nodes, an ordinary export of 50,000
        // rows by 12 columns was refused in every format.
        let flat = |n: u64| {
            Budget::new(
                2 * FLAT_ELEMENT_BYTES + 10_000 * FLAT_CELL_BYTES + TEXT_BYTES + n * CELL_BYTES,
                u64::MAX,
            )
            .for_output(Output::Flat)
        };
        assert!(grid_elements(sheets(), "xls", &mut flat(30_002)).is_ok());
        assert!(grid_elements(sheets(), "xls", &mut flat(30_001)).is_err());
    }

    #[test]
    fn a_shared_string_counts_for_each_cell_that_names_it() {
        // 4,000 cells naming one string of 64 Ki characters: 256 MiB of
        // copies, 3 GiB counted with their output.
        let big = "y".repeat(64 << 10);
        let shared = [big];
        let styles = styles();
        let ctx = Context {
            shared: &shared,
            styles: &styles,
            date1904: false,
        };
        let xml = |n: u32| {
            let cells: String = (1..=n)
                .map(|i| format!("<row r=\"{i}\"><c r=\"A{i}\" t=\"s\"><v>0</v></c></row>"))
                .collect();
            format!("<worksheet {NS}><sheetData>{cells}</sheetData></worksheet>")
        };
        assert!(parse_sheet_xml("S", &xml(10), 0, &ctx).is_ok());
        assert!(matches!(
            parse_sheet_xml("S", &xml(4_000), 0, &ctx),
            Err(XlsxError::TooLarge(_))
        ));
    }

    #[test]
    fn each_shared_string_is_spent_before_it_is_kept() {
        // `<si/>` is five bytes of the part and a string in the table: two
        // hundred million of them, 1.5 MB zipped, took the process down.
        let xml = |n: usize| format!("<sst {NS}>{}<si><t>a</t></si></sst>", "<si/>".repeat(n - 1));
        let slots = |n: u64| Budget::new(n * CELL_BYTES, 0);
        let table = read_shared_strings(&xml(1_000), &mut slots(1_000)).unwrap();
        assert_eq!(table.len(), 1_000);
        assert_eq!(table[999], "a");
        assert!(matches!(
            read_shared_strings(&xml(1_000), &mut slots(999)),
            Err(XlsxError::TooLarge(_))
        ));
    }

    #[test]
    fn a_number_format_is_kept_once_however_many_styles_name_it() {
        // A format's literals named by every style: copied into each, a
        // megabyte of them under 5,000 styles, 3 KB zipped, took the
        // process down.
        let xml = |literal: &str, n: usize| {
            format!(
                "<styleSheet {NS}><numFmts count=\"1\"><numFmt numFmtId=\"200\" formatCode=\"&quot;{literal}&quot;0\"/></numFmts><cellXfs>{}</cellXfs></styleSheet>",
                "<xf numFmtId=\"200\"/><xf numFmtId=\"3\"/><xf numFmtId=\"9999\"/>".repeat(n)
            )
        };
        let st = parse_styles(&xml(&"A".repeat(1 << 20), 1_000)).unwrap();
        assert_eq!(st.xfs.len(), 3_000);
        assert!(st
            .xfs
            .chunks(3)
            .all(|x| { (0..3).all(|k| Arc::ptr_eq(&x[k].format, &st.xfs[k].format)) }));
        assert_eq!(format_number("7", &st.xfs[2_999], false), "7");
        assert_eq!(format_number("1234", &st.xfs[2_998], false), "1,234");
        // The format, its literals as text, and each style, spent before
        // they are kept.
        let format = CELL_BYTES + text_cost("\"AB\"0") * TEXT_BYTES;
        let spent = |n: u64| Budget::new(format + n * CELL_BYTES, 0);
        assert!(read_styles(&xml("AB", 100), &mut spent(300)).is_ok());
        assert!(matches!(
            read_styles(&xml("AB", 100), &mut spent(299)),
            Err(XlsxError::TooLarge(_))
        ));
    }

    #[test]
    fn each_sheet_listed_is_spent_before_it_is_kept() {
        let xml = format!(
            "<workbook {NS}><sheets>{}</sheets></workbook>",
            "<sheet name=\"Sheet\" r:id=\"rId1\"/>".repeat(100)
        );
        let each = CELL_BYTES + (text_cost("Sheet") + text_cost("rId1")) * TEXT_BYTES;
        let (sheets, _) = parse_workbook_xml(&xml, &mut Budget::new(100 * each, 0)).unwrap();
        assert_eq!(sheets.len(), 100);
        assert!(parse_workbook_xml(&xml, &mut Budget::new(100 * each - 1, 0)).is_err());
    }

    const NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    /// A cell: reference, type attribute (`""` for a number), style index,
    /// and the inner XML.
    fn c(r: &str, t: &str, s: usize, inner: &str) -> String {
        let t = if t.is_empty() {
            String::new()
        } else {
            format!(" t=\"{t}\"")
        };
        format!("<c r=\"{r}\"{t} s=\"{s}\">{inner}</c>")
    }

    fn text(r: &str, s: usize, value: &str) -> String {
        c(r, "inlineStr", s, &format!("<is><t>{value}</t></is>"))
    }

    fn num(r: &str, s: usize, value: &str) -> String {
        c(r, "", s, &format!("<v>{value}</v>"))
    }

    /// Rows are grouped by the leading digits of the references.
    fn sheet(cells: &[String], extra: &str) -> String {
        let mut rows: BTreeMap<u32, Vec<&String>> = BTreeMap::new();
        for cell in cells {
            let r = cell.split("r=\"").nth(1).unwrap();
            let r = r.split('"').next().unwrap();
            let (row, _) = cell_ref(r).unwrap();
            rows.entry(row).or_default().push(cell);
        }
        let body: String = rows
            .iter()
            .map(|(r, cells)| {
                format!(
                    "<row r=\"{}\">{}</row>",
                    r + 1,
                    cells.iter().map(|c| c.as_str()).collect::<String>()
                )
            })
            .collect();
        format!("<worksheet {NS}>{extra}<sheetData>{body}</sheetData></worksheet>")
    }

    /// Styles 0: body; 1: bold body; 2: bold 16pt; 3: 9pt; 4: percent with
    /// one decimal; 5: date; 6: date-time.
    fn styles() -> Styles {
        let st = |format, bold, size| CellStyle {
            format: Arc::new(format),
            bold,
            size,
        };
        Styles {
            xfs: vec![
                st(NumFmt::General, false, 11.0),
                st(NumFmt::General, true, 11.0),
                st(NumFmt::General, true, 16.0),
                st(NumFmt::General, false, 9.0),
                st(NumFmt::Percent(1), false, 11.0),
                st(NumFmt::Date, false, 11.0),
                st(NumFmt::DateTime, false, 11.0),
            ],
        }
    }

    fn parse_sheet(xml: &str) -> Vec<Element> {
        let styles = styles();
        let ctx = Context {
            shared: &[],
            styles: &styles,
            date1904: false,
        };
        parse_sheet_xml("Sheet1", xml, 3, &ctx).unwrap()
    }

    fn kinds(els: &[Element]) -> Vec<String> {
        els.iter()
            .map(|e| match e.level {
                Some(l) => format!("{}:{l}", e.kind),
                None => e.kind.clone(),
            })
            .collect()
    }

    #[test]
    fn a_cell_past_the_grid_is_no_cell() {
        // `MWLQKWU` is column 4,294,967,294: the cell after it, which names
        // none, is the last column a u32 holds, whose neighbour past it was
        // column 0, one island over four billion columns.
        let t = |r: &str, v: &str| {
            let r = if r.is_empty() {
                String::new()
            } else {
                format!(" r=\"{r}\"")
            };
            format!("<c{r} t=\"inlineStr\"><is><t>{v}</t></is></c>")
        };
        let xml = format!(
            "<worksheet {NS}><sheetData><row r=\"1\">{}{}{}</row><row r=\"2\">{}{}</row><row r=\"1048577\">{}</row></sheetData><mergeCells><mergeCell ref=\"C2:ZZZZ9999999\"/><mergeCell ref=\"XFE1:XFF2\"/></mergeCells></worksheet>",
            t("A1", "a"),
            t("MWLQKWU1", "far"),
            t("", "next"),
            t("A2", "b"),
            t("B2", "c"),
            t("A1048577", "below")
        );
        let els = parse_sheet(&xml);
        let all: String = els.iter().map(|e| e.text.as_str()).collect();
        assert!(all.contains('a') && all.contains('c'), "{els:#?}");
        for gone in ["far", "next", "below"] {
            assert!(!all.contains(gone), "{els:#?}");
        }
        assert_eq!(
            parse_range("C2:ZZZZ9999999"),
            Some(Range {
                r0: 1,
                c0: 2,
                r1: MAX_ROWS - 1,
                c1: MAX_COLUMNS - 1
            })
        );
        assert_eq!(parse_range("XFE1:XFF2"), None);
        // The corners have no neighbour past them.
        let corners: HashSet<(u32, u32)> =
            [(0, 0), (0, u32::MAX), (u32::MAX, 0)].into_iter().collect();
        let islands = islands(&corners, &mut Budget::default()).unwrap();
        assert_eq!(islands.len(), 3);
    }

    #[test]
    fn column_letters_map_to_indices() {
        assert_eq!(column_index("A"), Some(0));
        assert_eq!(column_index("Z"), Some(25));
        assert_eq!(column_index("AA"), Some(26));
        assert_eq!(column_index("AB"), Some(27));
        assert_eq!(column_index("ZZ"), Some(701));
        assert_eq!(column_index(""), None);
        assert_eq!(column_index("A1"), None);
        assert_eq!(cell_ref("B7"), Some((6, 1)));
        assert_eq!(cell_ref("AA100"), Some((99, 26)));
        assert_eq!(
            parse_range("C4:A1"),
            Some(Range {
                r0: 0,
                c0: 0,
                r1: 3,
                c1: 2
            })
        );
    }

    #[test]
    fn general_numbers_are_the_shortest_round_trip() {
        assert_eq!(
            format_general("6.7400000000000002".parse().unwrap()),
            "6.74"
        );
        assert_eq!(format_general(1876.0), "1876");
        assert_eq!(format_general(-3.0), "-3");
        assert_eq!(format_general(0.5), "0.5");
        assert_eq!(format_general(1e-7), "0.0000001");
        // A computed value's last binary digits: fifteen figures show.
        assert_eq!(
            format_general("6.2432000000000015E-2".parse().unwrap()),
            "0.062432"
        );
        assert_eq!(format_general(0.1 + 0.2), "0.3");
        assert_eq!(format_general(1.0 / 3.0), "0.333333333333333");
    }

    #[test]
    fn serial_dates_and_times_render_as_iso() {
        assert_eq!(format_serial(45123.0, NumFmt::Date, false), "2023-07-16");
        assert_eq!(
            format_serial(45123.75, NumFmt::DateTime, false),
            "2023-07-16 18:00:00"
        );
        assert_eq!(format_serial(0.5, NumFmt::Time, false), "12:00:00");
        assert_eq!(format_serial(1.0, NumFmt::Date, false), "1900-01-01");
        assert_eq!(format_serial(61.0, NumFmt::Date, false), "1900-03-01");
        assert_eq!(format_serial(0.0, NumFmt::Date, true), "1904-01-01");
        assert_eq!(format_serial(-1.0, NumFmt::Date, false), "-1");
        // Excel's last day, and past it a number: a serial of 1e300 days
        // overflowed the count.
        assert_eq!(
            format_serial(2_958_465.0, NumFmt::Date, false),
            "9999-12-31"
        );
        assert_eq!(format_serial(2_958_466.0, NumFmt::Date, false), "2958466");
        // The 1904 system reaches it 1,462 days sooner.
        assert_eq!(format_serial(2_957_003.0, NumFmt::Date, true), "9999-12-31");
        assert_eq!(format_serial(2_957_004.0, NumFmt::Date, true), "2957004");
        assert_eq!(serial_datum(2_957_004.0, &NumFmt::Date, true), None);
        // A time that rounds into the day after the last is no date either.
        assert_eq!(
            format_serial(2_958_465.999_999_9, NumFmt::DateTime, false),
            format_general(2_958_465.999_999_9)
        );
        assert_eq!(
            format_serial(1e300, NumFmt::Date, true),
            format_general(1e300)
        );
        assert_eq!(serial_datum(1e300, &NumFmt::Date, false), None);
    }

    #[test]
    fn custom_format_codes_classify_by_their_bare_letters() {
        assert_eq!(custom_fmt("yyyy-mm-dd"), NumFmt::Date);
        assert_eq!(custom_fmt("h:mm"), NumFmt::Time);
        assert_eq!(custom_fmt("d-mmm-yy h:mm"), NumFmt::DateTime);
        assert_eq!(custom_fmt("[$-409]mmm-yy;@"), NumFmt::Date);
        assert_eq!(custom_fmt("0.0%"), NumFmt::Percent(1));
        assert_eq!(custom_fmt("0%"), NumFmt::Percent(0));
        assert!(matches!(custom_fmt("#,##0.00"), NumFmt::Number(_)));
        assert!(matches!(custom_fmt("\"Days:\" 0"), NumFmt::Number(_)));
        assert_eq!(custom_fmt("0.00E+00"), NumFmt::General);
        assert_eq!(custom_fmt("[>100]0.00;0"), NumFmt::General);
        assert_eq!(custom_fmt("# ?/?"), NumFmt::General);
        assert_eq!(custom_fmt("General"), NumFmt::General);
        assert_eq!(custom_fmt("@"), NumFmt::General);
    }

    #[test]
    fn percent_cells_render_as_percentages() {
        let st = &styles().xfs[4];
        assert_eq!(format_number("0.125", st, false), "12.5%");
        assert_eq!(format_number("1", st, false), "100.0%");
    }

    #[test]
    fn numbers_show_as_their_format_displays_them() {
        let f = |code: &str, v: f64| match custom_fmt(code) {
            NumFmt::Number(n) => n.format(v),
            other => panic!("{code} read as {other:?}"),
        };
        assert_eq!(f("0.00", 14.5), "14.50");
        assert_eq!(f("0.00", 29.0), "29.00");
        assert_eq!(
            f("0.00", 2.675),
            "2.68",
            "half away from zero, as Excel shows it"
        );
        assert_eq!(
            f("0.00", -0.001),
            "0.00",
            "no minus on a value that rounds away"
        );
        assert_eq!(f("#,##0.00", 1234567.891), "1,234,567.89");
        assert_eq!(f("#,##0", 999.5), "1,000");
        assert_eq!(f("0.0#", 3.0), "3.0");
        assert_eq!(f("0.0#", 1.23456), "1.23");
        assert_eq!(f("#.00", 0.5), ".50");
        assert_eq!(f("000", 7.0), "007");
        assert_eq!(f("\"$\"#,##0.00", 1234.5), "$1,234.50");
        assert_eq!(f("\"$\"#,##0.00", -1234.5), "-$1,234.50");
        assert_eq!(f("[$€-407] #,##0.00", 12.0), "€ 12.00");
        assert_eq!(f("#,##0.00 [$EUR]", 12.0), "12.00 EUR");
        assert_eq!(f("#,##0.00\\ \"€\"", 12.0), "12.00 €");
        assert_eq!(f("#,##0.00_);[Red](#,##0.00)", -3.5), "(3.50)");
        let accounting = "_(\"$\"* #,##0.00_);_(\"$\"* \\(#,##0.00\\);_(\"$\"* \"-\"??_);_(@_)";
        assert_eq!(f(accounting, 1234.5), "$1,234.50");
        assert_eq!(f(accounting, -1234.5), "$(1,234.50)");
        assert_eq!(f(accounting, 0.0), "$-");
        assert_eq!(f("#,##0.0,,\"M\"", 2_500_000.0), "2.5M");
        assert_eq!(f("\"Days:\" 0", 3.0), "Days: 3");
        // Built-in ids name the common ones; a currency id shows no sign,
        // which is the reader's locale rather than the file's.
        let b = |id: u32, v: f64| match builtin_fmt(id) {
            NumFmt::Number(n) => n.format(v),
            other => panic!("{id} read as {other:?}"),
        };
        assert_eq!(b(2, 9.6), "9.60");
        assert_eq!(b(4, 38600.0), "38,600.00");
        assert_eq!(b(7, -42.46), "(42.46)");
        assert_eq!(b(44, 0.0), "-");
        assert_eq!(b(44, -3.0), "(3.00)");
        assert_eq!(builtin_fmt(0), NumFmt::General);
        assert_eq!(builtin_fmt(11), NumFmt::General);
    }

    #[test]
    fn shared_strings_join_runs_and_skip_phonetics() {
        let xml = format!(
            "<sst {NS}><si><t>plain</t></si><si><r><t>rich </t></r><r><rPr><b/></rPr><t>text</t></r></si><si><t>漢字</t><rPh><t>かんじ</t></rPh></si><si/><si><t xml:space=\"preserve\"> spaced </t></si></sst>"
        );
        let s = parse_shared_strings(&xml).unwrap();
        assert_eq!(s, vec!["plain", "rich text", "漢字", "", " spaced "]);
    }

    #[test]
    fn styles_resolve_fonts_and_formats_by_index() {
        let xml = format!(
            "<styleSheet {NS}><numFmts count=\"1\"><numFmt numFmtId=\"164\" formatCode=\"yyyy-mm-dd\"/></numFmts>\
             <fonts count=\"3\"><font><sz val=\"11\"/></font><font><b/><sz val=\"14\"/></font><font/></fonts>\
             <cellStyleXfs count=\"1\"><xf numFmtId=\"10\" fontId=\"1\"/></cellStyleXfs>\
             <cellXfs count=\"4\"><xf numFmtId=\"0\" fontId=\"0\"/><xf numFmtId=\"164\" fontId=\"1\" applyNumberFormat=\"1\"/><xf numFmtId=\"10\" fontId=\"2\"><alignment/></xf><xf numFmtId=\"22\" fontId=\"1\"/></cellXfs></styleSheet>"
        );
        let st = parse_styles(&xml).unwrap();
        assert_eq!(st.xfs.len(), 4, "cellStyleXfs must not be counted");
        assert_eq!(
            st.xfs[0],
            CellStyle {
                format: Arc::new(NumFmt::General),
                bold: false,
                size: 11.0
            }
        );
        assert_eq!(
            st.xfs[1],
            CellStyle {
                format: Arc::new(NumFmt::Date),
                bold: true,
                size: 14.0
            }
        );
        assert_eq!(*st.xfs[2].format, NumFmt::Percent(2));
        // A quoted currency arrives entity-escaped in the attribute.
        let xml = format!(
            "<styleSheet {NS}><numFmts count=\"1\"><numFmt numFmtId=\"164\" formatCode=\"&quot;$&quot;#,##0.00\"/></numFmts>\
             <cellXfs count=\"1\"><xf numFmtId=\"164\" fontId=\"0\"/></cellXfs></styleSheet>"
        );
        let quoted = parse_styles(&xml).unwrap();
        assert_eq!(format_number("14.5", &quoted.xfs[0], false), "$14.50");
        assert_eq!(*st.xfs[3].format, NumFmt::DateTime);
        assert!(st.xfs[3].bold);
    }

    #[test]
    fn a_sheet_opens_with_its_name_and_its_index_is_the_page() {
        let els = parse_sheet(&sheet(&[text("A1", 0, "hello")], ""));
        assert_eq!(kinds(&els), vec!["doco:SectionTitle:1", "doco:Paragraph"]);
        assert_eq!(els[0].text, "Sheet1");
        assert!(els.iter().all(|e| e.page == 3));
        assert!(els.iter().all(|e| e.bbox.is_none()));
    }

    #[test]
    fn a_block_with_a_bold_first_row_is_a_table_with_a_header() {
        let xml = sheet(
            &[
                text("A1", 1, "Part"),
                text("B1", 1, "Description"),
                text("C1", 1, "Weight"),
                text("A2", 0, "BR-100"),
                text("B2", 0, "Bracket"),
                num("C2", 0, "6.7400000000000002"),
                text("A3", 0, "BR-101"),
                text("B3", 0, "Bracket, wide"),
                num("C3", 0, "9"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(kinds(&els), vec!["doco:SectionTitle:1", "doco:Table"]);
        let t = &els[1];
        assert_eq!(t.header_rows, Some(1));
        let cells = t.cells.as_ref().unwrap();
        assert_eq!(cells[0], vec!["Part", "Description", "Weight"]);
        assert_eq!(cells[1], vec!["BR-100", "Bracket", "6.74"]);
        assert_eq!(cells[2][2], "9");
        assert!(t.merged_left.is_none() && t.merged_down.is_none());
    }

    #[test]
    fn a_cell_stores_its_value_typed_beneath_what_it_shows() {
        let st = |format| CellStyle {
            format: Arc::new(format),
            ..Default::default()
        };
        let datum = |kind: &str, v: &str, format| cell_datum(kind, v, &st(format), false);
        let d = |ty, v: &str| Some(Datum::new(ty, v));
        // A number keeps every digit its format rounds away.
        assert_eq!(
            datum("", "0.12345", NumFmt::Percent(0)),
            d("xsd:decimal", "0.12345")
        );
        assert_eq!(
            format_number("0.12345", &st(NumFmt::Percent(0)), false),
            "12%"
        );
        assert_eq!(
            datum("n", "6.7400000000000002", NumFmt::General),
            d("xsd:decimal", "6.74")
        );
        assert_eq!(datum("", "17", NumFmt::General), d("xsd:decimal", "17"));
        // A date keeps the time of day its format hides.
        assert_eq!(
            datum("", "45123", NumFmt::Date),
            d("xsd:date", "2023-07-16")
        );
        assert_eq!(
            datum("", "45123.75", NumFmt::Date),
            d("xsd:dateTime", "2023-07-16T18:00:00")
        );
        assert_eq!(
            datum("", "45123.5000115741", NumFmt::DateTime),
            d("xsd:dateTime", "2023-07-16T12:00:01")
        );
        assert_eq!(
            datum("", "0.5000005787", NumFmt::Time),
            d("xsd:time", "12:00:00.050")
        );
        // A time format over a day or more: timestamp or duration, the file
        // does not say, so the number stands.
        assert_eq!(datum("", "1.5", NumFmt::Time), d("xsd:decimal", "1.5"));
        assert_eq!(datum("b", "1", NumFmt::General), d("xsd:boolean", "true"));
        assert_eq!(
            datum("d", "2023-07-16T18:00:00", NumFmt::General),
            d("xsd:dateTime", "2023-07-16T18:00:00")
        );
        // An ISO cell is typed by what it is and written as XML Schema
        // writes it, and one that is not a real date has none.
        assert_eq!(
            datum("d", "2026-07-17T10:00", NumFmt::General),
            d("xsd:dateTime", "2026-07-17T10:00:00")
        );
        assert_eq!(
            datum("d", "2026-07-17+02:00", NumFmt::General),
            d("xsd:date", "2026-07-17+02:00")
        );
        assert_eq!(
            datum("d", "10:30:00", NumFmt::General),
            d("xsd:time", "10:30:00")
        );
        assert_eq!(datum("d", "2026-02-30", NumFmt::General), None);
        // Text is text, digits or not: a code stored as a string stays one.
        assert_eq!(datum("inlineStr", "00123", NumFmt::General), None);
        assert_eq!(datum("s", "4", NumFmt::General), None);
        assert_eq!(datum("e", "#DIV/0!", NumFmt::General), None);
    }

    #[test]
    fn a_table_carries_its_cells_stored_values() {
        let xml = sheet(
            &[
                text("A1", 1, "Item"),
                text("B1", 1, "Share"),
                text("C1", 1, "Since"),
                text("A2", 0, "00123"),
                num("B2", 4, "0.12345"),
                num("C2", 5, "45123"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        let t = &els[1];
        assert_eq!(
            t.cells.as_ref().unwrap()[1],
            ["00123", "12.3%", "2023-07-16"]
        );
        let data = t.datums.as_ref().unwrap();
        assert_eq!(data[0], [None, None, None], "header labels are text");
        assert_eq!(
            data[1],
            [
                None,
                Some(Datum::new("xsd:decimal", "0.12345")),
                Some(Datum::new("xsd:date", "2023-07-16")),
            ]
        );
    }

    #[test]
    fn a_label_row_over_numbers_is_a_header_without_bold() {
        let xml = sheet(
            &[
                text("A1", 0, "Year"),
                text("B1", 0, "Total"),
                num("A2", 0, "2023"),
                num("B2", 0, "17"),
            ],
            "",
        );
        assert_eq!(parse_sheet(&xml)[1].header_rows, Some(1));
    }

    #[test]
    fn a_label_value_block_gets_no_header() {
        let xml = sheet(
            &[
                text("A1", 1, "Owner"),
                text("B1", 0, "Facilities team"),
                text("A2", 1, "Budget"),
                text("B2", 0, "Q3 plan"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(kinds(&els), vec!["doco:SectionTitle:1", "doco:Table"]);
        assert_eq!(els[1].header_rows, Some(0));
    }

    #[test]
    fn a_field_list_gets_no_header() {
        let xml = sheet(
            &[
                text("A1", 1, "Description"),
                text("B1", 0, "Cinnamon bun"),
                text("A2", 1, "Code"),
                text("B2", 0, "BK-215"),
                text("A3", 1, "Quantity"),
                num("B3", 0, "1"),
                text("A4", 1, "Total paid"),
                num("B4", 0, "9"),
            ],
            "",
        );
        assert_eq!(parse_sheet(&xml)[1].header_rows, Some(0));
    }

    #[test]
    fn a_bold_label_column_is_a_field_list_whatever_its_values() {
        // Labels over amounts read, by type alone, as a header over data;
        // the bold column says they are fields.
        let xml = sheet(
            &[
                text("A1", 1, "Goods total"),
                num("B1", 0, "78.5"),
                text("A2", 1, "GST 15%"),
                num("B2", 0, "11.19"),
                text("A3", 1, "Amount due"),
                num("B3", 0, "85.76"),
            ],
            "",
        );
        assert_eq!(parse_sheet(&xml)[1].header_rows, Some(0));
        // Two fields are a field list too.
        let xml = sheet(
            &[
                text("A1", 1, "Number"),
                text("B1", 0, "R-13247"),
                text("A2", 1, "Date"),
                text("B2", 0, "31.08.2026"),
            ],
            "",
        );
        assert_eq!(parse_sheet(&xml)[1].header_rows, Some(0));
    }

    #[test]
    fn a_title_on_top_of_a_block_is_peeled_as_a_heading() {
        let xml = sheet(
            &[
                text("A1", 2, "Sales by region"),
                text("A2", 1, "Region"),
                text("B2", 1, "Units"),
                text("A3", 0, "EMEA"),
                num("B3", 0, "28"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(
            kinds(&els),
            vec!["doco:SectionTitle:1", "doco:SectionTitle:2", "doco:Table"]
        );
        assert_eq!(els[1].text, "Sales by region");
        assert_eq!(els[2].cells.as_ref().unwrap().len(), 2);
        assert_eq!(els[2].header_rows, Some(1));
    }

    #[test]
    fn only_the_first_peeled_row_is_the_title() {
        // A bold title, a bold code under it, a long summary: one heading
        // and two paragraphs, then the fields as a table.
        let xml = sheet(
            &[
                text("A1", 2, "Project brief — Bridge repaint"),
                text("A2", 1, "P-01"),
                text("B3", 1, "Repaint the north span before the autumn closure"),
                text("A4", 1, "Owner"),
                text("B4", 0, "Facilities team"),
                text("C4", 0, "due in May"),
                text("A5", 1, "Budget"),
                text("B5", 0, "Q3 plan"),
            ],
            "<mergeCells count=\"2\"><mergeCell ref=\"A1:B1\"/><mergeCell ref=\"A2:B2\"/></mergeCells>",
        );
        let els = parse_sheet(&xml);
        assert_eq!(
            kinds(&els),
            vec![
                "doco:SectionTitle:1",
                "doco:SectionTitle:2",
                "doco:Paragraph",
                "doco:Paragraph",
                "doco:Table"
            ]
        );
        assert_eq!(els[2].text, "P-01");
        let t = &els[4];
        let cells = t.cells.as_ref().unwrap();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0], vec!["Owner", "Facilities team", "due in May"]);
        assert_eq!(cells[1], vec!["Budget", "Q3 plan", ""]);
        assert_eq!(t.header_rows, Some(0));
    }

    #[test]
    fn a_lone_cell_is_a_heading_when_set_as_display_type() {
        // A title in A1, a blank row, the table from A3: two islands.
        let xml = sheet(
            &[
                text("A1", 2, "Quarterly figures"),
                text("A3", 1, "Q"),
                text("B3", 1, "Revenue"),
                text("A4", 0, "Q1"),
                num("B4", 0, "10"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(
            kinds(&els),
            vec!["doco:SectionTitle:1", "doco:SectionTitle:2", "doco:Table"]
        );
        // The same cell in body type is a paragraph.
        let xml = sheet(&[text("A1", 0, "Just a remark"), text("A3", 0, "x")], "");
        assert_eq!(
            kinds(&parse_sheet(&xml)),
            vec!["doco:SectionTitle:1", "doco:Paragraph", "doco:Paragraph"]
        );
    }

    #[test]
    fn of_two_sizes_setting_as_much_text_the_smaller_is_the_body() {
        // As much text at 11pt as at 9pt: the 9pt is the body, and the 11pt
        // cell standing alone above it is display type.
        let xml = sheet(
            &[
                text("A1", 0, "Site summary"),
                text("A3", 3, "Site"),
                text("B3", 3, "Visits"),
                text("A4", 3, "Rd"),
            ],
            "",
        );
        for _ in 0..8 {
            assert_eq!(
                kinds(&parse_sheet(&xml)),
                vec!["doco:SectionTitle:1", "doco:SectionTitle:2", "doco:Table"]
            );
        }
    }

    #[test]
    fn lines_stacked_in_one_column_are_text_not_a_table() {
        // A receipt's head: title, shop, address, date, all in column A.
        let xml = sheet(
            &[
                text("A1", 2, "Receipt WB-37727"),
                text("A2", 1, "Wrenmoor Bakehouse"),
                text("A3", 0, "14 Quayside Row, Bristol"),
                text("A4", 0, "Date: 9 June 2026"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(
            kinds(&els),
            vec![
                "doco:SectionTitle:1",
                "doco:SectionTitle:2",
                "doco:Paragraph",
                "doco:Paragraph",
                "doco:Paragraph"
            ]
        );
        assert_eq!(els[1].text, "Receipt WB-37727");
        assert_eq!(els[4].text, "Date: 9 June 2026");
    }

    #[test]
    fn an_empty_column_separates_two_tables() {
        let xml = sheet(
            &[
                text("A1", 1, "L"),
                text("B1", 1, "M"),
                num("A2", 0, "1"),
                num("B2", 0, "2"),
                text("D1", 1, "R"),
                text("E1", 1, "S"),
                num("D2", 0, "3"),
                num("E2", 0, "4"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(
            kinds(&els),
            vec!["doco:SectionTitle:1", "doco:Table", "doco:Table"]
        );
        assert_eq!(els[1].cells.as_ref().unwrap()[0], vec!["L", "M"]);
        assert_eq!(els[2].cells.as_ref().unwrap()[0], vec!["R", "S"]);
    }

    #[test]
    fn labels_and_their_amounts_across_empty_columns_are_one_table() {
        // Totals under a line table: labels in A, amounts in D.
        let xml = sheet(
            &[
                text("A1", 1, "Item"),
                text("B1", 1, "Qty"),
                text("C1", 1, "Unit"),
                text("D1", 1, "Line"),
                text("A2", 0, "Fish pie"),
                num("B2", 0, "2"),
                num("C2", 0, "14.5"),
                num("D2", 0, "29"),
                text("A4", 0, "Subtotal"),
                num("D4", 0, "38.6"),
                text("A5", 0, "Total"),
                num("D5", 0, "42.46"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(
            kinds(&els),
            vec!["doco:SectionTitle:1", "doco:Table", "doco:Table"]
        );
        let totals = &els[2];
        assert_eq!(
            totals.cells.as_ref().unwrap(),
            &vec![vec!["Subtotal", "38.6"], vec!["Total", "42.46"]],
            "the empty columns between are not part of the table"
        );
        assert_eq!(totals.header_rows, Some(0));
    }

    #[test]
    fn a_row_missing_its_amount_does_not_split_the_table() {
        let xml = sheet(
            &[
                text("A1", 1, "Item"),
                text("B1", 1, "Units"),
                text("C1", 1, "Amount"),
                text("A2", 0, "Utility knife"),
                num("B2", 0, "4"),
                num("C2", 0, "43.16"),
                text("A3", 0, "Subtotal"),
                num("C3", 0, "70.7"),
                text("A4", 0, "TOTAL"),
                text("A5", 0, "Cash"),
                num("C5", 0, "80"),
                text("A6", 0, "Change"),
                num("C6", 0, "3.07"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(kinds(&els), vec!["doco:SectionTitle:1", "doco:Table"]);
        let cells = els[1].cells.as_ref().unwrap();
        assert_eq!(cells[3], vec!["TOTAL", "", ""]);
        assert_eq!(cells[5], vec!["Change", "", "3.07"]);
    }

    #[test]
    fn a_bold_header_over_row_labels_has_an_empty_corner() {
        let xml = sheet(
            &[
                text("B1", 1, "Paracetamol"),
                text("C1", 1, "Ibuprofen"),
                text("A2", 0, "Code"),
                text("B2", 0, "MD-0152"),
                text("C2", 0, "MD-0140"),
                text("A3", 0, "Each"),
                num("B3", 0, "2.49"),
                num("C3", 0, "3.5"),
            ],
            "",
        );
        let t = &parse_sheet(&xml)[1];
        assert_eq!(
            t.cells.as_ref().unwrap()[0],
            vec!["", "Paracetamol", "Ibuprofen"]
        );
        assert_eq!(t.header_rows, Some(1));
    }

    #[test]
    fn side_by_side_blocks_that_are_not_labels_and_amounts_stay_apart() {
        // Two address blocks: text beside text.
        let xml = sheet(
            &[
                text("A1", 0, "Bill to"),
                text("A2", 0, "Acme Ltd"),
                text("D1", 0, "Ship to"),
                text("D2", 0, "Beta plc"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(els.len(), 5, "four lines under the sheet title");
        assert!(els.iter().all(|e| e.kind != "doco:Table"));
        // A label column beside a table with its own header.
        let xml = sheet(
            &[
                text("A1", 0, "Notes"),
                text("A2", 0, "Paid"),
                text("C1", 1, "Qty"),
                text("D1", 1, "Price"),
                num("C2", 0, "2"),
                num("D2", 0, "14.5"),
            ],
            "",
        );
        let els = parse_sheet(&xml);
        assert_eq!(els[1].text, "Notes");
        let firsts: Vec<_> = els
            .iter()
            .filter_map(|e| e.cells.as_ref())
            .map(|c| c[0].clone())
            .collect();
        assert_eq!(firsts, vec![vec!["Qty", "Price"]]);
        // Rows that do not line up.
        let xml = sheet(
            &[
                text("A1", 0, "Subtotal"),
                text("A2", 0, "Total"),
                num("D2", 0, "42.46"),
                num("D3", 0, "1"),
            ],
            "",
        );
        assert!(parse_sheet(&xml)
            .iter()
            .all(|e| e.cells.as_ref().is_none_or(|c| c[0].len() == 1)));
    }

    #[test]
    fn merged_ranges_show_their_anchor_and_hide_the_rest() {
        let xml = sheet(
            &[
                text("A1", 1, "Item"),
                text("B1", 1, "Q1"),
                text("C1", 1, "Q2"),
                text("A2", 0, "Group A"),
                num("B2", 0, "1"),
                num("C2", 0, "2"),
                text("A3", 0, "stale"),
                num("B3", 0, "3"),
                num("C3", 0, "4"),
                text("A4", 0, "Both quarters"),
                num("B4", 0, "9"),
                text("C4", 0, "hidden"),
            ],
            "<mergeCells count=\"2\"><mergeCell ref=\"A2:A3\"/><mergeCell ref=\"B4:C4\"/></mergeCells>",
        );
        let els = parse_sheet(&xml);
        let t = &els[1];
        let cells = t.cells.as_ref().unwrap();
        assert_eq!(
            cells[2],
            vec!["", "3", "4"],
            "the hidden cell's stale text is not shown"
        );
        assert_eq!(cells[3], vec!["Both quarters", "9", ""]);
        let w = 3;
        let md = t.merged_down.as_ref().unwrap();
        assert!(md[2 * w], "A3 continues A2");
        let ml = t.merged_left.as_ref().unwrap();
        assert!(ml[3 * w + 2], "C4 continues B4");
        assert!(!ml[3 * w + 1]);
    }

    #[test]
    fn a_note_under_a_table_is_a_paragraph() {
        let xml = sheet(
            &[
                text("A1", 1, "Part"),
                text("B1", 1, "Price"),
                text("A2", 0, "BR-100"),
                num("B2", 0, "12.5"),
                text("A3", 3, "Note: prices are provisional."),
            ],
            "<mergeCells count=\"1\"><mergeCell ref=\"A3:B3\"/></mergeCells>",
        );
        let els = parse_sheet(&xml);
        assert_eq!(
            kinds(&els),
            vec!["doco:SectionTitle:1", "doco:Table", "doco:Paragraph"]
        );
        assert_eq!(els[1].cells.as_ref().unwrap().len(), 2);
        assert!(els[2].text.starts_with("Note:"));
    }

    #[test]
    fn frozen_rows_are_the_header() {
        let xml = sheet(
            &[
                text("A1", 0, "Region"),
                text("B1", 0, "Metric"),
                text("A2", 0, "Region"),
                text("B2", 0, "Units"),
                text("A3", 0, "EMEA"),
                text("B3", 0, "many"),
            ],
            "<sheetViews><sheetView workbookViewId=\"0\"><pane ySplit=\"2\" topLeftCell=\"A3\" activePane=\"bottomLeft\" state=\"frozen\"/></sheetView></sheetViews>",
        );
        assert_eq!(parse_sheet(&xml)[1].header_rows, Some(2));
    }

    #[test]
    fn a_header_merged_down_into_a_second_row_is_a_stacked_header() {
        let xml = sheet(
            &[
                text("A1", 1, "Item"),
                text("B1", 1, "Price"),
                text("B2", 1, "Unit"),
                text("C2", 1, "Line"),
                text("A3", 0, "Fish pie"),
                num("B3", 0, "14.5"),
                num("C3", 0, "29"),
            ],
            "<mergeCells count=\"2\"><mergeCell ref=\"A1:A2\"/><mergeCell ref=\"B1:C1\"/></mergeCells>",
        );
        let t = &parse_sheet(&xml)[1];
        assert_eq!(t.header_rows, Some(2));
        assert_eq!(t.cells.as_ref().unwrap()[0], vec!["Item", "Price", ""]);
    }

    #[test]
    fn an_autofilter_range_names_its_header_row() {
        let xml = sheet(
            &[
                text("A1", 0, "Name"),
                text("B1", 0, "Team"),
                text("A2", 0, "Ada"),
                text("B2", 0, "Blue"),
            ],
            "<autoFilter ref=\"A1:B2\"/>",
        );
        assert_eq!(parse_sheet(&xml)[1].header_rows, Some(1));
    }

    #[test]
    fn a_full_width_merged_row_inside_a_table_is_a_sub_header() {
        let xml = sheet(
            &[
                text("A1", 1, "Part"),
                text("B1", 1, "Price"),
                text("A2", 1, "Fasteners"),
                text("A3", 0, "BR-100"),
                num("B3", 0, "12.5"),
            ],
            "<mergeCells count=\"1\"><mergeCell ref=\"A2:B2\"/></mergeCells>",
        );
        let t = &parse_sheet(&xml)[1];
        assert_eq!(t.header_rows, Some(1));
        assert_eq!(t.sub_headers, Some(vec![1]));
    }

    #[test]
    fn values_read_through_their_types() {
        let styles = styles();
        let ctx = Context {
            shared: &["from the table".to_string()],
            styles: &styles,
            date1904: false,
        };
        let xml = sheet(
            &[
                c("A1", "s", 0, "<v>0</v>"),
                text("B1", 0, "two\nlines"),
                c("C1", "b", 0, "<v>1</v>"),
                c("D1", "e", 0, "<v>#N/A</v>"),
                c("E1", "str", 0, "<f>A1&amp;B1</f><v>joined</v>"),
                c("F1", "", 0, "<f>SUM(A2:A9)</f><v>42</v>"),
                c("G1", "", 5, "<v>45123</v>"),
                c("H1", "", 6, "<v>45123.75</v>"),
                c("I1", "d", 0, "<v>2023-07-16</v>"),
                c("J1", "", 4, "<v>0.125</v>"),
            ],
            "",
        );
        let els = parse_sheet_xml("S", &xml, 0, &ctx).unwrap();
        let row = &els[1].cells.as_ref().unwrap()[0];
        assert_eq!(
            row,
            &[
                "from the table",
                "two lines",
                "TRUE",
                "#N/A",
                "joined",
                "42",
                "2023-07-16",
                "2023-07-16 18:00:00",
                "2023-07-16",
                "12.5%"
            ]
        );
    }

    #[test]
    fn a_formula_with_no_cached_value_holds_nothing() {
        // Saved without recalculation: the cell has a formula and no `<v>`.
        // Nothing is computed here, so the cell is empty and the sheet has
        // only its title.
        let xml = sheet(&[c("A1", "", 0, "<f>NOW()</f>")], "");
        assert_eq!(kinds(&parse_sheet(&xml)), vec!["doco:SectionTitle:1"]);
    }

    #[test]
    fn cells_without_references_follow_in_sequence() {
        // No `r` attributes anywhere: rows and cells count from where the
        // previous one left off, and a self-closing `<c/>` takes a column.
        let xml = format!(
            "<worksheet {NS}><sheetData><row><c t=\"inlineStr\"><is><t>a</t></is></c><c t=\"inlineStr\"><is><t>b</t></is></c><c t=\"inlineStr\"><is><t>x</t></is></c></row><row><c t=\"inlineStr\"><is><t>c</t></is></c><c/><c><v>3</v></c></row></sheetData></worksheet>"
        );
        let cells = parse_sheet(&xml)[1].cells.clone().unwrap();
        assert_eq!(cells, vec![vec!["a", "b", "x"], vec!["c", "", "3"]]);
    }

    fn package(sheets: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write;
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let mut put = |name: &str, body: &str| {
            zw.start_file(name, opts).unwrap();
            zw.write_all(body.as_bytes()).unwrap();
        };
        let list: String = sheets
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                format!(
                    "<sheet name=\"{name}\" sheetId=\"{}\" r:id=\"rId{}\"/>",
                    i + 1,
                    i + 1
                )
            })
            .collect();
        put(
            "xl/workbook.xml",
            &format!("<workbook {NS}><sheets>{list}</sheets></workbook>"),
        );
        // Relationship ids deliberately do not follow file numbering.
        let rels: String = sheets
            .iter()
            .enumerate()
            .map(|(i, _)| {
                format!(
                    "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{}.xml\"/>",
                    i + 1,
                    sheets.len() - i
                )
            })
            .collect();
        put(
            "xl/_rels/workbook.xml.rels",
            &format!("<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{rels}</Relationships>"),
        );
        for (i, (_, xml)) in sheets.iter().enumerate() {
            put(&format!("xl/worksheets/sheet{}.xml", sheets.len() - i), xml);
        }
        zw.finish().unwrap().into_inner()
    }

    #[test]
    fn a_sheet_is_named_as_the_workbook_names_it() {
        // An attribute escapes its `&`; the sheet is still `R&D`.
        let bytes = package(&[("R&amp;D", &sheet(&[text("A1", 0, "Budget")], ""))]);
        assert_eq!(parse(&bytes).unwrap()[0].text, "R&D");
    }

    #[test]
    fn sheets_come_out_in_workbook_order_as_pages() {
        let first = sheet(&[text("A1", 0, "one")], "");
        let second = sheet(&[text("A1", 0, "two")], "");
        let bytes = package(&[("Index", &first), ("Detail", &second)]);
        let els = parse(&bytes).unwrap();
        let titles: Vec<(&str, usize)> = els
            .iter()
            .filter(|e| e.kind == "doco:SectionTitle")
            .map(|e| (e.text.as_str(), e.page))
            .collect();
        assert_eq!(titles, vec![("Index", 0), ("Detail", 1)]);
        assert_eq!(els[1].text, "one");
        assert_eq!(els[3].text, "two");
        assert!(els
            .iter()
            .enumerate()
            .all(|(i, e)| e.id == format!("elem-{:05}", i + 1)));
    }

    #[test]
    fn a_package_without_a_workbook_is_refused() {
        use std::io::Write;
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zw.start_file("word/document.xml", opts).unwrap();
        zw.write_all(b"<w:document/>").unwrap();
        let bytes = zw.finish().unwrap().into_inner();
        assert!(matches!(parse(&bytes), Err(XlsxError::NoWorkbook)));
        assert!(matches!(parse(b"not a zip"), Err(XlsxError::Zip(_))));
    }

    /// A workbook listing `listed` sheets, each by the relationship id
    /// `rId1`, and relating `rels` ids no sheet names before the sheet's
    /// part, `rId1`, once external and then within the package.
    fn listing(listed: usize, rels: usize) -> Vec<u8> {
        use std::io::Write;
        let list: String = (0..listed)
            .map(|i| format!("<sheet name=\"S{i}\" r:id=\"rId1\"/>"))
            .collect();
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet";
        let others = "<Relationship Id=\"p\" Target=\"a\"/>".repeat(rels);
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            (
                "xl/workbook.xml",
                format!("<workbook {NS}><sheets>{list}</sheets></workbook>"),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                format!(
                    "<Relationships>{others}\
                     <Relationship Id=\"rId1\" Type=\"{rel}\" Target=\"https://example.com/x.xml\" TargetMode=\"External\"/>\
                     <Relationship Id=\"rId1\" Type=\"{rel}\" Target=\"worksheets/sheet1.xml\"/></Relationships>"
                ),
            ),
            (
                "xl/worksheets/sheet1.xml",
                sheet(&[text("A1", 0, "cell")], ""),
            ),
        ] {
            zw.start_file(name, opts).unwrap();
            zw.write_all(body.as_bytes()).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    #[test]
    fn a_sheet_listed_again_is_read_once() {
        // Its first internal relationship names its part; listed again,
        // under any name, it is not read again.
        let els = parse(&listing(3, 0)).unwrap();
        let texts: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, vec!["S0", "cell"]);
    }

    #[test]
    fn a_sheet_is_found_among_the_relationships_in_one_lookup() {
        // 20,000 listings and 200,000 relationships, 21 KB zipped: each
        // listing scanning them all took 15 s in a release build.
        assert_eq!(parse(&listing(2_000, 20_000)).unwrap().len(), 2);
        fluree_doc_model::scaling::assert_linear(
            "sheets listed among relationships",
            1_000,
            |n| listing(n, 10 * n),
            |b| assert_eq!(parse(b).unwrap().len(), 2),
        );
    }

    const XDR: &str = r#"xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    /// A picture anchored at (row, col), 0-based, of the image `rid`.
    fn anchored(row: u32, col: u32, descr: &str, rid: &str) -> String {
        format!(
            "<xdr:twoCellAnchor><xdr:from><xdr:col>{col}</xdr:col><xdr:colOff>0</xdr:colOff>\
             <xdr:row>{row}</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>\
             <xdr:to><xdr:col>9</xdr:col><xdr:row>40</xdr:row></xdr:to>\
             <xdr:pic><xdr:nvPicPr><xdr:cNvPr id=\"2\" name=\"Picture 1\" descr=\"{descr}\"/></xdr:nvPicPr>\
             <xdr:blipFill><a:blip r:embed=\"{rid}\"/></xdr:blipFill></xdr:pic><xdr:clientData/></xdr:twoCellAnchor>"
        )
    }

    /// A workbook of `sheets` (name, sheet XML, drawing XML), each sheet's
    /// drawing relating its images as `rId<k>` to `media/image<k>.png`, and
    /// the parts `extra`.
    fn workbook(sheets: &[(&str, String, Option<String>)], extra: &[(&str, Vec<u8>)]) -> Vec<u8> {
        use std::io::Write;
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
        let list: String = (1..=sheets.len())
            .map(|i| {
                format!(
                    "<sheet name=\"{}\" sheetId=\"{i}\" r:id=\"rId{i}\"/>",
                    sheets[i - 1].0
                )
            })
            .collect();
        parts.push((
            "xl/workbook.xml".into(),
            format!("<workbook {NS}><sheets>{list}</sheets></workbook>").into_bytes(),
        ));
        let rels: String = (1..=sheets.len())
            .map(|i| {
                format!("<Relationship Id=\"rId{i}\" Type=\"{rel}/worksheet\" Target=\"worksheets/sheet{i}.xml\"/>")
            })
            .collect();
        parts.push((
            "xl/_rels/workbook.xml.rels".into(),
            format!("<Relationships>{rels}</Relationships>").into_bytes(),
        ));
        for (i, (_, xml, drawing)) in sheets.iter().enumerate() {
            let i = i + 1;
            parts.push((
                format!("xl/worksheets/sheet{i}.xml"),
                xml.clone().into_bytes(),
            ));
            if let Some(d) = drawing {
                parts.push((
                    format!("xl/worksheets/_rels/sheet{i}.xml.rels"),
                    format!(
                        "<Relationships><Relationship Id=\"rIdD\" Type=\"{rel}/drawing\" \
                         Target=\"../drawings/drawing{i}.xml\"/></Relationships>"
                    )
                    .into_bytes(),
                ));
                parts.push((
                    format!("xl/drawings/drawing{i}.xml"),
                    format!("<xdr:wsDr {XDR}>{d}</xdr:wsDr>").into_bytes(),
                ));
                let images: String = (1..=4)
                    .map(|k| {
                        format!("<Relationship Id=\"rId{k}\" Type=\"{rel}/image\" Target=\"../media/image{k}.png\"/>")
                    })
                    .collect();
                parts.push((
                    format!("xl/drawings/_rels/drawing{i}.xml.rels"),
                    format!("<Relationships>{images}</Relationships>").into_bytes(),
                ));
            }
        }
        parts.extend(extra.iter().map(|(n, b)| (n.to_string(), b.clone())));
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in parts {
            zw.start_file(name, opts).unwrap();
            zw.write_all(&bytes).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    /// A sheet that names its drawing.
    fn drawn(cells: &[String]) -> String {
        sheet(cells, "").replace("</sheetData>", "</sheetData><drawing r:id=\"rIdD\"/>")
    }

    #[test]
    fn a_picture_is_a_figure_where_it_is_anchored() {
        // A table on rows 3-4, a screenshot anchored at row 6, a note on
        // row 9; a second sheet with a picture of its own.
        let first = drawn(&[
            text("A3", 1, "Item"),
            text("B3", 1, "Amount"),
            text("A4", 0, "Fee"),
            num("B4", 0, "12"),
            text("A9", 0, "See the screenshot above."),
        ]);
        let second = drawn(&[text("A1", 0, "Notes")]);
        let bytes = workbook(
            &[
                (
                    "Summary",
                    first,
                    Some(anchored(5, 0, "Account screen", "rId1")),
                ),
                ("Detail", second, Some(anchored(3, 2, "", "rId2"))),
            ],
            &[
                ("xl/media/image1.png", b"1".to_vec()),
                ("xl/media/image2.png", b"2".to_vec()),
            ],
        );
        let (els, _, media) = parse_with_media(&bytes).unwrap();
        let got: Vec<(&str, &str, usize)> = els
            .iter()
            .map(|e| {
                (
                    e.kind.as_str(),
                    e.figure.as_deref().unwrap_or(&e.text),
                    e.page,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("doco:SectionTitle", "Summary", 0),
                ("doco:Table", els[1].text.as_str(), 0),
                ("doco:Figure", "figure-0-1", 0),
                ("doco:Paragraph", "See the screenshot above.", 0),
                ("doco:SectionTitle", "Detail", 1),
                ("doco:Paragraph", "Notes", 1),
                ("doco:Figure", "figure-1-2", 1),
            ]
        );
        assert_eq!(els[2].text, "Account screen");
        let listed: Vec<(&str, Option<&str>, String)> = media
            .iter()
            .map(|m| (m.figure.as_str(), m.source.part(), m.tag()))
            .collect();
        assert_eq!(
            listed,
            [
                ("figure-0-1", Some("xl/media/image1.png"), "m1".to_string()),
                ("figure-1-2", Some("xl/media/image2.png"), "m2".to_string()),
            ]
        );
        let mut pkg = Package::open(&bytes).unwrap();
        assert_eq!(read_media(&mut pkg, "xl/media/image2.png").unwrap(), b"2");
        // The plain reader reads the same elements, figures and all.
        let plain: Vec<String> = parse(&bytes)
            .unwrap()
            .iter()
            .map(|e| format!("{e:?}"))
            .collect();
        let full: Vec<String> = els.iter().map(|e| format!("{e:?}")).collect();
        assert_eq!(plain, full);
    }

    #[test]
    fn a_pictures_images_are_each_kept_once_in_one_lookup_and_spent() {
        // One picture naming 200,000 images, each its own id, and the first
        // named again: a scan of those kept for each took 15 s in a release
        // build, and none of them was spent.
        let blips: String = (0..200_000)
            .map(|i| format!("<a:blip r:embed=\"r{i}\"/>"))
            .collect();
        let xml = format!(
            "<xdr:wsDr><xdr:twoCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from>\
             <xdr:pic><xdr:blipFill>{blips}<a:blip r:embed=\"r0\"/></xdr:blipFill></xdr:pic>\
             </xdr:twoCellAnchor></xdr:wsDr>"
        );
        let got = pictures(&xml, &mut Budget::default()).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].rids.len(), 200_000);
        assert_eq!(got[0].rids[199_999], "r199999");
        let drawing = |n: usize| {
            let blips: String = (0..n)
                .map(|i| format!("<a:blip r:embed=\"r{i}\"/>"))
                .collect();
            format!(
                "<xdr:wsDr><xdr:twoCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from>\
                 <xdr:pic><xdr:blipFill>{blips}<a:blip r:embed=\"r0\"/></xdr:blipFill></xdr:pic>\
                 </xdr:twoCellAnchor></xdr:wsDr>"
            )
        };
        fluree_doc_model::scaling::assert_linear("a picture of many images", 5_000, drawing, |x| {
            pictures(x, &mut Budget::default()).unwrap();
        });
        // Each spent before it is kept.
        assert!(pictures(&xml, &mut Budget::new(16 << 20, 0)).is_err());
    }

    #[test]
    fn only_a_drawings_pictures_are_read_once_each() {
        let pic = |rid: &str, attr: &str| {
            format!(
                "<xdr:pic><xdr:nvPicPr><xdr:cNvPr id=\"2\" name=\"P\" title=\"T{rid}\"/></xdr:nvPicPr>\
                 <xdr:blipFill><a:blip r:{attr}=\"{rid}\"/></xdr:blipFill></xdr:pic>"
            )
        };
        let from = |row: u32, col: u32| {
            format!("<xdr:from><xdr:col>{col}</xdr:col><xdr:row>{row}</xdr:row></xdr:from>")
        };
        let xml = format!(
            "<xdr:wsDr {XDR}>\
             <xdr:oneCellAnchor>{}<xdr:grpSp>{}{}</xdr:grpSp></xdr:oneCellAnchor>\
             <xdr:twoCellAnchor>{}<xdr:graphicFrame><a:graphic><a:graphicData><c:chart r:id=\"rId9\"/></a:graphicData></a:graphic></xdr:graphicFrame></xdr:twoCellAnchor>\
             <xdr:twoCellAnchor>{}<xdr:sp><xdr:spPr><a:blipFill><a:blip r:embed=\"rId8\"/></a:blipFill></xdr:spPr></xdr:sp></xdr:twoCellAnchor>\
             <mc:AlternateContent><mc:Choice Requires=\"a14\"><xdr:twoCellAnchor>{}{}</xdr:twoCellAnchor></mc:Choice>\
             <mc:Fallback><xdr:twoCellAnchor>{}{}</xdr:twoCellAnchor></mc:Fallback></mc:AlternateContent>\
             <xdr:absoluteAnchor><xdr:pos x=\"0\" y=\"0\"/>{}</xdr:absoluteAnchor>\
             </xdr:wsDr>",
            from(2, 1),
            pic("rId1", "embed"),
            pic("rId2", "embed"),
            from(4, 0),
            from(5, 0),
            from(7, 3),
            pic("rId3", "embed"),
            from(7, 3),
            pic("rId6", "embed"),
            pic("rId4", "link"),
        );
        let got: Vec<((u32, u32), String, Vec<String>)> = pictures(&xml, &mut Budget::default())
            .unwrap()
            .into_iter()
            .map(|p| (p.at, p.alt, p.rids))
            .collect();
        let rids = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            got,
            [
                ((2, 1), "TrId1".to_string(), rids(&["rId1", "rId2"])),
                ((7, 3), "TrId3".to_string(), rids(&["rId3"])),
                ((u32::MAX, u32::MAX), "TrId4".to_string(), rids(&["rId4"])),
            ]
        );
        assert!(
            pictures("<xdr:wsDr><xdr:twoCellAnchor>", &mut Budget::default())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_pictures_image_not_in_the_package_is_listed_as_such() {
        let bytes = workbook(
            &[(
                "S",
                drawn(&[text("A1", 0, "x")]),
                Some(anchored(0, 0, "", "rId3") + &anchored(1, 0, "", "rId7")),
            )],
            &[("xl/media/IMAGE3.PNG", b"3".to_vec())],
        );
        let (_, _, media) = parse_with_media(&bytes).unwrap();
        let listed: Vec<&MediaSource> = media.iter().map(|m| &m.source).collect();
        assert_eq!(
            listed,
            [
                &MediaSource::Part("xl/media/IMAGE3.PNG".into()),
                &MediaSource::Missing("rId7".into())
            ]
        );
    }

    #[cfg(feature = "ocr")]
    #[test]
    fn a_pictures_text_is_read_after_its_figure() {
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
                    rules: Vec::new(),
                })
            }
        }
        let mut png = Vec::new();
        // Lines of strokes a letter wide on a light ground: text, to the
        // pixel probe, which a picture is read only when it is.
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(400, 120, |x, y| {
            let ink = (12..24).contains(&(y % 40)) && x % 10 < 6 && x < 300;
            image::Rgb(if ink { [40, 40, 40] } else { [240, 240, 240] })
        }))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
        let bytes = workbook(
            &[(
                "S",
                drawn(&[text("A1", 0, "Before"), text("A9", 0, "After")]),
                Some(anchored(3, 0, "", "rId1") + &anchored(4, 0, "", "rId4")),
            )],
            &[("xl/media/image1.png", png)],
        );
        let read = parse_with_ocr(&bytes, &OneLine).unwrap();
        let texts: Vec<(&str, &str)> = read
            .elements
            .iter()
            .map(|e| (e.kind.as_str(), e.text.as_str()))
            .collect();
        assert_eq!(
            texts,
            [
                ("doco:SectionTitle", "S"),
                ("doco:Paragraph", "Before"),
                ("doco:Figure", ""),
                ("doco:Paragraph", "Reference 4417-0093-21"),
                ("doco:Figure", ""),
                ("doco:Paragraph", "After"),
            ]
        );
        assert_eq!(read.elements[3].provenance, "ocr");
        // Its reading, by the picture's name, as the CLI lists it; the
        // picture not read has none.
        assert_eq!(
            read.ocr,
            [fluree_doc_model::OcrReading {
                page: None,
                image: Some("m1".into()),
                crop: "full".into(),
                engine: None,
                words: 1,
                mean_confidence: Some(0.9),
                min_confidence: Some(0.9),
                set_aside: None,
            }]
        );
        // The second picture's part is not in the package: named, it fails
        // to read.
        assert_eq!(read.unread.len(), 1);
        assert_eq!(
            (
                read.unread[0].image.as_str(),
                read.unread[0].reason.as_str()
            ),
            ("m2", "Failed")
        );
    }

    #[test]
    fn a_long_value_merged_down_is_refused_rather_than_copied_into_each_row() {
        // A2 merged down to the last row, beside columns of numbers, the
        // cells it covers written (their text hidden): DoCO writes A2's
        // value in each row of the merge, and beside it as each row's
        // label.
        const LONG: usize = 20_000;
        let workbook = |rows: u32| {
            let mut cells = vec![
                text("A1", 0, "Name"),
                text("B1", 0, "Q1"),
                text("C1", 0, "Q2"),
            ];
            cells.push(text("A2", 0, &"y".repeat(LONG)));
            cells.extend((3..=rows).map(|r| text(&format!("A{r}"), 0, "x")));
            cells.extend((2..=rows).map(|r| num(&format!("B{r}"), 0, "1")));
            cells.extend((2..=rows).map(|r| num(&format!("C{r}"), 0, "2")));
            let merge = format!("<mergeCells><mergeCell ref=\"A2:A{rows}\"/></mergeCells>");
            package(&[("S", &sheet(&cells, &merge))])
        };
        assert!(parse(&workbook(4)).is_ok());
        let err = parse(&workbook(5_000)).unwrap_err();
        assert!(err.to_string().contains("copies"), "{err}");
    }
}
