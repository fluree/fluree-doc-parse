//! The images a document places in its flow, and the one loop that reads
//! them: a DOCX's pictures, a workbook's, the images an e-mail shows in its
//! body. Each image is read once however many figures draw it, within an
//! allowance per document.

use std::collections::HashMap;

/// Where a figure's image is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MediaSource {
    /// Held by the file, under the name it keeps it by: a package part
    /// (`word/media/image1.png`), a message's `cid:` URL (RFC 2392) or
    /// attachment name.
    Part(String),
    /// A file outside it (a DOCX's `r:link`, `TargetMode="External"`), as
    /// the file names it: never read, even when it is spelled like one of
    /// its parts.
    External(String),
    /// A reference the file does not resolve: a relationship id it does not
    /// define.
    Missing(String),
}

impl MediaSource {
    /// The part the file holds the image under, for an image it holds.
    pub fn part(&self) -> Option<&str> {
        match self {
            MediaSource::Part(p) => Some(p),
            MediaSource::External(_) | MediaSource::Missing(_) => None,
        }
    }

    /// The name the file gives it, whatever it resolves to.
    pub fn name(&self) -> &str {
        match self {
            MediaSource::Part(n) | MediaSource::External(n) | MediaSource::Missing(n) => n,
        }
    }
}

impl std::fmt::Display for MediaSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MediaSource::Part(p) => f.write_str(p),
            MediaSource::External(url) => write!(f, "{url} (outside the file)"),
            MediaSource::Missing(rid) => write!(f, "{rid} (no such relationship)"),
        }
    }
}

/// An image placed in a document's flow: a picture in a DOCX, a picture
/// anchored to a sheet's cells, an image a message shows in its body. It is
/// drawn as a figure (`Element::figure`), after which the text read in it is
/// placed. Its bytes are read on demand, one image at a time, so a document
/// of many large pictures is never held decompressed all at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Media {
    /// The figure element it is drawn as (`Element::figure`).
    pub figure: String,
    /// Where its image is.
    pub source: MediaSource,
    /// Its place among the figure's images: a group of pictures is one
    /// figure holding several.
    pub index: usize,
}

impl Media {
    /// The name its crop and its words go by: `m<n>` for the figure
    /// `figure-<page>-<n>`, `m<n>_<k>` for the figure's `k`-th image after
    /// the first.
    pub fn tag(&self) -> String {
        let n = self.figure.rsplit('-').next().unwrap_or("0");
        match self.index {
            0 => format!("m{n}"),
            k => format!("m{n}_{k}"),
        }
    }
}

/// One image of the document, however many figures draw it: a logo or a
/// screenshot pasted several times is read once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    /// Where the image is.
    pub source: MediaSource,
    /// Each place the document draws it, in reading order. The first names
    /// its crop and its words ([`Picture::tag`]).
    pub uses: Vec<Media>,
}

impl Picture {
    /// The name its crop and its words go by: its first use's
    /// ([`Media::tag`]).
    pub fn tag(&self) -> String {
        self.uses[0].tag()
    }

    /// The figures it is drawn as, each once, in reading order.
    pub fn figures(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for m in &self.uses {
            if !out.contains(&m.figure.as_str()) {
                out.push(&m.figure);
            }
        }
        out
    }

    /// What was read in it, once after each figure it is drawn as: the
    /// readings `fluree_doc_image::place_after_figures` takes.
    pub fn after_each_figure<T: Clone>(&self, read: Vec<T>) -> Vec<(String, Vec<T>)> {
        let figures = self.figures();
        let last = figures.len().saturating_sub(1);
        let mut read = Some(read);
        figures
            .into_iter()
            .enumerate()
            .map(|(i, f)| {
                let r = if i == last {
                    read.take().unwrap_or_default()
                } else {
                    read.clone().unwrap_or_default()
                };
                (f.to_string(), r)
            })
            .collect()
    }
}

/// The document's pictures: its media grouped by where their image is, in
/// the order each is first drawn.
pub fn pictures(media: Vec<Media>) -> Vec<Picture> {
    let mut at: HashMap<MediaSource, usize> = HashMap::new();
    let mut out: Vec<Picture> = Vec::new();
    for m in media {
        match at.get(&m.source) {
            Some(&i) => out[i].uses.push(m),
            None => {
                at.insert(m.source.clone(), out.len());
                out.push(Picture {
                    source: m.source.clone(),
                    uses: vec![m],
                });
            }
        }
    }
    out
}

