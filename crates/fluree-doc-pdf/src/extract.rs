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
    // At most five corners, the last the first again: a longer path is no
    // rectangle, and is not walked to its end.
    let mut pts = [kurbo::Point::ZERO; 5];
    let mut n = 0;
    for el in p.elements() {
        match el {
            PathEl::MoveTo(q) if n == 0 => {
                pts[0] = *q;
                n = 1;
            }
            PathEl::LineTo(q) if n < 5 => {
                pts[n] = *q;
                n += 1;
            }
            PathEl::ClosePath => {}
            _ => return false,
        }
    }
    if n == 5 && pts[0].distance(pts[4]) < 0.01 {
        n = 4;
    }
    let near = |a: f64, b: f64| (a - b).abs() < 0.01;
    n == 4
        && (0..4).all(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % 4]);
            near(a.x, b.x) || near(a.y, b.y)
        })
}

/// Annotations read from a page for their rectangles, at most: `/Annots` is
/// an arbitrary array.
const MAX_APPEARANCES: usize = 10_000;

/// Where the page's annotations draw their appearances, in the space glyphs
/// are read in. hayro draws an annotation's normal appearance, unless the
/// annotation is hidden, after the page's own content, cut to the
/// annotation's rectangle. A fill there is the annotation's, not the page's.
fn appearance_boxes(page: &hayro_syntax::page::Page<'_>) -> Vec<BBox> {
    use hayro_syntax::object::{Array, Dict, Number, Stream};
    let Some(annots) = page.raw().get::<Array<'_>>(b"Annots") else {
        return Vec::new();
    };
    let to_page = page.initial_transform(true).to_kurbo();
    annots
        .iter::<Dict<'_>>()
        .take(MAX_APPEARANCES)
        .filter(|a| a.get::<u32>(b"F").unwrap_or(0) & 2 == 0)
        .filter(|a| {
            a.get::<Dict<'_>>(b"AP")
                .and_then(|ap| ap.get::<Stream<'_>>(b"N"))
                .is_some()
        })
        .filter_map(|a| {
            let v: Vec<f64> = a
                .get::<Array<'_>>(b"Rect")?
                .iter::<Number>()
                .map(|n| n.as_f64())
                .collect();
            (v.len() == 4).then(|| {
                let r = Rect::new(v[0], v[1], v[2], v[3]).abs();
                crate::geom::from_kurbo((to_page * r.to_path(0.1)).bounding_box())
            })
        })
        .collect()
}

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
    ///
    /// Measured only where something reads it: a raster image too small to
    /// be routed to a model, under a tenth of the page, is decoded only for
    /// an OCR engine, which reads the texty ones ([`crate::route::ocr_regions`],
    /// [`ExtractOptions::probe_small_images`]), and reads false otherwise;
    /// so does every image when extraction was told nothing will read it
    /// ([`ExtractOptions::probe_images`]).
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
}

/// How [`extract_bytes_with`] reads a document.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExtractOptions {
    /// Decode raster images to measure [`ImagePlacement::texty`]. The router
    /// reads it ([`crate::route::decide`], for a region it would send to a
    /// model); the decode is most of what an image costs. A caller that will
    /// not route can turn it off. On by default.
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
    /// The title the file's information dictionary states (`/Title`), as
    /// written: what the document is called by whoever made the file, and
    /// often stale or a file's name.
    pub title: Option<String>,
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

/// How far apart, per 8-bit sRGB channel, a glyph's colour and its
/// ground's may be for the glyph to vanish on it: the rounding of two
/// conversions to sRGB from colour spaces that state the same colour
/// (DeviceGray 1 and DeviceRGB white, an ICC space and its device twin).
/// Text a reader can make out on a tint differs from it by far more: a
/// light grey caption on a pale panel, by 20 or more.
const GROUND_TOLERANCE: u8 = 2;

/// How near, as a share of the type size, two draws of a glyph stand to be
/// one glyph drawn twice. Copies drawn in one place, a word once per cell it
/// crosses or lettering once per slice of a gradient, share a pen position
/// to float noise; the next letter of a word stands a narrow letter's width
/// on, a quarter of the type size or more.
const SAME_PLACE: f64 = 0.05;

/// Float noise in a box computed from a transformed outline.
const BOX_EPS: f64 = 0.1;

/// Whether `outer` holds all of `b`, to float noise.
fn holds(outer: &BBox, b: &BBox) -> bool {
    b.x0 >= outer.x0 - BOX_EPS
        && b.x1 <= outer.x1 + BOX_EPS
        && b.y0 >= outer.y0 - BOX_EPS
        && b.y1 <= outer.y1 + BOX_EPS
}

/// Whether a box painted over or under glyph `g` is a bar across its line
/// rather than a box round it: narrower across the line than half the
/// type's size. Such a bar hides no letter, under the glyph or over it. A
/// link's underline drawn through the underscores of its URL, a rule over a
/// dash, a cell's rule: each paints the very mark it lies on, and the text
/// still reads. A bar is a fraction of the type's size (a link's underline
/// 0.72pt under 11pt type); a box laid over or under a word to hide it is
/// about the type's size (a 9.6pt box over 10pt type).
fn bar_across(c: &BBox, g: &Glyph) -> bool {
    let turned = (g.rotation_deg.rem_euclid(180.0) - 90.0).abs() < 45.0;
    let across = if turned { c.x1 - c.x0 } else { c.y1 - c.y0 };
    across < 0.5 * g.font_size as f64
}

/// Whether two boxes share more than an edge, to float noise.
fn meets(a: &BBox, b: &BBox) -> bool {
    a.x0 < b.x1 - BOX_EPS && a.x1 > b.x0 + BOX_EPS && a.y0 < b.y1 - BOX_EPS && a.y1 > b.y0 + BOX_EPS
}

struct Collector<'a> {
    page: usize,
    glyphs: Vec<Glyph>,
    images: Vec<ImagePlacement>,
    rules: Vec<Rule>,
    fills: Vec<Fill>,
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
    /// Parallel to `glyphs`: whether the glyph is painted, filled or
    /// stroked rather than set invisible, with an outline to judge it by.
    /// Only such a glyph can be hidden ([`Collector::covered`]).
    painted: Vec<bool>,
    /// Opaque rectangles filled on the page, each with the number of glyphs
    /// drawn before it: what it hides was drawn first and lies under it.
    covers: Vec<(BBox, usize)>,
    /// What the page paints, in order, for the text it hides; see [`Hiding`].
    hide: Hiding,
    /// Whether the next shape is drawn through a soft mask, or blended other
    /// than normally: either lets what is under it show.
    masked: bool,
    blended: bool,
    /// The transparency groups open, innermost last: whether each puts its
    /// content down as drawn, at full opacity, normal blend and no mask.
    groups: Vec<bool>,
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

/// What a page paints, kept to tell the text it hides: the paths and images
/// in the order they are painted, and for each glyph its colour and the clip
/// it is drawn under. See [`Collector::grounded`] and [`Collector::covered`].
#[derive(Default)]
struct Hiding {
    /// Paths and images painted so far, glyphs too: the next one's number.
    seq: usize,
    /// Every path, image and glyph painted, in order.
    paints: Vec<Painted>,
    /// Parallel to `glyphs`: the glyph's number and its colour, where it is
    /// filled in one colour and nothing else (render mode 0, painted
    /// normally, at full opacity, not masked).
    inks: Vec<Option<(usize, [u8; 4])>>,
    /// Parallel to `glyphs`: the box the clips in force let the glyph show
    /// in, all it can show lying inside it.
    clip_boxes: Vec<Option<BBox>>,
}

/// One path, image or glyph painted on a page.
#[derive(Clone, Copy)]
struct Painted {
    /// Its number, in the order the page paints.
    seq: usize,
    /// The part of its box that shows.
    bbox: BBox,
    kind: PaintKind,
    /// A fill's colour where it is a ground text can be set on
    /// ([`Collector::ground_colour`]); a glyph's where it is filled in one.
    colour: Option<[u8; 4]>,
}

