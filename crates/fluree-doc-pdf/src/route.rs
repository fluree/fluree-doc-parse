//! Page routing: deterministic layout vs the VLM tier.
//!
//! The router's objective is asymmetric. A deterministic page costs
//! milliseconds; a VLM page costs seconds of GPU. So the question is never
//! "might the VLM do better?" — on ordinary born-digital pages it does not,
//! and routing them would multiply average latency for nothing. Measured on a
//! TI mechanical package drawing, the VLM returned the whole drawing as one
//! opaque image block and every dimension callout inside it as `<img>`, where
//! the deterministic path extracts that same text with per-glyph rotation: for
//! CAD the arrow points the other way. The question is "is the
//! deterministic output *unusable*?", which happens for exactly two reasons:
//!
//! 1. **The text is pixels.** A scanned page carries one large image and few
//!    or no glyphs. No geometric analysis of absent glyphs can parse it.
//! 2. **The text is garbage.** Broken CID fonts extract glyphs whose Unicode
//!    is unknown or wrong; the layout may be perfect and the text worthless.
//!
//! Both conditions are measured, not inferred: image coverage and glyph count
//! for the first, Unicode resolution rate for the second.

use crate::extract::Page;
use crate::geom::BBox;

/// Fraction of the page a single image (or the union of images) must cover
/// before the page is image-dominated. Half the page: figures and photos in
/// ordinary documents run 10-30%; scans run 90%+.
const IMAGE_DOMINANT_COVERAGE: f64 = 0.5;

/// Below this many text-bearing glyphs, an image-dominated page has no
/// meaningful born-digital text layer. A caption under a full-page figure is
/// ~10-40 glyphs; a page of prose is 1500+.
const MIN_GLYPHS_FOR_TEXT_PAGE: usize = 100;

/// Unicode resolution below this means the text layer cannot be trusted even
/// though glyphs exist — broken CID fonts, symbol soup.
const MIN_UNICODE_RATE: f64 = 0.85;

/// Below this many glyphs a page has effectively no text layer; with an image
/// present, the image *is* the page.
const NEAR_BLANK_GLYPHS: usize = 20;

/// A raster region must cover at least this fraction of the page before it is
/// worth a VLM call: below it, whatever text it holds is a logo or ornament.
const MIN_REGION_FRACTION: f64 = 0.10;

/// Above this many glyphs inside a candidate region, the text layer already
/// reads it — a figure with overlaid glyph labels, or text drawn on a
/// background image. A handful is tolerated for stray watermarks or a
/// caption grazing the box.
const MAX_GLYPHS_IN_UNREAD_REGION: usize = 15;

/// Grid resolution for image-coverage union. Coarse is fine: the signal is
/// "half the page", not a measurement.
const COVERAGE_BINS: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// The deterministic pipeline's output is trustworthy.
    Deterministic,
    /// Send the whole rendered page to the VLM.
    Vlm(Reason),
    /// The text layer is fine, but these raster regions carry text or table
    /// structure the deterministic path cannot read. Render each region, send
    /// it to the VLM, and splice the result into the deterministic output at
    /// the region's position.
    VlmRegions(Vec<BBox>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Image-dominated page with no meaningful text layer: a scan.
    Scanned,
    /// Almost no glyphs, but at least one image: the content is the image.
    NearBlank,
    /// Glyphs exist but their Unicode cannot be trusted.
    BrokenText,
}

/// Per-page signals, exposed for diagnostics (`fdoc route`).
#[derive(Debug, Clone, Copy)]
pub struct Signals {
    pub glyphs: usize,
    pub unicode_rate: f64,
    pub image_coverage: f64,
}

