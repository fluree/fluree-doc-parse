//! Glyph extraction from a PDF, via hayro's `Device` trait.
//!
//! All state lives in `Extractor` — a per-document value, not globals. This is
//! deliberate: carrying extraction state in globals or thread-locals forces
//! pipeline stages to run sequentially and makes state propagation to worker
//! threads a source of silent data loss.

use crate::geom::BBox;
use crate::glyph::Glyph;
use crate::rule::{self, Fill, Rule};
use fluree_doc_model::symbols;
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
    /// True when the pixels look like text or line structure
    /// ([`fluree_doc_vision::texty`]): bimodal luminance with frequent
    /// dark/light transitions along rows, or lines of text on a ground of
    /// one colour. A raster table, a scanned paragraph, a labelled chart or
    /// a screen capture with a few lines on it reads true; a photograph
    /// reads false. This is what separates "the VLM can recover content
    /// here" from "it is a picture", which placement geometry alone cannot
    /// see: two documents with near-identical image coverage can still
    /// diverge sharply — the AI-routed reference engine *loses* 0.38 on one
    /// and *gains* 0.62 on the other.
    pub texty: bool,
    /// The image's own resolution where it is drawn: its pixels per inch on
    /// the page, along whichever of its axes is drawn coarser. `None` when
    /// unknown.
    pub ppi: Option<f64>,
    /// One bit per pixel (an image mask, a fax, a bitonal scan), by its bit
    /// depth or by a filter that only codes one bit (CCITT, JBIG2). Rendered
    /// smoothed at the model scale, such an image reads better than at its
    /// own resolution.
    pub bitonal: bool,
    /// The image's own pixel grid, `(width, height)`: what it can show,
    /// however large it is drawn. A checkbox drawn from an 8 × 8 mask holds
    /// no more than 8 × 8 pixels at any size, and a rule stretched from a
    /// 1 × 1 mask none at all.
    pub grid: (u32, u32),
    /// What the placement shows, as a key: the image's own bytes (a raster
    /// image's encoded stream with its dictionary, a small stencil's decoded
    /// mask) with the size and orientation it is drawn at. Two placements
    /// with one key show the same pixels the same way: a form's checkbox
    /// drawn two hundred times is one image an OCR engine reads once
    /// ([`crate::ocr::copies`]). `None` when not known: a stencil of more
    /// than [`MAX_KEYED_STENCIL_PX`] pixels, which is not decoded for it.
    ///
    /// `None` too when its box shows more than the image, or less, since a
    /// crop of it would show something else than the image's pixels: when
    /// a glyph, a path (a rule, a fill, a tick) or another image (another
    /// key, or this one drawn elsewhere) overlaps it, or a clip cuts it. A
    /// form's field frame with a value typed in it, or a sprite sheet drawn
    /// twice through two clips, is each time a picture of its own.
    pub key: Option<u64>,
}

#[derive(Clone)]
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
    /// What the router made of this page as it was drawn, kept by
    /// [`crate::document::analyze_with`] before it cleans the glyphs (see
    /// [`Page::routing`]); `None` until then.
    pub routing: Option<crate::route::Routing>,
    /// Small filled marks, under a checkbox's size: a dot or a square a
    /// producer draws as a list's bullet rather than setting it as a glyph.
    pub dots: Vec<BBox>,
    /// Page size in PDF units, in the frame the glyphs are in — the reading
    /// frame (see [`Page::rotation`]). Needed to express positions as a
    /// fraction of page height, which is how furniture detection compares
    /// positions across pages of differing size.
    pub width: f64,
    pub height: f64,
    /// Quarter turns, in degrees counter-clockwise (0, 90, 180 or 270), the
    /// page as displayed was turned to bring its text upright. Every
    /// coordinate on this page — glyphs, rules, fills, images, `width` and
    /// `height` — is in that turned frame, the *reading frame*.
    ///
    /// Zero for nearly every page. Non-zero when the page displays sideways
    /// or upside down: a `/Rotate 90` page whose content was drawn upright,
    /// a landscape table bound into a portrait document. Read in the
    /// display frame such a page is all one rotation bucket, its strings
    /// come out reversed and its tables as one thin figure — a viewer
    /// shows exactly that, and a person turns the page. This is the turn.
    ///
    /// The output's boxes are always in the display frame, which is the
    /// frame a page render has; [`Page::to_display`] maps back.
    pub rotation: i32,
    /// What an OCR reading added to this page, when one did. See
    /// [`crate::ocr`].
    pub ocr: Option<OcrRead>,
    /// What the PDF interpreter reported while reading this page, once per
    /// time it did, by its name: `ImageDecodeFailure`, an image it could not
    /// decode. Such an image is still placed, so the page looks exactly like
    /// one whose image decoded; this is the only trace of the difference.
    /// An image shared by several pages is reported on each that draws it:
    /// the interpreter decodes it at every placement. An image mask (a
    /// stencil) is decoded while the page is read only for its key, when it
    /// is small ([`MAX_KEYED_STENCIL_PX`]), so a larger broken one is not
    /// reported.
    pub warnings: Vec<&'static str>,
}

/// The mark an OCR reading leaves on a page.
///
/// The glyphs it adds are told from the page's own by their draw index, so
/// the router can keep judging the text layer the page came with: a region
/// OCR filled keeps its place, and its number, among the page's routed
/// regions, and a crop name read before the injection still names the same
/// region after it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OcrRead {
    /// Glyphs whose `draw_index` is at least this were added by OCR.
    pub first_glyph: usize,
    /// A whole-page reading was taken.
    pub whole: bool,
    /// The routed regions, by index, a reading was taken for.
    pub regions: Vec<usize>,
}

impl Page {
    /// Whether a glyph was added by an OCR reading rather than drawn by the
    /// page.
    pub fn is_ocr(&self, g: &Glyph) -> bool {
        self.ocr
            .as_ref()
            .is_some_and(|o| g.draw_index >= o.first_glyph)
    }

    /// Whether OCR read routed region `i`, or the whole page.
    pub fn ocr_read_region(&self, i: usize) -> bool {
        self.ocr
            .as_ref()
            .is_some_and(|o| o.whole || o.regions.contains(&i))
    }

    /// Page size as displayed — the frame the output's boxes are in.
    pub fn display_size(&self) -> (f64, f64) {
        if self.rotation % 180 == 0 {
            (self.width, self.height)
        } else {
            (self.height, self.width)
        }
    }

    /// A reading-frame point in the display frame.
    pub fn to_display_point(&self, (x, y): (f64, f64)) -> (f64, f64) {
        // The reading frame is `width` x `height`; the display frame is
        // `display_size()`. Each case inverts the turn `read_upright` made.
        match self.rotation.rem_euclid(360) {
            90 => (self.height - y, x),
            180 => (self.width - x, self.height - y),
            270 => (y, self.width - x),
            _ => (x, y),
        }
    }

    /// A display-frame point in the reading frame.
    pub fn from_display_point(&self, (x, y): (f64, f64)) -> (f64, f64) {
        match self.rotation.rem_euclid(360) {
            90 => (y, self.height - x),
            180 => (self.width - x, self.height - y),
            270 => (self.width - y, x),
            _ => (x, y),
        }
    }

    /// A reading-frame box in the display frame.
    pub fn to_display(&self, b: BBox) -> BBox {
        if self.rotation.rem_euclid(360) == 0 {
            return b;
        }
        let (ax, ay) = self.to_display_point((b.x0, b.y0));
        let (bx, by) = self.to_display_point((b.x1, b.y1));
        BBox {
            x0: ax.min(bx),
            y0: ay.min(by),
            x1: ax.max(bx),
            y1: ay.max(by),
        }
    }

    /// A display-frame box in the reading frame.
    pub fn from_display(&self, b: BBox) -> BBox {
        if self.rotation.rem_euclid(360) == 0 {
            return b;
        }
        let (ax, ay) = self.from_display_point((b.x0, b.y0));
        let (bx, by) = self.from_display_point((b.x1, b.y1));
        BBox {
            x0: ax.min(bx),
            y0: ay.min(by),
            x1: ax.max(bx),
            y1: ay.max(by),
        }
    }
}

