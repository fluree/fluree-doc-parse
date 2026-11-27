//! Glyph extraction from a PDF, via hayro's `Device` trait.
//!
//! All state lives in `Extractor` — a per-document value, not globals. This is
//! deliberate: carrying extraction state in globals or thread-locals forces
//! pipeline stages to run sequentially and makes state propagation to worker
//! threads a source of silent data loss.

use crate::geom::BBox;
use crate::glyph::Glyph;
use crate::rule::{self, Fill, Rule};
use fluree_doc_model::budget::{Budget, TooLarge};
use fluree_doc_model::symbols;
use hayro_interpret::font::Glyph as HGlyph;
use hayro_interpret::hayro_syntax::object::ObjectIdentifier;
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::util::TransformExt;
use hayro_interpret::{interpret_page, Context, Device, InterpreterCache, InterpreterSettings};
use kurbo::{Affine, Rect, Shape};
use std::sync::Arc;

/// Whether a path is one axis-aligned rectangle: a frame drawn `re`, or as
/// four lines, closed.
fn is_rect(p: &kurbo::BezPath) -> bool {
    use kurbo::PathEl;
    let mut pts: Vec<kurbo::Point> = Vec::new();
    for el in p.elements() {
        match el {
            PathEl::MoveTo(q) if pts.is_empty() => pts.push(*q),
            PathEl::LineTo(q) => pts.push(*q),
            PathEl::ClosePath => {}
            _ => return false,
        }
    }
    if pts.len() == 5 && pts[0].distance(pts[4]) < 0.01 {
        pts.pop();
    }
    let near = |a: f64, b: f64| (a - b).abs() < 0.01;
    pts.len() == 4
        && (0..4).all(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % 4]);
            near(a.x, b.x) || near(a.y, b.y)
        })
}

/// Clip box for interpretation. Generous: we want every glyph the page draws,
/// including any drawn outside the crop box, and filter later.
const INTERPRET_BOUNDS: f64 = 20_000.0;

/// A raster image placed on the page, with a cheap probe of its content.
#[derive(Debug, Clone, Copy)]
pub struct ImagePlacement {
    /// Placement on the page, clipped to the page box.
    pub bbox: BBox,
    /// `Some(true)` when the pixels look like text or line structure
    /// ([`fluree_doc_vision::texty`]): bimodal luminance with frequent
    /// dark/light transitions along rows, or lines of text on a ground of
    /// one colour. A raster table, a scanned paragraph, a labelled chart or
    /// a screen capture with a few lines on it reads `Some(true)`; a
    /// photograph `Some(false)`. This is what separates "the VLM can recover
    /// content here" from "it is a picture", which placement geometry alone
    /// cannot see: two documents with near-identical image coverage can
    /// still diverge sharply — the AI-routed reference engine *loses* 0.38
    /// on one and *gains* 0.62 on the other.
    ///
    /// Measured only where something reads it: a raster image too small to
    /// be routed to a model, under a tenth of the page, is decoded only for
    /// an OCR engine, which reads the texty ones ([`crate::route::ocr_regions`],
    /// [`ExtractOptions::probe_small_images`]). `None` when the image was not
    /// probed: nothing is known of its pixels, and the router sends no
    /// reader to it ([`crate::route::Declined::Unprobed`]). A stencil (an
    /// image mask) is `Some(true)` either way: it is not decoded for it.
    pub texty: Option<bool>,
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

/// How [`extract_bytes_with`] reads a document.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExtractOptions {
    /// Decode raster images to measure [`ImagePlacement::texty`]. The router
    /// reads it ([`crate::route::decide`], for a region it would send to a
    /// model); the decode is most of what an image costs. A caller that will
    /// neither escalate nor read with an engine can turn it off: no image is
    /// then routed, and each one the text layer does not cover is reported
    /// unread as not examined ([`crate::route::Declined::Unprobed`]). On by
    /// default.
    pub probe_images: bool,
    /// Decode the images too small to be routed to a model as well, under a
    /// tenth of the page: only an OCR engine reads those
    /// ([`crate::route::ocr_regions`]), and a logo or an icon is most of
    /// what a document's probes would otherwise cost. A caller that reads
    /// with an engine turns it on. Off by default; nothing is probed while
    /// [`ExtractOptions::probe_images`] is off.
    pub probe_small_images: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            probe_images: true,
            probe_small_images: false,
        }
    }
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
    /// Stroked lines of diagonal segments: charts' traces.
    pub traces: Vec<rule::Trace>,
    /// The straight strokes of paths drawn as many apart, an axis's ticks
    /// among them, each as a rule. The paths stay whole among the fills too.
    pub ticks: Vec<Rule>,
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
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OcrRead {
    /// Glyphs whose `draw_index` is at least this were added by OCR.
    pub first_glyph: usize,
    /// A whole-page reading was taken.
    pub whole: bool,
    /// The routed regions, by index, a reading was taken for.
    pub regions: Vec<usize>,
    /// Those regions' boxes, in the same order: where each reading's words
    /// lie, read as one picture's whatever images draw it.
    pub region_boxes: Vec<crate::geom::BBox>,
    /// The rules the regions' pictures draw, read from their pixels and
    /// placed as their words are, each cut to its region
    /// ([`crate::ocr::read_rules`]). Not the page's ruling: a picture's
    /// rules join the page's only once its text is read as a table by them
    /// (`document`'s `picture_table`).
    pub rules: Vec<crate::rule::Rule>,
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
    /// Pages the file holds after the last one read: past the page limit
    /// ([`Limits::pages`](fluree_doc_model::budget::Limits::pages)), or
    /// left by the budget ([`Document::cut_short`]). Not read, and listed
    /// unread by [`crate::unread_pages`].
    pub pages_past_limit: usize,
    /// Where the budget ran out before the page limit: the document is
    /// read to the last page whole, and the pages from there to the page
    /// limit, the first of [`Document::pages_past_limit`], listed unread
    /// by the limit passed (`PastMemoryLimit`, `PastWorkLimit`); those
    /// past the page limit as well are `PastPageLimit`. `None` where the
    /// reading ran to the page limit or to the end.
    pub cut_short: Option<CutShort>,
    /// The title the file's information dictionary states (`/Title`), as
    /// written: what the document is called by whoever made the file, and
    /// often stale or a file's name.
    pub title: Option<String>,
}

/// The budget passed on a page after the first ([`Document::cut_short`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutShort {
    /// Which: [`Limit::Memory`](fluree_doc_model::budget::Limit::Memory)
    /// or [`Limit::Work`](fluree_doc_model::budget::Limit::Work).
    pub limit: fluree_doc_model::budget::Limit,
    /// The pages it left unread before the page limit, from the one it was
    /// passed on.
    pub pages: usize,
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

/// A page's glyphs' fonts and, for letters, ink ratios, in draw order: see
/// [`infer_weights`].
type PageInk = Vec<Option<(u128, Option<f64>)>>;

/// Most, in points, a clip's extent across a rule may exceed the rule's own
/// thickness for the clip to be drawn round that line alone: a stroke's
/// half-width and a little rounding, short of any text's height.
const TIGHT_CLIP: f64 = 3.0;

struct Collector<'a, 'b> {
    /// The document's budget, spent before each thing the page draws is
    /// kept, and a step of work for each call the interpreter makes.
    budget: &'b mut Budget,
    /// The limit passed, once spent: nothing more is kept, and the page is
    /// stopped where the build can unwind ([`Collector::spend`]).
    over: Option<TooLarge>,
    page: usize,
    glyphs: Vec<Glyph>,
    images: Vec<ImagePlacement>,
    rules: Vec<Rule>,
    fills: Vec<Fill>,
    /// The most glyphs a page read so far kept, this one included: what
    /// the largest page's layout is spent ([`Budget::spend_page_glyphs`]).
    largest_page: u64,
    dots: Vec<BBox>,
    traces: Vec<rule::Trace>,
    ticks: Vec<Rule>,
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
    /// Parallel to `clips`: whether each clip is an axis-aligned rectangle,
    /// its box the clip exactly.
    clip_rects: Vec<bool>,
    /// Parallel to `clips`: each clip's number, in the order the page sets
    /// them.
    clip_ids: Vec<usize>,
    /// Clips set so far on the page.
    clip_seq: usize,
    /// Parallel to `glyphs`: for a glyph judged against the clip in force,
    /// that clip's number, whether the glyph shows inside it, and whether
    /// it puts ink down ([`Collector::clipped_out`]).
    clip_votes: Vec<Option<(usize, bool, bool)>>,
    /// Per-glyph `(font identity, ink ratio)`, parallel to `glyphs`, for the
    /// weight inference in [`infer_weights`]: the font of every outline
    /// glyph, and the ink ratio of a letter. Kept alongside rather than on
    /// `Glyph` because it is scaffolding: once weights are resolved it has no
    /// further use, and every consumer of `Glyph` would have to carry it.
    ink: PageInk,
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
    /// What each font's dictionary says about its face, for the document.
    faces: crate::faces::Faces<'a>,
    /// The weight each font's glyphs are read at, once found.
    weights: std::collections::HashMap<u128, Option<u32>>,
    /// Whether each font is italic, by the interpreter's key for it.
    italics: std::collections::HashMap<u128, bool>,
    /// The document's image probes so far; see [`Probes`].
    probes: Probes,
}

/// What [`looks_like_text`] found for each image XObject the document has
/// drawn, so that an image drawn on many pages — a letterhead, a page's
/// background — is decoded once.
///
/// An image XObject decodes the same wherever it is drawn but for one input
/// the graphics state supplies, a transfer function; a found result is
/// reused only once the document is known to set none (`plain`). That is
/// asked of the document the first time an image is drawn again, so a
/// document that draws each image once never pays for the question. Inline
/// images have no object and are always decoded.
#[derive(Default)]
struct Probes {
    /// No image is probed: nothing will read the result.
    off: bool,
    /// The images too small to be routed to a model are probed too: an OCR
    /// engine will read them ([`ExtractOptions::probe_small_images`]).
    small: bool,
    found: std::collections::HashMap<ObjectIdentifier, bool>,
    /// No dictionary in the document sets a transfer function; `None` until
    /// `again` asked it.
    plain: Option<bool>,
    /// An image was drawn again before `plain` was known.
    again: bool,
}

impl Probes {
    /// Whether [`Probes::texty`] decodes `image`: it is probed, and not an
    /// image found already in a document known to decode it alike. Only
    /// then is its probe's work spent: a picture bullet drawn on every
    /// line costs a step a draw, not a decode.
    fn decodes(&self, image: &hayro_interpret::RasterImage<'_>) -> bool {
        let found =
            || (image.stream().dict().obj_id()).is_some_and(|id| self.found.contains_key(&id));
        !self.off && !(self.plain == Some(true) && found())
    }

