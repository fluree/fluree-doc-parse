//! A bare image as a one-page document, read by an OCR engine.
//!
//! A PNG screenshot, a phone photo of a form, a scanned TIFF: no text layer,
//! only pixels. This crate decodes the image upright (a phone photo's EXIF
//! orientation applied), hands the pixels to an engine, and lays its words on
//! a one-page document as [`fluree_doc_pdf::ocr`] lays them on a scanned PDF
//! page, so the same layout pass gives paragraphs and tables, each word with
//! its box. The page's units are the image's pixels, one to one, as
//! [`fluree_doc_pdf::image`] sets them.
//!
//! [`read`] gives the document with the words on it, for a caller that
//! analyses it itself; [`elements`] does the analysis too. A document's
//! embedded images (a DOCX's media, an e-mail's inline images) go the same
//! way, one image at a time.
//!
//! Only the first page of a multi-page TIFF is read; [`Decoded::more_pages`]
//! says whether there were more, and [`header`] how many.
//!
//! Some TIFF codings are not read: CCITT Group 3 fax (Group 4 is), YCbCr and
//! CIELab colour. Such a file is refused with the decoder's error.

use fluree_doc_pdf::ocr::{self, Crop, Injected};
use fluree_doc_pdf::Document;
use fluree_doc_vision::{Layout, OcrEngine, Pixels};
use image::{DynamicImage, ExtendedColorType, ImageDecoder, ImageReader};
use std::io::Cursor;

/// An image decoded upright.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// In the layout the image holds: grey, RGB, or RGBA only where it has
    /// transparency.
    pub pixels: Pixels,
    /// One bit per pixel as stored (a fax, a bitonal scan).
    pub bitonal: bool,
    /// More than one page was stored (a multi-page TIFF): only the first is
    /// decoded.
    pub more_pages: bool,
    /// The file ends before its image does (a JPEG cut short): the decoder
    /// filled in what was missing, so part of the page may be blank.
    pub truncated: bool,
}

/// What an image's header says, without decoding its pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Its size upright, by its EXIF orientation: the size [`decode`] gives.
    pub width: u32,
    pub height: u32,
    /// One bit per pixel as stored.
    pub bitonal: bool,
    /// Pages stored: more than one only for a multi-page TIFF.
    pub pages: usize,
}

/// The most pixels an image may hold to be decoded: 128 million. A file's
/// header states its size, and a few kilobytes of PNG can state a size that
/// takes gigabytes to decode; such a file is refused before anything is
/// allocated.
///
/// What [`decode`] holds at once, under this cap and [`MAX_DECODED_BYTES`]:
/// the image as stored (at most 512 MiB), its eight-bit copy where it is
/// stored deeper, and a second copy while a quarter turn by its EXIF
/// orientation is made. The worst case is an 8-bit RGBA image at the cap
/// turned a quarter: about 1 GB (985 MiB measured). Unturned, or grey, it
/// is half that or less. An OCR engine reading it adds an RGB copy (384 MB
/// at the cap) once the turn is done, and its own working memory.
pub const MAX_PIXELS: u64 = 128_000_000;

/// The most bytes the decoder may produce, in the image's own colour type:
/// an 8-bit image keeps all of [`MAX_PIXELS`], one stored wider gets fewer
/// pixels (64 Mi for 16-bit RGBA).
const MAX_DECODED_BYTES: u64 = 512 << 20;

/// The longest side an image may have to be decoded, in pixels: what bounds
/// one row's buffer, so a one-pixel-high image a hundred million wide is
/// refused too.
pub const MAX_SIDE: u32 = 65_535;

/// What a PNG decoder may allocate besides the image: a colour profile or a
/// text chunk, inflated whole as the header is read. A profile runs to
/// kilobytes; a file of a few hundred kilobytes can inflate one to a
/// gigabyte. Past the budget the decoder drops the chunk and reads on. It
/// bounds no pixels ([`decodable`] does), only chunk data and a row.
const PNG_CHUNK_BUDGET: u64 = 16 << 20;

/// The decoder for `bytes` and its format; with `capped`, refused beyond
/// [`MAX_PIXELS`], for a caller about to decode.
fn decoder(
    bytes: &[u8],
    capped: bool,
) -> Result<(impl ImageDecoder + '_, Option<image::ImageFormat>), String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    // A BigTIFF, whose signature the format guess does not know; the TIFF
    // decoder reads it.
    if is_big_tiff(bytes) {
        reader.set_format(image::ImageFormat::Tiff);
    }
    let format = reader.format();
    // The formats this crate reads. The `image` crate a build links may
    // decode more (an OCR engine's dependencies turn on all of them), and
    // each is one more decoder to trust with untrusted bytes.
    use image::ImageFormat::{Jpeg, Png, Tiff};
    if !matches!(format, Some(Png | Jpeg | Tiff)) {
        return Err(match format {
            Some(f) => format!("a {f:?} image: only PNG, JPEG and TIFF are read"),
            None => "not an image: only PNG, JPEG and TIFF are read".into(),
        });
    }
    // One number, two budgets: the TIFF and JPEG decoders bound their
    // buffers by it, while the PNG decoder bounds its chunks by it, inflated
    // as the header is read, before anything here can look at the size.
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(match format {
        Some(Png) => PNG_CHUNK_BUDGET,
        _ => MAX_DECODED_BYTES,
    });
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let (w, h) = decoder.dimensions();
    if capped {
        decodable(w, h, decoder.total_bytes())?;
    }
    Ok((decoder, format))
}

/// Whether an image of `w` × `h` pixels, `bytes` once decoded in its own
/// colour type, may be decoded: within [`MAX_PIXELS`], [`MAX_SIDE`] and
/// [`MAX_DECODED_BYTES`].
fn decodable(w: u32, h: u32, bytes: u64) -> Result<(), String> {
    if w.max(h) > MAX_SIDE {
        return Err(format!(
            "the image is {w} × {h} pixels, longer than the {MAX_SIDE} a side that may be decoded"
        ));
    }
    let pixels = u64::from(w) * u64::from(h);
    if pixels > MAX_PIXELS {
        return Err(format!(
            "the image is {w} × {h} pixels, more than the {MAX_PIXELS} that may be decoded"
        ));
    }
    if bytes > MAX_DECODED_BYTES {
        return Err(format!(
            "the image is {w} × {h} pixels of {} bytes each, too large to decode",
            bytes / pixels.max(1)
        ));
    }
    Ok(())
}

