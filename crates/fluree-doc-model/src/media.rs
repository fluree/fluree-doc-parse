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

#[cfg(test)]
mod tests {
    use super::*;

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
