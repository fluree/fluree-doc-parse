//! Shared plumbing: input discovery, tier configuration, and the two
//! post-analysis passes (layout title promotion, model-tier splicing) used by
//! `convert` and the dev renderers alike.

use std::path::{Path, PathBuf};

/// Which reader a file goes to, by its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceKind {
    Pdf,
    Markdown,
    Text,
    Html,
    Docx,
    Pptx,
    Xlsx,
    /// A delimited file: CSV, or TSV by its extension.
    Csv,
    /// An Excel 97-2003 workbook (BIFF8).
    Xls,
    /// An XPS or OpenXPS document.
    Xps,
    /// A Word 97-2003 document.
    Doc,
    /// An OpenDocument text, spreadsheet or presentation.
    Odf,
    /// An Excel binary workbook.
    Xlsb,
    /// A PowerPoint 97-2003 presentation.
    Ppt,
    /// A Rich Text Format document.
    Rtf,
    Transcript,
    Email,
    Image,
    /// A format recognised, by its extension or its content, that no reader
    /// reads yet ([`unsupported`]): refused by name rather than handed to the
    /// PDF reader, whose `parse: Invalid` would read as a corrupt PDF.
    Unsupported,
}

/// Every extension `convert` reads, by the reader it goes to. Conversion
/// dispatches on this table and directory discovery lists what it names, so
/// a file one accepts the other finds. Word, PowerPoint and Excel's
/// macro-enabled, template and show variants are the same package under
/// another name.
const SOURCE_EXTENSIONS: &[(SourceKind, &[&str])] = &[
    (SourceKind::Pdf, &["pdf"]),
    (SourceKind::Markdown, &["md", "markdown"]),
    (SourceKind::Text, &["txt", "text"]),
    (SourceKind::Html, &["html", "htm", "xhtml"]),
    (SourceKind::Docx, &["docx", "docm", "dotx", "dotm"]),
    (
        SourceKind::Pptx,
        &["pptx", "pptm", "potx", "potm", "ppsx", "ppsm"],
    ),
    (SourceKind::Xlsx, &["xlsx", "xlsm", "xltx", "xltm"]),
    (SourceKind::Csv, &["csv", "tsv"]),
    (SourceKind::Xls, &["xls", "xlt"]),
    (SourceKind::Xps, &["xps", "oxps"]),
    (SourceKind::Doc, &["doc", "dot"]),
    (SourceKind::Odf, &["odt", "ott", "ods", "ots", "odp", "otp"]),
    (SourceKind::Xlsb, &["xlsb"]),
    (SourceKind::Ppt, &["ppt", "pps", "pot"]),
    (SourceKind::Rtf, &["rtf"]),
    (SourceKind::Transcript, &["vtt", "srt"]),
    (SourceKind::Email, &["eml", "msg"]),
    (SourceKind::Image, fluree_doc_pdf::image::EXTENSIONS),
];

/// Document formats with no reader yet, by extension, with what each is.
///
/// Listed so that a file of one is refused by name, and counted as such in
/// a batch, rather than read as a PDF and failing as a corrupt one. A format
/// leaves this list when its reader lands.
const UNSUPPORTED: &[(&str, &str)] = &[
    ("wpd", "WordPerfect document"),
    ("pages", "Apple Pages document"),
    ("numbers", "Apple Numbers spreadsheet"),
    ("key", "Apple Keynote presentation"),
];

/// The reader a file's extension names, if it names one; [`SourceKind::Unsupported`]
/// for a format no reader reads yet.
pub(crate) fn source_kind(path: &Path) -> Option<SourceKind> {
    let ext = path.extension()?.to_str()?;
    if UNSUPPORTED.iter().any(|(e, _)| ext.eq_ignore_ascii_case(e)) {
        return Some(SourceKind::Unsupported);
    }
    SOURCE_EXTENSIONS
        .iter()
        .find(|(_, exts)| exts.iter().any(|e| ext.eq_ignore_ascii_case(e)))
        .map(|(kind, _)| *kind)
}

/// An OLE compound file's signature: Word, Excel and PowerPoint 97-2003
/// files, and Outlook's `.msg`, all start with it.
const COMPOUND_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Why `convert` refuses a file it has no reader for, naming the format: by
/// its extension where that names one ([`UNSUPPORTED`]), else by its content.
/// `None` for a file some reader takes.
///
/// Only called for an input [`input_kind`] found unsupported, or for one read
/// as a PDF: a compound file that is not an Outlook message (the e-mail
/// reader takes those first) is an Office 97-2003 file, never a PDF.
pub(crate) fn unsupported(path: Option<&Path>, data: &[u8]) -> Option<String> {
    let ext = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .and_then(|ext| {
            UNSUPPORTED
                .iter()
                .find(|(e, _)| ext.eq_ignore_ascii_case(e))
        });
    match ext {
        Some((e, what)) => Some(format!(
            "unsupported format (.{e}, {what}): no reader for it yet"
        )),
        None if is_spreadsheet_xml(data) => Some(
            "unsupported format (an Excel 2003 XML spreadsheet): no reader for it yet; save it \
             as .xlsx"
                .into(),
        ),
        None if data.starts_with(&COMPOUND_MAGIC) => Some(
            "unsupported format (an OLE compound file that is no Word, Excel or PowerPoint 97-2003 \
             document nor Outlook message): no reader for it yet"
                .into(),
        ),
        None => None,
    }
}

/// Whether `data` is a PDF whose header may come after a prefix: the header
/// within the first kilobyte, where a reader finds it, and what a PDF holds
/// that text quoting a header does not, either the comment line of binary
/// bytes a writer puts after the header (`%âãÏÓ`) or the end-of-file marker
/// within the last kilobyte. A header alone is no PDF: a list of file
/// signatures, one a line, holds one.
///
/// Where the file is named as a table (`table`), a list of signatures may
/// hold the marker too, so the marker counts only beside a NUL, which a
/// PDF's compressed streams hold and text does not: the CSV reader refuses
/// it.
fn is_pdf(data: &[u8], table: bool) -> bool {
    let Some(at) = data[..data.len().min(1024)]
        .windows(5)
        .position(|w| w == b"%PDF-")
    else {
        return false;
    };
    let binary_line = data[at..]
        .split(|&b| b == b'\n' || b == b'\r')
        .filter(|l| !l.is_empty())
        .nth(1)
        .is_some_and(|l| {
            let l = &l[..l.len().min(64)];
            l.starts_with(b"%") && l.iter().filter(|&&b| b >= 0x80).count() >= 4
        });
    let tail = &data[data.len().saturating_sub(1024)..];
    binary_line || (tail.windows(5).any(|w| w == b"%%EOF") && (!table || data.contains(&0)))
}

/// The reader a file's signature names: the bytes it opens with, which no
/// file of another format shares, so they win over any name. A PDF; an
/// image; an OLE compound file by the stream that makes it an Outlook
/// message, an Excel, Word or PowerPoint 97-2003 file; a zip by the part
/// that makes it an XPS document, an OpenDocument file, an Excel binary
/// workbook or an Office package; RTF by `{\rtf`. `None` for anything
/// else, a compound file or a zip none of them included.
fn by_signature(data: &[u8]) -> Option<SourceKind> {
    if data.starts_with(b"%PDF-") {
        return Some(SourceKind::Pdf);
    }
    if fluree_doc_pdf::image::Format::sniff(data).is_some() {
        return Some(SourceKind::Image);
    }
    if data.starts_with(&COMPOUND_MAGIC) {
        return if fluree_doc_email::Format::sniff(data).is_some() {
            Some(SourceKind::Email)
        } else if fluree_doc_xls::is_xls(data) {
            Some(SourceKind::Xls)
        } else if fluree_doc_doc::is_doc(data) {
            Some(SourceKind::Doc)
        } else if fluree_doc_ppt::is_ppt(data) {
            Some(SourceKind::Ppt)
        } else {
            None
        };
    }
    if data.starts_with(b"PK") {
        if fluree_doc_odf::Format::sniff(data).is_some() {
            return Some(SourceKind::Odf);
        }
        if fluree_doc_xls::is_xlsb(data) {
            return Some(SourceKind::Xlsb);
        }
        return by_content(data, false);
    }
    fluree_doc_rtf::is_rtf(data).then_some(SourceKind::Rtf)
}