    /// Whether the image's pixels look like text, decoding it unless the
    /// document has drawn it before.
    fn texty(&mut self, image: &hayro_interpret::RasterImage<'_>) -> Option<bool> {
        if self.off {
            return None;
        }
        let id = image.stream().dict().obj_id();
        if let Some(found) = id.and_then(|id| self.found.get(&id)) {
            if self.plain == Some(true) {
                return Some(*found);
            }
            self.again = true;
        }
        let mut texty = false;
        // The decoder honours a target size only for JPEG 2000: a Flate or
        // DCT image is decoded, and probed, at its own size, a cost that
        // grows with its pixels and stays under the decode's
        // (`fluree_doc_vision::texty`).
        image.with_rgba(
            |data, _| texty = looks_like_text(&data),
            Some((PROBE_DIM, PROBE_DIM)),
        );
        if let Some(id) = id.filter(|_| self.plain != Some(false)) {
            self.found.insert(id, texty);
        }
        Some(texty)
    }

    /// Ask the document whether it sets a transfer function, once an image
    /// has been drawn again.
    fn settle(&mut self, pdf: &Pdf) {
        if self.again && self.plain.is_none() {
            self.plain = Some(!sets_transfer(pdf));
        }
    }
}

/// Whether any dictionary in the document has a transfer function entry
/// (`/TR`, `/TR2`), the graphics state's one say in how an image decodes.
/// Every object is read, and every dictionary written inside one.
fn sets_transfer(pdf: &Pdf) -> bool {
    use hayro_interpret::hayro_syntax::object::{MaybeRef, Object};
    for object in pdf.objects() {
        let mut open = vec![object];
        while let Some(o) = open.pop() {
            let dict = match o {
                Object::Dict(d) => d,
                Object::Stream(s) => s.dict().clone(),
                Object::Array(a) => {
                    open.extend(a.raw_iter().filter_map(|m| match m {
                        MaybeRef::NotRef(v) => Some(v),
                        MaybeRef::Ref(_) => None,
                    }));
                    continue;
                }
                _ => continue,
            };
            if dict.contains_key(b"TR") || dict.contains_key(b"TR2") {
                return true;
            }
            open.extend(dict.entries().filter_map(|(_, m)| match m {
                MaybeRef::NotRef(v) => Some(v),
                MaybeRef::Ref(_) => None,
            }));
        }
    }
    false
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

impl<'a> Collector<'a, '_> {
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
    /// A rule cut to a clip drawn round its line alone: the clip's extent
    /// across the rule within [`TIGHT_CLIP`] of the rule's own thickness,
    /// covering its axis. Some producers draw each table rule far past the
    /// table, out past the page's edges, and clip it to the stretch that
    /// shows; read as drawn, every partial rule under a column group was a
    /// rule across the page. Under a broad clip — the page body, a table's
    /// region, the foot a table breaks at — a rule stays as drawn. `None`
    /// where nothing of it shows.
    fn clip_rule(&self, mut r: rule::Rule) -> Option<rule::Rule> {
        let Some(clip) = self.clips.last().copied() else {
            return Some(r);
        };
        let b = r.bbox;
        let horizontal = r.orientation == rule::Orientation::Horizontal;
        let (c0, c1, t0, t1) = if horizontal {
            (clip.y0, clip.y1, b.y0, b.y1)
        } else {
            (clip.x0, clip.x1, b.x0, b.x1)
        };
        let axis = (t0 + t1) * 0.5;
        let tight =
            c1 - c0 <= (t1 - t0) + TIGHT_CLIP && c0 <= axis + TIGHT_CLIP && c1 >= axis - TIGHT_CLIP;
        if !tight {
            return Some(r);
        }
        if horizontal {
            r.bbox.x0 = b.x0.max(clip.x0);
            r.bbox.x1 = b.x1.min(clip.x1);
            (r.bbox.x1 > r.bbox.x0).then_some(r)
        } else {
            r.bbox.y0 = b.y0.max(clip.y0);
            r.bbox.y1 = b.y1.min(clip.y1);
            (r.bbox.y1 > r.bbox.y0).then_some(r)
        }
    }
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
    /// The glyphs a clip takes away: every glyph judged against a clip none
    /// of whose inked glyphs shows inside it ([`Collector::draw_glyph`]).
    fn clipped_out(&self) -> Vec<bool> {
        let mut tally: std::collections::HashMap<usize, (usize, usize)> = Default::default();
        for &(id, shown, inked) in self.clip_votes.iter().flatten() {
            if inked {
                let t = tally.entry(id).or_default();
                t.0 += 1;
                t.1 += usize::from(shown);
            }
        }
        self.clip_votes
            .iter()
            .map(|v| {
                v.is_some_and(|(id, _, _)| {
                    tally
                        .get(&id)
                        .is_some_and(|&(n, shown)| n > 0 && shown == 0)
                })
            })
            .collect()
    }

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

impl Collector<'_, '_> {
    /// Whether a glyph at `origin` starts a line: it does not follow the
    /// glyph kept before it on the page along the same baseline (the same
    /// direction, its origin no further off the baseline than a tenth of
    /// its size, and no further along it than two sizes past it, nor half
    /// a size behind). Counted from the drawing order, as a bound on the
    /// lines the layout makes, not as them.
    fn starts_line(&self, origin: (f64, f64), rotation_deg: f32, font_size: f32) -> bool {
        let Some(prev) = self.glyphs.last() else {
            return true;
        };
        if (prev.rotation_deg - rotation_deg).abs() > 1.0 {
            return true;
        }
        let size = f64::from(font_size.max(prev.font_size));
        let (sin, cos) = f64::from(rotation_deg).to_radians().sin_cos();
        let (dx, dy) = (origin.0 - prev.origin.0, origin.1 - prev.origin.1);
        // Along the baseline, which runs (cos, -sin) in the device's frame
        // (`rotation_deg` is measured against its y axis, pointing down),
        // and across it. Anything not finite starts one.
        let along = dx * cos - dy * sin;
        let across = (dx * sin + dy * cos).abs();
        !(across <= 0.1 * size && along >= -0.5 * size && along <= 2.0 * size)
    }
}

/// Unwound out of the interpreter by a [`Collector`] whose budget is spent:
/// the page stops there, rather than at the end of its content.
struct Spent;

impl Collector<'_, '_> {
    /// Spend from the budget before keeping what the page drew: `false`,
    /// and nothing kept, once a limit is passed. The interpreter has no way
    /// to be stopped, so where the build unwinds the page is left there
    /// ([`Spent`], caught by [`extract_bytes`]); where it aborts on a
    /// panic, the interpreter runs the page to its end with nothing more
    /// kept, its memory bounded and its time not.
    fn spend(&mut self, f: impl FnOnce(&mut Budget) -> Result<(), TooLarge>) -> bool {
        if self.over.is_some() {
            return false;
        }
        match f(self.budget) {
            Ok(()) => true,
            Err(e) => {
                self.over = Some(e);
                #[cfg(panic = "unwind")]
                std::panic::resume_unwind(Box::new(Spent));
                #[cfg(not(panic = "unwind"))]
                false
            }
        }
    }
}

/// What an outline kept takes: its path's elements.
fn outline_bytes(path: &kurbo::BezPath) -> u64 {
    (std::mem::size_of_val(path.elements()) as u64).saturating_add(64)
}

/// Steps of work for one image's probe, where it decodes
/// ([`Probes::decodes`]): decoded at most [`PROBE_DIM`] pixels square, a
/// step for each sixteen.
const PROBE_STEPS: u64 = (PROBE_DIM as u64 * PROBE_DIM as u64) / 16;

/// A drawn shape's paint: its colour, or the kind of pattern it is painted
/// with.
fn paint_of(paint: &hayro_interpret::Paint<'_>) -> rule::Paint {
    match paint {
        hayro_interpret::Paint::Color(c) => rule::Paint::Solid(c.to_rgba().to_rgba8()),
        hayro_interpret::Paint::Pattern(p) => match p.as_ref() {
            hayro_interpret::pattern::Pattern::Shading(_) => rule::Paint::Gradient,
            hayro_interpret::pattern::Pattern::Tiling(_) => rule::Paint::Tiled,
        },
    }
}

impl<'a> Device<'a> for Collector<'a, '_> {
    fn set_soft_mask(&mut self, _m: Option<hayro_interpret::SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _b: hayro_interpret::BlendMode) {}
    fn draw_path(
        &mut self,
        path: &kurbo::BezPath,
        transform: Affine,
        paint: &hayro_interpret::Paint<'a>,
        mode: &hayro_interpret::PathDrawMode,
    ) {
        // A step for each segment: the copy below, and its walks.
        let segments = path.elements().len() as u64;
        if !self.spend(|b| b.spend_work(segments.max(1))) {
            return;
        }
        // A stroke shorter than its pen is wide is a bar across it: see
        // [`pen_bars`]. Each bar is read as any shape is, a rule or a fill.
        if let hayro_interpret::PathDrawMode::Stroke(props) = mode {
            if let Some(bars) = pen_bars(path, transform, props) {
                for b in bars {
                    if !self.page_box.intersects(&b) {
                        continue;
                    }
                    match rule::classify(b, self.page) {
                        Some(rule::Shape::Rule(x)) => {
                            if let Some(x) = self.clip_rule(x) {
                                if self.spend(|b| b.spend_cells(1)) {
                                    self.rules.push(x);
                                }
                            }
                        }
                        Some(rule::Shape::Fill(x)) => {
                            if let Some(bbox) = self.visible(x.bbox) {
                                if self.spend(|b| b.spend_cells(1)) {
                                    self.fills.push(rule::Fill { bbox, ..x });
                                }
                            }
                        }
                        None => {}
                    }
                }
                return;
            }
        }
        // Bounding box only. A table grid is axis-aligned rectangles and
        // segments; curve detail is irrelevant and expensive to keep.
        let path = transform * path.clone();
        let r: Rect = path.bounding_box();
        let drawn = crate::geom::from_kurbo(r);
        if !self.page_box.intersects(&drawn) {
            return;
        }
        self.painted.push(drawn);
        if !matches!(mode, hayro_interpret::PathDrawMode::Fill(_)) {
            if let Some(trace) = trace_of(&path, drawn, self.page) {
                self.traces.push(trace);
            }
            self.ticks.extend(marks_of(&path, self.page));
        }
        if matches!(mode, hayro_interpret::PathDrawMode::Fill(_)) && rule::is_dot(&drawn) {
            if let Some(seen) = self.visible(drawn) {
                if self.spend(|b| b.spend_cells(1)) {
                    self.dots.push(seen);
                }
            }
        }
        // One path can carry many rules. A financial statement strokes every
        // underline on the page — a short rule under each figure, the double
        // rule under each total — as subpaths of a single path (`m l m l …
        // S`), and that path's box is a block spanning all of them: read
        // whole it was one fill, and the page had no rules. Where every
        // subpath is a rule on its own, each is pushed as one.
        if let Some(rules) = subpath_rules(&path, self.page) {
            let rules: Vec<Rule> = rules
                .into_iter()
                .filter_map(|x| self.clip_rule(x))
                .filter(|x| self.page_box.intersects(&x.bbox))
                .collect();
            if self.spend(|b| b.spend_cells(rules.len() as u64)) {
                self.rules.extend(rules);
            }
            return;
        }
        // What the fills are painted with, read once and only for a fill.
        let filled = matches!(mode, hayro_interpret::PathDrawMode::Fill(_));
        let mut painted = None;
        let mut fill_of = |x: rule::Fill, bbox: BBox| rule::Fill {
            bbox,
            paint: Some(*painted.get_or_insert_with(|| paint_of(paint))),
            stroked: !filled,
            ..x
        };
        // A rule is kept as drawn: table structure reads a grid's ruling
        // whole, and a table broken by the page foot draws its next row's
        // rules on under the clip. A filled shape is measured by what shows
        // of it; see [`Collector::visible`].
        // A path filling several rectangles apart, a table's shaded cells
        // drawn as one, is a fill each: read whole, its box covered the
        // table, and the bands it shades were none.
        if filled {
            if let Some(boxes) = subpath_fills(&path) {
                if !self.spend(|b| b.spend_cells(boxes.len() as u64)) {
                    return;
                }
                for b in boxes {
                    match rule::classify(b, self.page) {
                        Some(rule::Shape::Rule(x)) => {
                            if let Some(x) = self.clip_rule(x) {
                                self.rules.push(x);
                            }
                        }
                        Some(rule::Shape::Fill(x)) => {
                            if let Some(bbox) = self.visible(x.bbox) {
                                self.fills.push(fill_of(x, bbox));
                            }
                        }
                        None => {}
                    }
                }
                return;
            }
        }
        match rule::classify(drawn, self.page) {
            Some(rule::Shape::Rule(x)) => {
                if let Some(x) = self.clip_rule(x) {
                    if self.spend(|b| b.spend_cells(1)) {
                        self.rules.push(x);
                    }
                }
            }
            Some(rule::Shape::Fill(x)) => {
                if let Some(bbox) = self.visible(x.bbox) {
                    if self.spend(|b| b.spend_cells(1)) {
                        self.fills.push(fill_of(x, bbox));
                    }
                }
            }
            _ => {}
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
        self.clip_rects.push(is_rect(&c.path));
        self.clip_seq += 1;
        self.clip_ids.push(self.clip_seq);
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
        // A step for the call, and its placement kept; its probe apart,
        // where it decodes (below).
        if !self.spend(|b| b.spend_work(1).and_then(|()| b.spend_cells(1))) {
            return;
        }
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
        let bbox = BBox {
            x0: b.x0.max(self.page_box.x0),
            y0: b.y0.max(self.page_box.y0),
            x1: b.x1.min(self.page_box.x1),
            y1: b.y1.min(self.page_box.y1),
        };
        let texty = match &i {
            hayro_interpret::Image::Raster(ri) => {
                // The router reads the probe only of an image big enough to
                // be a region, and decoding a logo or an icon to ask was most
                // of the probe's cost. An OCR engine reads the texty ones the
                // router leaves to it, a capture an inch high among them
                // ([`crate::route::ocr_regions`]), and has them all probed.
                // The margin keeps the test on the near side of the router's
                // own, which is made after the page may have been turned. An
                // image drawn again is decoded once ([`Probes`]).
                let page_area = ((self.page_box.x1 - self.page_box.x0)
                    * (self.page_box.y1 - self.page_box.y0))
                    .max(1.0);
                let area = (bbox.x1 - bbox.x0) * (bbox.y1 - bbox.y0);
                if !self.probes.small && area < 0.99 * crate::route::MIN_REGION_FRACTION * page_area
                {
                    None
                } else if self.probes.decodes(ri) && !self.spend(|b| b.spend_work(PROBE_STEPS)) {
                    return;
                } else {
                    self.probes.texty(ri)
                }
            }
            // Stencils are 1-bit masks — the format scanned text and line art
            // arrive in. Treat as text-bearing.
            hayro_interpret::Image::Stencil(_) => Some(true),
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
            bbox,
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
        self.clip_rects.pop();
        self.clip_ids.pop();
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
        mode: &hayro_interpret::GlyphDrawMode,
    ) {
        if !self.spend(|b| b.spend_work(1)) {
            return;
        }
        let mut text = match glyph.as_unicode() {
            Some(hayro_interpret::hayro_cmap::BfString::Char(c)) => c.to_string(),
            Some(hayro_interpret::hayro_cmap::BfString::String(s)) => s,
            // No text from the font's ToUnicode map or from a name the
            // Adobe Glyph List lists whole: the program's own name for the
            // glyph, read by the list's rules (`h.sc`, `f_i`), if it has one;
            // see `faces::Faces::named`.
            None => match glyph {
                HGlyph::Outline(o) => self
                    .faces
                    .named(o.font_cache_key(), o.glyph_id().to_u32())
                    .unwrap_or_default(),
                _ => String::new(),
            },
        };

        // A letter the font's ToUnicode map gives in the other case than
        // the glyph its encoding draws (`F` read `f`): see `faces::recased`.
        if let HGlyph::Outline(o) = glyph {
            let mut chars = text.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                if c.is_ascii_alphabetic() {
                    if let Some(drawn) = self.faces.recase(o.font_cache_key(), c, o.advance_width())
                    {
                        text = drawn.to_string();
                    }
                }
            }
        }

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
                        let outline = o.outline();
                        let bytes = family.len() as u64 + outline_bytes(&outline);
                        if !self.spend(|b| b.spend_outline(bytes)) {
                            return;
                        }
                        self.letters.keep(family, c, outline, advance);
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

        // The weight is what the font's name says (`-Bold`, `-Lt`,
        // `-Halbfett`), in the PDF or inside the program; else what its
        // descriptor's `/FontWeight` says; else the program's weight class.
        // A name is the likeliest to be right: in the disagreements on bold
        // looked at across a corpus of 7,000 documents, the name is the
        // source that matches the page (`CooperHewitt-Thin`
        // declared 701, `Frutiger-Black` 400, a subset of `NimbusSansL-Regu`
        // whose program says 700). The descriptor goes before the program
        // because it describes the face as drawn: Word's `Arial,Bold` embeds
        // the regular program, states 700, and draws it emboldened.
        let weight = match glyph {
            HGlyph::Outline(o) => *self.weights.entry(o.font_cache_key()).or_insert_with(|| {
                let face = self.faces.get(o.font_cache_key());
                let data = o.font_data();
                let program = data.as_ref().and_then(|f| f.postscript_name.as_deref());
                face.and_then(|f| f.named)
                    .or_else(|| program.and_then(crate::faces::named_weight))
                    .or_else(|| face.and_then(|f| f.declared))
                    .or_else(|| data.as_ref().and_then(|f| f.weight))
            }),
            _ => None,
        };
        // Italic as the font's dictionary says, else as its program's name
        // does.
        let italic = match glyph {
            HGlyph::Outline(o) => *self.italics.entry(o.font_cache_key()).or_insert_with(|| {
                self.faces.get(o.font_cache_key()).is_some_and(|f| f.italic)
                    || o.font_data()
                        .and_then(|f| f.postscript_name)
                        .is_some_and(|n| crate::faces::named_italic(&n))
            }),
            _ => false,
        };
        // Ink density, for the faces no weight is stated for. Measured
        // here because the transformed outline is already being built for the
        // bounding box, and is not available anywhere later.
        let mut ink = None;
        let bbox = match glyph {
            HGlyph::Outline(o) => {
                ink = Some((o.font_cache_key(), None));
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
                        ink = Some((o.font_cache_key(), Some(path.area().abs() / area)));
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
        // Its text beyond what a glyph holds inline: a mapping can give one
        // glyph a long string.
        let long = (text.len() > 4).then_some(text.as_str());
        // And the outline kept to read it by, where its text is unknown.
        let outline = unread
            .as_ref()
            .map_or(0, |(family, o)| family.len() as u64 + outline_bytes(o));
        // A line it starts, and the page grown past the largest one yet:
        // both cost more than its glyphs.
        let line = self.starts_line(origin, rotation_deg, font_size);
        let on_page = self.glyphs.len() as u64 + 1;
        let crowding = on_page.saturating_sub(self.largest_page);
        if !self.spend(|b| {
            b.spend_glyphs(1)?;
            if line {
                b.spend_lines(1)?;
            }
            b.spend_page_glyphs(crowding)?;
            b.spend_outline(outline)?;
            long.map_or(Ok(()), |t| b.spend_text(t))
        }) {
            return;
        }
        self.largest_page = self.largest_page.max(on_page);
        // Clipped out: text drawn wholly outside the clip in force shows
        // nowhere, as text set off the page shows nowhere. A placed
        // graphic's text frames, cut to the graphic's frame, keep their
        // other text out of sight (`MECHANICAL MEASURES` drawn under the
        // page's own heading, clipped away by the illustration's frame);
        // read, it ran through the heading a letter at a time. Each glyph
        // votes here, and a clip whose glyphs all lie outside it takes them
        // away at the page's end ([`Collector::clipped_out`]): a word drawn
        // once per cell it crosses, or lettering drawn a copy per slice of a
        // gradient, shows in part under each clip, and is kept whole.
        // Judged only under rectangles, the frames text is placed in, whose
        // boxes are the clips exactly. Text set invisible, an OCR'd scan's
        // words over or under its image, is the document's text wherever
        // it lies, and a Type3 glyph, with no box, is not judged; a space,
        // with no ink, stands where its pen does.
        let vote = match (self.clips.last(), self.clip_ids.last()) {
            (Some(clip), Some(&id))
                if self.clip_rects.iter().all(|&r| r)
                    && !matches!(mode, hayro_interpret::GlyphDrawMode::Invisible)
                    && matches!(glyph, HGlyph::Outline(_)) =>
            {
                let shown = match bbox {
                    Some(b) => clip.intersects(&b),
                    None => clip.contains(origin.0, origin.1),
                };
                Some((id, shown, bbox.is_some()))
            }
            _ => None,
        };
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
        self.clip_votes.push(vote);
        self.glyphs.push(Glyph {
            text,
            bbox,
            page: self.page,
            origin,
            rotation_deg,
            font_size,
            weight,
            italic,
            advance,
            draw_index,
            ocr: None,
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

/// The straight strokes of a path drawn as three or more apart, each a
/// segment of its own: an axis's ticks drawn as one path. Each is a rule.
fn marks_of(path: &kurbo::BezPath, page: usize) -> Vec<Rule> {
    let mut parts: Vec<(kurbo::Point, kurbo::Point)> = Vec::new();
    let mut at: Option<kurbo::Point> = None;
    let mut segs = 0usize;
    for el in path.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => {
                at = Some(p);
                segs = 0;
            }
            kurbo::PathEl::LineTo(p) => {
                let Some(a) = at else { return Vec::new() };
                segs += 1;
                if segs > 1 {
                    return Vec::new();
                }
                parts.push((a, p));
                at = Some(p);
            }
            kurbo::PathEl::ClosePath => {}
            kurbo::PathEl::QuadTo(..) | kurbo::PathEl::CurveTo(..) => return Vec::new(),
        }
    }
    if parts.len() < 3 {
        return Vec::new();
    }
    parts
        .into_iter()
        .filter_map(|(a, b)| {
            match rule::classify(
                crate::geom::from_kurbo(kurbo::Rect::from_points(a, b)),
                page,
            ) {
                Some(rule::Shape::Rule(x)) => Some(x),
                _ => None,
            }
        })
        .collect()
}

/// Most a segment may lean, in points, across its length and still be
/// horizontal or vertical: a rule's own rounding.
const SEGMENT_SQUARE: f64 = 0.5;

/// Most a square segment moves across its run, in points: the noise of a
/// coordinate carried through the page's transforms. A rule drawn
/// horizontal or vertical moves no more.
const SEGMENT_NOISE: f64 = 0.05;

/// Least lean of a short diagonal segment, as its move across over its run
/// (about six degrees). A series sampled densely steps less than a rule's
/// rounding along its axis per value (a month of 25 years plotted 150
/// points wide is half a point), so a step that short is read against its
/// own length.
const SEGMENT_LEAN: f64 = 0.1;

/// The stroked path as a chart's trace: three straight segments or more,
/// two of them diagonal at least. A table's rules, a box's sides and a
/// statement's underlines run square; a trace runs from value to value.
fn trace_of(path: &kurbo::BezPath, drawn: BBox, page: usize) -> Option<rule::Trace> {
    let (mut segments, mut diagonal) = (0usize, 0usize);
    let (mut right, mut left) = (0usize, 0usize);
    let mut at: Option<kurbo::Point> = None;
    for el in path.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => at = Some(p),
            kurbo::PathEl::LineTo(p) => {
                if let Some(a) = at {
                    segments += 1;
                    let (dx, dy) = ((p.x - a.x).abs(), (p.y - a.y).abs());
                    let (across, run) = (dx.min(dy), dx.max(dy));
                    if across > SEGMENT_SQUARE
                        || (across > SEGMENT_NOISE && across >= run * SEGMENT_LEAN)
                    {
                        diagonal += 1;
                    }
                    if p.x - a.x > SEGMENT_NOISE {
                        right += 1;
                    } else if a.x - p.x > SEGMENT_NOISE {
                        left += 1;
                    }
                }
                at = Some(p);
            }
            kurbo::PathEl::QuadTo(_, p) | kurbo::PathEl::CurveTo(_, _, p) => at = Some(p),
            kurbo::PathEl::ClosePath => {}
        }
    }
    // A segment straight up or down runs neither way, and is onward.
    let onward = segments - right.min(left);
    (segments >= 3 && diagonal >= 2).then_some(rule::Trace {
        bbox: drawn,
        segments,
        diagonal,
        onward,
        page,
    })
}

/// The rules a path of several straight subpaths draws, one per subpath, when
/// every subpath is a horizontal rule a figure wide; `None` for a path of one
/// subpath, a curve, or any other subpath (a box, a cell background, a
/// chart's tick marks, which are short, and its axes, which stand up), read
/// whole as before.
/// Shortest underline read out of a path of several, in points: a narrow
/// figure's width, longer than a chart's tick.
const MIN_UNDERLINE: f64 = 8.0;

/// The ink of a stroked path whose every subpath is one straight segment
/// shorter than the pen is wide, each its own bar on the page; `None` for
/// any other path, which is read by its outline as before.
///
/// A pen sweeps its width across the segment it strokes, so a segment
/// shorter than the pen draws a bar the pen's width long, at right angles
/// to the segment, as wide as the segment is long: a template that draws
/// its column rules as 0.5-pt horizontal segments with a 36-pt pen draws
/// three 0.5 × 36 vertical rules, and its borders, a 1-pt segment with a
/// 321-pt pen, two 1 × 321 rules. Read by its outline, each was a stub of
/// horizontal rule, and the three in one path one rule through the header
/// they divide. A butt cap ends the bar at the segment's ends; a square or
/// round cap carries it half the pen on past each. A dashed pen, and a
/// segment of no length, a dot or nothing, are left to the outline. Ordinary rules, their pen thinner than their length, are
/// not this.
fn pen_bars(
    path: &kurbo::BezPath,
    transform: Affine,
    props: &hayro_interpret::StrokeProps,
) -> Option<Vec<BBox>> {
    let pen = props.line_width as f64;
    if pen <= 0.0 || !props.dash_array.is_empty() {
        return None;
    }
    let cap = match props.line_cap {
        kurbo::Cap::Butt => 0.0,
        _ => pen / 2.0,
    };
    // Each subpath's one segment: a second turns it into an outline.
    let mut segments: Vec<(kurbo::Point, kurbo::Point)> = Vec::new();
    let mut start: Option<kurbo::Point> = None;
    for el in path.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => start = Some(p),
            kurbo::PathEl::LineTo(p) => segments.push((start.take()?, p)),
            kurbo::PathEl::ClosePath => {}
            _ => return None,
        }
    }
    if segments.is_empty() {
        return None;
    }
    let mut bars = Vec::new();
    for (a, b) in segments {
        let along = b - a;
        let length = along.hypot();
        // No length is a dot, or nothing with a butt cap: the outline's.
        // And only a bar that is a rule: a segment no longer than a rule is
        // thick, under a pen at least a rule's aspect longer. A short thick
        // stroke that paints a block is a shape the outline reads as before.
        if length == 0.0
            || length > rule::MAX_RULE_THICKNESS
            || pen < length * rule::MIN_RULE_ASPECT
        {
            return None;
        }
        let dir = if length > 0.0 {
            along / length
        } else {
            kurbo::Vec2::new(1.0, 0.0)
        };
        let across = kurbo::Vec2::new(-dir.y, dir.x) * (pen / 2.0);
        let (from, to) = (a - dir * cap, b + dir * cap);
        let corners = [from + across, from - across, to - across, to + across];
        let mut r = Rect::from_points(transform * corners[0], transform * corners[1]);
        for c in &corners[2..] {
            r = r.union_pt(transform * *c);
        }
        bars.push(crate::geom::from_kurbo(r));
    }
    Some(bars)
}

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
    let (w, h, luma): (usize, usize, std::borrow::Cow<'_, [u8]>) = match data {
        hayro_interpret::ImageData::Luma(l) => (
            l.width as usize,
            l.height as usize,
            std::borrow::Cow::Borrowed(&l.data),
        ),
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

/// Most a light body raises a document's weights: one step of the scale.
const MAX_LIGHT_LIFT: u32 = 100;

/// The weights a face stated to be medium declares: Medium is 500, and
/// some families' stand a step either side of it.
const MEDIUM_WEIGHTS: std::ops::RangeInclusive<u32> = 450..=550;

/// How much wider than the body face's stems a face's must be to count as
/// bold, for a face setting too few letters to be weighed by its ink.
///
/// A bold face's stems are 1.4 to 1.7 times its regular's (Helvetica 140 to
/// 88, Times 139 to 84, Frutiger 136 to 96); a medium's about 1.3 (Söhne
/// Kräftig 120 to Buch's 92), and an italic's about the regular's own.
const BOLD_STEM_RATIO: f32 = 1.35;

/// Infer the weight of the faces nothing names one for, from their ink.
///
/// `hayro`'s `font_data()` returns `None` for Type 1 fonts, and many embedded
/// subsets omit `/FontWeight` and carry a meaningless name. Across the
/// 200-document benchmark corpus **81 documents — 40% — expose no weight at
/// all**, which silently disables every weight-based heading signal on exactly
/// the classic LaTeX papers where bold section titles are the only cue. Other
/// documents name the weight of some faces and not of others.
///
/// A bold face is physically heavier: it covers more of each glyph's box. That
/// is measurable from the outline regardless of font format or naming. Faces
/// are compared *within* a document against its body face — absolute ink ratios
/// vary too much between typefaces to threshold directly. A face setting too
/// few letters for its ink to be trusted is compared by the stem width its
/// descriptor states, `stem`, against the body's. Letters alone are weighed,
/// and only a bold face's letters are read as bold: its figures and signs,
/// whose ink was not weighed, are left with no weight, which neither makes
/// a line bold nor keeps one from being. A medium's figures read as medium
/// outvoted its letters in a header cell (`WD8005FRYZ`), and read as bold
/// they made bold lines of figures set in it (`RCR: 0` in a table, a
/// paper's `doi:` line).
///
/// Faces stated to be medium are judged, and, where the body face states no
/// weight, faces with no weight; a weight the document states is otherwise
/// never second-guessed. Beside a body whose weight is stated, a face with
/// none is mostly a chart's, a logo's or a signature's, whose ink says
/// nothing of headings. Where the body states none, the faces around it are
/// as silent, and one face named elsewhere (`FrutigerLTStd-Bold` on a cover
/// page a repository added to a paper) says nothing of them.
///
/// Medium is the one step whose look depends on the family. Söhne's Kräftig
/// and Helvetica Neue's Medium are set as the bold, as heavy beside the
/// regular as a semibold is; other families' mediums sit close to their
/// regular. So a medium counts as bold when its ink, or its stems, say it is
/// as much heavier than the body as a bold is, and otherwise stays medium.
///
/// Nothing is judged against a body face stated to be lighter or heavier
/// than regular: over a light body, a regular face is heavier without being
/// bold, and [`lift_light_body`] reads the weights against it instead.
fn infer_weights(pages: &mut [Page], ink: &[PageInk], stem: impl Fn(u128) -> Option<f32>) {
    // Letters, ink and stated weight by face.
    let mut stats: std::collections::HashMap<u128, (usize, f64, Option<u32>)> = Default::default();
    for (page, marks) in pages.iter().zip(ink) {
        for (glyph, mark) in page.glyphs.iter().zip(marks) {
            if let Some((key, Some(ratio))) = mark {
                let e = stats.entry(*key).or_insert((0, 0.0, glyph.weight));
                e.0 += 1;
                e.1 += *ratio;
            }
        }
    }
    // The body face is the one setting the most letters. Comparing against the
    // mean of all faces instead would let a heading-heavy page drag the
    // reference upward and hide the very contrast being looked for. Two faces
    // setting as many letters are told apart by their ink, the lighter being
    // the body, and then by font, so the choice is the same on every run.
    let Some((&body, &(body_n, body_sum, body_weight))) = stats
        .iter()
        .filter(|(_, (n, _, _))| *n >= MIN_INK_GLYPHS)
        .max_by(|(ka, (na, sa, _)), (kb, (nb, sb, _))| {
            na.cmp(nb)
                .then_with(|| (sb / *nb as f64).total_cmp(&(sa / *na as f64)))
                .then_with(|| kb.cmp(ka))
        })
    else {
        return;
    };
    if body_weight.is_some_and(|w| !(400..=500).contains(&w)) {
        return;
    }
    let open = |w: &Option<u32>| match w {
        None => body_weight.is_none(),
        Some(w) => MEDIUM_WEIGHTS.contains(w),
    };
    let threshold = (body_sum / body_n as f64) * BOLD_INK_MARGIN;
    let body_stem = stem(body);
    let bold: std::collections::HashSet<u128> = stats
        .iter()
        .filter(|(k, (n, sum, w))| {
            open(w)
                && **k != body
                && if *n >= MIN_INK_GLYPHS {
                    sum / *n as f64 >= threshold
                } else {
                    stem(**k)
                        .zip(body_stem)
                        .is_some_and(|(s, b)| s >= b * BOLD_STEM_RATIO)
                }
        })
        .map(|(k, _)| *k)
        .collect();
    if bold.is_empty() {
        return;
    }
    for (page, marks) in pages.iter_mut().zip(ink) {
        for (glyph, mark) in page.glyphs.iter_mut().zip(marks) {
            if let Some((k, ratio)) = mark {
                if bold.contains(k) {
                    glyph.weight = ratio.map(|_| 700);
                }
            }
        }
    }
}

/// Read every weight against a regular body, for a document set in a light
/// face.
///
/// What reads as bold is a face heavier than the text around it. A document
/// whose body is Light (300) sets its headings in Medium (500), the step
/// above regular that its designer uses as the bold; read on the absolute
/// scale, that is lighter than bold, and every heading reads as body text.
/// So the weights of a document whose body is lighter than regular are
/// raised by as much as its body is, by one step at most: over a Light body
/// the body reads 400 and Medium 600; over a Thin one (200), regular stays
/// short of bold, as a table set in Helvetica beside text in Helvetica Neue
/// Thin is.
fn lift_light_body(pages: &mut [Page]) {
    let mut letters: std::collections::BTreeMap<Option<u32>, usize> = Default::default();
    for g in pages.iter().flat_map(|p| &p.glyphs) {
        if g.text.chars().all(char::is_alphabetic) && !g.text.is_empty() {
            *letters.entry(g.weight).or_default() += 1;
        }
    }
    // Ties go to the heavier weight, so the lift is never more than needed.
    let body = letters
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| a.0.cmp(b.0)))
        .and_then(|(w, _)| *w);
    let Some(body) = body.filter(|w| *w < 400) else {
        return;
    };
    let lift = (400 - body).min(MAX_LIGHT_LIFT);
    for g in pages.iter_mut().flat_map(|p| &mut p.glyphs) {
        if let Some(w) = g.weight.as_mut() {
            *w += lift;
        }
    }
}

#[derive(Debug)]
pub enum ExtractError {
    Io(String),
    Parse(String),
    /// Past a limit the document is read within
    /// ([`fluree_doc_model::budget::Limits`]): which, in the error.
    TooLarge(TooLarge),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::Io(e) => write!(f, "io: {e}"),
            ExtractError::Parse(e) => write!(f, "parse: {e}"),
            ExtractError::TooLarge(e) => write!(f, "PDF too large: {e}"),
        }
    }
}

