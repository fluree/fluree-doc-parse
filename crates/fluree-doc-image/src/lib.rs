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

/// The most pixels an image may hold to be decoded: 128 million, 512 MB as
/// RGBA. A file's header states its size, and a few kilobytes of PNG can
/// state a size that takes gigabytes to decode; such a file is refused
/// before anything is allocated.
pub const MAX_PIXELS: u64 = 128_000_000;

/// The most bytes the decoder may produce, in the image's own colour type
/// (16-bit RGBA at [`MAX_PIXELS`] is 1 GB).
const MAX_DECODED_BYTES: u64 = 1 << 30;

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
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_DECODED_BYTES);
    reader.limits(limits);
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
    let decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let (w, h) = decoder.dimensions();
    if capped {
        decodable(w, h, decoder.total_bytes())?;
    }
    Ok((decoder, format))
}

/// Whether an image of `w` × `h` pixels, `bytes` once decoded in its own
/// colour type, may be decoded: within [`MAX_PIXELS`] and
/// [`MAX_DECODED_BYTES`].
fn decodable(w: u32, h: u32, bytes: u64) -> Result<(), String> {
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

/// Decode an image and read it with an engine.
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
    /// Pages of the file not read (a multi-page TIFF's after the first).
    pub more_pages: bool,
}

/// Decode an image, read it and analyse it.
pub fn elements(bytes: &[u8], engine: &dyn OcrEngine) -> Result<ImageElements, String> {
    let mut read = read(bytes, engine)?;
    let page = &read.doc.pages[0];
    let size = fluree_doc_model::PageSize {
        index: 0,
        width: page.width,
        height: page.height,
        folio: None,
    };
    let mut analysis = fluree_doc_pdf::document::analyze(&mut read.doc, &[]);
    ocr::mark_provenance(&mut analysis.elements, &read.injected);
    Ok(ImageElements {
        elements: analysis.elements,
        size,
        more_pages: read.more_pages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluree_doc_vision::{Coords, OcrWord, WordReading};

    /// A PNG of `w` × `h` grey pixels.
    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 200, 200]));
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
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
            })
        }
    }

    #[test]
    fn an_image_reads_as_a_page_of_text() {
        let ImageElements {
            elements,
            size,
            more_pages,
        } = elements(&png(600, 400), &TwoLines).unwrap();
        assert_eq!((size.width, size.height), (600.0, 400.0));
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
        // Within the pixels, too many bytes: floating-point RGBA.
        let e = decodable(10_000, 10_000, 1_600_000_000).unwrap_err();
        assert!(e.contains("16 bytes each"), "{e}");
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