#[derive(Clone)]
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
    dots: Vec<BBox>,
    /// The page's visible area — the crop box in render space. Content outside
    /// it is collected by no viewer and must not be collected here either:
    /// a book-spread scan's crop box shows one 510pt page while the
    /// neighbouring page's full text sits at x=510-947, and emitting it
    /// doubled the document ("we extract everything and filter later" — this
    /// is the later).
    page_box: BBox,
    /// The area the clips in force let paint reach, innermost last, each
    /// already cut to the one outside it. A shape is measured by the part of
    /// it this shows, not by its whole path; see [`Collector::visible`].
    clips: Vec<BBox>,
    /// Per-glyph `(font identity, ink ratio)`, parallel to `glyphs`, for the
    /// weight inference in [`infer_weights`]. Kept alongside rather than on
    /// `Glyph` because it is scaffolding: once weights are resolved it has no
    /// further use, and every consumer of `Glyph` would have to carry it.
    ink: Vec<Option<(u128, f64)>>,
    /// The letters a ligature joins, as the document draws them, kept across
    /// pages to read the ligatures it draws unmapped; see [`crate::ligature`].
    letters: crate::ligature::Letters,
    /// Glyphs drawn as U+FFFD, by index into `glyphs`, with their family and
    /// outline, to be read by shape once the document's letters are known.
    unread: Vec<(usize, String, kurbo::BezPath)>,
    /// The page's marked content as its streams begin it, for the
    /// ActualText the device is not told; see [`crate::marked`].
    marks: crate::marked::Marks,
    /// The marked content open now, innermost last: the outermost one that
    /// states ActualText carries it, with the glyphs drawn inside it.
    open: Vec<Option<Spoken>>,
    /// The symbol font each font the page draws with is, if any, by font,
    /// and whether the font program's own name was asked.
    symbol_fonts: std::collections::HashMap<u128, (Option<&'static str>, bool)>,
    /// Glyphs drawn in a symbol font, by index into `glyphs`, with the font.
    symbols: Vec<(usize, &'static str)>,
    /// The clips in force, as [`Collector::clips`], each a rectangle's
    /// box, or `None` for any other shape. An image no rectangle in force
    /// holds whole is cut ([`ImagePlacement::key`]).
    rect_clips: Vec<Option<BBox>>,
    /// The box of every path painted on the page, whatever it is taken for:
    /// what an image's box may show besides the image
    /// ([`unkey_what_shows_more`]).
    painted: Vec<BBox>,
    /// ActualText that restates its glyphs in another case only, with the
    /// end of what it covers, held until the page's words are drawn whole;
    /// see [`Collector::read_cased`].
    cased: Vec<(Spoken, usize)>,
}

/// Marked content whose ActualText stands for the glyphs drawn inside it.
struct Spoken {
    text: String,
    /// `glyphs.len()` when it began: the glyphs drawn inside it follow.
    start: usize,
    /// Of those, the ones that put ink on the page.
    inked: Vec<usize>,
}

/// Most glyphs one ActualText may be given to whole, as the cluster a
/// shaper drew for it: a few. A longer span stated otherwise than one
/// character a glyph is not a cluster, and its glyphs keep their own text.
const MAX_CLUSTER: usize = 8;

/// An ActualText as it can stand for ink: tabs and line breaks read as
/// spaces and other control characters dropped. `None` when what is left
/// says nothing but spacing and punctuation — a tab, a soft hyphen, a space
/// a producer marks over a leader, the dots of the leader itself — which
/// are the layout's to read from the page, or holds U+FFFD, which says
/// nothing at all.
fn spoken(text: &str) -> Option<String> {
    let s: String = text
        .chars()
        .filter_map(|c| match c {
            '\t' | '\n' | '\r' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    let format = |c: char| {
        matches!(
            c,
            '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}'
        )
    };
    let says = s.chars().any(|c| {
        !c.is_whitespace() && !format(c) && !c.is_ascii_punctuation() && !is_punctuation(c)
    });
    (says && !s.contains('\u{FFFD}')).then_some(s)
}

/// A punctuation mark outside ASCII: dashes, quotation marks, the ellipsis,
/// CJK and full-width stops and brackets.
fn is_punctuation(c: char) -> bool {
    matches!(
        c,
        '\u{2010}'..='\u{2027}' | '\u{2030}'..='\u{205E}' | '\u{3001}'..='\u{3003}' | '\u{3008}'..='\u{3011}'
            | '\u{FF01}'..='\u{FF0F}' | '\u{FF1A}'..='\u{FF20}' | '\u{00A1}' | '\u{00A7}' | '\u{00AB}'
            | '\u{00B6}' | '\u{00B7}' | '\u{00BB}' | '\u{00BF}'
    )
}

impl Collector {
    /// The part of a drawn shape's box that can be painted: the box cut to
    /// the clips in force and to the page, or `None` when none of it shows.
    ///
    /// A PDF often draws more than it shows. A polar chart strokes whole
    /// circles and clips them to its quarter; artwork larger than the page is
    /// clipped to it; a wiring diagram's wires run past its frame. Measured
    /// by its whole path, one such circle reached 70pt above a spec sheet's
    /// page and across its text column, and one page-sized path joined every
    /// shape on a catalogue page into a single cluster, too large to be a
    /// drawing, so the page's five dimension drawings had no region at all.
    fn visible(&self, b: BBox) -> Option<BBox> {
        let area = self.clips.last().copied().unwrap_or(self.page_box);
        let cut = BBox {
            x0: b.x0.max(area.x0).max(self.page_box.x0),
            y0: b.y0.max(area.y0).max(self.page_box.y0),
            x1: b.x1.min(area.x1).min(self.page_box.x1),
            y1: b.y1.min(area.y1).min(self.page_box.y1),
        };
        // A hairline has no area, so the test is on each side, not on area.
        (cut.x1 >= cut.x0 && cut.y1 >= cut.y0).then_some(cut)
    }
    /// Read the glyphs drawn in Symbol or Wingdings as the characters the
    /// font shows.
    ///
    /// Word gives such a glyph the font's own code plus 0xF000 as its text,
    /// a private-use character, and every reader of the text layer gets that
    /// code: `μmole` reads U+F06D `mole`, an equation's `=` U+F03D. Read once
    /// the page is drawn, because a span's ActualText may state the code
    /// after its glyphs are drawn.
    fn read_symbols(&mut self) {
        for &(i, font) in &self.symbols {
            let g = &mut self.glyphs[i];
            if g.text.chars().any(symbol_code) {
                g.text = g
                    .text
                    .chars()
                    .map(|c| {
                        symbol_code(c)
                            .then(|| symbols::char_for(font, c as u32))
                            .flatten()
                            .unwrap_or(c)
                    })
                    .collect();
            }
        }
    }

    /// Give the glyphs a span drew the text its ActualText states for them.
    ///
    /// Taken only where it corrects the glyphs' own text: when the span inks
    /// something, on one line, and says more than the glyphs say already. A
    /// producer also states ActualText for the case a style changed — the
    /// heading set in capitals says `ecent` over the drawn `ECENT`, and only
    /// over some of its letters — and there the drawn capitals are the text
    /// a reader sees, and stay.
    fn speak(&mut self, span: Spoken) {
        let end = self.glyphs.len();
        self.state(span, end, false);
    }

    /// Give the glyphs `span.start..end` the text `span` states. Text that
    /// differs from what is drawn in case alone is held for
    /// [`Collector::read_cased`] unless `cased`.
    fn state(&mut self, span: Spoken, end: usize, cased: bool) {
        let Some(said) = spoken(&span.text) else {
            return;
        };
        let Some(&first) = span.inked.first() else {
            return;
        };
        let lead = &self.glyphs[first];
        let size = span
            .inked
            .iter()
            .map(|&i| self.glyphs[i].font_size as f64)
            .fold(0.0, f64::max)
            .max(1.0);
        let (sin, cos) = (lead.rotation_deg as f64).to_radians().sin_cos();
        let one_line = span.inked.iter().all(|&i| {
            let g = &self.glyphs[i];
            let (dx, dy) = (g.origin.0 - lead.origin.0, g.origin.1 - lead.origin.1);
            g.rotation_bucket() == lead.rotation_bucket()
                && (dx * sin + dy * cos).abs() <= 0.5 * size
        });
        if !one_line {
            return;
        }
        let range = span.start..end;
        let drawn: String = self.glyphs[range.clone()]
            .iter()
            .map(|g| g.text.as_str())
            .collect();
        let solid = |s: &str| -> String {
            crate::text::normalize(s)
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect()
        };
        let (was, now) = (solid(&drawn), solid(&said));
        if was == now {
            return;
        }
        if was.to_lowercase() == now.to_lowercase() && !cased {
            self.cased.push((span, end));
            return;
        }
        let chars: Vec<char> = said.chars().collect();
        let letters: Vec<char> = chars
            .iter()
            .copied()
            .filter(|c| !c.is_whitespace())
            .collect();
        if chars.len() == range.len() {
            // One character a glyph, in drawing order.
            for (g, c) in self.glyphs[range.clone()].iter_mut().zip(&chars) {
                g.text = c.to_string();
            }
        } else if letters.len() == span.inked.len() {
            // One letter an inked glyph; the spaces drawn between keep
            // theirs.
            for (&i, c) in span.inked.iter().zip(&letters) {
                self.glyphs[i].text = c.to_string();
            }
        } else if span.inked.len() <= MAX_CLUSTER {
            // A cluster stated whole: the first mark carries it.
            for i in range.clone() {
                self.glyphs[i].text = if i == first {
                    said.trim().to_string()
                } else {
                    String::new()
                };
            }
        } else {
            return;
        }
        // Their text is stated now, not left to be read from their shape.
        self.unread.retain(|(i, ..)| !range.contains(i));
    }

    /// Take the ActualText that restates its glyphs in another case only
    /// where the word they are drawn in mixes its cases at their size.
    ///
    /// InDesign states lower case over some letters of a word drawn all in
    /// capitals (`p` over the P of REPORT), and there the capitals drawn are
    /// what a reader sees, so a statement in case alone was never taken. A
    /// small-capital face drawn with some letters' capital codes and some
    /// letters' lower-case ones says the other thing: the word is one case
    /// and its codes are not (`SCrewS`, `MAhlE`), and the ActualText over
    /// the odd letters is the word's own case (`Screws`, `MAHLE`). Whether
    /// a word mixes its cases is known only once it is drawn whole, so the
    /// page is read to its end first.
    fn read_cased(&mut self) {
        for (span, end) in std::mem::take(&mut self.cased) {
            if self.mixes_case(span.start, end) {
                self.state(span, end, true);
            }
        }
    }

    /// Whether the word drawn around the glyphs `start..end` holds letters
    /// of both cases at their size: the glyphs either side of them, drawn
    /// one after another on their baseline with no space between.
    fn mixes_case(&self, start: usize, end: usize) -> bool {
        let gs = &self.glyphs;
        let Some(lead) = gs[start..end].iter().find(|g| g.bbox.is_some()) else {
            return false;
        };
        if lead.rotation_bucket() != 0 {
            return false;
        }
        let size = lead.font_size.max(1.0) as f64;
        // `b` is drawn on from `a` within the word.
        let joins = |a: &Glyph, b: &Glyph| {
            let pen = a.origin.0 + a.advance.unwrap_or(0.0);
            a.bbox.is_some()
                && b.bbox.is_some()
                && !b.text.trim().is_empty()
                && b.rotation_bucket() == 0
                && (b.origin.1 - a.origin.1).abs() <= 0.3 * size
                && (b.origin.0 - pen).abs() <= 0.3 * size
        };
        let mut from = start;
        while from > 0 && joins(&gs[from - 1], &gs[from]) {
            from -= 1;
        }
        let mut to = end;
        while to < gs.len() && to > 0 && joins(&gs[to - 1], &gs[to]) {
            to += 1;
        }
        let at_size = gs[from..to]
            .iter()
            .filter(|g| (g.font_size as f64 - size).abs() <= 0.15 * size);
        let (mut upper, mut lower) = (false, false);
        for c in at_size.flat_map(|g| g.text.chars()) {
            upper |= c.is_uppercase();
            lower |= c.is_lowercase();
        }
        upper && lower
    }
}

impl<'a> Device<'a> for Collector {
    fn set_soft_mask(&mut self, _m: Option<hayro_interpret::SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _b: hayro_interpret::BlendMode) {}
    fn draw_path(
        &mut self,
        path: &kurbo::BezPath,
        transform: Affine,
        _paint: &hayro_interpret::Paint<'a>,
        mode: &hayro_interpret::PathDrawMode,
    ) {
        // Bounding box only. A table grid is axis-aligned rectangles and
        // segments; curve detail is irrelevant and expensive to keep.
        let path = transform * path.clone();
        let r: Rect = path.bounding_box();
        let drawn = crate::geom::from_kurbo(r);
        if !self.page_box.intersects(&drawn) {
            return;
        }
        self.painted.push(drawn);
        if matches!(mode, hayro_interpret::PathDrawMode::Fill(_)) && rule::is_dot(&drawn) {
            if let Some(seen) = self.visible(drawn) {
                self.dots.push(seen);
            }
        }
        // One path can carry many rules. A financial statement strokes every
        // underline on the page — a short rule under each figure, the double
        // rule under each total — as subpaths of a single path (`m l m l …
        // S`), and that path's box is a block spanning all of them: read
        // whole it was one fill, and the page had no rules. Where every
        // subpath is a rule on its own, each is pushed as one.
        if let Some(rules) = subpath_rules(&path, self.page) {
            self.rules.extend(
                rules
                    .into_iter()
                    .filter(|x| self.page_box.intersects(&x.bbox)),
            );
            return;
        }
        // A rule is kept as drawn: table structure reads a grid's ruling
        // whole, and a table broken by the page foot draws its next row's
        // rules on under the clip. A filled shape is measured by what shows
        // of it; see [`Collector::visible`].
        // A path filling several rectangles apart, a table's shaded cells
        // drawn as one, is a fill each: read whole, its box covered the
        // table, and the bands it shades were none.
        if matches!(mode, hayro_interpret::PathDrawMode::Fill(_)) {
            if let Some(boxes) = subpath_fills(&path) {
                for b in boxes {
                    match rule::classify(b, self.page) {
                        Some(rule::Shape::Rule(x)) => self.rules.push(x),
                        Some(rule::Shape::Fill(x)) => {
                            if let Some(bbox) = self.visible(x.bbox) {
                                self.fills.push(rule::Fill { bbox, ..x });
                            }
                        }
                        None => {}
                    }
                }
                return;
            }
        }
        match rule::classify(drawn, self.page) {
            Some(rule::Shape::Rule(x)) => self.rules.push(x),
            Some(rule::Shape::Fill(x)) => {
                if let Some(bbox) = self.visible(x.bbox) {
                    self.fills.push(rule::Fill { bbox, ..x });
                }
            }
            None => {}
        }
    }
    fn push_clip_path(&mut self, c: &hayro_interpret::ClipPath) {
        // A clip's path is in page space already. It is a rectangle when it
        // is one closed figure filling its box: an axis-aligned rectangle,
        // the only clip whose box says what it lets through.
        let r = c.path.bounding_box();
        let figures = (c.path.elements().iter())
            .filter(|e| matches!(e, kurbo::PathEl::MoveTo(_)))
            .count();
        let rect = figures == 1 && r.area() > 0.0 && c.path.area().abs() >= r.area() * 0.999;
        self.rect_clips
            .push(rect.then(|| crate::geom::from_kurbo(r)));
        let outer = self.clips.last().copied().unwrap_or(self.page_box);
        let b = crate::geom::from_kurbo(c.path.bounding_box());
        self.clips.push(BBox {
            x0: outer.x0.max(b.x0),
            y0: outer.y0.max(b.y0),
            x1: outer.x1.min(b.x1),
            y1: outer.y1.min(b.y1),
        });
    }
    fn push_transparency_group(
        &mut self,
        _o: f32,
        _m: Option<hayro_interpret::SoftMask<'a>>,
        _b: hayro_interpret::BlendMode,
    ) {
    }
    fn draw_image(&mut self, i: hayro_interpret::Image<'a, '_>, t: Affine) {
        // The transform maps the image's *pixel* space onto the page.
        let grid = (i.width(), i.height());
        let (w, h) = (grid.0 as f64, grid.1 as f64);
        let r: Rect = (t * Rect::new(0.0, 0.0, w, h).to_path(0.1)).bounding_box();
        let b = crate::geom::from_kurbo(r);
        if !self.page_box.intersects(&b) {
            return;
        }
        // Pixels per inch along each axis: its pixels over the length that
        // axis is drawn at, in inches. An image squeezed one way is only as
        // fine as its coarser axis.
        let origin = t * kurbo::Point::new(0.0, 0.0);
        let across = (t * kurbo::Point::new(w, 0.0) - origin).hypot();
        let down = (t * kurbo::Point::new(0.0, h) - origin).hypot();
        let ppi =
            (across > 0.0 && down > 0.0).then(|| (w / (across / 72.0)).min(h / (down / 72.0)));
        let bitonal = match &i {
            hayro_interpret::Image::Stencil(_) => true,
            hayro_interpret::Image::Raster(ri) => {
                let dict = ri.stream().dict();
                // An inline image abbreviates its keys (`/BPC`, `/F`).
                [b"BitsPerComponent".as_slice(), b"BPC".as_slice()]
                    .iter()
                    .any(|k| dict.get::<i32>(*k) == Some(1))
                    || one_bit_filter(dict)
            }
        };
        let texty = match &i {
            hayro_interpret::Image::Raster(ri) => {
                let mut texty = false;
                // The decoder honours a target size only for JPEG 2000:
                // a Flate or DCT image is decoded, and probed, at its own
                // size, a cost that grows with its pixels and stays under
                // the decode's (`fluree_doc_vision::texty`).
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
        // A clip that cuts the image shows part of it: another picture than
        // the whole, which a key does not tell apart (a sprite sheet drawn
        // through two clips). Measured on the image's full box, before the
        // page's edge cuts it, which the crop's size tells.
        let cut = self.rect_clips.iter().any(|c| {
            c.as_ref().is_none_or(|c| {
                c.x0 > b.x0 + OVERLAP_EPS
                    || c.y0 > b.y0 + OVERLAP_EPS
                    || c.x1 < b.x1 - OVERLAP_EPS
                    || c.y1 < b.y1 - OVERLAP_EPS
            })
        });
        let key = if cut { None } else { image_key(&i, t) };
        self.images.push(ImagePlacement {
            bbox: BBox {
                x0: b.x0.max(self.page_box.x0),
                y0: b.y0.max(self.page_box.y0),
                x1: b.x1.min(self.page_box.x1),
                y1: b.y1.min(self.page_box.y1),
            },
            texty,
            ppi,
            bitonal,
            grid,
            key,
        });
    }
    fn pop_clip_path(&mut self) {
        self.clips.pop();
        self.rect_clips.pop();
    }
    fn pop_transparency_group(&mut self) {}

    fn begin_marked_content(&mut self, tag: &[u8], mcid: Option<i32>) {
        let actual = self.marks.begin(tag, mcid);
        let outer = self.open.iter().any(Option::is_some);
        self.open.push(match actual {
            Some(text) if !outer => Some(Spoken {
                text,
                start: self.glyphs.len(),
                inked: Vec::new(),
            }),
            _ => None,
        });
    }

    fn end_marked_content(&mut self) {
        if let Some(Some(span)) = self.open.pop() {
            self.speak(span);
        }
    }

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

        // Drawn in Symbol or Wingdings: its text is read as that font's
        // characters once it is final (see `read_symbols`). The font is
        // known by the name the PDF gives it, or, where a producer renamed
        // its fonts (`CIDFont+F4`) and the glyph says a symbol font's code,
        // by the name inside the font program.
        let symbol_font = match glyph {
            HGlyph::Outline(o) => {
                let (font, asked) =
                    self.symbol_fonts
                        .entry(o.font_cache_key())
                        .or_insert_with(|| {
                            let named = o.font_data().and_then(|f| f.postscript_name);
                            (named.as_deref().and_then(symbols::pdf_font), false)
                        });
                if font.is_none() && !*asked && text.chars().any(symbol_code) {
                    *asked = true;
                    *font = o
                        .font_data()
                        .and_then(|f| program_name((*f.data).as_ref()))
                        .as_deref()
                        .and_then(symbols::pdf_font);
                }
                *font
            }
            _ => None,
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

        // The letters a ligature is made of, and a glyph whose characters
        // the document left unknown, kept by family to be compared once the
        // whole document is read.
        let mut unread = None;
        if let HGlyph::Outline(o) = glyph {
            let lost = text == "\u{FFFD}";
            if lost || crate::ligature::is_ligature_letter(&text) {
                let family = self.letters.family(o.font_cache_key(), || {
                    o.font_data().and_then(|f| f.postscript_name)
                });
                if let Some(family) = family {
                    if lost {
                        unread = Some((family, o.outline()));
                    } else if let Some(c) = text
                        .chars()
                        .next()
                        .filter(|c| self.letters.wants(&family, *c))
                    {
                        let advance = o.advance_width().unwrap_or(0.0) as f64;
                        self.letters.keep(family, c, o.outline(), advance);
                    }
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
        if let Some((family, outline)) = unread {
            self.unread.push((draw_index, family, outline));
        }
        // A glyph inside a span that states its text: note it if it puts
        // ink down. A Type3 glyph has no box yet but draws all the same,
        // unless it is a space.
        let inks = bbox.is_some()
            || (!matches!(glyph, HGlyph::Outline(_))
                && (text.is_empty() || !text.trim().is_empty()));
        if inks {
            if let Some(span) = self.open.iter_mut().flatten().next() {
                span.inked.push(draw_index);
            }
        }
        // A soft hyphen drawn with ink is the hyphen it prints. Some
        // producers map every hyphen they draw to U+00AD, a compound's and a
        // code's (`60-70`, `MESA-approved`) as well as the one a line break
        // set (`microcom-` over `puter`), so it is read as `-`, and a line
        // ending on it joins the next by the rule for a printed hyphen. One
        // that draws nothing shows nothing.
        if text.contains('\u{AD}') {
            text = text.replace('\u{AD}', if inks { "-" } else { "" });
        }
        if let Some(font) = symbol_font {
            self.symbols.push((draw_index, font));
        }
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

/// Most pixels a stencil is decoded for its key ([`ImagePlacement::key`]):
/// a 512 × 512 mask. A stencil's encoded stream is out of reach, so its key
/// is its decoded mask; what a page draws again and again (a checkbox, a
/// bullet, a logo) is small, and a stencil the size of a scanned page is
/// not decoded twice for a key it would rarely share.
const MAX_KEYED_STENCIL_PX: u64 = 512 * 512;

/// How far, in PDF units, two boxes must reach into each other to overlap,
/// or a clip inside an image's edge to cut it: past the rounding of one
/// `cm` against another, so a label set beside a checkbox, or a clip drawn
/// on the image's own edge, is not taken for one drawn over it.
const OVERLAP_EPS: f64 = 0.01;

/// Whether two boxes share an area, not only an edge.
fn overlap(a: &BBox, b: &BBox) -> bool {
    a.x0.max(b.x0) + OVERLAP_EPS < a.x1.min(b.x1) && a.y0.max(b.y0) + OVERLAP_EPS < a.y1.min(b.y1)
}

/// Cells a side of the grid [`unkey_what_shows_more`] files what a page
/// draws on, so that an image is measured against what is drawn near it,
/// not against every glyph, path and image of a page that may hold
/// thousands of each.
const SHOWS_MORE_BINS: usize = 64;

/// What [`unkey_what_shows_more`] looks for in an image's box.
enum Drawn {
    /// A glyph's ink or a painted path.
    Area(BBox),
    /// A glyph with text and no box, by where it is set.
    Point(f64, f64),
    /// Another image, by its index.
    Image(usize),
}

/// Drops the key of each image whose box shows more than the image
/// ([`ImagePlacement::key`]): a glyph with ink in it (or, with no box,
/// set in it or on its edge), a path painted over it or under it, another
/// image drawn across it. An image drawn again in its very box with its key
/// is the same picture, and keeps it. Where an OCR reading's words go is not
/// looked at: a page has none while it is read. Run once the glyphs a font
/// draws no outline for (Type3) have the box layout reads them by
/// ([`synthesize_missing_boxes`]).
fn unkey_what_shows_more(
    page_box: &BBox,
    images: &mut [ImagePlacement],
    glyphs: &[Glyph],
    marks: &[BBox],
) {
    let keyed: Vec<usize> = (0..images.len())
        .filter(|&i| images[i].key.is_some())
        .collect();
    if keyed.is_empty() {
        return;
    }
    let cell = (
        page_box.width().max(1.0) / SHOWS_MORE_BINS as f64,
        page_box.height().max(1.0) / SHOWS_MORE_BINS as f64,
    );
    let bin = |v: f64, origin: f64, size: f64| {
        (((v - origin) / size).floor().max(0.0) as usize).min(SHOWS_MORE_BINS - 1)
    };
    let span = |b: &BBox| {
        let cols = bin(b.x0, page_box.x0, cell.0)..=bin(b.x1, page_box.x0, cell.0);
        let rows = bin(b.y0, page_box.y0, cell.1)..=bin(b.y1, page_box.y0, cell.1);
        (cols, rows)
    };
    let cells = |b: &BBox| {
        let (cols, rows) = span(b);
        rows.flat_map(move |r| cols.clone().map(move |c| r * SHOWS_MORE_BINS + c))
    };
    // Only the keyed images are filed, a few on most pages, and what the
    // page draws is measured against those of the cells it covers: a page
    // may paint paths by the hundred thousand, each across the page.
    let mut grid: Vec<Vec<usize>> = vec![Vec::new(); SHOWS_MORE_BINS * SHOWS_MORE_BINS];
    for &i in &keyed {
        for c in cells(&images[i].bbox) {
            grid[c].push(i);
        }
    }
    let drawn = (glyphs.iter())
        .filter_map(|g| match g.bbox {
            Some(b) => Some((b, Drawn::Area(b))),
            None => (!g.text.trim().is_empty()).then(|| {
                let (x, y) = g.origin;
                let at = BBox {
                    x0: x,
                    y0: y,
                    x1: x,
                    y1: y,
                };
                (at, Drawn::Point(x, y))
            }),
        })
        .chain(marks.iter().map(|m| (*m, Drawn::Area(*m))))
        .chain((images.iter().enumerate()).map(|(j, o)| (o.bbox, Drawn::Image(j))));
    let shows = |i: usize, d: &Drawn| {
        let b = &images[i].bbox;
        match d {
            Drawn::Area(m) => overlap(b, m),
            // Set on the image's edge, a glyph with no box still has its
            // letter over it.
            Drawn::Point(x, y) => b.x0 <= *x && *x <= b.x1 && b.y0 <= *y && *y <= b.y1,
            Drawn::Image(j) => {
                let o = &images[*j];
                *j != i && overlap(b, &o.bbox) && !(o.key == images[i].key && same_box(b, &o.bbox))
            }
        }
    };
    let mut shows_more = vec![false; images.len()];
    let mut left = keyed.len();
    for (b, d) in drawn {
        let mut test = |i: usize| {
            if !shows_more[i] && shows(i, &d) {
                shows_more[i] = true;
                left -= 1;
            }
        };
        let (cols, rows) = span(&b);
        // Across more cells than there are keyed images, the list is
        // shorter than the cells' lists.
        if cols.count() * rows.count() > keyed.len() {
            keyed.iter().for_each(|&i| test(i));
        } else {
            cells(&b).for_each(|c| grid[c].iter().for_each(|&i| test(i)));
        }
        if left == 0 {
            break;
        }
    }
    for (i, more) in shows_more.into_iter().enumerate() {
        if more {
            images[i].key = None;
        }
    }
}

/// Whether two boxes are one, to the rounding [`image_key`] allows.
fn same_box(a: &BBox, b: &BBox) -> bool {
    [a.x0 - b.x0, a.y0 - b.y0, a.x1 - b.x1, a.y1 - b.y1]
        .iter()
        .all(|d| d.abs() <= OVERLAP_EPS)
}

/// Where a box of a page `w` × `h` lands once the page is turned by `turn`
/// degrees, as [`turn_page`] turns it.
fn turn_box(w: f64, h: f64, turn: i32, b: BBox) -> BBox {
    let (ax, ay) = turn_point(w, h, turn, (b.x0, b.y0));
    let (bx, by) = turn_point(w, h, turn, (b.x1, b.y1));
    BBox {
        x0: ax.min(bx),
        y0: ay.min(by),
        x1: ax.max(bx),
        y1: ay.max(by),
    }
}

/// [`ImagePlacement::key`] of image `i` drawn with transform `t`.
fn image_key(i: &hayro_interpret::Image<'_, '_>, t: Affine) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    match i {
        hayro_interpret::Image::Raster(ri) => {
            0u8.hash(&mut h);
            ri.stream().dict().data().hash(&mut h);
            ri.stream().raw_data().hash(&mut h);
        }
        hayro_interpret::Image::Stencil(si) => {
            let pixels = u64::from(si.width()) * u64::from(si.height());
            if pixels > MAX_KEYED_STENCIL_PX {
                return None;
            }
            let mut mask = None;
            si.with_stencil(|luma, _| mask = Some(luma), None);
            let luma = mask?;
            1u8.hash(&mut h);
            (luma.width, luma.height, &luma.data).hash(&mut h);
        }
    }
    // Drawn at another size, turned or mirrored, the same image shows other
    // pixels; where on the page it is drawn is not part of what it shows.
    // To a thousandth of a unit, so the rounding of one `cm` against
    // another does not tell a box from itself.
    let [a, b, c, d, _, _] = t.as_coeffs();
    [a, b, c, d]
        .map(|v| (v * 1000.0).round() as i64)
        .hash(&mut h);
    Some(h.finish())
}

/// Decode size asked for the pixel probe. Only a JPEG 2000 image is decoded
/// to it, at a reduced resolution level; every other filter decodes at the
/// image's own size, and the probe looks at all of it.
const PROBE_DIM: u32 = 256;

/// The rules a path of several straight subpaths draws, one per subpath, when
/// every subpath is a horizontal rule a figure wide; `None` for a path of one
/// subpath, a curve, or any other subpath (a box, a cell background, a
/// chart's tick marks, which are short, and its axes, which stand up), read
/// whole as before.
/// Shortest underline read out of a path of several, in points: a narrow
/// figure's width, longer than a chart's tick.
const MIN_UNDERLINE: f64 = 8.0;

fn subpath_rules(path: &kurbo::BezPath, page: usize) -> Option<Vec<Rule>> {
    let mut boxes: Vec<Rect> = Vec::new();
    let mut current: Option<Rect> = None;
    for el in path.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => {
                boxes.extend(current.take());
                current = Some(Rect::from_points(p, p));
            }
            kurbo::PathEl::LineTo(p) => {
                current = Some(current?.union_pt(p));
            }
            kurbo::PathEl::ClosePath => {}
            kurbo::PathEl::QuadTo(..) | kurbo::PathEl::CurveTo(..) => return None,
        }
    }
    boxes.extend(current);
    if boxes.len() < 2 {
        return None;
    }
    // Underlines under columns of figures: some level carries two rules or
    // more, apart. A chart's gridlines are one rule a level, the plot's
    // width each, and read whole they are the fill that finds the chart.
    let parted = boxes.iter().enumerate().any(|(i, a)| {
        boxes[i + 1..]
            .iter()
            .any(|b| (a.y0 - b.y0).abs() <= 1.0 && (a.x1 < b.x0 - 1.0 || b.x1 < a.x0 - 1.0))
    });
    if !parted {
        return None;
    }
    boxes
        .into_iter()
        .map(|b| match rule::classify(crate::geom::from_kurbo(b), page) {
            Some(rule::Shape::Rule(x))
                if x.orientation == rule::Orientation::Horizontal
                    && x.length() >= MIN_UNDERLINE =>
            {
                Some(x)
            }
            _ => None,
        })
        .collect()
}

/// The cells a filled path of several straight subpaths draws, one per
/// subpath, when every subpath is an upright rectangle too thick to be a
/// rule, set beside another on its line touching it, none lies inside
/// another, and the bands they make lie rows apart (a table's sections);
/// `None` otherwise (one subpath, a curve, a glyph's outline, bars, a
/// chart's columns, stripes down a table, a frame drawn as a rectangle
/// inside a rectangle), read whole as before.
fn subpath_fills(path: &kurbo::BezPath) -> Option<Vec<crate::geom::BBox>> {
    let mut subpaths: Vec<Vec<kurbo::Point>> = Vec::new();
    for el in path.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => subpaths.push(vec![p]),
            kurbo::PathEl::LineTo(p) => subpaths.last_mut()?.push(p),
            kurbo::PathEl::ClosePath => {}
            kurbo::PathEl::QuadTo(..) | kurbo::PathEl::CurveTo(..) => return None,
        }
    }
    if subpaths.len() < 2 {
        return None;
    }
    let mut boxes: Vec<Rect> = Vec::with_capacity(subpaths.len());
    for pts in &subpaths {
        if !(4..=5).contains(&pts.len()) {
            return None;
        }
        let r = pts
            .iter()
            .fold(Rect::from_points(pts[0], pts[0]), |r, &p| r.union_pt(p));
        // Each point a corner of its box: an upright rectangle.
        let corner = |p: &kurbo::Point| {
            ((p.x - r.x0).abs() < 0.5 || (p.x - r.x1).abs() < 0.5)
                && ((p.y - r.y0).abs() < 0.5 || (p.y - r.y1).abs() < 0.5)
        };
        // A cell's shading, too thick either way to be a rule: bars drawn
        // as one path (a glyph's `=` outlined) are read whole, as before.
        if !pts.iter().all(corner)
            || r.width() <= rule::MAX_RULE_THICKNESS
            || r.height() <= rule::MAX_RULE_THICKNESS
        {
            return None;
        }
        boxes.push(r);
    }
    let inside = |a: &Rect, b: &Rect| {
        a.x0 >= b.x0 - 0.5 && a.x1 <= b.x1 + 0.5 && a.y0 >= b.y0 - 0.5 && a.y1 <= b.y1 + 0.5
    };
    let nested = boxes.iter().enumerate().any(|(i, a)| {
        boxes
            .iter()
            .enumerate()
            .any(|(j, b)| i != j && inside(a, b))
    });
    // Cells: each rectangle side by side with another on its line, touching
    // it. A chart's bars stand apart, and a table's stripes are one a line;
    // both are read whole, as before.
    let beside = |a: &Rect, b: &Rect| {
        (a.y0 - b.y0).abs() <= 1.0
            && (a.y1 - b.y1).abs() <= 1.0
            && ((a.x1 - b.x0).abs() <= 2.0 || (b.x1 - a.x0).abs() <= 2.0)
    };
    let cells = boxes.iter().enumerate().all(|(i, a)| {
        boxes
            .iter()
            .enumerate()
            .any(|(j, b)| i != j && beside(a, b))
    });
    if nested || !cells {
        return None;
    }
    // Bands set rows apart, a section's or a header's; not stripes a row
    // or so apart down a table, which are read whole, as before.
    let mut bands: Vec<(f64, f64)> = Vec::new();
    for b in &boxes {
        if !bands
            .iter()
            .any(|(y0, y1)| (y0 - b.y0).abs() <= 1.0 && (y1 - b.y1).abs() <= 1.0)
        {
            bands.push((b.y0, b.y1));
        }
    }
    bands.sort_by(|a, b| a.0.total_cmp(&b.0));
    if bands
        .windows(2)
        .any(|w| w[1].0 - w[0].1 <= 1.5 * (w[0].1 - w[0].0).max(w[1].1 - w[1].0))
    {
        return None;
    }
    Some(boxes.into_iter().map(crate::geom::from_kurbo).collect())
}

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
    let texty = fluree_doc_vision::texty::looks_like_text(w, h, &luma);
    if std::env::var_os("FDOC_PROBE_DEBUG").is_some() {
        eprintln!("PROBE texty={texty} {w}x{h}");
    }
    texty
}

/// A private-use character Word gives a glyph set in one of its symbol
/// fonts: the font's own code, `0x20`–`0xFF`, plus `0xF000`.
fn symbol_code(c: char) -> bool {
    ('\u{F020}'..='\u{F0FF}').contains(&c)
}

/// The PostScript name a TrueType or OpenType font program gives itself.
fn program_name(data: &[u8]) -> Option<String> {
    use read_fonts::{types::NameId, FontRef, TableProvider};
    let font = FontRef::new(data).ok()?;
    let name = font.name().ok()?;
    name.name_record()
        .iter()
        .filter(|r| r.name_id() == NameId::POSTSCRIPT_NAME)
        .find_map(|r| r.string(name.string_data()).ok())
        .map(|s| s.chars().collect())
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
    // A full stop: one small round mark resting on the baseline. Dot
    // leaders are set in it, and a producer that maps the leader's glyph to
    // U+FFFD leaves a run of unknowns between a table's label and its
    // figures. In the font's 1000-unit em a stop is about a tenth of it
    // each way; a middle dot sits higher, an ellipsis is three marks.
    if text == "\u{FFFD}"
        && subpaths == 1
        && w <= 250.0
        && h <= 250.0
        && (0.6 * h..=1.7 * h).contains(&w)
        && (-40.0..=60.0).contains(&b.y0)
    {
        return Some(".");
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
    // reference upward and hide the very contrast being looked for. Two faces
    // setting as many letters are told apart by their ink, the lighter being
    // the body, and then by font, so the choice is the same on every run.
    let (_, &(body_n, body_sum)) = stats
        .iter()
        .max_by(|(ka, (na, sa)), (kb, (nb, sb))| {
            na.cmp(nb)
                .then_with(|| (sb / *nb as f64).total_cmp(&(sa / *na as f64)))
                .then_with(|| kb.cmp(ka))
        })
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
    // The interpreter's warnings, gathered per page: by default they go
    // nowhere, and an image that failed to decode is placed all the same.
    let heard: Arc<std::sync::Mutex<Vec<&'static str>>> = Arc::default();
    let settings = InterpreterSettings {
        warning_sink: {
            let heard = Arc::clone(&heard);
            Arc::new(move |w| {
                if let Ok(mut h) = heard.lock() {
                    h.push(warning_name(w));
                }
            })
        },
        ..InterpreterSettings::default()
    };
    let mut pages = Vec::new();
    let mut ink: Vec<Vec<Option<(u128, f64)>>> = Vec::new();
    let mut letters = crate::ligature::Letters::default();
    let mut unread: Vec<(usize, usize, String, kurbo::BezPath)> = Vec::new();

    for (index, page) in pdf.pages().iter().enumerate() {
        let (w, h) = page.render_dimensions();
        let mut dev = Collector {
            page: index,
            glyphs: Vec::new(),
            images: Vec::new(),
            rules: Vec::new(),
            fills: Vec::new(),
            dots: Vec::new(),
            ink: Vec::new(),
            letters: std::mem::take(&mut letters),
            unread: Vec::new(),
            marks: crate::marked::Marks::read(page),
            open: Vec::new(),
            symbol_fonts: std::collections::HashMap::new(),
            symbols: Vec::new(),
            clips: Vec::new(),
            rect_clips: Vec::new(),
            painted: Vec::new(),
            cased: Vec::new(),
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
        // A span the page left open still covers what it drew.
        while let Some(open) = dev.open.pop() {
            if let Some(span) = open {
                dev.speak(span);
            }
        }
        dev.read_cased();
        dev.read_symbols();
        letters = std::mem::take(&mut dev.letters);
        unread.extend(dev.unread.drain(..).map(|(i, f, o)| (index, i, f, o)));
        let painted = std::mem::take(&mut dev.painted);
        let warnings = heard
            .lock()
            .map(|mut h| h.drain(..).collect())
            .unwrap_or_default();
        let mut page = Page {
            index,
            glyphs: dev.glyphs,
            images: dev.images,
            rules: dev.rules,
            routing: None,
            fills: dev.fills,
            dots: dev.dots,
            width: w as f64,
            height: h as f64,
            rotation: 0,
            ocr: None,
            warnings,
        };
        read_upright(&mut page);
        synthesize_missing_boxes(&mut page.glyphs);
        // The paths' boxes, turned as the page was.
        let painted: Vec<BBox> = match page.rotation {
            0 => painted,
            turn => (painted.into_iter())
                .map(|b| turn_box(w as f64, h as f64, turn, b))
                .collect(),
        };
        let page_box = BBox {
            x0: 0.0,
            y0: 0.0,
            x1: page.width,
            y1: page.height,
        };
        unkey_what_shows_more(&page_box, &mut page.images, &page.glyphs, &painted);
        ink.push(dev.ink);
        pages.push(page);
    }
    infer_weights(&mut pages, &ink);
    // Read the unmapped glyphs by their shape, now that every letter the
    // document draws is known. Glyph order is fixed from here on, so an
    // index still names the glyph it was taken from.
    for (page, i, family, outline) in unread {
        if let Some(name) = letters.name(&family, &outline) {
            if let Some(g) = pages[page].glyphs.get_mut(i) {
                g.text = name.to_string();
            }
        }
    }

    Ok(Document { pages })
}

/// The name a warning is reported by, as the interpreter names it.
fn warning_name(w: hayro_interpret::InterpreterWarning) -> &'static str {
    use hayro_interpret::InterpreterWarning as W;
    match w {
        W::UnsupportedFont => "UnsupportedFont",
        W::ImageDecodeFailure => "ImageDecodeFailure",
    }
}

/// Whether an image stream is coded by a filter that only codes one bit per
/// pixel: CCITT fax or JBIG2. Such a stream may leave out its bit depth.
fn one_bit_filter(dict: &hayro_syntax::object::Dict) -> bool {
    use hayro_syntax::object::{Array, Name};
    let one_bit = |n: &Name| matches!(&**n, b"CCITTFaxDecode" | b"CCF" | b"JBIG2Decode");
    [b"Filter".as_slice(), b"F".as_slice()]
        .iter()
        .any(|filter| {
            dict.get::<Name>(*filter).is_some_and(|n| one_bit(&n))
                || dict
                    .get::<Array>(*filter)
                    .is_some_and(|a| a.iter::<Name>().any(|n| one_bit(&n)))
        })
}

/// Share of a page's text glyphs that must sit in one non-horizontal
/// quarter-turn bucket before the page is read in that bucket's frame.
///
/// High on purpose: a page is turned as a whole, and a page that mixes a
/// sideways table with upright prose is not sideways — its rotated run is
/// handled as one by line assembly, and turning the page would rotate the
/// prose instead. Only a page whose text all runs one way is a turned page.
const UPRIGHT_SHARE: f64 = 0.85;

/// Fewer text glyphs than this and the page's orientation is not evidenced:
/// a lone rotated label on an otherwise empty page is a label, not a page.
const UPRIGHT_MIN_GLYPHS: usize = 12;

/// Turn a page whose text runs one non-horizontal way into the frame where
/// it reads upright.
///
/// A `/Rotate 90` page whose content was drawn upright displays sideways —
/// a viewer honours the rotation, and so does the glyph transform here, so
/// the page arrives with every glyph in the −90° bucket. Left there it is
/// assembled as one rotated run per column: every string reversed, the
/// tables one seven-point-wide figure. A person reading that page turns
/// it; this does the same, once, for every coordinate the page carries, and
/// records the turn in [`Page::rotation`] so the output's boxes can be
/// mapped back to the display frame.
///
/// Decided per page from the glyphs themselves, not from `/Rotate`: the
/// rotation entry is already applied by the time the glyphs exist, and the
/// same reading problem arises without it — a landscape drawing bound into
/// a portrait document and drawn sideways carries no `/Rotate` at all.
fn read_upright(page: &mut Page) {
    let mut by_bucket: [usize; 4] = [0; 4];
    let mut total = 0usize;
    for g in &page.glyphs {
        if g.text.trim().is_empty() {
            continue;
        }
        total += 1;
        match g.rotation_bucket().rem_euclid(360) {
            0 => by_bucket[0] += 1,
            90 => by_bucket[1] += 1,
            180 => by_bucket[2] += 1,
            270 => by_bucket[3] += 1,
            _ => {}
        }
    }
    if total < UPRIGHT_MIN_GLYPHS {
        return;
    }
    let (dominant, count) = by_bucket
        .iter()
        .enumerate()
        .max_by_key(|(_, n)| **n)
        .map(|(i, n)| (i, *n))
        .unwrap_or((0, 0));
    if dominant == 0 || (count as f64) < (total as f64) * UPRIGHT_SHARE {
        return;
    }
    // The glyphs' baselines run at `bucket` degrees counter-clockwise from
    // horizontal; turning the page by the opposite amount puts them at 0.
    // Text in the 270° bucket (baseline pointing down the page as
    // displayed) needs a quarter turn counter-clockwise; text in the 90°
    // bucket a quarter turn clockwise, which is 270° counter-clockwise.
    let turn = match dominant {
        1 => 270,
        2 => 180,
        _ => 90,
    };
    turn_page(page, turn);
}

/// Turn every coordinate a page carries by `turn` degrees (90, 180 or 270)
/// counter-clockwise, and record the turn in [`Page::rotation`], on top of
/// any it had, so the display frame stays where it was.
pub(crate) fn turn_page(page: &mut Page, turn: i32) {
    let (w, h) = (page.width, page.height);
    let point = |p: (f64, f64)| turn_point(w, h, turn, p);
    let rect = |b: BBox| turn_box(w, h, turn, b);
    for g in page.glyphs.iter_mut() {
        g.origin = point(g.origin);
        g.bbox = g.bbox.map(rect);
        let mut deg = g.rotation_deg + turn as f32;
        while deg > 180.0 {
            deg -= 360.0;
        }
        g.rotation_deg = deg;
    }
    for r in page.rules.iter_mut() {
        r.bbox = rect(r.bbox);
        if turn != 180 {
            r.orientation = match r.orientation {
                rule::Orientation::Horizontal => rule::Orientation::Vertical,
                rule::Orientation::Vertical => rule::Orientation::Horizontal,
            };
        }
    }
    for f in page.fills.iter_mut() {
        f.bbox = rect(f.bbox);
    }
    for d in page.dots.iter_mut() {
        *d = rect(*d);
    }
    for i in page.images.iter_mut() {
        i.bbox = rect(i.bbox);
    }
    if turn != 180 {
        page.width = h;
        page.height = w;
    }
    page.rotation = (page.rotation + turn).rem_euclid(360);
}

/// Where a point of a page `w` × `h` lands once the page is turned by `turn`
/// degrees (90, 180 or 270) counter-clockwise, as [`turn_page`] turns it.
pub(crate) fn turn_point(w: f64, h: f64, turn: i32, (x, y): (f64, f64)) -> (f64, f64) {
    match turn {
        90 => (y, w - x),
        180 => (w - x, h - y),
        _ => (h - y, x),
    }
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

/// Widest a glyph's advance is taken to be, in sizes, when measured to the
/// next glyph: a CJK character, `ﬃ` or `W` runs about one. Further on, the
/// pen crossed a word space no space glyph was drawn for (a deck's
/// `assume` and the `1b` set after it in another font), and the fallback
/// width leaves that space to be read.
const MAX_SYNTH_ADVANCE: f64 = 1.25;

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
/// The next glyph is the next of any font. A browser prints a ligature or
/// a run of CJK it has no embeddable font for in Type3, among text set in
/// another font: measured to the next Type3 glyph only, the `ﬃ` of
/// `oﬃce` and the last character before a full stop took the fallback
/// width, and the rest of their advance read as a word space (`offi ce`,
/// `有限公司 .`).
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
    // Every upright glyph, by baseline: the neighbours a width is measured
    // to, of whatever font, explicit spaces among them.
    let mut by_y: Vec<usize> = (0..glyphs.len())
        .filter(|&i| glyphs[i].rotation_bucket().rem_euclid(180) == 0)
        .collect();
    by_y.sort_by(|&a, &b| glyphs[a].origin.1.total_cmp(&glyphs[b].origin.1));
    for i in candidates {
        let (x, y) = glyphs[i].origin;
        let font = glyphs[i].font_size.max(1.0) as f64;
        // The nearest pen position to the right on the same baseline
        // (within the tolerance assembly uses): the pen delta is the
        // advance, whatever is drawn there.
        let tol = font * 0.25;
        let from = by_y.partition_point(|&j| glyphs[j].origin.1 < y - tol);
        let width = by_y[from..]
            .iter()
            .take_while(|&&j| glyphs[j].origin.1 <= y + tol)
            .map(|&j| glyphs[j].origin.0 - x)
            .filter(|&d| d > 0.01 && d < font * MAX_SYNTH_ADVANCE)
            .min_by(f64::total_cmp)
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

    #[test]
    fn a_type3_glyph_is_as_wide_as_the_pen_moves_to_any_next_glyph() {
        // `oﬃce`: the ﬃ is Type3 (no box), the letters around it are not.
        let glyph = |text: &str, x: f64, boxed: bool| Glyph {
            text: text.into(),
            bbox: boxed.then_some(BBox {
                x0: x,
                y0: 44.0,
                x1: x + 6.0,
                y1: 52.5,
            }),
            page: 0,
            origin: (x, 52.5),
            rotation_deg: 0.0,
            font_size: 12.0,
            weight: None,
            advance: boxed.then_some(6.67),
            draw_index: 0,
        };
        let mut g = vec![
            glyph("o", 57.76, true),
            glyph("\u{FB03}", 64.44, false),
            glyph("c", 76.22, true),
            glyph("青", 216.95, false),
        ];
        synthesize_missing_boxes(&mut g);
        assert!((g[1].advance.unwrap() - 11.78).abs() < 0.01);
        // Nothing follows on the line: the fallback.
        assert!((g[3].advance.unwrap() - 12.0 * SYNTH_WIDTH).abs() < 0.01);
    }

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
            routing: None,
            fills: Vec::new(),
            dots: Vec::new(),
            width: 600.0,
            height: 800.0,
            rotation: 0,
            ocr: None,
            warnings: Vec::new(),
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
    fn of_two_faces_setting_as_many_letters_the_lighter_is_the_body() {
        for _ in 0..8 {
            let mut pages = vec![page(200)];
            infer_weights(&mut pages, &ink(100, 100, 0.65));
            assert!(pages[0].glyphs[0].weight.is_none());
            assert_eq!(pages[0].glyphs[199].weight, Some(700));
        }
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

    fn keyed_at(x0: f64, y0: f64, side: f64, key: u64) -> ImagePlacement {
        ImagePlacement {
            bbox: BBox {
                x0,
                y0,
                x1: x0 + side,
                y1: y0 + side,
            },
            texty: true,
            ppi: None,
            bitonal: false,
            grid: (40, 40),
            key: Some(key),
        }
    }

    const LETTER: BBox = BBox {
        x0: 0.0,
        y0: 0.0,
        x1: 612.0,
        y1: 792.0,
    };

    #[test]
    fn a_glyph_with_no_box_set_on_an_images_corner_shows_more_than_it() {
        // Two boxes from one stencil, and a glyph turned a quarter, which
        // no box is made for, set at the first one's very corner.
        let mut images = vec![
            keyed_at(50.0, 300.0, 20.0, 7),
            keyed_at(50.0, 400.0, 20.0, 7),
        ];
        let glyph = Glyph {
            text: "x".into(),
            bbox: None,
            page: 0,
            origin: (50.0, 320.0),
            rotation_deg: 90.0,
            font_size: 12.0,
            weight: None,
            advance: None,
            draw_index: 0,
        };
        unkey_what_shows_more(&LETTER, &mut images, &[glyph], &[]);
        assert_eq!(images[0].key, None);
        assert_eq!(images[1].key, Some(7));
    }

    #[test]
    fn paths_by_the_hundred_thousand_across_a_page_cost_little_beside_a_logo() {
        // A plot: 100,000 strokes, each across the whole page, so each
        // covers every cell, and a logo none crosses, in the margin. What is
        // drawn is measured against the one keyed image, not filed in every
        // cell it covers.
        let strokes: Vec<BBox> = (0..100_000)
            .map(|i| {
                let d = (i % 50) as f64;
                BBox {
                    x0: d,
                    y0: 100.0 + d,
                    x1: 562.0 + d,
                    y1: 700.0 + d,
                }
            })
            .collect();
        let mut images = vec![keyed_at(560.0, 20.0, 40.0, 7)];
        let t = std::time::Instant::now();
        unkey_what_shows_more(&LETTER, &mut images, &[], &strokes);
        let took = t.elapsed().as_secs_f64();
        assert_eq!(images[0].key, Some(7));
        assert!(took < 1.0, "100k strokes {took:.3}s");
        // A stroke across it drops its key.
        let mut images = vec![keyed_at(300.0, 300.0, 40.0, 7)];
        unkey_what_shows_more(&LETTER, &mut images, &[], &strokes);
        assert_eq!(images[0].key, None);
    }
}

#[cfg(test)]
mod actual_text_tests {
    use super::*;

    fn glyph(text: &str, x: f64, y: f64, inked: bool) -> Glyph {
        Glyph {
            text: text.into(),
            bbox: inked.then_some(BBox {
                x0: x,
                y0: y - 8.0,
                x1: x + 5.0,
                y1: y + 2.0,
            }),
            page: 0,
            origin: (x, y),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            advance: Some(5.0),
            draw_index: 0,
        }
    }

    /// The glyphs' text after a span stating `said` covers all of them.
    fn spoken_over(glyphs: Vec<Glyph>, said: &str) -> Vec<String> {
        let inked = (0..glyphs.len())
            .filter(|&i| glyphs[i].bbox.is_some())
            .collect();
        let mut c = Collector {
            page: 0,
            glyphs,
            images: Vec::new(),
            rules: Vec::new(),
            fills: Vec::new(),
            dots: Vec::new(),
            page_box: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 600.0,
                y1: 800.0,
            },
            clips: Vec::new(),
            ink: Vec::new(),
            letters: crate::ligature::Letters::default(),
            unread: Vec::new(),
            marks: crate::marked::Marks::default(),
            open: Vec::new(),
            symbol_fonts: std::collections::HashMap::new(),
            symbols: Vec::new(),
            rect_clips: Vec::new(),
            painted: Vec::new(),
            cased: Vec::new(),
        };
        c.speak(Spoken {
            text: said.into(),
            start: 0,
            inked,
        });
        c.read_cased();
        c.glyphs.into_iter().map(|g| g.text).collect()
    }

    /// A word drawn from `letters`, `(text, size)` each 5 wide on one
    /// baseline, after spans stating `(start, end, said)` over some of
    /// them, read to the page's end.
    fn cased_over(letters: &[(&str, f32)], spans: &[(usize, usize, &str)]) -> String {
        let glyphs: Vec<Glyph> = letters
            .iter()
            .enumerate()
            .map(|(i, (t, size))| Glyph {
                font_size: *size,
                ..glyph(t, i as f64 * 5.0, 10.0, true)
            })
            .collect();
        let mut c = Collector {
            page: 0,
            glyphs,
            images: Vec::new(),
            rules: Vec::new(),
            fills: Vec::new(),
            dots: Vec::new(),
            page_box: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 600.0,
                y1: 800.0,
            },
            clips: Vec::new(),
            ink: Vec::new(),
            letters: crate::ligature::Letters::default(),
            unread: Vec::new(),
            marks: crate::marked::Marks::default(),
            open: Vec::new(),
            symbol_fonts: std::collections::HashMap::new(),
            symbols: Vec::new(),
            rect_clips: Vec::new(),
            painted: Vec::new(),
            cased: Vec::new(),
        };
        for &(start, end, said) in spans {
            let span = Spoken {
                text: said.into(),
                start,
                inked: (start..end).collect(),
            };
            c.state(span, end, false);
        }
        c.read_cased();
        c.glyphs.iter().map(|g| g.text.as_str()).collect()
    }

    #[test]
    fn a_word_whose_codes_mix_its_case_takes_the_case_stated() {
        // Small capitals drawn with some capital codes and some lower-case
        // ones, at one size, after a full capital: the ActualText over the
        // odd ones is the word's case.
        let screws = [
            ("S", 14.0),
            ("C", 9.8),
            ("r", 9.8),
            ("e", 9.8),
            ("w", 9.8),
            ("S", 9.8),
        ];
        assert_eq!(cased_over(&screws, &[(1, 2, "c"), (5, 6, "s")]), "Screws");
        let mahle = [
            ("M", 10.0),
            ("A", 10.0),
            ("h", 10.0),
            ("l", 10.0),
            ("E", 10.0),
        ];
        assert_eq!(cased_over(&mahle, &[(2, 4, "HL")]), "MAHLE");
        // A word drawn in one case keeps it: InDesign's `p` over the P of
        // REPORT is not what a reader sees.
        let report = [
            ("R", 10.0),
            ("E", 10.0),
            ("P", 10.0),
            ("O", 10.0),
            ("R", 10.0),
            ("T", 10.0),
        ];
        assert_eq!(cased_over(&report, &[(2, 3, "p")]), "REPORT");
    }

    #[test]
    fn a_shape_is_measured_by_what_its_clip_lets_show() {
        // A polar chart's outer circle, drawn whole and clipped to the
        // chart's quarter, centred on its corner and reaching off the page.
        let mut c = Collector {
            page: 0,
            glyphs: Vec::new(),
            images: Vec::new(),
            rules: Vec::new(),
            fills: Vec::new(),
            dots: Vec::new(),
            page_box: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 612.0,
                y1: 792.0,
            },
            clips: Vec::new(),
            ink: Vec::new(),
            letters: crate::ligature::Letters::default(),
            unread: Vec::new(),
            marks: crate::marked::Marks::default(),
            open: Vec::new(),
            symbol_fonts: std::collections::HashMap::new(),
            symbols: Vec::new(),
            rect_clips: Vec::new(),
            painted: Vec::new(),
            cased: Vec::new(),
        };
        let circle = BBox {
            x0: 133.0,
            y0: -71.0,
            x1: 490.0,
            y1: 285.0,
        };
        // Unclipped, the page is the limit.
        assert_eq!(c.visible(circle).unwrap().y0, 0.0);
        c.push_clip_path(&hayro_interpret::ClipPath {
            path: Rect::new(298.0, 98.0, 420.0, 236.0).to_path(0.1),
            fill: hayro_interpret::FillRule::NonZero,
        });
        let seen = c.visible(circle).unwrap();
        assert_eq!(
            (seen.x0, seen.y0, seen.x1, seen.y1),
            (298.0, 98.0, 420.0, 236.0)
        );
        // A shape wholly outside the clip shows nothing.
        let outside = BBox {
            x0: 40.0,
            y0: 300.0,
            x1: 200.0,
            y1: 400.0,
        };
        assert!(c.visible(outside).is_none());
        c.pop_clip_path();
        assert!(c.visible(outside).is_some());
    }

    #[test]
    fn a_radical_reads_as_the_ideograph_the_span_states() {
        assert_eq!(
            spoken_over(vec![glyph("\u{2ED8}", 0.0, 10.0, true)], "青"),
            ["青"]
        );
    }

    #[test]
    fn a_cluster_stated_whole_goes_to_its_first_mark() {
        let g = vec![glyph("", 0.0, 10.0, true), glyph("", 5.0, 10.0, true)];
        assert_eq!(
            spoken_over(g, "\u{0915}\u{094D}\u{0937}"),
            ["\u{0915}\u{094D}\u{0937}", ""]
        );
    }

    #[test]
    fn letters_stated_one_for_one_keep_their_spaces() {
        let g = vec![
            glyph("h", 0.0, 10.0, true),
            glyph(" ", 5.0, 10.0, false),
            glyph("h", 10.0, 10.0, true),
        ];
        assert_eq!(spoken_over(g, "l l"), ["l", " ", "l"]);
    }

    #[test]
    fn a_styled_case_a_leader_or_two_lines_keep_the_drawn_text() {
        // Capitals a style drew over lower-case text.
        let caps: Vec<Glyph> = "ECENT"
            .chars()
            .enumerate()
            .map(|(i, c)| glyph(&c.to_string(), i as f64 * 5.0, 10.0, true))
            .collect();
        assert_eq!(spoken_over(caps, "ecent").concat(), "ECENT");
        // A leader's dots, stated with spaces between.
        let mut dots = vec![glyph(" ", 0.0, 10.0, false)];
        dots.extend((1..16).map(|i| glyph(".", i as f64 * 3.0, 10.0, true)));
        assert_eq!(
            spoken_over(dots, &". ".repeat(16)).concat(),
            " ..............."
        );
        // A span that runs onto the next line.
        let wrapped = vec![glyph("a", 0.0, 10.0, true), glyph("b", 0.0, 24.0, true)];
        assert_eq!(spoken_over(wrapped, "xy"), ["a", "b"]);
    }

    #[test]
    fn spacing_and_punctuation_state_nothing() {
        for said in [
            "\t",
            "\u{00AD}",
            "\u{2002}",
            "\u{0007}",
            ". . . .",
            "\u{2026}",
            "a\u{FFFD}",
        ] {
            assert_eq!(spoken(said), None, "{said:?}");
        }
        assert_eq!(spoken("fi").as_deref(), Some("fi"));
        assert_eq!(spoken("x\ty").as_deref(), Some("x y"));
    }
}

#[cfg(test)]
mod subpath_fill_tests {
    use super::subpath_fills;
    use kurbo::BezPath;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
        format!("M{x0},{y0} L{x1},{y0} L{x1},{y1} L{x0},{y1} Z ")
    }

    #[test]
    fn a_path_shading_cells_apart_is_a_fill_each() {
        // A table's bands drawn as one path, a cell at a time, two bands
        // apart: read whole, its box covered the table.
        let p = BezPath::from_svg(
            &(rect(36., 107., 169., 121.)
                + &rect(169., 107., 250., 121.)
                + &rect(36., 226., 169., 240.)
                + &rect(169., 226., 250., 240.)),
        )
        .unwrap();
        let boxes = subpath_fills(&p).unwrap();
        assert_eq!(boxes.len(), 4);
        assert!((boxes[2].y0 - 226.0).abs() < 0.01);
    }

    #[test]
    fn frames_bars_and_curves_are_read_whole() {
        // A frame: a rectangle inside a rectangle.
        let frame =
            BezPath::from_svg(&(rect(0., 0., 100., 100.) + &rect(10., 10., 90., 90.))).unwrap();
        assert!(subpath_fills(&frame).is_none());
        // Two flat bars, a glyph's `=` outlined.
        let bars = BezPath::from_svg(&(rect(0., 0., 60., 1.) + &rect(0., 4., 60., 5.))).unwrap();
        assert!(subpath_fills(&bars).is_none());
        // A chart's columns, apart, and a table's stripes, one a line.
        let columns =
            BezPath::from_svg(&(rect(0., 50., 20., 100.) + &rect(40., 30., 60., 100.))).unwrap();
        assert!(subpath_fills(&columns).is_none());
        let stripes =
            BezPath::from_svg(&(rect(0., 0., 300., 12.) + &rect(0., 24., 300., 36.))).unwrap();
        assert!(subpath_fills(&stripes).is_none());
        // Stripes drawn a cell at a time, a row apart.
        let cells = BezPath::from_svg(
            &(rect(0., 0., 100., 12.)
                + &rect(100., 0., 200., 12.)
                + &rect(0., 24., 100., 36.)
                + &rect(100., 24., 200., 36.)),
        )
        .unwrap();
        assert!(subpath_fills(&cells).is_none());
        // A curve.
        let curve =
            BezPath::from_svg(&(rect(0., 0., 20., 20.) + "M30,0 Q40,10 50,0 L50,20 L30,20 Z"))
                .unwrap();
        assert!(subpath_fills(&curve).is_none());
        // One rectangle alone.
        assert!(subpath_fills(&BezPath::from_svg(&rect(0., 0., 20., 20.)).unwrap()).is_none());
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