#[derive(Clone, Copy, PartialEq)]
enum PaintKind {
    Stroke,
    Fill,
    Image,
    Glyph,
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
    /// Whether the image's pixels look like text, decoding it unless the
    /// document has drawn it before.
    fn texty(&mut self, image: &hayro_interpret::RasterImage<'_>) -> bool {
        if self.off {
            return false;
        }
        let id = image.stream().dict().obj_id();
        if let Some(found) = id.and_then(|id| self.found.get(&id)) {
            if self.plain == Some(true) {
                return *found;
            }
            self.again = true;
        }
        let mut texty = false;
        // A low-resolution decode is plenty for a structure probe, where
        // the format offers one.
        image.with_rgba(
            |data, _| texty = looks_like_text(&data),
            Some((PROBE_DIM, PROBE_DIM)),
        );
        if let Some(id) = id.filter(|_| self.plain != Some(false)) {
            self.found.insert(id, texty);
        }
        texty
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

impl<'a> Collector<'a> {
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
    /// of whose inked glyphs shows inside it ([`Collector::draw_glyph`]),
    /// and every inked glyph wholly outside its clip that is drawn nowhere
    /// else or shows where it is drawn again. A space is left: its pen can
    /// stand past a cell's edge at the end of a line of words that show,
    /// and it parts them from the next line's.
    ///
    /// A clip that shows some of its text can still hide a copy of it: a
    /// datasheet draws one block diagram twice, each copy cut by its form to
    /// one of the two figures, and the hidden copy's labels ran through the
    /// visible ones (`SSPPII FFllaasshh`); a tax form cuts its notes to their
    /// boxes, and the lines past a box read on into the next. Such a glyph
    /// goes. Where a glyph is drawn again in the same place, as a word is
    /// once per cell it crosses or lettering once per slice of a gradient,
    /// the copy outside its clip goes only when another copy shows: the
    /// copies stand for one glyph, and if none of them is judged to show,
    /// the page is not read well enough here to take it away.
    fn clipped_out(&self) -> Vec<bool> {
        let mut tally: std::collections::HashMap<usize, (usize, usize)> = Default::default();
        for &(id, shown, inked) in self.clip_votes.iter().flatten() {
            if inked {
                let t = tally.entry(id).or_default();
                t.0 += 1;
                t.1 += usize::from(shown);
            }
        }
        let mut out: Vec<bool> = self
            .clip_votes
            .iter()
            .map(|v| {
                v.is_some_and(|(id, _, _)| {
                    tally
                        .get(&id)
                        .is_some_and(|&(n, shown)| n > 0 && shown == 0)
                })
            })
            .collect();
        let hidden: Vec<usize> = (0..self.glyphs.len())
            .filter(|&i| !out[i] && matches!(self.clip_votes[i], Some((_, false, true))))
            .collect();
        if hidden.is_empty() {
            return out;
        }
        // Shown: judged inside its clip, or painted with no clip to judge
        // it by but the page.
        let shown = |j: usize| match self.clip_votes[j] {
            Some((_, shown, _)) => shown,
            None => self.painted[j] && self.hide.clip_boxes[j].is_some(),
        };
        let mut by_text: std::collections::HashMap<&str, Vec<usize>> = Default::default();
        for (j, g) in self.glyphs.iter().enumerate() {
            if self.painted[j] {
                by_text.entry(g.text.as_str()).or_default().push(j);
            }
        }
        for i in hidden {
            let g = &self.glyphs[i];
            let near = SAME_PLACE * g.font_size.max(1.0) as f64;
            let mut again = by_text[g.text.as_str()].iter().copied().filter(|&j| {
                j != i
                    && (self.glyphs[j].origin.0 - g.origin.0).abs() <= near
                    && (self.glyphs[j].origin.1 - g.origin.1).abs() <= near
            });
            let first = again.next();
            let others: Vec<usize> = first.into_iter().chain(again).collect();
            if others.is_empty() && self.lettered(i, None) {
                continue;
            }
            if others.is_empty() || others.iter().any(|&j| shown(j)) {
                out[i] = true;
            }
        }
        out
    }

    /// Whether something drawn where glyph `i` stands could show its
    /// letters, painted after paint number `after`: an image, any size, or
    /// a shape filled that is as tall across the line as type is, a quarter
    /// of the type's size to a little over it. A scanned page sets its words
    /// as text and paints the scan over them; a flyer draws its badge's
    /// lettering as outlines (0.7 to 0.8 of their type's size across) and
    /// keeps the words as text under a clip that shows none of them. Either
    /// way the text is what the page shows, and stays. A box drawn round a
    /// label (twice its type), a panel, a figure's frame or the page's
    /// ground is taller; a rule is thinner than its letters.
    fn lettered(&self, i: usize, after: Option<usize>) -> bool {
        let g = &self.glyphs[i];
        let b = g.bbox.unwrap_or(BBox {
            x0: g.origin.0,
            y0: g.origin.1,
            x1: g.origin.0,
            y1: g.origin.1,
        });
        let size = g.font_size.max(1.0) as f64;
        let turned = (g.rotation_deg.rem_euclid(180.0) - 90.0).abs() < 45.0;
        self.hide.paints.iter().any(|p| {
            let across = if turned {
                p.bbox.x1 - p.bbox.x0
            } else {
                p.bbox.y1 - p.bbox.y0
            };
            after.is_none_or(|a| p.seq > a)
                && match p.kind {
                    PaintKind::Image => true,
                    PaintKind::Fill => (0.25 * size..=1.2 * size).contains(&across),
                    _ => false,
                }
                && meets(&p.bbox, &b)
        })
    }

    /// Note a path or image painted on the page, by the part of its box that
    /// shows, with its colour where it is a ground ([`Collector::ground_colour`]).
    fn note_paint(&mut self, b: BBox, kind: PaintKind, colour: Option<[u8; 4]>) {
        let seq = self.hide.seq;
        self.hide.seq += 1;
        if let Some(bbox) = self.visible(b) {
            self.hide.paints.push(Painted {
                seq,
                bbox,
                kind,
                colour,
            });
        }
    }

    /// The colour of a fill text can be set on and vanish: one rectangle
    /// filled in a solid colour at full opacity, painted normally, nothing
    /// under it showing through, under rectangle clips only. The same
    /// conditions as a cover's ([`Collector::note_cover`]): the fill shows
    /// as one flat colour over its whole box.
    fn ground_colour(
        &self,
        path: &kurbo::BezPath,
        paint: &hayro_interpret::Paint<'_>,
        mode: &hayro_interpret::PathDrawMode,
    ) -> Option<[u8; 4]> {
        if !matches!(mode, hayro_interpret::PathDrawMode::Fill(_))
            || self.masked
            || self.blended
            || !self.groups.iter().all(|&g| g)
            || !self.clip_rects.iter().all(|&r| r)
        {
            return None;
        }
        let hayro_interpret::Paint::Color(c) = paint else {
            return None;
        };
        if !is_rect(path) {
            return None;
        }
        let rgba = c.to_rgba().to_rgba8();
        (rgba[3] == u8::MAX).then_some(rgba)
    }

    /// The glyphs set on a ground of their own colour: the topmost thing
    /// painted under the glyph is an opaque fill of one colour holding all
    /// of the glyph that shows, and the glyph is filled in that colour. A
    /// template's markers set white on its white page, a form's
    /// instructions printed in the colour of their box: no reader sees
    /// them, and read, they ran into the text that shows.
    ///
    /// Only a glyph filled and nothing else (render mode 0): a stroked
    /// outline shows on any ground. Where an image, or lettering drawn as
    /// shapes, is painted at its place after the ground, the text is what
    /// that shows, and stays ([`Collector::lettered`]). Anything painted over the fill before
    /// the glyph, an image, a shading, a path of another colour, under any
    /// part of it, leaves the glyph on that, and it stays. Colours are
    /// compared as the page shows them, in sRGB after conversion from each
    /// one's own space, to within [`GROUND_TOLERANCE`].
    fn grounded(&self) -> Vec<bool> {
        let mut out = vec![false; self.glyphs.len()];
        let grounds: Vec<&Painted> = self
            .hide
            .paints
            .iter()
            .filter(|p| p.kind == PaintKind::Fill && p.colour.is_some())
            .collect();
        if grounds.is_empty() {
            return out;
        }
        let close = |a: [u8; 4], b: [u8; 4]| {
            a[..3]
                .iter()
                .zip(&b[..3])
                .all(|(x, y)| x.abs_diff(*y) <= GROUND_TOLERANCE)
        };
        for (i, g) in self.glyphs.iter().enumerate() {
            let Some((seq, colour)) = self.hide.inks[i] else {
                continue;
            };
            // A space, with no ink, is where its pen is.
            let b = g.bbox.unwrap_or(BBox {
                x0: g.origin.0,
                y0: g.origin.1,
                x1: g.origin.0,
                y1: g.origin.1,
            });
            // What of the glyph shows: its ink cut to its clip.
            let b = match self.hide.clip_boxes[i] {
                Some(c) => BBox {
                    x0: b.x0.max(c.x0),
                    y0: b.y0.max(c.y0),
                    x1: b.x1.min(c.x1),
                    y1: b.y1.min(c.y1),
                },
                None => b,
            };
            if b.x1 < b.x0 || b.y1 < b.y0 {
                continue;
            }
            // The latest ground of its colour under all of it, drawn before it.
            let Some(ground) = grounds.iter().rev().filter(|p| p.seq < seq).find(|p| {
                p.colour.is_some_and(|c| close(c, colour))
                    && holds(&p.bbox, &b)
                    && !bar_across(&p.bbox, g)
            }) else {
                continue;
            };
            // Nothing painted between the ground and the glyph reaches it:
            // a shape, an image, or a glyph, a warning sign's white
            // triangle set on a black box for its black mark to stand on.
            // What is painted in the ground's own colour changes nothing.
            let after = self.hide.paints.partition_point(|p| p.seq <= ground.seq);
            let before = self.hide.paints.partition_point(|p| p.seq < seq);
            // Latest first: what lies on a ground is mostly drawn just
            // before the text set on it.
            // And nothing painted after the ground shows its letters: a
            // scanned page sets its words on a ground of their own colour
            // and paints the scan over them.
            if !self.hide.paints[after..before]
                .iter()
                .rev()
                .any(|p| meets(&p.bbox, &b) && !p.colour.is_some_and(|c| close(c, colour)))
                && !self.lettered(i, Some(ground.seq))
            {
                out[i] = true;
            }
        }
        out
    }

    /// Note a fill that hides what was drawn under it before: one rectangle,
    /// painted a solid colour at full opacity, nothing under it showing
    /// through. The fill's opacity is its paint's alpha, the graphics
    /// state's `ca` (a stroke's `CA` paints no fill); a soft mask, another
    /// blend mode or a group less than opaque lets what is under it show,
    /// and a fill in optional content switched off is never drawn. Only
    /// under rectangle clips, so the part of it that shows is its box cut
    /// to the clip. See [`Collector::covered`].
    fn note_cover(&mut self, path: &kurbo::BezPath, paint: &hayro_interpret::Paint<'_>) {
        if self.glyphs.is_empty()
            || self.masked
            || self.blended
            || !self.groups.iter().all(|&g| g)
            || !self.clip_rects.iter().all(|&r| r)
        {
            return;
        }
        let hayro_interpret::Paint::Color(c) = paint else {
            return;
        };
        // The shape first: it costs nothing to ask, and the colour is
        // converted only for a rectangle.
        if !is_rect(path) || c.to_rgba().to_rgba8()[3] < u8::MAX {
            return;
        }
        let b = crate::geom::from_kurbo(path.bounding_box());
        let clip = self.clips.last().copied().unwrap_or(self.page_box);
        let b = BBox {
            x0: b.x0.max(clip.x0).max(self.page_box.x0),
            y0: b.y0.max(clip.y0).max(self.page_box.y0),
            x1: b.x1.min(clip.x1).min(self.page_box.x1),
            y1: b.y1.min(clip.y1).min(self.page_box.y1),
        };
        if b.x1 > b.x0 && b.y1 > b.y0 {
            self.covers.push((b, self.glyphs.len()));
        }
    }

    /// The glyphs an opaque fill painted after them hides: a slide's band
    /// laid over the template's footer and page number, its own title then
    /// set on the band; read, the hidden footer ran through the title a
    /// letter at a time. A glyph is hidden when all of its ink that shows
    /// lies under a cover drawn later, its ink box cut to the clip it is
    /// drawn under; a space, with no ink, where its pen does. A form's box
    /// laid over the part of its instructions their clip leaves showing
    /// hides them as wholly as one laid over all of each letter. A cover
    /// whose edge cuts through what shows of a glyph drawn before it hides
    /// nothing: the glyph shows in part, and its word with it. A fill inside an
    /// annotation's rectangle is the annotation's appearance, drawn after
    /// the page's content and no part of it, so never a cover.
    fn covered(&self, annotated: &[BBox]) -> Vec<bool> {
        // Horizontal bands of the page, each listing the painted glyphs
        // whose ink reaches into it, in draw order.
        const BANDS: usize = 64;
        let mut out = vec![false; self.glyphs.len()];
        let covers: Vec<(BBox, usize)> = self
            .covers
            .iter()
            .copied()
            .filter(|(c, _)| {
                !annotated.iter().any(|a| {
                    c.x0 >= a.x0 - 1.0
                        && c.x1 <= a.x1 + 1.0
                        && c.y0 >= a.y0 - 1.0
                        && c.y1 <= a.y1 + 1.0
                })
            })
            .collect();
        if covers.is_empty() {
            return out;
        }
        let top = self.page_box.y0;
        let height = (self.page_box.y1 - self.page_box.y0).max(1.0);
        let band =
            |y: f64| (((y - top) / height * BANDS as f64).floor().max(0.0) as usize).min(BANDS - 1);
        let mut bands: Vec<Vec<usize>> = vec![Vec::new(); BANDS];
        for (i, g) in self.glyphs.iter().enumerate() {
            if let (true, Some(b)) = (self.painted[i], g.bbox) {
                for k in band(b.y0)..=band(b.y1) {
                    bands[k].push(i);
                }
            }
        }
        for (c, before) in covers {
            let (k0, k1) = (band(c.y0), band(c.y1));
            let mut hid = Vec::new();
            let mut torn = false;
            'bands: for k in k0..=k1 {
                for &i in &bands[k] {
                    if i >= before {
                        break;
                    }
                    let Some(ink) = self.glyphs[i].bbox else {
                        continue;
                    };
                    // Each glyph once: in the first band it shares with the
                    // cover.
                    if band(ink.y0).max(k0) != k {
                        continue;
                    }
                    let b = match self.hide.clip_boxes[i] {
                        Some(clip) => BBox {
                            x0: ink.x0.max(clip.x0),
                            y0: ink.y0.max(clip.y0),
                            x1: ink.x1.min(clip.x1),
                            y1: ink.y1.min(clip.y1),
                        },
                        None => ink,
                    };
                    if b.x1 <= b.x0 || b.y1 <= b.y0 || !meets(&b, &c) {
                        continue;
                    }
                    // A bar across the line hides nothing ([`bar_across`]).
                    if !holds(&c, &b) || bar_across(&c, &self.glyphs[i]) {
                        torn = true;
                        break 'bands;
                    }
                    hid.push(i);
                }
            }
            if torn || hid.is_empty() {
                continue;
            }
            for i in hid {
                out[i] = true;
            }
            for (i, g) in self.glyphs[..before].iter().enumerate() {
                if self.painted[i] && g.bbox.is_none() && c.contains(g.origin.0, g.origin.1) {
                    out[i] = true;
                }
            }
        }
        out
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

impl<'a> Device<'a> for Collector<'a> {
    // hayro states the mask and the blend mode before every shape and glyph
    // it draws, so these are always the next one's.
    fn set_soft_mask(&mut self, m: Option<hayro_interpret::SoftMask<'a>>) {
        self.masked = m.is_some();
    }
    fn set_blend_mode(&mut self, b: hayro_interpret::BlendMode) {
        self.blended = b != hayro_interpret::BlendMode::Normal;
    }
    fn draw_path(
        &mut self,
        path: &kurbo::BezPath,
        transform: Affine,
        paint: &hayro_interpret::Paint<'a>,
        mode: &hayro_interpret::PathDrawMode,
    ) {
        // A stroke shorter than its pen is wide is a bar across it: see
        // [`pen_bars`]. Each bar is read as any shape is, a rule or a fill.
        if let hayro_interpret::PathDrawMode::Stroke(props) = mode {
            if let Some(bars) = pen_bars(path, transform, props) {
                for &b in &bars {
                    self.note_paint(b, PaintKind::Stroke, None);
                }
                for b in bars {
                    if !self.page_box.intersects(&b) {
                        continue;
                    }
                    match rule::classify(b, self.page) {
                        Some(rule::Shape::Rule(x)) => {
                            if let Some(x) = self.clip_rule(x) {
                                self.rules.push(x);
                            }
                        }
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
        // Bounding box only. A table grid is axis-aligned rectangles and
        // segments; curve detail is irrelevant and expensive to keep.
        let path = transform * path.clone();
        let r: Rect = path.bounding_box();
        let drawn = crate::geom::from_kurbo(r);
        if !self.page_box.intersects(&drawn) {
            return;
        }
        // A stroke's paint reaches half its pen's width past the path.
        let reach = match mode {
            hayro_interpret::PathDrawMode::Stroke(props) => {
                props.line_width as f64 * 0.5 * transform.determinant().abs().sqrt()
            }
            _ => 0.0,
        };
        let ground = self.ground_colour(&path, paint, mode);
        self.note_paint(
            BBox {
                x0: drawn.x0 - reach,
                y0: drawn.y0 - reach,
                x1: drawn.x1 + reach,
                y1: drawn.y1 + reach,
            },
            if matches!(mode, hayro_interpret::PathDrawMode::Fill(_)) {
                PaintKind::Fill
            } else {
                PaintKind::Stroke
            },
            ground,
        );
        if matches!(mode, hayro_interpret::PathDrawMode::Fill(_)) {
            self.note_cover(&path, paint);
        }
        if !matches!(mode, hayro_interpret::PathDrawMode::Fill(_)) {
            if let Some(trace) = trace_of(&path, drawn, self.page) {
                self.traces.push(trace);
            }
            self.ticks.extend(marks_of(&path, self.page));
        }
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
            let rules: Vec<rule::Rule> = rules
                .into_iter()
                .filter_map(|x| self.clip_rule(x))
                .filter(|x| self.page_box.intersects(&x.bbox))
                .collect();
            self.rules.extend(rules);
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
    fn push_clip_path(&mut self, c: &hayro_interpret::ClipPath) {
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
        o: f32,
        m: Option<hayro_interpret::SoftMask<'a>>,
        b: hayro_interpret::BlendMode,
    ) {
        self.groups
            .push(o >= 1.0 && m.is_none() && b == hayro_interpret::BlendMode::Normal);
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
        self.note_paint(b, PaintKind::Image, None);
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
                    false
                } else {
                    self.probes.texty(ri)
                }
            }
            // Stencils are 1-bit masks — the format scanned text and line art
            // arrive in. Treat as text-bearing.
            hayro_interpret::Image::Stencil(_) => true,
        };
        self.images.push(ImagePlacement {
            bbox,
            texty,
            ppi,
            bitonal,
            grid,
        });
    }
    fn pop_clip_path(&mut self) {
        self.clips.pop();
        self.clip_rects.pop();
        self.clip_ids.pop();
    }
    fn pop_transparency_group(&mut self) {
        self.groups.pop();
    }

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
        paint: &hayro_interpret::Paint<'a>,
        mode: &hayro_interpret::GlyphDrawMode,
    ) {
        let mut text = match glyph.as_unicode() {
            Some(hayro_interpret::hayro_cmap::BfString::Char(c)) => c.to_string(),
            Some(hayro_interpret::hayro_cmap::BfString::String(s)) => s,
            None => String::new(),
        };
        // No text from the font's ToUnicode map or from a name the Adobe
        // Glyph List lists whole (an empty reading says no more than none):
        // the program's own name for the glyph, read by the list's rules
        // (`h.sc`, `f_i`), if it has one (see `faces::Faces::named`); in a CID
        // font set in TrueType, the program's own Unicode cmap
        // (`faces::Faces::mapped`); in a CID font numbered in an Adobe
        // collection, the collection's map (`faces::Faces::collection`).
        if text.is_empty() {
            if let HGlyph::Outline(o) = glyph {
                let (key, id) = (o.font_cache_key(), o.glyph_id().to_u32());
                text = self
                    .faces
                    .named(key, id)
                    .or_else(|| self.faces.mapped(key, id).map(String::from))
                    .or_else(|| self.faces.collection(key, id))
                    .unwrap_or_default();
            }
        }

        // A glyph of one of TeX's fonts whose vector labels glyphs by their
        // codes (`/equal` at 61, where the math italic draws a slash): read
        // by TeX's layout for the font's family; see `crate::tex`.
        if let HGlyph::Outline(o) = glyph {
            if let Some(tex) = self
                .faces
                .tex(o.font_cache_key(), o.glyph_id().to_u32(), &text)
            {
                text = tex.to_string();
            }
        }

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
        // A clip of any shape lies inside its box, and the boxes of the
        // clips in force cut to one another (`clips`) hold all a glyph can
        // show in: a glyph outside them shows nowhere, whatever the clips'
        // shapes. One that meets them is judged to show, though a clip cut
        // to curves may yet hide it: the judgment errs toward keeping text.
        // Text set invisible, an OCR'd scan's words over or under its
        // image, is the document's text wherever it lies, and a Type3
        // glyph, with no box, is not judged; a space, with no ink, stands
        // where its pen does.
        let vote = match (self.clips.last(), self.clip_ids.last()) {
            (Some(clip), Some(&id))
                if !matches!(mode, hayro_interpret::GlyphDrawMode::Invisible)
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
        // Filled and stroked (render mode 2), a glyph is drawn twice, filled
        // then stroked: the outline shows whatever the fill does, so the
        // fill's colour says nothing about what shows.
        let seq = self.hide.seq;
        self.hide.seq += 1;
        if matches!(mode, hayro_interpret::GlyphDrawMode::Stroke(_)) {
            if let (Some(last), Some(ink)) = (self.glyphs.last(), self.hide.inks.last_mut()) {
                if last.origin == origin && last.text == text {
                    *ink = None;
                }
            }
        }
        let filled = matches!(mode, hayro_interpret::GlyphDrawMode::Fill)
            && matches!(glyph, HGlyph::Outline(_))
            && !self.masked
            && !self.blended
            && self.groups.iter().all(|&g| g);
        let ink_colour = match paint {
            hayro_interpret::Paint::Color(c) if filled => Some(c.to_rgba().to_rgba8()),
            _ => None,
        };
        self.hide.inks.push(ink_colour.map(|c| (seq, c)));
        // A glyph that shows is painted like a shape: what is set on it
        // stands on it.
        if let (Some(b), false) = (
            bbox,
            matches!(mode, hayro_interpret::GlyphDrawMode::Invisible),
        ) {
            if let Some(bbox) = self.visible(b) {
                self.hide.paints.push(Painted {
                    seq,
                    bbox,
                    kind: PaintKind::Glyph,
                    colour: ink_colour,
                });
            }
        }
        self.hide
            .clip_boxes
            .push(Some(self.clips.last().copied().unwrap_or(self.page_box)));
        self.painted.push(
            !matches!(mode, hayro_interpret::GlyphDrawMode::Invisible)
                && matches!(glyph, HGlyph::Outline(_)),
        );
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

/// Whether decoded pixels look like text or line structure rather than a
/// photograph. See [`ImagePlacement::texty`].
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
    if w < 8 || h < 8 || luma.len() < w * h {
        return false;
    }

    let (mut bimodal, mut lo, mut hi) = (0usize, u8::MAX, u8::MIN);
    for &v in luma.iter() {
        bimodal += usize::from(!(64..192).contains(&v));
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if (bimodal as f64) < MIN_BIMODAL_FRACTION * luma.len() as f64 {
        return false;
    }

    // Binarise at the luminance midpoint and count row transitions.
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
    extract_bytes_with(data, &ExtractOptions::default())
}

/// [`extract_bytes`], read as `options` say.
pub fn extract_bytes_with(
    data: Vec<u8>,
    options: &ExtractOptions,
) -> Result<Document, ExtractError> {
    let pdf = Pdf::new(Arc::new(data)).map_err(|e| ExtractError::Parse(format!("{e:?}")))?;
    let cache = InterpreterCache::new();
    let settings = InterpreterSettings::default();
    let mut pages = Vec::new();
    let mut ink: Vec<PageInk> = Vec::new();
    let mut letters = crate::ligature::Letters::default();
    let mut unread: Vec<(usize, usize, String, kurbo::BezPath)> = Vec::new();
    let mut faces = crate::faces::Faces::default();
    let mut weights = std::collections::HashMap::new();
    let mut italics = std::collections::HashMap::new();
    let mut probes = Probes {
        off: !options.probe_images,
        small: options.probe_small_images,
        ..Probes::default()
    };

    for (index, page) in pdf.pages().iter().enumerate() {
        let (w, h) = page.render_dimensions();
        faces.read(page);
        let mut dev = Collector {
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
            unread: Vec::new(),
            marks: crate::marked::Marks::read(page),
            open: Vec::new(),
            symbol_fonts: std::collections::HashMap::new(),
            symbols: Vec::new(),
            clips: Vec::new(),
            clip_rects: Vec::new(),
            clip_ids: Vec::new(),
            clip_seq: 0,
            clip_votes: Vec::new(),
            hide: Hiding::default(),
            painted: Vec::new(),
            covers: Vec::new(),
            masked: false,
            blended: false,
            groups: Vec::new(),
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
        interpret_page(page, &mut ctx, &mut dev);
        // A span the page left open still covers what it drew.
        while let Some(open) = dev.open.pop() {
            if let Some(span) = open {
                dev.speak(span);
            }
        }
        dev.read_cased();
        dev.read_symbols();
        let mut out = dev.clipped_out();
        for (o, g) in out.iter_mut().zip(dev.grounded()) {
            *o |= g;
        }
        if !dev.covers.is_empty() {
            let annotated = appearance_boxes(page);
            for (o, c) in out.iter_mut().zip(dev.covered(&annotated)) {
                *o |= c;
            }
        }
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
        faces = std::mem::take(&mut dev.faces);
        weights = std::mem::take(&mut dev.weights);
        italics = std::mem::take(&mut dev.italics);
        probes = std::mem::take(&mut dev.probes);
        probes.settle(&pdf);
        unread.extend(dev.unread.drain(..).map(|(i, f, o)| (index, i, f, o)));
        let mut page = Page {
            index,
            glyphs: dev.glyphs,
            images: dev.images,
            rules: dev.rules,
            fills: dev.fills,
            dots: dev.dots,
            traces: dev.traces,
            ticks: dev.ticks,
            width: w as f64,
            height: h as f64,
            rotation: 0,
            ocr: None,
        };
        read_upright(&mut page);
        synthesize_missing_boxes(&mut page.glyphs, &page.rules);
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
        pages,
        title: crate::info::read(&pdf).title,
    })
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
    let point = |(x, y): (f64, f64)| -> (f64, f64) {
        match turn {
            90 => (y, w - x),
            180 => (w - x, h - y),
            _ => (h - y, x),
        }
    };
    let rect = |b: BBox| -> BBox {
        let (ax, ay) = point((b.x0, b.y0));
        let (bx, by) = point((b.x1, b.y1));
        BBox {
            x0: ax.min(bx),
            y0: ay.min(by),
            x1: ax.max(bx),
            y1: ay.max(by),
        }
    };
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

/// Whether a vertical rule stands between `x0` and `x1` on the baseline `y`
/// of a glyph `font` points high: a cell's border the pen crossed.
fn ruled_between(rules: &[crate::rule::Rule], x0: f64, x1: f64, y: f64, font: f64) -> bool {
    rules.iter().any(|r| {
        r.orientation == crate::rule::Orientation::Vertical
            && r.axis_pos() > x0
            && r.axis_pos() < x1
            && r.bbox.y0 <= y
            && r.bbox.y1 >= y - font * 0.5
    })
}

/// How far the middle of a character's measured widths may stand above the
/// narrowest for its widths to agree: the pen's rounding, not the text's
/// own spacing.
const AGREEING_WIDTHS: f64 = 1.1;

/// How much wider than the narrowest measure of itself a boxless glyph's
/// measured advance may be before the rest is read as a gap: a character's
/// widths at one size vary only by the pen's rounding.
const WIDER_THAN_ITSELF: f64 = 1.6;

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
fn synthesize_missing_boxes(glyphs: &mut [Glyph], rules: &[crate::rule::Rule]) {
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
    // Each glyph's measured pen delta, `None` where nothing follows it.
    let measured: Vec<Option<f64>> = candidates
        .iter()
        .map(|&i| {
            let (x, y) = glyphs[i].origin;
            let font = glyphs[i].font_size.max(1.0) as f64;
            // The nearest pen position to the right on the same baseline
            // (within the tolerance assembly uses): the pen delta is the
            // advance, whatever is drawn there.
            let tol = font * 0.25;
            let from = by_y.partition_point(|&j| glyphs[j].origin.1 < y - tol);
            by_y[from..]
                .iter()
                .take_while(|&&j| glyphs[j].origin.1 <= y + tol)
                .map(|&j| glyphs[j].origin.0 - x)
                .filter(|&d| d > 0.01 && d < font * MAX_SYNTH_ADVANCE)
                .min_by(f64::total_cmp)
        })
        .collect();
    // A character's own width, at its size: the narrowest delta measured
    // for it on the page. Measured once more widely, the pen crossed a gap
    // no space glyph was drawn for: a spreadsheet sets each cell's text in
    // one string, an amount right against its cell's edge and the next
    // cell's word moved on by the string's own offset (`1,234.95` and
    // `CASH`), and the `5`'s width took in the gap, its box the border.
    let keys: Vec<(String, i64)> = candidates
        .iter()
        .map(|&i| {
            (
                glyphs[i].text.clone(),
                (glyphs[i].font_size * 100.0).round() as i64,
            )
        })
        .collect();
    // Only where its measures agree: a font sets a character at one width,
    // so most of them sit at the narrowest. An OCR'd page's invisible text
    // sets its glyphs where the scan's letters were, each at its own
    // spacing, and its narrowest says nothing of a gap (`PA GE`).
    let mut measures: std::collections::HashMap<&(String, i64), Vec<f64>> = Default::default();
    for (k, m) in keys.iter().zip(&measured) {
        if let Some(d) = *m {
            measures.entry(k).or_default().push(d);
        }
    }
    let narrowest: std::collections::HashMap<&(String, i64), f64> = measures
        .into_iter()
        .filter_map(|(k, mut v)| {
            if v.len() < 3 {
                return None;
            }
            v.sort_by(f64::total_cmp);
            let (low, mid) = (v[0], v[v.len() / 2]);
            (mid <= low * AGREEING_WIDTHS).then_some((k, low))
        })
        .collect();
    // Each size's typical glyph width, the middle of all its measures: what
    // a glyph whose own width no agreeing measures give is taken to be,
    // where a rule drawn down between it and the next glyph says the pen
    // crossed a cell's border (a name's last letter, seen once on the page,
    // beside the next cell's date).
    let mut by_size: std::collections::HashMap<i64, Vec<f64>> = Default::default();
    for (k, m) in keys.iter().zip(&measured) {
        if let Some(d) = *m {
            by_size.entry(k.1).or_default().push(d);
        }
    }
    let typical: std::collections::HashMap<i64, f64> = by_size
        .into_iter()
        .filter(|(_, v)| v.len() >= 3)
        .map(|(s, mut v)| {
            v.sort_by(f64::total_cmp);
            (s, v[v.len() / 2])
        })
        .collect();
    for ((&i, m), k) in candidates.iter().zip(&measured).zip(&keys) {
        let (x, y) = glyphs[i].origin;
        let font = glyphs[i].font_size.max(1.0) as f64;
        let width = match *m {
            Some(d) => match (narrowest.get(k), typical.get(&k.1)) {
                (Some(&own), _) if d > own * WIDER_THAN_ITSELF && d - own > font * 0.2 => own,
                (None, Some(&t)) if t < d && ruled_between(rules, x + t, x + d, y, font) => t,
                _ => d,
            },
            None => font * SYNTH_WIDTH,
        };
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
        synthesize_missing_boxes(&mut g, &[]);
        assert!((g[1].advance.unwrap() - 11.78).abs() < 0.01);
        // Nothing follows on the line: the fallback.
        assert!((g[3].advance.unwrap() - 12.0 * SYNTH_WIDTH).abs() < 0.01);
    }

    #[test]
    fn a_type3_glyph_measured_across_a_gap_takes_its_own_width() {
        // A spreadsheet's cells, each in one Type3 string: an amount right
        // against its cell's edge, the next cell's word moved on by the
        // string's offset. Measured to the next glyph, the amount's last
        // digit took the gap.
        let glyph = |text: &str, x: f64, y: f64| Glyph {
            text: text.into(),
            bbox: None,
            page: 0,
            origin: (x, y),
            rotation_deg: 0.0,
            font_size: 9.45,
            weight: None,
            italic: false,
            advance: None,
            draw_index: 0,
            ocr: None,
        };
        let run = |text: &str, x: f64, y: f64, w: f64| -> Vec<Glyph> {
            text.chars()
                .enumerate()
                .map(|(k, c)| glyph(&c.to_string(), x + k as f64 * w, y))
                .collect()
        };
        let mut g = Vec::new();
        g.extend(run("5555255", 100.0, 20.0, 5.25));
        g.extend(run("9", 464.0, 116.25, 5.26));
        let five = g.len();
        g.push(glyph("5", 469.26, 116.25));
        g.extend(run("CASH", 479.77, 116.25, 5.5));
        // A letter seen once, measured across its cell's edge.
        g.extend(run("ABCDEFG", 200.0, 46.5, 5.25));
        let z = g.len();
        g.push(glyph("Z", 236.75, 46.5));
        g.extend(run("11", 247.78, 46.5, 5.25));
        // The border between the name's cell and the date's.
        let border = crate::rule::Rule {
            bbox: BBox {
                x0: 245.0,
                y0: 36.0,
                x1: 245.75,
                y1: 50.0,
            },
            orientation: crate::rule::Orientation::Vertical,
            page: 0,
        };
        synthesize_missing_boxes(&mut g, std::slice::from_ref(&border));
        // Its own width: what it measures before other digits.
        assert!(
            (g[five].advance.unwrap() - 5.25).abs() < 0.01,
            "{:?}",
            g[five].advance
        );
        assert!(g[five].bbox.unwrap().x1 < 475.0);
        // No measures of its own: across the drawn border, it takes the
        // size's typical width.
        assert!(g[z].advance.unwrap() < 6.0, "{:?}", g[z].advance);
        // `W` measures wider than a digit, within a glyph's width: kept.
        let w = five + 1;
        assert!((g[w].advance.unwrap() - 5.5).abs() < 0.01);

        // An OCR'd page's invisible text: one letter's measures scattered,
        // as the scan's letters stood. Its widest is no gap.
        let mut g = vec![
            glyph("A", 10.0, 50.0),
            glyph("A", 15.0, 50.0),
            glyph("A", 22.0, 50.0),
            glyph("A", 31.0, 50.0),
            glyph("x", 40.5, 50.0),
        ];
        synthesize_missing_boxes(&mut g, &[]);
        assert!(
            (g[3].advance.unwrap() - 9.5).abs() < 0.01,
            "{:?}",
            g[3].advance
        );
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
            fills: Vec::new(),
            dots: Vec::new(),
            traces: Vec::new(),
            ticks: Vec::new(),
            width: 600.0,
            height: 800.0,
            rotation: 0,
            ocr: None,
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
            hide: Hiding::default(),
            painted: Vec::new(),
            covers: Vec::new(),
            masked: false,
            blended: false,
            groups: Vec::new(),
            ink: Vec::new(),
            letters: crate::ligature::Letters::default(),
            unread: Vec::new(),
            marks: crate::marked::Marks::default(),
            open: Vec::new(),
            symbol_fonts: std::collections::HashMap::new(),
            symbols: Vec::new(),
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
            hide: Hiding::default(),
            painted: Vec::new(),
            covers: Vec::new(),
            masked: false,
            blended: false,
            groups: Vec::new(),
            ink: Vec::new(),
            letters: crate::ligature::Letters::default(),
            unread: Vec::new(),
            marks: crate::marked::Marks::default(),
            open: Vec::new(),
            symbol_fonts: std::collections::HashMap::new(),
            symbols: Vec::new(),
            cased: Vec::new(),
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

    /// A collector for a letter-size page with nothing drawn on it yet.
    fn collector() -> Collector<'static> {
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
            hide: Hiding::default(),
            painted: Vec::new(),
            covers: Vec::new(),
            masked: false,
            blended: false,
            groups: Vec::new(),
            ink: Vec::new(),
            letters: crate::ligature::Letters::default(),
            unread: Vec::new(),
            marks: crate::marked::Marks::default(),
            open: Vec::new(),
            symbol_fonts: std::collections::HashMap::new(),
            symbols: Vec::new(),
            cased: Vec::new(),
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
        let mut c = collector();
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
        let mut c = collector();
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
        assert!(
            probed.pages[0].images[0].texty,
            "stripes read as text structure"
        );
        let mut options = ExtractOptions::default();
        options.probe_images = false;
        let unprobed = extract_bytes_with(striped_page(), &options).expect("pdf");
        assert_eq!(unprobed.pages[0].images.len(), 1);
        assert!(!unprobed.pages[0].images[0].texty);
    }

    #[test]
    fn an_image_too_small_to_route_is_probed_only_for_an_engine() {
        // 20 units square on a 100-unit page: a twenty-fifth of it, under
        // the tenth a model is sent.
        let small = || striped_page_drawn(20);
        let doc = extract_bytes(small()).expect("pdf");
        assert!(!doc.pages[0].images[0].texty, "no model reads it");
        let mut options = ExtractOptions::default();
        options.probe_small_images = true;
        let doc = extract_bytes_with(small(), &options).expect("pdf");
        assert!(doc.pages[0].images[0].texty, "an engine reads it");
        // An image a model is sent is probed either way.
        let doc = extract_bytes(striped_page_drawn(40)).expect("pdf");
        assert!(doc.pages[0].images[0].texty);
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
        // Under a clip some of whose text shows, the text outside it goes
        // too: a form's notes run past their box, a hidden copy of a
        // figure's labels under the clip of the copy that shows.
        let text = read(
            "q 0 0 150 200 re W n \
             BT /F1 12 Tf 1 0 0 1 20 100 Tm (Left) Tj ET \
             BT /F1 12 Tf 1 0 0 1 200 100 Tm (Right) Tj ET Q",
        );
        assert!(text.contains("Left") && !text.contains("Right"), "{text:?}");
        // Letters the clip's edge cuts through show their inside part; the
        // ones past it show nowhere.
        let text = read("q 0 0 150 200 re W n BT /F1 12 Tf 1 0 0 1 140 100 Tm (Edge) Tj ET Q");
        assert_eq!(text, "Ed");
    }

    #[test]
    fn a_word_drawn_once_per_cell_reads_once_and_whole() {
        // Each copy cut to its cell: each letter shows in one copy, and the
        // copy outside its cell goes.
        let text = read(
            "q 0 0 150 200 re W n BT /F1 12 Tf 1 0 0 1 120 100 Tm (provisions) Tj ET Q \
             q 150 0 150 200 re W n BT /F1 12 Tf 1 0 0 1 120 100 Tm (provisions) Tj ET Q",
        );
        assert!(
            text.starts_with("provi") && text.ends_with("ons"),
            "{text:?}"
        );
        assert_eq!(text.matches('p').count(), 1, "{text:?}");
        assert_eq!(text.matches('n').count(), 1, "{text:?}");
        // Drawn twice in one place, with neither copy judged to show, it
        // stays: the copies stand for one glyph.
        let text = read(
            "q 0 0 150 200 re W n BT /F1 12 Tf 1 0 0 1 20 100 Tm (One) Tj ET \
             BT /F1 12 Tf 1 0 0 1 200 100 Tm (Twice) Tj ET Q \
             q 0 0 150 200 re W n BT /F1 12 Tf 1 0 0 1 20 60 Tm (Two) Tj ET \
             BT /F1 12 Tf 1 0 0 1 200 100 Tm (Twice) Tj ET Q",
        );
        assert!(text.contains("Twice"), "{text:?}");
    }

    #[test]
    fn clipped_text_where_its_lettering_is_drawn_as_shapes_stays() {
        // A badge's word drawn as a filled outline, a line of type tall, and
        // kept as text under a clip that shows none of it: the text is what
        // the page shows.
        const CLIPPED: &str =
            "q 0 0 100 60 re W n 0 g BT /F1 12 Tf 1 0 0 1 20 20 Tm (Shown) Tj ET \
                               BT /F1 12 Tf 1 0 0 1 20 98 Tm (Badge) Tj ET Q";
        let text = read(&format!("0.5 g 18 96 60 12 re f {CLIPPED}"));
        assert!(text.contains("Badge"), "{text:?}");
        // Under an image, of any size, that may show it: so does a scan.
        let text = read(&format!(
            "{CLIPPED} q 280 0 0 180 10 10 cm BI /W 1 /H 1 /BPC 8 /CS /G /F /AHx ID 80> EI Q"
        ));
        assert!(text.contains("Badge"), "{text:?}");
        // Behind it, a box drawn round a label, twice its type, a panel
        // far taller, or a rule thinner than its letters: nothing there
        // shows it, and it goes.
        for under in [
            "0.5 g 18 90 60 24 re f",
            "0.5 g 10 70 200 120 re f",
            "0 G 0.5 w 18 100 m 80 100 l S",
        ] {
            let text = read(&format!("{under} {CLIPPED}"));
            assert!(
                text.contains("Shown") && !text.contains("Badge"),
                "{under}: {text:?}"
            );
        }
    }

    #[test]
    fn a_clip_of_any_shape_hides_what_lies_outside_its_box() {
        // A triangle in the page's left half: text in the right half lies
        // outside its box, and shows nowhere.
        let text = read(
            "q 0 0 m 150 0 l 0 200 l h W n \
             BT /F1 12 Tf 1 0 0 1 20 20 Tm (Inside) Tj ET \
             BT /F1 12 Tf 1 0 0 1 200 100 Tm (Outside) Tj ET Q",
        );
        assert!(
            text.contains("Inside") && !text.contains("Outside"),
            "{text:?}"
        );
        // Inside its box but past its slope, a word may or may not show
        // there: it stays.
        let text = read(
            "q 0 0 m 150 0 l 0 200 l h W n \
             BT /F1 12 Tf 1 0 0 1 20 20 Tm (Inside) Tj ET \
             BT /F1 12 Tf 1 0 0 1 100 150 Tm (Corner) Tj ET Q",
        );
        assert!(text.contains("Corner"), "{text:?}");
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

#[cfg(test)]
mod cover_tests {
    use super::extract_bytes;

    /// A one-page document, 300 × 200, its font Helvetica as `/F1`; the
    /// `resources`, `catalog` and `page` entries added to those dictionaries,
    /// and `extra` objects numbered from 6.
    fn build(
        content: &str,
        resources: &str,
        catalog: &str,
        page: &str,
        extra: &[String],
    ) -> Vec<u8> {
        let mut objects: Vec<Vec<u8>> = vec![
            format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>").into_bytes(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
                 /Resources << /Font << /F1 4 0 R >> {resources} >> /Contents 5 0 R {page} >>"
            )
            .into_bytes(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            stream("", content).into_bytes(),
        ];
        objects.extend(extra.iter().map(|o| o.clone().into_bytes()));
        let mut pdf = b"%PDF-1.5\n".to_vec();
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

    fn stream(dict: &str, data: &str) -> String {
        format!(
            "<< {dict} /Length {} >>\nstream\n{data}\nendstream",
            data.len()
        )
    }

    fn read_with(
        content: &str,
        resources: &str,
        catalog: &str,
        page: &str,
        extra: &[String],
    ) -> String {
        let doc = extract_bytes(build(content, resources, catalog, page, extra)).expect("pdf");
        doc.pages[0]
            .glyphs
            .iter()
            .map(|g| g.text.as_str())
            .collect()
    }

    fn read(content: &str) -> String {
        read_with(content, "", "", "", &[])
    }

    /// Words set at y 100, a band filled over them, and a line below it.
    const UNDER: &str = "BT /F1 12 Tf 1 0 0 1 20 100 Tm (Hidden words) Tj ET";
    const BAND: &str = "0 0 1 rg 10 90 200 30 re f";
    const SHOWN: &str = "BT /F1 12 Tf 1 0 0 1 20 40 Tm (Shown) Tj ET";

    #[test]
    fn text_painted_over_by_an_opaque_fill_is_not_read() {
        // The words and the space between them go; the line below stays.
        assert_eq!(read(&format!("{UNDER} {BAND} {SHOWN}")), "Shown");
        // Set on the band, after it, they show: in black, not in the
        // band's own blue, the colour the band left set.
        let text = read(&format!("{BAND} 0 g {UNDER} {SHOWN}"));
        assert!(text.contains("Hidden words"), "{text:?}");
        // A fill is as opaque as its own alpha, `ca`: the stroke's, `CA`,
        // is no part of it.
        let text = read_with(
            &format!("{UNDER} /G1 gs {BAND} {SHOWN}"),
            "/ExtGState << /G1 << /CA 0.5 >> >>",
            "",
            "",
            &[],
        );
        assert_eq!(text, "Shown");
    }

    #[test]
    fn a_fill_that_lets_the_text_show_hides_nothing() {
        // Half opacity, and a highlighter's blend modes, which darken what
        // is under them and leave it to be read.
        for gs in ["<< /ca 0.5 >>", "<< /BM /Multiply >>", "<< /BM /Darken >>"] {
            let text = read_with(
                &format!("{UNDER} /G1 gs {BAND} {SHOWN}"),
                &format!("/ExtGState << /G1 {gs} >>"),
                "",
                "",
                &[],
            );
            assert!(text.contains("Hidden words"), "{gs}: {text:?}");
        }
        // Painted inside a transparency group drawn at half opacity; the
        // same group drawn opaque hides them.
        let group = stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 300 200] /Group << /S /Transparency >>",
            BAND,
        );
        let half = read_with(
            &format!("{UNDER} q /G1 gs /X Do Q {SHOWN}"),
            "/XObject << /X 6 0 R >> /ExtGState << /G1 << /ca 0.5 >> >>",
            "",
            "",
            std::slice::from_ref(&group),
        );
        assert!(half.contains("Hidden words"), "{half:?}");
        let whole = read_with(
            &format!("{UNDER} q /X Do Q {SHOWN}"),
            "/XObject << /X 6 0 R >>",
            "",
            "",
            &[group],
        );
        assert_eq!(whole, "Shown");
        // Painted through a soft mask, which lets a share of what is under
        // it through.
        let mask = stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 300 200] /Group << /S /Transparency /CS /DeviceGray >>",
            "0.5 g 0 0 300 200 re f",
        );
        let text = read_with(
            &format!("{UNDER} q /G1 gs {BAND} Q {SHOWN}"),
            "/ExtGState << /G1 << /SMask << /Type /Mask /S /Luminosity /G 6 0 R >> >> >>",
            "",
            "",
            &[mask],
        );
        assert!(text.contains("Hidden words"), "{text:?}");
        // A picture is no cover: an OCR'd scan's words lie under its image.
        let text = read(&format!(
            "{UNDER} q 200 0 0 30 10 90 cm BI /W 1 /H 1 /CS /RGB /BPC 8 /F /AHx ID 0000FF> EI Q {SHOWN}"
        ));
        assert!(text.contains("Hidden words"), "{text:?}");
    }

    #[test]
    fn a_fill_in_optional_content_switched_off_hides_nothing() {
        let layer = "<< /Type /OCG /Name (Band) >>".to_string();
        let content = format!("{UNDER} /OC /L1 BDC {BAND} EMC {SHOWN}");
        let off = read_with(
            &content,
            "/Properties << /L1 6 0 R >>",
            "/OCProperties << /OCGs [6 0 R] /D << /OFF [6 0 R] >> >>",
            "",
            std::slice::from_ref(&layer),
        );
        assert!(off.contains("Hidden words"), "{off:?}");
        let on = read_with(
            &content,
            "/Properties << /L1 6 0 R >>",
            "/OCProperties << /OCGs [6 0 R] /D << /ON [6 0 R] >> >>",
            "",
            &[layer],
        );
        assert_eq!(on, "Shown");
    }

    #[test]
    fn invisible_text_under_a_fill_is_still_read() {
        // An OCR'd scan's words, set invisible.
        let text = read(&format!(
            "BT 3 Tr /F1 12 Tf 1 0 0 1 20 100 Tm (Hidden words) Tj ET {BAND} {SHOWN}"
        ));
        assert!(text.contains("Hidden words"), "{text:?}");
    }

    #[test]
    fn a_fill_whose_edge_cuts_a_glyph_hides_nothing() {
        // The band ends inside the `e`: the letter shows in part, and its
        // word with it, so the band takes none of the line.
        let text = read(&format!("{UNDER} 0 0 1 rg 10 90 40 30 re f {SHOWN}"));
        assert!(text.contains("Hidden words"), "{text:?}");
    }

    #[test]
    fn a_bar_thinner_than_the_type_hides_nothing() {
        // A link's underline, drawn after its URL, over the underscore it
        // lies on: the underscore still reads.
        let text = read(&format!(
            "BT /F1 12 Tf 1 0 0 1 20 100 Tm (a_b) Tj ET 0 0 1 rg 15 98 40 1.4 re f {SHOWN}"
        ));
        assert!(text.contains("a_b"), "{text:?}");
    }

    #[test]
    fn an_annotations_appearance_is_no_cover() {
        // A square annotation's appearance, an opaque box, drawn over the
        // words after the page's own content.
        let annot = "<< /Type /Annot /Subtype /Square /Rect [10 90 210 120] /AP << /N 7 0 R >> >>"
            .to_string();
        let look = stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 200 30]",
            "0 0 1 rg 0 0 200 30 re f",
        );
        let text = read_with(
            &format!("{UNDER} {SHOWN}"),
            "",
            "",
            "/Annots [6 0 R]",
            &[annot, look],
        );
        assert!(text.contains("Hidden words"), "{text:?}");
    }

    #[test]
    fn a_cover_hides_what_the_clip_lets_show_of_a_glyph() {
        // The words cut by a clip at x 40, the part that shows covered: the
        // letter the clip's edge crosses shows nothing past the cover, and
        // goes with the rest.
        let text = read(
            "q 0 0 40 200 re W n BT /F1 12 Tf 1 0 0 1 20 100 Tm (Hidden words) Tj ET Q \
             1 1 1 rg 10 90 32 30 re f 0 g {SHOWN}"
                .replace("{SHOWN}", SHOWN)
                .as_str(),
        );
        assert_eq!(text.trim(), "Shown");
        // Not clipped, the cover's edge cuts through what shows: it all
        // stays.
        let text = read(&format!(
            "BT /F1 12 Tf 1 0 0 1 20 100 Tm (Hidden words) Tj ET 1 1 1 rg 10 90 32 30 re f 0 g {SHOWN}"
        ));
        assert!(text.contains("Hidden words"), "{text:?}");
    }

    #[test]
    fn a_box_about_the_type_size_hides_a_dash_under_it() {
        // A box 9pt tall over 10pt type: the dash and the letters, all
        // their ink under it, go.
        let text = read(&format!(
            "BT /F1 10 Tf 1 0 0 1 20 100 Tm (-Con.) Tj ET 1 1 1 rg 15 99.5 40 9 re f 0 g {SHOWN}"
        ));
        assert_eq!(text, "Shown");
        // A bar a point thick over a run of underscores is the line they
        // draw: they stay.
        let text = read(&format!(
            "BT /F1 10 Tf 1 0 0 1 20 100 Tm (____) Tj ET 0 0 1 rg 15 97.5 40 1.5 re f {SHOWN}"
        ));
        assert!(text.contains("____"), "{text:?}");
    }

    #[test]
    fn text_in_the_colour_of_the_fill_it_is_set_on_is_not_read() {
        // White on a white box: gone. Black on it, and white off it, stay.
        let text = read(
            "1 1 1 rg 10 90 200 30 re f \
             BT /F1 12 Tf 1 0 0 1 20 100 Tm (Unseen) Tj ET \
             0 g BT /F1 12 Tf 1 0 0 1 20 40 Tm (Seen) Tj ET",
        );
        assert_eq!(text, "Seen");
        // The same colour stated in another space: gray 0.5 on an RGB fill
        // of 0.5, 0.5, 0.5.
        let text = read(
            "0.5 0.5 0.5 rg 10 90 200 30 re f \
             0.5 g BT /F1 12 Tf 1 0 0 1 20 100 Tm (Unseen) Tj ET",
        );
        assert_eq!(text, "");
        // A slightly different tint shows: a reader can make it out.
        let text = read(
            "0.5 0.5 0.5 rg 10 90 200 30 re f \
             0.56 0.56 0.56 rg BT /F1 12 Tf 1 0 0 1 20 100 Tm (Tinted) Tj ET",
        );
        assert!(text.contains("Tinted"), "{text:?}");
    }

    #[test]
    fn a_links_underline_under_its_underscores_is_no_ground() {
        // A link's underline, a bar a point tall in the link's blue, drawn
        // before the URL set over it: the URL's underscores lie wholly on
        // the bar, in its colour, and stay. The bar is a rule, no ground.
        let url =
            "0 0 1 rg 15 98.4 80 1 re f BT /F1 11 Tf 1 0 0 1 20 100 Tm (cms_ia/alert_64) Tj ET";
        assert_eq!(read(url), "cms_ia/alert_64");
        // A box of the same blue, a line of type tall, hides them all.
        let boxed =
            "0 0 1 rg 10 94 120 16 re f BT /F1 11 Tf 1 0 0 1 20 100 Tm (cms_ia/alert_64) Tj ET";
        assert_eq!(read(boxed), "");
    }

    #[test]
    fn text_on_its_colour_with_something_between_shows() {
        // White text on a dark band laid over a white page: the band is
        // the topmost thing under it.
        let text = read(
            "1 1 1 rg 0 0 300 200 re f 0 0 0.5 rg 10 90 200 30 re f \
             1 1 1 rg BT /F1 12 Tf 1 0 0 1 20 100 Tm (Banner) Tj ET",
        );
        assert!(text.contains("Banner"), "{text:?}");
        // A scanned page's words, set black on a black ground with the
        // scan painted over them: the scan shows them, and they stay.
        // Without the scan they show nowhere.
        const SCAN: &str = "q 280 0 0 180 10 10 cm BI /W 1 /H 1 /BPC 8 /CS /G /F /AHx ID 80> EI Q";
        const WORDS: &str = "0 g 0 0 300 200 re f BT /F1 12 Tf 1 0 0 1 20 100 Tm (Scanned) Tj ET";
        assert_eq!(read(&format!("{WORDS} {SCAN}")), "Scanned");
        assert_eq!(read(WORDS), "");
        // So does a glyph: a warning sign's white mark set on a black box,
        // and the black `!` set on the mark.
        let text = read(
            "0 g 10 90 200 30 re f 1 g BT /F1 24 Tf 1 0 0 1 20 95 Tm (M) Tj ET \
             0 g BT /F1 12 Tf 1 0 0 1 24 100 Tm (!) Tj ET",
        );
        assert_eq!(text, "M!");
        // A dark shape of any outline does the same.
        let text = read(
            "1 1 1 rg 0 0 300 200 re f 0 0 0.5 rg 10 90 m 210 90 l 110 130 l h f \
             1 1 1 rg BT /F1 12 Tf 1 0 0 1 60 100 Tm (Peak) Tj ET",
        );
        assert!(text.contains("Peak"), "{text:?}");
        // Stroked (1 Tr), or filled and stroked (2 Tr, each glyph drawn
        // twice), its outline shows.
        for (mode, read_as) in [("1", "Outlined"), ("2", "OOuuttlliinneedd")] {
            let text = read(&format!(
                "1 1 1 rg 1 1 1 RG 10 90 200 30 re f 0 0 0 RG \
                 BT {mode} Tr /F1 12 Tf 1 0 0 1 20 100 Tm (Outlined) Tj ET"
            ));
            assert_eq!(text, read_as, "{mode}");
        }
        // Set invisible (3 Tr), an OCR layer, it is the document's text.
        let text =
            read("1 1 1 rg 10 90 200 30 re f BT 3 Tr /F1 12 Tf 1 0 0 1 20 100 Tm (Layer) Tj ET");
        assert!(text.contains("Layer"), "{text:?}");
    }
}
