//! How an image is sized for the OCR engine: one plan per image, decided on
//! its size alone.
//!
//! A local OCR engine's cost is this machine's time and memory, and both
//! grow with the pixels its detector reads in one pass (about 200 bytes a
//! pixel). So an image is read at the resolution it has, in passes of at
//! most [`OcrSizing::max_pixels`] each:
//!
//! - one pass, the whole image, when it fits;
//! - else horizontal strips across its full width, overlapping, each within
//!   the cap: a tall capture or a long scroll;
//! - else, an image too wide for a strip at least as tall as wide to fit,
//!   the whole image shrunk until it fits, and a notice says so.
//!
//! The detector sees small text better larger: an image whose short side is
//! under [`DET_SHORT_SIDE`] is read up to that, at most [`MAX_UPSCALE`]
//! times larger. The engine does the enlarging, inside its detector, so the
//! recognizer still reads each line from the image's own pixels: the image
//! is padded with white paper to the short side that makes the detector's
//! own sizing (up to [`DET_SHORT_SIDE`]) come out at the scale chosen here.
//!
//! This is the OCR half of how a crop is sized. A model reader's crops are
//! sized for what a call costs, not for this machine, and are rendered at a
//! fixed scale (`fluree_doc_pdf::escalate::CropScale::Model`).

/// The default of [`OcrSizing::max_pixels`], in megapixels: a Letter, A4
/// or Legal page scanned at 300 dpi (8.4, 8.7 and 10.7 Mpx) and a 12 Mpx
/// phone photo (3,024 × 4,032) are each read in one pass.
///
/// Measured against 8 Mpx: a lower cap cuts those pages and photos in two
/// strips, which reads them more slowly and lost text on a corpus of scanned
/// pages; it does not lower the peak memory of a reading much, as a strip
/// costs what a whole image of its size does.
pub const DEFAULT_MAX_MEGAPIXELS: f64 = 12.5;

/// The least cap one may set, in megapixels: a narrow image padded to the
/// detector's short side ([`DET_SHORT_SIDE`]) still fits a strip at least
/// as tall as it is wide.
pub const MIN_MEGAPIXELS: f64 = 1.0;

/// The most, in megapixels: an image file holds at most 128 Mi pixels.
pub const MAX_MEGAPIXELS: f64 = 128.0;

/// The short side, in the detector's pixels, an image is enlarged toward:
/// PP-OCR's detector misses thin lines below it.
pub const DET_SHORT_SIDE: u32 = 736;

/// The most an image is enlarged for the detector. Measured on narrow
/// crops of 8 to 10 px text (1,200 × 40 and the like): twice as large reads
/// nearly all the lines an unbounded enlargement does, which can take a
/// strip of a few pixels to many times its size.
pub const MAX_UPSCALE: f64 = 2.0;

/// The share of a strip the next one overlaps: a line cut by one strip's
/// edge is whole in the next.
const OVERLAP: u32 = 5;

/// How the OCR engine sizes the images it reads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OcrSizing {
    max_pixels: u64,
}

impl Default for OcrSizing {
    fn default() -> Self {
        OcrSizing::megapixels(DEFAULT_MAX_MEGAPIXELS).expect("the default is in range")
    }
}

impl OcrSizing {
    /// At most `mpx` megapixels a pass, between [`MIN_MEGAPIXELS`] and
    /// [`MAX_MEGAPIXELS`].
    pub fn megapixels(mpx: f64) -> Result<OcrSizing, String> {
        if !(MIN_MEGAPIXELS..=MAX_MEGAPIXELS).contains(&mpx) {
            return Err(format!(
                "{mpx} megapixels: an OCR pass holds from {MIN_MEGAPIXELS} to {MAX_MEGAPIXELS}"
            ));
        }
        Ok(OcrSizing {
            max_pixels: (mpx * 1e6) as u64,
        })
    }

    /// The most pixels the detector reads in one pass.
    pub fn max_pixels(&self) -> u64 {
        self.max_pixels
    }

    /// The longest side the detector may be given: a pass within the cap,
    /// its short side at least [`DET_SHORT_SIDE`], is no longer than this.
    pub fn max_side(&self) -> u32 {
        (self.max_pixels / DET_SHORT_SIDE as u64) as u32 + 64
    }

