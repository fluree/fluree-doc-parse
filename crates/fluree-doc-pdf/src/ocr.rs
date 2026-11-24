//! OCR words as glyphs.
//!
//! The router knows where a page's text is pixels — a scan, a near-blank
//! page that is one image, a screenshot pasted into a text page — but the
//! only reader those routes led to was a model, which hands back finished
//! prose without a box per word. An OCR engine hands back the opposite:
//! words, each with its box, and no structure. Turned into glyphs, those
//! words go through the same pipeline a born-digital page does — lines,
//! columns, unruled tables, headings — and every character keeps a real
//! position, so occurrence geometry and highlights work on a scan too.
//!
//! The words come from any engine. This module takes them as data
//! ([`WordReading`], which also reads the `--ocr-words` sidecar format) and
//! decides which of them the page may take:
//!
//! - **Only where the router sends a reader.** A whole-page reading is taken
//!   for a page routed `Scanned`, `NearBlank` or `BrokenText`; on a page with
//!   routed regions, a whole-page reading gives the words inside them, and a
//!   region reading those inside its region. Words for any other page
//!   or crop are dropped: the born-digital text layer stays authoritative,
//!   and a page asked for on a doubt about its structure is not a page whose
//!   text is missing.
//! - **Never over the text layer.** A word whose box holds the center of a
//!   glyph the page already has is dropped, so a stamped header over a scan
//!   is not read twice. A `BrokenText` page is the exception: its glyphs are
//!   the thing that is wrong, so a whole-page reading replaces them.
//! - **Inside the region it was read for.** A region crop carries a margin of
//!   the page around it; a word whose center lies outside the region belongs
//!   to that margin and is dropped.
//!
//! Injection runs on the document as extracted, before analysis, so the
//! glyphs are counted, deduplicated and assembled like any others — and a
//! region they now fill no longer routes to a model.

use crate::extract::{Document, Page};
use crate::geom::BBox;
use crate::glyph::Glyph;
use crate::route::{self, Route, CROP_MARGIN};

/// Height of a detector box, in ems of the text it holds.
///
/// Detection boxes are the text line's extent plus a dilation (DB-style
/// detectors unclip their shrunk masks by 1.4-1.6), so a box runs from above
/// the ascenders to below the descenders and some, whatever its letters.
const BOX_HEIGHT_EM: f64 = 1.3;

/// Where a letter's ink reaches, in ems from the baseline: capitals, digits
/// and ascenders up to about 0.72, the x-height 0.5, descenders down 0.22.
/// An engine that boxes words by their ink (Tesseract, hOCR, ALTO) gives a
/// box exactly that tall, so `our` is half the height of `Payments`, and only
/// the letters inside say what size the text is.
const CAP_HEIGHT_EM: f64 = 0.72;
const X_HEIGHT_EM: f64 = 0.5;
const DESCENDER_EM: f64 = 0.22;
/// A CJK character fills most of its em square.
const CJK_HEIGHT_EM: f64 = 0.88;

/// Widest gap between two boxes of one line run, in box heights. Word spaces
/// run to about half a box; a column gutter or the gap between two table
/// cells is wider, and joining across it would read two columns as one line.
const MAX_WORD_GAP: f64 = 0.9;

/// How many times taller than wide a box of several characters must be to
/// hold upright text.
const VERTICAL_ASPECT: f64 = 1.5;

/// Most height bands one word is filed under when looking for neighbours.
const MAX_BANDS: f64 = 64.0;

/// Where the baseline sits in a detector box, as a fraction of its height
/// from the top: the descender band below it is about a fifth of the box.
const BASELINE_FROM_TOP: f64 = 0.78;

/// Angle, in degrees, below which a word belongs to a level line.
///
/// Line assembly groups glyphs within the nearest 15° of rotation, so
/// anything under half that is a level line that happens to be skewed —
/// a sheet fed off square — and is set level after the reading's skew is
/// taken out ([`skew`], [`runs`]). Above it, the word runs its own way
/// (a sideways label, a stamp) and is laid along its angle.
const LEVEL_BELOW_DEG: f64 = 7.5;

/// Most two runs of one visual line may overlap, in the smaller run's size.
/// An engine that splits a long line boxes its halves with a few pixels in
/// common (`shown on` / `this form`); laid where they are boxed, their
/// characters interleave (`otnhis`).
const MAX_RUN_OVERLAP_EM: f64 = 1.0;

/// The gap left between two runs of one line pushed apart, in the run's size:
/// a word space, well short of a column gutter.
const PUSHED_GAP_EM: f64 = 0.5;

/// Closest two stacked lines' baselines may sit, in the larger size. Line
/// assembly reads a smaller line within 0.7 of a size of a larger one as its
/// superscript or subscript (`SCRIPT_SHIFT` in `line.rs`) and interleaves
/// their characters, so where a tightly set form stacks a label on its value
/// both are set no larger than their spacing allows.
const STACKED_PITCH_EM: f64 = 0.75;

/// Largest ratio of two sizes at which line assembly can still read the
/// smaller as the larger's superscript (`SCRIPT_MIN_RATIO` 0.45 in
/// `line.rs`): within it both runs give way to their spacing.
const SCRIPT_SIZE_SPAN: f64 = 2.25;

/// Line assembly reads two glyphs as one baseline within 0.3 of the larger
/// size (`BASELINE_TOLERANCE` in `line.rs`); with a margin, a large run kept
/// this far from a smaller one's baseline, in its own size, stays its own
/// line.
const MERGED_WITHIN_EM: f64 = 0.35;

/// The word format, shared with every reader of pixels. In
/// [`Coords::Px`] a reading of a page's `full` crop covers the whole page,
/// and one of `r{i}` covers routed region *i* plus [`CROP_MARGIN`] around
/// it, clipped to the page: the crops `fdoc dev render-routed` writes.
pub use fluree_doc_vision::{Coords, OcrWord, WordReading};

/// Which crop of a page a reading is of, by the tag of its crop name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Crop {
    /// `full`: the whole page.
    Page,
    /// `r{i}`: the page's routed region *i*.
    Region(usize),
}

impl Crop {
    /// The crop a tag names (`full`, `r0`, …); `None` for any other tag,
    /// such as a table crop, which an OCR reading has no use for.
    pub fn from_tag(tag: &str) -> Option<Crop> {
        if tag == "full" {
            return Some(Crop::Page);
        }
        tag.strip_prefix('r')?.parse().ok().map(Crop::Region)
    }

    pub fn tag(&self) -> String {
        match self {
            Crop::Page => "full".into(),
            Crop::Region(i) => format!("r{i}"),
        }
    }
}

/// What became of one reading.
#[derive(Debug, Clone)]
pub struct Injected {
    pub page: usize,
    pub crop: Crop,
    /// Why the whole reading was set aside, when it was: the page or crop
    /// is not one the router sends a reader, or the reading cannot be placed.
    pub refused: Option<&'static str>,
    /// Words taken, and dropped: over the text layer, outside the crop or
    /// region read, or with no usable box.
    pub taken: usize,
    pub dropped: usize,
    /// Mean confidence of the words taken, where the engine gave one.
    pub mean_conf: Option<f64>,
    /// Skew of the reading's level lines, degrees counter-clockwise, as
    /// estimated and corrected for.
    pub skew_deg: f64,
    /// The box of every character taken, in the display frame (the
    /// output's). What [`mark_provenance`] reads.
    pub chars: Vec<BBox>,
    /// Words taken in each routed region, by index: `(region, words)`.
    pub regions_taken: Vec<(usize, usize)>,
}

impl Injected {
    fn new(page: usize, crop: Crop) -> Self {
        Injected {
            page,
            crop,
            refused: None,
            taken: 0,
            dropped: 0,
            mean_conf: None,
            skew_deg: 0.0,
            chars: Vec::new(),
            regions_taken: Vec::new(),
        }
    }
}

/// Add the words of each reading to its page as glyphs, and mark the page
/// with what was read ([`Page::ocr`]).
///
/// `readings` is `(page index, crop, reading)`. Routing is decided on every
/// page before any word is added, so two readings of one page are judged
/// against the page as extracted, not against each other.
pub fn inject(doc: &mut Document, readings: &[(usize, Crop, WordReading)]) -> Vec<Injected> {
    let routes: Vec<Route> = doc.pages.iter().map(|p| route::decide(p).0).collect();
    // The regions an engine may fill: the routed ones.
    let regions: Vec<Vec<BBox>> = routes
        .iter()
        .map(|r| match r {
            Route::VlmRegions(r) => r.clone(),
            _ => Vec::new(),
        })
        .collect();
    // Each reading judged against the router first: where on its page it may
    // land, or why it may not.
    let judged: Vec<Result<Target, &'static str>> = readings
        .iter()
        .map(|(page, crop, _)| {
            let pi = doc
                .pages
                .iter()
                .position(|p| p.index == *page)
                .ok_or("no such page")?;
            // What the image read shows (`shown`), and where on it words may
            // land (`area`): the whole page where its text is pixels, else
            // its unread regions. A whole-page reading of a page with such
            // regions — the crop a reader gets when the page is also asked
            // for on a doubt — gives only the words inside them.
            let here = &regions[pi];
            let (shown, area, replaces) = match (crop, &routes[pi]) {
                (Crop::Page, Route::Vlm(reason)) => (
                    None,
                    Area::Page,
                    matches!(reason, route::Reason::BrokenText),
                ),
                (Crop::Page, _) if !here.is_empty() => (
                    None,
                    Area::Regions(here.iter().copied().enumerate().collect()),
                    false,
                ),
                (Crop::Page, _) => return Err("page not routed to a reader"),
                (Crop::Region(i), _) if *i < here.len() => {
                    (Some(here[*i]), Area::Regions(vec![(*i, here[*i])]), false)
                }
                (Crop::Region(_), _) => return Err("region not routed to a reader"),
            };
            Ok(Target {
                pi,
                shown,
                area,
                replaces,
            })
        })
        .collect();
    // Whether the engine boxes ink or detected lines is one fact about the
    // document, not about each page: decided once, over every reading the
    // router lets in (one it refuses says nothing about the others). A region
    // reading of a page that also has a whole reading does not vote: it is set
    // aside wherever the whole reading reads its region, and a reading set
    // aside must not size the one taken.
    let read_whole_too: Vec<usize> = readings
        .iter()
        .zip(&judged)
        .filter(|((_, crop, _), t)| *crop == Crop::Page && t.is_ok())
        .map(|((page, _, _), _)| *page)
        .collect();
    let ink = boxes_ink(
        readings
            .iter()
            .zip(&judged)
            .filter(|((page, crop, _), _)| *crop == Crop::Page || !read_whole_too.contains(page))
            .filter_map(|((_, _, reading), target)| {
                let t = target.as_ref().ok()?;
                place(&doc.pages[t.pi], t.shown, reading).ok()
            })
            .map(|placed| {
                let words: Vec<Placed> = placed.into_iter().flatten().collect();
                let sheet = sheet_angle(&words);
                let level: Vec<Placed> = words
                    .into_iter()
                    .filter(|w| normalise_deg(w.angle - sheet).abs() < LEVEL_BELOW_DEG)
                    .collect();
                let tan = skew(&level).to_radians().tan();
                ink_pairs(&level, tan)
            }),
    );
    let mut out: Vec<Option<Injected>> = (0..readings.len()).map(|_| None).collect();
    // A whole-page reading holds what the page's region crops show: where one
    // took words in a region, taking that region's reading too would read it
    // twice. Whole-page readings are judged first, so a region reading is set
    // aside only where a whole reading actually read its region (or, on a page
    // whose text is all pixels, the page).
    //
    // A whole reading that took a stray word or two in a region does not
    // stand for it: the region's own reading, read closer, is set aside only
    // where the whole one took at least half as many words there.
    let mut read_whole: Vec<(usize, Option<crate::extract::OcrRead>)> = Vec::new();
    // Words the whole readings took, by (page, region).
    let mut took: Vec<((usize, usize), usize)> = Vec::new();
    for whole_pass in [true, false] {
        if !whole_pass {
            read_whole = doc.pages.iter().map(|p| (p.index, p.ocr.clone())).collect();
            for r in out.iter().flatten().filter(|r| r.refused.is_none()) {
                for &(i, n) in &r.regions_taken {
                    took.push(((r.page, i), n));
                }
            }
        }
        let whole_took = |page: usize, region: usize| -> usize {
            took.iter()
                .filter(|(k, _)| *k == (page, region))
                .map(|(_, n)| n)
                .sum()
        };
        for (i, ((page, crop, reading), target)) in readings.iter().zip(&judged).enumerate() {
            if (*crop == Crop::Page) != whole_pass {
                continue;
            }
            let t = match target {
                Ok(t) => t,
                Err(why) => {
                    out[i] = Some(refused(*page, *crop, why));
                    continue;
                }
            };
            let superseded = match crop {
                Crop::Region(r) => read_whole.iter().any(|(pi, read)| {
                    let own = reading
                        .words
                        .iter()
                        .filter(|w| !w.text.trim().is_empty())
                        .count();
                    pi == page
                        && read.as_ref().is_some_and(|o| {
                            o.whole || (o.regions.contains(r) && whole_took(*page, *r) * 2 >= own)
                        })
                }),
                Crop::Page => false,
            };
            if superseded {
                out[i] = Some(refused(
                    *page,
                    *crop,
                    "superseded by the page's whole reading",
                ));
                continue;
            }
            out[i] = Some(inject_one(&mut doc.pages[t.pi], *crop, t, ink, reading));
        }
    }

    out.into_iter()
        .map(|r| r.expect("every reading judged"))
        .collect()
}

/// Where the router lets one reading land.
struct Target {
    /// The page's position in the document.
    pi: usize,
    /// What the image read shows: a region, or the whole page.
    shown: Option<BBox>,
    area: Area,
    /// The page's own glyphs are the thing that is wrong (`BrokenText`).
    replaces: bool,
}

/// Lay one reading the router let in on its page.
fn inject_one(p: &mut Page, crop: Crop, t: &Target, ink: bool, reading: &WordReading) -> Injected {
    let page = p.index;
    let mut placed = match place(p, t.shown, reading) {
        Ok(placed) => placed,
        Err(why) => return refused(page, crop, why),
    };
    // A sheet fed upside down, read by an engine that says so. Line assembly
    // reads a line rightwards, up or down, never right to left, so the page
    // is read in the frame where its words run upright, as a page drawn
    // upside down is when it is extracted. Only a page whose text is all
    // pixels can be turned for its reading.
    let turned = upside_down(&placed);
    if turned {
        // A page with text of its own (a fax header, a stamped number) would
        // have that text turned with it, and read backwards.
        let own_text = p.glyphs.iter().any(|g| !g.text.trim().is_empty());
        if !matches!(t.area, Area::Page) || (own_text && !t.replaces) {
            return refused(
                page,
                crop,
                "the reading runs upside down over a page of text",
            );
        }
        crate::extract::turn_page(p, 180);
        let (w, h) = (p.width, p.height);
        for word in placed.iter_mut().flatten() {
            word.quad = word.quad.map(|(x, y)| (w - x, h - y));
            word.angle = normalise_deg(word.angle - 180.0);
        }
    }
    let first_glyph = match &p.ocr {
        Some(o) => o.first_glyph,
        None => p.glyphs.iter().map(|g| g.draw_index + 1).max().unwrap_or(0),
    };
    let offered = placed.len();
    let added_from = p.glyphs.iter().map(|g| g.draw_index + 1).max().unwrap_or(0);
    let (result, regions_read) = add_words(p, crop, &t.area, first_glyph, t.replaces, ink, placed);
    // A whole-page reading most of whose words land nowhere is not a
    // reading of this page: a stray word must not mark a scan as read, or
    // take the place of a text layer.
    // A page turned for a reading it then does not take is turned back.
    let unturn = |p: &mut Page| {
        if turned {
            crate::extract::turn_page(p, 180);
        }
    };
    if matches!(t.area, Area::Page) && result.taken > 0 && result.taken * 2 < offered {
        p.glyphs.retain(|g| g.draw_index < added_from);
        unturn(p);
        return refused(page, crop, "most of the reading could not be placed");
    }
    // A reading that took nothing read nothing: the page stays as it
    // was, routed to whatever else may read it.
    if result.taken == 0 {
        unturn(p);
        return result;
    }
    // A BrokenText page's glyphs are the thing that is wrong: a
    // whole-page reading that took words takes their place.
    if t.replaces {
        p.glyphs.retain(|g| g.draw_index >= first_glyph);
    }
    let mark = p.ocr.get_or_insert_with(|| crate::extract::OcrRead {
        first_glyph,
        ..Default::default()
    });
    mark.whole |= matches!(t.area, Area::Page);
    for i in regions_read {
        if !mark.regions.contains(&i) {
            mark.regions.push(i);
        }
    }
    result
}

