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
//! The page's size comes from the caller: `fluree_doc_image::header` reads
//! it upright, by the image's EXIF orientation, without decoding the pixels.
//! [`dimensions`] reads the size as stored, by hand, for a header the
//! decoder will not take. The reader gets the bytes exactly as they arrived.

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

    /// The bytes a format is told by: the longest signature [`Format::sniff`]
    /// reads, PNG's. Fewer than that is no image of any format, but a part
    /// emptied or cut short, which failed rather than held another format.
    pub const SNIFFED: usize = 8;

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
        // Classic TIFF (version 42) and BigTIFF (43).
        if bytes.starts_with(b"II*\0")
            || bytes.starts_with(b"MM\0*")
            || bytes.starts_with(b"II+\0")
            || bytes.starts_with(b"MM\0+")
        {
            return Some(Format::Tiff);
        }
        None
    }
}

/// Extensions that name a raster image, for a caller routing by filename.
pub const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "tif", "tiff"];

/// The image's pixel size as stored, read from its header by hand
/// ([`fluree_doc_model::stored_size`]), for the formats this recognises: a
/// phone photo's EXIF orientation is not applied, nor a BigTIFF's size read.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    Format::sniff(bytes)?;
    fluree_doc_model::stored_size(bytes)
}

/// The one-page document an image `width` × `height` pixels stands for.
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
///
/// Sized as the caller says: upright, by its EXIF orientation, is the size a
/// reader sees, which `fluree_doc_image::header` gives without decoding the
/// pixels ([`dimensions`], the size as stored, does not). `bitonal` says the
/// image is one bit deep.
pub fn page_of(width: u32, height: u32, bitonal: bool) -> Document {
    let grid = (width, height);
    let (width, height) = (f64::from(width), f64::from(height));
    let bbox = BBox {
        x0: 0.0,
        y0: 0.0,
        x1: width,
        y1: height,
    };
    Document {
        pages: vec![Page {
            index: 0,
            glyphs: Vec::new(),
            images: vec![ImagePlacement {
                bbox,
                texty: Some(true),
                // One pixel per unit: 72 per inch of the page's units.
                ppi: Some(72.0),
                bitonal,
                grid,
                key: None,
            }],
            rules: Vec::new(),
            routing: None,
            fills: Vec::new(),
            dots: Vec::new(),
            traces: Vec::new(),
            ticks: Vec::new(),
            width,
            height,
            rotation: 0,
            ocr: None,
            warnings: Vec::new(),
        }],
        title: None,
    }
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
        assert_eq!(dimensions(&pad), None);
        assert_eq!(dimensions(b"II*\0"), None);
        // A BigTIFF's first directory is laid out otherwise: no size read.
        let mut big = b"II+\0".to_vec();
        big.extend(8u16.to_le_bytes());
        big.extend(0u16.to_le_bytes());
        big.extend(16u64.to_le_bytes());
        big.extend([0u8; 64]);
        assert_eq!(dimensions(&big), None);
    }

    #[test]
    fn a_scan_marker_stops_the_walk_rather_than_reading_pixels_as_a_header() {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x02];
        v.extend(std::iter::repeat_n(0u8, 64));
        assert_eq!(dimensions(&v), None);
    }

    #[test]
    fn an_image_becomes_one_page_of_pixels_with_no_glyphs() {
        let doc = page_of(1224, 1584, false);
        assert_eq!(doc.pages.len(), 1);
        let p = &doc.pages[0];
        assert!(p.glyphs.is_empty());
        assert_eq!((p.width, p.height), (1224.0, 1584.0));
        assert_eq!(p.images.len(), 1);
        assert_eq!(
            p.images[0].texty,
            Some(true),
            "must read as recoverable content"
        );
    }

    #[test]
    fn a_page_of_pixels_routes_to_a_full_reading() {
        // The same verdict a scanned PDF page gets, reached the same way.
        let doc = page_of(1224, 1584, false);
        assert!(matches!(
            crate::route::decide(&doc.pages[0]).0,
            crate::route::Route::Vlm(_)
        ));
    }
}
