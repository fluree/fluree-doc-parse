//! Renderer probe: correctness + speed of hayro::render() on real PDFs.
use hayro::{RenderCache, RenderSettings, render};
use hayro::vello_cpu::color::{AlphaColor, Srgb};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use std::sync::Arc;
use std::time::Instant;

pub fn render_page(path: &str, page_idx: usize, scale: f32, out: Option<&str>) -> (u32, u32, f64, usize) {
    let data = std::fs::read(path).unwrap();
    let pdf = Pdf::new(Arc::new(data)).unwrap();
    let pages = pdf.pages();
    let page = &pages[page_idx];
    let cache = RenderCache::new();
    // White, not the TRANSPARENT default: a transparent background flattens to
    // black in any RGB consumer, which makes black text invisible. Cost us a
    // full VLM run to learn.
    let white: AlphaColor<Srgb> = AlphaColor::new([1.0, 1.0, 1.0, 1.0]);
    let settings = RenderSettings { x_scale: scale, y_scale: scale, bg_color: white, ..Default::default() };
    let t0 = Instant::now();
    let pix = render(page, &cache, &InterpreterSettings::default(), &settings);
    let secs = t0.elapsed().as_secs_f64();
    let (w, h) = (pix.width() as u32, pix.height() as u32);
    let rgba = pix.take_unpremultiplied();
    // Count non-white pixels as a crude "did anything actually draw" check.
    let ink = rgba.iter().filter(|p| p.a > 0 && (p.r < 250 || p.g < 250 || p.b < 250)).count();
    if let Some(o) = out {
        let file = std::fs::File::create(o).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().unwrap();
        let flat: Vec<u8> = rgba.iter().flat_map(|p| [p.r, p.g, p.b, p.a]).collect();
        wr.write_image_data(&flat).unwrap();
    }
    (w, h, secs, ink)
}