pub fn signals(page: &Page) -> Signals {
    let glyphs = page
        .glyphs
        .iter()
        .filter(|g| !g.text.trim().is_empty())
        .count();
    // Unresolved means *no* Unicode at all. An explicit space glyph carries
    // " " as its text and is perfectly resolved — counting whitespace as
    // unresolved made every ordinary page look ~15% broken and routed half
    // the corpus.
    let resolved = page.glyphs.iter().filter(|g| !g.text.is_empty()).count();
    let total = page.glyphs.len();
    let unicode_rate = if total == 0 {
        1.0
    } else {
        resolved as f64 / total as f64
    };

    // Union of image areas on a coarse grid, as a fraction of the page.
    let mut grid = [false; COVERAGE_BINS * COVERAGE_BINS];
    let (pw, ph) = (page.width.max(1.0), page.height.max(1.0));
    for pl in &page.images {
        let b = &pl.bbox;
        let c0 = ((b.x0 / pw * COVERAGE_BINS as f64).floor().max(0.0)) as usize;
        let c1 = ((b.x1 / pw * COVERAGE_BINS as f64).ceil() as usize).min(COVERAGE_BINS);
        let r0 = ((b.y0 / ph * COVERAGE_BINS as f64).floor().max(0.0)) as usize;
        let r1 = ((b.y1 / ph * COVERAGE_BINS as f64).ceil() as usize).min(COVERAGE_BINS);
        for r in r0..r1 {
            for c in c0..c1 {
                grid[r * COVERAGE_BINS + c] = true;
            }
        }
    }
    let covered = grid.iter().filter(|x| **x).count();
    let image_coverage = covered as f64 / (COVERAGE_BINS * COVERAGE_BINS) as f64;

    Signals {
        glyphs,
        unicode_rate,
        image_coverage,
    }
}

