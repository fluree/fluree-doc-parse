//! A bare image as a one-page document.
//!
//! An image has no text layer, so there is nothing for the geometric pipeline
//! to measure — no glyphs, no rules, no columns. What it has is a page's worth
//! of pixels, which is exactly the case the deep reader exists for. Treating
//! it as a single page that routes to a full reading puts it through the same
//! path as a scanned PDF page, with the same arbitration and the same output
//! shape, rather than making it a second kind of document.
//!
//! The consequence is worth stating plainly: **an image is only ever as good
//! as the configured reader.** Every other source this library takes has a
//! deterministic reading to fall back on; this one does not. With no provider
//! configured the honest answer is no content, and the caller is told so
//! rather than handed an empty document.
//!
//! Dimensions are read from the file header rather than by decoding. A page
//! size is four numbers, a decoder is a large dependency and a large attack
//! surface, and nothing here needs the pixels — the reader gets the bytes
//! exactly as they arrived.

use crate::extract::{Document, ImagePlacement, Page};
use crate::geom::BBox;

/// The raster formats this recognises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Tiff,
}

impl Format {
    /// The media type a reader should be told.
    pub fn mime(self) -> &'static str {
        match self {
            Format::Png => "image/png",
            Format::Jpeg => "image/jpeg",
            Format::Tiff => "image/tiff",
        }
    }

    /// Recognise by content, not by extension: a file named `.png` holding a
    /// JPEG is common enough, and the reader must be told what it is actually
    /// looking at.
    pub fn sniff(bytes: &[u8]) -> Option<Format> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some(Format::Png);
        }
        if bytes.starts_with(&[0xFF, 0xD8]) {
            return Some(Format::Jpeg);
        }
        if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
            return Some(Format::Tiff);
        }
        None
    }
}

/// Extensions that name a raster image, for a caller routing by filename.
pub const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "tif", "tiff"];

/// The image's pixel size, read from its header.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    match Format::sniff(bytes)? {
        Format::Png => png_size(bytes),
        Format::Jpeg => jpeg_size(bytes),
        Format::Tiff => tiff_size(bytes),
    }
}

/// IHDR is required to be the first chunk, so its position is fixed.
fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.get(12..16)? != b"IHDR" {
        return None;
    }
    Some((be32(b.get(16..20)?), be32(b.get(20..24)?)))
}

/// Walk the marker segments to the frame header.
///
/// The size lives in whichever SOF marker the encoder used, and which one that
/// is says how the image is coded rather than how big it is — baseline,
/// progressive and the arithmetic-coded variants all carry the same four
/// numbers in the same place.
fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize;
    // Bounded: a truncated or hostile file must not walk forever.
    for _ in 0..MAX_JPEG_SEGMENTS {
        // Markers may be padded with any number of 0xFF bytes.
        while *b.get(i)? == 0xFF {
            i += 1;
        }
        let marker = *b.get(i)?;
        i += 1;
        // Standalone markers carry no length.
        if matches!(marker, 0xD8 | 0xD9 | 0x01) || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        let len = be16(b.get(i..i + 2)?) as usize;
        let is_frame = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_frame {
            // length(2) precision(1) height(2) width(2)
            let h = be16(b.get(i + 3..i + 5)?);
            let w = be16(b.get(i + 5..i + 7)?);
            return Some((w as u32, h as u32));
        }
        // Entropy-coded data follows the scan header and is not a segment.
        if marker == 0xDA {
            return None;
        }
        i += len.max(2);
    }
    None
}

const MAX_JPEG_SEGMENTS: usize = 512;
const MAX_TIFF_ENTRIES: usize = 512;

