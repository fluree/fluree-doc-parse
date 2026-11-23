//! Rasterising a page, so pixels and coordinates come from one parse.
//!
//! A highlight is a rectangle in PDF user units and an image is pixels, and
//! the two only line up if the same code produced both. Pairing these
//! coordinates with a second PDF renderer means reconciling two
//! implementations that were never guaranteed to agree — the failure mode is
//! a highlight that drifts further down the page the further you scroll, and
//! it looks like a CSS bug rather than a rendering one.
//!
//! Behind the `render` feature: a consumer that only wants elements should
//! not build a rasteriser.

use hayro::vello_cpu::color::{AlphaColor, Srgb};
use hayro::{render, RenderCache, RenderSettings};
use hayro_syntax::Pdf;

/// Default oversampling. Measured against the deep reader: 1x, 1.5x and 2x
/// all bill the same input tokens because the API resizes below a threshold,
/// and 3x costs two and a half times as many and reads *worse*. It is also a
/// sensible screen density, which is why one number serves both.
pub const SCALE: f32 = 2.0;

/// A rendered page: RGBA pixels and the size they were drawn at.
pub struct Raster {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA, four bytes per pixel.
    pub rgba: Vec<u8>,
    /// What the PDF units were multiplied by. Divide a pixel coordinate by
    /// this to get back to the space `doc:bbox` is in.
    pub scale: f32,
}

/// The longest side a render may take, in pixels: 32,767. The renderer
/// sizes its canvas in 16 bits and pads it: a canvas 65,528 pixels wide
/// (a page 65,528 points wide at 1x, three high) asked for 16 GiB and
/// aborted the process, where 65,527 drew in 12 MB. Half of that leaves
/// room for whatever else in the renderer rounds a side up, and still
/// draws the largest page the format allows (14,400 points) at 2x whole.
pub const MAX_RENDER_SIDE: u32 = 32_767;

/// The scale a page of `width` by `height` points is drawn at when `scale`
/// is asked for: `scale`, or, where that would draw more than `max_pixels`
/// or a side longer than [`MAX_RENDER_SIDE`], the largest that draws no
/// more. The pixels are counted as the renderer sizes its canvas, each side
/// multiplied in `f32` and rounded down, so the canvas drawn at the result
/// is within both.
///
/// A 3000 x 3000 pt page asked for at 2x is 36 million pixels; a 6000 pt
/// one 144 million, 576 MB of RGBA held at once. A page past the limit is
/// drawn smaller, never cut: a model, or an OCR engine, still sees all of
/// it.
pub fn fitted_scale(width: f64, height: f64, scale: f32, max_pixels: u64) -> f32 {
    fit(width, height, scale, max_pixels, MAX_RENDER_SIDE)
}

/// What holds a page below the scale asked for ([`fitted_scale`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shrunk {
    /// The pixels a render may take
    /// ([`Limits::render_pixels`](fluree_doc_model::budget::Limits::render_pixels)).
    Pixels,
    /// The longest side a render may take, [`MAX_RENDER_SIDE`].
    Side,
}

/// Why a page of `width` by `height` points is drawn below `scale`, if it
/// is: the pixels where, within them alone, it is drawn at the scale that
/// fits both; else the longest side.
pub fn shrunk_by(width: f64, height: f64, scale: f32, max_pixels: u64) -> Option<Shrunk> {
    let fitted = fitted_scale(width, height, scale, max_pixels);
    if fitted == scale || fitted.is_nan() {
        return None;
    }
    if fit(width, height, scale, max_pixels, u32::MAX) == fitted {
        Some(Shrunk::Pixels)
    } else {
        Some(Shrunk::Side)
    }
}

/// [`shrunk_by`] for `page`, within the limits in effect.
pub fn page_shrunk_by(page: &hayro_syntax::page::Page<'_>, scale: f32) -> Option<Shrunk> {
    let (w, h) = page.render_dimensions();
    let max = fluree_doc_model::budget::limits().render_pixels;
    shrunk_by(f64::from(w), f64::from(h), scale, max)
}

/// [`fitted_scale`] with a longest side of `max_side`.
fn fit(width: f64, height: f64, scale: f32, max_pixels: u64, max_side: u32) -> f32 {
    let (w, h) = (width as f32, height as f32);
    let side = |n: f32, s: f32| f64::from((n * s).floor());
    let fits = |s: f32| {
        let (pw, ph) = (side(w, s), side(h, s));
        pw * ph <= max_pixels as f64 && pw <= f64::from(max_side) && ph <= f64::from(max_side)
    };
    // NaN, an empty page or no scale: drawn as asked.
    let positive = |x: f32| x > 0.0;
    if !positive(w * h) || !positive(scale) || fits(scale) {
        return scale;
    }
    // The largest scale that fits, found over the bits of positive floats,
    // which order as the floats do: the canvas grows with the scale.
    let (mut lo, mut hi) = (0u32, scale.to_bits());
    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        if fits(f32::from_bits(mid)) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    f32::from_bits(lo)
}