/// A limit passed is the error's source: [`TooLarge::limit`] says which.
impl std::error::Error for ExtractError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TooLarge(e) => Some(e),
            _ => None,
        }
    }
}

/// Extract the PDF in `data`: opened ([`crate::open`], its page tree
/// measured first), then read ([`extract_pdf`]).
pub fn extract_bytes(data: Vec<u8>) -> Result<Document, ExtractError> {
    extract_bytes_with(data, &ExtractOptions::default())
}

/// [`extract_bytes`], read as `options` say.
pub fn extract_bytes_with(
    data: Vec<u8>,
    options: &ExtractOptions,
) -> Result<Document, ExtractError> {
    extract_pdf_with(&crate::page_tree::open(data)?, options)
}

/// Extract a PDF already opened ([`crate::open`]): a caller that also needs
/// the parsed document (its outline, its links, its renders) opens it once
/// for both.
pub fn extract_pdf(pdf: &hayro_interpret::hayro_syntax::Pdf) -> Result<Document, ExtractError> {
    extract_pdf_with(pdf, &ExtractOptions::default())
}

/// [`extract_pdf`], read as `options` say.
pub fn extract_pdf_with(
    pdf: &hayro_interpret::hayro_syntax::Pdf,
    options: &ExtractOptions,
) -> Result<Document, ExtractError> {
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
    // One budget for the document's pages: what each draws is spent from
    // it as it is kept, and each call the interpreter makes is a step.
    let mut budget = Budget::default();
    let mut pages = Vec::new();
    let mut ink: Vec<PageInk> = Vec::new();
    let mut letters = crate::ligature::Letters::default();
    let mut largest_page = 0;
    let mut unread: Vec<(usize, usize, String, kurbo::BezPath)> = Vec::new();
    let mut faces = crate::faces::Faces::default();
    let mut weights = std::collections::HashMap::new();
    let mut italics = std::collections::HashMap::new();
    let mut probes = Probes {
        off: !options.probe_images,
        small: options.probe_small_images,
        ..Probes::default()
    };

    // Past the page limit, the pages are not read, and said to be.
    let all = pdf.pages();
    let read = all.len().min(fluree_doc_model::budget::limits().pages);
    let mut cut_short = None;
    for (index, page) in all.iter().take(read).enumerate() {
        let (w, h) = page.render_dimensions();
        faces.read(page);
        let mut dev = Collector {
            budget: &mut budget,
            over: None,
            page: index,
            glyphs: Vec::new(),
            images: Vec::new(),
            rules: Vec::new(),
            fills: Vec::new(),
            dots: Vec::new(),
            traces: Vec::new(),
            ticks: Vec::new(),
            ink: Vec::new(),
            letters: std::mem::take(&mut letters),
            largest_page,
            unread: Vec::new(),
            marks: crate::marked::Marks::read(page),
            open: Vec::new(),
            symbol_fonts: std::collections::HashMap::new(),
            symbols: Vec::new(),
            clips: Vec::new(),
            rect_clips: Vec::new(),
            painted: Vec::new(),
            clip_rects: Vec::new(),
            clip_ids: Vec::new(),
            clip_seq: 0,
            clip_votes: Vec::new(),
            cased: Vec::new(),
            faces: std::mem::take(&mut faces),
            weights: std::mem::take(&mut weights),
            italics: std::mem::take(&mut italics),
            probes: std::mem::take(&mut probes),
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
        // Stopped by the collector once the budget is spent, the page is
        // left where it stopped, and dropped: the document is read to the
        // page before, and the rest listed unread, as past the page limit.
        // A text PDF of 5,000 pages, past the memory at its 3,500th or so,
        // is read to there rather than refused. A first page past it alone
        // leaves nothing read: refused.
        let interpreted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            interpret_page(page, &mut ctx, &mut dev)
        }));
        if let Err(p) = interpreted {
            if !p.is::<Spent>() {
                std::panic::resume_unwind(p);
            }
        }
        if let Some(e) = dev.over.take() {
            use fluree_doc_model::budget::Limit;
            if index == 0 || !matches!(e.limit(), Limit::Memory | Limit::Work) {
                return Err(ExtractError::TooLarge(e));
            }
            // What the pages before it found, read again once all are.
            letters = std::mem::take(&mut dev.letters);
            faces = std::mem::take(&mut dev.faces);
            cut_short = Some(CutShort {
                limit: e.limit(),
                pages: read - index,
            });
            break;
        }
        // A span the page left open still covers what it drew.
        while let Some(open) = dev.open.pop() {
            if let Some(span) = open {
                dev.speak(span);
            }
        }
        dev.read_cased();
        dev.read_symbols();
        let out = dev.clipped_out();
        if out.iter().any(|&o| o) {
            // Kept glyphs' new places, for the readings that name a glyph by
            // its place.
            let mut place = vec![None; out.len()];
            let mut k = 0;
            for (i, &o) in out.iter().enumerate() {
                if !o {
                    place[i] = Some(k);
                    k += 1;
                }
            }
            let mut i = 0;
            dev.glyphs.retain(|_| {
                i += 1;
                !out[i - 1]
            });
            let mut i = 0;
            dev.ink.retain(|_| {
                i += 1;
                !out[i - 1]
            });
            dev.unread = std::mem::take(&mut dev.unread)
                .into_iter()
                .filter_map(|(g, f, o)| place.get(g).copied().flatten().map(|n| (n, f, o)))
                .collect();
        }
        letters = std::mem::take(&mut dev.letters);
        largest_page = dev.largest_page;
        faces = std::mem::take(&mut dev.faces);
        weights = std::mem::take(&mut dev.weights);
        italics = std::mem::take(&mut dev.italics);
        probes = std::mem::take(&mut dev.probes);
        probes.settle(pdf);
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
            traces: dev.traces,
            ticks: dev.ticks,
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
    infer_weights(&mut pages, &ink, |k| faces.get(k).and_then(|f| f.stem));
    lift_light_body(&mut pages);
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

    Ok(Document {
        pages_past_limit: all.len() - pages.len(),
        pages,
        cut_short,
        title: crate::info::read(&pdf).title,
    })
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
    for t in page.traces.iter_mut() {
        t.bbox = rect(t.bbox);
    }
    for r in page.ticks.iter_mut() {
        r.bbox = rect(r.bbox);
        if turn != 180 {
            r.orientation = match r.orientation {
                rule::Orientation::Horizontal => rule::Orientation::Vertical,
                rule::Orientation::Vertical => rule::Orientation::Horizontal,
            };
        }
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

    fn polyline(points: &[(f64, f64)]) -> kurbo::BezPath {
        let mut p = kurbo::BezPath::new();
        p.move_to(points[0]);
        for &q in &points[1..] {
            p.line_to(q);
        }
        p
    }

    fn drawn(p: &kurbo::BezPath) -> BBox {
        crate::geom::from_kurbo(kurbo::Shape::bounding_box(p))
    }

    #[test]
    fn a_series_line_is_a_trace() {
        // A series plotted left to right, rising and falling.
        let line = polyline(&[
            (80.0, 220.0),
            (96.0, 204.0),
            (112.0, 211.0),
            (128.0, 188.0),
            (144.0, 196.0),
        ]);
        let t = trace_of(&line, drawn(&line), 0).expect("a trace");
        assert_eq!((t.segments, t.diagonal, t.onward), (4, 4, 4));
        // A series sampled densely steps under half a point per value, and
        // its steps still lean.
        let dense: Vec<(f64, f64)> = (0..60)
            .map(|i| (100.0 + i as f64 * 0.4, 300.0 + [0.0, 1.5, 0.8, 2.6][i % 4]))
            .collect();
        let dense = polyline(&dense);
        let t = trace_of(&dense, drawn(&dense), 0).expect("a dense trace");
        assert_eq!((t.segments, t.diagonal, t.onward), (59, 59, 59));
        // A series sloping gently leans more than a rule's rounding, however
        // long its steps.
        let gentle = polyline(&[
            (346.0, 215.0),
            (386.0, 213.5),
            (426.0, 212.0),
            (470.0, 213.0),
        ]);
        let t = trace_of(&gentle, drawn(&gentle), 0).expect("a gentle trace");
        assert_eq!(t.diagonal, 3);
        // A box's outline runs square: no trace.
        let boxed = polyline(&[
            (80.0, 100.0),
            (180.0, 100.0),
            (180.0, 160.0),
            (80.0, 160.0),
            (80.0, 100.0),
        ]);
        assert!(trace_of(&boxed, drawn(&boxed), 0).is_none());
        // An outline turning back on itself has a trace's segments but runs
        // both ways.
        let outline = polyline(&[
            (80.0, 100.0),
            (120.0, 80.0),
            (160.0, 100.0),
            (120.0, 120.0),
            (80.0, 100.0),
        ]);
        let t = trace_of(&outline, drawn(&outline), 0).expect("diagonal segments");
        assert!(t.onward < t.segments);
    }

    #[test]
    fn ticks_drawn_as_one_path_are_marks() {
        // Four ticks down a scale, each its own short stroke.
        let mut ticks = kurbo::BezPath::new();
        for k in 0..4 {
            let y = 140.0 + 15.0 * k as f64;
            ticks.move_to((70.0, y));
            ticks.line_to((76.0, y));
        }
        let marks = marks_of(&ticks, 0);
        assert_eq!(marks.len(), 4);
        assert!(marks
            .iter()
            .all(|m| m.orientation == rule::Orientation::Horizontal));
        // A path whose parts run on through several segments is no set of
        // ticks.
        let mut zigzag = polyline(&[(70.0, 140.0), (76.0, 146.0), (82.0, 140.0)]);
        zigzag.move_to((70.0, 160.0));
        zigzag.line_to((76.0, 160.0));
        assert!(marks_of(&zigzag, 0).is_empty());
    }

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
            italic: false,
            advance: boxed.then_some(6.67),
            draw_index: 0,
            ocr: None,
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
                italic: false,
                advance: None,
                draw_index: i,
                ocr: None,
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
            traces: Vec::new(),
            ticks: Vec::new(),
            width: 600.0,
            height: 800.0,
            rotation: 0,
            ocr: None,
            warnings: Vec::new(),
        }
    }

    /// `body` letters at ratio 0.50, then `heavy` at `ratio`.
    fn ink(body: usize, heavy: usize, ratio: f64) -> Vec<PageInk> {
        let mut v: PageInk = (0..body).map(|_| Some((1u128, Some(0.50)))).collect();
        v.extend((0..heavy).map(|_| Some((2u128, Some(ratio)))));
        vec![v]
    }

    #[test]
    fn a_denser_face_is_inferred_bold() {
        let mut pages = vec![page(200)];
        infer_weights(&mut pages, &ink(150, 50, 0.65), |_| None);
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
            infer_weights(&mut pages, &ink(100, 100, 0.65), |_| None);
            assert!(pages[0].glyphs[0].weight.is_none());
            assert_eq!(pages[0].glyphs[199].weight, Some(700));
        }
    }

    #[test]
    fn an_italic_is_not_bold() {
        // A regular face and its italic differ by a few percent, well inside
        // the margin; promoting italics would flood the Bold heading signal.
        let mut pages = vec![page(200)];
        infer_weights(&mut pages, &ink(150, 50, 0.53), |_| None);
        assert!(pages[0].glyphs.iter().all(|g| g.weight.is_none()));
    }

    #[test]
    fn a_declared_weight_is_never_second_guessed() {
        let mut pages = vec![page(200)];
        for g in &mut pages[0].glyphs[150..] {
            g.weight = Some(400);
        }
        infer_weights(&mut pages, &ink(150, 50, 0.90), |_| None);
        assert_eq!(pages[0].glyphs[199].weight, Some(400));
    }

    #[test]
    fn a_silent_face_beside_a_stated_body_is_not_weighed() {
        let mut pages = vec![page(200)];
        for g in &mut pages[0].glyphs[..150] {
            g.weight = Some(400);
        }
        infer_weights(&mut pages, &ink(150, 50, 0.65), |_| None);
        assert!(pages[0].glyphs[199].weight.is_none());
    }

    #[test]
    fn a_face_named_elsewhere_does_not_stop_a_silent_body_being_weighed() {
        let mut pages = vec![page(201)];
        pages[0].glyphs[200].weight = Some(700);
        infer_weights(&mut pages, &ink(150, 50, 0.65), |_| None);
        assert!(pages[0].glyphs[0].weight.is_none());
        assert_eq!(pages[0].glyphs[199].weight, Some(700));
    }

    #[test]
    fn a_medium_as_heavy_as_a_bold_reads_bold() {
        for (ratio, weight) in [(0.65, 700), (0.53, 500)] {
            let mut pages = vec![page(200)];
            for (i, g) in pages[0].glyphs.iter_mut().enumerate() {
                g.weight = Some(if i < 150 { 400 } else { 500 });
            }
            infer_weights(&mut pages, &ink(150, 50, ratio), |_| None);
            assert_eq!(pages[0].glyphs[199].weight, Some(weight));
        }
    }

    #[test]
    fn a_medium_read_bold_leaves_its_figures_unweighed() {
        // A header cell `WD8005FRYZ` in a medium read as bold: its digits,
        // whose ink was not weighed, must not outvote its letters as medium.
        let mut pages = vec![page(201)];
        for (i, g) in pages[0].glyphs.iter_mut().enumerate() {
            g.weight = Some(if i < 150 { 400 } else { 500 });
        }
        let mut marks = ink(150, 50, 0.65);
        marks[0].push(Some((2u128, None)));
        infer_weights(&mut pages, &marks, |_| None);
        assert_eq!(pages[0].glyphs[199].weight, Some(700));
        assert_eq!(pages[0].glyphs[200].weight, None);
    }

    #[test]
    fn nothing_is_weighed_against_a_light_body() {
        // A regular face is denser than a light one without being bold.
        let mut pages = vec![page(200)];
        for g in &mut pages[0].glyphs[..150] {
            g.weight = Some(300);
        }
        infer_weights(&mut pages, &ink(150, 50, 0.65), |_| None);
        assert!(pages[0].glyphs[199].weight.is_none());
    }

    #[test]
    fn a_rare_face_is_weighed_by_its_stems() {
        let stems = |k: u128| Some(if k == 2 { 140.0 } else { 88.0 });
        let mut pages = vec![page(200)];
        infer_weights(&mut pages, &ink(180, 20, 0.50), stems);
        assert_eq!(pages[0].glyphs[199].weight, Some(700));
        // A medium's stems are not a bold's.
        let stems = |k: u128| Some(if k == 2 { 115.0 } else { 88.0 });
        let mut pages = vec![page(200)];
        infer_weights(&mut pages, &ink(180, 20, 0.50), stems);
        assert!(pages[0].glyphs[199].weight.is_none());
    }

    #[test]
    fn a_light_body_lifts_every_weight_until_it_reads_regular() {
        let mut pages = vec![page(200)];
        for (i, g) in pages[0].glyphs.iter_mut().enumerate() {
            g.weight = Some(if i < 150 { 300 } else { 500 });
        }
        pages[0].glyphs[199].weight = None;
        lift_light_body(&mut pages);
        assert_eq!(pages[0].glyphs[0].weight, Some(400));
        assert_eq!(pages[0].glyphs[150].weight, Some(600));
        assert_eq!(pages[0].glyphs[199].weight, None);
    }

    #[test]
    fn a_thin_body_lifts_by_one_step_only() {
        let mut pages = vec![page(200)];
        for (i, g) in pages[0].glyphs.iter_mut().enumerate() {
            g.weight = Some(if i < 150 { 200 } else { 400 });
        }
        lift_light_body(&mut pages);
        assert_eq!(pages[0].glyphs[0].weight, Some(300));
        assert_eq!(pages[0].glyphs[199].weight, Some(500));
    }

    #[test]
    fn a_regular_body_is_not_lifted() {
        let mut pages = vec![page(200)];
        for (i, g) in pages[0].glyphs.iter_mut().enumerate() {
            g.weight = Some(if i < 150 { 400 } else { 300 });
        }
        lift_light_body(&mut pages);
        assert_eq!(pages[0].glyphs[0].weight, Some(400));
        assert_eq!(pages[0].glyphs[199].weight, Some(300));
    }

    #[test]
    fn a_rare_face_is_ignored() {
        // Below MIN_INK_GLYPHS, and with no stems stated: a maths italic or
        // a logo, not a text face.
        let mut pages = vec![page(200)];
        infer_weights(&mut pages, &ink(180, 20, 0.90), |_| None);
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
            texty: Some(true),
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
            italic: false,
            advance: None,
            draw_index: 0,
            ocr: None,
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
            italic: false,
            advance: Some(5.0),
            draw_index: 0,
            ocr: None,
        }
    }

    /// The glyphs' text after a span stating `said` covers all of them.
    fn spoken_over(glyphs: Vec<Glyph>, said: &str) -> Vec<String> {
        let inked = (0..glyphs.len())
            .filter(|&i| glyphs[i].bbox.is_some())
            .collect();
        let mut budget = Budget::default();
        let mut c = Collector {
            page: 0,
            glyphs,
            images: Vec::new(),
            rules: Vec::new(),
            fills: Vec::new(),
            dots: Vec::new(),
            traces: Vec::new(),
            ticks: Vec::new(),
            page_box: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 600.0,
                y1: 800.0,
            },
            clips: Vec::new(),
            clip_rects: Vec::new(),
            clip_ids: Vec::new(),
            clip_seq: 0,
            clip_votes: Vec::new(),
            ink: Vec::new(),
            letters: crate::ligature::Letters::default(),
            largest_page: 0,
            unread: Vec::new(),
            marks: crate::marked::Marks::default(),
            open: Vec::new(),
            symbol_fonts: std::collections::HashMap::new(),
            symbols: Vec::new(),
            rect_clips: Vec::new(),
            painted: Vec::new(),
            budget: &mut budget,
            over: None,
            cased: Vec::new(),
            faces: Default::default(),
            weights: Default::default(),
            italics: Default::default(),
            probes: Probes::default(),
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
        let mut budget = Budget::default();
        let mut c = Collector {
            page: 0,
            glyphs,
            images: Vec::new(),
            rules: Vec::new(),
            fills: Vec::new(),
            dots: Vec::new(),
            traces: Vec::new(),
            ticks: Vec::new(),
            page_box: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 600.0,
                y1: 800.0,
            },
            clips: Vec::new(),
            clip_rects: Vec::new(),
            clip_ids: Vec::new(),
            clip_seq: 0,
            clip_votes: Vec::new(),
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
            largest_page: 0,
            budget: &mut budget,
            over: None,
            faces: Default::default(),
            weights: Default::default(),
            italics: Default::default(),
            probes: Probes::default(),
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

    /// A collector for a letter-size page with nothing drawn on it yet,
    /// spending from `budget`.
    fn collector(budget: &mut Budget) -> Collector<'static, '_> {
        Collector {
            page: 0,
            glyphs: Vec::new(),
            images: Vec::new(),
            rules: Vec::new(),
            fills: Vec::new(),
            dots: Vec::new(),
            traces: Vec::new(),
            ticks: Vec::new(),
            page_box: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 612.0,
                y1: 792.0,
            },
            clips: Vec::new(),
            clip_rects: Vec::new(),
            clip_ids: Vec::new(),
            clip_seq: 0,
            clip_votes: Vec::new(),
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
            largest_page: 0,
            budget,
            over: None,
            faces: Default::default(),
            weights: Default::default(),
            italics: Default::default(),
            probes: Probes::default(),
        }
    }

    #[test]
    fn a_shape_is_measured_by_what_its_clip_lets_show() {
        // A polar chart's outer circle, drawn whole and clipped to the
        // chart's quarter, centred on its corner and reaching off the page.
        let mut budget = Budget::default();
        let mut c = collector(&mut budget);
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
    fn a_fill_carries_its_paint_and_an_outline_says_it_is_one() {
        use hayro_interpret::color::{AlphaColor, Color};
        use hayro_interpret::{FillRule, Paint, PathDrawMode};
        let mut budget = Budget::default();
        let mut c = collector(&mut budget);
        // A table's shaded cells drawn as one path: two rows of two cells,
        // side by side, the rows a section apart.
        let grey = Paint::Color(Color::from_rgba(AlphaColor::from_rgba8(217, 217, 217, 255)));
        let mut cells = kurbo::BezPath::new();
        for (x0, y0) in [
            (100.0, 100.0),
            (200.0, 100.0),
            (100.0, 180.0),
            (200.0, 180.0),
        ] {
            for el in Rect::new(x0, y0, x0 + 100.0, y0 + 20.0)
                .to_path(0.1)
                .elements()
            {
                cells.push(*el);
            }
        }
        c.draw_path(
            &cells,
            Affine::IDENTITY,
            &grey,
            &PathDrawMode::Fill(FillRule::NonZero),
        );
        // A box drawn as a black outline: a fill by its extent only.
        let black = Paint::Color(Color::from_rgba(AlphaColor::BLACK));
        c.draw_path(
            &Rect::new(300.0, 300.0, 400.0, 360.0).to_path(0.1),
            Affine::IDENTITY,
            &black,
            &PathDrawMode::Stroke(Default::default()),
        );
        let painted: Vec<_> = c.fills.iter().map(|f| (f.paint, f.stroked)).collect();
        let solid = |v| Some(rule::Paint::Solid([v, v, v, 255]));
        assert_eq!(
            painted,
            vec![
                (solid(217), false),
                (solid(217), false),
                (solid(217), false),
                (solid(217), false),
                (solid(0), true)
            ]
        );
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

    #[test]
    fn a_stroke_shorter_than_its_pen_is_a_bar_across_it() {
        let props = |w: f32| hayro_interpret::StrokeProps {
            line_width: w,
            ..Default::default()
        };
        // Three 0.5-pt segments along a row, a 36-pt pen: three columns'
        // rules, 0.5 wide and 36 tall.
        let mut p = BezPath::new();
        for x in [0.0, 92.0, 184.0] {
            p.move_to((x, 0.0));
            p.line_to((x + 0.5, 0.0));
        }
        let bars = super::pen_bars(&p, kurbo::Affine::IDENTITY, &props(36.0)).expect("bars");
        assert_eq!(bars.len(), 3);
        for b in &bars {
            assert!(
                (b.width() - 0.5).abs() < 1e-6 && (b.height() - 36.0).abs() < 1e-6,
                "{b:?}"
            );
        }
        // An ordinary rule, its pen thinner than its length, is not one.
        let mut rule = BezPath::new();
        rule.move_to((0.0, 0.0));
        rule.line_to((100.0, 0.0));
        assert!(super::pen_bars(&rule, kurbo::Affine::IDENTITY, &props(1.0)).is_none());
        // Nor a dot, a segment of no length, nor a short thick stroke that
        // paints a block.
        let mut dot = BezPath::new();
        dot.move_to((5.0, 5.0));
        dot.line_to((5.0, 5.0));
        assert!(super::pen_bars(&dot, kurbo::Affine::IDENTITY, &props(36.0)).is_none());
        let mut block = BezPath::new();
        block.move_to((0.0, 0.0));
        block.line_to((10.0, 0.0));
        assert!(super::pen_bars(&block, kurbo::Affine::IDENTITY, &props(36.0)).is_none());
    }

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

#[cfg(test)]
mod budget_tests {
    use super::*;
    use fluree_doc_model::budget::{
        limited_to, Limit, Limits, GLYPH_BYTES, LINE_BYTES, PAGE_GLYPH_BYTES,
    };

    /// A one-page PDF drawing `content` in Helvetica.
    fn page_of(content: &str) -> Vec<u8> {
        pages_of(1, content)
    }

    /// A PDF of `n` pages, each drawing `content` in Helvetica.
    fn pages_of(n: usize, content: &str) -> Vec<u8> {
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 5 + i)).collect();
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
        ];
        for _ in 0..n {
            objects.push(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 3 0 R >> >> >>"
                    .to_string(),
            );
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        let size = objects.len() + 1;
        out.extend(format!("xref\n0 {size}\n0000000000 65535 f \n").bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").bytes());
        }
        out.extend(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").bytes(),
        );
        out
    }

    /// A hundred lines of ten letters, a thousand glyphs, on each of `n`
    /// pages.
    fn letters_on(n: usize) -> Vec<u8> {
        let lines: String = (0..100)
            .map(|i| format!("1 0 0 1 72 {} Tm (abcdefghij) Tj ", 750 - 7 * i))
            .collect();
        pages_of(n, &format!("BT /F1 6 Tf {lines}ET"))
    }

    /// A thousand glyphs: a hundred lines of ten letters.
    fn letters() -> Vec<u8> {
        letters_on(1)
    }

    fn within(memory: u64, work: u64, pdf: Vec<u8>) -> Result<Document, ExtractError> {
        let mut l = Limits::default();
        l.memory = memory;
        l.work = work;
        limited_to(l, || extract_bytes(pdf))
    }

    /// What `pages` pages of [`letters_on`] are spent: each glyph, each
    /// line, and the glyphs of the largest page once.
    fn letters_cost(pages: u64) -> u64 {
        pages * (1000 * GLYPH_BYTES + 100 * LINE_BYTES) + 1000 * PAGE_GLYPH_BYTES
    }

    #[test]
    fn the_glyphs_a_page_draws_are_spent_from_the_documents_budget() {
        let doc = within(1 << 20, 1 << 20, letters()).unwrap();
        assert_eq!(doc.glyph_count(), 1000);
        // Not room for them all: refused, saying which limit.
        let need = letters_cost(1);
        match within(need - 1, 1 << 20, letters()) {
            Err(ExtractError::TooLarge(e)) => {
                assert_eq!((e.limit(), e.max()), (Limit::Memory, need - 1));
                assert!(e.to_string().starts_with("glyphs past"), "{e}");
            }
            other => panic!("{:?}", other.map(|d| d.glyph_count())),
        }
        assert!(within(need, 1 << 20, letters()).is_ok());
        // Each line spent once, not each glyph, and the largest page's
        // glyphs once, not each page's: a byte less, and the fifth page is
        // not read.
        let need = letters_cost(5);
        assert_eq!(within(need, 1 << 20, letters_on(5)).unwrap().pages.len(), 5);
        let short = within(need - 1, 1 << 20, letters_on(5)).unwrap();
        assert_eq!(short.pages.len(), 4);
    }

    /// The reasons the pages after the last one read are listed by.
    fn past(doc: &Document) -> Vec<(usize, String)> {
        crate::past_limit(doc)
            .map(|u| (u.index, u.reason))
            .collect()
    }

    #[test]
    fn a_long_text_pdf_past_its_budget_is_read_to_the_last_whole_page() {
        // Forty pages of a thousand glyphs, the memory enough for ten and
        // a half: read to the tenth, the thirty after it listed unread by
        // the memory, rather than the document refused.
        let doc = within(
            letters_cost(10) + 500 * GLYPH_BYTES,
            1 << 30,
            letters_on(40),
        )
        .unwrap();
        assert_eq!(doc.pages.len(), 10);
        assert!(doc.pages.iter().all(|p| p.glyphs.len() == 1000));
        assert_eq!(doc.pages_past_limit, 30);
        assert_eq!(
            doc.cut_short,
            Some(CutShort {
                limit: Limit::Memory,
                pages: 30
            })
        );
        let listed = past(&doc);
        assert_eq!(listed.len(), 30);
        assert_eq!(listed[0], (10, "PastMemoryLimit".to_string()));
        assert!(listed.iter().all(|(_, r)| r == "PastMemoryLimit"));
        // With a page limit of 20 as well: the memory's to it, the page
        // limit's past it, whichever is raised first.
        let mut l = Limits::default();
        (l.memory, l.work, l.pages) = (letters_cost(10) + 500 * GLYPH_BYTES, 1 << 30, 20);
        let doc = limited_to(l, || extract_bytes(letters_on(40))).unwrap();
        assert_eq!((doc.pages.len(), doc.pages_past_limit), (10, 30));
        let listed = past(&doc);
        assert_eq!(listed[9], (19, "PastMemoryLimit".to_string()));
        assert_eq!(listed[10], (20, "PastPageLimit".to_string()));
        assert_eq!(listed.last().unwrap().0, 39);
        // The work runs out the same way: a step a glyph, two pages and a
        // half.
        let doc = within(1 << 30, 2500, letters_on(6)).unwrap();
        assert_eq!(doc.pages.len(), 2);
        assert_eq!(past(&doc)[0], (2, "PastWorkLimit".to_string()));
        // Within both, read whole and nothing listed.
        let doc = within(1 << 30, 1 << 30, letters_on(6)).unwrap();
        assert_eq!((doc.pages.len(), doc.cut_short), (6, None));
        assert!(past(&doc).is_empty());
    }

    /// A PDF whose pages each draw one picture, a 500 pt square, as often
    /// as `draws` says.
    fn pictures(draws: &[usize]) -> Vec<u8> {
        let n = draws.len();
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
        let pixels = [0x80u8; 16];
        let mut objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).into_bytes(),
            [
                b"<< /Type /XObject /Subtype /Image /Width 4 /Height 4 /ColorSpace /DeviceGray                   /BitsPerComponent 8 /Length 16 >>\nstream\n"
                    .to_vec(),
                pixels.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ];
        for (i, d) in draws.iter().enumerate() {
            let content = "q 500 0 0 500 50 50 cm /Im1 Do Q ".repeat(*d);
            objects.push(
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R \
                     /Resources << /XObject << /Im1 3 0 R >> >> >>",
                    5 + 2 * i
                )
                .into_bytes(),
            );
            objects.push(
                format!(
                    "<< /Length {} >>\nstream\n{content}\nendstream",
                    content.len()
                )
                .into_bytes(),
            );
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n", i + 1).bytes());
            out.extend(o);
            out.extend(b"\nendobj\n");
        }
        let xref = out.len();
        let size = objects.len() + 1;
        out.extend(format!("xref\n0 {size}\n0000000000 65535 f \n").bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").bytes());
        }
        out.extend(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").bytes(),
        );
        out
    }

    #[test]
    fn a_picture_drawn_again_costs_its_probe_only_where_it_is_decoded() {
        let read = |work: u64, draws: &[usize], probe: bool| {
            let mut l = Limits::default();
            l.work = work;
            let mut o = ExtractOptions::default();
            o.probe_images = probe;
            let pdf = pictures(draws);
            limited_to(l, || extract_bytes_with(pdf, &o)).map(|d| d.pages.len())
        };
        // Not probed: a step a draw. A thousand draws of a picture bullet
        // were four million steps, as many as 65,536 draws past the
        // shipped work.
        assert_eq!(read(1000, &[1000], false).unwrap(), 1);
        assert!(read(999, &[1000], false).is_err());
        // Probed: decoded on the first page, drawn twice there, so the
        // document is asked whether it decodes alike; on the second, a
        // thousand times, decoded no more.
        let probes = 2 * PROBE_STEPS;
        assert_eq!(read(probes + 1002, &[2, 1000], true).unwrap(), 2);
        // A step short, the second page is not read.
        assert_eq!(read(probes + 1001, &[2, 1000], true).unwrap(), 1);
    }

    #[test]
    fn a_line_is_a_glyph_off_the_baseline_of_the_one_before() {
        let doc = within(1 << 20, 1 << 20, letters()).unwrap();
        let g = &doc.pages[0].glyphs;
        let starts =
            |c: &Collector<'_, '_>, g: &Glyph| c.starts_line(g.origin, g.rotation_deg, g.font_size);
        let mut budget = Budget::default();
        let mut c = Collector {
            page: 0,
            glyphs: Vec::new(),
            images: Vec::new(),
            rules: Vec::new(),
            fills: Vec::new(),
            page_box: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 612.0,
                y1: 792.0,
            },
            ink: Vec::new(),
            letters: crate::ligature::Letters::default(),
            largest_page: 0,
            unread: Vec::new(),
            marks: crate::marked::Marks::default(),
            open: Vec::new(),
            symbol_fonts: std::collections::HashMap::new(),
            symbols: Vec::new(),
            clips: Vec::new(),
            clip_rects: Vec::new(),
            clip_ids: Vec::new(),
            clip_seq: 0,
            clip_votes: Vec::new(),
            rect_clips: Vec::new(),
            painted: Vec::new(),
            dots: Vec::new(),
            traces: Vec::new(),
            ticks: Vec::new(),
            cased: Vec::new(),
            faces: Default::default(),
            weights: Default::default(),
            italics: Default::default(),
            probes: Default::default(),
            budget: &mut budget,
            over: None,
        };
        let mut lines = 0;
        for glyph in g {
            lines += usize::from(starts(&c, glyph));
            c.glyphs.push(glyph.clone());
        }
        assert_eq!(lines, 100);
        // Turned a quarter, a line down the page: the next glyph below
        // follows, one beside it does not; nor one far along the line,
        // nor one behind.
        let mut turned = g[0].clone();
        turned.rotation_deg = 90.0;
        turned.origin = (100.0, 100.0);
        c.glyphs.push(turned);
        assert!(!c.starts_line((100.0, 95.0), 90.0, 6.0));
        assert!(c.starts_line((106.0, 100.0), 90.0, 6.0));
        assert!(c.starts_line((100.0, 80.0), 90.0, 6.0));
        assert!(c.starts_line((100.0, 104.0), 90.0, 6.0));
        assert!(c.starts_line((100.0, 95.0), 0.0, 6.0));
        assert!(c.starts_line((f64::NAN, 95.0), 90.0, 6.0));
    }

    #[test]
    fn a_glyph_costs_what_one_on_a_line_of_its_own_takes() {
        // 200,000 glyphs, each on a line of its own (the font a third of
        // the leading): measured at 372 B a glyph at the peak, 74 MB, so
        // held to 64 MiB the page is refused rather than converted past it.
        let n = 200_000;
        let lead = 600.0 / n as f64;
        let pdf = page_of(&format!(
            "BT /F1 {:.9} Tf 72 700 Td {lead:.9} TL {}ET",
            lead * 0.4,
            "(a)' ".repeat(n)
        ));
        match within(64 << 20, 1 << 30, pdf) {
            Err(ExtractError::TooLarge(e)) => assert_eq!(e.limit(), Limit::Memory),
            other => panic!("{:?}", other.map(|d| d.glyph_count())),
        }
    }

    #[test]
    fn each_call_the_interpreter_makes_is_a_step_of_work() {
        match within(1 << 20, 999, letters()) {
            Err(ExtractError::TooLarge(e)) => assert_eq!(e.limit(), Limit::Work),
            other => panic!("{:?}", other.map(|d| d.glyph_count())),
        }
        assert!(within(1 << 20, 1000, letters()).is_ok());
        // A path, a step a segment: a hundred rectangles of five.
        let rules: String = (0..100).map(|i| format!("0 {i} 600 0.3 re f ")).collect();
        assert!(within(1 << 20, 500, page_of(&rules)).is_ok());
        assert!(within(1 << 20, 499, page_of(&rules)).is_err());
    }

    #[test]
    fn the_rules_a_page_draws_are_spent_as_cells() {
        let rules: String = (0..100)
            .map(|i| format!("0 {} 600 0.3 re f ", 5 * i))
            .collect();
        let doc = within(1 << 20, 1 << 20, page_of(&rules)).unwrap();
        assert_eq!(doc.pages[0].rules.len(), 100);
        let cells = 100 * fluree_doc_model::budget::CELL_BYTES;
        assert!(within(cells, 1 << 20, page_of(&rules)).is_ok());
        match within(cells - 1, 1 << 20, page_of(&rules)) {
            Err(ExtractError::TooLarge(e)) => assert_eq!(e.limit(), Limit::Memory),
            other => panic!("{:?}", other.map(|d| d.pages[0].rules.len())),
        }
    }
}

