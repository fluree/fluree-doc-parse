//! Glyph extraction from a PDF, via hayro's `Device` trait.
//!
//! All state lives in `Extractor` — a per-document value, not globals. This is
//! deliberate: carrying extraction state in globals or thread-locals forces
//! pipeline stages to run sequentially and makes state propagation to worker
//! threads a source of silent data loss.

use crate::geom::BBox;
use crate::glyph::Glyph;
use crate::rule::{self, Fill, Rule};
use hayro_interpret::font::Glyph as HGlyph;
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::util::TransformExt;
use hayro_interpret::{interpret_page, Context, Device, InterpreterCache, InterpreterSettings};
use kurbo::{Affine, Rect, Shape};
use std::sync::Arc;

/// Clip box for interpretation. Generous: we want every glyph the page draws,
/// including any drawn outside the crop box, and filter later.
const INTERPRET_BOUNDS: f64 = 20_000.0;

/// A raster image placed on the page, with a cheap probe of its content.
#[derive(Debug, Clone, Copy)]
pub struct ImagePlacement {
    /// Placement on the page, clipped to the page box.
    pub bbox: BBox,
    /// True when the pixels look like text or line structure — bimodal
    /// luminance with frequent dark/light transitions along rows. A raster
    /// table, a scanned paragraph or a labelled chart reads true; a photograph
    /// reads false. This is what separates "the VLM can recover content here"
    /// from "it is a picture", which placement geometry alone cannot see:
    /// two documents with near-identical image coverage can still diverge
    /// sharply — the AI-routed reference engine *loses* 0.38 on one and
    /// *gains* 0.62 on the other.
    pub texty: bool,
}

pub struct Page {
    pub index: usize,
    pub glyphs: Vec<Glyph>,
    /// Raster image placements, clipped to the page box. The router's primary
    /// signal: a page that is one large image with few or no glyphs is a scan,
    /// and no geometric analysis of absent glyphs can parse it.
    pub images: Vec<ImagePlacement>,
    /// Thin stroked/filled shapes — a bordered table's grid.
    pub rules: Vec<Rule>,
    /// Larger filled areas — header shading and zebra striping, which mark row
    /// structure in tables drawn without vertical rules.
    pub fills: Vec<Fill>,
    /// Rendered page size in PDF units. Needed to express positions as a
    /// fraction of page height, which is how furniture detection compares
    /// positions across pages of differing size.
    pub width: f64,
    pub height: f64,
}

pub struct Document {
    pub pages: Vec<Page>,
}

impl Document {
    pub fn glyph_count(&self) -> usize {
        self.pages.iter().map(|p| p.glyphs.len()).sum()
    }
    /// Fraction of glyphs that resolved to a Unicode value. The primary
    /// routing signal: a low rate means broken CID fonts and the page should
    /// go to the VLM.
    pub fn unicode_rate(&self) -> f64 {
        let total = self.glyph_count();
        if total == 0 {
            return 0.0;
        }
        let ok: usize = self
            .pages
            .iter()
            .flat_map(|p| &p.glyphs)
            .filter(|g| !g.text.is_empty())
            .count();
        ok as f64 / total as f64
    }
}

struct Collector {
    page: usize,
    glyphs: Vec<Glyph>,
    images: Vec<ImagePlacement>,
    rules: Vec<Rule>,
    fills: Vec<Fill>,
    /// The page's visible area — the crop box in render space. Content outside
    /// it is collected by no viewer and must not be collected here either:
    /// a book-spread scan's crop box shows one 510pt page while the
    /// neighbouring page's full text sits at x=510-947, and emitting it
    /// doubled the document ("we extract everything and filter later" — this
    /// is the later).
    page_box: BBox,
    /// Per-glyph `(font identity, ink ratio)`, parallel to `glyphs`, for the
    /// weight inference in [`infer_weights`]. Kept alongside rather than on
    /// `Glyph` because it is scaffolding: once weights are resolved it has no
    /// further use, and every consumer of `Glyph` would have to carry it.
    ink: Vec<Option<(u128, f64)>>,
}