/// The scale `page` is drawn at when `scale` is asked for, within the
/// pixels the [`limits`](fluree_doc_model::budget::limits) in effect allow
/// a render ([`fitted_scale`]).
pub fn page_scale(page: &hayro_syntax::page::Page<'_>, scale: f32) -> f32 {
    let (w, h) = page.render_dimensions();
    let max = fluree_doc_model::budget::limits().render_pixels;
    fitted_scale(f64::from(w), f64::from(h), scale, max)
}

/// Rasterise one page, at `scale` or, for a page that would draw more
/// pixels than a render may take, at the scale that fits ([`page_scale`]):
/// [`Raster::scale`] says which. `None` when the index is past the end.
pub fn page(pdf: &Pdf, index: usize, scale: f32) -> Option<Raster> {
    let pages = pdf.pages();
    let page = pages.get(index)?;
    let scale = page_scale(page, scale);
    let white: AlphaColor<Srgb> = AlphaColor::new([1.0, 1.0, 1.0, 1.0]);
    let settings = RenderSettings {
        x_scale: scale,
        y_scale: scale,
        bg_color: white,
        ..Default::default()
    };
    let pix = render(
        page,
        &RenderCache::new(),
        &hayro::hayro_interpret::InterpreterSettings::default(),
        &settings,
    );
    let (width, height) = (pix.width() as u32, pix.height() as u32);
    Some(Raster {
        width,
        height,
        rgba: pix
            .take_unpremultiplied()
            .iter()
            .flat_map(|p| [p.r, p.g, p.b, p.a])
            .collect(),
        scale,
    })
}

impl Raster {
    /// Encode as a PNG.
    pub fn to_png(&self) -> Option<Vec<u8>> {
        self.crop_to_png(0, 0, self.width, self.height)
    }