/// How much one document may ask a reader of its pictures to decode: past
/// it, pictures are left unread ([`Unread::PastAllowance`]). Each picture is
/// bounded on its own (the bytes of its part, the pixels of its image), but
/// a document can hold hundreds of them in a few kilobytes each, so the
/// document is bounded too, the way a PDF page bounds its crops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allowance {
    /// Pictures whose part is read.
    pub pictures: usize,
    /// Pixels the pictures read hold together, as their headers state them.
    pub pixels: u64,
}

impl Allowance {
    /// A document's: 256 pictures and 512 million pixels, four images at the
    /// most one image may hold, or some sixty pages scanned at 300 dpi.
    pub const DOCUMENT: Allowance = Allowance {
        pictures: 256,
        pixels: 512_000_000,
    };
}

/// Why a picture was not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// The file does not hold its image: a file outside it, or a reference
    /// it does not resolve. Nothing is fetched.
    NotInPackage,
    /// The reader had nothing to read it with (no words written for it).
    NotAsked,
    /// Past the document's [`Allowance`].
    PastAllowance,
    /// Reading it failed: its part missing or too large, not a raster image
    /// (EMF, WMF), or refused by the reader.
    Failed(String),
}

impl std::fmt::Display for Unread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unread::NotInPackage => f.write_str("its image is not in the file"),
            Unread::NotAsked => f.write_str("nothing was given to read it with"),
            Unread::PastAllowance => {
                f.write_str("past the pictures or pixels one document may have read")
            }
            Unread::Failed(e) => f.write_str(e),
        }
    }
}

/// What reads a document's pictures, for [`read_pictures`].
pub trait PictureReader {
    type Reading;
    /// Whether there is anything to read `picture` with (an engine always
    /// has; words written for it may not be there). Asked before its bytes
    /// are read.
    fn ready(&mut self, _picture: &Picture) -> bool {
        true
    }
    /// The pixels an image holds, from its header, without decoding it; an
    /// error for what this reader cannot read.
    fn pixels(&mut self, image: &[u8]) -> Result<u64, String>;
    /// Read the image.
    fn read(&mut self, picture: &Picture, image: &[u8]) -> Result<Self::Reading, String>;
}

/// What [`read_pictures`] gives.
#[derive(Debug, Clone)]
pub struct PictureReads<R> {
    /// The pictures read, in the order they are first drawn, each with its
    /// reading.
    pub read: Vec<(Picture, R)>,
    /// The pictures not read, each with why.
    pub unread: Vec<(Picture, Unread)>,
}

/// Read a document's pictures ([`pictures`]): each image once, however
/// many figures draw it, one at a time (its bytes read by `bytes` from the
/// part the file holds it under, measured, read by `reader` and dropped),
/// within `allowance`. A picture is charged to the allowance before its part
/// is read, and its pixels once its header is.
pub fn read_pictures<R: PictureReader>(
    media: Vec<Media>,
    bytes: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
    reader: &mut R,
    allowance: Allowance,
) -> PictureReads<R::Reading> {
    let mut out = PictureReads {
        read: Vec::new(),
        unread: Vec::new(),
    };
    let (mut pictures_left, mut pixels_left) = (allowance.pictures, allowance.pixels);
    for picture in pictures(media) {
        let Some(part) = picture.source.part() else {
            out.unread.push((picture, Unread::NotInPackage));
            continue;
        };
        if !reader.ready(&picture) {
            out.unread.push((picture, Unread::NotAsked));
            continue;
        }
        if pictures_left == 0 {
            out.unread.push((picture, Unread::PastAllowance));
            continue;
        }
        pictures_left -= 1;
        let got = bytes(part).and_then(|image| {
            let pixels = reader.pixels(&image)?;
            if pixels > pixels_left {
                return Ok(None);
            }
            pixels_left -= pixels;
            reader.read(&picture, &image).map(Some)
        });
        match got {
            Ok(Some(r)) => out.read.push((picture, r)),
            Ok(None) => out.unread.push((picture, Unread::PastAllowance)),
            Err(e) => out.unread.push((picture, Unread::Failed(e))),
        }
    }
    out
}

/// The shortest side, in pixels, of an image a document places that a
/// model is sent: a small signature logo, a social icon or a rule is
/// shorter, a screenshot of one line of a form is not. Measured on
/// synthetic e-mails and documents, whose captures were 60 pixels high and
/// more.
pub const MIN_SHORT_SIDE_PX: u32 = 48;