impl<'a> Device<'a> for Collector {
    fn set_soft_mask(&mut self, _m: Option<hayro_interpret::SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _b: hayro_interpret::BlendMode) {}
    fn draw_path(
        &mut self,
        path: &kurbo::BezPath,
        transform: Affine,
        _paint: &hayro_interpret::Paint<'a>,
        _d: &hayro_interpret::PathDrawMode,
    ) {
        // Bounding box only. A table grid is axis-aligned rectangles and
        // segments; curve detail is irrelevant and expensive to keep.
        let r: Rect = (transform * path.clone()).bounding_box();
        if !self.page_box.intersects(&crate::geom::from_kurbo(r)) {
            return;
        }
        match rule::classify(crate::geom::from_kurbo(r), self.page) {
            Some(rule::Shape::Rule(x)) => self.rules.push(x),
            Some(rule::Shape::Fill(x)) => self.fills.push(x),
            None => {}
        }
    }
    fn push_clip_path(&mut self, _c: &hayro_interpret::ClipPath) {}
    fn push_transparency_group(
        &mut self,
        _o: f32,
        _m: Option<hayro_interpret::SoftMask<'a>>,
        _b: hayro_interpret::BlendMode,
    ) {
    }
    fn draw_image(&mut self, i: hayro_interpret::Image<'a, '_>, t: Affine) {
        // The transform maps the image's *pixel* space onto the page.
        let (w, h) = (i.width() as f64, i.height() as f64);
        let r: Rect = (t * Rect::new(0.0, 0.0, w, h).to_path(0.1)).bounding_box();
        let b = crate::geom::from_kurbo(r);
        if !self.page_box.intersects(&b) {
            return;
        }
        let texty = match &i {
            hayro_interpret::Image::Raster(ri) => {
                let mut texty = false;
                // A low-resolution decode is plenty for a structure probe and
                // keeps the cost per image at microseconds.
                ri.with_rgba(
                    |data, _| texty = looks_like_text(&data),
                    Some((PROBE_DIM, PROBE_DIM)),
                );
                texty
            }
            // Stencils are 1-bit masks — the format scanned text and line art
            // arrive in. Treat as text-bearing.
            hayro_interpret::Image::Stencil(_) => true,
        };
        self.images.push(ImagePlacement {
            bbox: BBox {
                x0: b.x0.max(self.page_box.x0),
                y0: b.y0.max(self.page_box.y0),
                x1: b.x1.min(self.page_box.x1),
                y1: b.y1.min(self.page_box.y1),
            },
            texty,
        });
    }
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_glyph(
        &mut self,
        glyph: &HGlyph<'a>,
        transform: Affine,
        glyph_transform: Affine,
        _paint: &hayro_interpret::Paint<'a>,
        _mode: &hayro_interpret::GlyphDrawMode,
    ) {
        let mut text = match glyph.as_unicode() {
            Some(hayro_interpret::hayro_cmap::BfString::Char(c)) => c.to_string(),
            Some(hayro_interpret::hayro_cmap::BfString::String(s)) => s,
            None => String::new(),
        };

        // Cross-examine suspicious decodes against the drawn shape. A known
        // academic-publisher toolchain family subsets
        // TeX math fonts with the symbol glyphs renamed after Latin lookalikes
        // — equal becomes /onequarter, plus becomes /thorn — and the glyph
        // names, /Differences and the generated ToUnicode all repeat the lie
        // consistently, so no metadata-conforming decode can recover the
        // truth (every conforming reader emits the same `S ¼ kB ln Ω`). The outline cannot
        // lie: an equals sign is two straight bars, and no real ¼ ever is.
        if matches!(text.as_str(), "¼" | "½" | "þ" | "ð" | "Þ" | "\u{FFFD}") {
            if let HGlyph::Outline(o) = glyph {
                if let Some(fixed) = lookalike_repair(&text, &o.outline()) {
                    text = fixed.to_string();
                }
            }
        }

        let full = transform * glyph_transform;
        let m = full.as_coeffs();
        // Baseline direction = image of the x-axis under the full transform.
        let rotation_deg = (-m[1]).atan2(m[0]).to_degrees() as f32;
        // The outline is in a 1000-upem space, so glyph_transform carries a
        // 1/1000 scale; multiply back out to get the rendered size in PDF units.
        let font_size = ((m[0] * m[0] + m[1] * m[1]).sqrt() * 1000.0) as f32;
        // Translation component = pen position on the baseline.
        let origin = (m[4], m[5]);

        // advance_width() is in the glyph's 1000-upem space, same as outline().
        let advance = match glyph {
            HGlyph::Outline(o) => o.advance_width().map(|a| {
                let m2 = (transform * glyph_transform).as_coeffs();
                let scale = (m2[0] * m2[0] + m2[1] * m2[1]).sqrt();
                a as f64 * scale
            }),
            _ => None,
        };

        let weight = match glyph {
            HGlyph::Outline(o) => o.font_data().and_then(|f| {
                // Prefer the declared weight; fall back to the PostScript name,
                // since many embedded subsets omit the weight class but keep a
                // name like "TimesNewRomanPS-BoldMT".
                f.weight.or_else(|| {
                    let n = f
                        .postscript_name
                        .as_deref()
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    // URW's Ghostscript families — the standard substitutes in
                    // pdfTeX output — abbreviate their four styles to `Regu`,
                    // `Medi`, `Ital` and `Bold`, and there `Medi` *is* the bold
                    // face: `NimbusRomNo9L-Medi` sets every bold word in a LaTeX
                    // paper. Full-word "Medium" is the ordinary 500 weight in
                    // other families and must not be caught with it.
                    let urw_medium = n.contains("medi") && !n.contains("medium");
                    (n.contains("bold") || n.contains("black") || n.contains("heavy") || urw_medium)
                        .then_some(700)
                })
            }),
            _ => None,
        };

        // Ink density, for documents that declare no weights at all. Measured
        // here because the transformed outline is already being built for the
        // bounding box, and is not available anywhere later.
        let mut ink = None;
        let bbox = match glyph {
            HGlyph::Outline(o) => {
                let path = full * o.outline();
                let r: Rect = path.bounding_box();
                let b = crate::geom::from_kurbo(r);
                if b.is_empty() {
                    None
                } else {
                    let area = (b.x1 - b.x0) * (b.y1 - b.y0);
                    // Letters only: punctuation and digits have ink ratios of
                    // their own that swamp the comparison between two faces.
                    if area > 0.0 && text.chars().all(|c| c.is_alphabetic()) {
                        ink = Some((o.font_cache_key(), path.area().abs() / area));
                    }
                    Some(b)
                }
            }
            // Type3 glyphs are drawing programs; we do not yet compute their
            // extent. They are rare and currently contribute no bbox.
            _ => None,
        };

        // Off-page content is invisible in every viewer. A glyph whose ink
        // intersects the page still shows its inside part and is kept; one
        // with no box (an explicit space) is kept only when the pen is on the
        // page — a trailing space beyond the margin bounds no visible word.
        let visible = match bbox {
            Some(b) => self.page_box.intersects(&b),
            None => self.page_box.contains(origin.0, origin.1),
        };
        if !visible {
            return;
        }
        let draw_index = self.glyphs.len();
        self.ink.push(ink);
        self.glyphs.push(Glyph {
            text,
            bbox,
            page: self.page,
            origin,
            rotation_deg,
            font_size,
            weight,
            advance,
            draw_index,
        });
    }
}

