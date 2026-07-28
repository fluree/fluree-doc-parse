//! Geometry helpers shared by extraction and overlay resolution.

/// A rectangle in PDF user units with a **top-left origin** (y grows downward).
///
/// `Page::initial_transform(true)` already flips the PDF's bottom-up axis, so
/// everything downstream — including CSS overlay coordinates — uses this
/// convention without a further flip.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct BBox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl BBox {
    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }
    pub fn is_empty(&self) -> bool {
        self.width() <= 0.0 || self.height() <= 0.0
    }
    /// Any overlap at all, boundary contact included.
    pub fn intersects(&self, o: &BBox) -> bool {
        self.x0 <= o.x1 && o.x0 <= self.x1 && self.y0 <= o.y1 && o.y0 <= self.y1
    }
    pub fn contains(&self, x: f64, y: f64) -> bool {
        (self.x0..=self.x1).contains(&x) && (self.y0..=self.y1).contains(&y)
    }

    pub fn union(&self, o: &BBox) -> BBox {
        BBox {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    /// True when two boxes sit on the same visual line — used to merge a matched
    /// span's glyph boxes into one rect per line rather than one per glyph.
    pub fn same_line(&self, o: &BBox) -> bool {
        let tol = self.height().max(o.height()) * 0.5;
        (self.y0 - o.y0).abs() < tol
    }
}

/// A page's rendered size, in PDF user units.
///
/// Emitted alongside the graph because a bounding box is meaningless without
/// it: a consumer drawing a highlight over a rendered page has to scale
/// `doc:bbox` by the ratio between the page's units and the pixels it
/// rendered to, and nothing else in the output carries the denominator.
///
/// Only sources with pages have these. Markdown and DOCX declare structure
/// and no geometry, so they report none rather than a zeroed size.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct PageSize {
    /// 0-based physical position, the same space as `Element::page`.
    #[serde(rename = "pageIndex")]
    pub index: usize,
    pub width: f64,
    pub height: f64,
}