/// Where a reading's words may land on its page.
enum Area {
    /// Anywhere: the page's text is pixels.
    Page,
    /// Inside one of these routed regions, by index.
    Regions(Vec<(usize, BBox)>),
}

fn refused(page: usize, crop: Crop, why: &'static str) -> Injected {
    Injected {
        refused: Some(why),
        ..Injected::new(page, crop)
    }
}

/// The crop's rectangle in the display frame, PDF units: what the image
/// read shows.
fn crop_rect(p: &Page, region: Option<BBox>) -> BBox {
    let (w, h) = p.display_size();
    match region {
        None => BBox {
            x0: 0.0,
            y0: 0.0,
            x1: w,
            y1: h,
        },
        Some(r) => {
            let d = p.to_display(r);
            BBox {
                x0: (d.x0 - CROP_MARGIN).max(0.0),
                y0: (d.y0 - CROP_MARGIN).max(0.0),
                x1: (d.x1 + CROP_MARGIN).min(w),
                y1: (d.y1 + CROP_MARGIN).min(h),
            }
        }
    }
}

/// A word mapped onto the page: its text, NFC-normalised, and its corners
/// in the page's reading frame.
struct Placed {
    text: String,
    quad: [(f64, f64); 4],
    /// Baseline direction on the page, degrees in (-180, 180].
    angle: f64,
    conf: Option<f64>,
}

/// Map a reading's words onto the page, or say why the reading cannot be:
/// a pixel reading with no size to scale by, or one whose size is the
/// crop's turned a quarter. Words with an unusable box come back as `None`
/// in their slot, to be counted as dropped.
fn place(
    p: &Page,
    shown: Option<BBox>,
    reading: &WordReading,
) -> Result<Vec<Option<Placed>>, &'static str> {
    use unicode_normalization::UnicodeNormalization;
    let rect = crop_rect(p, shown);
    // Pixels of the image read → display frame. A reading in page units is
    // already there.
    let (sx, sy, ox, oy) = match (reading.coords, reading.width, reading.height) {
        (Coords::Px, Some(w), Some(h)) if w > 0.0 && h > 0.0 => {
            if quarter_turned(w / h, rect.width() / rect.height()) {
                return Err("the reading is sized for the image turned a quarter");
            }
            (rect.width() / w, rect.height() / h, rect.x0, rect.y0)
        }
        (Coords::Px, _, _) => return Err("pixel coordinates without a size"),
        (Coords::Page, _, _) => (1.0, 1.0, 0.0, 0.0),
    };
    let placed = reading
        .words
        .iter()
        .filter(|w| !w.text.trim().is_empty())
        .map(|w| {
            let [x0, y0, x1, y1] = w.bbox;
            let angle = normalise_deg(w.angle.unwrap_or(0.0));
            if !(w.bbox.iter().all(|v| v.is_finite()) && x1 > x0 && y1 > y0 && angle.is_finite()) {
                return None;
            }
            // A box much taller than wide holding several characters is text
            // set upright, whatever angle the engine failed to state: read
            // up the page, or down it for CJK columns.
            let (bw, bh) = ((x1 - x0) * sx, (y1 - y0) * sy);
            let letters = w.text.chars().filter(|c| !c.is_whitespace()).count();
            // Set level, a box this narrow would hold impossibly thin letters.
            let upright = letters >= 3
                && bh > VERTICAL_ASPECT * bw
                && bw < 0.14 * letters as f64 * bh
                && angle.abs() < LEVEL_BELOW_DEG;
            let (bbox, angle) = if upright {
                let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
                let (half_len, half_thick) = ((y1 - y0) / 2.0, (x1 - x0) / 2.0);
                let turn = if w.text.chars().any(is_cjk) {
                    -90.0
                } else {
                    90.0
                };
                (
                    [
                        cx - half_len,
                        cy - half_thick,
                        cx + half_len,
                        cy + half_thick,
                    ],
                    turn,
                )
            } else {
                (w.bbox, angle)
            };
            let quad = word_quad(bbox, angle)
                .map(|(x, y)| p.from_display_point((ox + x * sx, oy + y * sy)));
            // A box no page could hold is not a word. Everything after this
            // measures boxes, and input decides how large they are.
            // A box that overflows to infinity once scaled has no corners.
            if !quad.iter().all(|(x, y)| x.is_finite() && y.is_finite()) {
                return None;
            }
            let (bx, by) = quad_bounds(&quad);
            let reach = 2.0 * p.width.max(p.height);
            if !(bx.1 - bx.0 <= reach && by.1 - by.0 <= reach) {
                return None;
            }
            Some(Placed {
                text: w.text.nfc().collect(),
                quad,
                // The page turned to read upright turns the word's baseline
                // too.
                angle: normalise_deg(angle + f64::from(p.rotation)),
                conf: w.conf.filter(|c| (0.0..=1.0).contains(c)),
            })
        })
        .collect();
    Ok(placed)
}

/// Whether an image of aspect `read` (width over height) is the crop of
/// aspect `crop` turned a quarter rather than the crop itself: an engine
/// that read a photo without applying its EXIF orientation. Stretched onto
/// the crop, its words would land across the page.
fn quarter_turned(read: f64, crop: f64) -> bool {
    let (r, c) = (read.ln(), crop.ln());
    r.is_finite() && c.is_finite() && (r - c).abs() > 1.25f64.ln() && (r + c).abs() < 1.05f64.ln()
}

/// Whether most of a reading's words run upside down on the page.
fn upside_down(placed: &[Option<Placed>]) -> bool {
    let words: Vec<&Placed> = placed.iter().flatten().collect();
    let turned = words
        .iter()
        .filter(|w| normalise_deg(w.angle - 180.0).abs() < 2.0 * LEVEL_BELOW_DEG)
        .count();
    turned * 2 > words.len()
}

/// An angle in degrees, brought into (-180, 180].
fn normalise_deg(a: f64) -> f64 {
    let r = a.rem_euclid(360.0);
    if r > 180.0 {
        r - 360.0
    } else {
        r
    }
}