/// The longest side, in pixels, of such an image that a model is sent: a
/// small logo is narrower, and a phone's screenshot is taller. The
/// screenshots measured were 547 pixels wide and more, but a small dialog
/// box can be narrower (380 pixels): `fdoc convert` does not send it, and
/// lists it as not read (`Small`), for OCR to read it, without counting it
/// on stderr.
pub const MIN_LONG_SIDE_PX: u32 = 400;

/// Whether an image a document places, of `width` × `height` pixels, is
/// under the floor a model is sent ([`MIN_SHORT_SIDE_PX`],
/// [`MIN_LONG_SIDE_PX`]): one OCR reads, and `fdoc render-routed` crops as
/// `small_image`. The same either way up, so an image's size as stored
/// ([`stored_size`]) answers it as well as its size upright.
///
/// An e-mail's image under it is taken for an icon of a signature (a
/// `LinkedIn`, a `Logo`): `fluree_doc_email` keeps no alternative text on
/// its figure.
pub fn under_model_floor(width: u32, height: u32) -> bool {
    width.min(height) < MIN_SHORT_SIDE_PX || width.max(height) < MIN_LONG_SIDE_PX
}

/// An image's size in pixels as stored, read from its header by hand, for a
/// PNG, a JPEG, a classic TIFF, a GIF or a BMP: a phone photo's EXIF
/// orientation is not applied, nor a BigTIFF's size read. `None` for any
/// other file, or a header cut short.
pub fn stored_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let b = bytes;
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return png_size(b);
    }
    if b.starts_with(&[0xFF, 0xD8]) {
        return jpeg_size(b);
    }
    if b.starts_with(b"II*\0") || b.starts_with(b"MM\0*") {
        return tiff_size(b);
    }
    // A GIF's logical screen: two little-endian 16-bit numbers.
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        let le16 = |at: usize| {
            Some(u32::from(u16::from_le_bytes(
                b.get(at..at + 2)?.try_into().ok()?,
            )))
        };
        return Some((le16(6)?, le16(8)?));
    }
    // A BMP's info header: its height is negative where its rows run top
    // down.
    if b.starts_with(b"BM") {
        let le32 = |at: usize| Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?));
        return Some((le32(18)?.unsigned_abs(), le32(22)?.unsigned_abs()));
    }
    None
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

/// Read `ImageWidth` (256) and `ImageLength` (257) from the first IFD of a
/// classic TIFF.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gifs_or_a_bmps_size_is_read_from_its_header() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[16, 0, 24, 0]);
        assert_eq!(stored_size(&gif), Some((16, 24)));
        assert_eq!(stored_size(&gif[..9]), None);
        // A top-down BMP states a negative height.
        let mut bmp = vec![0u8; 26];
        bmp[..2].copy_from_slice(b"BM");
        bmp[18..22].copy_from_slice(&640i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&(-480i32).to_le_bytes());
        assert_eq!(stored_size(&bmp), Some((640, 480)));
        assert_eq!(stored_size(b"BMW,Audi\n"), None);
        assert_eq!(stored_size(b"%PDF-1.7"), None);
    }

    #[test]
    fn the_model_floor_is_the_same_either_way_up() {
        assert!(under_model_floor(16, 16));
        assert!(under_model_floor(1000, 47));
        assert!(under_model_floor(47, 1000));
        assert!(under_model_floor(399, 399));
        assert!(!under_model_floor(400, 48));
        assert!(!under_model_floor(48, 400));
    }

    fn m(figure: &str, index: usize) -> Media {
        Media {
            figure: figure.into(),
            source: MediaSource::Part(String::new()),
            index,
        }
    }

    #[test]
    fn an_image_is_named_by_its_figures_number_whatever_its_page() {
        assert_eq!(m("figure-0-3", 0).tag(), "m3");
        assert_eq!(m("figure-2-12", 0).tag(), "m12");
        assert_eq!(m("figure-2-12", 2).tag(), "m12_2");
    }

    #[test]
    fn a_source_is_named_as_the_file_names_it() {
        let ext = MediaSource::External("https://example.org/x.png".into());
        assert_eq!(ext.name(), "https://example.org/x.png");
        assert_eq!(ext.part(), None);
        assert_eq!(MediaSource::Part("cid:a@b".into()).part(), Some("cid:a@b"));
    }
}