/// Decode a PNG, JPEG or TIFF, turned upright by its EXIF orientation.
/// Refused, before anything is allocated, beyond [`MAX_PIXELS`].
pub fn decode(bytes: &[u8]) -> Result<Decoded, String> {
    let (decoder, format) = decoder(bytes, true)?;
    // A PNG decoder states a one-bit image's colour as the eight bits it
    // expands it to; its header says what is stored.
    let bitonal = is_bitonal(&decoder, bytes);
    // A missing or unreadable orientation tag leaves the image as stored.
    let mut decoder = decoder;
    let orientation = decoder.orientation().ok();
    // Eight bits a channel before it is turned, so a quarter turn copies the
    // smaller image.
    let mut img = eight_bit(DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?);
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    let (w, h) = (img.width(), img.height());
    let (layout, data) = match img {
        DynamicImage::ImageLuma8(b) => (Layout::Luma8, b.into_raw()),
        DynamicImage::ImageRgb8(b) => (Layout::Rgb8, b.into_raw()),
        // Not reached: `eight_bit` gives one of the three.
        other => (Layout::Rgba8, other.into_rgba8().into_raw()),
    };
    let pixels = Pixels::new(w, h, layout, data).ok_or("the image is empty")?;
    let more_pages = format == Some(image::ImageFormat::Tiff) && tiff_pages(bytes) > 1;
    let truncated = format == Some(image::ImageFormat::Jpeg) && !jpeg_is_whole(bytes);
    Ok(Decoded {
        pixels,
        bitonal,
        more_pages,
        truncated,
    })
}

/// `img` in the layout it holds, eight bits a channel: grey stays one byte a
/// pixel (a one-bit fax too), colour three, and only an image with
/// transparency takes four. Kept as it is when it already is one of those.
fn eight_bit(img: DynamicImage) -> DynamicImage {
    use DynamicImage::{ImageLuma8, ImageRgb8, ImageRgba8};
    let color = img.color();
    match img {
        ImageLuma8(_) | ImageRgb8(_) | ImageRgba8(_) => img,
        other if color.has_alpha() => ImageRgba8(other.into_rgba8()),
        other if color.has_color() => ImageRgb8(other.into_rgb8()),
        other => ImageLuma8(other.into_luma8()),
    }
}

/// What the image's header says: its size upright, whether it is one bit
/// deep and how many pages it stores, without decoding its pixels. Not
/// bounded by [`MAX_PIXELS`]: nothing is decoded.
pub fn header(bytes: &[u8]) -> Result<Header, String> {
    use image::metadata::Orientation::*;
    let (mut decoder, format) = decoder(bytes, false)?;
    let (w, h) = decoder.dimensions();
    let bitonal = is_bitonal(&decoder, bytes);
    let (width, height) = match decoder.orientation() {
        Ok(Rotate90 | Rotate270 | Rotate90FlipH | Rotate270FlipH) => (h, w),
        _ => (w, h),
    };
    let pages = if format == Some(image::ImageFormat::Tiff) {
        tiff_pages(bytes).max(1)
    } else {
        1
    };
    Ok(Header {
        width,
        height,
        bitonal,
        pages,
    })
}

/// The image's width and height upright: [`header`]'s.
pub fn upright_size(bytes: &[u8]) -> Result<(u32, u32), String> {
    header(bytes).map(|h| (h.width, h.height))
}

/// One bit per pixel as stored. A PNG decoder states a one-bit image's
/// colour as the eight bits it expands it to; its header says what is
/// stored.
fn is_bitonal(decoder: &impl ImageDecoder, bytes: &[u8]) -> bool {
    decoder.original_color_type() == ExtendedColorType::L1 || png_is_one_bit(bytes)
}

/// A BigTIFF's signature: version 43 where a classic TIFF has 42.
pub fn is_big_tiff(b: &[u8]) -> bool {
    b.starts_with(b"II+\0") || b.starts_with(b"MM\0+")
}

/// Whether a JPEG reaches its end-of-image marker, walked to from its first
/// segment: whatever follows it (a phone's trailer, a motion photo's video)
/// is no part of the image, and a file cut short never reaches it.
fn jpeg_is_whole(b: &[u8]) -> bool {
    let mut i = 2usize;
    // Bounded: a hostile file must not walk forever.
    for _ in 0..100_000 {
        // The next marker: 0xFF, any fill bytes, then its code.
        while b.get(i) == Some(&0xFF) && b.get(i + 1) == Some(&0xFF) {
            i += 1;
        }
        if b.get(i) != Some(&0xFF) {
            return false;
        }
        let Some(&code) = b.get(i + 1) else {
            return false;
        };
        i += 2;
        match code {
            0xD9 => return true,
            0x01 | 0xD0..=0xD7 => continue,
            _ => {}
        }
        let Some(len) = b
            .get(i..i + 2)
            .map(|l| usize::from(u16::from_be_bytes([l[0], l[1]])))
        else {
            return false;
        };
        i += len.max(2);
        if code == 0xDA {
            // Entropy-coded data runs to the next marker that is neither a
            // stuffed 0xFF00 nor a restart.
            loop {
                match (b.get(i), b.get(i + 1)) {
                    (Some(0xFF), Some(0x00 | 0xD0..=0xD7)) => i += 2,
                    (Some(0xFF), Some(_)) => break,
                    (Some(_), _) => i += 1,
                    (None, _) => return false,
                }
            }
        }
    }
    false
}

/// Whether a PNG stores one bit of grey per pixel (its IHDR's bit depth 1,
/// colour type 0).
fn png_is_one_bit(b: &[u8]) -> bool {
    b.starts_with(b"\x89PNG\r\n\x1a\n")
        && b.get(12..16) == Some(b"IHDR")
        && b.get(24..26) == Some(&[1, 0])
}

/// How many image directories (pages) a TIFF chains, classic or BigTIFF; 0
/// for a file that is not one. A chain that loops, or runs past the file or
/// past [`MAX_TIFF_PAGES`], is counted as far as it goes.
fn tiff_pages(b: &[u8]) -> usize {
    let le = b.starts_with(b"II");
    let at = |o: u64, n: usize| -> Option<u64> {
        let o = usize::try_from(o).ok()?;
        let x = b.get(o..o.checked_add(n)?)?;
        let mut v = 0u64;
        for k in 0..n {
            let byte = if le { x[n - 1 - k] } else { x[k] };
            v = (v << 8) | u64::from(byte);
        }
        Some(v)
    };
    // A BigTIFF (version 43) has 8-byte offsets and counts and 20-byte
    // entries; a classic TIFF (42), 4-byte offsets, 2-byte counts and 12-byte
    // entries.
    let (first, count, entry, offset) = match at(2, 2) {
        Some(43) => (at(8, 8), 8, 20, 8),
        Some(42) => (at(4, 4), 2, 12, 4),
        _ => return 0,
    };
    let mut seen = std::collections::HashSet::new();
    let mut next = first;
    while let Some(ifd) = next.filter(|&o| o != 0) {
        if seen.len() >= MAX_TIFF_PAGES || !seen.insert(ifd) {
            break;
        }
        next = at(ifd, count).and_then(|n| {
            let end = ifd
                .checked_add(count as u64)?
                .checked_add(n.checked_mul(entry)?)?;
            at(end, offset)
        });
    }
    seen.len()
}

/// The most pages of a TIFF counted: each one after the first is listed
/// unread in the output, and a few kilobytes can chain a hundred thousand.
const MAX_TIFF_PAGES: usize = 10_000;

/// Whether a decoded image looks like text as the PDF router judges a raster
/// region: its pixels to the probe ([`fluree_doc_vision::texty`]) — a
/// capture, a scanned table or a labelled chart does; a photograph, a
/// gradient or a logo does not — and enough of them on its short side for a
/// line of text ([`fluree_doc_pdf::route::MIN_TEXT_LINE_PX`]), under which
/// an engine misreads what it reads. Transparent pixels are laid over
/// white, as a page draws them.
pub fn texty(image: &Decoded) -> bool {
    let p = &image.pixels;
    p.width.min(p.height) >= fluree_doc_pdf::route::MIN_TEXT_LINE_PX
        && fluree_doc_vision::texty::pixels_look_like_text(p)
}