/// Add a reading's placed words to the page; returns what was taken and
/// the routed regions that took at least one word.
fn add_words(
    p: &mut Page,
    crop: Crop,
    area: &Area,
    first_glyph: usize,
    replaces: bool,
    ink: bool,
    placed: Vec<Option<Placed>>,
) -> (Injected, Vec<usize>) {
    let mut result = Injected::new(p.index, crop);
    let mut regions_read = Vec::new();
    let page = BBox {
        x0: 0.0,
        y0: 0.0,
        x1: p.width,
        y1: p.height,
    };
    // What the page already holds, as the centers it would claim: the text
    // layer (unless the reading replaces it), and the words an earlier
    // reading laid. Two regions that overlap are two crops of the same
    // pixels, and a word in the overlap is read by both.
    let native = Centers::of(
        p.glyphs
            .iter()
            .filter(|g| !g.text.trim().is_empty())
            .filter(|g| g.draw_index >= first_glyph || !replaces)
            .filter_map(|g| g.bbox)
            .map(|b| ((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5)),
    );
    let mut kept = Vec::new();
    for word in placed {
        let Some(word) = word else {
            result.dropped += 1;
            continue;
        };
        let (bx, by) = quad_bounds(&word.quad);
        let (cx, cy) = ((bx.0 + bx.1) * 0.5, (by.0 + by.1) * 0.5);
        let region = match area {
            Area::Page => page.contains(cx, cy).then_some(None),
            Area::Regions(rs) => rs
                .iter()
                .find(|(_, r)| r.contains(cx, cy))
                .map(|(i, _)| Some(*i)),
        };
        let Some(region) = region else {
            result.dropped += 1;
            continue;
        };
        if native.any_in(&word.quad) {
            result.dropped += 1;
            continue;
        }
        if let Some(i) = region {
            if !regions_read.contains(&i) {
                regions_read.push(i);
            }
            match result.regions_taken.iter_mut().find(|(r, _)| *r == i) {
                Some((_, n)) => *n += 1,
                None => result.regions_taken.push((i, 1)),
            }
        }
        kept.push(word);
    }
    let confs: Vec<f64> = kept.iter().filter_map(|w| w.conf).collect();
    result.mean_conf = (!confs.is_empty()).then(|| confs.iter().sum::<f64>() / confs.len() as f64);
    result.taken = kept.len();

    let mut next = p.glyphs.iter().map(|g| g.draw_index + 1).max().unwrap_or(0);
    let sheet = sheet_angle(&kept);
    let (level, turned): (Vec<Placed>, Vec<Placed>) = kept
        .into_iter()
        .partition(|w| normalise_deg(w.angle - sheet).abs() < LEVEL_BELOW_DEG);
    for w in &turned {
        let font = (dist(w.quad[0], w.quad[3]) / BOX_HEIGHT_EM) as f32;
        // Line assembly reads runs along right angles only. A stamp at any
        // other angle is set level about its center, or it comes out in
        // fragments and backwards.
        let sideways = (w.angle.abs() - 90.0).abs() < LEVEL_BELOW_DEG;
        let (quad, rotation) = if sideways {
            (w.quad, w.angle as f32)
        } else {
            (level_quad(&w.quad), 0.0)
        };
        let chars = lay_along(p, &w.text, &quad, rotation, font, &mut next);
        result.chars.extend(chars);
    }
    result.skew_deg = skew(&level);
    let tan = result.skew_deg.to_radians().tan();
    let words: Vec<Level> = level.iter().map(|w| Level::of(w, tan)).collect();
    let mut runs: Vec<Run> = runs(&words)
        .into_iter()
        .map(|r| Run::of(&words, r, ink))
        .collect();
    // The whole reading goes back on the sheet by one offset, taken at its
    // middle. Set back line by line, at each line's own center, two stacked
    // lines of different extents would drift towards each other, and on a
    // tightly set form line assembly then reads them as one line, their
    // characters interleaved.
    let left = runs.iter().map(|r| r.x0).fold(f64::MAX, f64::min);
    let right = runs.iter().map(|r| r.x1).fold(f64::MIN, f64::max);
    let offset = (left + right) * 0.5 * tan;
    let lines = visual_lines(&runs);
    for line in &lines {
        let baseline = median(line.iter().map(|&r| runs[r].baseline).collect()) - offset;
        for &r in line {
            runs[r].baseline = baseline;
        }
    }
    fit_stacked(&mut runs, &lines);
    for mut line in lines {
        line.sort_by(|&a, &b| runs[a].x0.total_cmp(&runs[b].x0));
        let mut end = f64::MIN;
        for r in line {
            let run = &runs[r];
            let pushed = end + PUSHED_GAP_EM * run.font;
            // Squeezed into what is left of its box, the run keeps its words in
            // order after the one it overlapped; too little left, it stays.
            let start = if run.x0 < pushed && run.x1 - pushed >= 0.5 * (run.x1 - run.x0) {
                pushed
            } else {
                run.x0
            };
            let chars = lay_run(p, &words, run, start, &mut next);
            result.chars.extend(chars);
            end = end.max(run.x1);
        }
    }
    let shown: Vec<BBox> = result.chars.iter().map(|b| p.to_display(*b)).collect();
    result.chars = shown;
    (result, regions_read)
}

/// Points filed by grid cell, so a word looks only at the points under it,
/// not at every glyph of the page.
struct Centers {
    cell: f64,
    by_cell: std::collections::HashMap<(i64, i64), Vec<(f64, f64)>>,
    count: usize,
}

impl Centers {
    /// A cell about the size of a line: small enough that a word's cells hold
    /// little more than the glyphs it covers.
    const CELL: f64 = 12.0;

    fn of(points: impl Iterator<Item = (f64, f64)>) -> Self {
        let mut by_cell: std::collections::HashMap<(i64, i64), Vec<(f64, f64)>> =
            Default::default();
        let mut count = 0;
        for (x, y) in points {
            count += 1;
            by_cell
                .entry((
                    (x / Self::CELL).floor() as i64,
                    (y / Self::CELL).floor() as i64,
                ))
                .or_default()
                .push((x, y));
        }
        Centers {
            cell: Self::CELL,
            by_cell,
            count,
        }
    }

    /// Whether any point lies inside the quad.
    fn any_in(&self, quad: &[(f64, f64); 4]) -> bool {
        if self.by_cell.is_empty() {
            return false;
        }
        let ((x0, x1), (y0, y1)) = quad_bounds(quad);
        let cells =
            |a: f64, b: f64| (a / self.cell).floor() as i64..=(b / self.cell).floor() as i64;
        // A word wider than the page's points are many looks at the points.
        let span = |a: f64, b: f64| ((b - a) / self.cell + 2.0).max(1.0);
        if span(x0, x1) * span(y0, y1) > self.count as f64 {
            return self.by_cell.values().flatten().any(|&c| in_quad(quad, c));
        }
        cells(y0, y1).any(|cy| {
            cells(x0, x1).any(|cx| {
                self.by_cell
                    .get(&(cx, cy))
                    .is_some_and(|ps| ps.iter().any(|&c| in_quad(quad, c)))
            })
        })
    }
}

/// A quad set level about its center, at its own length and thickness.
fn level_quad(q: &[(f64, f64); 4]) -> [(f64, f64); 4] {
    let (len, thick) = (dist(q[0], q[1]), dist(q[0], q[3]));
    let cx = q.iter().map(|c| c.0).sum::<f64>() / 4.0;
    let cy = q.iter().map(|c| c.1).sum::<f64>() / 4.0;
    let (l, r) = (cx - len / 2.0, cx + len / 2.0);
    let (t, b) = (cy - thick / 2.0, cy + thick / 2.0);
    [(l, t), (r, t), (r, b), (l, b)]
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Whether a point lies inside a convex quad, either winding.
fn in_quad(q: &[(f64, f64); 4], (x, y): (f64, f64)) -> bool {
    let mut sign = 0.0f64;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let cross = (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
        if cross != 0.0 {
            if sign != 0.0 && cross.signum() != sign {
                return false;
            }
            sign = cross.signum();
        }
    }
    true
}

/// A level word with its skew taken out: set square about its own center,
/// at its own length and thickness — not the box around it turned, which
/// grows with its length.
struct Level<'a> {
    x0: f64,
    x1: f64,
    top: f64,
    bottom: f64,
    word: &'a Placed,
}

impl<'a> Level<'a> {
    /// `tan` is the tangent of the skew: a line rising to the right
    /// (counter-clockwise) has y falling with x, put back level about x = 0.
    fn of(w: &'a Placed, tan: f64) -> Self {
        let [tl, tr, _, bl] = w.quad;
        let (len, thick) = (dist(tl, tr), dist(tl, bl));
        let cx = w.quad.iter().map(|q| q.0).sum::<f64>() / 4.0;
        let cy = w.quad.iter().map(|q| q.1).sum::<f64>() / 4.0 + cx * tan;
        Level {
            x0: cx - len / 2.0,
            x1: cx + len / 2.0,
            top: cy - thick / 2.0,
            bottom: cy + thick / 2.0,
            word: w,
        }
    }

    fn height(&self) -> f64 {
        self.bottom - self.top
    }
}

/// For each word, the nearest word starting to its right on the same line,
/// within `max_gap` box heights: overlapping it vertically by half the
/// smaller height, of a comparable height (a word twice as tall is a
/// heading or a stamp, not the same line), and claimed by no closer word.
///
/// Words are bucketed by height bands, so a page of thousands of words looks
/// only at its neighbours.
fn right_neighbours(words: &[Level], max_gap: f64) -> Vec<Option<usize>> {
    use std::collections::HashMap;
    let mut next = vec![None; words.len()];
    if words.is_empty() {
        return next;
    }
    // At most `MAX_BANDS` bands per word, however the heights are spread: one
    // page-tall box among hair-thin ones must not take a bucket per hair.
    let tallest = words.iter().map(Level::height).fold(0.0, f64::max);
    let band = median(words.iter().map(Level::height).collect())
        .max(tallest / MAX_BANDS)
        .max(f64::EPSILON);
    let mut buckets: HashMap<i64, Vec<usize>> = HashMap::new();
    for (i, w) in words.iter().enumerate() {
        for b in (w.top / band).floor() as i64..=(w.bottom / band).floor() as i64 {
            buckets.entry(b).or_default().push(i);
        }
    }
    // Each band left to right, with its tallest word: a word's right
    // neighbour lies in a window that starts at its own right edge, so a line
    // of thousands of words is searched in a window each, not end to end.
    let buckets: HashMap<i64, (Vec<usize>, f64)> = buckets
        .into_iter()
        .map(|(k, mut v)| {
            v.sort_by(|&a, &b| words[a].x0.total_cmp(&words[b].x0));
            let tallest = v.iter().map(|&i| words[i].height()).fold(0.0, f64::max);
            (k, (v, tallest))
        })
        .collect();
    let marks: Vec<bool> = words
        .iter()
        .map(|w| ink_extent(&w.word.text).is_none())
        .collect();
    // How far right a word's neighbour may start. A word's neighbour is at
    // most twice its height (a word or a mark beside it, see `comparable`),
    // so the gap it allows is at most `max_gap` twice its height; only a
    // mark, which has no size of its own, takes the scale of a word of any
    // height beside it, up to the tallest in its band. One tall box among a
    // line's words then widens the windows of marks alone.
    let reach = |i: usize, tallest: f64| {
        let h = words[i].height();
        max_gap * if marks[i] { h.max(tallest) } else { 2.0 * h }
    };
    let mut claimed: HashMap<usize, (usize, f64)> = HashMap::new();
    for (i, a) in words.iter().enumerate() {
        let h = a.height();
        let mut best: Option<(usize, f64)> = None;
        let lo = (a.top / band).floor() as i64;
        let hi = (a.bottom / band).floor() as i64;
        let window = (lo..=hi)
            .filter_map(|b| buckets.get(&b))
            .flat_map(|(v, tallest)| {
                let from = v.partition_point(|&j| words[j].x0 < a.x1 - 0.25 * h);
                let till = a.x1 + reach(i, *tallest);
                v[from..].iter().take_while(move |&&j| words[j].x0 <= till)
            });
        for j in window.copied() {
            let b = &words[j];
            let gap = b.x0 - a.x1;
            // Strictly to the right, so two boxes cannot each be the other's
            // neighbour; a tie goes by index.
            let (ca, cb) = (a.x0 + a.x1, b.x0 + b.x1);
            let rightward = cb > ca || (cb == ca && j > i);
            let overlap = a.bottom.min(b.bottom) - a.top.max(b.top);
            // A dash is a sliver: the word beside it sets the scale of the gap.
            let scale = if marks[i] || marks[j] {
                h.max(b.height())
            } else {
                h.min(b.height())
            };
            if j == i
                || !rightward
                || gap < -0.25 * h
                || gap > max_gap * scale
                || overlap <= 0.5 * h.min(b.height())
            {
                continue;
            }
            // A mark of punctuation has no size of its own to compare: a
            // dash is a sliver, `&` a capital. It belongs to the line its
            // middle sits in.
            let comparable = if marks[i] || marks[j] {
                let (small, big) = if h < b.height() { (a, b) } else { (b, a) };
                let mid = (small.top + small.bottom) * 0.5;
                // A rule read as `|` across several rows belongs to none.
                let (mark, other) = if marks[i] { (a, b) } else { (b, a) };
                big.top <= mid && mid <= big.bottom && mark.height() <= 2.0 * other.height()
            } else {
                (0.5..=2.0).contains(&(h / b.height().max(f64::EPSILON)))
            };
            if !comparable {
                continue;
            }
            if best.is_none_or(|(_, g)| gap < g) {
                best = Some((j, gap));
            }
        }
        if let Some((j, gap)) = best {
            // A word has one left neighbour: the closest that points at it.
            match claimed.get(&j) {
                Some(&(_, g)) if g <= gap => {}
                previous => {
                    if let Some(&(k, _)) = previous {
                        next[k] = None;
                    }
                    next[i] = Some(j);
                    claimed.insert(j, (i, gap));
                }
            }
        }
    }
    next
}

/// The skew of a reading's level words, degrees counter-clockwise.
///
/// A scan is rarely square to its page, and every line on it shares the
/// sheet's angle. An engine that boxes lines states it (`angle`); one that
/// boxes words usually does not, but each word sits beside the next on its
/// line, so the slope between neighbours says the same thing. The median of
/// either is the reading's skew; too few to judge is none.
///
/// An engine that boxes whole lines square, without an angle, gives neither:
/// such a reading on a skewed sheet is read as level, its lines one run each.
fn skew(words: &[Placed]) -> f64 {
    let stated: Vec<f64> = words
        .iter()
        .map(|w| w.angle)
        .filter(|a| *a != 0.0)
        .collect();
    if stated.len() * 2 >= words.len() && !stated.is_empty() {
        return median(stated);
    }
    let level: Vec<Level> = words.iter().map(|w| Level::of(w, 0.0)).collect();
    let mut slopes = Vec::new();
    for (i, j) in right_neighbours(&level, 3.0).into_iter().enumerate() {
        let Some(j) = j else { continue };
        let (a, b) = (&level[i], &level[j]);
        let dx = (b.x0 + b.x1 - a.x0 - a.x1) * 0.5;
        let dy = (b.top + b.bottom - a.top - a.bottom) * 0.5;
        if dx > 0.0 {
            slopes.push((-dy).atan2(dx).to_degrees());
        }
    }
    if slopes.len() < 3 {
        return 0.0;
    }
    let s = median(slopes);
    if s.abs() < LEVEL_BELOW_DEG {
        s
    } else {
        0.0
    }
}

/// The angle most of a reading's words share: the sheet's, from which a word
/// is judged level or turned. A photographed page varies by a degree or two
/// from word to word, so a fixed limit would turn the odd word of a line.
fn sheet_angle(words: &[Placed]) -> f64 {
    let near: Vec<f64> = words
        .iter()
        .map(|w| w.angle)
        .filter(|a| a.abs() < 2.0 * LEVEL_BELOW_DEG)
        .collect();
    if near.is_empty() {
        0.0
    } else {
        median(near)
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// How far a word's ink reaches above and below its baseline, in ems, from
/// the letters it holds; `None` for a word of punctuation alone.
fn ink_extent(text: &str) -> Option<(f64, f64)> {
    let mut up: Option<f64> = None;
    let mut down: f64 = 0.0;
    for c in text.chars() {
        let reach = if is_cjk(c) {
            CJK_HEIGHT_EM
        } else if c.is_uppercase() || c.is_ascii_digit() || "bdfhklt&$%#@".contains(c) {
            CAP_HEIGHT_EM
        } else if c.is_alphabetic() {
            X_HEIGHT_EM
        } else {
            continue;
        };
        up = Some(up.map_or(reach, |u: f64| u.max(reach)));
        if "gjpqy".contains(c) {
            down = DESCENDER_EM;
        }
    }
    up.map(|u| (u, down))
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3000..=0x30FF | 0xFF00..=0xFFEF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF)
}

/// Neighbouring words of one reading, as pairs of log height ratios: as
/// boxed, and once each height is divided by what its letters reach
/// ([`ink_extent`]).
fn ink_pairs(words: &[Placed], tan: f64) -> Vec<(f64, f64)> {
    let level: Vec<Level> = words.iter().map(|w| Level::of(w, tan)).collect();
    right_neighbours(&level, 3.0)
        .into_iter()
        .enumerate()
        .filter_map(|(i, j)| {
            let (a, b) = (&level[i], &level[j?]);
            let (ua, da) = ink_extent(&a.word.text)?;
            let (ub, db) = ink_extent(&b.word.text)?;
            let raw = (a.height() / b.height()).ln();
            Some((raw.abs(), (raw - ((ua + da) / (ub + db)).ln()).abs()))
        })
        .collect()
}

/// Whether a document's readings box ink rather than detected lines.
///
/// Side by side on one line, two ink boxes differ in height as their
/// letters do — `our` beside `Payments` — and dividing each by what its
/// letters reach evens them out. Two detector boxes are as tall whatever
/// their letters, and the division only adds a spread. Judged on
/// neighbours, so the sizes of different lines do not drown the signal, and
/// over the whole document, so its pages are not sized two ways.
fn boxes_ink(readings: impl Iterator<Item = Vec<(f64, f64)>>) -> bool {
    let pairs: Vec<(f64, f64)> = readings.flatten().collect();
    if pairs.len() < 5 {
        return false;
    }
    let n = pairs.len() as f64;
    let raw = pairs.iter().map(|p| p.0).sum::<f64>() / n;
    let even = pairs.iter().map(|p| p.1).sum::<f64>() / n;
    raw > 0.08 && even < 0.6 * raw
}

/// The runs of a reading: words chained to their right neighbours, each run
/// a stretch of one line between gaps no wider than a word space. A run
/// never crosses a column gutter or a table cell's gap, never holds two
/// words that overlap, and never takes in a word of another size.
fn runs(words: &[Level]) -> Vec<Vec<usize>> {
    let next = right_neighbours(words, MAX_WORD_GAP);
    let mut has_left = vec![false; words.len()];
    for j in next.iter().flatten() {
        has_left[*j] = true;
    }
    let mut out = Vec::new();
    for start in (0..words.len()).filter(|i| !has_left[*i]) {
        // A chain cannot loop: each link is strictly rightward
        // (`right_neighbours`), and each word has one left neighbour.
        let mut run = vec![start];
        let mut at = start;
        while let Some(j) = next[at] {
            run.push(j);
            at = j;
        }
        out.push(run);
    }
    // Every word is laid: a word the chains missed is a run of its own.
    let mut laid = vec![false; words.len()];
    for i in out.iter().flatten() {
        laid[*i] = true;
    }
    out.extend((0..words.len()).filter(|i| !laid[*i]).map(|i| vec![i]));
    out
}

/// A run's size and baseline, deskewed: the median of what each of its
/// words says. A detector box is [`BOX_HEIGHT_EM`] tall with its baseline
/// [`BASELINE_FROM_TOP`] down; an ink box reaches as far as its letters do,
/// so a word with no ascenders still sits at the size of its line.
struct Run {
    words: Vec<usize>,
    font: f64,
    baseline: f64,
    x0: f64,
    x1: f64,
}

impl Run {
    fn of(words: &[Level], run: Vec<usize>, ink: bool) -> Self {
        let mut fonts = Vec::new();
        let mut baselines = Vec::new();
        // Punctuation does not vote: a dash's box says nothing of the size.
        let voters: Vec<usize> = match run
            .iter()
            .copied()
            .filter(|&i| ink_extent(&words[i].word.text).is_some())
            .collect::<Vec<_>>()
        {
            v if v.is_empty() => run.clone(),
            v => v,
        };
        for &i in &voters {
            let w = &words[i];
            let h = w.height();
            match (ink, ink_extent(&w.word.text)) {
                (true, Some((up, down))) => {
                    let font = h / (up + down);
                    fonts.push(font);
                    baselines.push(w.bottom - down * font);
                }
                _ => {
                    fonts.push(h / BOX_HEIGHT_EM);
                    baselines.push(w.top + h * BASELINE_FROM_TOP);
                }
            }
        }
        Run {
            x0: run.iter().map(|&i| words[i].x0).fold(f64::MAX, f64::min),
            x1: run.iter().map(|&i| words[i].x1).fold(f64::MIN, f64::max),
            words: run,
            font: median(fonts),
            baseline: median(baselines),
        }
    }
}

/// Runs that sit on one visual line: the same baseline within a third of
/// the smaller size, sizes within a factor of two, and side by side,
/// overlapping by no more than [`MAX_RUN_OVERLAP_EM`] (they are pushed apart
/// when laid). Compared with the line's first run, not with everything
/// gathered so far, so a line cannot creep down the page.
///
/// A skewed sheet is laid out run by run at each run's own height, so two
/// runs of one line across a wide gap — a label and its value, a header's
/// two halves — would land a point or two apart, and line assembly would
/// read them as two lines, or cut one at a column gutter it then crosses.
/// One baseline per visual line keeps them one line; the gap between them
/// stays, so columns still read apart.
fn visual_lines(runs: &[Run]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..runs.len()).collect();
    order.sort_by(|&a, &b| runs[a].baseline.total_cmp(&runs[b].baseline));
    let mut lines: Vec<(Vec<usize>, Across)> = Vec::new();
    for r in order {
        let run = &runs[r];
        let home = lines.iter_mut().rev().take(8).find(|(line, across)| {
            let seed = &runs[line[0]];
            let small = seed.font.min(run.font);
            (seed.baseline - run.baseline).abs() < small / 3.0
                && (0.5..=2.0).contains(&(seed.font / run.font.max(f64::EPSILON)))
                && across.overlapping(runs, run.x0, run.x1).all(|o| {
                    let other = &runs[o];
                    let overlap = other.x1.min(run.x1) - other.x0.max(run.x0);
                    overlap <= MAX_RUN_OVERLAP_EM * other.font.min(run.font)
                })
        });
        match home {
            Some((line, across)) => {
                line.push(r);
                across.insert(runs, r);
            }
            None => {
                let mut across = Across::default();
                across.insert(runs, r);
                lines.push((vec![r], across));
            }
        }
    }
    lines.into_iter().map(|(line, _)| line).collect()
}

/// Runs indexed by where they start across the page, so the ones a span
/// overlaps are found without walking a long line end to end.
#[derive(Default)]
struct Across {
    /// Run indices, by `x0`.
    by_x0: Vec<usize>,
    /// The longest run's extent.
    longest: f64,
}

impl Across {
    fn insert(&mut self, runs: &[Run], r: usize) {
        let at = self.by_x0.partition_point(|&o| runs[o].x0 < runs[r].x0);
        self.by_x0.insert(at, r);
        self.longest = self.longest.max(runs[r].x1 - runs[r].x0);
    }

    /// The runs overlapping `x0..x1`.
    fn overlapping<'a>(
        &'a self,
        runs: &'a [Run],
        x0: f64,
        x1: f64,
    ) -> impl Iterator<Item = usize> + 'a {
        let from = self
            .by_x0
            .partition_point(|&o| runs[o].x0 < x0 - self.longest);
        self.by_x0[from..]
            .iter()
            .copied()
            .take_while(move |&o| runs[o].x0 < x1)
            .filter(move |&o| runs[o].x1 > x0)
    }
}

/// A run's line box, from its baseline, in its size: the glyphs laid in it
/// ([`lay_along`]) have their origins [`BASELINE_FROM_TOP`] down it, a
/// little below the run's baseline. A space glyph sits on the baseline
/// itself, so it sorts before the next word's first character.
const RUN_TOP_EM: f64 = 0.8;
const RUN_BOTTOM_EM: f64 = 0.25;

/// Where the glyphs of a run of size `font` on `baseline` have their origins.
fn glyph_baseline(baseline: f64, font: f64) -> f64 {
    baseline - RUN_TOP_EM * font + BASELINE_FROM_TOP * (RUN_TOP_EM + RUN_BOTTOM_EM) * font
}

