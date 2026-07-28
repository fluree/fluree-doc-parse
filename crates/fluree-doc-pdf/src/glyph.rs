//! The primitive our whole pipeline is built on: a positioned glyph.

use crate::geom::BBox;

/// One glyph as drawn on the page.
///
/// `text` is the *raw* string from the font's ToUnicode mapping — not normalized.
/// Normalization happens later and produces a separate offset space (see
/// [`crate::text::PageText`]), because NFKC can change string length and we must
/// keep raw offsets to resolve back to a bounding box.
#[derive(Clone, Debug)]
pub struct Glyph {
    pub text: String,
    /// `None` for glyphs with no outline (whitespace). The span-merge step
    /// bridges these from neighbouring glyphs rather than dropping the offset.
    pub bbox: Option<BBox>,
    pub page: usize,
    /// Pen position on the text baseline, from the transform's translation.
    ///
    /// This — not the bbox top — is the correct anchor for line grouping.
    /// Cap-height, x-height and descender glyphs on one visual line have
    /// different bbox tops but share a baseline.
    pub origin: (f64, f64),
    /// Baseline direction in degrees, counter-clockwise from horizontal.
    ///
    /// Semantically load-bearing: in a mechanical drawing a 90° run is an axis
    /// title or a vertical dimension, not body text, and must not be fed into
    /// horizontal reading order.
    pub rotation_deg: f32,
    /// Rendered font size in PDF units.
    pub font_size: f32,
    /// Pen advance in PDF units — how far the pen moves after this glyph.
    ///
    /// The typographically correct basis for gap measurement. Using bbox edges
    /// instead under-measures after letters with a right overhang (`f`, `y`,
    /// `r`), which is why `of the` was extracting as `ofthe`.
    pub advance: Option<f64>,
    /// OpenType weight class, 100-900 (400 normal, 700 bold), when the font
    /// declares one. Bold is the most common heading cue after size, and
    /// without it headings set at body size in a bold face are invisible.
    pub weight: Option<u32>,
    /// Index in the draw order. Faux-bold overprint shows up as consecutive
    /// glyphs with identical text and near-identical boxes.
    pub draw_index: usize,
}

impl Glyph {
    /// Rotation bucketed to the nearest 15°, for grouping runs by orientation.
    pub fn rotation_bucket(&self) -> i32 {
        ((self.rotation_deg / 15.0).round() as i32) * 15
    }
    pub fn is_horizontal(&self) -> bool {
        self.rotation_bucket() == 0
    }
    /// Geometric center: bbox midpoint when the glyph has a box, else its
    /// pen origin.
    pub fn center(&self) -> (f64, f64) {
        match self.bbox {
            Some(b) => ((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5),
            None => self.origin,
        }
    }
}