/// The reader a file's content names, where its name does not: an XPS
/// document, or an Office package by the part its root relationship names
/// as the document (`word/`, `ppt/`, `xl/`); a web page; and, where
/// `delimited` (a file named as an Office 97-2003 one, which spreadsheet
/// exports are), comma-, semicolon- or tab-separated text.
fn by_content(data: &[u8], delimited: bool) -> Option<SourceKind> {
    // Excel 2003's XML workbook holds `<Table>`, which is no web page.
    if is_spreadsheet_xml(data) {
        return Some(SourceKind::Unsupported);
    }
    if data.starts_with(b"PK") {
        if fluree_doc_xps::is_xps(data) {
            return Some(SourceKind::Xps);
        }
        let main = fluree_doc_ooxml::Package::open(data)
            .ok()?
            .root("officeDocument")?;
        return match main.split('/').next()? {
            "word" => Some(SourceKind::Docx),
            "ppt" => Some(SourceKind::Pptx),
            "xl" => Some(SourceKind::Xlsx),
            _ => None,
        };
    }
    if is_html(data) {
        return Some(SourceKind::Html);
    }
    (delimited && is_delimited(data)).then_some(SourceKind::Csv)
}

/// Whether `data` is an Excel 2003 XML workbook (SpreadsheetML), by the
/// processing instruction Excel writes or the namespace of its root, within
/// its first kilobyte. No reader reads one yet.
fn is_spreadsheet_xml(data: &[u8]) -> bool {
    let head = data[..data.len().min(1024)].to_ascii_lowercase();
    let has = |s: &[u8]| head.windows(s.len()).any(|w| w == s);
    has(b"progid=\"excel.sheet\"")
        || (has(b"<workbook") && has(b"urn:schemas-microsoft-com:office:spreadsheet"))
}

/// Whether `data` opens as a web page: past a byte-order mark and
/// whitespace, a doctype or an `html` element, or markup holding one, or a
/// table, within its first kilobyte.
fn is_html(data: &[u8]) -> bool {
    let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
    let start = data
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(data.len());
    let head = data[start..data.len().min(start + 1024)].to_ascii_lowercase();
    let has = |s: &[u8]| head.windows(s.len()).any(|w| w == s);
    head.starts_with(b"<!doctype html")
        || head.starts_with(b"<html")
        || (head.starts_with(b"<") && (has(b"<html") || has(b"<table")))
}

/// Whether `data` reads as delimited text: no NUL in the text of its first
/// 64 KiB, decoded as the CSV reader decodes it (UTF-16 is half zero bytes,
/// and no NUL), and its first lines (two at least, twenty at most) each
/// separated into the same number of fields, more than one, by the
/// delimiter the CSV reader finds.
fn is_delimited(data: &[u8]) -> bool {
    let head = &data[..data.len().min(64 << 10)];
    let text = fluree_doc_csv::decode(head);
    if text.contains('\0') {
        return false;
    }
    let mut lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    // The last line of a file cut at 64 KiB may be cut too.
    if head.len() < data.len() {
        lines.pop();
    }
    lines.truncate(20);
    if lines.len() < 2 {
        return false;
    }
    let d = fluree_doc_csv::sniff(&lines.join("\n"));
    let fields = |l: &str| l.matches(d).count();
    let first = fields(lines[0]);
    first > 0 && lines.iter().all(|l| fields(l) == first)
}

/// The reader a file goes to, by its content first, its extension only
/// where the content cannot tell: what `convert` reads it as, and
/// `render-routed` crops it as.
///
/// A file's signature first ([`by_signature`]): a PDF named `.doc`, a
/// screenshot named `.csv` or a workbook named `.csv` is what it holds, and
/// its name is a mistake. Then text formats by their content: a transcript
/// and an email, since `.vtt` has no registered type on most systems and a
/// saved message is `.eml`, `.mht`, `.txt` or nothing at all, so either
/// arrives renamed as often as not; a PDF by a header further in, within
/// the first kilobyte, with a line of binary bytes after it or an
/// end-of-file marker (beside a NUL, for a file named as a table), unless
/// the file is named as text or reads as a web page or a table; and, for a file named as an Office 97-2003 one or named
/// as nothing, a web page or (named as an Office 97-2003 one, which
/// spreadsheet exports are) delimited text, an Excel 2003 XML workbook
/// being neither. Then the
/// formats an extension names, each to its own reader, which says what is
/// wrong with a file that is not what its name says: never to the PDF
/// reader, whose `parse: Invalid` would read as a corrupt PDF. A format no
/// reader reads yet, by its extension or as a compound file that is not a
/// message, is [`SourceKind::Unsupported`]; anything else is read as a PDF,
/// whose header may come anywhere in its first kilobyte.
pub(crate) fn input_kind(path: &Path, data: &[u8]) -> SourceKind {
    if let Some(k) = by_signature(data) {
        return k;
    }
    let kind = source_kind(path);
    if fluree_doc_transcript::Format::sniff(data).is_some() || kind == Some(SourceKind::Transcript)
    {
        return SourceKind::Transcript;
    }
    if fluree_doc_email::Format::sniff(data).is_some() || kind == Some(SourceKind::Email) {
        return SourceKind::Email;
    }
    // A PDF whose header comes a few bytes in (a mail gateway's or a
    // download's prefix) has no signature, but the PDF reader finds the
    // header within the first kilobyte, so it is one whatever its name, when
    // it holds what a PDF does past its header ([`is_pdf`]): except a text
    // format's, and text that reads as a table or a page, which may well
    // quote `%PDF-`; a file named as a table needs more than the end-of-file
    // marker, since a one-column list of signatures is not delimited.
    if is_pdf(data, kind == Some(SourceKind::Csv))
        && !matches!(kind, Some(SourceKind::Markdown | SourceKind::Html))
        && !is_html(data)
        && !is_delimited(data)
    {
        return SourceKind::Pdf;
    }
    let office_97 = matches!(
        kind,
        Some(SourceKind::Xls | SourceKind::Doc | SourceKind::Ppt)
    );
    if (office_97 || kind.is_none()) && !data.starts_with(&COMPOUND_MAGIC) {
        if let Some(k) = by_content(data, office_97) {
            return k;
        }
    }
    match kind {
        Some(SourceKind::Pdf | SourceKind::Unsupported) | None => {}
        Some(k) => return k,
    }
    match kind {
        _ if is_pdf(data, false) => SourceKind::Pdf,
        Some(SourceKind::Unsupported) => SourceKind::Unsupported,
        _ if data.starts_with(&COMPOUND_MAGIC) => SourceKind::Unsupported,
        _ => SourceKind::Pdf,
    }
}

/// What an XPS page is read through by OCR: the picture of a page that is
/// only a picture, whole, or the part of a picture under each raster region
/// the router lists on a page of text, its pixels decoded upright, and the
/// box on the page they show. The crop is named as a PDF's is (`p<N>_full`,
/// `p<N>_r<i>`), so its words and its readings go by the same names.
pub(crate) struct XpsCrop {
    pub page: usize,
    pub crop: fluree_doc_pdf::ocr::Crop,
    pub pixels: fluree_doc_vision::Pixels,
    pub bbox: fluree_doc_pdf::geom::BBox,
    /// The manifest's reason: why the router sends it to a reader.
    pub reason: &'static str,
}

/// A crop of an XPS page before its picture is decoded: the picture, and
/// the region of it the crop shows (`None`: all of it).
pub(crate) struct XpsCropPlan<'x> {
    pub page: usize,
    pub crop: fluree_doc_pdf::ocr::Crop,
    pub image: &'x fluree_doc_xps::PageImage,
    region: Option<fluree_doc_pdf::geom::BBox>,
    pub reason: &'static str,
}

impl XpsCropPlan<'_> {
    /// The pixels of its picture, `width` × `height` upright, the crop
    /// shows (`[x0, y0, x1, y1)`), and the box on the page they show.
    pub(crate) fn rect(
        &self,
        width: u32,
        height: u32,
    ) -> Option<([u32; 4], fluree_doc_pdf::geom::BBox)> {
        match &self.region {
            None => (width > 0 && height > 0).then_some(([0, 0, width, height], self.image.bbox)),
            Some(region) => crop_rect(width, height, &self.image.bbox, region),
        }
    }

    /// The box on the page the crop shows, by its picture's size alone:
    /// what words read from its pixels are placed by.
    pub(crate) fn bbox(&self, data: &[u8]) -> Result<fluree_doc_pdf::geom::BBox, String> {
        let bytes = fluree_doc_xps::read_image(data, &self.image.part)?;
        let (w, h) = fluree_doc_image::upright_size(&bytes)?;
        self.rect(w, h)
            .map(|(_, b)| b)
            .ok_or_else(|| format!("{}: shows nothing of its picture", self.image.part))
    }
}