/// Requested decode size for the image structure probe. Enough to see text
/// banding; small enough to cost microseconds.
const PROBE_DIM: u32 = 256;

/// Fraction of pixels that must sit in the outer luminance quartiles. Ink on
/// paper is bimodal — near-white ground, near-dark marks; photographs live in
/// the middle of the range.
const MIN_BIMODAL_FRACTION: f64 = 0.55;

/// Fraction of rows that must show frequent dark/light transitions. Text and
/// table rows alternate ink and ground many times across their width; a
/// photograph's rows do not survive binarisation with that structure.
const MIN_TEXTY_ROW_FRACTION: f64 = 0.18;

/// Transitions per row (at probe resolution) for the row to count as texty.
const MIN_ROW_TRANSITIONS: usize = 6;

/// Whether decoded pixels look like text or line structure rather than a
/// photograph. See [`ImagePlacement::texty`].
fn looks_like_text(data: &hayro_interpret::ImageData) -> bool {
    let (w, h, luma): (usize, usize, Vec<u8>) = match data {
        hayro_interpret::ImageData::Luma(l) => {
            (l.width as usize, l.height as usize, l.data.clone())
        }
        hayro_interpret::ImageData::Rgb(r) => (
            r.width as usize,
            r.height as usize,
            r.data
                .chunks_exact(3)
                .map(|c| ((c[0] as u16 + c[1] as u16 + c[2] as u16) / 3) as u8)
                .collect(),
        ),
    };
    if w < 8 || h < 8 || luma.len() < w * h {
        return false;
    }

    let bimodal = luma.iter().filter(|&&v| !(64..192).contains(&v)).count();
    if (bimodal as f64) < MIN_BIMODAL_FRACTION * luma.len() as f64 {
        return false;
    }

    // Binarise at the luminance midpoint and count row transitions.
    let (mut lo, mut hi) = (u8::MAX, u8::MIN);
    for &v in &luma {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if hi - lo < 64 {
        return false; // near-uniform: a wash of colour, not marks on ground
    }
    let mid = lo + (hi - lo) / 2;
    let mut texty_rows = 0usize;
    for row in luma.chunks_exact(w) {
        let mut transitions = 0usize;
        for pair in row.windows(2) {
            if (pair[0] >= mid) != (pair[1] >= mid) {
                transitions += 1;
            }
        }
        if transitions >= MIN_ROW_TRANSITIONS {
            texty_rows += 1;
        }
    }
    let frac = texty_rows as f64 / h as f64;
    if std::env::var_os("FDOC_PROBE_DEBUG").is_some() {
        eprintln!(
            "PROBE texty_frac={frac:.3} bimodal={:.3} {w}x{h}",
            bimodal as f64 / luma.len() as f64
        );
    }
    frac >= MIN_TEXTY_ROW_FRACTION
}

/// Repair a TeX-lookalike misdecode by interrogating the outline itself.
///
/// The characters in the suspicious set are what the broken producer's tables
/// emit where TeX math symbols were drawn. The metadata is known-garbage for
/// glyphs in that set, so the *shape classifies the glyph*: each recoverable
/// symbol has a topology no member of the set can legitimately have —
///
/// * `=` — two contours, much wider than tall (a real ¼ is three-plus
///   contours, roughly square; measured here: 2 subpaths, 665×234)
/// * `+` — one square cross of many segments (a real þ is 0.5–0.75 w/h with
///   a bowl; measured: 1 subpath, 665×666)
/// * `-` — one flat bar (nothing in the set is a single flat bar)
/// * `( ) [ ]` — one contour at least three times taller than wide, curved
///   for parentheses and rectilinear for brackets, with the side taken from
///   the ink centroid (a bracket's spine carries most of its ink)
///
/// A genuine ¼, ½, þ, ð or Þ matches none of these and keeps its text. Note
/// the assignments are *per subset*, so a fixed name-keyed table cannot work:
/// the same lookalike name stands for `-` in one document family and for a
/// 137×1000 rectilinear `[` here. The shape is the only authority the
/// producer never forged.
fn lookalike_repair(text: &str, outline: &kurbo::BezPath) -> Option<&'static str> {
    let mut subpaths = 0usize;
    let mut curves = 0usize;
    let mut segments = 0usize;
    for el in outline.elements() {
        match el {
            kurbo::PathEl::MoveTo(_) => subpaths += 1,
            kurbo::PathEl::LineTo(_) => segments += 1,
            kurbo::PathEl::QuadTo(..) | kurbo::PathEl::CurveTo(..) => {
                curves += 1;
                segments += 1;
            }
            kurbo::PathEl::ClosePath => {}
        }
    }
    let b = outline.bounding_box();
    let (w, h) = (b.width(), b.height());
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    if subpaths == 2 && w >= 2.0 * h && segments <= 16 {
        return Some("=");
    }
    if subpaths != 1 {
        return None;
    }
    if (0.85 * h..=1.2 * h).contains(&w) && (8..=16).contains(&segments) {
        return Some("+");
    }
    if w >= 3.0 * h && segments <= 8 {
        return Some("-");
    }
    if h >= 3.0 * w && segments <= 12 {
        let left = ink_centroid_is_left(outline, &b)?;
        return Some(match (curves > 0, left) {
            (true, true) => "(",
            (true, false) => ")",
            (false, true) => "[",
            (false, false) => "]",
        });
    }
    let _ = text;
    None
}