#[cfg(test)]
mod transfer_tests {
    use super::sets_transfer;
    use hayro_interpret::hayro_syntax::Pdf;
    use std::sync::Arc;

    /// A one-page document whose page states `gs` as a graphics state of
    /// its own, written inside its resources.
    fn with_state(gs: &str) -> Pdf {
        let body = format!(
            "%PDF-1.4\n\
             1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
             2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
             3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
             /Resources << /ExtGState << /G0 << {gs} >> >> >> >> endobj\n\
             trailer << /Root 1 0 R /Size 4 >>\n%%EOF\n"
        );
        Pdf::new(Arc::new(body.into_bytes())).expect("pdf")
    }

    #[test]
    fn a_transfer_function_written_inside_a_page_is_found() {
        assert!(sets_transfer(&with_state("/TR /Identity")));
        assert!(sets_transfer(&with_state("/TR2 [/Default]")));
        assert!(!sets_transfer(&with_state("/CA 0.5 /ca 0.5")));
    }
}

#[cfg(test)]
mod probe_tests {
    use super::{extract_bytes, extract_bytes_with, ExtractOptions};

    /// A page covered by one grey image of vertical stripes, two pixels
    /// wide: ink on ground across every row, as a scanned table reads.
    fn striped_page() -> Vec<u8> {
        striped_page_drawn(100)
    }