/// The crops of an XPS document's pages (all, or those `pages` keeps), in
/// page order, before any picture is decoded.
pub(crate) fn xps_crop_plans<'x>(
    xps: &'x fluree_doc_xps::Xps,
    pages: Option<&[usize]>,
) -> Vec<XpsCropPlan<'x>> {
    use fluree_doc_pdf::escalate::CropReason;
    use fluree_doc_pdf::ocr::Crop;
    use fluree_doc_pdf::route::{decide, ocr_regions, Route};
    let mut out = Vec::new();
    for p in &xps.doc.pages {
        if pages.is_some_and(|keep| !keep.contains(&p.index)) {
            continue;
        }
        match decide(p).0 {
            Route::Vlm(reason) => {
                if let Some(image) = xps.page_image(p.index) {
                    out.push(XpsCropPlan {
                        page: p.index,
                        crop: Crop::Page,
                        image,
                        region: None,
                        reason: CropReason::from(reason).as_str(),
                    });
                }
            }
            Route::VlmRegions(routed) => {
                for (i, region) in ocr_regions(p).into_iter().enumerate() {
                    let area = |b: &fluree_doc_pdf::geom::BBox| {
                        (b.x1.min(region.x1) - b.x0.max(region.x0)).max(0.0)
                            * (b.y1.min(region.y1) - b.y0.max(region.y0)).max(0.0)
                    };
                    let Some(image) = xps
                        .images
                        .iter()
                        .filter(|im| im.page == p.index && area(&im.bbox) > 0.0)
                        .max_by(|a, b| area(&a.bbox).total_cmp(&area(&b.bbox)))
                    else {
                        continue;
                    };
                    out.push(XpsCropPlan {
                        page: p.index,
                        crop: Crop::Region(i),
                        image,
                        region: Some(region),
                        reason: if i < routed.len() {
                            CropReason::Region.as_str()
                        } else {
                            CropReason::SmallRegion.as_str()
                        },
                    });
                }
            }
            Route::Deterministic => {}
        }
    }
    out
}

/// The most pixels the pictures an XPS document's crops are cut from may
/// hold in all, decoded: 512 million, two gigabytes of RGBA, read one page's
/// pictures at a time.
pub(crate) const MAX_XPS_CROP_PIXELS: u64 = 1 << 29;

/// The crops `plans` lists, each handed to `each` as soon as it is cut: a
/// page's pictures are decoded when it is reached and let go when the next
/// is, so a document's pictures are never held at once. Each picture's size
/// is checked against what is left of [`MAX_XPS_CROP_PIXELS`] before it is
/// decoded; past it, no more are. Returns a message for each picture that
/// does not decode, and one for those past the budget.
pub(crate) fn xps_crops(
    data: &[u8],
    plans: &[XpsCropPlan<'_>],
    each: &mut dyn FnMut(XpsCrop),
) -> Vec<String> {
    xps_crops_within(data, plans, MAX_XPS_CROP_PIXELS, each)
}

fn xps_crops_within(
    data: &[u8],
    plans: &[XpsCropPlan<'_>],
    max_pixels: u64,
    each: &mut dyn FnMut(XpsCrop),
) -> Vec<String> {
    let mut failed = Vec::new();
    let mut left = max_pixels;
    // The first picture past the budget, and how many more were not read.
    let mut past: Option<(&str, usize)> = None;
    // This page's pictures, decoded: a page may cut several regions from
    // one.
    let mut page = None;
    let mut decoded: std::collections::HashMap<
        &str,
        Option<std::rc::Rc<fluree_doc_vision::Pixels>>,
    > = Default::default();
    for plan in plans {
        if page != Some(plan.page) {
            decoded.clear();
            page = Some(plan.page);
        }
        let part = plan.image.part.as_str();
        let pixels = match decoded.get(part) {
            Some(p) => p.clone(),
            None if past.is_some() => {
                if let Some((_, more)) = past.as_mut() {
                    *more += 1;
                }
                continue;
            }
            None => {
                let mut over = false;
                let read = fluree_doc_xps::read_image(data, part).and_then(|bytes| {
                    let (w, h) = fluree_doc_image::upright_size(&bytes)?;
                    let n = u64::from(w) * u64::from(h);
                    if n > left {
                        over = true;
                        return Err(String::new());
                    }
                    left -= n;
                    fluree_doc_image::decode(&bytes).map(|d| d.pixels)
                });
                let p = match read {
                    Ok(p) => Some(std::rc::Rc::new(p)),
                    Err(_) if over => {
                        past = Some((part, 0));
                        continue;
                    }
                    Err(e) => {
                        failed.push(format!("{part}: {e}"));
                        None
                    }
                };
                decoded.insert(part, p.clone());
                p
            }
        };
        let Some(pixels) = pixels else {
            continue;
        };
        let Some((rect, bbox)) = plan.rect(pixels.width, pixels.height) else {
            continue;
        };
        let pixels = if rect == [0, 0, pixels.width, pixels.height] {
            // The whole picture: handed over, not copied, where nothing
            // else on the page is cut from it.
            drop(decoded.remove(part));
            std::rc::Rc::try_unwrap(pixels).unwrap_or_else(|p| (*p).clone())
        } else {
            let Some(p) = cut(&pixels, rect) else {
                continue;
            };
            p
        };
        each(XpsCrop {
            page: plan.page,
            crop: plan.crop,
            pixels,
            bbox,
            reason: plan.reason,
        });
    }
    if let Some((part, more)) = past {
        let and = match more {
            0 => String::new(),
            1 => " and the crop after it".into(),
            n => format!(" and the {n} crops after it"),
        };
        failed.push(format!(
            "{part}{and}: not read, past the {max_pixels} pixels a document's pictures are read to"
        ));
    }
    failed
}

/// The part of a picture `width` × `height` drawn at `drawn` that `region`
/// covers: its pixels (`[x0, y0, x1, y1)`), and the box on the page they
/// show.
fn crop_rect(
    width: u32,
    height: u32,
    drawn: &fluree_doc_pdf::geom::BBox,
    region: &fluree_doc_pdf::geom::BBox,
) -> Option<([u32; 4], fluree_doc_pdf::geom::BBox)> {
    let (w, h) = (f64::from(width), f64::from(height));
    let (sx, sy) = (w / drawn.width(), h / drawn.height());
    if !(sx.is_finite() && sy.is_finite()) {
        return None;
    }
    let x0 = ((region.x0.max(drawn.x0) - drawn.x0) * sx)
        .floor()
        .clamp(0.0, w) as u32;
    let y0 = ((region.y0.max(drawn.y0) - drawn.y0) * sy)
        .floor()
        .clamp(0.0, h) as u32;
    let x1 = ((region.x1.min(drawn.x1) - drawn.x0) * sx)
        .ceil()
        .clamp(0.0, w) as u32;
    let y1 = ((region.y1.min(drawn.y1) - drawn.y0) * sy)
        .ceil()
        .clamp(0.0, h) as u32;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let bbox = fluree_doc_pdf::geom::BBox {
        x0: drawn.x0 + f64::from(x0) / sx,
        y0: drawn.y0 + f64::from(y0) / sy,
        x1: drawn.x0 + f64::from(x1) / sx,
        y1: drawn.y0 + f64::from(y1) / sy,
    };
    Some(([x0, y0, x1, y1], bbox))
}

/// The pixels `[x0, y0, x1, y1)` of a picture.
fn cut(
    pixels: &fluree_doc_vision::Pixels,
    [x0, y0, x1, y1]: [u32; 4],
) -> Option<fluree_doc_vision::Pixels> {
    let (cw, ch) = (x1 - x0, y1 - y0);
    let n = pixels.layout.channels();
    let mut data = Vec::with_capacity(cw as usize * ch as usize * n);
    for y in y0..y1 {
        let row = (y as usize * pixels.width as usize + x0 as usize) * n;
        data.extend_from_slice(pixels.data.get(row..row + cw as usize * n)?);
    }
    fluree_doc_vision::Pixels::new(cw, ch, pixels.layout, data)
}

/// The part of a picture drawn at `drawn` that `region` covers: its pixels,
/// and the box on the page they show.
#[cfg(test)]
fn crop_pixels(
    pixels: &fluree_doc_vision::Pixels,
    drawn: &fluree_doc_pdf::geom::BBox,
    region: &fluree_doc_pdf::geom::BBox,
) -> Option<(fluree_doc_vision::Pixels, fluree_doc_pdf::geom::BBox)> {
    let (rect, bbox) = crop_rect(pixels.width, pixels.height, drawn, region)?;
    Some((cut(pixels, rect)?, bbox))
}

/// The files in a directory `convert` reads, in name order.
pub(crate) fn sources_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && source_kind(p).is_some())
        .collect();
    v.sort();
    v
}