/// Whether a path's area-weighted centroid sits left of its box centre, with
/// a dead zone: `None` when it is too central to call. Distinguishes `[`
/// from `]` and `(` from `)` — the spine or belly carries the ink.
fn ink_centroid_is_left(outline: &kurbo::BezPath, b: &Rect) -> Option<bool> {
    let mut pts: Vec<kurbo::Point> = Vec::new();
    let mut area2 = 0.0; // twice the signed area
    let mut cx6 = 0.0; // six times area-weighted centroid x
    let mut close = |pts: &mut Vec<kurbo::Point>| {
        for i in 0..pts.len() {
            let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
            let cross = p.x * q.y - q.x * p.y;
            area2 += cross;
            cx6 += (p.x + q.x) * cross;
        }
        pts.clear();
    };
    kurbo::flatten(outline.elements().iter().copied(), 1.0, |el| match el {
        kurbo::PathEl::MoveTo(p) => {
            if !pts.is_empty() {
                close(&mut pts);
            }
            pts.push(p);
        }
        kurbo::PathEl::LineTo(p) => pts.push(p),
        kurbo::PathEl::ClosePath => close(&mut pts),
        _ => {}
    });
    if !pts.is_empty() {
        close(&mut pts);
    }
    if area2.abs() < f64::EPSILON {
        return None;
    }
    let cx = cx6 / (3.0 * area2);
    let mid = (b.x0 + b.x1) * 0.5;
    let dead = (b.x1 - b.x0) * 0.05;
    if cx < mid - dead {
        Some(true)
    } else if cx > mid + dead {
        Some(false)
    } else {
        None
    }
}