    /// A 100-unit page with an image of stripes drawn `side` units square.
    fn striped_page_drawn(side: u32) -> Vec<u8> {
        let pixels: Vec<u8> = (0..64 * 64)
            .map(|i| if (i % 64) / 2 % 2 == 0 { 0 } else { 255 })
            .collect();
        let content = format!("q {side} 0 0 {side} 0 0 cm /Im0 Do Q");
        let content = content.as_bytes();
        let mut pdf = Vec::new();
        pdf.extend_from_slice(
            b"%PDF-1.4\n\
              1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
              2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
              3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
              /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >> endobj\n",
        );
        pdf.extend_from_slice(
            format!("4 0 obj << /Length {} >> stream\n", content.len()).as_bytes(),
        );
        pdf.extend_from_slice(content);
        pdf.extend_from_slice(b"\nendstream endobj\n");
        pdf.extend_from_slice(
            format!(
                "5 0 obj << /Type /XObject /Subtype /Image /Width 64 /Height 64 \
                 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {} >> stream\n",
                pixels.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&pixels);
        pdf.extend_from_slice(b"\nendstream endobj\ntrailer << /Root 1 0 R /Size 6 >>\n%%EOF\n");
        pdf
    }

    #[test]
    fn an_image_is_probed_unless_no_router_will_read_it() {
        let probed = extract_bytes(striped_page()).expect("pdf");
        assert_eq!(probed.pages[0].images.len(), 1);
        assert_eq!(
            probed.pages[0].images[0].texty,
            Some(true),
            "stripes read as text structure"
        );
        let mut options = ExtractOptions::default();
        options.probe_images = false;
        let unprobed = extract_bytes_with(striped_page(), &options).expect("pdf");
        assert_eq!(unprobed.pages[0].images.len(), 1);
        assert_eq!(unprobed.pages[0].images[0].texty, None);
    }

    #[test]
    fn an_image_too_small_to_route_is_probed_only_for_an_engine() {
        // 20 units square on a 100-unit page: a twenty-fifth of it, under
        // the tenth a model is sent.
        let small = || striped_page_drawn(20);
        let doc = extract_bytes(small()).expect("pdf");
        assert_eq!(doc.pages[0].images[0].texty, None, "no model reads it");
        let mut options = ExtractOptions::default();
        options.probe_small_images = true;
        let doc = extract_bytes_with(small(), &options).expect("pdf");
        assert_eq!(
            doc.pages[0].images[0].texty,
            Some(true),
            "an engine reads it"
        );
        // An image a model is sent is probed either way.
        let doc = extract_bytes(striped_page_drawn(40)).expect("pdf");
        assert_eq!(doc.pages[0].images[0].texty, Some(true));
    }
}

#[cfg(test)]
mod clip_tests {
    use super::extract_bytes;