/// [`texty`] of an image's bytes: `None` where they do not decode.
pub fn looks_like_text(bytes: &[u8]) -> Option<bool> {
    decode(bytes).ok().map(|image| texty(&image))
}

/// What [`read`] gives: the document, its page carrying the engine's words
/// as glyphs, ready for analysis.
pub struct ImageRead {
    pub doc: Document,
    /// The reading as [`ocr::inject`] reports it.
    pub injected: Vec<Injected>,
    /// Pages of the file not read (a multi-page TIFF's after the first).
    pub more_pages: bool,
}

/// The one-page document a decoded image stands for, with no words yet.
pub fn document(image: &Decoded) -> Document {
    fluree_doc_pdf::image::page_of(image.pixels.width, image.pixels.height, image.bitonal)
}

/// Decode an image and read it with an engine, as a scanned page is read:
/// neither the pixel probe nor the noise rule judges it, an image given on
/// its own being given to be read.
pub fn read(bytes: &[u8], engine: &dyn OcrEngine) -> Result<ImageRead, String> {
    let image = decode(bytes)?;
    let mut doc = document(&image);
    let reading = engine.read(&image.pixels)?;
    let injected = ocr::inject(&mut doc, &[(0, Crop::Page, reading)]);
    Ok(ImageRead {
        doc,
        injected,
        more_pages: image.more_pages,
    })
}

/// What [`elements`] gives: an image's elements and its page.
#[derive(Debug, Clone)]
pub struct ImageElements {
    /// Marked `provenance: "ocr"`, boxed in the image's pixels.
    pub elements: Vec<fluree_doc_model::Element>,
    /// The page the boxes are on: the image upright, in pixels.
    pub size: fluree_doc_model::PageSize,
    /// The pixel probe does not take the image for text ([`texty`]): a
    /// photograph, a chart, a logo. Nothing read it, `elements` is empty,
    /// and the image is not read.
    pub not_texty: bool,
    /// The reading was noise ([`ocr::is_noise`]): a photograph or a chart
    /// read for text it does not hold. Its words are set aside, so
    /// `elements` is empty, and the image is not read.
    pub noise: bool,
    /// Pages of the file not read (a multi-page TIFF's after the first).
    pub more_pages: bool,
}

/// Decode an image, read it and analyse it: an image the pixel probe does
/// not take for text ([`texty`]) is not read, and comes back
/// [`ImageElements::not_texty`]. What an engine reads in one it takes for
/// text is still held to the noise rule ([`without_noise`]).
pub fn elements(bytes: &[u8], engine: &dyn OcrEngine) -> Result<ImageElements, String> {
    let image = decode(bytes)?;
    if !texty(&image) {
        return Ok(not_texty(&image));
    }
    read_texty(image, engine)
}

/// As [`elements`], of an image a caller has decoded and found [`texty`]
/// itself: it is neither decoded nor probed again.
pub fn read_texty(image: Decoded, engine: &dyn OcrEngine) -> Result<ImageElements, String> {
    let reading = engine.read(&image.pixels)?;
    Ok(analysed(document(&image), image.more_pages, reading))
}

/// What [`elements`] gives for an image the pixel probe does not take for
/// text: nothing read in it.
fn not_texty(image: &Decoded) -> ImageElements {
    ImageElements {
        elements: Vec::new(),
        size: fluree_doc_model::PageSize {
            index: 0,
            width: f64::from(image.pixels.width),
            height: f64::from(image.pixels.height),
            folio: None,
        },
        not_texty: true,
        noise: false,
        more_pages: image.more_pages,
    }
}

/// As [`elements`], from the words an engine already read in the image
/// (decoded upright, as [`decode`] gives it). Only the image's header is
/// read, for the page the words are on: the pixel probe is not run, the
/// caller has chosen to read it.
pub fn elements_from_words(
    bytes: &[u8],
    reading: fluree_doc_vision::WordReading,
) -> Result<ImageElements, String> {
    // Only the header: the words are already read.
    let h = header(bytes)?;
    let doc = fluree_doc_pdf::image::page_of(h.width, h.height, h.bitonal);
    Ok(analysed(doc, h.pages > 1, reading))
}

/// `reading`, an engine's reading of the one page of `doc`, a picture, with
/// its words set aside where they are noise ([`ocr::is_noise`]), and whether
/// they were. A picture need not hold any text, and the pixel probe can
/// take a photograph or a gradient for text: the words of a reading of it
/// that is noise are set aside, as those of an OCR reading of a PDF
/// region are, and the picture is left unread.
pub fn without_noise(
    doc: &Document,
    reading: fluree_doc_vision::WordReading,
) -> (fluree_doc_vision::WordReading, bool) {
    if ocr::is_noise(&doc.pages[0], &reading) {
        let words = Vec::new();
        (fluree_doc_vision::WordReading { words, ..reading }, true)
    } else {
        (reading, false)
    }
}

fn analysed(
    mut doc: Document,
    more_pages: bool,
    reading: fluree_doc_vision::WordReading,
) -> ImageElements {
    let (reading, noise) = without_noise(&doc, reading);
    let injected = ocr::inject(&mut doc, &[(0, Crop::Page, reading)]);
    let page = &doc.pages[0];
    let size = fluree_doc_model::PageSize {
        index: 0,
        width: page.width,
        height: page.height,
        folio: None,
    };
    let mut analysis = fluree_doc_pdf::document::analyze(&mut doc, &[]);
    ocr::mark_provenance(&mut analysis.elements, &injected);
    ImageElements {
        elements: analysis.elements,
        size,
        not_texty: false,
        noise,
        more_pages,
    }
}

/// What an engine made of the images a document places in its flow.
#[derive(Debug, Clone, Default)]
pub struct PlacedRead {
    /// The elements read in each image, once after each figure drawing it,
    /// for [`place_after_figures`].
    pub read: Vec<(String, Vec<fluree_doc_model::Element>)>,
    /// The images not read, as `doc:unreadImages` lists them, once for each
    /// figure drawing them, with why: `Missing` (not in the file),
    /// `UnsupportedFormat` (not a PNG, JPEG or TIFF: a drawing kept as EMF or
    /// WMF, a GIF), `PastAllowance` (past what one document may have read,
    /// [`fluree_doc_model::Allowance::DOCUMENT`]), `Failed` (its bytes or its
    /// reading failed) or `NotTexty` (the pixel probe does not take it for
    /// text, or its reading was noise: [`ImageElements::not_texty`],
    /// [`ImageElements::noise`]). Never `NotRead`: each image held is read.
    pub unread: Vec<fluree_doc_model::UnreadImage>,
    /// What went wrong with each image `UnsupportedFormat` or `Failed`, by
    /// the name it goes by (`m1`).
    pub errors: Vec<(String, String)>,
    /// Images of more than one page (a multi-page TIFF), of which only the
    /// first was read.
    pub first_page_only: Vec<fluree_doc_model::Picture>,
}