/// Minimum letters a face must set before its ink ratio is trusted. A handful
/// of glyphs is a maths italic or a logo, not a text face.
const MIN_INK_GLYPHS: usize = 40;

/// How much denser than the body face a face must be to count as bold.
///
/// Times bold carries roughly 25-30% more ink than Times regular at the same
/// size. 1.15 sits clear of that floor while staying well above the spread
/// between a regular face and its italic, which differ by only a few percent.
const BOLD_INK_MARGIN: f64 = 1.15;

/// Infer font weight from ink density, for documents that declare none.
///
/// `hayro`'s `font_data()` returns `None` for Type 1 fonts, and many embedded
/// subsets omit `/FontWeight` and carry a meaningless name. Across the
/// 200-document benchmark corpus **81 documents — 40% — expose no weight at
/// all**, which silently disables every weight-based heading signal on exactly
/// the classic LaTeX papers where bold section titles are the only cue.
///
/// A bold face is physically heavier: it covers more of each glyph's box. That
/// is measurable from the outline regardless of font format or naming. Faces
/// are compared *within* a document against its body face — absolute ink ratios
/// vary too much between typefaces to threshold directly.
///
/// Runs only when nothing is declared, so a document with real weight data is
/// never second-guessed.
fn infer_weights(pages: &mut [Page], ink: &[Vec<Option<(u128, f64)>>]) {
    if pages
        .iter()
        .flat_map(|p| &p.glyphs)
        .any(|g| g.weight.is_some())
    {
        return;
    }
    let mut stats: std::collections::HashMap<u128, (usize, f64)> = Default::default();
    for page in ink {
        for (key, ratio) in page.iter().flatten() {
            let e = stats.entry(*key).or_insert((0, 0.0));
            e.0 += 1;
            e.1 += *ratio;
        }
    }
    stats.retain(|_, (n, _)| *n >= MIN_INK_GLYPHS);
    if stats.len() < 2 {
        return;
    }
    // The body face is the one setting the most letters. Comparing against the
    // mean of all faces instead would let a heading-heavy page drag the
    // reference upward and hide the very contrast being looked for.
    let (_, &(body_n, body_sum)) = stats
        .iter()
        .max_by_key(|(_, (n, _))| *n)
        .expect("non-empty");
    let threshold = (body_sum / body_n as f64) * BOLD_INK_MARGIN;
    let bold: std::collections::HashSet<u128> = stats
        .iter()
        .filter(|(_, (n, sum))| sum / *n as f64 >= threshold)
        .map(|(k, _)| *k)
        .collect();
    if bold.is_empty() {
        return;
    }
    for (page, marks) in pages.iter_mut().zip(ink) {
        for (glyph, mark) in page.glyphs.iter_mut().zip(marks) {
            if mark.is_some_and(|(k, _)| bold.contains(&k)) {
                glyph.weight = Some(700);
            }
        }
    }
}