/// Set each run no larger than its stacked neighbours' spacing allows
/// ([`STACKED_PITCH_EM`]): the runs of other visual lines it overlaps
/// horizontally. Two such runs closer than that are a label boxed on its
/// value; their boxes are padded to overlap, so the size they state is more
/// than the text is.
///
/// A stamp — a run too large to be read as the script of a smaller one it
/// passes over, a display line over body text — keeps its size, which says
/// what it is, and is moved instead: the least it must for line assembly to
/// read it apart from every smaller run under it, the smallest stamps
/// first, so a larger one also keeps clear of where they went. Line assembly
/// is left as it reads every PDF; the stamp is placed where it cannot be
/// misread.
///
/// Sorted by baseline, each run looks only as far as the largest size
/// allows, so a page of many lines stays near linear.
fn fit_stacked(runs: &mut [Run], lines: &[Vec<usize>]) {
    // Each visual line, by its baseline (one for all its runs), with its runs
    // indexed across the page.
    let mut by_base: Vec<(f64, usize, Across)> = lines
        .iter()
        .enumerate()
        .map(|(k, line)| {
            let mut across = Across::default();
            for &r in line {
                across.insert(runs, r);
            }
            (runs[line[0]].baseline, k, across)
        })
        .collect();
    by_base.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut line_of = vec![0usize; runs.len()];
    for (k, line) in lines.iter().enumerate() {
        for &r in line {
            line_of[r] = k;
        }
    }
    // The smaller runs of other lines each run passes over, within its reach.
    // A run is confused only with one within its own reach, and each pair is
    // weighed from its larger run's side: so a page-tall word looks at the
    // page once, and a body line only at its neighbours above and below it,
    // not along its own line.
    let near: Vec<Vec<usize>> = (0..runs.len())
        .map(|a| {
            let (ra, fa) = (&runs[a], runs[a].font);
            let reach = fa * STACKED_PITCH_EM;
            let from = by_base.partition_point(|l| l.0 <= ra.baseline - reach);
            // A run thinner than one step of its coordinate has no reach, and
            // `to` would land before its own line: clamp it to `from`.
            let to = by_base
                .partition_point(|l| l.0 < ra.baseline + reach)
                .max(from);
            by_base[from..to]
                .iter()
                .filter(|(_, k, _)| *k != line_of[a])
                .flat_map(|(_, _, across)| across.overlapping(runs, ra.x0, ra.x1))
                .filter(|&b| {
                    let fb = runs[b].font;
                    fb < fa || (fb == fa && b > a)
                })
                .collect()
        })
        .collect();
    let stamp: Vec<bool> = (0..runs.len())
        .map(|a| {
            near[a]
                .iter()
                .any(|&b| runs[a].font > SCRIPT_SIZE_SPAN * runs[b].font)
        })
        .collect();
    // Comparable sizes: the smaller reads as the larger's superscript within
    // `STACKED_PITCH_EM` of it. A stamp keeps its size.
    let mut fit: Vec<f64> = runs.iter().map(|r| r.font).collect();
    for a in 0..runs.len() {
        for &b in &near[a] {
            let (ra, rb) = (&runs[a], &runs[b]);
            let pitch = (ra.baseline - rb.baseline).abs();
            if stamp[a] || stamp[b] || pitch <= 0.0 || ra.font > SCRIPT_SIZE_SPAN * rb.font {
                continue;
            }
            let most = pitch / STACKED_PITCH_EM;
            fit[a] = fit[a].min(most);
            fit[b] = fit[b].min(most);
        }
    }
    for (r, f) in runs.iter_mut().zip(fit) {
        r.font = f;
    }
    // Each stamp, smallest first, kept clear of every smaller run it passes
    // over, wherever on the page: where line assembly will see them, at the
    // glyphs' origins, and at the sizes they are laid at.
    let mut stamps: Vec<usize> = (0..runs.len()).filter(|&a| stamp[a]).collect();
    stamps.sort_by(|&a, &b| runs[a].font.total_cmp(&runs[b].font));
    for a in stamps {
        let (x0, x1, fa) = (runs[a].x0, runs[a].x1, runs[a].font);
        let blockers: Vec<(f64, f64)> = (0..runs.len())
            .filter(|&b| b != a && line_of[b] != line_of[a])
            .filter(|&b| runs[b].x1.min(x1) > runs[b].x0.max(x0))
            .filter(|&b| runs[b].font < fa || (runs[b].font == fa && b < a))
            .map(|b| {
                let rb = &runs[b];
                (glyph_baseline(rb.baseline, rb.font), MERGED_WITHIN_EM * fa)
            })
            .collect();
        let at = runs[a].baseline;
        let glyphs = glyph_baseline(at, fa);
        runs[a].baseline = at + clear_baseline(glyphs, &blockers) - glyphs;
    }
}

/// The baseline nearest `at` that sits at least `d` from each `(b, d)`.
/// Some candidate always qualifies: the farthest blocker's far side.
fn clear_baseline(at: f64, blockers: &[(f64, f64)]) -> f64 {
    let clear = |y: f64| blockers.iter().all(|&(b, d)| (y - b).abs() >= d);
    std::iter::once(at)
        .chain(blockers.iter().flat_map(|&(b, d)| [b - d, b + d]))
        .filter(|&y| clear(y))
        .min_by(|x, y| (x - at).abs().total_cmp(&(y - at).abs()))
        .unwrap_or(at)
}

/// Lay a run's words on its baseline, at its size, from `start`: where it
/// is boxed, or further right when it overlapped the run before it, its words
/// squeezed in proportion. Returns the boxes of the characters with ink.
///
/// On a skewed sheet the lines are set level, so text sits off where it is
/// drawn by its distance from the middle of the reading times the skew's
/// tangent — about two points at the edge of a page at 0.6°. Two boxes in a
/// run are two words however close, so a space glyph stands between them.
fn lay_run(p: &mut Page, words: &[Level], run: &Run, start: f64, next: &mut usize) -> Vec<BBox> {
    let (font, baseline) = (run.font, run.baseline);
    let (top, bottom) = (
        baseline - RUN_TOP_EM * font,
        baseline + RUN_BOTTOM_EM * font,
    );
    let scale = (run.x1 - start) / (run.x1 - run.x0).max(f64::EPSILON);
    let at = |x: f64| start + (x - run.x0) * scale;
    let mut boxes = Vec::new();
    for (k, &i) in run.words.iter().enumerate() {
        let w = &words[i];
        let (x0, x1) = (at(w.x0), at(w.x1));
        let quad = [(x0, top), (x1, top), (x1, bottom), (x0, bottom)];
        boxes.extend(lay_along(p, &w.word.text, &quad, 0.0, font as f32, next));
        let Some(&j) = run.words.get(k + 1) else {
            continue;
        };
        let following = &words[j].word.text;
        let (last, first) = (w.word.text.chars().last(), following.chars().next());
        // Scripts written without spaces (CJK, Thai) are boxed a character
        // at a time by some engines; those boxes are not words.
        let spaceless = |c: Option<char>| {
            c.is_some_and(|c| is_cjk(c) || ('\u{0E00}'..='\u{0E7F}').contains(&c))
        };
        if last.is_some_and(char::is_whitespace)
            || first.is_some_and(char::is_whitespace)
            || (spaceless(last) && spaceless(first))
        {
            continue;
        }
        p.glyphs.push(Glyph {
            text: " ".into(),
            bbox: None,
            page: p.index,
            // At the next word when the two overlap, or the space sorts
            // after that word's first character (`555-01811 1/06`).
            origin: (x1.min(at(words[j].x0)), baseline),
            rotation_deg: 0.0,
            font_size: font as f32,
            advance: Some((at(words[j].x0) - x1).max(0.0)),
            weight: None,
            draw_index: *next,
        });
        *next += 1;
    }
    boxes
}

/// Spread a text's characters evenly along a quad's top edge, one glyph
/// each, with the baseline [`BASELINE_FROM_TOP`] down; returns the boxes of
/// the characters that have ink. `next` is the draw index the first glyph
/// takes, advanced past the last.
fn lay_along(
    p: &mut Page,
    text: &str,
    quad: &[(f64, f64); 4],
    rotation_deg: f32,
    font_size: f32,
    next: &mut usize,
) -> Vec<BBox> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len() as f64;
    let [tl, tr, _, bl] = *quad;
    let along = ((tr.0 - tl.0) / n, (tr.1 - tl.1) / n);
    let advance = (along.0.powi(2) + along.1.powi(2)).sqrt();
    let down = (bl.0 - tl.0, bl.1 - tl.1);
    let mut boxes = Vec::new();
    for (k, ch) in chars.iter().enumerate() {
        let draw_index = *next;
        *next += 1;
        let k = k as f64;
        let at = |t: f64, d: f64| {
            (
                tl.0 + along.0 * t + down.0 * d,
                tl.1 + along.1 * t + down.1 * d,
            )
        };
        let bbox = (!ch.is_whitespace()).then(|| {
            let cell = [at(k, 0.0), at(k + 1.0, 0.0), at(k + 1.0, 1.0), at(k, 1.0)];
            let (bx, by) = quad_bounds(&cell);
            BBox {
                x0: bx.0,
                y0: by.0,
                x1: bx.1,
                y1: by.1,
            }
        });
        boxes.extend(bbox);
        p.glyphs.push(Glyph {
            text: ch.to_string(),
            bbox,
            page: p.index,
            origin: at(k, BASELINE_FROM_TOP),
            rotation_deg,
            font_size,
            advance: Some(advance),
            weight: None,
            draw_index,
        });
    }
    boxes
}

/// The word's corners in its own image, clockwise from the start of its
/// top edge: top-left, top-right, bottom-right, bottom-left as read.
fn word_quad([x0, y0, x1, y1]: [f64; 4], a: f64) -> [(f64, f64); 4] {
    let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
    if a == 0.0 {
        return corners;
    }
    // Counter-clockwise as viewed, in a frame whose y points down.
    let (s, c) = a.to_radians().sin_cos();
    let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    corners.map(|(x, y)| {
        let (dx, dy) = (x - cx, y - cy);
        (cx + dx * c + dy * s, cy - dx * s + dy * c)
    })
}

fn quad_bounds(q: &[(f64, f64); 4]) -> ((f64, f64), (f64, f64)) {
    let xs = q.iter().map(|p| p.0);
    let ys = q.iter().map(|p| p.1);
    (
        (
            xs.clone().fold(f64::MAX, f64::min),
            xs.fold(f64::MIN, f64::max),
        ),
        (
            ys.clone().fold(f64::MAX, f64::min),
            ys.fold(f64::MIN, f64::max),
        ),
    )
}