/// Read the images a document places in its flow with `engine`
/// ([`fluree_doc_model::read_pictures`]): each image once however many
/// figures draw it, one at a time, within what one document may have read.
/// `bytes` reads one by the part the file holds it under; it is decoded,
/// read and dropped before the next, so a document of many large pictures
/// is never held decoded at once.
pub fn read_placed(
    media: Vec<fluree_doc_model::Media>,
    bytes: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
    engine: &dyn OcrEngine,
) -> PlacedRead {
    use fluree_doc_model::{Picture, PictureReader, Unread};
    struct Engine<'e> {
        engine: &'e dyn OcrEngine,
        /// The picture being read, by tag.
        current: String,
        /// The pictures whose bytes are no image OCR decodes, by tag.
        unsupported: std::collections::HashSet<String>,
    }
    impl PictureReader for Engine<'_> {
        type Reading = ImageElements;
        fn ready(&mut self, picture: &Picture) -> bool {
            self.current = picture.tag();
            true
        }
        fn pixels(&mut self, image: &[u8]) -> Result<u64, String> {
            // A part emptied or cut short failed; it is no drawing.
            if image.len() < fluree_doc_pdf::image::Format::SNIFFED {
                return Err(format!(
                    "its part holds {} byte(s), too few for an image",
                    image.len()
                ));
            }
            // A drawing kept as EMF or WMF, or a GIF, is no image OCR
            // decodes.
            header(image)
                .map(|h| u64::from(h.width) * u64::from(h.height))
                .inspect_err(|_| {
                    if fluree_doc_pdf::image::Format::sniff(image).is_none() {
                        self.unsupported.insert(self.current.clone());
                    }
                })
        }
        fn read(&mut self, _: &Picture, image: &[u8]) -> Result<ImageElements, String> {
            elements(image, self.engine)
        }
    }
    // Each picture's place among the document's, by the name it goes by.
    let mut order: std::collections::HashMap<String, usize> = Default::default();
    for (i, m) in media.iter().enumerate() {
        order.entry(m.tag()).or_insert(i);
    }
    let mut reader = Engine {
        engine,
        current: String::new(),
        unsupported: Default::default(),
    };
    let reads = fluree_doc_model::read_pictures(
        media,
        bytes,
        &mut reader,
        fluree_doc_model::Allowance::DOCUMENT,
    );
    let mut out = PlacedRead::default();
    let mut unread: Vec<(fluree_doc_model::Picture, &str)> = Vec::new();
    for (picture, r) in reads.read {
        // An image the pixel probe does not take for text, or whose reading
        // was noise, is read by nothing.
        if r.not_texty || r.noise {
            unread.push((picture, "NotTexty"));
            continue;
        }
        if r.more_pages {
            out.first_page_only.push(picture.clone());
        }
        out.read.extend(picture.after_each_figure(r.elements));
    }
    for (picture, why) in reads.unread {
        let reason = match &why {
            Unread::NotInPackage => "Missing",
            Unread::PastAllowance => "PastAllowance",
            Unread::NotAsked => "NotRead",
            Unread::Failed(e) => {
                out.errors.push((picture.tag(), e.clone()));
                if reader.unsupported.contains(&picture.tag()) {
                    "UnsupportedFormat"
                } else {
                    "Failed"
                }
            }
        };
        unread.push((picture, reason));
    }
    // In the order the document first draws them.
    unread.sort_by_key(|(picture, _)| order.get(&picture.tag()).copied());
    for (picture, reason) in unread {
        for figure in picture.figures() {
            out.unread.push(fluree_doc_model::UnreadImage {
                figure: figure.to_string(),
                image: picture.tag(),
                part: picture.source.name().to_string(),
                reason: reason.into(),
            });
        }
    }
    out
}