#[derive(Debug)]
pub enum ExtractError {
    Io(String),
    Parse(String),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::Io(e) => write!(f, "io: {e}"),
            ExtractError::Parse(e) => write!(f, "parse: {e}"),
        }
    }
}

impl std::error::Error for ExtractError {}

pub fn extract_bytes(data: Vec<u8>) -> Result<Document, ExtractError> {
    let pdf = Pdf::new(Arc::new(data)).map_err(|e| ExtractError::Parse(format!("{e:?}")))?;
    let cache = InterpreterCache::new();
    let settings = InterpreterSettings::default();
    let mut pages = Vec::new();
    let mut ink: Vec<Vec<Option<(u128, f64)>>> = Vec::new();

    for (index, page) in pdf.pages().iter().enumerate() {
        let (w, h) = page.render_dimensions();
        let mut dev = Collector {
            page: index,
            glyphs: Vec::new(),
            images: Vec::new(),
            rules: Vec::new(),
            fills: Vec::new(),
            ink: Vec::new(),
            page_box: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: w as f64,
                y1: h as f64,
            },
        };
        let mut ctx = Context::new(
            page.initial_transform(true).to_kurbo(),
            Rect::new(0.0, 0.0, INTERPRET_BOUNDS, INTERPRET_BOUNDS),
            &cache,
            page.xref(),
            settings.clone(),
        );
        interpret_page(page, &mut ctx, &mut dev);
        synthesize_missing_boxes(&mut dev.glyphs);
        ink.push(dev.ink);
        pages.push(Page {
            index,
            glyphs: dev.glyphs,
            images: dev.images,
            rules: dev.rules,
            fills: dev.fills,
            width: w as f64,
            height: h as f64,
        });
    }
    infer_weights(&mut pages, &ink);

    Ok(Document { pages })
}

/// Ascent above the baseline as a fraction of font size, for synthesized
/// boxes. A cap-height box, not a full em: line grouping compares
/// baselines, and highlight rectangles drawn from these look right.
const SYNTH_ASCENT: f64 = 0.80;
/// Descent below the baseline as a fraction of font size.
const SYNTH_DESCENT: f64 = 0.22;
/// Width fallback (fraction of font size) when a glyph has no same-line
/// right neighbour to measure against — a line's last glyph, or an
/// isolated one.
const SYNTH_WIDTH: f64 = 0.60;