    /// Encode a sub-rectangle, in *pixels* — multiply a `doc:bbox` by
    /// [`Raster::scale`] to get here.
    pub fn crop_to_png(&self, x0: u32, y0: u32, x1: u32, y1: u32) -> Option<Vec<u8>> {
        let (w, h) = (x1.checked_sub(x0)?, y1.checked_sub(y0)?);
        if w == 0 || h == 0 || x1 > self.width || y1 > self.height {
            return None;
        }
        let mut buf = Vec::with_capacity(w as usize * h as usize * 4);
        for row in y0..y1 {
            let start = (row as usize * self.width as usize + x0 as usize) * 4;
            buf.extend_from_slice(self.rgba.get(start..start + w as usize * 4)?);
        }
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().ok()?.write_image_data(&buf).ok()?;
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_past_the_pixels_a_render_may_take_is_drawn_smaller() {
        // A letter page at 2x, and a 3000 pt one, are drawn as asked.
        assert_eq!(fitted_scale(612.0, 792.0, 2.0, 64_000_000), 2.0);
        assert_eq!(fitted_scale(3000.0, 3000.0, 2.0, 64_000_000), 2.0);
        // Held to 16 million pixels, the 3000 pt page is drawn at 4/3, 4,000
        // pixels a side (the largest scale that rounds down to it).
        let s = fitted_scale(3000.0, 3000.0, 2.0, 16_000_000);
        assert!((f64::from(s) - 4.0 / 3.0).abs() < 1e-3, "{s}");
        assert_eq!((3000.0f32 * s).floor(), 4000.0);
        // As the renderer sizes its canvas, in f32, never past the limit,
        // and within a pixel a side of it.
        for (w, h, max) in [
            (6000.0, 6000.0, 64_000_000),
            (14_400.0, 300.0, 1_000_003),
            (1.0e5, 7.0, 999),
            (4096.1, 4095.9, 16_777_216),
            (3333.3, 4444.4, 9_999_991),
        ] {
            let s = fitted_scale(w, h, 2.0, max);
            let px = |n: f64| f64::from((n as f32 * s).floor());
            assert!(px(w) * px(h) <= max as f64, "{w}x{h}: {s}");
            let up = f32::from_bits(s.to_bits() + 1);
            let px_up = |n: f64| f64::from((n as f32 * up).floor());
            assert!(
                px_up(w) * px_up(h) > max as f64,
                "{w}x{h}: {s} not the largest"
            );
        }
        // An empty page is not scaled.
        assert_eq!(fitted_scale(0.0, 792.0, 2.0, 1), 2.0);
    }

    #[test]
    fn no_side_is_drawn_past_what_the_renderer_draws() {
        // 32,764 x 3 pt at 2x is 393,000 pixels, well within the pixels,
        // and 65,528 a side, which aborted the renderer: drawn at the
        // scale that fits the side.
        let s = fitted_scale(32_764.0, 3.0, 2.0, 64_000_000);
        assert!(s < 1.0002, "{s}");
        assert_eq!((32_764.0f32 * s).floor(), MAX_RENDER_SIDE as f32);
        assert_eq!(fitted_scale(16_383.0, 3.0, 2.0, 64_000_000), 2.0);
        // The largest page the format allows, at 2x, whole.
        assert_eq!(fitted_scale(14_400.0, 14_400.0, 2.0, u64::MAX), 2.0);
        let s = fitted_scale(3.0, 40_000.0, 1.0, 64_000_000);
        assert_eq!((40_000.0f32 * s).floor(), MAX_RENDER_SIDE as f32);
        // Drawn so, rather than aborting the process.
        let r = page(&sized_page(32_764, 3), 0, 2.0).unwrap();
        assert_eq!((r.width, r.height), (MAX_RENDER_SIDE, 3));
        // Said to be held by the side, not the pixels; each by what holds
        // it.
        assert_eq!(
            shrunk_by(32_764.0, 3.0, 2.0, 64_000_000),
            Some(Shrunk::Side)
        );
        assert_eq!(
            shrunk_by(3000.0, 3000.0, 2.0, 16_000_000),
            Some(Shrunk::Pixels)
        );
        assert_eq!(
            shrunk_by(40_000.0, 40_000.0, 2.0, 1_000_000),
            Some(Shrunk::Pixels)
        );
        assert_eq!(shrunk_by(612.0, 792.0, 2.0, 64_000_000), None);
    }

    #[test]
    fn a_render_records_the_scale_it_was_drawn_at() {
        use fluree_doc_model::budget::{limited_to, Limits};
        let pdf = big_page(3000);
        let r = page(&pdf, 0, 2.0).unwrap();
        assert_eq!((r.width, r.height, r.scale), (6000, 6000, 2.0));
        let mut small = Limits::default();
        small.render_pixels = 16_000_000;
        let r = limited_to(small, || page(&pdf, 0, 2.0)).unwrap();
        assert!(u64::from(r.width) * u64::from(r.height) <= 16_000_000);
        assert_eq!(r.width, (3000.0 * r.scale) as u32);
        assert_eq!(r.rgba.len(), r.width as usize * r.height as usize * 4);
    }

    #[test]
    fn a_crop_of_a_page_past_the_limit_is_cut_at_the_scale_it_was_drawn_at() {
        use fluree_doc_model::budget::{limited_to, Limits};
        let pdf = big_page(3000);
        let region = crate::BBox {
            x0: 1500.0,
            y0: 1500.0,
            x1: 1600.0,
            y1: 1550.0,
        };
        let jobs: crate::escalate::CropJobs =
            vec![(0, None), (0, Some(vec![("r0".to_string(), region)]))];
        let crops = |max: u64| {
            let mut l = Limits::default();
            l.render_pixels = max;
            let mut out = Vec::new();
            limited_to(l, || {
                crate::escalate::for_each_crop(&pdf, &jobs, |c| {
                    out.push((c.scale, c.pixels.width, c.pixels.height))
                })
            });
            out
        };
        // Within the default, at the model's 2x.
        let at_2x = crops(64_000_000);
        assert_eq!(at_2x[0], (2.0, 6000, 6000));
        assert_eq!(at_2x[1].0, 2.0);
        // Held to a million pixels, the page and the region are both cut
        // from the render that fits, and say its scale.
        let small = crops(1_000_000);
        let s = small[0].0;
        assert!(s < 0.34 && s > 0.33, "{s}");
        assert!(u64::from(small[0].1) * u64::from(small[0].2) <= 1_000_000);
        assert_eq!(small[1].0, s);
        // The region's width, its margins included, at that scale.
        let want = at_2x[1].1 as f32 * s / 2.0;
        assert!((small[1].1 as f32 - want).abs() <= 2.0, "{small:?} {want}");
    }

    /// A one-page PDF `side` points square.
    fn big_page(side: u32) -> Pdf {
        sized_page(side, side)
    }

    /// A one-page PDF of `w` by `h` points.
    fn sized_page(w: u32, h: u32) -> Pdf {
        let mut out = b"%PDF-1.4\n".to_vec();
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] >>"),
        ];
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(b"xref\n0 4\n0000000000 65535 f \n");
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").bytes());
        }
        out.extend(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").bytes(),
        );
        Pdf::new(std::sync::Arc::new(out)).unwrap()
    }
}