/// Read `ImageWidth` (256) and `ImageLength` (257) from the first IFD.
fn tiff_size(b: &[u8]) -> Option<(u32, u32)> {
    let little = b.starts_with(b"II*\0");
    let u16at = |o: usize| -> Option<u16> {
        let s = b.get(o..o + 2)?;
        Some(if little {
            u16::from_le_bytes([s[0], s[1]])
        } else {
            u16::from_be_bytes([s[0], s[1]])
        })
    };
    let u32at = |o: usize| -> Option<u32> {
        let s = b.get(o..o + 4)?;
        Some(if little {
            u32::from_le_bytes([s[0], s[1], s[2], s[3]])
        } else {
            u32::from_be_bytes([s[0], s[1], s[2], s[3]])
        })
    };
    let ifd = u32at(4)? as usize;
    let count = u16at(ifd)? as usize;
    let (mut w, mut h) = (None, None);
    for e in 0..count.min(MAX_TIFF_ENTRIES) {
        let at = ifd + 2 + e * 12;
        let tag = u16at(at)?;
        // A SHORT sits in the low half of the value field; a LONG fills it.
        let value = match u16at(at + 2)? {
            3 => u16at(at + 8)? as u32,
            4 => u32at(at + 8)?,
            _ => continue,
        };
        match tag {
            256 => w = Some(value),
            257 => h = Some(value),
            _ => {}
        }
        if let (Some(w), Some(h)) = (w, h) {
            return Some((w, h));
        }
    }
    None
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Build the one-page document an image stands for.
///
/// The page carries the image as its only content and no glyphs, which is
/// what makes [`crate::route::decide`] return `Scanned` — the same verdict a
/// scanned PDF page gets, reached the same way, so no separate rule is needed
/// anywhere downstream.
///
/// Size is in pixels treated as PDF units at 1:1. There is no other honest
/// choice: a bare image declares no physical size, and inventing one from an
/// assumed DPI would put a wrong number in `doc:pages` for every consumer
/// that scales by it.
pub fn as_document(bytes: &[u8]) -> Option<Document> {
    let (w, h) = dimensions(bytes)?;
    if w == 0 || h == 0 {
        return None;
    }
    let (width, height) = (f64::from(w), f64::from(h));
    let bbox = BBox {
        x0: 0.0,
        y0: 0.0,
        x1: width,
        y1: height,
    };
    Some(Document {
        pages: vec![Page {
            index: 0,
            glyphs: Vec::new(),
            images: vec![ImagePlacement { bbox, texty: true }],
            rules: Vec::new(),
            fills: Vec::new(),
            width,
            height,
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v
    }

    fn jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        // An APP0 segment first, so the walk has to skip something.
        v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v
    }

    fn tiff(w: u32, h: u32, little: bool) -> Vec<u8> {
        let mut v = if little {
            b"II*\0".to_vec()
        } else {
            b"MM\0*".to_vec()
        };
        let put32 = |v: &mut Vec<u8>, x: u32| {
            if little {
                v.extend_from_slice(&x.to_le_bytes())
            } else {
                v.extend_from_slice(&x.to_be_bytes())
            }
        };
        let put16 = |v: &mut Vec<u8>, x: u16| {
            if little {
                v.extend_from_slice(&x.to_le_bytes())
            } else {
                v.extend_from_slice(&x.to_be_bytes())
            }
        };
        put32(&mut v, 8);
        put16(&mut v, 2);
        for (tag, value) in [(256u16, w), (257u16, h)] {
            put16(&mut v, tag);
            put16(&mut v, 4); // LONG
            put32(&mut v, 1);
            put32(&mut v, value);
        }
        v
    }

    #[test]
    fn each_format_reports_its_size() {
        assert_eq!(dimensions(&png(1280, 720)), Some((1280, 720)));
        assert_eq!(dimensions(&jpeg(800, 600)), Some((800, 600)));
        assert_eq!(dimensions(&tiff(2480, 3508, true)), Some((2480, 3508)));
        assert_eq!(dimensions(&tiff(2480, 3508, false)), Some((2480, 3508)));
    }

    #[test]
    fn the_format_is_read_from_the_bytes_not_the_name() {
        assert_eq!(Format::sniff(&png(1, 1)), Some(Format::Png));
        assert_eq!(Format::sniff(&jpeg(1, 1)), Some(Format::Jpeg));
        assert_eq!(Format::sniff(b"%PDF-1.7"), None);
        assert_eq!(Format::sniff(b""), None);
    }

    #[test]
    fn a_truncated_or_hostile_file_returns_none_rather_than_looping() {
        assert_eq!(dimensions(&png(1, 1)[..20]), None);
        assert_eq!(dimensions(&[0xFF, 0xD8]), None);
        // Nothing but padding: the marker walk must terminate.
        let mut pad = vec![0xFF, 0xD8];
        pad.extend(std::iter::repeat_n(0xFFu8, 4096));
        assert_eq!(jpeg_size(&pad), None);
        assert_eq!(dimensions(b"II*\0"), None);
    }

    #[test]
    fn a_scan_marker_stops_the_walk_rather_than_reading_pixels_as_a_header() {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x02];
        v.extend(std::iter::repeat_n(0u8, 64));
        assert_eq!(jpeg_size(&v), None);
    }

    #[test]
    fn an_image_becomes_one_page_of_pixels_with_no_glyphs() {
        let doc = as_document(&png(1224, 1584)).expect("document");
        assert_eq!(doc.pages.len(), 1);
        let p = &doc.pages[0];
        assert!(p.glyphs.is_empty());
        assert_eq!((p.width, p.height), (1224.0, 1584.0));
        assert_eq!(p.images.len(), 1);
        assert!(p.images[0].texty, "must read as recoverable content");
    }

    #[test]
    fn a_page_of_pixels_routes_to_a_full_reading() {
        // The same verdict a scanned PDF page gets, reached the same way.
        let doc = as_document(&png(1224, 1584)).unwrap();
        assert!(matches!(
            crate::route::decide(&doc.pages[0]).0,
            crate::route::Route::Vlm(_)
        ));
    }

    #[test]
    fn a_zero_sized_image_is_not_a_page() {
        assert!(as_document(&png(0, 100)).is_none());
        assert!(as_document(b"not an image").is_none());
    }
}