/// Give outline-less TEXT glyphs a geometric extent.
///
/// Type3 fonts are drawing programs whose glyph procedures we do not
/// interpret, so their glyphs arrive with `bbox: None` — real text, a real
/// pen position, and no box. Design-tool exports (Canva, Figma decks) set
/// entire documents in Type3, and a page whose every glyph is boxless
/// assembles zero lines: the text layer is present and the extraction
/// reads nothing (the 2026-deck defect).
///
/// The geometry is recoverable from what IS known: the origin is the true
/// baseline pen position, and the next glyph's origin on the same
/// baseline states this glyph's advance exactly (Type3 or not, the pen
/// moved). Synthesize `bbox`/`advance` from those, with a font-size
/// fallback for line-final glyphs.
///
/// Whitespace keeps `bbox: None` deliberately — the span-merge and
/// explicit-space rules key on boxless spaces, and giving a space ink
/// would invent geometry nothing drew.
fn synthesize_missing_boxes(glyphs: &mut [Glyph]) {
    // Baseline-cluster per rotation bucket, mirroring line::assemble's
    // grouping so the synthesized runs are the runs assembly will see.
    let candidates: Vec<usize> = (0..glyphs.len())
        .filter(|&i| {
            glyphs[i].bbox.is_none()
                && !glyphs[i].text.trim().is_empty()
                && glyphs[i].rotation_bucket().rem_euclid(180) == 0
        })
        .collect();
    if candidates.is_empty() {
        return;
    }
    // Sort by (baseline y, x); successive same-baseline deltas give widths.
    let mut order = candidates;
    order.sort_by(|&a, &b| {
        let (oa, ob) = (glyphs[a].origin, glyphs[b].origin);
        oa.1.partial_cmp(&ob.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(oa.0.partial_cmp(&ob.0).unwrap_or(std::cmp::Ordering::Equal))
    });
    for w in 0..order.len() {
        let i = order[w];
        let (x, y) = glyphs[i].origin;
        let font = glyphs[i].font_size.max(1.0) as f64;
        // Right neighbour on the same baseline (within the same tolerance
        // assembly uses), measured through explicit spaces: the pen delta
        // is the advance whether or not something is drawn between.
        let width = order
            .get(w + 1)
            .map(|&j| glyphs[j].origin)
            .filter(|(nx, ny)| (ny - y).abs() < font * 0.25 && *nx > x && (nx - x) < font * 2.5)
            .map(|(nx, _)| nx - x)
            .unwrap_or(font * SYNTH_WIDTH);
        glyphs[i].bbox = Some(BBox {
            x0: x,
            y0: y - font * SYNTH_ASCENT,
            x1: x + width,
            y1: y + font * SYNTH_DESCENT,
        });
        if glyphs[i].advance.is_none() {
            glyphs[i].advance = Some(width);
        }
    }
}

pub fn extract_file(path: &std::path::Path) -> Result<Document, ExtractError> {
    let data = std::fs::read(path).map_err(|e| ExtractError::Io(e.to_string()))?;
    extract_bytes(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(n: usize) -> Page {
        let glyphs = (0..n)
            .map(|i| Glyph {
                text: "a".into(),
                bbox: None,
                page: 0,
                origin: (0.0, 0.0),
                rotation_deg: 0.0,
                font_size: 10.0,
                weight: None,
                advance: None,
                draw_index: i,
            })
            .collect();
        Page {
            index: 0,
            glyphs,
            images: Vec::new(),
            rules: Vec::new(),
            fills: Vec::new(),
            width: 600.0,
            height: 800.0,
        }
    }

    /// `body` letters at ratio 0.50, then `heavy` at `ratio`.
    fn ink(body: usize, heavy: usize, ratio: f64) -> Vec<Vec<Option<(u128, f64)>>> {
        let mut v: Vec<Option<(u128, f64)>> = (0..body).map(|_| Some((1u128, 0.50))).collect();
        v.extend((0..heavy).map(|_| Some((2u128, ratio))));
        vec![v]
    }

    #[test]
    fn a_denser_face_is_inferred_bold() {
        let mut pages = vec![page(200)];
        infer_weights(&mut pages, &ink(150, 50, 0.65));
        assert!(
            pages[0].glyphs[0].weight.is_none(),
            "body face stays regular"
        );
        assert_eq!(
            pages[0].glyphs[199].weight,
            Some(700),
            "denser face reads bold"
        );
    }

    #[test]
    fn an_italic_is_not_bold() {
        // A regular face and its italic differ by a few percent, well inside
        // the margin; promoting italics would flood the Bold heading signal.
        let mut pages = vec![page(200)];
        infer_weights(&mut pages, &ink(150, 50, 0.53));
        assert!(pages[0].glyphs.iter().all(|g| g.weight.is_none()));
    }

    #[test]
    fn a_declared_weight_is_never_second_guessed() {
        let mut pages = vec![page(200)];
        pages[0].glyphs[0].weight = Some(400);
        infer_weights(&mut pages, &ink(150, 50, 0.90));
        assert!(
            pages[0].glyphs[199].weight.is_none(),
            "inference must not run"
        );
    }

    #[test]
    fn a_rare_face_is_ignored() {
        // Below MIN_INK_GLYPHS: a maths italic or a logo, not a text face.
        let mut pages = vec![page(200)];
        infer_weights(&mut pages, &ink(180, 20, 0.90));
        assert!(pages[0].glyphs.iter().all(|g| g.weight.is_none()));
    }
}

#[cfg(test)]
mod lookalike_tests {
    use super::lookalike_repair;
    use kurbo::BezPath;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
        format!("M{x0},{y0} L{x1},{y0} L{x1},{y1} L{x0},{y1} Z ")
    }

    #[test]
    fn two_flat_bars_decoded_as_onequarter_are_an_equals_sign() {
        let p =
            BezPath::from_svg(&(rect(0., 0., 660., 80.) + &rect(0., 160., 660., 240.))).unwrap();
        assert_eq!(lookalike_repair("¼", &p), Some("="));
    }

    #[test]
    fn a_square_cross_decoded_as_thorn_is_a_plus() {
        let p = BezPath::from_svg(
            "M270,0 L390,0 L390,270 L660,270 L660,390 L390,390 L390,660 L270,660 L270,390 L0,390 L0,270 L270,270 Z",
        )
        .unwrap();
        assert_eq!(lookalike_repair("þ", &p), Some("+"));
    }

    #[test]
    fn a_tall_rectilinear_spine_left_is_an_opening_bracket() {
        // [ : spine at the left, teeth pointing right.
        let p =
            BezPath::from_svg("M0,0 L137,0 L137,40 L40,40 L40,960 L137,960 L137,1000 L0,1000 Z")
                .unwrap();
        assert_eq!(lookalike_repair("½", &p), Some("["));
    }

    #[test]
    fn a_genuine_onequarter_is_left_alone() {
        // Three contours, roughly square — a real ¼ (digit, slash, digit).
        let p = BezPath::from_svg(
            &(rect(0., 0., 200., 450.)
                + &rect(280., 0., 420., 1000.)
                + &rect(500., 550., 700., 1000.)),
        )
        .unwrap();
        assert_eq!(lookalike_repair("¼", &p), None);
    }

    #[test]
    fn a_genuine_thorn_is_left_alone() {
        // Tall (0.6 aspect) single contour — thorn-like proportions.
        let p = BezPath::from_svg("M0,0 L400,0 L400,660 L0,660 Z").unwrap();
        assert_eq!(lookalike_repair("þ", &p), None);
    }
}