    /// A one-page document of `content`, its font Helvetica as `/F1`.
    fn page(content: &str) -> Vec<u8> {
        let objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
              /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"
                .to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            [
                format!("<< /Length {} >>\nstream\n", content.len()).as_bytes(),
                content.as_bytes(),
                b"\nendstream",
            ]
            .concat(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for o in offsets {
            pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer << /Root 1 0 R /Size {} >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    fn read(content: &str) -> String {
        let doc = extract_bytes(page(content)).expect("pdf");
        doc.pages[0]
            .glyphs
            .iter()
            .map(|g| g.text.as_str())
            .collect()
    }

    #[test]
    fn text_drawn_wholly_outside_the_clip_is_not_read() {
        // A frame's own words inside its clip, and other words set under a
        // clip of the page's left half but drawn in the right half: those
        // show nowhere.
        let text = read(
            "q 0 0 150 200 re W n BT /F1 12 Tf 1 0 0 1 20 100 Tm (Shown) Tj ET Q \
             q 0 0 150 200 re W n BT /F1 12 Tf 1 0 0 1 200 100 Tm (Hidden) Tj ET Q",
        );
        assert!(text.contains("Shown"), "{text:?}");
        assert!(!text.contains("Hidden"), "{text:?}");
        // Under a clip some of whose text shows, all of it stays: a word
        // drawn once per cell it crosses, each copy cut to its cell.
        let text = read(
            "q 0 0 150 200 re W n \
             BT /F1 12 Tf 1 0 0 1 20 100 Tm (Left) Tj ET \
             BT /F1 12 Tf 1 0 0 1 200 100 Tm (Right) Tj ET Q",
        );
        assert!(text.contains("Left") && text.contains("Right"), "{text:?}");
        // A word the clip's edge cuts through shows its inside part.
        let text = read("q 0 0 150 200 re W n BT /F1 12 Tf 1 0 0 1 140 100 Tm (Edge) Tj ET Q");
        assert!(text.contains("Edge"), "{text:?}");
    }

    #[test]
    fn invisible_text_outside_the_clip_is_still_read() {
        // An OCR'd scan's words, drawn invisible: the document's text,
        // wherever a clip leaves them.
        let text = read(
            "q 0 0 150 200 re W n \
             BT 3 Tr /F1 12 Tf 1 0 0 1 200 100 Tm (Layer) Tj ET Q",
        );
        assert!(text.contains("Layer"), "{text:?}");
    }
}