    /// How an image of `width` × `height` is read.
    pub fn plan(&self, width: u32, height: u32) -> OcrPlan {
        let (w, h) = (width.max(1), height.max(1));
        let cap = self.max_pixels as f64;
        let whole = Pass::rows(w, 0, h, upscale(w.min(h)));
        if whole.det_pixels() <= cap {
            return OcrPlan {
                width: w,
                height: h,
                shrunk: None,
                passes: vec![whole],
            };
        }
        // Strips across the full width, at least as tall as wide: the
        // image's width is their short side.
        let up = upscale(w);
        let across = f64::from(w.max(pad_to(up)));
        let len = (cap / (across * up * up)).floor() as u32;
        if len >= w.max(pad_to(up)) && len < h {
            let step = len - len / OVERLAP;
            let mut passes = Vec::new();
            let mut top = 0;
            while top + len < h {
                passes.push(Pass::rows(w, top, top + len, up));
                top += step;
            }
            passes.push(Pass::rows(w, h - len, h, up));
            return OcrPlan {
                width: w,
                height: h,
                shrunk: None,
                passes,
            };
        }
        // Too wide for strips: the whole image, not enlarged, if that fits.
        let flat = Pass::rows(w, 0, h, 1.0);
        if flat.det_pixels() <= cap {
            return OcrPlan {
                width: w,
                height: h,
                shrunk: None,
                passes: vec![flat],
            };
        }
        // Else shrunk until it fits. A short side shrunk under the
        // detector's is padded back to it.
        let (fw, fh) = (f64::from(w), f64::from(h));
        let mut f = (cap / (fw * fh)).sqrt();
        if fw.min(fh) * f < f64::from(DET_SHORT_SIDE) {
            f = cap / (f64::from(DET_SHORT_SIDE) * fw.max(fh));
        }
        let (sw, sh) = (
            ((fw * f).floor() as u32).max(1),
            ((fh * f).floor() as u32).max(1),
        );
        OcrPlan {
            width: sw,
            height: sh,
            shrunk: Some(f),
            passes: vec![Pass::rows(sw, 0, sh, 1.0)],
        }
    }
}

/// How the detector enlarges an image whose short side is `short` pixels:
/// toward [`DET_SHORT_SIDE`], at most [`MAX_UPSCALE`] times.
fn upscale(short: u32) -> f64 {
    (f64::from(DET_SHORT_SIDE) / f64::from(short.max(1))).clamp(1.0, MAX_UPSCALE)
}

/// The short side an image is padded to for the detector to enlarge it
/// `up` times: the detector takes a shorter side up to [`DET_SHORT_SIDE`].
fn pad_to(up: f64) -> u32 {
    (f64::from(DET_SHORT_SIDE) / up).ceil() as u32
}

/// How an image is read: its passes, over the image as [`OcrPlan::width`]
/// × [`OcrPlan::height`] give it.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrPlan {
    /// The image's size as read: its own, or shrunk ([`OcrPlan::shrunk`]).
    pub width: u32,
    pub height: u32,
    /// The scale the image is shrunk by before it is read, when it is too
    /// wide to read within the cap otherwise.
    pub shrunk: Option<f64>,
    /// One pass, or strips top to bottom, each overlapping the next.
    pub passes: Vec<Pass>,
}

/// One detector pass: rows `top..bottom` of the image, padded with white to
/// `canvas` (the image's rows at its top left), which the detector enlarges
/// `up` times.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pass {
    pub top: u32,
    pub bottom: u32,
    pub canvas: (u32, u32),
    pub up: f64,
}

impl Pass {
    fn rows(width: u32, top: u32, bottom: u32, up: f64) -> Pass {
        // Both sides at least the short side the detector takes up to
        // [`DET_SHORT_SIDE`]: it then enlarges the canvas `up` times.
        let short = pad_to(up);
        let canvas = (width.max(short), (bottom - top).max(short));
        Pass {
            top,
            bottom,
            canvas,
            up,
        }
    }

