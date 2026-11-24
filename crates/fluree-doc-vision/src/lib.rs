//! Reading pixels.
//!
//! A document's text is not always characters: a scanned page, a screenshot
//! pasted into a report, a photographed form in an e-mail. This crate holds
//! what every format needs to have such pixels read:
//!
//! - the format an OCR engine's words come in ([`WordReading`]), which is
//!   also the `--ocr-words` sidecar format;
//! - the interface of an engine ([`OcrEngine`]), which reads an image held
//!   in memory ([`Pixels`]) into that format;
//! - with the `oar` feature, a built-in engine ([`oar::OarEngine`]):
//!   PP-OCRv6 through oar-ocr, on an ONNX Runtime loaded from an assets
//!   directory rather than linked in;
//! - how an image is sized for an OCR engine ([`sizing`]): at its own
//!   resolution, in passes within a pixel cap;
//! - the probe that tells whether an image looks like text at all
//!   ([`texty`]), and so is worth reading.
//!
//! Laying the words out as text is the layout engine's job
//! (`fluree_doc_pdf::ocr`).

use serde::{Deserialize, Serialize};

#[cfg(feature = "oar")]
pub mod assets;
#[cfg(feature = "oar")]
pub mod oar;
pub mod sizing;
pub mod texty;

/// How each pixel of [`Pixels`] is laid out: 8 bits a channel.
///
/// An image is held as it was decoded or rendered, so a grey scan or a fax
/// costs one byte a pixel, not four; an engine converts it only if it reads
/// another layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// One grey channel.
    Luma8,
    /// Red, green, blue.
    Rgb8,
    /// Red, green, blue, and alpha (not premultiplied): what is transparent
    /// is paper, white.
    Rgba8,
}

impl Layout {
    /// Bytes a pixel.
    pub fn channels(self) -> usize {
        match self {
            Layout::Luma8 => 1,
            Layout::Rgb8 => 3,
            Layout::Rgba8 => 4,
        }
    }
}

/// An image held in memory: rows top to bottom, no padding, each pixel as
/// [`Layout`] says.
#[derive(Debug, Clone, PartialEq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub layout: Layout,
    /// `width * height * layout.channels()` bytes.
    pub data: Vec<u8>,
}

impl Pixels {
    /// `None` when the buffer does not hold `width * height` pixels of
    /// `layout`.
    pub fn new(width: u32, height: u32, layout: Layout, data: Vec<u8>) -> Option<Pixels> {
        let size = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(layout.channels())?;
        (width > 0 && height > 0 && data.len() == size).then_some(Pixels {
            width,
            height,
            layout,
            data,
        })
    }

    /// The image as 8-bit RGB, composited over white: what an engine that
    /// reads colour takes. A copy whatever the layout, since the caller
    /// keeps its image.
    pub fn to_rgb(&self) -> Vec<u8> {
        match self.layout {
            Layout::Rgb8 => self.data.clone(),
            Layout::Luma8 => self.data.iter().flat_map(|&g| [g, g, g]).collect(),
            Layout::Rgba8 => self
                .data
                .chunks_exact(4)
                .flat_map(|p| {
                    let a = p[3] as u16;
                    let over = |c: u8| ((c as u16 * a + 255 * (255 - a)) / 255) as u8;
                    [over(p[0]), over(p[1]), over(p[2])]
                })
                .collect(),
        }
    }
}

/// An OCR engine: reads one image into words with their boxes.
///
/// The reading is in pixels of the image read ([`Coords::Px`], with its
/// `width` and `height` set), in whatever unit the engine boxes: words or
/// whole lines. `Send + Sync`, because a caller reads several crops at once
/// with one engine, loaded once.
pub trait OcrEngine: Send + Sync {
    /// The engine's name, recorded in each reading as `engine`.
    fn name(&self) -> &str;

    /// Read one image.
    fn read(&self, image: &Pixels) -> Result<WordReading, String>;

    /// The most memory, in bytes, the engine may hold at once while it
    /// reads as many images together as it will: what a caller running
    /// other work beside it sets aside. None of note by default.
    fn most_memory(&self) -> u64 {
        0
    }
}

/// One word, or one line, as an OCR engine read it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrWord {
    pub text: String,
    /// `[x0, y0, x1, y1]`, y down: the word's own extent, before any
    /// rotation. With `angle`, the word is this box turned about its center.
    #[serde(rename = "box")]
    pub bbox: [f64; 4],
    /// Baseline direction, degrees counter-clockwise from horizontal as the
    /// image is viewed. Absent means horizontal, unless the box is much taller
    /// than wide and holds several characters: that is text set sideways.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<f64>,
    /// The engine's confidence in the reading, 0-1, when it gives one. A
    /// score outside 0-1 says the reading is on another scale: none of its
    /// words' scores is then used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conf: Option<f64>,
}

/// The frame a reading's boxes are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Coords {
    /// Pixels of the image that was read. Any resolution: the reading's
    /// `width` and `height` scale it onto what the image shows.
    #[default]
    Px,
    /// The units of the page the image is placed on, as displayed (after
    /// `/Rotate`), y down: the frame the output's boxes are in. Not PDF user
    /// space, whose y runs up and which is taken before the page is turned.
    Page,
}

/// What an engine read in one image.
///
/// The `--ocr-words` sidecar format, and stable as such: a release changes
/// or removes a field only as a breaking change; a new optional field may
/// be added.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordReading {
    #[serde(default)]
    pub coords: Coords,
    /// Size of the image read, in pixels. Required for [`Coords::Px`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    /// Which engine read it, for whoever compares two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    pub words: Vec<OcrWord>,
}