pub(crate) fn stem_of(pdf: &Path) -> &str {
    pdf.file_stem().and_then(|x| x.to_str()).unwrap_or("doc")
}

/// The crop render scale, the library's: the crops `render-routed`
/// writes and the ones `convert --escalate` sends are the same pixels.
pub(crate) use fluree_doc_pdf::escalate::VLM_RENDER_SCALE;

/// An opened OCR engine, shared by every document of a run.
#[derive(Clone)]
pub(crate) struct OcrHandle(pub std::sync::Arc<dyn fluree_doc_vision::OcrEngine>);

impl std::fmt::Debug for OcrHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OcrHandle({})", self.0.name())
    }
}

/// Where the optional model tiers read their sidecar files from. Flags
/// override the corresponding `FDOC_*` environment variables; the environment
/// remains so benchmark harnesses can configure tiers without touching the
/// adapter command line.
#[derive(Debug, Clone, Default)]
pub(crate) struct TierConfig {
    pub layout_boxes: Option<PathBuf>,
    pub tier_results: Option<PathBuf>,
    pub structure_results: Option<PathBuf>,
    /// OCR word readings, added as glyphs before analysis.
    pub ocr_words: Option<PathBuf>,
    /// An OCR engine reading routed pixels in this run, opened once.
    pub ocr: Option<OcrHandle>,
    pub emit_anchors: bool,
    /// Call the configured reader during this run.
    pub escalate: bool,
    /// What the config file says, loaded once per invocation rather than per
    /// document — a batch of a thousand files must not read it a thousand
    /// times.
    pub config: crate::config::Config,
    /// The file `config` was read from, when [`Self::configured`] found one.
    pub config_path: Option<PathBuf>,
    /// What [`COLUMNS_ENV`](crate::escalate::jobs::COLUMNS_ENV) decides about
    /// column doubt for this run, over `config`: `Some(false)` for `0`,
    /// `Some(true)` for any other value, `None` unset. Read once with the
    /// rest of the environment, so every command resolves column doubt from
    /// the same two inputs ([`crate::escalate::jobs::escalates_columns`]).
    pub columns: Option<bool>,
    pub verbose: bool,
    /// `FDOC_TABLE_CONF`: print each table's confidence signals.
    pub table_conf_debug: bool,
    /// `FDOC_INSERT_TABLES`: anchor the tables a layout detector found and
    /// no grid covered.
    pub insert_missing_tables: bool,
}

impl TierConfig {
    pub fn from_env() -> Self {
        Self {
            layout_boxes: std::env::var_os("FDOC_TITLE_BOXES").map(PathBuf::from),
            tier_results: std::env::var_os("FDOC_TIER_RESULTS").map(PathBuf::from),
            structure_results: std::env::var_os("FDOC_STRUCTURE_RESULTS").map(PathBuf::from),
            ocr_words: std::env::var_os("FDOC_OCR_WORDS").map(PathBuf::from),
            ocr: None,
            emit_anchors: std::env::var_os("FDOC_VLM_ANCHORS").is_some(),
            escalate: false,
            config: crate::config::Config::default(),
            config_path: None,
            columns: crate::escalate::jobs::columns_override(
                std::env::var_os(crate::escalate::jobs::COLUMNS_ENV).as_deref(),
            ),
            verbose: false,
            table_conf_debug: std::env::var_os("FDOC_TABLE_CONF").is_some(),
            insert_missing_tables: std::env::var_os("FDOC_INSERT_TABLES").is_some(),
        }
    }

    /// Environment config and the config file, for a command that calls no
    /// reader but chooses the crops one would be handed: what the file says
    /// about escalation (`escalation.on_column_doubt`) changes that set, so
    /// a command that left it out would count a set `convert --escalate`
    /// does not send. A file that cannot be read is warned about and left
    /// out, as `convert` leaves it out.
    pub fn configured() -> Self {
        let mut tiers = Self::from_env();
        match crate::config::load() {
            Ok(l) => {
                tiers.config_path = l.exists.then_some(l.path);
                tiers.config = l.config;
            }
            Err(e) => eprintln!("warning: {e}"),
        }
        tiers
    }

    /// Environment config with per-flag overrides on top.
    pub fn from_env_with(
        layout_boxes: Option<&Path>,
        tier_results: Option<&Path>,
        structure_results: Option<&Path>,
        ocr_words: Option<&Path>,
        emit_anchors: bool,
    ) -> Self {
        Self::from_env().with_flags(
            layout_boxes,
            tier_results,
            structure_results,
            ocr_words,
            emit_anchors,
        )
    }

    /// `self` with the flags of [`Self::from_env_with`] on top. What no flag
    /// overrides is kept, column doubt among it: `convert` takes it from the
    /// environment as `triage` and `render-routed` do.
    fn with_flags(
        self,
        layout_boxes: Option<&Path>,
        tier_results: Option<&Path>,
        structure_results: Option<&Path>,
        ocr_words: Option<&Path>,
        emit_anchors: bool,
    ) -> Self {
        Self {
            layout_boxes: layout_boxes.map(Path::to_path_buf).or(self.layout_boxes),
            tier_results: tier_results.map(Path::to_path_buf).or(self.tier_results),
            structure_results: structure_results
                .map(Path::to_path_buf)
                .or(self.structure_results),
            ocr_words: ocr_words.map(Path::to_path_buf).or(self.ocr_words),
            emit_anchors: emit_anchors || self.emit_anchors,
            ..self
        }
    }

    /// Decide whether this run calls a reader, and say so when the answer is
    /// a surprise.
    ///
    /// A configured provider escalates by default, because configuring one is
    /// itself the decision to use it. Asking for escalation without a
    /// provider is not an error — the deterministic parse is still the right
    /// output — but it must not be silent, or a batch run under a wrong
    /// assumption looks like a working one.
    pub fn resolve_escalation(&mut self, on: bool, off: bool, quiet: bool) {
        let loaded = match crate::config::load() {
            Ok(l) => l,
            Err(e) => {
                eprintln!("warning: {e}");
                eprintln!("         parsing deterministically");
                return;
            }
        };
        self.config = loaded.config;
        let ready = self.config.reader_is_configured();
        self.escalate = !off && (on || (ready && self.config.escalation.enabled));
        if self.escalate && !ready {
            let why = self
                .config
                .missing()
                .unwrap_or_else(|| "no reader is configured".into());
            eprintln!("warning: --escalate was asked for but {why}");
            eprintln!("         run `fdoc config gemini --credentials <key.json>` to set one up");
            eprintln!("         parsing deterministically");
            self.escalate = false;
        }
        if self.escalate && self.tier_results.is_some() && !quiet {
            eprintln!("note: --tier-results supplies the readings; not calling the model");
        }
    }

    pub fn options_for(&self, stem: &str) -> fluree_doc_pdf::document::AnalyzeOptions {
        fluree_doc_pdf::document::AnalyzeOptions {
            layout_prefix: self.layout_boxes.as_ref().map(|d| d.join(stem)),
            // Anchors whenever this run splices readings, from sidecars or
            // from a reader it calls itself: a region, table or inserted
            // table reading lands only at its anchor, and one with nowhere
            // to land was paid for and dropped. The splice removes every
            // anchor no reading answers, so none reaches the output unless
            // `--emit-anchors` asked for them.
            emit_anchors: self.emit_anchors || self.tier_results.is_some() || self.escalate,
            table_conf_debug: self.table_conf_debug,
            insert_missing_tables: self.insert_missing_tables,
        }
    }
}