/// Mark the elements OCR words make up as `provenance: "ocr"`.
///
/// Called on the final elements, after analysis has turned glyphs into
/// paragraphs and tables. An element is OCR's when any of its characters
/// lies on a character a reading added: a character read from pixels is
/// never presented as the PDF's own, so an element that holds both — a
/// table whose rows mix them, a line set across both — is OCR's. Block
/// assembly keeps an OCR line out of the page's own paragraphs, so the text
/// layer's prose keeps its provenance, and a scanned page's stamped header,
/// which the text layer had, keeps its own. Elements a model tier spliced in
/// keep theirs.
pub fn mark_provenance(elements: &mut [fluree_doc_model::Element], injected: &[Injected]) {
    use std::collections::HashMap;
    // Per page, character centers sorted by y: an element looks only at
    // the band of the page it covers.
    let mut by_page: HashMap<usize, Vec<(f64, f64)>> = HashMap::new();
    for r in injected {
        by_page.entry(r.page).or_default().extend(
            r.chars
                .iter()
                .map(|b| ((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5)),
        );
    }
    for v in by_page.values_mut() {
        v.sort_by(|a, b| a.1.total_cmp(&b.1));
    }
    // Only what the deterministic engine assembled: a model's splice says
    // where its own text came from.
    for e in elements.iter_mut().filter(|e| e.provenance == "rust") {
        let (Some(b), Some(centers)) = (e.bbox, by_page.get(&e.page)) else {
            continue;
        };
        let total = e.text.chars().filter(|c| !c.is_whitespace()).count();
        if total == 0 {
            continue;
        }
        let from = centers.partition_point(|c| c.1 < b.y0);
        let ocr = centers[from..]
            .iter()
            .take_while(|c| c.1 <= b.y1)
            .filter(|c| b.contains(c.0, c.1))
            .count();
        if ocr > 0 {
            e.provenance = "ocr";
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ImagePlacement;

    fn page(glyphs: Vec<Glyph>, images: Vec<ImagePlacement>) -> Page {
        Page {
            index: 0,
            glyphs,
            images,
            rules: Vec::new(),
            fills: Vec::new(),
            dots: Vec::new(),
            width: 612.0,
            height: 792.0,
            rotation: 0,
            ocr: None,
        }
    }

    fn full_page_image() -> ImagePlacement {
        ImagePlacement {
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 612.0,
                y1: 792.0,
            },
            texty: true,
        }
    }

    fn native(text: &str, x: f64, y: f64) -> Vec<Glyph> {
        text.chars()
            .enumerate()
            .map(|(i, c)| Glyph {
                text: c.to_string(),
                bbox: Some(BBox {
                    x0: x + i as f64 * 6.0,
                    y0: y - 9.0,
                    x1: x + i as f64 * 6.0 + 6.0,
                    y1: y + 2.0,
                }),
                page: 0,
                origin: (x + i as f64 * 6.0, y),
                rotation_deg: 0.0,
                font_size: 10.0,
                advance: Some(6.0),
                weight: None,
                draw_index: i,
            })
            .collect()
    }

    fn word(text: &str, b: [f64; 4]) -> OcrWord {
        OcrWord {
            text: text.into(),
            bbox: b,
            angle: None,
            conf: Some(0.9),
        }
    }

    /// A 2× render of the page: 1224 × 1584 pixels.
    fn px(words: Vec<OcrWord>) -> WordReading {
        WordReading {
            coords: Coords::Px,
            width: Some(1224.0),
            height: Some(1584.0),
            engine: None,
            words,
        }
    }

    fn text_of(p: &Page) -> String {
        p.glyphs.iter().map(|g| g.text.as_str()).collect()
    }

    #[test]
    fn a_scanned_page_takes_its_words_in_page_units() {
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        let r = inject(
            &mut doc,
            &[(
                0,
                Crop::Page,
                px(vec![word("Account", [144.0, 200.0, 284.0, 226.0])]),
            )],
        );
        assert_eq!(r[0].taken, 1);
        assert_eq!(text_of(&doc.pages[0]), "Account");
        let g = &doc.pages[0].glyphs;
        // Halved onto the page, and spread evenly along it.
        assert!((g[0].bbox.unwrap().x0 - 72.0).abs() < 1e-9);
        assert!((g[6].bbox.unwrap().x1 - 142.0).abs() < 1e-9);
        assert!((g[0].font_size - 10.0).abs() < 0.1);
        assert_eq!(r[0].mean_conf, Some(0.9));
    }

    #[test]
    fn a_born_digital_page_refuses_a_whole_page_reading() {
        // Two hundred glyphs of text and no image: nothing routes, and a
        // reading of it would only replace good text with a guess at it.
        let glyphs: Vec<Glyph> = (0..20)
            .flat_map(|row| native("Account no", 72.0, 100.0 + row as f64 * 14.0))
            .collect();
        let before = glyphs.len();
        let mut doc = Document {
            pages: vec![page(glyphs, Vec::new())],
        };
        let r = inject(
            &mut doc,
            &[(
                0,
                Crop::Page,
                px(vec![word("Acc0unt", [144.0, 190.0, 284.0, 216.0])]),
            )],
        );
        assert_eq!(r[0].refused, Some("page not routed to a reader"));
        assert_eq!(doc.pages[0].glyphs.len(), before);
    }

    #[test]
    fn a_word_over_the_text_layer_is_dropped() {
        // A scan with a stamped header the text layer already carries.
        let mut doc = Document {
            pages: vec![page(native("REF", 72.0, 60.0), vec![full_page_image()])],
        };
        let r = inject(
            &mut doc,
            &[(
                0,
                Crop::Page,
                px(vec![
                    word("REF", [144.0, 100.0, 180.0, 124.0]),
                    word("Holder", [144.0, 300.0, 240.0, 326.0]),
                ]),
            )],
        );
        assert_eq!((r[0].taken, r[0].dropped), (1, 1));
        assert_eq!(text_of(&doc.pages[0]), "REFHolder");
    }

    #[test]
    fn a_region_reading_maps_through_its_margin_and_keeps_to_the_region() {
        // A text page with a screenshot pasted in its middle.
        let mut glyphs = Vec::new();
        for row in 0..12 {
            glyphs.extend(native("Body text line", 72.0, 80.0 + row as f64 * 14.0));
        }
        let shot = BBox {
            x0: 100.0,
            y0: 300.0,
            x1: 500.0,
            y1: 500.0,
        };
        let mut doc = Document {
            pages: vec![page(
                glyphs,
                vec![ImagePlacement {
                    bbox: shot,
                    texty: true,
                }],
            )],
        };
        assert!(matches!(
            route::decide(&doc.pages[0]).0,
            Route::VlmRegions(_)
        ));
        // The crop is the region plus 6 units each side: 412 × 212, read at
        // 4 px per unit.
        let reading = WordReading {
            coords: Coords::Px,
            width: Some(1648.0),
            height: Some(848.0),
            engine: None,
            words: vec![
                // 24 px in: the region's own left edge, 100 on the page.
                word("Balance", [24.0, 100.0, 184.0, 140.0]),
                // In the margin above the region: a neighbour's text.
                word("cut", [400.0, 2.0, 460.0, 20.0]),
            ],
        };
        let r = inject(&mut doc, &[(0, Crop::Region(0), reading)]);
        assert_eq!((r[0].taken, r[0].dropped), (1, 1));
        let g = doc.pages[0].glyphs.iter().rfind(|g| g.text == "B").unwrap();
        assert!((g.bbox.unwrap().x0 - 100.0).abs() < 1e-9);
        // Inside the word's band, 319 to 329 on the page.
        let b = g.bbox.unwrap();
        assert!(b.y0 >= 319.0 && b.y1 <= 329.5, "{b:?}");
    }

    #[test]
    fn a_crop_the_router_did_not_ask_for_is_refused() {
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        let r = inject(
            &mut doc,
            &[(
                0,
                Crop::Region(0),
                px(vec![word("x", [0.0, 0.0, 9.0, 9.0])]),
            )],
        );
        assert_eq!(r[0].refused, Some("region not routed to a reader"));
        assert!(doc.pages[0].glyphs.is_empty());
    }

    #[test]
    fn a_broken_text_page_trades_its_glyphs_for_the_reading() {
        // Glyphs whose Unicode never resolved: the page is there, the text
        // is not.
        let mut glyphs = native(&"x".repeat(150), 10.0, 100.0);
        for g in glyphs.iter_mut() {
            g.text.clear();
        }
        let mut doc = Document {
            pages: vec![page(glyphs, Vec::new())],
        };
        assert!(matches!(
            route::decide(&doc.pages[0]).0,
            Route::Vlm(route::Reason::BrokenText)
        ));
        inject(
            &mut doc,
            &[(
                0,
                Crop::Page,
                px(vec![word("Total", [20.0, 180.0, 120.0, 206.0])]),
            )],
        );
        assert_eq!(text_of(&doc.pages[0]), "Total");
        // The broken glyphs are gone, not only blank: none from before the
        // reading is left.
        assert_eq!(
            doc.pages[0]
                .glyphs
                .iter()
                .filter(|g| g.bbox.is_some())
                .count(),
            5
        );
    }

    #[test]
    fn a_line_reading_keeps_its_spaces_as_spaces() {
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        inject(
            &mut doc,
            &[(
                0,
                Crop::Page,
                px(vec![word("Date of birth", [144.0, 200.0, 404.0, 226.0])]),
            )],
        );
        let g = &doc.pages[0].glyphs;
        assert_eq!(g.len(), 13);
        assert!(g[4].text == " " && g[4].bbox.is_none());
    }

    #[test]
    fn a_turned_word_runs_along_its_angle() {
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        let mut w = word("AB", [100.0, 100.0, 140.0, 120.0]);
        w.angle = Some(90.0);
        inject(&mut doc, &[(0, Crop::Page, px(vec![w]))]);
        let g = &doc.pages[0].glyphs;
        assert!((g[0].rotation_deg - 90.0).abs() < 1e-6);
        // Read bottom to top: the second letter sits above the first.
        assert!(g[1].origin.1 < g[0].origin.1);
    }

    #[test]
    fn a_slightly_skewed_word_is_laid_level() {
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        let mut w = word("Balances", [144.0, 200.0, 544.0, 226.0]);
        w.angle = Some(0.6);
        inject(&mut doc, &[(0, Crop::Page, px(vec![w]))]);
        let g = &doc.pages[0].glyphs;
        assert!(g.iter().all(|g| g.rotation_deg == 0.0));
        assert!(g.iter().all(|x| x.origin.1 == g[0].origin.1));
    }

    #[test]
    fn the_sidecar_needs_a_size_for_pixels() {
        assert!(WordReading::from_json(r#"{"words":[]}"#).is_err());
        let r = WordReading::from_json(
            r#"{"width":10,"height":10,"words":[{"text":"a","box":[0,0,5,5],"conf":0.5}]}"#,
        )
        .unwrap();
        assert_eq!(r.words[0].conf, Some(0.5));
        assert!(WordReading::from_json(r#"{"coords":"page","words":[]}"#).is_ok());
    }

    #[test]
    fn crop_tags_round_trip() {
        assert_eq!(Crop::from_tag("full"), Some(Crop::Page));
        assert_eq!(Crop::from_tag("r3"), Some(Crop::Region(3)));
        assert_eq!(Crop::from_tag("t0"), None);
        assert_eq!(Crop::Region(3).tag(), "r3");
    }

    /// Words an engine would report for a scanned statement, in 2× render
    /// pixels: a title, a sentence, and a three-column table without rules.
    fn statement() -> Vec<OcrWord> {
        let mut words = vec![word("Account statement", [144.0, 120.0, 560.0, 166.0])];
        let sentence = ["Balances", "are", "shown", "in", "US", "dollars."];
        let mut x = 144.0;
        for w in sentence {
            let len = w.chars().count() as f64 * 12.0;
            words.push(word(w, [x, 220.0, x + len, 246.0]));
            x += len + 8.0;
        }
        let rows = [
            ["Date", "Reference", "Amount"],
            ["2026-01-05", "FR-0042", "1,250.00"],
            ["2026-01-19", "FR-0057", "310.40"],
            ["2026-02-02", "FR-0063", "88.15"],
        ];
        for (r, row) in rows.iter().enumerate() {
            let y = 320.0 + r as f64 * 40.0;
            for (c, cell) in row.iter().enumerate() {
                let x = 144.0 + c as f64 * 260.0;
                let len = cell.chars().count() as f64 * 12.0;
                words.push(word(cell, [x, y, x + len, y + 26.0]));
            }
        }
        words
    }

    #[test]
    fn a_scanned_page_reads_as_paragraphs_and_a_table() {
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        assert_eq!(crate::unread_pages(&doc, &[]).len(), 1);
        let injected = inject(&mut doc, &[(0, Crop::Page, px(statement()))]);
        let mut a = crate::document::analyze_with(
            &mut doc,
            &[],
            &crate::document::AnalyzeOptions::default(),
        );
        mark_provenance(&mut a.elements, &injected);
        assert!(a
            .elements
            .iter()
            .all(|e| e.provenance == "ocr" && e.bbox.is_some()));
        assert!(a
            .elements
            .iter()
            .any(|e| e.kind == "doco:SectionTitle" && e.text == "Account statement"));
        assert!(a
            .elements
            .iter()
            .any(|e| e.kind == "doco:Paragraph" && e.text == "Balances are shown in US dollars."));
        let table = a
            .elements
            .iter()
            .find(|e| e.kind == "doco:Table")
            .expect("a table");
        let cells = table.cells.as_ref().unwrap();
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[1], ["2026-01-05", "FR-0042", "1,250.00"]);
        assert!(crate::unread_pages(&doc, &a.elements).is_empty());
    }

    fn page_units(words: Vec<OcrWord>) -> WordReading {
        WordReading {
            coords: Coords::Page,
            width: None,
            height: None,
            engine: None,
            words,
        }
    }

    fn analysed(doc: &mut Document, injected: &[Injected]) -> Vec<fluree_doc_model::Element> {
        let mut a =
            crate::document::analyze_with(doc, &[], &crate::document::AnalyzeOptions::default());
        mark_provenance(&mut a.elements, injected);
        a.elements
    }

    /// One line of word boxes on a sheet turned `deg` counter-clockwise, each
    /// word sitting a little higher than the one before.
    fn skewed_line(deg: f64, stated: bool) -> Vec<OcrWord> {
        let t = deg.to_radians().tan();
        let mut x = 60.0;
        let mut out = Vec::new();
        for w in "Balances are shown in US dollars for the whole period ending".split(' ') {
            let len = w.len() as f64 * 6.0;
            let cy = 300.0 - (x + len / 2.0 - 60.0) * t;
            let mut o = word(w, [x, cy - 6.5, x + len, cy + 6.5]);
            o.angle = stated.then_some(deg);
            out.push(o);
            x += len + 4.0;
        }
        out
    }

    #[test]
    fn a_skewed_line_of_word_boxes_reads_as_one_line() {
        for stated in [true, false] {
            for deg in [0.6, 1.0, 2.0, 3.0, 5.0, -3.0] {
                let mut doc = Document {
                    pages: vec![page(Vec::new(), vec![full_page_image()])],
                };
                let r = inject(
                    &mut doc,
                    &[(0, Crop::Page, page_units(skewed_line(deg, stated)))],
                );
                assert!(
                    (r[0].skew_deg - deg).abs() < 0.2,
                    "{deg}° read as {}",
                    r[0].skew_deg
                );
                let texts: Vec<String> = analysed(&mut doc, &r)
                    .iter()
                    .map(|e| e.text.clone())
                    .collect();
                assert_eq!(
                    texts,
                    ["Balances are shown in US dollars for the whole period ending"],
                    "{deg}°, angle stated: {stated}"
                );
            }
        }
    }

    /// Tesseract-style ink boxes: a word's box is as tall as its letters,
    /// `our` half the height of `Payments`.
    fn ink_box(text: &str, x: f64, baseline: f64, em: f64) -> OcrWord {
        let tall = text
            .chars()
            .any(|c| c.is_uppercase() || c.is_ascii_digit() || "bdfhklt".contains(c));
        let deep = text.chars().any(|c| "gjpqy,".contains(c));
        let top = baseline - if tall { 0.72 * em } else { 0.5 * em };
        let bottom = baseline + if deep { 0.22 * em } else { 0.0 };
        word(
            text,
            [x, top, x + text.chars().count() as f64 * em * 0.5, bottom],
        )
    }

    #[test]
    fn ink_tight_word_boxes_keep_one_size_per_line() {
        let mut words = Vec::new();
        let mut x = 60.0;
        for w in ["Quarterly", "summary"] {
            words.push(ink_box(w, x, 100.0, 18.0));
            x += w.len() as f64 * 9.0 + 6.0;
        }
        let body = [
            "our new cosmic name is now known across many rooms",
            "and every morning we review the open issues in a row",
            "Payments were made in January as agreed by the parties",
            "in some cases a minor revision was more or less necessary",
        ];
        for (i, l) in body.iter().enumerate() {
            let mut x = 60.0;
            for w in l.split(' ') {
                words.push(ink_box(w, x, 140.0 + i as f64 * 13.0, 10.0));
                x += w.len() as f64 * 5.0 + 3.0;
            }
        }
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let els = analysed(&mut doc, &r);
        let kinds: Vec<(&str, &str)> = els
            .iter()
            .map(|e| (e.kind.as_str(), e.text.as_str()))
            .collect();
        assert_eq!(kinds[0], ("doco:SectionTitle", "Quarterly summary"));
        assert_eq!(kinds.len(), 2, "{kinds:?}");
        assert_eq!(kinds[1].0, "doco:Paragraph");
        assert!(kinds[1].1.ends_with("more or less necessary"));
    }

    #[test]
    fn region_numbers_survive_a_partial_reading() {
        let mut glyphs = Vec::new();
        for row in 0..30 {
            glyphs.extend(native(
                "Body text line here",
                72.0,
                60.0 + row as f64 * 12.0,
            ));
        }
        let r0 = BBox {
            x0: 60.0,
            y0: 430.0,
            x1: 550.0,
            y1: 560.0,
        };
        let r1 = BBox {
            x0: 60.0,
            y0: 600.0,
            x1: 550.0,
            y1: 760.0,
        };
        let shots = [r0, r1].map(|bbox| ImagePlacement { bbox, texty: true });
        let mut doc = Document {
            pages: vec![page(glyphs, shots.to_vec())],
        };
        let words: Vec<OcrWord> = (0..5)
            .map(|i| {
                let x = 80.0 + i as f64 * 80.0;
                word("Revenue", [x, 450.0, x + 60.0, 463.0])
            })
            .collect();
        let r = inject(&mut doc, &[(0, Crop::Region(0), page_units(words))]);
        assert_eq!(r[0].taken, 5);
        assert!(matches!(route::decide(&doc.pages[0]).0, Route::VlmRegions(ref v) if v.len() == 2));
        let a = crate::document::analyze_with(
            &mut doc,
            &[],
            &crate::document::AnalyzeOptions {
                emit_anchors: true,
                ..Default::default()
            },
        );
        let anchors: Vec<(&str, f64)> = a
            .elements
            .iter()
            .filter(|e| e.text.starts_with("[[VLM:"))
            .map(|e| (e.text.as_str(), e.bbox.unwrap().y0))
            .collect();
        assert_eq!(anchors, [("[[VLM:p0:r1]]", r1.y0)]);
    }

    #[test]
    fn a_turned_stamp_is_judged_by_its_own_area() {
        // A 45° stamp's axis-aligned box covers a glyph far off its band.
        let mut glyphs = native("ok", 400.0, 500.0);
        glyphs.push(Glyph {
            bbox: Some(BBox {
                x0: 127.0,
                y0: 125.0,
                x1: 133.0,
                y1: 135.0,
            }),
            ..native("x", 127.0, 133.0).remove(0)
        });
        let mut doc = Document {
            pages: vec![page(glyphs, vec![full_page_image()])],
        };
        let mut w = word("CONFIDENTIAL", [100.0, 185.0, 300.0, 215.0]);
        w.angle = Some(45.0);
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(vec![w]))]);
        assert_eq!((r[0].taken, r[0].dropped), (1, 0));
    }

    #[test]
    fn a_broken_page_keeps_its_glyphs_when_the_reading_cannot_be_placed() {
        let mut glyphs = native(&"x".repeat(150), 10.0, 100.0);
        for g in glyphs.iter_mut().take(40) {
            g.text.clear();
        }
        let mut doc = Document {
            pages: vec![page(glyphs, Vec::new())],
        };
        let sizeless = WordReading {
            width: None,
            height: None,
            ..px(vec![word("x", [0.0, 0.0, 5.0, 5.0])])
        };
        let r = inject(&mut doc, &[(0, Crop::Page, sizeless)]);
        assert!(r[0].refused.is_some());
        assert_eq!(doc.pages[0].glyphs.len(), 150);
        assert!(doc.pages[0].ocr.is_none());
    }

    #[test]
    fn words_off_the_page_or_without_a_box_are_dropped() {
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        let words = vec![
            word("far", [1e9, 1e9, 1e9 + 50.0, 1e9 + 13.0]),
            word("neg", [-5000.0, -5000.0, -4950.0, -4987.0]),
            word("flat", [10.0, 10.0, 60.0, 10.0]),
            word("huge", [0.0, 0.0, 1e300, 1e300]),
            word("ok", [100.0, 100.0, 120.0, 113.0]),
            word("ok", [100.0, 130.0, 120.0, 143.0]),
            word("ok", [100.0, 160.0, 120.0, 173.0]),
            word("ok", [100.0, 190.0, 120.0, 203.0]),
            word("ok", [100.0, 220.0, 120.0, 233.0]),
        ];
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        assert_eq!((r[0].taken, r[0].dropped), (5, 4));
        assert_eq!(text_of(&doc.pages[0]), "okokokokok");
    }

    #[test]
    fn overlapping_narrow_boxes_are_all_laid() {
        // Each would otherwise be the other's right neighbour, and neither
        // would start a run.
        let words = vec![
            word("Total", [60.0, 100.0, 100.0, 113.0]),
            word("|", [110.0, 100.0, 113.0, 113.0]),
            word("|", [111.0, 100.0, 114.0, 113.0]),
            word("42", [125.0, 100.0, 140.0, 113.0]),
        ];
        let mut doc = blank_scan();
        inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        assert_eq!(text_of(&doc.pages[0]).matches('|').count(), 2);
    }

    #[test]
    fn a_dash_between_ink_boxes_stays_on_its_line() {
        let mut words = Vec::new();
        let mut x = 60.0;
        for w in "Certified true copy — 02/01/2026 — P. Vantongeren".split(' ') {
            let len = w.chars().count() as f64 * 5.0;
            words.push(if w == "—" {
                word(w, [x, 136.0, x + len, 137.0])
            } else {
                ink_box(w, x, 140.0, 10.0)
            });
            x += len + 3.0;
        }
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        assert_eq!(
            texts(&mut doc, &r),
            ["Certified true copy — 02/01/2026 — P. Vantongeren"]
        );
    }

    #[test]
    fn characters_boxed_one_by_one_in_cjk_take_no_spaces() {
        let words: Vec<OcrWord> = "東京都庁の案内"
            .chars()
            .enumerate()
            .map(|(i, c)| {
                let x = 60.0 + i as f64 * 11.0;
                word(&c.to_string(), [x, 100.0, x + 10.0, 110.0])
            })
            .collect();
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        assert_eq!(texts(&mut doc, &r), ["東京都庁の案内"]);
    }

    #[test]
    fn a_page_ocr_read_whole_is_reported_unread_only_when_nearly_empty() {
        // Two words for a scan: read, but next to nothing came of it.
        let mut doc = blank_scan();
        let short = vec![word("KYC screen", [72.0, 72.0, 140.0, 85.0])];
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(short))]);
        let els = analysed(&mut doc, &r);
        let unread = crate::unread_pages(&doc, &els);
        assert_eq!(unread.len(), 1, "{unread:?}");
        assert_eq!(unread[0].reason, crate::OCR_SPARSE);
        // A line of real text clears it.
        let mut doc = blank_scan();
        let line = vec![word(
            "Account holder: Jane Doe, opened on 2026-01-05 at the branch",
            [72.0, 72.0, 432.0, 85.0],
        )];
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(line))]);
        let els = analysed(&mut doc, &r);
        assert!(crate::unread_pages(&doc, &els).is_empty());
    }

    #[test]
    fn repeated_dated_lines_stay_in_the_body_and_dates_in_their_tables() {
        // A statement line whose date changes each page repeats like a page
        // number, and is not one.
        let mut pages = Vec::new();
        for m in 1..=4 {
            let mut p = page(Vec::new(), vec![full_page_image()]);
            p.index = m - 1;
            pages.push(p);
        }
        let mut doc = Document { pages };
        let readings: Vec<(usize, Crop, WordReading)> = (1..=4)
            .map(|m| {
                let mut w = run_words(
                    &format!("Statement ending on 0{m}/15/2025"),
                    60.0,
                    40.0,
                    10.0,
                    5.0,
                );
                w.extend(run_words(
                    &format!("Paid 0{m}/15/2025 in full today"),
                    60.0,
                    300.0,
                    10.0,
                    5.0,
                ));
                w.extend(run_words(&format!("{m}"), 300.0, 760.0, 10.0, 5.0));
                (m - 1, Crop::Page, page_units(w))
            })
            .collect();
        let r = inject(&mut doc, &readings);
        let all = texts(&mut doc, &r).join("\n");
        for m in 1..=4 {
            assert!(
                all.contains(&format!("Statement ending on 0{m}/15/2025")),
                "{all}"
            );
            assert!(
                all.contains(&format!("Paid 0{m}/15/2025 in full today")),
                "{all}"
            );
        }
    }

    #[test]
    fn a_margin_note_read_on_every_scanned_page_stays_in_the_text() {
        // A form copied per applicant, scanned: the same note runs down the
        // left margin of every page, beside the text and on none of it, as a
        // watermark turned up the margin does. It was read, and it is the
        // page's.
        let note = "Form MX-100 Rev 2026 Office copy";
        let mut pages = Vec::new();
        for m in 0..3 {
            let mut p = page(Vec::new(), vec![full_page_image()]);
            p.index = m;
            pages.push(p);
        }
        let mut doc = Document { pages };
        let readings: Vec<(usize, Crop, WordReading)> = (0..3)
            .map(|m| {
                let mut w = Vec::new();
                for (l, name) in ["Ann Mitchell", "Bo Lindqvist", "Chen Wu"]
                    .iter()
                    .enumerate()
                {
                    let y = 200.0 + 40.0 * l as f64;
                    w.extend(run_words(
                        &format!("Applicant {m}.{l} {name}"),
                        90.0,
                        y,
                        10.0,
                        5.0,
                    ));
                }
                // Boxed before its turn about its centre: x 25-35, y 250-410
                // once turned.
                let mut v = word(note, [-50.0, 325.0, 110.0, 335.0]);
                v.angle = Some(-90.0);
                w.push(v);
                (m, Crop::Page, page_units(w))
            })
            .collect();
        let r = inject(&mut doc, &readings);
        let all = texts(&mut doc, &r).join("\n");
        assert_eq!(all.matches(note).count(), 3, "{all}");
        assert!(all.contains("Applicant 2.1 Bo Lindqvist"), "{all}");
    }

    #[test]
    fn stacked_lines_on_a_skewed_sheet_do_not_interleave() {
        // A tightly set form photographed at -1.8°: labels over values, one
        // row spanning the page and the next only its left side.
        let t = (-1.8f64).to_radians().tan();
        let at = |text: &str, x: f64, y: f64, len: f64| {
            let cy = y - (x + len / 2.0) * t;
            let mut w = word(text, [x, cy, x + len, cy + 8.0]);
            w.angle = Some(-1.8);
            w
        };
        let words = vec![
            at("Party Name", 60.0, 100.0, 32.0),
            at("Party ID", 160.0, 100.0, 22.0),
            at("Date of Birth", 270.0, 100.0, 30.0),
            at("Notes", 500.0, 100.0, 16.0),
            at("Ann Nancy Mitchell", 60.0, 106.0, 53.0),
            at("SSN/TIN", 60.0, 112.0, 25.0),
            at("914-35-7075", 60.0, 118.0, 36.0),
        ];
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let all = texts(&mut doc, &r).join("\n");
        for t in ["Party Name", "Ann Nancy Mitchell", "SSN/TIN", "914-35-7075"] {
            assert!(all.contains(t), "{t} in {all}");
        }
    }

    #[test]
    fn two_overlapping_halves_of_a_line_do_not_interleave() {
        // A W-9's certification line, boxed in two halves that share 4pt.
        let words = vec![
            word(
                "Under penalties of perjury, I certify that the number shown on",
                [56.0, 290.0, 284.0, 300.0],
            ),
            word(
                "this form is my correct taxpayer identification number and",
                [280.0, 292.0, 534.0, 302.0],
            ),
            word("citizen or other U.S. person.", [56.0, 299.0, 160.0, 309.0]),
        ];
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let all = texts(&mut doc, &r).join("\n");
        assert!(all.contains("shown on this form"), "{all}");
    }

    #[test]
    fn the_space_between_two_overlapping_words_stays_between_them() {
        // A fax header: the sender and the date boxed a pixel into each other,
        // under a page count set tight above them.
        let words = vec![
            word("P.002/002", [198.6, 2.0, 252.2, 11.0]),
            word("FROM: (613) 555-0181", [13.0, 2.0, 115.5, 12.5]),
            word("11/06/202513:54", [115.0, 3.0, 202.0, 12.0]),
        ];
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let all = texts(&mut doc, &r).join("\n");
        assert!(all.contains("555-0181 11/06/2025"), "{all}");
    }

    #[test]
    fn a_label_boxed_on_its_value_stays_apart() {
        // A screen's field labels set a few points over their values, the
        // boxes overlapping: as boxed, `Reference` read as the value's
        // superscript and the two came out as `fe7r5`.
        let words = vec![
            word("Reference", [114.0, 87.6, 127.5, 91.1]),
            word("MSG757644818", [112.5, 88.5, 139.5, 95.5]),
            word("Status", [202.0, 87.8, 210.0, 91.1]),
            word("Completed", [201.0, 89.5, 219.0, 95.0]),
        ];
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let all = texts(&mut doc, &r).join("\n");
        for t in ["Reference", "MSG757644818", "Status", "Completed"] {
            assert!(all.contains(t), "{t} in {all}");
        }
    }

    #[test]
    fn a_word_a_little_off_a_tilted_sheet_stays_on_its_line() {
        // Neighbours at 6-7°, one at 8.2°: past a fixed limit, not off the sheet.
        let mut doc = blank_scan();
        let t = 6.8f64.to_radians().tan();
        let words: Vec<OcrWord> = [
            ("DOB 02/22/1937", 100.0, 6.4),
            ("HART", 130.0, 8.2),
            ("ALEXANDER BENJAMIN", 160.0, 7.1),
        ]
        .iter()
        .map(|(text, y, a)| {
            let len = text.chars().count() as f64 * 6.0;
            let cy = y - (60.0 + len / 2.0) * t;
            let mut w = word(text, [60.0, cy, 60.0 + len, cy + 12.0]);
            w.angle = Some(*a);
            w
        })
        .collect();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let all = texts(&mut doc, &r).join("\n");
        assert!(all.contains("HART"), "{all}");
    }

    #[test]
    fn upright_headers_do_not_interleave() {
        // Two table headers set sideways, in tall boxes with no angle stated.
        let words = vec![
            word("Account Holder", [300.0, 100.0, 314.0, 190.0]),
            word("OWNER ROLE", [320.0, 110.0, 334.0, 180.0]),
        ];
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let all = texts(&mut doc, &r).join("\n");
        assert!(all.contains("Account Holder"), "{all}");
        assert!(all.contains("OWNER ROLE"), "{all}");
    }

    #[test]
    fn page_numbers_do_not_scrub_the_numbers_of_a_table() {
        let mut pages = Vec::new();
        for i in 0..6 {
            let mut p = page(Vec::new(), vec![full_page_image()]);
            p.index = i;
            pages.push(p);
        }
        let mut doc = Document { pages };
        let rows = [("Widget", "12"), ("Bolt", "7"), ("Nut", "45"), ("Pin", "3")];
        let readings: Vec<(usize, Crop, WordReading)> = (0..6)
            .map(|i| {
                let mut w = Vec::new();
                for (r, (a, b)) in rows.iter().enumerate() {
                    let y = 100.0 + r as f64 * 20.0;
                    w.push(word(a, [60.0, y, 100.0, y + 10.0]));
                    w.push(word(b, [300.0, y, 312.0, y + 10.0]));
                }
                w.push(word(&(i + 1).to_string(), [300.0, 760.0, 306.0, 770.0]));
                (i, Crop::Page, page_units(w))
            })
            .collect();
        let r = inject(&mut doc, &readings);
        let all = texts(&mut doc, &r).join("\n");
        assert_eq!(all.matches("45").count(), 6, "{all}");
        assert_eq!(all.matches("12").count(), 6, "{all}");
    }

    #[test]
    fn an_oblique_stamp_reads_in_order() {
        for angle in [45.0, -45.0, 30.0, 180.0] {
            let mut w = word("RECEIVED 12 MAR 2026", [200.0, 300.0, 320.0, 316.0]);
            w.angle = Some(angle);
            let mut doc = blank_scan();
            let r = inject(&mut doc, &[(0, Crop::Page, page_units(vec![w]))]);
            assert_eq!(texts(&mut doc, &r), ["RECEIVED 12 MAR 2026"], "{angle}");
        }
    }

    #[test]
    fn a_tall_rule_read_as_a_bar_joins_no_row() {
        let rows = [("North", "East"), ("South", "West"), ("Upper", "Lower")];
        let mut words = Vec::new();
        for (i, (a, b)) in rows.iter().enumerate() {
            let y = 100.0 + i as f64 * 20.0;
            words.push(word(a, [60.0, y, 95.0, y + 10.0]));
            words.push(word(b, [105.0, y, 140.0, y + 10.0]));
        }
        words.push(word("|", [97.0, 98.0, 103.0, 152.0]));
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let all = texts(&mut doc, &r).join("\n");
        for t in ["North", "East", "South", "West", "Upper", "Lower"] {
            assert!(all.contains(t), "{t} in {all}");
        }
    }

    #[test]
    fn a_stray_word_does_not_read_a_page() {
        // One word lands, five hundred do not: the scan stays unread.
        let mut words = vec![word("stray", [100.0, 100.0, 130.0, 110.0])];
        for i in 0..500 {
            let x = 5000.0 + i as f64 * 40.0;
            words.push(word("far", [x, 5000.0, x + 30.0, 5010.0]));
        }
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        assert!(r[0].refused.is_some());
        assert!(doc.pages[0].glyphs.is_empty() && doc.pages[0].ocr.is_none());
        assert_eq!(crate::unread_pages(&doc, &[]).len(), 1);
    }

    #[test]
    fn short_narrow_words_are_not_taken_for_upright_text() {
        let words: Vec<OcrWord> = ["ill", "lit", "fit", "III"]
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let y = 100.0 + i as f64 * 20.0;
                word(t, [60.0, y, 68.0, y + 14.0])
            })
            .collect();
        let mut doc = blank_scan();
        inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        assert!(doc.pages[0].glyphs.iter().all(|g| g.rotation_deg == 0.0));
    }

    #[test]
    fn an_upright_box_stays_where_it_was_drawn_at_any_scale() {
        let mut doc = blank_scan();
        // A 1224 x 792 image of a 612 x 792 page: x halves, y does not.
        let reading = WordReading {
            coords: Coords::Px,
            width: Some(1224.0),
            height: Some(792.0),
            engine: None,
            words: vec![word("Account Holder", [600.0, 200.0, 640.0, 290.0])],
        };
        inject(&mut doc, &[(0, Crop::Page, reading)]);
        for g in doc.pages[0].glyphs.iter().filter_map(|g| g.bbox) {
            assert!(g.y0 >= 199.0 && g.y1 <= 291.0, "{g:?}");
        }
    }

    #[test]
    fn boxes_of_any_size_cost_bounded_memory() {
        // Centered on the page, so only its size can rule it out.
        let mut words = vec![word("huge", [-1e300, -1e300, 1e300, 1e300])];
        // One page-tall box among hair-thin ones.
        words.push(word("tall", [10.0, 0.0, 20.0, 790.0]));
        for i in 0..200 {
            let x = 30.0 + i as f64 * 2.0;
            words.push(word("a", [x, 100.0, x + 1.0, 100.0 + 1e-9]));
        }
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        assert_eq!(r[0].taken + r[0].dropped, 202);
        assert!(text_of(&doc.pages[0]).contains("tall"));
        assert!(!text_of(&doc.pages[0]).contains("huge"));
    }

    #[test]
    fn a_box_one_float_step_tall_is_laid_without_a_panic() {
        // Thinner than one step of its coordinate: it passes the `y1 > y0`
        // check but lays out at size 0, with no reach for its neighbours.
        let up = |y: f64| f64::from_bits(y.to_bits() + 1);
        let alone = vec![word("ab", [72.0, 300.0, 142.0, up(300.0)])];
        let beside = vec![
            word("Balance", [72.0, 300.0, 140.0, 313.0]),
            word("_", [144.0, 313.0, 160.0, up(313.0)]),
        ];
        for words in [alone, beside] {
            let n = words.len();
            let mut doc = blank_scan();
            let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
            assert_eq!(r[0].taken + r[0].dropped, n, "{r:?}");
        }
    }

    #[test]
    fn decomposed_accents_are_composed_and_angles_wrap() {
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        let mut w = word("Cafe\u{301}", [60.0, 100.0, 100.0, 113.0]);
        w.angle = Some(359.4);
        inject(&mut doc, &[(0, Crop::Page, page_units(vec![w]))]);
        assert_eq!(text_of(&doc.pages[0]), "Café");
        assert!(doc.pages[0].glyphs.iter().all(|g| g.rotation_deg == 0.0));
    }

    #[cfg(feature = "render")]
    #[test]
    fn a_page_ocr_read_whole_asks_no_model_for_a_reading() {
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        let short = vec![word("Page", [72.0, 72.0, 110.0, 85.0])];
        inject(&mut doc, &[(0, Crop::Page, page_units(short))]);
        let a = crate::document::analyze_with(
            &mut doc.clone(),
            &[],
            &crate::document::AnalyzeOptions {
                emit_anchors: true,
                ..Default::default()
            },
        );
        assert!(crate::escalate::crops_for(&doc, &a, false).is_empty());
    }

    #[test]
    fn abutting_boxes_on_a_line_are_two_words() {
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        let words = vec![
            word("of returned information", [86.0, 500.0, 184.0, 507.0]),
            word("against that of SS Pack", [184.0, 500.0, 276.0, 507.0]),
        ];
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let texts: Vec<String> = analysed(&mut doc, &r)
            .iter()
            .map(|e| e.text.clone())
            .collect();
        assert_eq!(texts, ["of returned information against that of SS Pack"]);
    }

    #[test]
    fn a_whole_page_reading_of_a_region_page_fills_only_its_regions() {
        // A text page with a footnote pasted in as a picture: a whole-page
        // reading also reads the body, which the text layer already holds.
        let mut glyphs = Vec::new();
        for row in 0..12 {
            glyphs.extend(native("Body text line", 72.0, 80.0 + row as f64 * 14.0));
        }
        let note = BBox {
            x0: 60.0,
            y0: 600.0,
            x1: 560.0,
            y1: 700.0,
        };
        let mut doc = Document {
            pages: vec![page(
                glyphs,
                vec![ImagePlacement {
                    bbox: note,
                    texty: true,
                }],
            )],
        };
        let before = doc.pages[0].glyphs.len();
        let words = vec![
            word("Body text line", [72.0, 71.0, 156.0, 82.0]),
            word("Somewhere else", [300.0, 300.0, 400.0, 313.0]),
            word(
                "1 Evaluated against 100 test queries",
                [64.0, 640.0, 300.0, 653.0],
            ),
        ];
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        assert_eq!((r[0].taken, r[0].dropped), (1, 2));
        assert_eq!(doc.pages[0].glyphs.len() - before, 36);
        assert_eq!(doc.pages[0].ocr.as_ref().unwrap().regions, [0]);
        assert!(!doc.pages[0].ocr.as_ref().unwrap().whole);
    }

    fn blank_scan() -> Document {
        Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        }
    }

    fn texts(doc: &mut Document, r: &[Injected]) -> Vec<String> {
        analysed(doc, r).iter().map(|e| e.text.clone()).collect()
    }

    /// One box per word, `h` tall, `cw` per character, a character apart.
    fn run_words(line: &str, x0: f64, y: f64, h: f64, cw: f64) -> Vec<OcrWord> {
        let mut x = x0;
        let mut out = Vec::new();
        for w in line.split(' ') {
            let len = w.chars().count() as f64 * cw;
            out.push(word(w, [x, y, x + len, y + h]));
            x += len + cw;
        }
        out
    }

    /// The center of the first `n` character boxes, `n` the letters of `word`.
    fn words_box_of(word: &str, chars: &[BBox]) -> (f64, f64) {
        let n = word.chars().count();
        let (sx, sy) = chars[..n].iter().fold((0.0, 0.0), |(x, y), b| {
            (x + (b.x0 + b.x1) * 0.5, y + (b.y0 + b.y1) * 0.5)
        });
        (sx / n as f64, sy / n as f64)
    }

    #[test]
    fn a_page_read_upside_down_reads_line_by_line() {
        // A sheet fed upside down, read by an engine that says so: every
        // word at 180°, the first line at the bottom of the image.
        let lines = [
            "The quick brown fox jumps over the dog",
            "Pack my box with five dozen liquor jugs",
            "How vexingly quick daft zebras jump",
        ];
        let mut words = Vec::new();
        for (k, line) in lines.iter().enumerate() {
            for w in run_words(line, 72.0, 100.0 + 14.0 * k as f64, 13.0, 6.0) {
                let [x0, y0, x1, y1] = w.bbox;
                words.push(OcrWord {
                    bbox: [612.0 - x1, 792.0 - y1, 612.0 - x0, 792.0 - y0],
                    angle: Some(180.0),
                    ..w
                });
            }
        }
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        assert_eq!(r[0].taken, 22);
        // Each character is still marked where the image shows it.
        let first = words_box_of("The", &r[0].chars);
        assert!(
            (612.0 - 90.0..=612.0 - 72.0).contains(&first.0)
                && (792.0 - 113.0..=792.0 - 100.0).contains(&first.1),
            "{first:?}"
        );
        assert_eq!(texts(&mut doc, &r), [lines.join(" ")]);
    }

    /// Three lines of words as a sheet fed upside down shows them, every
    /// word at 180°; `off` of them pushed off the page.
    fn upside_down_words(off: usize) -> Vec<OcrWord> {
        let lines = [
            "The quick brown fox jumps over the dog",
            "Pack my box with five dozen liquor jugs",
            "How vexingly quick daft zebras jump",
        ];
        let mut words = Vec::new();
        for (k, line) in lines.iter().enumerate() {
            for w in run_words(line, 72.0, 300.0 + 14.0 * k as f64, 13.0, 6.0) {
                let [x0, y0, x1, y1] = w.bbox;
                let shift = if words.len() < off { 5000.0 } else { 0.0 };
                words.push(OcrWord {
                    bbox: [
                        612.0 - x1 + shift,
                        792.0 - y1,
                        612.0 - x0 + shift,
                        792.0 - y0,
                    ],
                    angle: Some(180.0),
                    ..w
                });
            }
        }
        words
    }

    #[test]
    fn a_page_with_text_of_its_own_is_not_turned_for_an_upside_down_reading() {
        // A fax: a full-page image, and a header the machine drew as text.
        let header = native("FAX FROM ACME", 40.0, 20.0);
        let mut doc = Document {
            pages: vec![page(header.clone(), vec![full_page_image()])],
        };
        let r = inject(
            &mut doc,
            &[(0, Crop::Page, page_units(upside_down_words(0)))],
        );
        assert_eq!(
            r[0].refused,
            Some("the reading runs upside down over a page of text")
        );
        let p = &doc.pages[0];
        assert_eq!(p.rotation, 0);
        let origins: Vec<(f64, f64)> = p.glyphs.iter().map(|g| g.origin).collect();
        let before: Vec<(f64, f64)> = header.iter().map(|g| g.origin).collect();
        assert_eq!(origins, before);
    }

    #[test]
    fn a_page_turned_for_a_reading_it_then_refuses_is_turned_back() {
        // Most of the words off the page: refused after the page was turned.
        let mut doc = blank_scan();
        let before = doc.pages[0].images[0].bbox;
        let r = inject(
            &mut doc,
            &[(0, Crop::Page, page_units(upside_down_words(15)))],
        );
        assert_eq!(
            r[0].refused,
            Some("most of the reading could not be placed")
        );
        let p = &doc.pages[0];
        assert_eq!(p.rotation, 0);
        let after = p.images[0].bbox;
        assert!((after.x0 - before.x0).abs() < 1e-9 && (after.y1 - before.y1).abs() < 1e-9);
    }

    #[test]
    fn a_stray_word_of_a_whole_reading_does_not_set_a_region_reading_aside() {
        let mut doc = page_with_regions(&[LOWER]);
        let region_words: Vec<OcrWord> = run_words(
            "Statement of account for the period ending in March",
            72.0,
            480.0,
            13.0,
            6.0,
        );
        let r = inject(
            &mut doc,
            &[
                (
                    0,
                    Crop::Page,
                    page_units(vec![word("Statement", [72.0, 480.0, 126.0, 493.0])]),
                ),
                (0, Crop::Region(0), page_units(region_words)),
            ],
        );
        assert_eq!(r[0].taken, 1);
        assert_eq!(r[1].refused, None, "{r:?}");
        assert!(r[1].taken >= 8, "{r:?}");
    }

    #[test]
    fn a_stamp_over_the_text_leaves_the_text_whole() {
        // Wherever a large level stamp lands over dense body text, the body
        // reads line for line and the stamp reads as its own word, neither
        // shrunk into the body's size nor cut into it.
        for dy in [0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0] {
            let mut words = Vec::new();
            for k in 0..20 {
                words.extend(run_words(
                    "The quick brown fox jumps over the dog",
                    72.0,
                    100.0 + 14.0 * k as f64,
                    13.0,
                    6.0,
                ));
            }
            words.push(word("PAID", [250.0, 204.0 + dy, 450.0, 269.0 + dy]));
            let mut doc = blank_scan();
            let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
            let text = texts(&mut doc, &r).join("\n");
            let body = text
                .matches("The quick brown fox jumps over the dog")
                .count();
            assert_eq!(body, 20, "dy {dy}: every body line whole:\n{text}");
            assert!(
                text.split_whitespace().any(|w| w == "PAID"),
                "dy {dy}: the stamp is a word of its own:\n{text}"
            );
            let size = |c: &str| {
                doc.pages[0]
                    .glyphs
                    .iter()
                    .find(|g| g.text == c)
                    .unwrap()
                    .font_size
            };
            assert!(
                size("P") > 2.0 * size("q"),
                "dy {dy}: the stamp keeps its size"
            );
        }
    }

    #[test]
    fn a_stamp_of_two_lines_over_the_text_leaves_the_text_whole() {
        // `PAID` with a smaller `VOID` stacked above it, over twenty body
        // lines, at every offset and spacing: each body line reads whole and
        // each stamp word stays a word.
        for gap in [0.0, 6.0, 12.0, 15.0, 18.0] {
            for dy in (0..=28).step_by(2).map(f64::from) {
                let mut words = Vec::new();
                for k in 0..20 {
                    words.extend(run_words(
                        "The quick brown fox jumps over the dog",
                        72.0,
                        100.0 + 14.0 * k as f64,
                        13.0,
                        6.0,
                    ));
                }
                let paid = 204.0 + dy;
                words.push(word("PAID", [250.0, paid, 450.0, paid + 65.0]));
                let void = paid - gap - 32.5 + 0.22 * 65.0;
                words.push(word("VOID", [280.0, void, 380.0, void + 32.5]));
                let mut doc = blank_scan();
                let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
                let text = texts(&mut doc, &r).join("\n");
                let body = text
                    .matches("The quick brown fox jumps over the dog")
                    .count();
                assert_eq!(body, 20, "gap {gap} dy {dy}:\n{text}");
                for stamp in ["PAID", "VOID"] {
                    assert!(
                        text.split_whitespace().any(|w| w == stamp),
                        "gap {gap} dy {dy}: {stamp} is a word:\n{text}"
                    );
                }
                // Each stamp keeps its size, which says what it is.
                let size = |c: &str| {
                    doc.pages[0]
                        .glyphs
                        .iter()
                        .find(|g| g.text == c)
                        .unwrap()
                        .font_size
                };
                assert!(
                    size("P") > 1.8 * size("V") && size("V") > 2.0 * size("q"),
                    "gap {gap} dy {dy}: sizes {} {} {}",
                    size("P"),
                    size("V"),
                    size("q")
                );
            }
        }
    }

    #[test]
    fn stamps_stacked_tight_keep_their_sizes() {
        // Two stamps stacked closer than their sizes would allow a label
        // and its value, over body lines: neither is shrunk to its spacing.
        let run = |font: f64, baseline: f64, x0: f64, x1: f64| Run {
            words: Vec::new(),
            font,
            baseline,
            x0,
            x1,
        };
        let mut runs: Vec<Run> = (0..14)
            .map(|k| run(10.0, 100.0 + 14.0 * k as f64, 72.0, 300.0))
            .collect();
        runs.push(run(50.0, 240.0, 250.0, 450.0));
        runs.push(run(25.0, 215.0, 280.0, 380.0));
        let lines: Vec<Vec<usize>> = (0..runs.len()).map(|r| vec![r]).collect();
        fit_stacked(&mut runs, &lines);
        assert_eq!(runs[14].font, 50.0);
        assert_eq!(runs[15].font, 25.0);
        assert!(runs[..14].iter().all(|r| r.font == 10.0));
    }

    #[test]
    fn a_stamp_over_the_text_leaves_the_text_its_size() {
        // Twenty lines of body text and a large level stamp boxed over two of
        // them: the stamp gives way, the body keeps one size and one
        // paragraph.
        let mut words = Vec::new();
        for k in 0..20 {
            words.extend(run_words(
                "The quick brown fox jumps over the dog",
                72.0,
                100.0 + 14.0 * k as f64,
                13.0,
                6.0,
            ));
        }
        words.push(word("PAID", [250.0, 204.0, 450.0, 269.0]));
        let mut doc = blank_scan();
        inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let body: Vec<f32> = doc.pages[0]
            .glyphs
            .iter()
            .filter(|g| g.bbox.is_some() && !"PAID".contains(g.text.as_str()))
            .map(|g| g.font_size)
            .collect();
        let (lo, hi) = body
            .iter()
            .fold((f32::MAX, 0.0f32), |(l, h), f| (l.min(*f), h.max(*f)));
        assert!(hi / lo < 1.01, "body sizes {lo}..{hi}");
        let stamp = doc.pages[0]
            .glyphs
            .iter()
            .find(|g| g.text == "P")
            .unwrap()
            .font_size;
        assert!(
            stamp > hi,
            "the stamp {stamp} stays larger than the text {hi}"
        );
    }

    #[test]
    fn a_region_reading_stands_when_the_whole_reading_took_nothing() {
        // A whole-page reading whose one word lies outside the page's region,
        // and a region reading inside it: the region is still read.
        let region = BBox {
            x0: 60.0,
            y0: 430.0,
            x1: 550.0,
            y1: 600.0,
        };
        let mut doc = Document {
            pages: vec![page(
                (0..1500)
                    .flat_map(|i| {
                        native("a", 20.0 + (i % 50) as f64 * 10.0, 650.0 + (i / 50) as f64)
                    })
                    .collect(),
                vec![ImagePlacement {
                    bbox: region,
                    texty: true,
                }],
            )],
        };
        let r = inject(
            &mut doc,
            &[
                (
                    0,
                    Crop::Page,
                    page_units(vec![word("Elsewhere", [100.0, 100.0, 160.0, 113.0])]),
                ),
                (
                    0,
                    Crop::Region(0),
                    page_units(vec![word("Inside", [100.0, 500.0, 150.0, 513.0])]),
                ),
            ],
        );
        assert_eq!(r[0].taken, 0);
        assert_eq!(r[1].refused, None, "{r:?}");
        assert_eq!(r[1].taken, 1);
    }

    /// A page of native text below two routed regions.
    fn page_with_regions(regions: &[BBox]) -> Document {
        Document {
            pages: vec![page(
                (0..1500)
                    .flat_map(|i| {
                        native("a", 20.0 + (i % 50) as f64 * 10.0, 650.0 + (i / 50) as f64)
                    })
                    .collect(),
                regions
                    .iter()
                    .map(|&bbox| ImagePlacement { bbox, texty: true })
                    .collect(),
            )],
        }
    }

    const UPPER: BBox = BBox {
        x0: 60.0,
        y0: 100.0,
        x1: 550.0,
        y1: 300.0,
    };
    const LOWER: BBox = BBox {
        x0: 60.0,
        y0: 430.0,
        x1: 550.0,
        y1: 600.0,
    };

    #[test]
    fn a_region_the_whole_reading_did_not_read_keeps_its_own_reading() {
        let mut doc = page_with_regions(&[UPPER, LOWER]);
        let r = inject(
            &mut doc,
            &[
                (
                    0,
                    Crop::Page,
                    page_units(vec![word("Above", [100.0, 150.0, 140.0, 163.0])]),
                ),
                (
                    0,
                    Crop::Region(0),
                    page_units(vec![word("Again", [100.0, 150.0, 140.0, 163.0])]),
                ),
                (
                    0,
                    Crop::Region(1),
                    page_units(vec![word("Below", [100.0, 500.0, 140.0, 513.0])]),
                ),
                (
                    0,
                    Crop::Region(9),
                    page_units(vec![word("Nowhere", [100.0, 500.0, 140.0, 513.0])]),
                ),
            ],
        );
        assert_eq!(r[0].taken, 1);
        assert_eq!(r[1].refused, Some("superseded by the page's whole reading"));
        assert_eq!(r[2].refused, None, "{r:?}");
        assert_eq!(r[2].taken, 1);
        // A region no one routed is that, whatever else read the page.
        assert_eq!(r[3].refused, Some("region not routed to a reader"));
        assert_eq!(doc.pages[0].ocr.as_ref().unwrap().regions, [0, 1]);
    }

    #[test]
    fn a_reading_set_aside_does_not_size_the_one_taken() {
        // A whole reading that boxes detected lines, and a reading of the
        // same region that boxes ink: the region's is set aside, so it must
        // not decide how the whole reading's boxes are sized.
        let lines = [
            "our new cosmic name is now known across many rooms",
            "Payments were made in January as agreed by the parties",
            "in some cases a minor revision was more or less necessary",
        ];
        let whole: Vec<OcrWord> = lines
            .iter()
            .enumerate()
            .flat_map(|(k, l)| run_words(l, 72.0, 450.0 + 16.0 * k as f64, 13.0, 6.0))
            .collect();
        // Enough of them to carry the vote on their own.
        let mut ink = Vec::new();
        for k in 0..8 {
            let mut x = 72.0;
            for w in lines[k % 3].split(' ') {
                ink.push(ink_box(w, x, 445.0 + 18.0 * k as f64, 10.0));
                x += w.len() as f64 * 6.0 + 6.0;
            }
        }
        let size = |readings: &[(usize, Crop, WordReading)]| {
            let mut doc = page_with_regions(&[LOWER]);
            let r = inject(&mut doc, readings);
            assert!(r[0].taken > 0, "{r:?}");
            doc.pages[0]
                .glyphs
                .iter()
                .find(|g| g.text == "J")
                .unwrap()
                .font_size
        };
        let alone = size(&[(0, Crop::Page, page_units(whole.clone()))]);
        let both = size(&[
            (0, Crop::Page, page_units(whole)),
            (0, Crop::Region(0), page_units(ink)),
        ]);
        assert_eq!(alone, both);
    }

    #[test]
    fn a_box_that_overflows_once_scaled_is_dropped() {
        let mut doc = blank_scan();
        let r = inject(
            &mut doc,
            &[(
                0,
                Crop::Page,
                px(vec![
                    word("Hello", [1e308, -1e308, 1.7e308, 1e308]),
                    word("world", [100.0, 100.0, 200.0, 126.0]),
                ]),
            )],
        );
        assert_eq!((r[0].taken, r[0].dropped), (1, 1), "{r:?}");
        assert!(doc.pages[0]
            .glyphs
            .iter()
            .all(|g| g.origin.0.is_finite() && g.origin.1.is_finite()));
    }

    #[test]
    fn overlapping_regions_lay_a_word_once() {
        // Two regions sharing a band: each crop reads the word in it, a
        // little apart, as two crops of the same pixels do.
        let at = |y0, y1| ImagePlacement {
            bbox: BBox {
                x0: 60.0,
                y0,
                x1: 550.0,
                y1,
            },
            texty: true,
        };
        let mut doc = Document {
            pages: vec![page(
                (0..1500)
                    .flat_map(|i| {
                        native(
                            "a",
                            20.0 + (i % 50) as f64 * 10.0,
                            720.0 + (i / 70) as f64 * 0.01,
                        )
                    })
                    .collect(),
                vec![at(430.0, 600.0), at(500.0, 700.0)],
            )],
        };
        let r = inject(
            &mut doc,
            &[
                (
                    0,
                    Crop::Region(0),
                    page_units(vec![word("Overlap", [100.0, 540.0, 160.0, 553.0])]),
                ),
                (
                    0,
                    Crop::Region(1),
                    page_units(vec![word("Overlap", [100.5, 540.4, 160.5, 553.4])]),
                ),
            ],
        );
        assert_eq!((r[0].taken, r[1].taken), (1, 0), "{r:?}");
        let ocr: String = doc.pages[0]
            .glyphs
            .iter()
            .filter(|g| doc.pages[0].is_ocr(g))
            .map(|g| g.text.as_str())
            .collect();
        assert_eq!(ocr.matches("Overlap").count(), 1, "{ocr}");
    }

    #[test]
    fn the_runs_of_a_visual_line_share_a_baseline() {
        // A label and its value across a gap, boxed a point and a half apart:
        // one line, one baseline.
        let mut doc = blank_scan();
        inject(
            &mut doc,
            &[(
                0,
                Crop::Page,
                page_units(vec![
                    word("Account", [60.0, 300.0, 120.0, 313.0]),
                    word("4417-0093-21", [300.0, 301.5, 400.0, 314.5]),
                ]),
            )],
        );
        let ys: Vec<f64> = doc.pages[0]
            .glyphs
            .iter()
            .filter(|g| g.bbox.is_some())
            .map(|g| g.origin.1)
            .collect();
        assert!(ys.iter().all(|y| (y - ys[0]).abs() < 1e-9), "{ys:?}");
    }

    #[test]
    fn a_reading_of_the_image_turned_a_quarter_is_refused() {
        // A full-page reading whose size is the page's turned: an engine that
        // read a photo without its EXIF orientation.
        let mut doc = blank_scan();
        let (w, h) = doc.pages[0].display_size();
        let reading = WordReading {
            coords: Coords::Px,
            width: Some(h * 2.0),
            height: Some(w * 2.0),
            engine: None,
            words: vec![word("Total", [20.0, 20.0, 120.0, 46.0])],
        };
        let r = inject(&mut doc, &[(0, Crop::Page, reading)]);
        assert_eq!(
            r[0].refused,
            Some("the reading is sized for the image turned a quarter")
        );
        // At the page's own aspect it is taken.
        let reading = WordReading {
            coords: Coords::Px,
            width: Some(w * 2.0),
            height: Some(h * 2.0),
            engine: None,
            words: vec![word("Total", [20.0, 20.0, 120.0, 46.0])],
        };
        assert_eq!(
            inject(&mut blank_scan(), &[(0, Crop::Page, reading)])[0].taken,
            1
        );
    }

    #[test]
    fn a_long_line_of_words_injects_in_linear_time() {
        // One line, thousands of words: a neighbour search over the whole
        // line for each word is quadratic.
        let time = |n: usize, tall: bool| {
            let mut words: Vec<OcrWord> = (0..n)
                .map(|i| {
                    word(
                        "ab",
                        [
                            20.0 + i as f64 * 0.07,
                            300.0,
                            20.05 + i as f64 * 0.07,
                            303.0,
                        ],
                    )
                })
                .collect();
            if tall {
                words.push(word("X", [10.0, 10.0, 12.0, 780.0]));
            }
            let mut doc = blank_scan();
            let t = std::time::Instant::now();
            inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
            t.elapsed().as_secs_f64()
        };
        let (small, large) = (time(2_000, false), time(8_000, false));
        assert!(
            large < 8.0 * small.max(0.01),
            "2k words {small:.3}s, 8k {large:.3}s"
        );
        // One page-tall box among them must not widen every word's window.
        let (small, large) = (time(2_000, true), time(8_000, true));
        assert!(
            large < 8.0 * small.max(0.01),
            "with a tall box: 2k words {small:.3}s, 8k {large:.3}s"
        );
    }

    #[test]
    fn many_runs_on_one_line_inject_in_near_linear_time() {
        // Words a column gutter apart on one baseline: each a run of its own,
        // and each run judged against the others of its line.
        let time = |n: usize| {
            let words: Vec<OcrWord> = (0..n)
                .map(|i| {
                    let x = 20.0 + i as f64 * 0.03;
                    word("ab", [x, 300.0, x + 0.002, 300.03])
                })
                .collect();
            let mut doc = blank_scan();
            let t = std::time::Instant::now();
            inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
            t.elapsed().as_secs_f64()
        };
        let (small, large) = (time(5_000), time(20_000));
        assert!(
            large < 8.0 * small.max(0.01),
            "5k runs {small:.3}s, 20k {large:.3}s"
        );
    }

    #[test]
    fn words_over_a_dense_page_are_checked_in_near_linear_time() {
        // A second whole reading of a page the first already filled: each of
        // its words is checked against the glyphs under it, not against all
        // of the page's.
        let grid = |n: usize, dx: f64| -> Vec<OcrWord> {
            (0..n)
                .map(|i| {
                    let (x, y) = (
                        20.0 + (i % 200) as f64 * 2.8 + dx,
                        20.0 + (i / 200) as f64 * 3.5,
                    );
                    word("ab", [x, y, x + 2.0, y + 3.0])
                })
                .collect()
        };
        let time = |n: usize| {
            let mut doc = blank_scan();
            let t = std::time::Instant::now();
            let r = inject(
                &mut doc,
                &[
                    (0, Crop::Page, page_units(grid(n, 0.0))),
                    (0, Crop::Page, page_units(grid(n, 0.4))),
                ],
            );
            let took = t.elapsed().as_secs_f64();
            assert!(r[0].taken > 0 && r[1].dropped > 0, "{r:?}");
            took
        };
        let (small, large) = (time(5_000), time(20_000));
        assert!(
            large < 8.0 * small.max(0.01),
            "5k words {small:.3}s, 20k {large:.3}s"
        );
    }

    #[test]
    fn two_columns_at_the_same_heights_stay_two_columns() {
        let left = [
            "Revenue rose in the quarter as",
            "customers renewed their plans",
            "and new accounts were opened",
            "across every region we serve",
        ];
        let right = [
            "Costs fell slightly because the",
            "team moved two data centres",
            "to cheaper hosting contracts",
            "signed earlier in the year",
        ];
        let mut words = Vec::new();
        for i in 0..16 {
            words.extend(run_words(
                left[i % 4],
                60.0,
                100.0 + i as f64 * 13.0,
                10.0,
                5.0,
            ));
            words.extend(run_words(
                right[i % 4],
                330.0,
                100.0 + i as f64 * 13.0,
                10.0,
                5.0,
            ));
        }
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let all = texts(&mut doc, &r).join("\n");
        assert!(
            all.contains("Revenue rose in the quarter as customers renewed"),
            "{all}"
        );
        assert!(!all.contains("as Costs"), "{all}");
    }

    #[test]
    fn a_heading_beside_a_column_keeps_its_own_size() {
        let mut words = run_words("Summary", 60.0, 100.0, 24.0, 12.0);
        words.extend(run_words("alpha beta gamma delta", 320.0, 100.0, 10.0, 5.0));
        words.extend(run_words("one two three four", 320.0, 113.0, 10.0, 5.0));
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let size_of = |c: &str| {
            doc.pages[0]
                .glyphs
                .iter()
                .find(|g| g.text == c)
                .map(|g| g.font_size)
                .unwrap()
        };
        assert!(size_of("S") > 2.0 * size_of("w"));
        let all = texts(&mut doc, &r).join("\n");
        assert!(all.contains("alpha beta gamma delta"), "{all}");
        assert!(all.contains("one two three four"), "{all}");
    }

    #[test]
    fn a_tall_box_does_not_chain_lines_together() {
        let mut words = Vec::new();
        for i in 0..5 {
            words.extend(run_words(
                "the line of body text",
                100.0,
                100.0 + i as f64 * 14.0,
                10.0,
                5.0,
            ));
        }
        // A sideways margin label boxed upright, and a merged two-line box.
        words.push(word("CONFIDENTIAL", [20.0, 95.0, 32.0, 170.0]));
        words.push(word("Total 42", [300.0, 98.0, 360.0, 126.0]));
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let all = texts(&mut doc, &r).join("\n");
        assert_eq!(all.matches("the line of body text").count(), 5, "{all}");
        assert!(
            all.contains("CONFIDENTIAL") && all.contains("Total 42"),
            "{all}"
        );
    }

    #[test]
    fn an_ink_line_without_ascenders_reads_at_its_neighbours_size() {
        let body = [
            "Payments were made in January as agreed by the parties",
            "our new cosmic room was a reason none can see now",
            "and every morning we review the open issues in a row",
        ];
        let mut words = Vec::new();
        for (i, l) in body.iter().enumerate() {
            let mut x = 60.0;
            for w in l.split(' ') {
                words.push(ink_box(w, x, 140.0 + i as f64 * 13.0, 10.0));
                x += w.len() as f64 * 5.0 + 3.0;
            }
        }
        // A sparse form too: single words per line.
        for (i, w) in ["Name", "Account", "Signature", "Phone"].iter().enumerate() {
            words.push(ink_box(w, 60.0, 300.0 + i as f64 * 13.0, 10.0));
        }
        let mut doc = blank_scan();
        inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        for g in doc.pages[0].glyphs.iter().filter(|g| g.bbox.is_some()) {
            assert!(
                (g.font_size - 10.0).abs() < 1.0,
                "{} at {}",
                g.text,
                g.font_size
            );
        }
    }

    #[test]
    fn a_reading_that_takes_nothing_leaves_the_page_as_it_was() {
        // An empty reading of a scan: still unread, still routed.
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(vec![]))]);
        assert_eq!(r[0].taken, 0);
        assert!(doc.pages[0].ocr.is_none());
        // A BrokenText page whose reading lands nowhere keeps its glyphs.
        let mut glyphs = native(&"x".repeat(150), 10.0, 100.0);
        for g in glyphs.iter_mut().take(40) {
            g.text.clear();
        }
        let mut doc = Document {
            pages: vec![page(glyphs, Vec::new())],
        };
        let far = page_units(vec![word("far", [5000.0, 5000.0, 5050.0, 5013.0])]);
        inject(&mut doc, &[(0, Crop::Page, far)]);
        assert_eq!(doc.pages[0].glyphs.len(), 150);
        assert!(doc.pages[0].ocr.is_none());
    }

    #[test]
    fn a_whole_page_reading_supersedes_region_readings_of_its_page() {
        let mut glyphs = Vec::new();
        for row in 0..12 {
            glyphs.extend(native("Body text line", 72.0, 80.0 + row as f64 * 14.0));
        }
        let note = BBox {
            x0: 60.0,
            y0: 600.0,
            x1: 560.0,
            y1: 700.0,
        };
        let mut doc = Document {
            pages: vec![page(
                glyphs,
                vec![ImagePlacement {
                    bbox: note,
                    texty: true,
                }],
            )],
        };
        let w = || {
            page_units(vec![word(
                "Evaluated against tests",
                [64.0, 640.0, 300.0, 653.0],
            )])
        };
        let r = inject(&mut doc, &[(0, Crop::Page, w()), (0, Crop::Region(0), w())]);
        assert_eq!(r[0].taken, 1);
        assert_eq!(r[1].refused, Some("superseded by the page's whole reading"));
    }

    #[test]
    fn many_words_inject_in_linear_time() {
        // Rows of ten words, set closer than their height, as a dense table
        // on a fine scan. Timed at two sizes rather than against a clock, so
        // a loaded machine slows both: four times the words must cost about
        // four times as long, where quadratic work would cost sixteen.
        let time = |n: usize| {
            let mut words = Vec::new();
            for i in 0..n {
                let (r, c) = (i / 10, i % 10);
                let y = 20.0 + r as f64 * 0.19;
                words.push(word(
                    "w0rd",
                    [20.0 + c as f64 * 58.0, y, 50.0 + c as f64 * 58.0, y + 3.0],
                ));
            }
            let mut doc = blank_scan();
            let t = std::time::Instant::now();
            inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
            t.elapsed().as_secs_f64()
        };
        let (small, large) = (time(10_000), time(40_000));
        assert!(
            large < 8.0 * small.max(0.01),
            "10k words {small:.3}s, 40k {large:.3}s"
        );
    }

    #[test]
    fn ocr_lines_under_a_paragraph_do_not_join_it() {
        // A text page with a screenshot pasted under its paragraph, the
        // screenshot's lines read by OCR at the paragraph's size and pitch,
        // one with an O/0 confusion. The PDF's own text stays the PDF's; the
        // characters read from pixels are said to be.
        let body = [
            "The statement below records each transfer made from the old",
            "account and the date on which it was booked, in the order of",
            "the ledger, so that a reader can follow every entry through",
            "the period it covers and up to its later reconciliation and",
        ];
        let mut glyphs = Vec::new();
        for (row, line) in body.iter().enumerate() {
            glyphs.extend(native(line, 72.0, 140.0 + row as f64 * 12.0));
        }
        for (i, g) in glyphs.iter_mut().enumerate() {
            g.draw_index = i;
        }
        let shot = BBox {
            x0: 66.0,
            y0: 182.0,
            x1: 540.0,
            y1: 300.0,
        };
        let mut doc = Document {
            pages: vec![page(
                glyphs,
                vec![ImagePlacement {
                    bbox: shot,
                    texty: true,
                }],
            )],
        };
        assert!(matches!(
            route::decide(&doc.pages[0]).0,
            Route::VlmRegions(_)
        ));
        // Detector boxes, 1.3 ems tall with the baseline 0.78 down: on the
        // paragraph's next two baselines.
        let line = |text: &str, baseline: f64| {
            word(
                text,
                [
                    72.0,
                    baseline - 0.78 * 13.0,
                    72.0 + text.len() as f64 * 6.0,
                    baseline + 0.22 * 13.0,
                ],
            )
        };
        let reading = page_units(vec![
            line(
                "Balance on 15-0CT-1997 was 1,25O.00 in the account named",
                188.0,
            ),
            line(
                "above, carried forward to the next statement of the period",
                200.0,
            ),
        ]);
        let r = inject(&mut doc, &[(0, Crop::Region(0), reading)]);
        assert_eq!(r[0].taken, 2, "{r:?}");
        let els = analysed(&mut doc, &r);
        let born = els
            .iter()
            .find(|e| e.text.contains("statement below"))
            .expect("the PDF's paragraph");
        assert_eq!(born.provenance, "rust", "{els:#?}");
        assert!(!born.text.contains("15-0CT"), "{els:#?}");
        let read = els
            .iter()
            .find(|e| e.text.contains("15-0CT-1997"))
            .expect("the screenshot's lines");
        assert_eq!(read.provenance, "ocr", "{els:#?}");
    }

    #[test]
    fn elements_holding_any_ocr_character_are_marked() {
        use fluree_doc_model::Element;
        // "Account": seven characters, ten units each.
        let injected = vec![Injected {
            chars: (0..7)
                .map(|i| BBox {
                    x0: 72.0 + i as f64 * 10.0,
                    y0: 100.0,
                    x1: 82.0 + i as f64 * 10.0,
                    y1: 113.0,
                })
                .collect(),
            ..Injected::new(0, Crop::Page)
        }];
        let at = |x0: f64, text: &str| Element {
            bbox: Some(BBox {
                x0,
                y0: 95.0,
                x1: x0 + 200.0,
                y1: 120.0,
            }),
            ..Element::new("doco:Paragraph", text, "rust", "layout")
        };
        // Over the same characters, most of its own from the text layer: a
        // table row mixing both, say. Still read from pixels in part.
        let mixed = at(60.0, "Account, then a long run of the page's own text");
        let mut els = vec![at(60.0, "Account"), at(300.0, "Elsewhere"), mixed];
        mark_provenance(&mut els, &injected);
        assert_eq!(els[0].provenance, "ocr");
        assert_eq!(els[1].provenance, "rust");
        assert_eq!(els[2].provenance, "ocr");
    }
}