pub fn decide(page: &Page) -> (Route, Signals) {
    let s = signals(page);
    let page_area = (page.width * page.height).max(1.0);

    if s.glyphs < NEAR_BLANK_GLYPHS && !page.images.is_empty() {
        return (Route::Vlm(Reason::NearBlank), s);
    }
    if s.image_coverage >= IMAGE_DOMINANT_COVERAGE && s.glyphs < MIN_GLYPHS_FOR_TEXT_PAGE {
        return (Route::Vlm(Reason::Scanned), s);
    }
    // Gate on glyph *events*, not resolved glyphs: a fully broken font (a
    // Type3 subset with no ToUnicode, say) resolves nothing, so counting only
    // usable glyphs would make the most broken page of all look blank and
    // sail through as a confident empty answer.
    if page.glyphs.len() >= MIN_GLYPHS_FOR_TEXT_PAGE && s.unicode_rate < MIN_UNICODE_RATE {
        return (Route::Vlm(Reason::BrokenText), s);
    }

    // Region routing: a healthy text page carrying sizable raster regions
    // whose pixels look like text or table structure *and which the glyph
    // layer does not already cover*. The deterministic output stands; the VLM
    // reads only the pixels it alone can read.
    //
    // The glyph test is what makes this minimal. Many figures are drawn as an
    // image with their labels overlaid as real glyphs — we already read
    // those, and a VLM pass would re-transcribe content we have. A raster
    // table is the opposite: text above and below, a glyph void where the
    // content sits. Pixel statistics cannot make this distinction (measured:
    // texty-row fractions of helped and unhelped documents overlap
    // completely); glyph coverage can.
    let regions: Vec<BBox> = page
        .images
        .iter()
        .filter(|p| {
            if !p.texty
                || (p.bbox.x1 - p.bbox.x0) * (p.bbox.y1 - p.bbox.y0)
                    < MIN_REGION_FRACTION * page_area
            {
                return false;
            }
            let inside = page
                .glyphs
                .iter()
                .filter(|g| {
                    let (x, y) = match g.bbox {
                        Some(b) => ((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5),
                        None => g.origin,
                    };
                    !g.text.trim().is_empty() && p.bbox.contains(x, y)
                })
                .count();
            inside < MAX_GLYPHS_IN_UNREAD_REGION
        })
        .map(|p| p.bbox)
        .collect();
    if !regions.is_empty() {
        return (Route::VlmRegions(regions), s);
    }
    (Route::Deterministic, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::Page;
    use crate::glyph::Glyph;

    use crate::extract::ImagePlacement;

    fn page_with(glyphs: usize, unresolved: usize, images: Vec<(BBox, bool)>) -> Page {
        let mut v = Vec::new();
        for i in 0..glyphs + unresolved {
            v.push(Glyph {
                text: if i < glyphs {
                    "a".into()
                } else {
                    String::new()
                },
                bbox: Some(BBox {
                    x0: 10.0,
                    y0: 10.0,
                    x1: 15.0,
                    y1: 20.0,
                }),
                page: 0,
                origin: (10.0, 20.0),
                rotation_deg: 0.0,
                font_size: 10.0,
                weight: None,
                advance: None,
                draw_index: i,
            });
        }
        Page {
            index: 0,
            glyphs: v,
            images: images
                .into_iter()
                .map(|(bbox, texty)| ImagePlacement { bbox, texty })
                .collect(),
            rules: Vec::new(),
            fills: Vec::new(),
            width: 600.0,
            height: 800.0,
        }
    }

    #[test]
    fn a_full_page_scan_routes_to_the_vlm() {
        // Near-blank fires first — same destination, more specific reason.
        let p = page_with(
            5,
            0,
            vec![(
                BBox {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 600.0,
                    y1: 800.0,
                },
                true,
            )],
        );
        assert!(matches!(decide(&p).0, Route::Vlm(Reason::NearBlank)));
        // With a thin caption layer it is a scan proper.
        let p = page_with(
            60,
            0,
            vec![(
                BBox {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 600.0,
                    y1: 800.0,
                },
                true,
            )],
        );
        assert!(matches!(decide(&p).0, Route::Vlm(Reason::Scanned)));
    }

    #[test]
    fn a_photograph_in_a_text_page_stays_deterministic() {
        // Third of a page of image, plenty of text — but the pixels read as a
        // photograph, so there is nothing there the VLM can transcribe.
        let p = page_with(
            1500,
            0,
            vec![(
                BBox {
                    x0: 50.0,
                    y0: 50.0,
                    x1: 550.0,
                    y1: 300.0,
                },
                false,
            )],
        );
        assert!(matches!(decide(&p).0, Route::Deterministic));
    }

    #[test]
    fn a_texty_raster_region_the_glyphs_cannot_read_routes() {
        // Same geometry, but the pixels carry text structure and the glyph
        // layer has a void there: a raster table.
        let p = page_with(
            1500,
            0,
            vec![(
                BBox {
                    x0: 50.0,
                    y0: 50.0,
                    x1: 550.0,
                    y1: 300.0,
                },
                true,
            )],
        );
        assert!(matches!(decide(&p).0, Route::VlmRegions(_)));
    }

    #[test]
    fn broken_cid_text_routes_to_the_vlm() {
        let p = page_with(300, 300, Vec::new());
        assert!(matches!(decide(&p).0, Route::Vlm(Reason::BrokenText)));
    }

    #[test]
    fn fully_unresolved_text_routes_to_the_vlm() {
        // Every glyph event unresolved and no images: the page is full of ink
        // the text layer cannot read. Silent empty output is the one wrong
        // answer.
        let p = page_with(0, 300, Vec::new());
        assert!(matches!(decide(&p).0, Route::Vlm(Reason::BrokenText)));
    }

    #[test]
    fn an_ordinary_text_page_stays_deterministic() {
        let p = page_with(1500, 10, Vec::new());
        assert!(matches!(decide(&p).0, Route::Deterministic));
    }

    #[test]
    fn a_vector_only_drawing_stays_deterministic() {
        // CAD: no images, few glyphs — the VLM loses dimension text (§9.4);
        // sparse vector pages must not route.
        let p = page_with(40, 0, Vec::new());
        assert!(matches!(decide(&p).0, Route::Deterministic));
    }
}