/// Build core options from the CLI's environment variables.
pub(crate) fn opts_for(pdf: &Path) -> fluree_doc_pdf::document::AnalyzeOptions {
    TierConfig::from_env().options_for(stem_of(pdf))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LayoutRole {
    DocumentTitle,
    FloatTitle,
    FloatBody,
    Ignore,
}

fn layout_role(label: &str) -> LayoutRole {
    match label {
        "paragraph_title" | "doc_title" | "title" => LayoutRole::DocumentTitle,
        "figure_title" | "chart_title" | "table_title" => LayoutRole::FloatTitle,
        "figure_caption" | "chart" | "image" | "figure" => LayoutRole::FloatBody,
        _ => LayoutRole::Ignore,
    }
}

fn box_covers(
    element: fluree_doc_model::geom::BBox,
    &(x0, y0, x1, y1): &(f64, f64, f64, f64),
) -> bool {
    let area = ((element.x1 - element.x0) * (element.y1 - element.y0)).max(1.0);
    let ix = (element.x1.min(x1) - element.x0.max(x0)).max(0.0);
    let iy = (element.y1.min(y1) - element.y0.max(y0)).max(0.0);
    ix * iy >= 0.5 * area
}

fn explicit_float_caption(text: &str) -> bool {
    let Some(first) = text.split_whitespace().next() else {
        return false;
    };
    matches!(
        first
            .trim_matches(|c: char| !c.is_ascii_alphabetic())
            .to_ascii_lowercase()
            .as_str(),
        "figure" | "fig" | "table" | "chart"
    )
}

/// Arbitrate headings where a layout detector saw a title or float.
///
/// `dir` holds `<stem>_p<N>_page.json` files — layout-detector boxes over 2x
/// page renders. Document-title boxes promote short prose blocks. Float-body
/// boxes demote non-outline headings. Float-title boxes demote explicit
/// `Figure`/`Table`/`Chart` captions, but otherwise defer to the deterministic
/// reading because the benchmark treats some descriptive float titles as
/// document headings.
pub(crate) fn arbitrate_layout_titles(
    dir: Option<&Path>,
    stem: &str,
    elements: &mut [fluree_doc_pdf::document::Element],
) {
    let Some(dir) = dir else {
        return;
    };
    let mut boxes_per_page: std::collections::HashMap<usize, Vec<(f64, f64, f64, f64)>> =
        Default::default();
    let mut float_title_boxes: std::collections::HashMap<usize, Vec<(f64, f64, f64, f64)>> =
        Default::default();
    let mut caption_boxes: std::collections::HashMap<usize, Vec<(f64, f64, f64, f64)>> =
        Default::default();
    let mut pages_seen: std::collections::HashSet<usize> = Default::default();
    for e in elements.iter() {
        if !pages_seen.insert(e.page) {
            continue;
        }
        let f = dir.join(format!("{stem}_p{}_page.json", e.page));
        let Ok(txt) = std::fs::read_to_string(&f) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
            continue;
        };
        for b in v.as_array().into_iter().flatten() {
            let label = b["label"].as_str().unwrap_or("");
            let score = b["score"].as_f64().unwrap_or(0.0);
            let role = layout_role(label);
            if role == LayoutRole::Ignore || score < 0.6 {
                continue;
            }
            let c: Vec<f64> = b["box"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_f64())
                .collect();
            if c.len() == 4 {
                // Pixel space at 2x back to PDF units.
                let r = (c[0] / 2.0, c[1] / 2.0, c[2] / 2.0, c[3] / 2.0);
                match role {
                    LayoutRole::DocumentTitle => {
                        boxes_per_page.entry(e.page).or_default().push(r);
                    }
                    LayoutRole::FloatTitle => {
                        float_title_boxes.entry(e.page).or_default().push(r);
                    }
                    LayoutRole::FloatBody => {
                        caption_boxes.entry(e.page).or_default().push(r);
                    }
                    LayoutRole::Ignore => {}
                }
            }
        }
    }
    // Arbitrated demotion: a heading of ours sitting in a figure/chart body is
    // float content. Float titles are deliberately excluded: this corpus'
    // ground truth blesses some of them as document headings.
    for e in elements.iter_mut() {
        if e.kind != "doco:SectionTitle" || e.evidence == "outline" {
            continue;
        }
        let eb = e.rect();
        let in_caption = caption_boxes
            .get(&e.page)
            .is_some_and(|bs| bs.iter().any(|b| box_covers(eb, b)));
        let in_float_title = float_title_boxes
            .get(&e.page)
            .is_some_and(|bs| bs.iter().any(|b| box_covers(eb, b)));
        let in_title = boxes_per_page
            .get(&e.page)
            .is_some_and(|bs| bs.iter().any(|b| box_covers(eb, b)));
        if (in_caption || (in_float_title && explicit_float_caption(&e.text))) && !in_title {
            e.kind = "doco:Paragraph".to_string();
            e.level = None;
            e.evidence = "layout-caption";
        }
    }
    for e in elements.iter_mut() {
        if e.kind != "doco:Paragraph" && e.kind != "doc:ListItem" {
            continue;
        }
        if e.text.chars().count() > 120 {
            continue;
        }
        let Some(boxes) = boxes_per_page.get(&e.page) else {
            continue;
        };
        let eb = e.rect();
        for b in boxes {
            if box_covers(eb, b) {
                e.kind = "doco:SectionTitle".to_string();
                e.level = Some(2);
                e.evidence = "layout-title";
                break;
            }
        }
    }
}

/// The OCR word readings `dir` holds for one document: for each page, the
/// whole-page reading `{stem}_p{N}_full.json` and one `{stem}_p{N}_r{i}.json`
/// per region the router routes, in page and crop order.
///
/// The names are asked for, not searched: a batch shares one directory, and
/// listing it once per document would cost every document the whole batch.
/// Region readings are asked for only up to the page's routed regions; a
/// whole-page reading is asked for on every page, so one the router would
/// not send a reader is still found and refused aloud.
///
/// Unlike the model-tier sidecars, a file that does not parse is reported
/// (unless `--quiet`): a word reading is produced by the embedder's own
/// pipeline, and one it cannot read is a defect in it, not a partial batch.
/// Scores out of range are not: [`warn_scores_out_of_range`] says so of the
/// readings the caller takes, once it has chosen them.
pub(crate) fn ocr_readings(
    dir: &Path,
    stem: &str,
    doc: &fluree_doc_pdf::Document,
    quiet: bool,
) -> Vec<(
    usize,
    fluree_doc_pdf::ocr::Crop,
    fluree_doc_pdf::ocr::WordReading,
)> {
    use fluree_doc_pdf::ocr::{Crop, WordReading};
    let mut out = Vec::new();
    // Numbered as the analysis's anchors and crops are.
    for p in &fluree_doc_pdf::document::routing_pages(doc) {
        // The regions no model is sent too: an engine reads them all.
        let regions = fluree_doc_pdf::route::ocr_regions(p).len();
        let crops = std::iter::once(Crop::Page).chain((0..regions).map(Crop::Region));
        for crop in crops {
            let name = format!("{stem}_p{}_{}.json", p.index, crop.tag());
            let path = dir.join(&name);
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    if !quiet {
                        eprintln!("warning: --ocr-words: {name}: {e}");
                    }
                    continue;
                }
            };
            match WordReading::from_json(&text) {
                Ok(r) => out.push((p.index, crop, r)),
                Err(e) => {
                    if !quiet {
                        eprintln!("warning: --ocr-words: {name}: {e}");
                    }
                }
            }
        }
    }
    out
}

/// Say (unless `--quiet`) that `name` reports no confidence, `n` of its
/// `words` scores being outside 0-1
/// ([`WordReading::scores_out_of_range`](fluree_doc_pdf::ocr::WordReading::scores_out_of_range)).
/// The sidecar's `conf` is 0-1: a reading scored 0-100, as Tesseract
/// scores, would otherwise lose its confidence without a word of why, and
/// with it the noise rule's judgement by confidence of a reading of a
/// region or of a picture
/// ([`MIN_PICTURE_CONFIDENCE`](fluree_doc_pdf::ocr::MIN_PICTURE_CONFIDENCE)):
/// only its boxes are judged, as those of a reading scored not at all are.
/// A reading of a whole page, or of an image read as a file, is not judged
/// noise at all.
pub(crate) fn warn_scores_out_of_range(name: &str, n: usize, words: usize, quiet: bool) {
    if n > 0 && !quiet {
        eprintln!(
            "warning: --ocr-words: {name}: {n} of {words} word score(s) outside 0-1, so no confidence is reported for it, nor, for a region or a picture, is it judged noise by its confidence (`conf` is 0-1)"
        );
    }
}

/// Splice model-tier readings (VLM crops, and optionally table-structure
/// readings for the three-way veto) into the element stream. The readings
/// of the region crops named in `screens` ([`pasted_regions`]) are a
/// screen's, their headings read as paragraphs ([`screen_blocks`]).
pub(crate) fn apply_tiers(
    tier_results: Option<&Path>,
    structure_results: Option<&Path>,
    emit_anchors: bool,
    stem: &str,
    elements: &mut Vec<fluree_doc_pdf::document::Element>,
    pages: &[Vec<String>],
    furniture: &[(String, bool)],
    screens: &std::collections::HashSet<String>,
) {
    let Some(dir) = tier_results else {
        return;
    };
    let vlm = fluree_doc_pdf::arbiter::FixtureBackend::new(dir.to_path_buf());
    let structure = structure_backend(structure_results);
    splice(
        emit_anchors,
        elements,
        stem,
        &vlm,
        structure.as_ref().map(|s| s as _),
        pages,
        furniture,
        screens,
    );
}

