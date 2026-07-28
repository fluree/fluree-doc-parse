//! Turning the requested crops into PNG bytes.
//!
//! Shared by the two consumers so a crop written to disk and a crop sent to a
//! reader are the same pixels. The scale is fixed: measured, 1×, 1.5× and 2×
//! all bill the same input tokens because the API resizes below a threshold,
//! and 3× costs two and a half times as many and reads *worse*.

use crate::commands::common::{CROP_MARGIN, VLM_RENDER_SCALE};
use crate::escalate::jobs::CropJobs;
use hayro::vello_cpu::color::{AlphaColor, Srgb};
use hayro::{render, RenderCache, RenderSettings};
use hayro_syntax::Pdf;

/// One rendered crop, named the way the splice will ask for it.
pub(crate) struct Crop {
    /// `p{page}_{tag}` — the crop name a reading is filed under.
    pub name: String,
    pub page: usize,
    /// The region on the page, or `None` for a whole page.
    pub bbox: Option<fluree_doc_pdf::geom::BBox>,
    pub png: Vec<u8>,
}

impl Crop {
    /// Whole pages carry a document's structure and are prompted differently.
    pub fn is_page(&self) -> bool {
        self.name.ends_with("_full")
    }

    /// Tables are transcribed as markup rather than prose.
    pub fn is_table(&self) -> bool {
        self.name
            .rsplit_once('_')
            .is_some_and(|(_, tag)| tag.starts_with('t'))
    }
}

/// Render every job to PNG bytes, in page order.
pub(crate) fn render_crops(pdf: &Pdf, jobs: &CropJobs) -> Vec<Crop> {
    let pages = pdf.pages();
    let cache = RenderCache::new();
    let white: AlphaColor<Srgb> = AlphaColor::new([1.0, 1.0, 1.0, 1.0]);
    let settings = RenderSettings {
        x_scale: VLM_RENDER_SCALE,
        y_scale: VLM_RENDER_SCALE,
        bg_color: white,
        ..Default::default()
    };
    let mut out = Vec::new();
    for (page_idx, regions) in jobs {
        let Some(page) = pages.get(*page_idx) else {
            continue;
        };
        let pix = render(
            page,
            &cache,
            &hayro::hayro_interpret::InterpreterSettings::default(),
            &settings,
        );
        let (w, h) = (pix.width() as usize, pix.height() as usize);
        let rgba: Vec<u8> = pix
            .take_unpremultiplied()
            .iter()
            .flat_map(|p| [p.r, p.g, p.b, p.a])
            .collect();
        match regions {
            None => {
                if let Some(png) = encode(&rgba, w, 0, 0, w, h) {
                    out.push(Crop {
                        name: format!("p{page_idx}_full"),
                        page: *page_idx,
                        bbox: None,
                        png,
                    });
                }
            }
            Some(regions) => {
                for (tag, b) in regions {
                    let sc = VLM_RENDER_SCALE as f64;
                    let x0 = (((b.x0 - CROP_MARGIN) * sc).floor().max(0.0)) as usize;
                    let y0 = (((b.y0 - CROP_MARGIN) * sc).floor().max(0.0)) as usize;
                    let x1 = ((((b.x1 + CROP_MARGIN) * sc).ceil()) as usize).min(w);
                    let y1 = ((((b.y1 + CROP_MARGIN) * sc).ceil()) as usize).min(h);
                    if x1 <= x0 || y1 <= y0 {
                        continue;
                    }
                    if let Some(png) = encode(&rgba, w, x0, y0, x1, y1) {
                        out.push(Crop {
                            name: format!("p{page_idx}_{tag}"),
                            page: *page_idx,
                            bbox: Some(*b),
                            png,
                        });
                    }
                }
            }
        }
    }
    out
}

/// Encode a sub-rectangle of an RGBA buffer as a PNG.
fn encode(
    rgba: &[u8],
    width: usize,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) -> Option<Vec<u8>> {
    let (cw, ch) = (x1.checked_sub(x0)?, y1.checked_sub(y0)?);
    if cw == 0 || ch == 0 {
        return None;
    }
    let mut buf = Vec::with_capacity(cw * ch * 4);
    for row in y0..y1 {
        let s = (row * width + x0) * 4;
        buf.extend_from_slice(rgba.get(s..s + cw * 4)?);
    }
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, cw as u32, ch as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().ok()?.write_image_data(&buf).ok()?;
    }
    Some(png)
}
