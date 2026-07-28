//! Batch robustness/quality probe for hayro as an extraction foundation.
//! Per document: pages, glyphs, unicode-resolution rate, wall time, failures.
use hayro_interpret::font::Glyph;
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::util::TransformExt;
use hayro_interpret::{Context, Device, InterpreterCache, InterpreterSettings, interpret_page};
use kurbo::{Affine, Rect, Shape};
use std::sync::Arc;
use std::time::Instant;
mod render;

#[derive(Default)]
struct Stats { glyphs: usize, unicode: usize, boxed: usize, ligatures: usize, chars: usize }

struct Collector { s: Stats }

impl<'a> Device<'a> for Collector {
    fn set_soft_mask(&mut self, _m: Option<hayro_interpret::SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _b: hayro_interpret::BlendMode) {}
    fn draw_path(&mut self, _p: &kurbo::BezPath, _t: Affine, _pa: &hayro_interpret::Paint<'a>, _d: &hayro_interpret::PathDrawMode) {}
    fn push_clip_path(&mut self, _c: &hayro_interpret::ClipPath) {}
    fn push_transparency_group(&mut self, _o: f32, _m: Option<hayro_interpret::SoftMask<'a>>, _b: hayro_interpret::BlendMode) {}
    fn draw_image(&mut self, _i: hayro_interpret::Image<'a, '_>, _t: Affine) {}
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_glyph(&mut self, glyph: &Glyph<'a>, transform: Affine, glyph_transform: Affine,
                  _paint: &hayro_interpret::Paint<'a>, _mode: &hayro_interpret::GlyphDrawMode) {
        self.s.glyphs += 1;
        let text = match glyph.as_unicode() {
            Some(hayro_interpret::hayro_cmap::BfString::Char(c)) => c.to_string(),
            Some(hayro_interpret::hayro_cmap::BfString::String(s)) => s,
            None => String::new(),
        };
        if !text.is_empty() { self.s.unicode += 1; }
        self.s.chars += text.chars().count();
        self.s.ligatures += text.chars().filter(|c| matches!(c, '\u{FB00}'..='\u{FB06}')).count();
        if let Glyph::Outline(o) = glyph {
            let r = ((transform * glyph_transform) * o.outline()).bounding_box();
            if r.width() > 0.0 && r.height() > 0.0 { self.s.boxed += 1; }
        }
    }
}

fn run(path: &str) -> Result<(usize, Stats, f64), String> {
    let data = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    let t0 = Instant::now();
    let pdf = Pdf::new(Arc::new(data)).map_err(|e| format!("parse: {e:?}"))?;
    let pages = pdf.pages();
    let mut dev = Collector { s: Stats::default() };
    let cache = InterpreterCache::new();
    for page in pages.iter() {
        let mut ctx = Context::new(page.initial_transform(true).to_kurbo(),
            Rect::new(0.0, 0.0, 20000.0, 20000.0), &cache, page.xref(), InterpreterSettings::default());
        interpret_page(page, &mut ctx, &mut dev);
    }
    Ok((pages.len(), dev.s, t0.elapsed().as_secs_f64()))
}

fn dump(path: &str) {
    let data = std::fs::read(path).unwrap();
    let pdf = Pdf::new(Arc::new(data)).unwrap();
    let pages = pdf.pages();
    let cache = InterpreterCache::new();
    let mut text = String::new();
    for page in pages.iter() {
        let mut d = TextDump { out: String::new() };
        let mut ctx = Context::new(page.initial_transform(true).to_kurbo(),
            Rect::new(0.0,0.0,20000.0,20000.0), &cache, page.xref(), InterpreterSettings::default());
        interpret_page(page, &mut ctx, &mut d);
        text.push_str(&d.out); text.push('\n');
    }
    let cjk = text.chars().filter(|c| matches!(c, '\u{4E00}'..='\u{9FFF}'|'\u{3040}'..='\u{30FF}'|'\u{AC00}'..='\u{D7AF}')).count();
    let repl = text.chars().filter(|&c| c=='\u{FFFD}').count();
    println!("chars={} cjk={} replacement={}", text.chars().count(), cjk, repl);
    let snippet: String = text.chars().filter(|c| !c.is_whitespace()).take(160).collect();
    println!("sample: {}", snippet);
}

fn dump_pages(path: &str) {
    let data = std::fs::read(path).unwrap();
    let pdf = Pdf::new(Arc::new(data)).unwrap();
    let pages = pdf.pages();
    let cache = InterpreterCache::new();
    for (i, page) in pages.iter().enumerate() {
        let mut d = TextDump { out: String::new() };
        let mut ctx = Context::new(page.initial_transform(true).to_kurbo(),
            Rect::new(0.0,0.0,20000.0,20000.0), &cache, page.xref(), InterpreterSettings::default());
        interpret_page(page, &mut ctx, &mut d);
        let t: String = d.out.chars().filter(|c| !c.is_whitespace()).collect();
        println!("p{:<3} chars={:<6} {}", i, t.chars().count(), t.chars().take(90).collect::<String>());
    }
}

fn rot(path: &str, pg: usize) {
    let data = std::fs::read(path).unwrap();
    let pdf = Pdf::new(Arc::new(data)).unwrap();
    let pages = pdf.pages();
    let cache = InterpreterCache::new();
    let page = &pages[pg];
    let mut d = RotDump { items: Vec::new() };
    let mut ctx = Context::new(page.initial_transform(true).to_kurbo(),
        Rect::new(0.0,0.0,20000.0,20000.0), &cache, page.xref(), InterpreterSettings::default());
    interpret_page(page, &mut ctx, &mut d);
    let mut buckets: std::collections::BTreeMap<i64, (usize, String)> = Default::default();
    for (ang, ch) in &d.items {
        let key = (ang / 15.0).round() as i64 * 15;
        let e = buckets.entry(key).or_insert((0, String::new()));
        e.0 += 1;
        if e.1.chars().count() < 40 && !ch.is_whitespace() { e.1.push(*ch); }
    }
    println!("page {} — glyph rotation histogram (degrees):", pg);
    for (k,(n,sample)) in buckets { println!("  {:>5}deg  {:>5} glyphs   sample: {}", k, n, sample); }
}

struct RotDump { items: Vec<(f64, char)> }
impl<'a> Device<'a> for RotDump {
    fn set_soft_mask(&mut self,_m:Option<hayro_interpret::SoftMask<'a>>) {}
    fn set_blend_mode(&mut self,_b:hayro_interpret::BlendMode) {}
    fn draw_path(&mut self,_p:&kurbo::BezPath,_t:Affine,_pa:&hayro_interpret::Paint<'a>,_d:&hayro_interpret::PathDrawMode) {}
    fn push_clip_path(&mut self,_c:&hayro_interpret::ClipPath) {}
    fn push_transparency_group(&mut self,_o:f32,_m:Option<hayro_interpret::SoftMask<'a>>,_b:hayro_interpret::BlendMode) {}
    fn draw_image(&mut self,_i:hayro_interpret::Image<'a,'_>,_t:Affine) {}
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}
    fn draw_glyph(&mut self, glyph:&Glyph<'a>, transform:Affine, glyph_transform:Affine,
                  _p:&hayro_interpret::Paint<'a>,_m:&hayro_interpret::GlyphDrawMode) {
        let c = match glyph.as_unicode() {
            Some(hayro_interpret::hayro_cmap::BfString::Char(c)) => c,
            Some(hayro_interpret::hayro_cmap::BfString::String(s)) => s.chars().next().unwrap_or(' '),
            None => '\u{FFFD}',
        };
        // Text baseline direction = image of the x-axis under the full transform.
        let m = (transform * glyph_transform).as_coeffs();
        let ang = (-m[1]).atan2(m[0]).to_degrees();
        self.items.push((ang, c));
    }
}

struct TextDump { out: String }
impl<'a> Device<'a> for TextDump {
    fn set_soft_mask(&mut self, _m: Option<hayro_interpret::SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _b: hayro_interpret::BlendMode) {}
    fn draw_path(&mut self, _p:&kurbo::BezPath,_t:Affine,_pa:&hayro_interpret::Paint<'a>,_d:&hayro_interpret::PathDrawMode) {}
    fn push_clip_path(&mut self, _c:&hayro_interpret::ClipPath) {}
    fn push_transparency_group(&mut self,_o:f32,_m:Option<hayro_interpret::SoftMask<'a>>,_b:hayro_interpret::BlendMode) {}
    fn draw_image(&mut self,_i:hayro_interpret::Image<'a,'_>,_t:Affine) {}
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}
    fn draw_glyph(&mut self, glyph:&Glyph<'a>,_t:Affine,_gt:Affine,_p:&hayro_interpret::Paint<'a>,_m:&hayro_interpret::GlyphDrawMode) {
        match glyph.as_unicode() {
            Some(hayro_interpret::hayro_cmap::BfString::Char(c)) => self.out.push(c),
            Some(hayro_interpret::hayro_cmap::BfString::String(s)) => self.out.push_str(&s),
            None => self.out.push('\u{FFFD}'),
        }
    }
}

fn main() {
    let a1 = std::env::args().nth(1).unwrap();
    if a1 == "--dump" { dump(&std::env::args().nth(2).unwrap()); return; }
    if a1 == "--pages" { dump_pages(&std::env::args().nth(2).unwrap()); return; }
    if a1 == "--render-all" {
        let dir = std::env::args().nth(2).unwrap();
        let scale: f32 = std::env::args().nth(3).map(|x| x.parse().unwrap()).unwrap_or(2.0);
        let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |x| x == "pdf")).collect();
        files.sort();
        let (mut pages, mut secs, mut blank, mut panics) = (0usize, 0.0f64, Vec::new(), Vec::new());
        for f in &files {
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            let path = f.to_string_lossy().to_string();
            let np = match std::panic::catch_unwind(|| {
                let d = std::fs::read(&path).unwrap();
                Pdf::new(Arc::new(d)).map(|p| p.pages().len()).unwrap_or(0)
            }) { Ok(n) => n, Err(_) => { panics.push(name.clone()); continue } };
            for i in 0..np {
                match std::panic::catch_unwind(|| render::render_page(&path, i, scale, None)) {
                    Ok((w,h,t,ink)) => {
                        pages += 1; secs += t;
                        if ink * 1000 < (w as usize * h as usize) { blank.push(format!("{name} p{i}")); }
                    }
                    Err(_) => panics.push(format!("{name} p{i}")),
                }
            }
        }
        println!("rendered {} pages of {} PDFs at {}x", pages, files.len(), scale);
        println!("  time      : {:.2}s total, {:.1} ms/page, {:.1} pages/s", secs, 1000.0*secs/pages.max(1) as f64, pages as f64/secs);
        println!("  PANICS    : {}", panics.len());
        println!("  near-blank: {} (possible render failures)", blank.len());
        for b in blank.iter().take(8) { println!("    {b}"); }
        for b in panics.iter().take(8) { println!("    PANIC {b}"); }
        return;
    }
    if a1 == "--render" {
        let f = std::env::args().nth(2).unwrap();
        let pg: usize = std::env::args().nth(3).unwrap().parse().unwrap();
        let scale: f32 = std::env::args().nth(4).map(|x| x.parse().unwrap()).unwrap_or(2.0);
        let out = std::env::args().nth(5);
        let (w,h,secs,ink) = render::render_page(&f, pg, scale, out.as_deref());
        println!("{}x{} px in {:.3}s ({:.0} ms) ink_pixels={} ({:.2}% of page)",
                 w, h, secs, secs*1000.0, ink, 100.0*ink as f64/(w*h) as f64);
        return;
    }
    if a1 == "--rot" {
        let f = std::env::args().nth(2).unwrap();
        let pg: usize = std::env::args().nth(3).unwrap().parse().unwrap();
        rot(&f, pg); return;
    }
    let dir = a1;
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap()
        .filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |x| x == "pdf")).collect();
    files.sort();

    let (mut tg, mut tu, mut tb, mut tl, mut tp, mut tt) = (0usize, 0usize, 0usize, 0usize, 0usize, 0.0f64);
    let (mut ok, mut failed, mut panicked, mut zero_glyph) = (0, Vec::new(), Vec::new(), Vec::new());
    let mut low = Vec::new();

    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let p = f.to_string_lossy().to_string();
        // Catch panics: a panic on real-world input is itself a finding.
        match std::panic::catch_unwind(|| run(&p)) {
            Ok(Ok((pages, s, secs))) => {
                ok += 1; tg += s.glyphs; tu += s.unicode; tb += s.boxed;
                tl += s.ligatures; tp += pages; tt += secs;
                if s.glyphs == 0 { zero_glyph.push(name.clone()); }
                else {
                    let rate = s.unicode as f64 / s.glyphs as f64;
                    if rate < 0.99 { low.push((name.clone(), rate, s.glyphs)); }
                }
            }
            Ok(Err(e)) => failed.push((name, e)),
            Err(_) => panicked.push(name),
        }
    }