/// Place the text read from a document's images in its flow, each image's
/// elements right after the figure it is drawn as (`Element::figure`).
///
/// They take the figure's page. Their boxes are the image's own pixels,
/// which mean nothing on a page without geometry, so they are dropped. A
/// heading read in a screenshot is a heading of the screen, not of the
/// document: it is read as a paragraph, so the document's outline stays its
/// own. Tables stay tables. Ids are renumbered in order, and an element's
/// reference to another by id (`resumes`, `continues`) follows it.
pub fn place_after_figures(
    elements: &mut Vec<fluree_doc_model::Element>,
    readings: Vec<(String, Vec<fluree_doc_model::Element>)>,
) {
    if readings.is_empty() {
        return;
    }
    // A figure may hold several images (a group of pictures): their texts
    // follow it in order.
    let mut by_figure: std::collections::HashMap<String, Vec<fluree_doc_model::Element>> =
        std::collections::HashMap::new();
    for (figure, read) in readings {
        by_figure.entry(figure).or_default().extend(read);
    }
    let mut out = Vec::with_capacity(elements.len());
    for e in elements.drain(..) {
        let read = e.figure.as_ref().and_then(|f| by_figure.remove(f));
        let page = e.page;
        out.push(e);
        for mut r in read.into_iter().flatten() {
            // The reading's own ids (`elem-00001`… from its analysis) name
            // nothing of this document: kept, they would take the place of
            // the document's elements of the same id in the renaming below.
            r.id.clear();
            r.page = page;
            r.bbox = None;
            if r.kind != "doco:Table" {
                r.kind = "doco:Paragraph".into();
                r.level = None;
            }
            out.push(r);
        }
    }
    let mut renamed = std::collections::HashMap::new();
    for (i, e) in out.iter_mut().enumerate() {
        let id = format!("elem-{:05}", i + 1);
        let old = std::mem::replace(&mut e.id, id.clone());
        if !old.is_empty() {
            renamed.insert(old, id);
        }
    }
    for e in &mut out {
        for r in [&mut e.resumes, &mut e.continues].into_iter().flatten() {
            if let Some(id) = renamed.get(r.as_str()) {
                r.clone_from(id);
            }
        }
    }
    *elements = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluree_doc_vision::{Coords, OcrWord, WordReading};

    /// A PNG of `w` × `h` grey pixels.
    /// A `w × h` PNG the pixel probe takes for text: lines of strokes a
    /// letter wide on a light ground, as a capture holds.
    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            let ink = (12..24).contains(&(y % 40)) && x % 10 < 6 && x < w * 3 / 4;
            image::Rgb(if ink { [40, 40, 40] } else { [200, 200, 200] })
        });
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    /// A `w × h` PNG of one grey: no text, to the probe or to anyone.
    fn flat_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 200, 200]));
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    /// An engine that must not be asked to read anything.
    struct Unasked;

    impl OcrEngine for Unasked {
        fn name(&self) -> &str {
            "unasked"
        }
        fn read(&self, _: &Pixels) -> Result<WordReading, String> {
            panic!("an image the probe does not take for text was read")
        }
    }

    #[test]
    fn a_picture_too_thin_for_a_line_of_text_is_not_read() {
        // The floor a PDF's region is held to: from 10 pixels on its short
        // side, whichever side that is, the line is read; under it, it is
        // not, as an engine misreads it.
        assert_eq!(looks_like_text(&line_png(700, 10)), Some(true));
        assert_eq!(looks_like_text(&line_png(700, 9)), Some(false));
        let read = elements(&line_png(700, 9), &Unasked).unwrap();
        assert!(read.not_texty);
        let read = elements(&line_png(700, 10), &TwoLines).unwrap();
        assert!(!read.not_texty);
    }

    #[test]
    fn a_picture_the_probe_does_not_take_for_text_is_not_read() {
        assert_eq!(looks_like_text(&flat_png(600, 400)), Some(false));
        assert_eq!(looks_like_text(&png(600, 400)), Some(true));
        assert_eq!(looks_like_text(b"not an image"), None);
        let read = elements(&flat_png(600, 400), &Unasked).unwrap();
        assert!(read.not_texty && !read.noise && read.elements.is_empty());
        assert_eq!((read.size.width, read.size.height), (600.0, 400.0));
        let media = fluree_doc_model::Media {
            figure: "figure-0-1".into(),
            source: fluree_doc_model::MediaSource::Part("word/media/image1.png".into()),
            index: 0,
        };
        let r = read_placed(vec![media], &mut |_| Ok(flat_png(600, 400)), &Unasked);
        assert!(r.read.is_empty(), "{:?}", r.read);
        let reasons: Vec<&str> = r.unread.iter().map(|u| u.reason.as_str()).collect();
        assert_eq!(reasons, ["NotTexty"]);
        assert!(r.errors.is_empty());
    }

    /// An engine that reads two lines across whatever it is shown.
    struct TwoLines;

    impl OcrEngine for TwoLines {
        fn name(&self) -> &str {
            "two-lines"
        }
        fn read(&self, image: &Pixels) -> Result<WordReading, String> {
            let (w, h) = (image.width as f64, image.height as f64);
            let line = |text: &str, y: f64| OcrWord {
                text: text.into(),
                bbox: [w * 0.1, h * y, w * 0.8, h * y + 24.0],
                angle: None,
                conf: Some(0.95),
            };
            Ok(WordReading {
                coords: Coords::Px,
                width: Some(w),
                height: Some(h),
                engine: Some("two-lines".into()),
                words: vec![
                    line("Account 4417-0093-21", 0.2),
                    line("Date of birth 15-OCT-1997", 0.4),
                ],
                rules: Vec::new(),
            })
        }
    }

    #[test]
    fn a_part_too_short_for_any_format_failed() {
        // Emptied, or cut short within PNG's signature: failed, as the CLI
        // lists it; a part long enough to tell holds another format.
        let mut parts = [&b""[..], b"\x89PNG", b"\x01\x00\x00\x00 EMF record"].into_iter();
        let media = (1..=3)
            .map(|n| fluree_doc_model::Media {
                figure: format!("figure-0-{n}"),
                source: fluree_doc_model::MediaSource::Part(format!("word/media/image{n}")),
                index: 0,
            })
            .collect();
        let r = read_placed(
            media,
            &mut |_| Ok(parts.next().unwrap().to_vec()),
            &TwoLines,
        );
        let reasons: Vec<&str> = r.unread.iter().map(|u| u.reason.as_str()).collect();
        assert_eq!(reasons, ["Failed", "Failed", "UnsupportedFormat"]);
        assert_eq!(
            r.errors[0],
            (
                "m1".to_string(),
                "its part holds 0 byte(s), too few for an image".to_string()
            )
        );
    }

    /// A `w × h` PNG of one line of strokes, a pixel of paper above and
    /// below: text, to the probe, however thin.
    fn line_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::GrayImage::from_fn(w, h, |x, y| {
            let ink = y > 0 && y + 1 < h && x % 6 < 3;
            image::Luma([if ink { 30 } else { 230 }])
        });
        let mut out = Vec::new();
        DynamicImage::ImageLuma8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn an_image_reads_as_a_page_of_text() {
        let ImageElements {
            elements,
            size,
            not_texty,
            noise,
            more_pages,
        } = elements(&png(600, 400), &TwoLines).unwrap();
        assert_eq!((size.width, size.height), (600.0, 400.0));
        assert!(!not_texty);
        assert!(!noise);
        assert!(!more_pages);
        let text: String = elements
            .iter()
            .map(|e| e.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("4417-0093-21") && text.contains("15-OCT-1997"),
            "{text}"
        );
        assert!(
            elements.iter().all(|e| e.provenance == "ocr"),
            "{elements:?}"
        );
    }

    /// A reading of a 600 × 400 picture: `words`, in its pixels, each
    /// scored `conf`.
    fn reading(words: &[(&str, [f64; 4])], conf: Option<f64>) -> WordReading {
        WordReading {
            coords: Coords::Px,
            width: Some(600.0),
            height: Some(400.0),
            engine: None,
            words: words
                .iter()
                .map(|&(text, bbox)| OcrWord {
                    text: text.into(),
                    bbox,
                    angle: None,
                    conf,
                })
                .collect(),
            rules: Vec::new(),
        }
    }

    #[test]
    fn a_pictures_reading_that_is_noise_reads_nothing() {
        let image = png(600, 400);
        let strokes = [
            ("Account 4417", [40.0, 60.0, 240.0, 84.0]),
            ("Balance 9,876.54", [40.0, 100.0, 300.0, 124.0]),
        ];
        let fragments = [
            ("I7", [40.0, 60.0, 60.0, 84.0]),
            ("1", [300.0, 100.0, 310.0, 124.0]),
            ("J", [500.0, 300.0, 510.0, 324.0]),
        ];
        let all_of_it = [("7", [0.0, 0.0, 600.0, 400.0])];
        // Unsure in the mean, fragments not sure enough to be a chart's, or
        // boxed over the whole picture however sure: a photograph's or a
        // gradient's noise.
        for r in [
            reading(&strokes, Some(0.3)),
            reading(&fragments, Some(0.6)),
            reading(&all_of_it, None),
            reading(&all_of_it, Some(0.66)),
        ] {
            let read = elements_from_words(&image, r).unwrap();
            assert!(read.noise);
            assert!(read.elements.is_empty(), "{:?}", read.elements);
        }
        // Read with confidence it is text: words, fragments as sure as a
        // chart's axis, a large word over most of it too.
        let large = [("SALE", [60.0, 60.0, 540.0, 340.0])];
        for r in [
            reading(&strokes, Some(0.6)),
            reading(&fragments, Some(0.9)),
            reading(&large, Some(0.99)),
        ] {
            let read = elements_from_words(&image, r).unwrap();
            assert!(!read.noise);
            assert!(!read.elements.is_empty());
        }
    }

    /// An engine that reads one unsure stroke in whatever it is shown.
    struct Unsure;

    impl OcrEngine for Unsure {
        fn name(&self) -> &str {
            "unsure"
        }
        fn read(&self, _: &Pixels) -> Result<WordReading, String> {
            Ok(reading(&[("M", [40.0, 60.0, 200.0, 220.0])], Some(0.2)))
        }
    }

    #[test]
    fn an_image_read_as_a_file_is_not_judged() {
        // Read as a scan is: an unsure stroke a placed picture would set
        // aside is taken, and a flat image the probe takes for no text is
        // read all the same.
        let read = read(&png(600, 400), &Unsure).unwrap();
        assert_eq!(read.injected[0].taken, 1);
        assert!(!read.doc.pages[0].glyphs.is_empty());
        let read = super::read(&flat_png(600, 400), &Unsure).unwrap();
        assert_eq!(read.injected[0].taken, 1);
    }

    #[test]
    fn a_placed_picture_read_as_noise_is_listed_unread() {
        let media = fluree_doc_model::Media {
            figure: "figure-0-1".into(),
            source: fluree_doc_model::MediaSource::Part("word/media/image1.png".into()),
            index: 0,
        };
        let r = read_placed(vec![media], &mut |_| Ok(png(600, 400)), &Unsure);
        assert!(r.read.is_empty(), "{:?}", r.read);
        let reasons: Vec<&str> = r.unread.iter().map(|u| u.reason.as_str()).collect();
        assert_eq!(reasons, ["NotTexty"]);
        assert!(r.errors.is_empty());
    }

    #[test]
    fn an_images_text_follows_its_figure() {
        use fluree_doc_model::Element;
        let mut figure = Element::new("doco:Figure", "A screenshot", "docx", "docx");
        figure.figure = Some("figure-0-1".into());
        // The figure's page, which the image's text takes for its own.
        figure.page = 3;
        let mut els = vec![
            Element::new("doco:Paragraph", "See below.", "docx", "docx"),
            figure,
            Element::new("doco:Paragraph", "After.", "docx", "docx"),
        ];
        let read = elements(&png(600, 400), &TwoLines).unwrap().elements;
        assert!(read.iter().all(|e| e.bbox.is_some()));
        place_after_figures(&mut els, vec![("figure-0-1".into(), read)]);
        let texts: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts[0..2], ["See below.", "A screenshot"]);
        assert_eq!(texts.last(), Some(&"After."));
        assert!(
            texts[2..texts.len() - 1].join(" ").contains("4417-0093-21"),
            "{texts:?}"
        );
        assert!(els[2..els.len() - 1]
            .iter()
            .all(|e| e.bbox.is_none() && e.provenance == "ocr" && e.page == 3));
        let ids: std::collections::HashSet<&str> = els.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids.len(), els.len());
    }

    #[test]
    fn an_element_naming_another_by_id_still_names_it_once_text_is_placed() {
        // A message's opener, a picture, a quote, then the message going on
        // after it (`resumes`) and a table continued (`continues`): the text
        // read in the picture moves every id after it. That text comes with
        // ids of its own analysis, the same `elem-0000N` as the document's,
        // which must not be taken for the document's.
        use fluree_doc_model::Element;
        let el = |kind: &str, text: &str, id: &str, by: &'static str| {
            let mut e = Element::new(kind, text, "eml", by);
            e.id = id.into();
            e
        };
        let opener = el("doco:Paragraph", "From: A", "elem-00001", "eml");
        let mut figure = el("doco:Figure", "", "elem-00002", "eml");
        figure.figure = Some("figure-0-1".into());
        let quoted = el("doco:Table", "", "elem-00003", "eml");
        let mut back = el("doco:Paragraph", "A's signature", "elem-00004", "eml");
        back.resumes = Some("elem-00001".into());
        let mut part = el("doco:Table", "", "elem-00005", "eml");
        part.continues = Some("elem-00003".into());
        let mut els = vec![opener, figure, quoted, back, part];
        let read = vec![
            el("doco:Paragraph", "Read 1", "elem-00001", "ocr"),
            el("doco:Paragraph", "Read 2", "elem-00002", "ocr"),
            el("doco:Paragraph", "Read 3", "elem-00003", "ocr"),
        ];
        place_after_figures(&mut els, vec![("figure-0-1".into(), read)]);
        let texts: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "From: A",
                "",
                "Read 1",
                "Read 2",
                "Read 3",
                "",
                "A's signature",
                ""
            ]
        );
        assert_eq!(els[6].resumes.as_deref(), Some("elem-00001"));
        assert_eq!(els[7].continues.as_deref(), Some("elem-00006"));
    }

    #[test]
    fn a_heading_in_a_picture_is_a_paragraph_of_the_document() {
        // A screen's title is no section of the document; a table stays one.
        // Two images of one figure (a group) follow it in order.
        use fluree_doc_model::Element;
        let mut figure = Element::new("doco:Figure", "", "docx", "docx");
        figure.figure = Some("figure-0-1".into());
        let mut els = vec![figure];
        let mut heading = Element::new("doco:SectionTitle", "Account summary", "rust", "ocr");
        heading.level = Some(1);
        let table = Element::new("doco:Table", "a | b", "rust", "ocr");
        let after = Element::new("doco:Paragraph", "Second screen", "rust", "ocr");
        place_after_figures(
            &mut els,
            vec![
                ("figure-0-1".into(), vec![heading, table]),
                ("figure-0-1".into(), vec![after]),
            ],
        );
        let got: Vec<(&str, &str, Option<usize>)> = els
            .iter()
            .map(|e| (e.kind.as_str(), e.text.as_str(), e.level))
            .collect();
        assert_eq!(
            got,
            [
                ("doco:Figure", "", None),
                ("doco:Paragraph", "Account summary", None),
                ("doco:Table", "a | b", None),
                ("doco:Paragraph", "Second screen", None),
            ]
        );
    }

    #[test]
    fn a_phone_photo_is_read_upright() {
        // A 300 × 100 JPEG whose EXIF says "rotate 90° clockwise": the page,
        // and the pixels the engine sees, are 100 × 300.
        let img = image::RgbImage::from_pixel(300, 100, image::Rgb([90, 90, 90]));
        let mut jpeg = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();
        let rotated = with_exif_orientation(&jpeg, 6);
        let d = decode(&rotated).unwrap();
        assert_eq!((d.pixels.width, d.pixels.height), (100, 300));
        let doc = document(&d);
        assert_eq!((doc.pages[0].width, doc.pages[0].height), (100.0, 300.0));
    }

    #[test]
    fn an_image_is_decoded_in_the_layout_it_holds() {
        let encode = |img: DynamicImage, format| {
            let mut out = Vec::new();
            img.write_to(&mut Cursor::new(&mut out), format).unwrap();
            out
        };
        let grey = image::GrayImage::from_pixel(30, 10, image::Luma([200]));
        let d = decode(&encode(grey.clone().into(), image::ImageFormat::Png)).unwrap();
        assert_eq!((d.pixels.layout, d.pixels.data.len()), (Layout::Luma8, 300));
        let rgb = image::RgbImage::from_pixel(30, 10, image::Rgb([1, 2, 3]));
        let d = decode(&encode(rgb.into(), image::ImageFormat::Png)).unwrap();
        assert_eq!(d.pixels.layout, Layout::Rgb8);
        // Sixteen bits of grey are one byte of grey.
        let deep =
            image::ImageBuffer::<image::Luma<u16>, _>::from_pixel(30, 10, image::Luma([0x8000]));
        let d = decode(&encode(
            DynamicImage::ImageLuma16(deep),
            image::ImageFormat::Png,
        ))
        .unwrap();
        assert_eq!((d.pixels.layout, d.pixels.data[0]), (Layout::Luma8, 0x80));
        // Grey with transparency keeps it, and reads as paper.
        let clear = image::GrayAlphaImage::from_pixel(30, 10, image::LumaA([0, 0]));
        let d = decode(&encode(clear.into(), image::ImageFormat::Png)).unwrap();
        assert_eq!(d.pixels.layout, Layout::Rgba8);
        assert_eq!(d.pixels.to_rgb()[..3], [255, 255, 255]);
        // A grey photo turned upright stays grey.
        let mut jpeg = Vec::new();
        DynamicImage::ImageLuma8(image::GrayImage::from_pixel(300, 100, image::Luma([90])))
            .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();
        let d = decode(&with_exif_orientation(&jpeg, 6)).unwrap();
        assert_eq!(
            (d.pixels.width, d.pixels.height, d.pixels.layout),
            (100, 300, Layout::Luma8)
        );
    }

    #[test]
    fn a_bitonal_image_is_noted() {
        let img = image::GrayImage::from_fn(64, 8, |x, _| {
            image::Luma([if x % 2 == 0 { 0 } else { 255 }])
        });
        let mut tiff = Vec::new();
        DynamicImage::ImageLuma8(img)
            .write_to(&mut Cursor::new(&mut tiff), image::ImageFormat::Tiff)
            .unwrap();
        // Eight bits stored: not one-bit, and one page.
        let d = decode(&tiff).unwrap();
        assert!(!d.bitonal && !d.more_pages);
    }

    #[test]
    fn a_one_bit_png_is_noted() {
        // A PNG of one bit of grey per pixel, as a fax saved to PNG is: the
        // decoder expands it to eight, the header says one.
        let mut png1 = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png1, 16, 2);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::One);
            enc.write_header()
                .unwrap()
                .write_image_data(&[0x0f, 0xf0, 0xf0, 0x0f])
                .unwrap();
        }
        assert!(decode(&png1).unwrap().bitonal);
        assert!(!decode(&png(16, 2)).unwrap().bitonal);
    }

    #[test]
    fn an_image_too_large_to_decode_is_refused_before_it_is() {
        // A PNG whose header states 30,000 × 30,000 pixels: 3.6 GB as RGBA,
        // from a few hundred bytes. Refused on its header.
        let mut bomb = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut bomb, 30_000, 30_000);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::One);
            let mut writer = enc.write_header().unwrap();
            {
                let mut w = writer.stream_writer().unwrap();
                let row = vec![0u8; 30_000 / 8];
                for _ in 0..30_000 {
                    std::io::Write::write_all(&mut w, &row).unwrap();
                }
                w.finish().unwrap();
            }
            drop(writer);
        }
        let e = decode(&bomb).unwrap_err();
        assert!(e.contains("30000 × 30000"), "{e}");
        // Its size can still be read, for a model sent the file as it is.
        assert_eq!(upright_size(&bomb).unwrap(), (30_000, 30_000));
    }

    #[test]
    fn the_upright_size_is_read_without_decoding() {
        let img = image::RgbImage::from_pixel(300, 100, image::Rgb([90, 90, 90]));
        let mut jpeg = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();
        assert_eq!(upright_size(&jpeg).unwrap(), (300, 100));
        assert_eq!(
            upright_size(&with_exif_orientation(&jpeg, 6)).unwrap(),
            (100, 300)
        );
        assert_eq!(
            upright_size(&with_exif_orientation(&jpeg, 3)).unwrap(),
            (300, 100)
        );
        // Turned a quarter and mirrored: still turned.
        for o in [5, 7] {
            assert_eq!(
                upright_size(&with_exif_orientation(&jpeg, o)).unwrap(),
                (100, 300),
                "orientation {o}"
            );
        }
    }

    #[test]
    fn a_bigtiff_chain_is_counted() {
        // A BigTIFF: version 43, 8-byte offsets, two IFDs.
        let mut t = b"II".to_vec();
        t.extend(43u16.to_le_bytes());
        t.extend(8u16.to_le_bytes()); // offset size
        t.extend(0u16.to_le_bytes());
        t.extend(16u64.to_le_bytes()); // first IFD at 16
        t.extend(0u64.to_le_bytes()); // no entries
        t.extend(32u64.to_le_bytes()); // next IFD at 32
        t.extend(0u64.to_le_bytes());
        t.extend(0u64.to_le_bytes());
        assert_eq!(tiff_pages(&t), 2);
        t[24..32].copy_from_slice(&0u64.to_le_bytes());
        assert_eq!(tiff_pages(&t), 1);
        // A directory that points back at itself ends the count.
        t[24..32].copy_from_slice(&16u64.to_le_bytes());
        assert_eq!(tiff_pages(&t), 1);
    }

    /// A BigTIFF of `pages` grey pages, written by the TIFF encoder.
    fn bigtiff(pages: usize) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut enc = tiff::encoder::TiffEncoder::new_big(&mut out).unwrap();
        for _ in 0..pages {
            enc.write_image::<tiff::encoder::colortype::Gray8>(40, 20, &[120u8; 800])
                .unwrap();
        }
        out.into_inner()
    }

    #[test]
    fn a_bigtiff_is_read() {
        let t = bigtiff(3);
        assert!(is_big_tiff(&t));
        let d = decode(&t).unwrap();
        assert_eq!((d.pixels.width, d.pixels.height), (40, 20));
        assert!(d.more_pages);
        assert_eq!(header(&t).unwrap().pages, 3);
        assert_eq!(
            fluree_doc_pdf::image::Format::sniff(&t),
            Some(fluree_doc_pdf::image::Format::Tiff)
        );
    }

    #[test]
    fn a_jpeg_cut_short_is_marked_truncated() {
        let img = image::RgbImage::from_fn(200, 200, |x, y| image::Rgb([(x ^ y) as u8, 90, 90]));
        let mut jpeg = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();
        assert!(!decode(&jpeg).unwrap().truncated);
        let mut padded = jpeg.clone();
        padded.extend([0u8; 16]);
        assert!(!decode(&padded).unwrap().truncated, "padding is not a cut");
        // The decoder fills in what is missing without a word.
        let half = &jpeg[..jpeg.len() * 2 / 3];
        assert!(decode(half).unwrap().truncated);
        // Data after the image (a phone's trailer, a motion photo's video)
        // is no truncation, even when it holds marker-like bytes.
        let mut trailer = jpeg.clone();
        trailer.extend(b"\xff\xd8 vendor trailer \xff\xda\x00\x02 more");
        assert!(!decode(&trailer).unwrap().truncated);
    }

    #[test]
    fn only_what_fits_is_decoded() {
        assert!(decodable(1000, 1000, 4_000_000).is_ok());
        assert!(decodable(20_000, 20_000, 4).is_err(), "too many pixels");
        let e = decodable(100_000, 1, 100_000).unwrap_err();
        assert!(e.contains("longer than"), "{e}");
        assert!(decodable(MAX_SIDE, 1, 4).is_ok());
        // Within the pixels, too many bytes: floating-point RGBA.
        let e = decodable(10_000, 10_000, 1_600_000_000).unwrap_err();
        assert!(e.contains("16 bytes each"), "{e}");
        // 8-bit RGBA at the cap is decoded; 16-bit RGBA is, up to 64 Mi
        // pixels.
        assert!(decodable(11_313, 11_313, 11_313 * 11_313 * 4).is_ok());
        let e = decodable(11_313, 11_313, 11_313 * 11_313 * 8).unwrap_err();
        assert!(e.contains("8 bytes each"), "{e}");
        assert!(decodable(8192, 8192, 8192 * 8192 * 8).is_ok());
    }

    #[test]
    fn only_png_jpeg_and_tiff_are_read() {
        // A bitmap and a GIF, whatever decoders the build links.
        let bmp = [b"BM".as_slice(), &[0u8; 60]].concat();
        let gif = [b"GIF89a".as_slice(), &[0u8; 30]].concat();
        for bytes in [bmp, gif] {
            let e = decode(&bytes).unwrap_err();
            assert!(e.contains("only PNG, JPEG and TIFF"), "{e}");
            assert!(upright_size(&bytes).is_err());
        }
    }

    #[test]
    fn a_long_tiff_chain_is_counted_up_to_its_cap() {
        // Empty directories chained one after the other, past the cap.
        let n = MAX_TIFF_PAGES + 50;
        let mut t = b"II*\0".to_vec();
        t.extend(8u32.to_le_bytes());
        for k in 0..n {
            let next = if k + 1 < n { 8 + 6 * (k as u32 + 1) } else { 0 };
            t.extend(0u16.to_le_bytes());
            t.extend(next.to_le_bytes());
        }
        assert_eq!(tiff_pages(&t), MAX_TIFF_PAGES);
    }

    #[test]
    fn a_tiff_chain_is_counted() {
        // Two IFDs, the first pointing to the second: built by hand.
        let mut t = b"II*\0".to_vec();
        t.extend(8u32.to_le_bytes()); // first IFD at 8
        t.extend(0u16.to_le_bytes()); // no entries
        t.extend(14u32.to_le_bytes()); // next IFD at 14
        t.extend(0u16.to_le_bytes());
        t.extend(0u32.to_le_bytes());
        assert_eq!(tiff_pages(&t), 2);
        t[10..14].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(tiff_pages(&t), 1);
        assert_eq!(tiff_pages(b"not a tiff"), 0);
    }

    /// A PNG of one grey pixel claiming `(width, height)`, and with an
    /// `iCCP` chunk holding `profile` zero bytes when given.
    fn png_claiming(width: u32, height: u32, profile: Option<usize>) -> Vec<u8> {
        fn crc(bytes: &[u8]) -> u32 {
            let mut c = 0xffff_ffffu32;
            for &b in bytes {
                c ^= b as u32;
                for _ in 0..8 {
                    c = if c & 1 == 1 {
                        0xedb8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
            out.extend((data.len() as u32).to_be_bytes());
            let body = [kind, data].concat();
            out.extend(&body);
            out.extend(crc(&body).to_be_bytes());
        }
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let ihdr = [width.to_be_bytes(), height.to_be_bytes()].concat();
        chunk(&mut out, b"IHDR", &[&ihdr[..], &[8, 0, 0, 0, 0]].concat());
        if let Some(n) = profile {
            chunk(
                &mut out,
                b"iCCP",
                &[b"p\0\0".as_slice(), &zlib_zeros(n)].concat(),
            );
        }
        // One row: filter byte 0, one grey pixel, as a stored zlib block.
        chunk(
            &mut out,
            b"IDAT",
            &[0x78, 0x01, 1, 2, 0, 0xfd, 0xff, 0, 128, 0, 0x81, 0, 0x81],
        );
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// A zlib stream of `n` zero bytes, `n - 1` a multiple of 258: one
    /// literal zero, then matches of 258 a distance of 1 back, in a block
    /// of the fixed codes, a few bytes per 64 KiB.
    fn zlib_zeros(n: usize) -> Vec<u8> {
        assert_eq!((n - 1) % 258, 0);
        let mut bits: Vec<bool> = Vec::new();
        let mut put = |code: u32, len: u32, msb_first: bool| {
            for i in 0..len {
                let bit = if msb_first { len - 1 - i } else { i };
                bits.push(code >> bit & 1 == 1);
            }
        };
        put(1, 1, false); // final block
        put(1, 2, false); // fixed codes
        put(0x30, 8, true); // literal 0
        for _ in 0..(n - 1) / 258 {
            put(0b1100_0101, 8, true); // length 258
            put(0, 5, true); // distance 1
        }
        put(0, 7, true); // end of block
        let mut out = vec![0x78, 0x01];
        for byte in bits.chunks(8) {
            out.push(byte.iter().enumerate().map(|(i, &b)| (b as u8) << i).sum());
        }
        let b = (n % 65521) as u32;
        out.extend(((b << 16) | 1).to_be_bytes());
        out
    }

    #[test]
    fn a_png_colour_profile_is_inflated_within_a_budget() {
        // The fixture is a real PNG: one pixel reads.
        let one = decode(&png_claiming(1, 1, None)).unwrap();
        assert_eq!((one.pixels.width, one.pixels.layout), (1, Layout::Luma8));
        // A profile of 33 MB from a file of a quarter of that, past the
        // chunk budget: dropped as the header is read, the image read on,
        // whether its size alone is asked for or its pixels.
        let bomb = png_claiming(1, 1, Some(258 * 130_000 + 1));
        assert!(bomb.len() < 250_000, "33 MB from a small file");
        let profile = |b: &[u8]| {
            let (mut d, _) = decoder(b, false).unwrap();
            d.icc_profile().unwrap().map(|p| p.len())
        };
        assert_eq!(profile(&bomb), None);
        assert_eq!(header(&bomb).unwrap().width, 1);
        assert!(decode(&bomb).is_ok());
        // A profile of a normal size is kept.
        assert_eq!(profile(&png_claiming(1, 1, Some(258 * 4 + 1))), Some(1033));
        // A header claiming more than may be decoded is refused before any
        // pixel is allocated; its size can still be read.
        let big = png_claiming(20_000, 20_000, None);
        assert!(decode(&big).unwrap_err().contains("more than"));
        assert_eq!(upright_size(&big).unwrap(), (20_000, 20_000));
        assert!(decode(&png_claiming(100_000, 1, None)).is_err());
    }

    /// `jpeg` with an APP1 EXIF segment carrying `orientation`, right after
    /// the SOI marker.
    fn with_exif_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        let mut tiff = b"MM\0*".to_vec();
        tiff.extend(8u32.to_be_bytes());
        tiff.extend(1u16.to_be_bytes()); // one entry
        tiff.extend(0x0112u16.to_be_bytes()); // Orientation
        tiff.extend(3u16.to_be_bytes()); // SHORT
        tiff.extend(1u32.to_be_bytes()); // count
        tiff.extend(orientation.to_be_bytes());
        tiff.extend([0, 0]);
        tiff.extend(0u32.to_be_bytes()); // no next IFD
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(tiff);
        let mut out = jpeg[..2].to_vec();
        out.extend([0xFF, 0xE1]);
        out.extend(((app1.len() + 2) as u16).to_be_bytes());
        out.extend(app1);
        out.extend(&jpeg[2..]);
        out
    }
}
