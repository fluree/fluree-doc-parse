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

/// Rasterise one page. `None` when the index is past the end.
pub fn page(pdf: &Pdf, index: usize, scale: f32) -> Option<Raster> {
    let pages = pdf.pages();
    let page = pages.get(index)?;
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