/// The structure readings in `structure_results`, for the three-way veto.
/// One per run of a document, so the crops chosen and the splice read each
/// file once between them.
pub(crate) fn structure_backend(
    structure_results: Option<&Path>,
) -> Option<fluree_doc_pdf::arbiter::FixtureBackend> {
    structure_results.map(|d| fluree_doc_pdf::arbiter::FixtureBackend::new(d.to_path_buf()))
}

/// Splice readings into the element stream, from sidecars or from a reader
/// called in this run.
///
/// `structure` is read for the three-way veto whichever produced
/// `readings`: a table reading is as likely to be wrong from a model called
/// here as from a sidecar, and the structure reading is what overrules it.
///
/// An anchor no reading answers drops, unless the anchors were asked for
/// (`--emit-anchors`): then it stays where it is for a later tier to fill,
/// and only what was read in this run lands. The readings of the region
/// crops named in `screens` ([`pasted_regions`]) are a screen's, their
/// headings read as paragraphs ([`screen_blocks`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn splice(
    emit_anchors: bool,
    elements: &mut Vec<fluree_doc_pdf::document::Element>,
    stem: &str,
    readings: &dyn fluree_doc_pdf::arbiter::TierBackend,
    structure: Option<&dyn fluree_doc_pdf::arbiter::TierBackend>,
    pages: &[Vec<String>],
    furniture: &[(String, bool)],
    screens: &std::collections::HashSet<String>,
) {
    let readings = Screens {
        inner: readings,
        crops: screens,
    };
    if emit_anchors {
        fluree_doc_pdf::arbiter::splice_leaving_unanswered(
            elements, stem, &readings, structure, pages,
        );
    } else {
        fluree_doc_pdf::arbiter::splice_with_page(elements, stem, &readings, structure, pages);
    }
    // The same furniture the deterministic pass already stripped, so a page
    // reads the same whether or not it escalated.
    fluree_doc_pdf::arbiter::scrub_furniture(elements, furniture);
}