    println!("=== hayro batch probe: {} PDFs ===", files.len());
    println!("  parsed ok      : {}", ok);
    println!("  parse errors   : {}", failed.len());
    println!("  PANICS         : {}", panicked.len());
    println!("  zero glyphs    : {}  (scanned / image-only -> would route to VLM)", zero_glyph.len());
    println!();
    println!("  pages          : {}", tp);
    println!("  glyphs         : {}", tg);
    println!("  unicode        : {} ({:.3}%)", tu, 100.0 * tu as f64 / tg.max(1) as f64);
    println!("  with bbox      : {} ({:.3}%)", tb, 100.0 * tb as f64 / tg.max(1) as f64);
    println!("  raw ligatures  : {}", tl);
    println!("  wall time      : {:.2}s total, {:.2}ms/page, {:.1} pages/s", tt, 1000.0*tt/tp.max(1) as f64, tp as f64/tt);
    if !failed.is_empty() {
        println!("\n  --- parse errors ---");
        for (n, e) in failed.iter().take(10) { println!("    {n}: {e}"); }
    }
    if !panicked.is_empty() {
        println!("\n  --- PANICS ---");
        for n in panicked.iter().take(10) { println!("    {n}"); }
    }
    if !low.is_empty() {
        println!("\n  --- docs below 99% unicode resolution ({}) ---", low.len());
        let mut l = low.clone(); l.sort_by(|a,b| a.1.partial_cmp(&b.1).unwrap());
        for (n, r, g) in l.iter().take(15) { println!("    {n}: {:.1}% of {} glyphs", 100.0*r, g); }
    }
    if !zero_glyph.is_empty() {
        println!("\n  --- zero-glyph docs ({}) ---", zero_glyph.len());
        for n in zero_glyph.iter().take(8) { println!("    {n}"); }
    }
}