    /// The pixels the detector reads in this pass: what its memory grows
    /// with.
    pub fn det_pixels(&self) -> f64 {
        f64::from(self.canvas.0) * f64::from(self.canvas.1) * self.up * self.up
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(mpx: f64) -> OcrSizing {
        OcrSizing::megapixels(mpx).unwrap()
    }

    #[test]
    fn a_page_or_a_photo_is_read_whole_at_its_own_size() {
        let s = OcrSizing::default();
        for (w, h) in [
            (2550, 3300),
            (2480, 3508),
            (2550, 4200),
            (3024, 4032),
            (4032, 3024),
        ] {
            let p = s.plan(w, h);
            assert_eq!(p.passes.len(), 1, "{w} × {h}");
            assert_eq!(p.shrunk, None);
            assert_eq!(p.passes[0].canvas, (w, h), "not padded");
            assert_eq!(p.passes[0].up, 1.0, "not enlarged");
        }
    }

    #[test]
    fn a_narrow_crop_is_enlarged_at_most_twice() {
        let s = OcrSizing::default();
        // 40 rows of text: twice, padded to 368 rows so the detector's 736
        // comes out at twice.
        let p = s.plan(1200, 40);
        assert_eq!(p.passes[0].up, 2.0);
        assert_eq!(p.passes[0].canvas, (1200, 368));
        // 500 rows: 736 / 500.
        let p = s.plan(1200, 500);
        assert!((p.passes[0].up - 1.472).abs() < 1e-9);
        assert_eq!(p.passes[0].canvas, (1200, 500));
        // A narrow receipt: across.
        let p = s.plan(400, 3000);
        assert!((p.passes[0].up - 1.84).abs() < 1e-9);
        assert_eq!(p.passes[0].canvas, (400, 3000));
    }

    #[test]
    fn a_tall_capture_is_read_in_overlapping_strips_within_the_cap() {
        let s = OcrSizing::default();
        let p = s.plan(2400, 9000);
        assert_eq!(p.shrunk, None);
        assert!(p.passes.len() > 1);
        assert_eq!(p.passes[0].top, 0);
        assert_eq!(p.passes.last().unwrap().bottom, 9000);
        for w in p.passes.windows(2) {
            assert!(w[1].top < w[0].bottom, "each overlaps the next");
        }
        for pass in &p.passes {
            assert!(pass.det_pixels() <= 12.5e6, "{pass:?}");
            assert!(pass.bottom - pass.top >= 2400, "at least as tall as wide");
        }
        // A narrow one is enlarged in its strips as it would be whole.
        let p = s.plan(400, 40_000);
        assert!(p.passes.len() > 1);
        assert!(p.passes.iter().all(|q| (q.up - 1.84).abs() < 1e-9));
        assert!(p.passes.iter().all(|q| q.det_pixels() <= 12.5e6));
    }

    #[test]
    fn a_page_just_over_the_cap_is_cut_and_a_lower_cap_cuts_more() {
        // The phone photo is 12.19 Mpx: two strips at 12, one at 12.5.
        assert_eq!(at(12.0).plan(3024, 4032).passes.len(), 2);
        assert_eq!(at(12.5).plan(3024, 4032).passes.len(), 1);
        assert!(at(8.0).plan(2550, 3300).passes.len() > 1);
    }

    #[test]
    fn an_image_too_wide_for_a_strip_is_shrunk_to_fit() {
        let s = OcrSizing::default();
        // A 600 dpi Letter page: a strip as tall as wide is 33.7 Mpx.
        let p = s.plan(5100, 6600);
        let f = p.shrunk.expect("shrunk");
        assert!(f < 1.0);
        assert_eq!(p.passes.len(), 1);
        assert!(p.passes[0].det_pixels() <= 12.5e6);
        assert_eq!(
            (p.width, p.height),
            (p.passes[0].canvas.0, p.passes[0].canvas.1)
        );
        // A wide panorama in one row of text.
        let p = s.plan(16_000, 1200);
        assert!(p.shrunk.is_some());
        assert!(p.passes[0].det_pixels() <= 12.5e6);
        // A strip 65,535 wide and 10 tall: shrunk so that, padded to the
        // detector's short side, it fits.
        let p = s.plan(65_535, 10);
        assert!(p.passes[0].det_pixels() <= 12.5e6, "{p:?}");
        assert_eq!(p.passes[0].up, 1.0);
    }

    #[test]
    fn every_pass_fits_the_detector() {
        for mpx in [1.0, 4.0, 12.5, 40.0] {
            let s = at(mpx);
            for (w, h) in [
                (1, 1),
                (10, 65_535),
                (65_535, 10),
                (736, 736),
                (3000, 3000),
                (5000, 5000),
                (9000, 1200),
                (1200, 9000),
                (65_535, 2048),
            ] {
                let p = s.plan(w, h);
                for q in &p.passes {
                    assert!(
                        q.det_pixels() <= s.max_pixels() as f64 * 1.000_001,
                        "{mpx} {w}×{h} {q:?}"
                    );
                    let long = f64::from(q.canvas.0.max(q.canvas.1)) * q.up;
                    assert!(long <= f64::from(s.max_side()), "{mpx} {w}×{h} {q:?}");
                    assert!(q.bottom <= p.height && q.top < q.bottom);
                }
            }
        }
    }

    #[test]
    fn the_cap_is_bounded() {
        assert!(OcrSizing::megapixels(0.5).is_err());
        assert!(OcrSizing::megapixels(f64::NAN).is_err());
        assert!(OcrSizing::megapixels(200.0).is_err());
        assert_eq!(OcrSizing::default().max_pixels(), 12_500_000);
    }
}