/// A model's reading of a screen pasted into a document (a picture a DOCX,
/// an XLSX or an e-mail places, or a region of a PDF's page of text), with
/// its headings read as paragraphs: they
/// are the screen's, not the document's, and as headings they would put the
/// document's own text under them. A title block is read as text, and a
/// line marked up as a Markdown heading loses its marks; a table is left as
/// it is.
pub(crate) fn screen_blocks(
    blocks: Vec<fluree_doc_pdf::arbiter::Block>,
) -> Vec<fluree_doc_pdf::arbiter::Block> {
    blocks
        .into_iter()
        .map(|mut b| {
            if matches!(b.label.as_str(), "title" | "doc_title" | "paragraph_title") {
                b.label = "text".into();
            }
            if !b.content.trim_start().starts_with("<table") {
                b.content = b
                    .content
                    .lines()
                    .map(|line| {
                        let t = line.trim_start();
                        let marks = t.len() - t.trim_start_matches('#').len();
                        let rest = &t[marks..];
                        if (1..=6).contains(&marks) && rest.starts_with([' ', '\t']) {
                            rest.trim_start()
                        } else {
                            line
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            b
        })
        .collect()
}

/// The crops a screen pasted into a page fills, whose readings are read as
/// a screen's: the region crops (`p<N>_r<i>`) of the anchors in `elements`
/// a pasted region fills ([`fluree_doc_pdf::route::pasted`]), the rule
/// OCR's reading of it follows ([`fluree_doc_pdf::ocr::mark_provenance`]).
/// A region that, with the images it abuts, covers half its page or more is
/// the page's content, a scan under a stamped line the text layer carries,
/// whole or stored in strips, and keeps its headings.
///
/// A page read whole (`p<N>_full`) keeps its headings, even one near blank
/// but for such a screen, where OCR demotes those whose words land in the
/// screen: a model's reading has no positions to tell the screen's title
/// from the page's own, a cover's drawn in outlines or a caption that
/// repeats the screen's name.
pub(crate) fn pasted_regions(
    elements: &[fluree_doc_pdf::document::Element],
    pages: &[fluree_doc_pdf::Page],
) -> std::collections::HashSet<String> {
    // The anchors by page, each judged with its page's other anchors
    // ([`fluree_doc_pdf::route::pasted_set`]): the page's pictures are
    // measured once.
    let mut by_page: Vec<(
        &fluree_doc_pdf::Page,
        Vec<String>,
        Vec<fluree_doc_pdf::BBox>,
    )> = Vec::new();
    for e in elements {
        let Some(crop) = (e.text.strip_prefix("[[VLM:")).and_then(|t| t.strip_suffix("]]")) else {
            continue;
        };
        let (Some(b), Some(page)) = (e.bbox, pages.iter().find(|p| p.index == e.page)) else {
            continue;
        };
        let at = match by_page.iter().position(|(p, ..)| p.index == page.index) {
            Some(at) => at,
            None => {
                by_page.push((page, Vec::new(), Vec::new()));
                by_page.len() - 1
            }
        };
        // An element's box is in the display frame, the page's images in
        // its reading frame.
        by_page[at].1.push(crop.replace(':', "_"));
        by_page[at].2.push(page.from_display(b));
    }
    by_page
        .into_iter()
        .flat_map(|(page, crops, boxes)| {
            let pasted = fluree_doc_pdf::route::pasted_set(page, &boxes);
            crops
                .into_iter()
                .zip(pasted)
                .filter_map(|(c, p)| p.then_some(c))
        })
        .collect()
}

/// Model readings whose crops named in `crops` are read as a screen's
/// ([`screen_blocks`]).
struct Screens<'a> {
    inner: &'a dyn fluree_doc_pdf::arbiter::TierBackend,
    crops: &'a std::collections::HashSet<String>,
}

impl fluree_doc_pdf::arbiter::TierBackend for Screens<'_> {
    fn read(&self, stem: &str, crop: &str) -> Option<Vec<fluree_doc_pdf::arbiter::Block>> {
        let blocks = self.inner.read(stem, crop)?;
        Some(if self.crops.contains(crop) {
            screen_blocks(blocks)
        } else {
            blocks
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluree_doc_model::{element::Element, geom::BBox};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn element(kind: &str, text: &str, bbox: BBox) -> Element {
        Element {
            bbox: Some(bbox),
            level: (kind == "doco:SectionTitle").then_some(2),
            ..Element::new(kind, text, "rust", "font-size")
        }
    }

    /// `convert`'s flags override the environment they name and keep the
    /// rest of it: `FDOC_ESCALATE_COLUMNS` decides column doubt for
    /// `convert` as for the commands that take no flag.
    #[test]
    fn the_flags_keep_what_the_environment_says_about_column_doubt() {
        for columns in [None, Some(false), Some(true)] {
            let env = TierConfig {
                tier_results: Some(PathBuf::from("env-readings")),
                columns,
                ..Default::default()
            };
            let cfg = env.with_flags(None, Some(Path::new("flag-readings")), None, None, true);
            assert_eq!(cfg.columns, columns);
            assert_eq!(cfg.tier_results, Some(PathBuf::from("flag-readings")));
            assert!(cfg.emit_anchors);
        }
    }

    #[test]
    fn a_screens_headings_are_read_as_paragraphs() {
        let block = |label: &str, content: &str| fluree_doc_pdf::arbiter::Block {
            label: label.into(),
            content: content.into(),
        };
        let got = screen_blocks(vec![
            block("doc_title", "SCREENSHOT"),
            block("text", "## Balance 9,876.54\n#7781 opened\n####### not one"),
            block("table", "<table><tr><td># kept</td></tr></table>"),
        ]);
        let got: Vec<(&str, &str)> = got
            .iter()
            .map(|b| (b.label.as_str(), b.content.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("text", "SCREENSHOT"),
                ("text", "Balance 9,876.54\n#7781 opened\n####### not one"),
                ("table", "<table><tr><td># kept</td></tr></table>"),
            ]
        );
    }

    #[test]
    fn a_reading_of_a_pasted_region_is_spliced_as_a_screens() {
        // The configured model's readings and those supplied go through
        // the same splice: a region crop named a screen loses its heading,
        // another keeps it.
        struct Titled;
        impl fluree_doc_pdf::arbiter::TierBackend for Titled {
            fn read(&self, _: &str, crop: &str) -> Option<Vec<fluree_doc_pdf::arbiter::Block>> {
                // Region crops only: a whole page's reading would replace it.
                crop.contains("_r").then(|| {
                    vec![fluree_doc_pdf::arbiter::Block {
                        label: "doc_title".into(),
                        content: format!("Balance of {crop}"),
                    }]
                })
            }
        }
        let anchor = |region: &str| {
            Element::new(
                "doco:Paragraph",
                format!("[[VLM:p0:{region}]]"),
                "rust",
                "region-anchor",
            )
        };
        let mut elements = vec![anchor("r0"), anchor("r1")];
        let screens = std::collections::HashSet::from(["p0_r0".to_string()]);
        splice(
            false,
            &mut elements,
            "doc",
            &Titled,
            None,
            &[],
            &[],
            &screens,
        );
        let texts: Vec<&str> = elements.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["Balance of p0_r0", "# Balance of p0_r1"]);
    }

    #[test]
    fn an_xps_documents_pictures_are_read_within_a_pixel_budget() {
        use std::io::Write;
        // Three pages, each only a picture of 10 × 10 pixels.
        let mut png = Vec::new();
        image::GrayImage::from_pixel(10, 10, image::Luma([255u8]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let page = |n: usize| {
            format!(
                "<FixedPage Width=\"816\" Height=\"1056\"><Path Data=\"M 0,0 L 816,0 816,1056 0,1056 Z\">\
                 <Path.Fill><ImageBrush ImageSource=\"/i{n}.png\" Viewport=\"0,0,816,1056\"/></Path.Fill></Path></FixedPage>"
            )
        };
        let mut parts: Vec<(String, Vec<u8>)> = vec![
            (
                "_rels/.rels".into(),
                b"<Relationships><Relationship Id=\"R0\" Type=\"http://schemas.microsoft.com/xps/2005/06/fixedrepresentation\" Target=\"/s.fdseq\"/></Relationships>".to_vec(),
            ),
            (
                "s.fdseq".into(),
                b"<FixedDocumentSequence><DocumentReference Source=\"/d.fdoc\"/></FixedDocumentSequence>".to_vec(),
            ),
            (
                "d.fdoc".into(),
                b"<FixedDocument><PageContent Source=\"/p0.fpage\"/><PageContent Source=\"/p1.fpage\"/><PageContent Source=\"/p2.fpage\"/></FixedDocument>".to_vec(),
            ),
        ];
        for n in 0..3 {
            parts.push((format!("p{n}.fpage"), page(n).into_bytes()));
            parts.push((format!("i{n}.png"), png.clone()));
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        for (name, body) in &parts {
            z.start_file(name.as_str(), zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(body).unwrap();
        }
        z.finish().unwrap();
        let data = buf.into_inner();
        let xps = fluree_doc_xps::parse(&data).unwrap();
        let plans = xps_crop_plans(&xps, None);
        assert_eq!(plans.len(), 3);
        // Room for two pictures' pixels: the third is not decoded.
        let mut read = Vec::new();
        let failed = xps_crops_within(&data, &plans, 250, &mut |c| read.push(c.page));
        assert_eq!(read, [0, 1]);
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(
            failed[0].starts_with("i2.png: not read, past"),
            "{failed:?}"
        );
        let mut read = Vec::new();
        let failed = xps_crops_within(&data, &plans, 150, &mut |c| read.push(c.page));
        assert_eq!(read, [0]);
        assert!(
            failed[0].starts_with("i1.png and the crop after it: not read"),
            "{failed:?}"
        );
        // A page crop records the box its picture is drawn in.
        let b = plans[0].bbox(&data).unwrap();
        assert_eq!((b.x0, b.y0, b.x1, b.y1), (0.0, 0.0, 612.0, 792.0));
    }

    #[test]
    fn a_region_crops_the_part_of_the_picture_under_it() {
        use fluree_doc_pdf::geom::BBox;
        // 4 × 2 pixels drawn over 40 × 20 units, each pixel its own value.
        let rgba: Vec<u8> = (0u8..8).flat_map(|v| [v, v, v, 255]).collect();
        let pixels =
            fluree_doc_vision::Pixels::new(4, 2, fluree_doc_vision::Layout::Rgba8, rgba).unwrap();
        let drawn = BBox {
            x0: 100.0,
            y0: 50.0,
            x1: 140.0,
            y1: 70.0,
        };
        // The region runs past the picture's right edge: cut to it.
        let region = BBox {
            x0: 110.0,
            y0: 50.0,
            x1: 160.0,
            y1: 60.0,
        };
        let (crop, bbox) = crop_pixels(&pixels, &drawn, &region).unwrap();
        assert_eq!((crop.width, crop.height), (3, 1));
        assert_eq!(
            crop.data.chunks_exact(4).map(|p| p[0]).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            (bbox.x0, bbox.y0, bbox.x1, bbox.y1),
            (110.0, 50.0, 140.0, 60.0)
        );
        // A region beside the picture covers none of it.
        let beside = BBox {
            x0: 0.0,
            y0: 0.0,
            x1: 50.0,
            y1: 50.0,
        };
        assert!(crop_pixels(&pixels, &drawn, &beside).is_none());
    }

    #[test]
    fn a_format_with_no_reader_is_refused_by_name_not_read_as_a_pdf() {
        let compound = [&COMPOUND_MAGIC[..], &[0u8; 504]].concat();
        let zip = b"PK\x03\x04 an Apple package".to_vec();
        for (name, data) in [
            ("letter.wpd", &compound),
            ("Letter.PAGES", &compound),
            ("slides.key", &zip),
            ("notes.numbers", &zip),
        ] {
            let path = Path::new(name);
            assert_eq!(input_kind(path, data), SourceKind::Unsupported, "{name}");
            let why = unsupported(Some(path), data).unwrap();
            let ext = path.extension().unwrap().to_str().unwrap().to_lowercase();
            assert!(
                why.starts_with(&format!("unsupported format (.{ext}, ")),
                "{why}"
            );
        }
        // Discovery lists them, so a batch counts them.
        assert_eq!(
            source_kind(Path::new("a.wpd")),
            Some(SourceKind::Unsupported)
        );
        // A compound file by its content alone, renamed or from stdin.
        assert_eq!(
            input_kind(Path::new("scan"), &compound),
            SourceKind::Unsupported
        );
        assert!(unsupported(None, &compound)
            .unwrap()
            .contains("compound file"));
        // Content that is a PDF or an image is read as one, whatever its name.
        let pdf = b"%PDF-1.4\n%junk".to_vec();
        assert_eq!(input_kind(Path::new("x.pages"), &pdf), SourceKind::Pdf);
        assert_eq!(input_kind(Path::new("x.dat"), &pdf), SourceKind::Pdf);
        assert_eq!(unsupported(None, &pdf), None);
        let png = b"\x89PNG\r\n\x1a\n....".to_vec();
        assert_eq!(input_kind(Path::new("x.wpd"), &png), SourceKind::Image);
        // Anything else still goes to the PDF reader, as before.
        assert_eq!(input_kind(Path::new("x.bin"), b"junk"), SourceKind::Pdf);
    }

    #[test]
    fn content_with_a_signature_is_read_as_what_it_is_whatever_its_name() {
        use std::io::Write;
        let pdf = b"%PDF-1.7\n1 0 obj\n<< >>\nendobj\n".to_vec();
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        z.start_file("_rels/.rels", zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(
            b"<Relationships><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/\
              officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>\
              </Relationships>",
        )
        .unwrap();
        z.finish().unwrap();
        let xlsx = buf.into_inner();
        for (name, data, kind) in [
            ("scan.doc", &pdf, SourceKind::Pdf),
            ("report.xls", &pdf, SourceKind::Pdf),
            ("report.ppt", &pdf, SourceKind::Pdf),
            ("report.rtf", &pdf, SourceKind::Pdf),
            ("report.csv", &pdf, SourceKind::Pdf),
            ("report.docx", &pdf, SourceKind::Pdf),
            ("report.odt", &pdf, SourceKind::Pdf),
            ("report.xps", &pdf, SourceKind::Pdf),
            ("report.md", &pdf, SourceKind::Pdf),
            ("shot.doc", &png, SourceKind::Image),
            ("shot.csv", &png, SourceKind::Image),
            ("shot.key", &png, SourceKind::Image),
            ("book.csv", &xlsx, SourceKind::Xlsx),
            ("book.doc", &xlsx, SourceKind::Xlsx),
            ("-", &pdf, SourceKind::Pdf),
            ("-", &png, SourceKind::Image),
        ] {
            assert_eq!(input_kind(Path::new(name), data), kind, "{name}");
        }
        // A PDF a few bytes in, as a gateway's prefix leaves it, is one
        // whatever its name, by the binary line after its header or by its
        // end-of-file marker, but for text that may quote a header.
        let prefix = &b"\r\n\r\n\0\x01junk"[..];
        let binary = [
            prefix,
            b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n1 0 obj\n<< >>\nendobj\n",
        ]
        .concat();
        let ended = [prefix, &pdf, b"trailer\n<< >>\n%%EOF\n"].concat();
        for late in [&binary, &ended] {
            for name in ["scan.doc", "report.csv", "report.xls", "report.docx", "-"] {
                assert_eq!(input_kind(Path::new(name), late), SourceKind::Pdf, "{name}");
            }
        }
        let quoting = b"Header,Note\nfile 1,starts with %PDF-1.7\nfile 2,ok\n";
        assert_eq!(input_kind(Path::new("list.csv"), quoting), SourceKind::Csv);
        // A header alone is no PDF: a list of signatures, one column, one
        // a line, is the table it is named as.
        let signatures = b"Signature\n%PDF-1.7\n%PNG\nGIF89a\n";
        assert_eq!(
            input_kind(Path::new("magic.csv"), signatures),
            SourceKind::Csv
        );
        // Nor with the end-of-file marker listed too, in one column, or in
        // two whose quoted commas hide the delimiter: named as a table, the
        // marker counts only beside a byte no text holds.
        for (name, list) in [
            ("one.csv", &b"marker\n%PDF-1.7\n%%EOF\nstartxref\n"[..]),
            ("magic.tsv", b"Signature\n%PDF-\nPK\nGIF89a\n%%EOF\n"),
            (
                "two.csv",
                b"Signature,Note\n%PDF-,\"opens, a PDF\"\n%%EOF,\"ends, it\"\n",
            ),
        ] {
            assert_eq!(input_kind(Path::new(name), list), SourceKind::Csv, "{name}");
        }
        // The same list named as nothing in particular is still taken for
        // the PDF its marker says.
        assert_eq!(
            input_kind(Path::new("-"), b"marker\n%PDF-1.7\n%%EOF\n"),
            SourceKind::Pdf
        );
        let late = ended;
        let page = b"<html><body><p>A PDF opens with %PDF-1.7.</p></body></html>";
        assert_eq!(input_kind(Path::new("page.doc"), page), SourceKind::Html);
        assert_eq!(
            input_kind(Path::new("notes.md"), &late),
            SourceKind::Markdown
        );
        // RTF by its opening, whatever its name.
        assert_eq!(
            input_kind(Path::new("memo.csv"), b"{\\rtf1\\ansi Hello}"),
            SourceKind::Rtf
        );
    }

    #[test]
    fn a_file_named_as_an_office_97_one_is_read_as_what_it_holds() {
        use std::io::Write;
        let package = |main: &str| {
            let mut buf = std::io::Cursor::new(Vec::new());
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("_rels/.rels", zip::write::SimpleFileOptions::default())
                .unwrap();
            write!(
                z,
                "<Relationships><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/\
                 officeDocument/2006/relationships/officeDocument\" Target=\"{main}\"/></Relationships>"
            )
            .unwrap();
            z.finish().unwrap();
            buf.into_inner()
        };
        let html = b"\xEF\xBB\xBF\n<html><body><table><tr><td>a</td></tr></table></body></html>";
        for (name, data, kind) in [
            ("export.xls", &html[..], SourceKind::Html),
            (
                "webpage.doc",
                b"<!DOCTYPE html><p>Saved as doc</p>",
                SourceKind::Html,
            ),
            ("tab.xls", b"col1\tcol2\n1\t2\n", SourceKind::Csv),
            ("semi.xls", b"a;b;c\r\n1;2;3\r\n", SourceKind::Csv),
            (
                "renamed.doc",
                &package("word/document.xml")[..],
                SourceKind::Docx,
            ),
            (
                "renamed.ppt",
                &package("ppt/presentation.xml")[..],
                SourceKind::Pptx,
            ),
            (
                "renamed.xls",
                &package("xl/workbook.xml")[..],
                SourceKind::Xlsx,
            ),
            // Named as nothing, as stdin is: a package and a web page by
            // their content, but plain text is no table for it.
            ("stdin", &package("word/document.xml")[..], SourceKind::Docx),
            ("stdin", b"<html><p>Hi</p></html>", SourceKind::Html),
        ] {
            assert_eq!(input_kind(Path::new(name), data), kind, "{name}");
        }
        // What none of them is still goes to the reader its name says,
        // which says what it is not, never to the PDF reader.
        for (name, kind) in [
            ("notes.xls", SourceKind::Xls),
            ("notes.doc", SourceKind::Doc),
            ("notes.ppt", SourceKind::Ppt),
            ("notes.docx", SourceKind::Docx),
            ("notes.xps", SourceKind::Xps),
            ("photo.png", SourceKind::Image),
        ] {
            assert_eq!(
                input_kind(Path::new(name), b"just some words"),
                kind,
                "{name}"
            );
        }
        assert_eq!(input_kind(Path::new("one.xls"), b"a\tb\n"), SourceKind::Xls);
        assert_eq!(
            input_kind(Path::new("ragged.xls"), b"a\tb\nc d e\n"),
            SourceKind::Xls
        );
        assert_eq!(
            input_kind(Path::new("stdin"), b"a\tb\n1\t2\n"),
            SourceKind::Pdf
        );
    }

    #[test]
    fn an_excel_2003_xml_workbook_is_refused_by_name_not_read_as_a_page() {
        // Its `<Table>` read as a web page's table ran its cells together.
        let xml = b"<?xml version=\"1.0\"?>\n<?mso-application progid=\"Excel.Sheet\"?>\n\
            <Workbook xmlns=\"urn:schemas-microsoft-com:office:spreadsheet\" \
            xmlns:ss=\"urn:schemas-microsoft-com:office:spreadsheet\">\
            <Worksheet ss:Name=\"S\"><Table><Row><Cell><Data ss:Type=\"String\">Name</Data></Cell>\
            </Row></Table></Worksheet></Workbook>";
        for name in ["export.xls", "export.xml", "-"] {
            let path = Path::new(name);
            assert_eq!(input_kind(path, xml), SourceKind::Unsupported, "{name}");
            let why = unsupported(Some(path), xml).unwrap();
            assert!(why.contains("Excel 2003 XML"), "{why}");
        }
        // Without the instruction, by its root's namespace.
        let bare =
            b"<Workbook xmlns=\"urn:schemas-microsoft-com:office:spreadsheet\"><Table/></Workbook>";
        assert_eq!(
            input_kind(Path::new("a.xls"), bare),
            SourceKind::Unsupported
        );
        // Excel's HTML export, which names Excel's namespaces too, is a page.
        let html = b"<html xmlns:x=\"urn:schemas-microsoft-com:office:excel\"><body>\
            <table><tr><td>1</td></tr></table></body></html>";
        assert_eq!(input_kind(Path::new("a.xls"), html), SourceKind::Html);
    }

    #[test]
    fn unicode_text_named_xls_is_delimited_text() {
        // Excel's "Unicode Text" export: UTF-16 with no byte-order mark,
        // tab-separated, which it names `.txt` and users rename `.xls`.
        let tsv: Vec<u8> = "Name\tAmount\nAlex\t12\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(input_kind(Path::new("export.xls"), &tsv), SourceKind::Csv);
        assert_eq!(
            input_kind(Path::new("export.xls"), b"a\tb\0\n1\t2\n"),
            SourceKind::Xls
        );
    }

    #[test]
    fn float_titles_are_neutral_but_float_bodies_demote() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("fdoc-layout-{nonce}"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("sample_p0_page.json"),
            r#"[
                {"label":"figure_title","score":0.9,"box":[0,0,200,20]},
                {"label":"figure_title","score":0.9,"box":[0,40,200,60]},
                {"label":"chart","score":0.9,"box":[0,80,200,100]}
            ]"#,
        )
        .unwrap();
        let mut elements = vec![
            element(
                "doco:SectionTitle",
                "Performance details",
                BBox {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 100.0,
                    y1: 10.0,
                },
            ),
            element(
                "doco:SectionTitle",
                "Figure 4.5. Fuel mix",
                BBox {
                    x0: 0.0,
                    y0: 20.0,
                    x1: 100.0,
                    y1: 30.0,
                },
            ),
            element(
                "doco:SectionTitle",
                "OCR-Recall",
                BBox {
                    x0: 0.0,
                    y0: 40.0,
                    x1: 100.0,
                    y1: 50.0,
                },
            ),
        ];

        arbitrate_layout_titles(Some(&dir), "sample", &mut elements);

        assert_eq!(elements[0].kind, "doco:SectionTitle");
        assert_eq!(elements[1].kind, "doco:Paragraph");
        assert_eq!(elements[1].evidence, "layout-caption");
        assert_eq!(elements[2].kind, "doco:Paragraph");
        assert_eq!(elements[2].evidence, "layout-caption");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