impl WordReading {
    /// Parse the `--ocr-words` sidecar format.
    pub fn from_json(s: &str) -> Result<Self, String> {
        let r: WordReading = serde_json::from_str(s).map_err(|e| e.to_string())?;
        if r.coords == Coords::Px
            && !matches!((r.width, r.height), (Some(w), Some(h)) if w > 0.0 && h > 0.0)
        {
            return Err("pixel coordinates need the image's `width` and `height`".into());
        }
        Ok(r)
    }

    /// How many words carry a confidence outside 0-1: a Tesseract reading
    /// converted without dividing its 0-100 scores by 100, say. One is
    /// enough for the reading to report no confidence at all. A word with
    /// no text is not counted: it is never placed, and Tesseract scores the
    /// rows of its blocks and lines, which hold none, -1 on any scale.
    pub fn scores_out_of_range(&self) -> usize {
        self.with_text()
            .filter(|w| w.conf.is_some_and(|c| !(0.0..=1.0).contains(&c)))
            .count()
    }

    /// How many words hold text: those [`scores_out_of_range`] counts
    /// among, the total its count is of.
    ///
    /// [`scores_out_of_range`]: WordReading::scores_out_of_range
    pub fn words_with_text(&self) -> usize {
        self.with_text().count()
    }

    fn with_text(&self) -> impl Iterator<Item = &OcrWord> {
        self.words.iter().filter(|w| !w.text.trim().is_empty())
    }

    /// Write the sidecar format, as [`WordReading::from_json`] reads it.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a reading is plain data")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_outside_zero_to_one_are_counted() {
        let r = WordReading::from_json(
            r#"{"coords": "page", "words": [
                {"text": "a", "box": [0, 0, 1, 1], "conf": 96},
                {"text": "b", "box": [0, 0, 1, 1], "conf": 1},
                {"text": "c", "box": [0, 0, 1, 1], "conf": 0},
                {"text": "d", "box": [0, 0, 1, 1], "conf": -1},
                {"text": "e", "box": [0, 0, 1, 1]},
                {"text": " ", "box": [0, 0, 1, 1], "conf": -1}
            ]}"#,
        )
        .unwrap();
        // Not the row with no text, which no word is placed from.
        assert_eq!((r.scores_out_of_range(), r.words_with_text()), (2, 5));
    }

    #[test]
    fn a_box_reads_back_as_the_number_written() {
        // Written by `fdoc ocr`, read by `convert --ocr-words`: the boxes must
        // be the numbers the engine gave, to the last bit.
        let words: Vec<OcrWord> = (0..2000)
            .map(|i| OcrWord {
                text: "w".into(),
                bbox: [
                    116.995_171_723_846_03 + i as f64 / 7.0,
                    1.0 / 3.0 + i as f64,
                    0.1 + 0.2 * i as f64,
                    2.0_f64.sqrt() * i as f64,
                ],
                angle: Some(-0.123_456_789_012_345_67 * i as f64),
                conf: Some(0.987_654_321_098_765_4),
            })
            .collect();
        let reading = WordReading {
            coords: Coords::Px,
            width: Some(1224.0),
            height: Some(1584.0),
            engine: None,
            words,
        };
        let back = WordReading::from_json(&reading.to_json()).unwrap();
        for (a, b) in reading.words.iter().zip(&back.words) {
            assert_eq!(a.bbox.map(f64::to_bits), b.bbox.map(f64::to_bits));
            assert_eq!(a.angle.map(f64::to_bits), b.angle.map(f64::to_bits));
        }
    }

    #[test]
    fn the_sidecar_round_trips() {
        let json = r#"{"width": 1224, "height": 1584, "engine": "pp-ocrv6-small",
            "words": [{"text": "Account", "box": [144, 200, 284, 226], "conf": 0.98},
                      {"text": "Date of birth", "box": [144, 260, 404, 286], "angle": 0.0}]}"#;
        let r = WordReading::from_json(json).unwrap();
        assert_eq!(r.coords, Coords::Px);
        assert_eq!(r.words[1].angle, Some(0.0));
        assert_eq!(r.words[0].angle, None);
        assert_eq!(WordReading::from_json(&r.to_json()).unwrap(), r);
    }

    #[test]
    fn pixels_hold_their_layout_and_read_as_rgb() {
        // One pixel each: a grey, a colour, a half-transparent black.
        let grey = Pixels::new(1, 1, Layout::Luma8, vec![40]).unwrap();
        assert_eq!(grey.to_rgb(), [40, 40, 40]);
        let colour = Pixels::new(1, 1, Layout::Rgb8, vec![1, 2, 3]).unwrap();
        assert_eq!(colour.to_rgb(), [1, 2, 3]);
        let half = Pixels::new(1, 1, Layout::Rgba8, vec![0, 0, 0, 128]).unwrap();
        assert_eq!(half.to_rgb(), [127, 127, 127], "over white paper");
        // A buffer sized for another layout is refused.
        assert!(Pixels::new(2, 2, Layout::Luma8, vec![0; 16]).is_none());
        assert!(Pixels::new(2, 2, Layout::Rgba8, vec![0; 4]).is_none());
        assert!(Pixels::new(0, 2, Layout::Luma8, Vec::new()).is_none());
    }

    #[test]
    fn pixels_need_the_image_size() {
        let e = WordReading::from_json(r#"{"words": []}"#).unwrap_err();
        assert!(e.contains("width"), "{e}");
        let page = WordReading::from_json(r#"{"coords": "page", "words": []}"#).unwrap();
        assert_eq!(page.coords, Coords::Page);
    }
}
