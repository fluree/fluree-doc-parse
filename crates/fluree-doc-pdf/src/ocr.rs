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

/// Height of a detector box, in ems of the text it holds, is
/// `BOX_BASE_EM + BOX_INK_SHARE * (up + down)`, `up` and `down` being how far
/// its letters reach, and taller for a box shorter than `SHORT_BOX_ASPECT`
/// times its thickness ([`box_height_em`]).
///
/// Detection boxes are the text line's ink plus a dilation (DB-style
/// detectors unclip their shrunk masks), so a box runs from just above its
/// tallest letters to just below its lowest, and grows with the ink by about
/// half as much. Measured on the built-in engine (PP-OCRv6) over synthetic
/// pages of known size — DejaVu Sans and Serif, Liberation Sans, Serif and
/// Mono from 8 to 18 pt, rendered at 100 to 300 dpi, 800 lines — a line with
/// capitals and descenders boxes 1.09 em in the median, one with no
/// descenders 0.96 to 1.0, one of x-height letters only 0.87. The body of
/// the generated scanned letters boxes 1.05 em. Taken as 1.3 em whatever its
/// letters, OCR text read a fifth small, and the lines of a paragraph or an
/// address, spaced as such, read as separate blocks; taken as one height
/// whatever its letters, a line with descenders read a tenth larger than the
/// lines around it and a heading without them a tenth smaller than its type.
///
/// Those were lines of prose. A box over a word or two is taller for its
/// type than one over a line: `Total`, `Reçu` or `Bill to:` alone on a line
/// of a letter in 10 to 12 pt boxes up to 0.2 em taller than the
/// paragraph's lines, and sized as they are read a fifth larger, as a
/// heading set larger does. A box under `SHORT_BOX_ASPECT` times as long as
/// it is thick is taken as `BOX_SHORT_EM` times the difference of their
/// inverses taller, 0.15 em at an aspect of 2; a longer one, a heading
/// of two words or a line, is sized as before. Measured on paragraphs set as
/// a letter sets them (the five fonts, 10 to 12 pt, 150 to 300 dpi, lines
/// 1.3 em apart, with short lines and headings set a fifth larger among
/// them), of the short lines at body size 39 in 594 read more than 15% over
/// their prose, against 318 sized as lines (443 with no mark counted
/// either), and of the headings 215 in 313, against 243. A term over every
/// box's aspect, longer ones included, set down the headings of two or three
/// words with the short lines (181 of 313 for 79 short lines, at the share
/// that fits the short lines best).
///
/// That margin is a paragraph's, its lines 1.3 em apart. With more room
/// above and below, the engine boxes a word alone on its line taller still,
/// 0.09 em over its letters at 1.3 em, 0.17 at 1.6, 0.24 at double spacing
/// (11 pt, four fonts, 150 to 300 dpi), and at double spacing such a word
/// at body size can read 15% over its prose, as a heading. The room around
/// a box does not say how much it grew: in the same room a word set larger
/// than the text around it grows far less (0.10 em for 14 pt among 11 at
/// double spacing). Sized by its room as well, the short headings of
/// double-spaced pages were set down with the words (285 of 336 headings
/// read, from 299, taking half the growth the words fit; 227 taking all of
/// it), and a word alone on a double-spaced line, set down, was read into
/// the paragraph above it.
const BOX_BASE_EM: f64 = 0.62;
const BOX_INK_SHARE: f64 = 0.5;
const BOX_SHORT_EM: f64 = 0.5;
const SHORT_BOX_ASPECT: f64 = 5.0;

/// Where a letter's ink reaches, in ems from the baseline: capitals, digits
/// and ascenders up to about 0.72, the x-height 0.5, descenders down 0.22.
/// An engine that boxes words by their ink (Tesseract, hOCR, ALTO) gives a
/// box exactly that tall, so `our` is half the height of `Payments`, and only
/// the letters inside say what size the text is.
///
/// A mark over a small letter (`é`, `ü`, `ñ`) reaches about a capital's
/// height, as the dot over `i` does; one over a capital (`É`, `Ü`) about
/// `ACCENTED_CAP_EM` (on a detector's box over a line, half as far above
/// the capitals: [`box_height_em`]); a cedilla or an ogonek (`ç`, `ą`) as
/// deep as a descender. Brackets and `|` reach from a capital's height to a
/// descender's depth, a comma, a semicolon and the stroke of `$` hang
/// `TAIL_EM` below the baseline, and quotes and `/` rise to a capital's
/// height. Calibrated on Latin script: other scripts are sized as the
/// letters' case and these marks say.
const CAP_HEIGHT_EM: f64 = 0.72;
const X_HEIGHT_EM: f64 = 0.5;
const DESCENDER_EM: f64 = 0.22;
const ACCENTED_CAP_EM: f64 = 0.9;
const TAIL_EM: f64 = 0.13;
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
    /// Mean and lowest confidence of the words taken, where the engine
    /// gave one.
    pub mean_conf: Option<f64>,
    pub min_conf: Option<f64>,
    /// The engine the reading names, if it names one.
    pub engine: Option<String>,
    /// Skew of the reading's level lines, degrees counter-clockwise, as
    /// estimated and corrected for.
    pub skew_deg: f64,
    /// The box of every character taken, in the display frame (the
    /// output's). What [`mark_provenance`] reads.
    pub chars: Vec<BBox>,
    /// Words taken in each routed region, by index: `(region, words)`.
    pub regions_taken: Vec<(usize, usize)>,
    /// The regions it may read that are pasted into their page
    /// ([`route::pasted`]: the picture they are part of covers under half
    /// of it), as a screenshot pasted into a page of text is, in the
    /// display frame: a heading read there is the screen's
    /// ([`mark_provenance`]). The one a region's reading reads, or those
    /// among the regions a whole reading of a page routed in regions gives
    /// only the words of, or the pictures of a page read whole as near
    /// blank; none for any other page read whole, and none for a region or
    /// picture that, with the images it abuts, covers half its page or
    /// more, a scan with a stamped line of text of its own, whole or stored
    /// in strips.
    pub pasted: Vec<BBox>,
    /// The regions the reading was to read, in the display frame: the one a
    /// region's reading read, or every region an engine reads on a page a
    /// whole reading gives only the regions of ([`route::ocr_regions`]: the
    /// routed ones, then those no model is sent); none for a page read
    /// whole. Each is the box a model's reading of the same region is
    /// spliced in with, where a model reads it.
    pub regions: Vec<BBox>,
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
            min_conf: None,
            engine: None,
            skew_deg: 0.0,
            chars: Vec::new(),
            regions_taken: Vec::new(),
            pasted: Vec::new(),
            regions: Vec::new(),
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
    // Each page's routing is kept as drawn, before any word is added: what
    // reads the page after its analysis reads that routing. A reading's
    // glyphs are counted in the page's signals, and a few words of a region
    // on a short page whose font resolves badly would route it `BrokenText`
    // and leave its other regions without an anchor or a report.
    for p in doc.pages.iter_mut() {
        p.keep_routing();
    }
    let routes: Vec<Route> = doc
        .pages
        .iter()
        .map(|p| p.routing().route.clone())
        .collect();
    // The regions an engine may fill: the routed ones, then those the router
    // sends no model (see `route::ocr_regions`).
    let regions: Vec<Vec<BBox>> = doc
        .pages
        .iter()
        .zip(&routes)
        .map(|(p, r)| route::ocr_regions_for(p, r))
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
    // What an engine reads in a region's crop is held to the noise rule
    // ([`noise`]), a net behind the pixel probe that sent it there: a
    // capture the probe took for text may still be a photograph or a
    // gradient, whose reading is strokes. Its words are set aside, and the
    // region stays unread. A reading of the whole page is not judged: a
    // scan's lines are words, and its regions are read with it.
    let set_aside: Vec<Vec<usize>> = readings
        .iter()
        .zip(&judged)
        .map(|((_, crop, reading), t)| match (crop, t) {
            (Crop::Region(_), Ok(t)) => noise_in_regions(&doc.pages[t.pi], t, reading),
            _ => Vec::new(),
        })
        .collect();
    let readings: Vec<(usize, Crop, std::borrow::Cow<WordReading>)> = readings
        .iter()
        .zip(&set_aside)
        .map(|((page, crop, reading), gone)| {
            let kept = if gone.is_empty() {
                std::borrow::Cow::Borrowed(reading)
            } else {
                let mut kept = reading.clone();
                kept.words = (reading.words.iter().enumerate())
                    .filter(|(i, _)| !gone.contains(i))
                    .map(|(_, w)| w.clone())
                    .collect();
                std::borrow::Cow::Owned(kept)
            };
            (*page, *crop, kept)
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
    // The size detector boxes say is snapped to a median of the boxes the
    // same readings give: their page's, or the document's on a page of too
    // few lines to have one of its own (see `PAGE_MEDIAN_MIN_LINES`).
    let (pairs, sizes): (Vec<_>, Vec<_>) = readings
        .iter()
        .zip(&judged)
        .filter(|((page, crop, _), _)| *crop == Crop::Page || !read_whole_too.contains(page))
        .filter_map(|((_, _, reading), target)| {
            let t = target.as_ref().ok()?;
            place(&doc.pages[t.pi], t.shown, reading)
                .ok()
                .map(|placed| (t.pi, placed))
        })
        .map(|(pi, placed)| {
            let words: Vec<Placed> = placed.into_iter().flatten().collect();
            // A box is as thick whichever way its text runs: a page that is
            // to be turned for its reading sizes its words all the same. Each
            // box weighs as many letters as it holds: a box over one figure
            // or one word is taller for its length than one over a line of
            // prose, and the cells of a table outnumber the lines of a
            // paragraph under it.
            let sizes: Vec<(usize, f64, f64)> = words
                .iter()
                .filter(|w| ink_extent(&w.text).is_some())
                .map(|w| {
                    let letters = w.text.chars().filter(|c| c.is_alphanumeric()).count();
                    let size = dist(w.quad[0], w.quad[3]) / box_height_em(&w.text, aspect(&w.quad));
                    (pi, size, letters as f64)
                })
                .collect();
            let sheet = sheet_angle(&words);
            let level: Vec<Placed> = words
                .into_iter()
                .filter(|w| normalise_deg(w.angle - sheet).abs() < LEVEL_BELOW_DEG)
                .collect();
            let tan = skew(&level).to_radians().tan();
            (ink_pairs(&level, tan), sizes)
        })
        .unzip();
    let detected = !boxes_ink(pairs.into_iter());
    let sizes: Vec<(usize, f64, f64)> = sizes.into_iter().flatten().collect();
    let document_median = weighted_median(sizes.iter().map(|&(_, s, w)| (s, w)).collect());
    let page_boxes = |pi: usize| -> Boxes {
        if !detected {
            return Boxes::Ink;
        }
        let own: Vec<(f64, f64)> = sizes
            .iter()
            .filter(|&&(p, _, _)| p == pi)
            .map(|&(_, s, w)| (s, w))
            .collect();
        Boxes::Detected {
            median: if own.len() >= PAGE_MEDIAN_MIN_LINES {
                weighted_median(own)
            } else {
                document_median
            },
        }
    };
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
            // Decided by the regions the words land in, not by the crop read:
            // a whole reading of a page routed in regions lands its words in
            // the same regions a region's reading would. A page near blank
            // but for its pictures is read whole, and its pictures are where
            // the words land: one under half the page is a screen pasted
            // there, as on a page of text.
            let p = &doc.pages[t.pi];
            let landing: Vec<BBox> = match (&t.area, &routes[t.pi]) {
                (Area::Page, Route::Vlm(route::Reason::NearBlank)) => {
                    p.images.iter().map(|im| im.bbox).collect()
                }
                (Area::Page, _) => Vec::new(),
                (Area::Regions(within), _) => within.iter().map(|(_, b)| *b).collect(),
            };
            let pasted = (landing.iter().zip(route::pasted_set(p, &landing)))
                .filter(|(_, pasted)| *pasted)
                .map(|(b, _)| p.to_display(*b))
                .collect();
            out[i] = Some(Injected {
                pasted,
                ..inject_one(&mut doc.pages[t.pi], *crop, t, page_boxes(t.pi), reading)
            });
        }
    }

    out.into_iter()
        .zip(&set_aside)
        .map(|(r, gone)| {
            let mut r = r.expect("every reading judged");
            if r.refused.is_none() {
                r.dropped += gone.len();
            }
            r
        })
        .collect()
}

/// Mean confidence of its words under which an OCR reading of a region or
/// of a picture is noise whatever its words look like: a net behind the
/// pixel probe, for a photograph or a gradient it took for text.
///
/// Measured with the built-in engine on synthetic pictures: a field of grass
/// read one "word" at 0.11 to 0.29, a photograph's one at 0.19, and a field
/// of strokes 60 at 0.499. Real text read 0.66 at the least (a date in a
/// corner of a landscape) and otherwise 0.87 to 1.0, so a mean this low is
/// noise however its words look.
pub const MIN_PICTURE_CONFIDENCE: f64 = 0.5;

/// Mean confidence under which a reading of such pixels made mostly of
/// fragments ([`FRAGMENT_SHARE`]) is noise.
///
/// Confidence alone does not tell noise from text: dense foliage read 16
/// to 188 "words" at a mean of 0.57 to 0.63, and a texture of blobs 29 at
/// 0.52, over the date in a landscape read at 0.66. What tells them apart
/// is what the words are: in each reading of noise measured, 93 % of them
/// or more were fragments of one or two letters or figures (`1`, `I`, `J`,
/// `米`, `-----`), where the date was one fragment in two and the captures,
/// signs, charts and stamps read one in six at most. A chart's axis or a
/// code (`A1`) is fragments too; the text measured read at 0.87 to 1.0 in
/// the mean, so over this mean fragments are taken for text. So is CJK
/// signage read as short lines (`出口`, two characters a line), and the
/// short tokens of a reading boxed word by word (a sidecar from an engine
/// that boxes words, where a line of prose holds `a`, `of`, `to`) count
/// as fragments more often than the built-in engine's lines do: read with
/// a mean under this, such text can be set aside.
pub const MIN_FRAGMENTS_CONFIDENCE: f64 = 0.8;

/// Share of a reading's words over which they are mostly fragments
/// ([`MIN_FRAGMENTS_CONFIDENCE`]): words of two letters or figures at most.
const FRAGMENT_SHARE: f64 = 2.0 / 3.0;

/// Share of the picture read over which one word's box makes a reading of
/// it noise, when that word is a fragment or is read under
/// [`MIN_FRAGMENTS_CONFIDENCE`].
///
/// The built-in engine boxes the one "word" it reads in a photograph or a
/// gradient over all of the picture, and some of those it scores over
/// [`MIN_PICTURE_CONFIDENCE`] (a grey gradient: a `7` at 0.66). The text of
/// a capture or a chart boxes 0.02 of its picture at most, a large word or
/// a large number set in it 0.61 to 0.88. A crop of one line of text, a
/// few pixels of margin around it, boxes its line over all of it too, and
/// so does a sign or a stamp whose one word fills the picture (`OPEN`,
/// `PAID`): read surely (0.99, 1.0), words and not fragments, they are
/// text, and are kept.
const NOISE_BOX_SHARE: f64 = 0.9;

/// Whether `words`, read in a picture of `area` (square PDF units), are an
/// engine's noise rather than text: their mean confidence is under
/// [`MIN_PICTURE_CONFIDENCE`], or under [`MIN_FRAGMENTS_CONFIDENCE`] with
/// more than [`FRAGMENT_SHARE`] of them fragments, or one of them is boxed
/// over [`NOISE_BOX_SHARE`] of the picture and is a fragment or is read
/// under [`MIN_FRAGMENTS_CONFIDENCE`]. A reading with no confidence (none
/// given, or scored outside 0-1) is judged by its boxes alone, a word with
/// no confidence as one read unsurely. No words are no noise.
fn noise(words: &[&Placed], area: f64) -> bool {
    let confs: Vec<f64> = words.iter().filter_map(|w| w.conf).collect();
    let fragments = words.iter().filter(|w| fragment(&w.text)).count();
    let fragmented = fragments as f64 > FRAGMENT_SHARE * words.len() as f64;
    let unsure = !confs.is_empty() && {
        let mean = confs.iter().sum::<f64>() / (confs.len() as f64);
        mean < MIN_PICTURE_CONFIDENCE || (fragmented && mean < MIN_FRAGMENTS_CONFIDENCE)
    };
    let over_all = words.iter().any(|w| {
        let (bx, by) = quad_bounds(&w.quad);
        let unsure = fragment(&w.text) || w.conf.is_none_or(|c| c < MIN_FRAGMENTS_CONFIDENCE);
        unsure && (bx.1 - bx.0) * (by.1 - by.0) >= NOISE_BOX_SHARE * area
    });
    unsure || over_all
}

/// A word of two letters or figures at most, other marks not counted: the
/// stroke an engine reads in a photograph's texture (`1`, `I7`, `-----`).
fn fragment(text: &str) -> bool {
    text.chars().filter(|c| c.is_alphanumeric()).count() <= 2
}

/// Whether a reading of the one page of `page`, a picture read on its own,
/// is noise ([`MIN_PICTURE_CONFIDENCE`], [`MIN_FRAGMENTS_CONFIDENCE`],
/// [`NOISE_BOX_SHARE`]): a photograph or a chart read for text it does not
/// hold. The rule an OCR reading of a region of a PDF page is held to, for
/// the pictures of a DOCX, an XLSX or an e-mail: a net behind the pixel
/// probe, which has taken the picture for text before it is read. Not for
/// an image read as a file, read as a scan is. A reading that cannot be
/// placed is not noise: [`inject`] says why it is refused. Nor is one it refuses for
/// where its words land, none of them on the picture or under half of
/// them: that is decided first, so a reading of the wrong origin or scale
/// is listed as failed, not set aside as one of a picture with no text.
pub fn is_noise(page: &Page, reading: &WordReading) -> bool {
    let Ok(placed) = place(page, None, reading) else {
        return false;
    };
    // As `inject_one` judges a whole-page reading of a page with no text of
    // its own, where each word that lands on the page is taken.
    let on = placed
        .iter()
        .flatten()
        .filter(|w| lands_on(page, None, w))
        .count();
    if on == 0 || on * 2 < placed.len() {
        return false;
    }
    let words: Vec<&Placed> = placed.iter().flatten().collect();
    noise(&words, page.width * page.height)
}

/// The words of `reading`, by their index among its words, that land in a
/// region it may fill and are noise there ([`noise`]): each region judged
/// by the words that land in it, as [`add_words`] lands them.
fn noise_in_regions(p: &Page, t: &Target, reading: &WordReading) -> Vec<usize> {
    let Area::Regions(within) = &t.area else {
        return Vec::new();
    };
    let Ok(placed) = place(p, t.shown, reading) else {
        return Vec::new();
    };
    // `place` skips the blank words: its slots are the others, in order.
    let index = (reading.words.iter().enumerate())
        .filter(|(_, w)| !w.text.trim().is_empty())
        .map(|(i, _)| i);
    let landed: Vec<(usize, usize, &Placed)> = placed
        .iter()
        .zip(index)
        .filter_map(|(w, k)| {
            let w = w.as_ref()?;
            let (bx, by) = quad_bounds(&w.quad);
            let (cx, cy) = ((bx.0 + bx.1) * 0.5, (by.0 + by.1) * 0.5);
            let (region, _) = within.iter().find(|(_, r)| r.contains(cx, cy))?;
            Some((*region, k, w))
        })
        .collect();
    let mut gone = Vec::new();
    for (i, region) in within {
        let words: Vec<&Placed> = landed
            .iter()
            .filter(|(r, _, _)| r == i)
            .map(|(_, _, w)| *w)
            .collect();
        if noise(&words, region.width() * region.height()) {
            gone.extend(landed.iter().filter(|(r, _, _)| r == i).map(|(_, k, _)| *k));
        }
    }
    gone
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

/// What a document's readings box: each word's ink, or the line a
/// detector found, with the median size of the boxes on the page.
#[derive(Clone, Copy)]
enum Boxes {
    Ink,
    Detected { median: Option<f64> },
}

/// Lay one reading the router let in on its page.
fn inject_one(
    p: &mut Page,
    crop: Crop,
    t: &Target,
    boxes: Boxes,
    reading: &WordReading,
) -> Injected {
    let page = p.index;
    let mut placed = match place(p, t.shown, reading) {
        Ok(placed) => placed,
        Err(why) => return refused(page, crop, why),
    };
    // A sheet fed upside down or sideways, read by an engine that says so.
    // Line assembly reads a line rightwards, up or down, never right to
    // left, and orders lines down the page, so the page is read in the
    // frame where most of its words run upright, as a page drawn turned is
    // when it is extracted. Only a page whose text is all pixels can be
    // turned for its reading.
    let mut turned = page_turn(&placed);
    if let Some(turn) = turned {
        // A page with text of its own (a fax header, a stamped number) would
        // have that text turned with it, and read backwards or sideways.
        let own_text = p.glyphs.iter().any(|g| !g.text.trim().is_empty());
        if !matches!(t.area, Area::Page) || (own_text && !t.replaces) {
            if turn == 180 {
                return refused(
                    page,
                    crop,
                    "the reading runs upside down over a page of text",
                );
            }
            // Sideways words are laid along their angle where they are.
            turned = None;
        } else {
            let (w, h) = (p.width, p.height);
            crate::extract::turn_page(p, turn);
            for word in placed.iter_mut().flatten() {
                word.quad = word.quad.map(|q| crate::extract::turn_point(w, h, turn, q));
                word.angle = normalise_deg(word.angle + f64::from(turn));
            }
        }
    }
    let first_glyph = match &p.ocr {
        Some(o) => o.first_glyph,
        None => p.glyphs.iter().map(|g| g.draw_index + 1).max().unwrap_or(0),
    };
    let offered = placed.len();
    let on_crop = (placed.iter().flatten())
        .filter(|w| lands_on(p, t.shown, w))
        .count();
    let added_from = p.glyphs.iter().map(|g| g.draw_index + 1).max().unwrap_or(0);
    let (mut result, regions_read) =
        add_words(p, crop, &t.area, first_glyph, t.replaces, boxes, placed);
    result.engine = reading.engine.clone();
    if let Area::Regions(within) = &t.area {
        result.regions = within.iter().map(|(_, r)| p.to_display(*r)).collect();
    }
    // A whole-page reading most of whose words land nowhere is not a
    // reading of this page: a stray word must not mark a scan as read, or
    // take the place of a text layer.
    // A page turned for a reading it then does not take is turned back.
    let unturn = |p: &mut Page| {
        if let Some(turn) = turned {
            crate::extract::turn_page(p, 360 - turn);
        }
    };
    if matches!(t.area, Area::Page) && result.taken > 0 && result.taken * 2 < offered {
        p.glyphs.retain(|g| g.draw_index < added_from);
        unturn(p);
        return refused(page, crop, "most of the reading could not be placed");
    }
    // Nor is one none of whose words land on what it read, as an origin or
    // a scale not the crop's puts them: the page, or the region with its
    // margin. Set aside, not taken as a reading that came back blank. Words
    // on the page but over its text layer, or a whole-page reading's words
    // outside the regions it may land in, are not this: the page is read,
    // and holds nothing more.
    if offered > 0 && on_crop == 0 {
        unturn(p);
        return refused(page, crop, "none of the reading could be placed");
    }
    // Nor, as a whole-page reading is, a region's most of whose words land
    // off its crop: at twice the crop's scale, the words of its top left
    // corner land on it and the rest off. Counted on the crop, its margin
    // included, not on the region: a word read in the margin, of the text
    // beside the image, is dropped but was placed where it was read.
    if !matches!(t.area, Area::Page) && on_crop * 2 < offered {
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
    // A score outside 0-1 says the reading is scored on another scale
    // (Tesseract's 0-100, not divided), where its other scores mean
    // something else too: a 1 out of 100 is not near certain. The reading
    // then has no confidence, rather than one made of its words that
    // happen to fall in 0-1.
    let scored = reading.scores_out_of_range() == 0;
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
                conf: w.conf.filter(|_| scored),
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

/// The turn, in degrees counter-clockwise, that sets a reading's text
/// upright: 180 when it runs upside down, 90 when it runs down the page, 270
/// when it runs up it.
///
/// Text is weighed by its characters, so the cells of a table count for what
/// they hold, not as many lines, and words of one or two characters not at
/// all: a digit's box is about as tall as wide, and an engine states any
/// angle for it. The page turns a quarter for [`TURN_SHARE`] of its text at
/// one angle, and only while less than [`KEEP_LEVEL_SHARE`] of it is level: a
/// page with upright text of its own keeps its frame, and a table set
/// sideways on it is laid along its angle where it is. It turns a half for
/// [`TURN_SHARE`] of its text upside down, or for more upside down than level
/// when the two make [`KEEP_LEVEL_SHARE`] of it: a sheet fed upside down
/// under an upright fax header, or with a table set sideways on it. None
/// otherwise.
fn page_turn(placed: &[Option<Placed>]) -> Option<i32> {
    let weight = |w: &Placed| {
        let n = w.text.chars().filter(|c| !c.is_whitespace()).count();
        if n >= 3 {
            n as f64
        } else {
            0.0
        }
    };
    let words = || placed.iter().flatten();
    let at = |deg: f64| -> f64 {
        words()
            .filter(|w| normalise_deg(w.angle - deg).abs() < 2.0 * LEVEL_BELOW_DEG)
            .map(weight)
            .sum()
    };
    let total: f64 = words().map(weight).sum();
    if total == 0.0 {
        return None;
    }
    // Words upside down cannot be laid along their angle where they are:
    // read in the page's frame, their lines come out last first. The page
    // turns for them once they outweigh its level text, and its level text
    // is laid upside down in turn. They are weighed with the level text
    // against what runs sideways, as level text alone keeps the frame: a
    // sheet fed upside down with a table set sideways on it is a page with
    // text of its own, turned a half.
    let (level, upside_down) = (at(0.0), at(180.0));
    if upside_down >= TURN_SHARE * total
        || (upside_down > level && upside_down + level >= KEEP_LEVEL_SHARE * total)
    {
        return Some(180);
    }
    if level >= KEEP_LEVEL_SHARE * total {
        return None;
    }
    [(-90.0, 90), (90.0, 270)]
        .into_iter()
        .find(|&(deg, _)| at(deg) >= TURN_SHARE * total)
        .map(|(_, turn)| turn)
}

/// Share of a reading's text, by characters, that must run at one angle for
/// its page to be read turned.
const TURN_SHARE: f64 = 2.0 / 3.0;

/// Share of a reading's text, by characters, set level that keeps its page
/// in its own frame, however much of the rest runs sideways; text upside
/// down that outweighs the level text makes up that share with it, and the
/// page is turned a half. A page scanned sideways reads a few hundredths
/// level at most; a caption line over a table set sideways is about a fifth
/// of the page's text, and the page reads better turned, the table kept a
/// table; four lines of text and up keep the frame.
const KEEP_LEVEL_SHARE: f64 = 0.25;

/// An angle in degrees, brought into (-180, 180].
fn normalise_deg(a: f64) -> f64 {
    let r = a.rem_euclid(360.0);
    if r > 180.0 {
        r - 360.0
    } else {
        r
    }
}

/// Whether a placed word lands on the image read: its box's center falls
/// on the crop `shown` is ([`crop_rect`]), the whole page as [`add_words`]
/// judges a word of a whole-page reading, or a region with its margin.
fn lands_on(p: &Page, shown: Option<BBox>, w: &Placed) -> bool {
    let (bx, by) = quad_bounds(&w.quad);
    let (cx, cy) = ((bx.0 + bx.1) * 0.5, (by.0 + by.1) * 0.5);
    p.from_display(crop_rect(p, shown)).contains(cx, cy)
}

/// Add a reading's placed words to the page; returns what was taken and
/// the routed regions that took at least one word.
fn add_words(
    p: &mut Page,
    crop: Crop,
    area: &Area,
    first_glyph: usize,
    replaces: bool,
    boxes: Boxes,
    placed: Vec<Option<Placed>>,
) -> (Injected, Vec<usize>) {
    let ink = matches!(boxes, Boxes::Ink);
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
    result.min_conf = confs.iter().copied().reduce(f64::min);
    result.taken = kept.len();

    let mut next = p.glyphs.iter().map(|g| g.draw_index + 1).max().unwrap_or(0);
    let sheet = sheet_angle(&kept);
    let (level, turned): (Vec<Placed>, Vec<Placed>) = kept
        .into_iter()
        .partition(|w| normalise_deg(w.angle - sheet).abs() < LEVEL_BELOW_DEG);
    for w in &turned {
        let font = (dist(w.quad[0], w.quad[3]) / box_height_em(&w.text, aspect(&w.quad))) as f32;
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
    if let Boxes::Detected { median: Some(body) } = boxes {
        snap_sizes(&mut runs, body);
    }
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

/// The value below which half the weight of `(value, weight)` lies; `None`
/// with no weight at all.
fn weighted_median(mut v: Vec<(f64, f64)>) -> Option<f64> {
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let half = v.iter().map(|p| p.1).sum::<f64>() / 2.0;
    let mut acc = 0.0;
    v.into_iter()
        .find(|&(_, w)| {
            acc += w;
            w > 0.0 && acc >= half
        })
        .map(|(x, _)| x)
}

/// How far a word's ink reaches above and below its baseline, in ems, from
/// the letters it holds and the marks around them; `None` for a word of
/// punctuation alone.
fn ink_extent(text: &str) -> Option<(f64, f64)> {
    use unicode_normalization::char::canonical_combining_class;
    use unicode_normalization::UnicodeNormalization;
    let mut up: Option<f64> = None;
    let mut down: f64 = 0.0;
    // Punctuation reaches as far as its letters' ink, but makes no word.
    let (mut marks_up, mut marks_down) = (0.0_f64, 0.0_f64);
    // How high the letter a combining mark sits on reaches.
    let mut base: Option<f64> = None;
    for c in text.nfd() {
        match canonical_combining_class(c) {
            0 => {}
            // Below: attached (cedilla, ogonek), or apart (dot, comma).
            202 | 218 | 220 | 222 | 233 => {
                if base.is_some() {
                    down = DESCENDER_EM;
                }
                continue;
            }
            // Above, attached or apart.
            214 | 216 | 228 | 230 | 232 | 234 => {
                if let Some(b) = base {
                    let over = if b < CAP_HEIGHT_EM {
                        CAP_HEIGHT_EM
                    } else {
                        ACCENTED_CAP_EM.max(b)
                    };
                    up = Some(up.map_or(over, |u: f64| u.max(over)));
                }
                continue;
            }
            _ => continue,
        }
        let reach = if is_cjk(c) {
            CJK_HEIGHT_EM
        } else if c.is_uppercase() || c.is_ascii_digit() || "bdfhklt&$%#@ßđħł".contains(c) {
            CAP_HEIGHT_EM
        } else if c.is_alphabetic() {
            X_HEIGHT_EM
        } else {
            let (u, d) = match c {
                '(' | ')' | '[' | ']' | '{' | '}' | '|' => (CAP_HEIGHT_EM, DESCENDER_EM),
                ',' | ';' => (0.0, TAIL_EM),
                '/' | '\\' | '!' | '?' | '"' | '\'' | '‘' | '’' | '“' | '”' => {
                    (CAP_HEIGHT_EM, 0.0)
                }
                _ => (0.0, 0.0),
            };
            marks_up = marks_up.max(u);
            marks_down = marks_down.max(d);
            base = None;
            continue;
        };
        base = Some(reach);
        up = Some(up.map_or(reach, |u: f64| u.max(reach)));
        if "gjpqy".contains(c) {
            down = DESCENDER_EM;
        } else if c == '$' {
            down = down.max(TAIL_EM);
        }
    }
    up.map(|u| (u.max(marks_up), down.max(marks_down)))
}

/// How tall a detector box over `text` is, in ems of its type
/// ([`BOX_BASE_EM`]): from the letters it holds, the dots over `i` and `j`
/// reaching about as high as a capital, as the box does over them, and from
/// its `aspect`, its length over its thickness. A box of punctuation alone
/// is taken as one over a line of prose.
///
/// A mark over a capital stands over one letter. Over a word it is a share
/// of the ink the detector boxes, and the box rises with it; over a line it
/// is a speck, and a line opening on `À` or `É` boxes only about half as
/// much taller than the same line without the mark (0.03 to 0.07 em over
/// four fonts at 150 to 300 dpi, against the 0.09 a word's gives). A box of
/// [`SHORT_BOX_ASPECT`] and over reaches half as far above its capitals.
fn box_height_em(text: &str, aspect: f64) -> f64 {
    let (up, down) = ink_extent(text).map_or((CAP_HEIGHT_EM, DESCENDER_EM), |(up, down)| {
        if text.contains(['i', 'j']) {
            (up.max(CAP_HEIGHT_EM), down)
        } else {
            (up, down)
        }
    });
    // No letter reaches past a capital or a CJK character: only a mark does.
    let up = if aspect >= SHORT_BOX_ASPECT && up > CAP_HEIGHT_EM.max(CJK_HEIGHT_EM) {
        (up + CAP_HEIGHT_EM) / 2.0
    } else {
        up
    };
    let short = (1.0 / aspect.max(1.0) - 1.0 / SHORT_BOX_ASPECT).max(0.0);
    BOX_BASE_EM + BOX_INK_SHARE * (up + down) + BOX_SHORT_EM * short
}

/// A box's length over its thickness, from its corners (top left, top
/// right, bottom right, bottom left).
fn aspect(quad: &[(f64, f64); 4]) -> f64 {
    dist(quad[0], quad[1]) / dist(quad[0], quad[3])
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
/// words says. A detector box is as tall as its letters make it
/// ([`box_height_em`]) with its baseline
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
                    fonts.push(h / box_height_em(&w.word.text, (w.x1 - w.x0) / h));
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

/// Widest difference from its page's median size, as a share of it, that a
/// run boxed by a detector is put down to its box rather than to its type.
///
/// A box is sized by its letters ([`box_height_em`]), so what sets a line
/// of the body apart from another is the engine's noise: a pixel or two of
/// the box, a font that boxes its descenders deeper or shallower than the
/// fit. On the synthetic lines `BOX_BASE_EM` was measured on (an em of 16 px
/// and up), 94% of the lines size within 10% of their page's median, 3%
/// over it; sized by one height whatever their letters, 75% did, and 15%
/// over it. A heading set larger is 15% over its body and up (12 pt over 10
/// is 20%), and its box shows it whatever its letters: a 14 pt `Exclusions`
/// over 11.5 pt prose sizes 21% over its page's median at 200 dpi. Snapped
/// within 12%, a 15 pt `Renewal terms` on a page of 12 pt prose was set to
/// it; within 8%, the lines of a sparse cover page read as headings.
const SNAP_SIZE_SHARE: f64 = 0.10;

/// Lines (detector boxes) a page needs to be sized by its own median; a page
/// of fewer is sized by the document's.
///
/// The pages of one document are read one by one and need not share a body:
/// a page set in 11.5 pt scanned with two in 10 is 15% over the document's
/// median, and snapped to it, the lines its reading boxed within the band
/// were set down to it and the others not, so its paragraphs came apart
/// line by line. Each page is also read at its own scale: four copies of one
/// form read in four orientations box their text from 10 to 11.6 pt, and a
/// page snapped to its own median keeps its reading's. But a median is the
/// body's only where the body holds most of the page's letters. Eight lines
/// hold a short paragraph under a heading or two; a page of fewer (a cover,
/// a signature, a slip) may have no body at all, and is sized with the
/// document.
const PAGE_MEDIAN_MIN_LINES: usize = 8;

/// Set the runs within [`SNAP_SIZE_SHARE`] of their page's median size
/// `body` to that size: what sets them apart is their letters and their
/// reading, not their type.
fn snap_sizes(runs: &mut [Run], body: f64) {
    for r in runs {
        if (r.font - body).abs() <= SNAP_SIZE_SHARE * body {
            r.font = body;
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
/// Some candidate always qualifies: the farthest blocker's far side. A
/// candidate `b ± d` is `d` from `b` only to rounding (`110.4 - 10.5315` is
/// 10.531499999999994 below it), so `d` is compared with that much slack: a
/// strict test refused every candidate and left the stamp where it was.
fn clear_baseline(at: f64, blockers: &[(f64, f64)]) -> f64 {
    let clear = |y: f64| {
        blockers
            .iter()
            .all(|&(b, d)| (y - b).abs() >= d - 1e-9 * d.max(b.abs()))
    };
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

/// Per page, `None` to read it whole, else its regions by crop tag: the
/// shape of the model tier's crop jobs.
pub type OcrJobs = Vec<(usize, Option<Vec<(String, BBox)>>)>;

/// The crops an OCR engine reads: each page whose text is pixels, whole,
/// and on every other page its unread raster regions
/// ([`route::ocr_regions`]), those no model is sent included. Limited to
/// `only`'s pages when given.
///
/// At most `max_crops` crops (`0` for no limit), the page order kept, and
/// how many were left out. Past the limit the crops that lose the most if
/// left unread are kept first: a page read whole, whose text is all pixels;
/// then a region the router sends a reader; then a region too small for a
/// model's call. Within each kind, the earlier pages first.
pub fn ocr_jobs(doc: &Document, only: Option<&[usize]>, max_crops: usize) -> (OcrJobs, usize) {
    // A region showing what an earlier one shows is not read again
    // ([`copies`]): it takes the earlier one's reading, and counts against
    // the limit only through it.
    let copied: std::collections::HashMap<(usize, usize), (usize, usize)> =
        copies(doc, only).into_iter().collect();
    // Every crop, in page order, with its kind: 0 a page read whole, 1 a
    // routed region, 2 a small one.
    type Ranked = (u8, usize, Option<(usize, BBox)>);
    let mut crops: Vec<Ranked> = Vec::new();
    for p in &doc.pages {
        if only.is_some_and(|keep| !keep.contains(&p.index)) {
            continue;
        }
        let routed = match &p.routing().route {
            Route::Vlm(_) => {
                crops.push((0, p.index, None));
                continue;
            }
            Route::VlmRegions(r) => r.len(),
            Route::Deterministic => 0,
        };
        for (i, b) in route::ocr_regions(p).into_iter().enumerate() {
            if copied.contains_key(&(p.index, i)) {
                continue;
            }
            let kind = if i < routed { 1 } else { 2 };
            crops.push((kind, p.index, Some((i, b))));
        }
    }
    let mut dropped = 0;
    if max_crops > 0 && crops.len() > max_crops {
        let mut order: Vec<usize> = (0..crops.len()).collect();
        order.sort_by_key(|&i| (crops[i].0, i));
        let mut keep = vec![false; crops.len()];
        for &i in &order[..max_crops] {
            keep[i] = true;
        }
        dropped = crops.len() - max_crops;
        let mut k = keep.into_iter();
        crops.retain(|_| k.next() == Some(true));
        // The copies of a region left unread are left unread with it.
        let read: std::collections::HashSet<(usize, usize)> = (crops.iter())
            .filter_map(|(_, page, c)| c.as_ref().map(|(i, _)| (*page, *i)))
            .collect();
        dropped += copied.values().filter(|of| !read.contains(of)).count();
    }
    let mut jobs: OcrJobs = Vec::new();
    for (_, page, crop) in crops {
        let crop = crop.map(|(i, b)| (Crop::Region(i).tag(), b));
        match (crop, jobs.last_mut()) {
            (None, _) => jobs.push((page, None)),
            (Some(c), Some((last, Some(list)))) if *last == page => list.push(c),
            (Some(c), _) => jobs.push((page, Some(vec![c]))),
        }
    }
    (jobs, dropped)
}

/// A region an OCR engine reads that shows what an earlier one shows, by
/// `(page, region)`: the copy, then the region it is a copy of.
pub type RegionCopy = ((usize, usize), (usize, usize));

/// The regions an OCR engine reads ([`route::ocr_regions`]) that show what
/// an earlier one shows, each with the first region showing it, in page
/// then region order. Limited to `only`'s pages when given.
///
/// One image drawn at the same size and the same way up, with nothing
/// else in its box and no clip cutting it
/// ([`ImagePlacement::key`](crate::extract::ImagePlacement::key)), on pages
/// turned alike, with a crop of the same size (not cut short by the page's
/// edge), shows the same pixels: a form's checkbox drawn two hundred times
/// is one region read once, not two hundred calls of the engine
/// ([`ocr_jobs`]). A box that shows more (a value typed in a field's frame,
/// a tick drawn over a box) or less (one part of a sprite sheet, through a
/// clip) is read on its own, and lends its reading to none. The copies are
/// numbered as before and take the reading of the region they copy
/// ([`with_copies`]), its words laid where each is drawn and judged there,
/// as a reading of its own would be: over the page's own text, outside the
/// region, or noise. What the margin of each crop shows apart from the
/// image is not read again: a word an engine would read across the edge of
/// a copy's region is lost.
pub fn copies(doc: &Document, only: Option<&[usize]>) -> Vec<RegionCopy> {
    let mut first: std::collections::HashMap<(u64, i32, [i64; 2]), (usize, usize)> =
        std::collections::HashMap::new();
    let mut out = Vec::new();
    for p in &doc.pages {
        if only.is_some_and(|keep| !keep.contains(&p.index)) {
            continue;
        }
        let regions = route::ocr_regions(p);
        let keys = route::region_keys(p, &regions);
        for (i, (b, key)) in regions.iter().zip(keys).enumerate() {
            let Some(key) = key else { continue };
            let r = crop_rect(p, Some(*b));
            // To a thousandth of a unit, as the key's own size is.
            let size = [r.width(), r.height()].map(|v| (v * 1000.0).round() as i64);
            match first.entry((key, p.rotation, size)) {
                std::collections::hash_map::Entry::Occupied(e) => {
                    out.push(((p.index, i), *e.get()));
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert((p.index, i));
                }
            }
        }
    }
    out
}

/// `readings` with, for each copy of a region among `only`'s pages
/// ([`copies`]) that has no reading of its own, the reading of the region
/// it copies when that reading is in the crop's pixels (an engine's
/// always is), in page then crop order: what [`inject`] takes, through
/// [`read_pages`] or from word files, which [`ocr_jobs`] has no copy read
/// for.
pub fn with_copies(
    doc: &Document,
    only: Option<&[usize]>,
    mut readings: Vec<(usize, Crop, WordReading)>,
) -> Vec<(usize, Crop, WordReading)> {
    let copies = copies(doc, only);
    if copies.is_empty() {
        return readings;
    }
    let own: std::collections::HashSet<(usize, usize)> = (readings.iter())
        .filter_map(|(page, crop, _)| match crop {
            Crop::Region(i) => Some((*page, *i)),
            Crop::Page => None,
        })
        .collect();
    let mut added = Vec::new();
    for ((page, i), (of_page, of)) in copies {
        if own.contains(&(page, i)) {
            continue;
        }
        let read = readings
            .iter()
            .find(|(p, c, _)| *p == of_page && *c == Crop::Region(of));
        // A reading in page units says where on the page its words are,
        // not where in the crop: it is of its own region only.
        if let Some((_, _, reading)) = read.filter(|(_, _, r)| r.coords == Coords::Px) {
            added.push((page, Crop::Region(i), reading.clone()));
        }
    }
    readings.extend(added);
    readings.sort_by_key(|(page, crop, _)| {
        (
            *page,
            match crop {
                Crop::Page => None,
                Crop::Region(i) => Some(*i),
            },
        )
    });
    readings
}

/// What one OCR pass over a document did.
#[derive(Debug, Default)]
pub struct OcrPass {
    /// One per reading, as [`inject`] reports it.
    pub injected: Vec<Injected>,
    /// Crops the engine could not read, with its error, by crop name
    /// (`p{page}_{tag}`). The rest of the document is read regardless.
    pub failed: Vec<(String, String)>,
    /// Crops left unread past the limit ([`ocr_jobs`]).
    pub capped: usize,
}

/// Read a document's pixels with an engine and lay the words on its pages:
/// [`ocr_jobs`] (at most `max_crops`, `0` for no limit), rendered one crop
/// at a time, read, then [`inject`]ed.
///
/// Runs on the document as extracted, before analysis, as [`inject`] does;
/// no file is written. The crops are rendered as `render-routed` renders
/// them at the same `scale` (`escalate::for_each_crop_of`), so a reading
/// of the same crop gives the same words through this call or through
/// `--ocr-words`. [`CropScale::Stored`](crate::escalate::CropScale::Stored)
/// is the one for an engine: each crop at the resolution of the image under
/// it.
#[cfg(feature = "render")]
pub fn read_pages(
    doc: &mut Document,
    pdf: &hayro_syntax::Pdf,
    engine: &dyn fluree_doc_vision::OcrEngine,
    only: Option<&[usize]>,
    scale: crate::escalate::CropScale,
    max_crops: usize,
) -> OcrPass {
    let (jobs, capped) = ocr_jobs(doc, only, max_crops);
    // Each crop is read as it is cut, its pixels dropped after; the
    // readings are put back in the jobs' order, which a page rendered at
    // several scales hands its crops over out of.
    let order = crate::escalate::crop_order(&jobs);
    let mut readings = Vec::new();
    let mut failed = Vec::new();
    crate::escalate::for_each_crop_of(pdf, doc, &jobs, scale, |crop| {
        let at = order.get(&crop.name).copied().unwrap_or(usize::MAX);
        let tag = crop.name.split_once('_').map_or("", |(_, t)| t);
        let Some(which) = Crop::from_tag(tag) else {
            return;
        };
        match engine.read(&crop.pixels) {
            Ok(reading) => readings.push((at, (crop.page, which, reading))),
            Err(e) => failed.push((at, (crop.name, e))),
        }
    });
    readings.sort_by_key(|(at, _)| *at);
    failed.sort_by_key(|(at, _)| *at);
    let readings: Vec<_> = readings.into_iter().map(|(_, r)| r).collect();
    let mut failed: Vec<(String, String)> = failed.into_iter().map(|(_, f)| f).collect();
    // A copy of a region the engine could not read is not read either.
    for ((page, i), (of_page, of)) in copies(doc, only) {
        let of = format!("p{of_page}_{}", Crop::Region(of).tag());
        if let Some(e) = failed
            .iter()
            .find(|(name, _)| *name == of)
            .map(|f| f.1.clone())
        {
            failed.push((format!("p{page}_{}", Crop::Region(i).tag()), e));
        }
    }
    let readings = with_copies(doc, only, readings);
    OcrPass {
        injected: inject(doc, &readings),
        failed,
        capped,
    }
}

/// The readings [`inject`] took into the page that are still in `elements`,
/// the final output, as it reports them (`doc:ocrReadings`): one per
/// reading, in page then crop order, with the engine's own confidence
/// rounded to four decimals.
///
/// A reading is in the output when an element the deterministic engine
/// assembled holds one of its characters: one [`mark_provenance`] marked as
/// OCR's, or a line of the text layer its few words were joined to (a
/// scanned amount after a native label). Not listed, then: a reading set
/// aside, and one whose words a model's reading replaced (a whole page read
/// by a model through `--tier-results` drops what OCR made of it). A reading that
/// took no word is listed unless a model's reading replaced its page or read
/// every region the reading was to read (its region, for a region's reading),
/// so a reading that came back empty still shows.
///
/// No threshold is applied here. The built-in engine's confidence is packed
/// near 1; it shows a reading that failed as a whole, not a misread word. A
/// reading of a region the probe does not take for text that is noise
/// ([`MIN_PICTURE_CONFIDENCE`]) is set aside before it is taken, and is
/// listed as one that took no word.
pub fn readings(
    injected: &[Injected],
    elements: &[fluree_doc_model::Element],
) -> Vec<fluree_doc_model::OcrReading> {
    let round = |c: f64| (c * 1e4).round() / 1e4;
    // Per page once, not the whole document once per reading: a long scan
    // has a reading or more on every page.
    let mut by_page: std::collections::HashMap<usize, Vec<&fluree_doc_model::Element>> =
        std::collections::HashMap::new();
    for e in elements {
        by_page.entry(e.page).or_default().push(e);
    }
    let in_output = |r: &Injected| {
        let on_page = by_page.get(&r.page).map_or(&[][..], Vec::as_slice);
        if r.chars.is_empty() {
            // A reading that took nothing leaves its regions to a model:
            // one that read them all replaced the empty reading, as one of
            // the whole page does.
            let spliced = |shown: &BBox| {
                on_page.iter().any(|e| {
                    matches!(e.evidence, "region-splice" | "region-chart")
                        && e.bbox.is_some_and(|b| crate::same_box(&b, shown))
                })
            };
            let replaced = on_page.iter().any(|e| e.evidence == "page-tier")
                || (!r.regions.is_empty() && r.regions.iter().all(spliced));
            return !replaced;
        }
        on_page
            .iter()
            .filter(|e| matches!(e.provenance, "ocr" | "rust"))
            .filter_map(|e| e.bbox)
            .any(|b| {
                r.chars
                    .iter()
                    .any(|c| b.contains((c.x0 + c.x1) * 0.5, (c.y0 + c.y1) * 0.5))
            })
    };
    let mut kept: Vec<&Injected> = injected
        .iter()
        .filter(|r| r.refused.is_none() && in_output(r))
        .collect();
    kept.sort_by_key(|r| {
        (
            r.page,
            match r.crop {
                Crop::Page => None,
                Crop::Region(i) => Some(i),
            },
        )
    });
    kept.into_iter()
        .map(|r| fluree_doc_model::OcrReading {
            page: Some(r.page),
            image: None,
            crop: r.crop.tag(),
            engine: r.engine.clone(),
            words: r.taken,
            mean_confidence: r.mean_conf.map(round),
            min_confidence: r.min_conf.map(round),
        })
        .collect()
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
///
/// A heading OCR read in a screenshot pasted into a page of text (a region
/// whose picture covers under half its page, [`Injected::pasted`]) is a heading of the screen,
/// not of the document: it is read as a paragraph, as the text of a picture
/// placed in a DOCX is, so the document's outline stays its own. This is
/// decided for each heading by the regions its OCR characters land in, as a
/// model's reading is decided for each region it reads: most of them in a
/// pasted region, and it is the screen's, whatever else the page holds,
/// and whether the region's own crop or the whole page was read. A page
/// near blank but for a screenshot (a caption under it, say) is read whole,
/// and its pictures under half the page are pasted screens too. A scanned
/// page read whole keeps its headings: they are the document's; so does a
/// region that, with the images it abuts, covers half its page or more, a
/// scan whose page also carries a stamped line or a fax header of its own,
/// whole or stored in strips.
pub fn mark_provenance(elements: &mut [fluree_doc_model::Element], injected: &[Injected]) {
    use std::collections::HashMap;
    // Per page, character centers sorted by y, each with whether it lies in
    // a pasted region: an element looks only at the band of the page it
    // covers. A word is taken by its center, so a character of a word at a
    // region's edge may sit just outside it: the crop's margin is the
    // region's too.
    let mut by_page: HashMap<usize, Vec<(f64, f64, bool)>> = HashMap::new();
    for r in injected {
        by_page
            .entry(r.page)
            .or_default()
            .extend(r.chars.iter().map(|b| {
                let (x, y) = ((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5);
                let pasted = r.pasted.iter().any(|p| {
                    (p.x0 - CROP_MARGIN..=p.x1 + CROP_MARGIN).contains(&x)
                        && (p.y0 - CROP_MARGIN..=p.y1 + CROP_MARGIN).contains(&y)
                });
                (x, y, pasted)
            }));
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
        let (ocr, pasted) = centers[from..]
            .iter()
            .take_while(|c| c.1 <= b.y1)
            .filter(|c| b.contains(c.0, c.1))
            .fold((0, 0), |(n, p), c| (n + 1, p + usize::from(c.2)));
        if ocr > 0 {
            e.provenance = "ocr";
            if e.kind == "doco:SectionTitle" && pasted * 2 > ocr {
                e.kind = "doco:Paragraph".into();
                e.level = None;
            }
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
            routing: None,
            fills: Vec::new(),
            width: 612.0,
            height: 792.0,
            rotation: 0,
            ocr: None,
            warnings: Vec::new(),
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
            ppi: None,
            bitonal: false,
            grid: (1000, 1000),
            key: None,
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

    /// A one-page PDF, 200 × 100 pt, that is one grey image and no text.
    #[cfg(feature = "render")]
    fn image_only_pdf() -> Vec<u8> {
        image_pdf(
            b"/Width 2 /Height 2 /BitsPerComponent 8",
            &[0x20, 0xe0, 0xe0, 0x20],
        )
    }

    /// As [`image_only_pdf`], with the image's size and depth given.
    fn image_pdf(shape: &[u8], data: &[u8]) -> Vec<u8> {
        let content = b"q 200 0 0 100 0 0 cm /Im0 Do Q";
        let objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
               /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
            [
                format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(),
                content.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
            [
                b"<< /Type /XObject /Subtype /Image ".to_vec(),
                shape.to_vec(),
                format!(
                    " /ColorSpace /DeviceGray /Length {} >>\nstream\n",
                    data.len()
                )
                .into_bytes(),
                data.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n", i + 1).into_bytes());
            out.extend(body);
            out.extend(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).into_bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").into_bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .into_bytes(),
        );
        out
    }

    /// An engine that reads one word across the middle of whatever it is
    /// shown, and remembers the sizes it was shown.
    #[cfg(feature = "render")]
    struct OneWord(std::sync::Mutex<Vec<(u32, u32)>>);

    #[cfg(feature = "render")]
    impl fluree_doc_vision::OcrEngine for OneWord {
        fn name(&self) -> &str {
            "one-word"
        }
        fn read(&self, image: &fluree_doc_vision::Pixels) -> Result<WordReading, String> {
            self.0.lock().unwrap().push((image.width, image.height));
            let (w, h) = (image.width as f64, image.height as f64);
            Ok(WordReading {
                coords: Coords::Px,
                width: Some(w),
                height: Some(h),
                engine: Some("one-word".into()),
                words: vec![OcrWord {
                    text: "HELLO".into(),
                    bbox: [w * 0.25, h * 0.4, w * 0.75, h * 0.6],
                    angle: None,
                    conf: Some(0.9),
                }],
            })
        }
    }

    #[cfg(feature = "render")]
    #[test]
    fn an_engine_reads_the_rendered_page_and_its_words_land_on_it() {
        let bytes = image_only_pdf();
        let pdf = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes.clone())).unwrap();
        let mut doc = crate::extract::extract_bytes(bytes).unwrap();
        assert!(matches!(route::decide(&doc.pages[0]).0, Route::Vlm(_)));
        // Two pixels across 200 points: 0.72 pixels per inch, eight bits.
        let image = doc.pages[0].images[0];
        assert!((image.ppi.unwrap() - 0.72).abs() < 1e-9, "{image:?}");
        assert!(!image.bitonal);
        // A one-bit image, as a fax is: noted, so it is read at the model's
        // scale rather than its own.
        let fax = crate::extract::extract_bytes(image_pdf(
            b"/Width 16 /Height 2 /BitsPerComponent 1",
            &[0x0f, 0xf0, 0xf0, 0x0f],
        ))
        .unwrap();
        assert!(fax.pages[0].images[0].bitonal);
        let engine = OneWord(Default::default());
        let pass = read_pages(
            &mut doc,
            &pdf,
            &engine,
            None,
            crate::escalate::CropScale::Model,
            0,
        );
        // The page whole, at the model tier's scale: 2 × 200 by 2 × 100.
        assert_eq!(*engine.0.lock().unwrap(), vec![(400, 200)]);
        assert!(pass.failed.is_empty());
        assert_eq!(pass.injected.len(), 1);
        assert_eq!(pass.injected[0].taken, 1);
        assert!(text_of(&doc.pages[0]).contains("HELLO"));
        // Asked only for another page, it reads nothing.
        let mut again = crate::extract::extract_bytes(image_only_pdf()).unwrap();
        let pass = read_pages(
            &mut again,
            &pdf,
            &engine,
            Some(&[3]),
            crate::escalate::CropScale::Model,
            0,
        );
        assert!(pass.injected.is_empty());
        assert_eq!(engine.0.lock().unwrap().len(), 1);
    }

    /// An engine that cannot read, and counts how often it was asked.
    #[cfg(feature = "render")]
    struct Broken(std::sync::atomic::AtomicUsize);

    #[cfg(feature = "render")]
    impl fluree_doc_vision::OcrEngine for Broken {
        fn name(&self) -> &str {
            "broken"
        }
        fn read(&self, _: &fluree_doc_vision::Pixels) -> Result<WordReading, String> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Err("no model".into())
        }
    }

    #[cfg(feature = "render")]
    #[test]
    fn an_engine_reads_an_image_drawn_three_times_once() {
        let content = format!(
            "{}{}{}",
            capture_at(50, 40),
            capture_at(300, 40),
            capture_at(50, 100)
        );
        let bytes = form_pdf(&content, &[("Cap", 200, 30)]);
        let pdf = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes.clone())).unwrap();
        let mut doc = crate::extract::extract_bytes(bytes.clone()).unwrap();
        let engine = OneWord(Default::default());
        let stored = crate::escalate::CropScale::Stored;
        let pass = read_pages(&mut doc, &pdf, &engine, None, stored, 0);
        assert_eq!(engine.0.lock().unwrap().len(), 1);
        let taken: Vec<(Crop, usize)> = pass.injected.iter().map(|r| (r.crop, r.taken)).collect();
        assert_eq!(
            taken,
            [
                (Crop::Region(0), 1),
                (Crop::Region(1), 1),
                (Crop::Region(2), 1)
            ]
        );
        assert_eq!(text_of(&doc.pages[0]).matches("HELLO").count(), 3);
        // Where the one reading fails, each drawing is told of.
        let mut doc = crate::extract::extract_bytes(bytes).unwrap();
        let engine = Broken(Default::default());
        let pass = read_pages(&mut doc, &pdf, &engine, None, stored, 0);
        assert_eq!(engine.0.load(std::sync::atomic::Ordering::Relaxed), 1);
        let failed: Vec<&str> = pass.failed.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(failed, ["p0_r0", "p0_r1", "p0_r2"]);
        assert!(pass.injected.is_empty());
    }

    #[test]
    fn an_image_is_as_fine_as_its_coarser_axis() {
        // 600 × 100 pixels over 200 × 100 points: 216 ppi across, 72 down.
        let doc = crate::extract::extract_bytes(image_pdf(
            b"/Width 600 /Height 100 /BitsPerComponent 8",
            &vec![0x80; 600 * 100],
        ))
        .unwrap();
        let ppi = doc.pages[0].images[0].ppi.unwrap();
        assert!((ppi - 72.0).abs() < 1e-6, "{ppi}");
    }

    #[test]
    fn one_bit_images_are_known_by_depth_mask_or_filter() {
        let bitonal = |shape: &[u8], data: &[u8]| {
            let doc = crate::extract::extract_bytes(image_pdf(shape, data)).unwrap();
            doc.pages[0].images.first().map(|i| i.bitonal)
        };
        assert_eq!(
            bitonal(b"/Width 8 /Height 8 /BitsPerComponent 8", &[0x80; 64]),
            Some(false)
        );
        assert_eq!(
            bitonal(b"/Width 8 /Height 8 /BitsPerComponent 1", &[0x0f; 8]),
            Some(true)
        );
        // A mask is drawn in the fill colour: one bit, whatever it says.
        assert_eq!(
            bitonal(b"/Width 8 /Height 8 /ImageMask true", &[0x0f; 8]),
            Some(true)
        );
        // A filter array, one of which codes one bit.
        assert_eq!(
            bitonal(
                b"/Width 8 /Height 8 /Filter [/CCITTFaxDecode] /DecodeParms [<< /K -1 /Columns 8 >>]",
                &[0x00; 4]
            ),
            Some(true)
        );
        // A fax stream that leaves out its depth.
        assert_eq!(
            bitonal(
                b"/Width 8 /Height 8 /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 8 >>",
                &[0x00; 4]
            ),
            Some(true)
        );
    }

    #[test]
    fn an_inline_image_of_one_bit_is_known_by_its_abbreviated_keys() {
        // `BI … ID … EI` writes `/BPC` and `/F` for the image's depth and
        // filter.
        for dict in [
            "/W 16 /H 2 /BPC 1 /CS /G",
            "/W 16 /H 2 /F /CCF /DP << /K -1 /Columns 16 >>",
        ] {
            let content = format!("q 160 0 0 20 20 20 cm BI {dict} ID \x0f\x0f\x0f\x0f EI Q");
            let bytes = pdf_with(
                b"/MediaBox [0 0 200 100]",
                b"",
                content.as_bytes(),
                b"/Width 1 /Height 1 /BitsPerComponent 8",
                &[0],
            );
            let doc = crate::extract::extract_bytes(bytes).unwrap();
            let inline: Vec<bool> = doc.pages[0].images.iter().map(|i| i.bitonal).collect();
            assert_eq!(inline, [true], "{dict}");
        }
    }

    #[cfg(feature = "render")]
    #[test]
    fn a_region_of_a_turned_page_is_cut_where_it_shows() {
        // A /Rotate 90 page whose text runs along the sheet, so it is read in
        // the frame turned back upright, with a black image in one corner:
        // the crop of the region the image is routed as shows the image, not
        // white paper.
        let mut content = b"q 60 0 0 40 20 240 cm /Im0 Do Q\n".to_vec();
        for k in 0..6 {
            content.extend(
                format!(
                    "BT /F1 10 Tf 30 {} Td (Line of upright text {k}) Tj ET\n",
                    40 + 20 * k
                )
                .into_bytes(),
            );
        }
        let bytes = pdf_with(
            b"/MediaBox [0 0 200 300] /Rotate 90",
            b"/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
            &content,
            b"/Width 6 /Height 4 /BitsPerComponent 8",
            &[0x00; 24],
        );
        let doc = crate::extract::extract_bytes(bytes.clone()).unwrap();
        let p = &doc.pages[0];
        assert_ne!(p.rotation, 0, "read in a turned frame");
        let region = p.images[0].bbox;
        let pdf = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes)).unwrap();
        let mut dark = None;
        crate::escalate::for_each_crop_of(
            &pdf,
            &doc,
            &vec![(0, Some(vec![("r0".to_string(), region)]))],
            crate::escalate::CropScale::Model,
            |c| {
                let px = c.pixels.data.chunks_exact(4);
                let n = px.len();
                dark = Some(px.filter(|q| q[0] < 64).count() as f64 / n as f64);
            },
        );
        let dark = dark.expect("the region is cut");
        assert!(dark > 0.3, "the crop shows the image: {dark}");
    }

    #[cfg(feature = "render")]
    #[test]
    fn a_box_in_the_display_frame_is_cut_as_given() {
        // The same turned page, its image's box given as an element's
        // `doc:bbox` gives it: in the display frame already.
        let mut content = b"q 60 0 0 40 20 240 cm /Im0 Do Q\n".to_vec();
        for k in 0..6 {
            content.extend(
                format!(
                    "BT /F1 10 Tf 30 {} Td (Line of upright text {k}) Tj ET\n",
                    40 + 20 * k
                )
                .into_bytes(),
            );
        }
        let bytes = pdf_with(
            b"/MediaBox [0 0 200 300] /Rotate 90",
            b"/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
            &content,
            b"/Width 6 /Height 4 /BitsPerComponent 8",
            &[0x00; 24],
        );
        let doc = crate::extract::extract_bytes(bytes.clone()).unwrap();
        let p = &doc.pages[0];
        let shown = p.to_display(p.images[0].bbox);
        let pdf = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes)).unwrap();
        let crops = crate::escalate::render_crops_displayed(
            &pdf,
            &vec![(0, Some(vec![("r0".to_string(), shown)]))],
        );
        let mut reader = png::Decoder::new(std::io::Cursor::new(&crops[0].png))
            .read_info()
            .unwrap();
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        let px = buf[..info.buffer_size()].chunks_exact(info.color_type.samples());
        let n = px.len();
        let dark = px.filter(|q| q[0] < 64).count() as f64 / n as f64;
        assert!(dark > 0.3, "the crop shows the image: {dark}");
    }

    #[cfg(feature = "render")]
    #[test]
    fn crops_rendered_at_several_scales_are_handed_over_as_cut() {
        let bytes = image_pdf(
            b"/Width 600 /Height 300 /BitsPerComponent 8",
            &vec![0x80; 600 * 300],
        );
        let pdf = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes)).unwrap();
        let b = |x0: f64| BBox {
            x0,
            y0: 10.0,
            x1: x0 + 20.0,
            y1: 30.0,
        };
        let regions: Vec<(String, BBox)> = (0..5)
            .map(|i| (format!("r{i}"), b(10.0 + 30.0 * i as f64)))
            .collect();
        let scales = [3.0f32, 2.0, 4.0, 2.0, 3.5];
        let jobs = vec![(0, Some(regions))];
        let mut names = Vec::new();
        crate::escalate::for_each_crop_scaled(
            &pdf,
            &jobs,
            |_, r| scales[((r.unwrap().x0 - 10.0) / 30.0) as usize],
            |c| names.push((c.name, c.scale)),
        );
        // Each render's crops as it is cut, coarsest first, in the jobs'
        // order within one.
        let named = |i: &[usize]| -> Vec<(String, f32)> {
            i.iter().map(|&i| (format!("p0_r{i}"), scales[i])).collect()
        };
        assert_eq!(names, named(&[1, 3, 0, 4, 2]));
        // The jobs' order, given back.
        let order = crate::escalate::crop_order(&jobs);
        names.sort_by_key(|(n, _)| order[n]);
        assert_eq!(names, named(&[0, 1, 2, 3, 4]));
    }

    #[cfg(feature = "render")]
    #[test]
    fn a_page_read_at_several_scales_is_injected_in_the_jobs_order() {
        // A text page with two small captures, the first at 288 ppi and the
        // second at 150: the second is rendered, cut and read first.
        let shot = |y0: f64, ppi: f64| ImagePlacement {
            bbox: BBox {
                x0: 100.0,
                y0,
                x1: 300.0,
                y1: y0 + 30.0,
            },
            texty: true,
            ppi: Some(ppi),
            bitonal: false,
            grid: (1000, 1000),
            key: None,
        };
        let mut doc = Document {
            pages: vec![page(
                (0..1500)
                    .flat_map(|i| {
                        native("a", 20.0 + (i % 50) as f64 * 10.0, 600.0 + (i / 50) as f64)
                    })
                    .collect(),
                vec![shot(100.0, 288.0), shot(200.0, 150.0)],
            )],
        };
        let pdf = hayro_syntax::Pdf::new(std::sync::Arc::new(pdf_with(
            b"/MediaBox [0 0 612 792]",
            b"",
            b"q 200 0 0 30 100 100 cm /Im0 Do Q",
            b"/Width 2 /Height 2 /BitsPerComponent 8",
            &[0x20, 0xe0, 0xe0, 0x20],
        )))
        .unwrap();
        let (jobs, _) = ocr_jobs(&doc, None, 0);
        assert_eq!(jobs.len(), 1, "{jobs:?}");
        let engine = OneWord(Default::default());
        let pass = read_pages(
            &mut doc,
            &pdf,
            &engine,
            None,
            crate::escalate::CropScale::Stored,
            0,
        );
        let heights: Vec<u32> = engine.0.lock().unwrap().iter().map(|r| r.1).collect();
        assert!(heights[0] < heights[1], "read coarsest first: {heights:?}");
        let crops: Vec<Crop> = pass.injected.iter().map(|r| r.crop).collect();
        assert_eq!(crops, [Crop::Region(0), Crop::Region(1)]);
    }

    /// A one-page PDF of `page` entries, resources beside its image, one
    /// content stream and one image.
    fn pdf_with(
        page: &[u8],
        resources: &[u8],
        content: &[u8],
        shape: &[u8],
        data: &[u8],
    ) -> Vec<u8> {
        let objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            [
                b"<< /Type /Page /Parent 2 0 R ".to_vec(),
                page.to_vec(),
                b" /Resources << ".to_vec(),
                resources.to_vec(),
                b" /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
            ]
            .concat(),
            [
                format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(),
                content.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
            [
                b"<< /Type /XObject /Subtype /Image ".to_vec(),
                shape.to_vec(),
                format!(
                    " /ColorSpace /DeviceGray /Length {} >>\nstream\n",
                    data.len()
                )
                .into_bytes(),
                data.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n", i + 1).into_bytes());
            out.extend(body);
            out.extend(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).into_bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").into_bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .into_bytes(),
        );
        out
    }

    #[cfg(feature = "render")]
    #[test]
    fn a_stored_resolution_read_gets_the_images_own_pixels() {
        // 600 × 300 pixels over 200 × 100 points: 216 ppi, so 3 px per point
        // stored, against 2 for the model.
        let bytes = image_pdf(
            b"/Width 600 /Height 300 /BitsPerComponent 8",
            &vec![0x80; 600 * 300],
        );
        let pdf = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes.clone())).unwrap();
        for (mode, want) in [
            (crate::escalate::CropScale::Model, (400, 200)),
            (crate::escalate::CropScale::Stored, (600, 300)),
        ] {
            let mut doc = crate::extract::extract_bytes(bytes.clone()).unwrap();
            let engine = OneWord(Default::default());
            let pass = read_pages(&mut doc, &pdf, &engine, None, mode, 0);
            assert_eq!(*engine.0.lock().unwrap(), vec![want], "{mode:?}");
            assert_eq!(pass.injected[0].taken, 1);
        }
    }

    #[test]
    fn a_small_region_takes_its_reading() {
        // A text page carrying a capture too small for a model: region 0 is
        // the engine's.
        let mut doc = Document {
            pages: vec![page(
                (0..1500)
                    .flat_map(|i| {
                        native("a", 20.0 + (i % 50) as f64 * 10.0, 600.0 + (i / 50) as f64)
                    })
                    .collect(),
                vec![ImagePlacement {
                    bbox: BBox {
                        x0: 100.0,
                        y0: 100.0,
                        x1: 300.0,
                        y1: 130.0,
                    },
                    texty: true,
                    ppi: None,
                    bitonal: false,
                    grid: (1000, 1000),
                    key: None,
                }],
            )],
        };
        assert_eq!(route::decide(&doc.pages[0]).0, Route::Deterministic);
        let r = inject(
            &mut doc,
            &[(
                0,
                Crop::Region(0),
                page_units(vec![word("ACCT 4417", [120.0, 108.0, 200.0, 122.0])]),
            )],
        );
        assert_eq!(r[0].refused, None, "{r:?}");
        assert_eq!(r[0].taken, 1);
        // A region the page does not have is still refused.
        let r = inject(
            &mut doc,
            &[(
                0,
                Crop::Region(1),
                page_units(vec![word("x", [120.0, 108.0, 130.0, 122.0])]),
            )],
        );
        assert_eq!(r[0].refused, Some("region not routed to a reader"));
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
        // A detector box 13 pt tall on the page, at the engine's calibration.
        assert!(
            (f64::from(g[0].font_size) - 13.0 / box_height_em("Account", 140.0 / 26.0)).abs()
                < 0.01
        );
        assert_eq!(r[0].mean_conf, Some(0.9));
    }

    #[test]
    fn a_reading_taken_is_reported_with_its_engine_and_confidence() {
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        let conf = |w: OcrWord, c: Option<f64>| OcrWord { conf: c, ..w };
        let reading = WordReading {
            engine: Some("test-engine".into()),
            ..px(vec![
                conf(word("Account", [144.0, 200.0, 284.0, 226.0]), Some(0.95)),
                conf(word("number", [300.0, 200.0, 420.0, 226.0]), Some(0.62111)),
                // A word with no score counts as a word, not as a score.
                conf(word("4471", [440.0, 200.0, 520.0, 226.0]), None),
            ])
        };
        let r = inject(
            &mut doc,
            &[
                (0, Crop::Page, reading),
                // A region the page does not have: set aside, not reported.
                (
                    0,
                    Crop::Region(3),
                    px(vec![word("x", [144.0, 300.0, 160.0, 320.0])]),
                ),
            ],
        );
        assert_eq!(r[1].refused, Some("region not routed to a reader"));
        let elements = analysed(&mut doc, &r);
        assert_eq!(
            readings(&r, &elements),
            vec![fluree_doc_model::OcrReading {
                page: Some(0),
                image: None,
                crop: "full".into(),
                engine: Some("test-engine".into()),
                words: 3,
                mean_confidence: Some(0.7856),
                min_confidence: Some(0.6211),
            }]
        );
    }

    #[test]
    fn a_reading_scored_on_another_scale_reports_no_confidence() {
        // Tesseract's 0-100, not divided: its 1 and its 0.5 are not near
        // certain and even odds, so no word's score is reported.
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        let conf = |w: OcrWord, c: f64| OcrWord { conf: Some(c), ..w };
        let reading = px(vec![
            conf(word("Account", [144.0, 200.0, 284.0, 226.0]), 96.0),
            conf(word("number", [300.0, 200.0, 420.0, 226.0]), 1.0),
            conf(word("4471", [440.0, 200.0, 520.0, 226.0]), 0.5),
        ]);
        let r = inject(&mut doc, &[(0, Crop::Page, reading)]);
        assert_eq!((r[0].taken, r[0].mean_conf, r[0].min_conf), (3, None, None));
        let elements = analysed(&mut doc, &r);
        let listed = readings(&r, &elements);
        assert_eq!(listed.len(), 1);
        assert_eq!(
            (listed[0].mean_confidence, listed[0].min_confidence),
            (None, None)
        );
    }

    #[test]
    fn a_region_reading_scored_0_100_is_judged_noise_by_its_boxes_alone() {
        // One decision for the whole reading: a score outside 0-1 says all
        // of them are on another scale, a 0 out of 100 among them. Judged
        // word by word, the 0 would be a score of 0-1 and the mean of the
        // words that have one under the noise rule's floor.
        let mut doc = prose_with_a_photograph();
        let reading = scored(
            &[
                ("Account", [60.0, 382.0, 100.0, 398.0]),
                ("4471", [110.0, 382.0, 140.0, 398.0]),
                ("ref", [150.0, 382.0, 170.0, 398.0]),
            ],
            None,
        );
        let reading = WordReading {
            words: (reading.words.into_iter().zip([96.0, 0.0, 88.0]))
                .map(|(w, c)| OcrWord { conf: Some(c), ..w })
                .collect(),
            ..reading
        };
        assert_eq!(reading.scores_out_of_range(), 2);
        let r = inject(&mut doc, &[(0, Crop::Region(0), reading)]);
        assert_eq!((r[0].taken, r[0].mean_conf), (3, None), "{r:?}");
    }

    #[test]
    fn readings_are_reported_in_page_then_crop_order() {
        let r = [
            Injected::new(1, Crop::Page),
            Injected::new(0, Crop::Region(10)),
            Injected::new(0, Crop::Page),
            Injected::new(0, Crop::Region(2)),
        ];
        let order: Vec<(Option<usize>, String)> = readings(&r, &[])
            .into_iter()
            .map(|o| (o.page, o.crop))
            .collect();
        assert_eq!(
            order,
            [
                (Some(0), "full".to_string()),
                (Some(0), "r2".into()),
                (Some(0), "r10".into()),
                (Some(1), "full".into()),
            ]
        );
        // A reading with no scored word reports none.
        assert_eq!(readings(&r, &[])[0].mean_confidence, None);
    }

    #[test]
    fn a_reading_a_models_page_reading_replaced_is_not_reported() {
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
        let mut elements = analysed(&mut doc, &r);
        assert_eq!(readings(&r, &elements).len(), 1);
        // Only an element of the reading's own page holds its words.
        let elsewhere: Vec<fluree_doc_model::Element> = elements
            .iter()
            .map(|e| fluree_doc_model::Element {
                page: 1,
                ..e.clone()
            })
            .collect();
        assert!(readings(&r, &elsewhere).is_empty());
        // A model's reading of the whole page, as `--tier-results` splices
        // it: OCR's elements are gone, and so is its reading.
        // Its element covers the page, OCR's characters included: only its
        // provenance tells it apart.
        elements = vec![fluree_doc_model::Element {
            page: 0,
            bbox: Some(full_page_image().bbox),
            ..fluree_doc_model::Element::new("doco:Paragraph", "Account", "vlm", "page-tier")
        }];
        assert!(readings(&r, &elements).is_empty());
        // A reading that took no word shows, unless its page was replaced.
        let empty = [Injected::new(0, Crop::Page)];
        assert!(readings(&empty, &elements).is_empty());
        assert_eq!(readings(&empty, &[]).len(), 1);
        // Another page's model reading does not replace this one's.
        assert_eq!(
            readings(&[Injected::new(1, Crop::Page)], &elements).len(),
            1
        );
    }

    #[test]
    fn a_reading_joined_to_a_native_line_is_reported() {
        // A native label with a scanned amount just after it: the amount's
        // word joins the label's line, which is then OCR's, a character read
        // from pixels never being presented as the PDF's own. Text enough
        // elsewhere for the page to be read as it is.
        let mut glyphs = native("Total amount due:", 60.0, 500.0);
        glyphs.extend(
            (0..1500)
                .flat_map(|i| native("a", 20.0 + (i % 50) as f64 * 10.0, 600.0 + (i / 50) as f64)),
        );
        let mut doc = Document {
            pages: vec![page(
                glyphs,
                vec![ImagePlacement {
                    bbox: BBox {
                        x0: 164.0,
                        y0: 488.0,
                        x1: 260.0,
                        y1: 506.0,
                    },
                    texty: true,
                    ppi: None,
                    bitonal: false,
                    grid: (1000, 1000),
                    key: None,
                }],
            )],
        };
        let r = inject(
            &mut doc,
            &[(
                0,
                Crop::Region(0),
                page_units(vec![word("4471", [168.0, 491.0, 192.0, 502.0])]),
            )],
        );
        assert_eq!(r[0].taken, 1, "{r:?}");
        let elements = analysed(&mut doc, &r);
        let line = elements
            .iter()
            .find(|e| e.text.contains("4471"))
            .expect("the amount is in the output");
        assert!(line.text.contains("Total amount due"), "{elements:?}");
        assert_eq!(line.provenance, "ocr");
        let listed = readings(&r, &elements);
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!((listed[0].crop.as_str(), listed[0].words), ("r0", 1));
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
    fn a_reading_none_of_whose_words_land_on_the_page_is_set_aside() {
        // Every word off the page, as an origin or a scale not the page's
        // puts them: not a reading that came back blank, listed with no
        // word, but one that could not be placed, and is not listed.
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        let off = vec![
            word("Account", [1000.0, 100.0, 1080.0, 116.0]),
            word("4417", [1100.0, 100.0, 1140.0, 116.0]),
        ];
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(off.clone()))]);
        assert_eq!(r[0].refused, Some("none of the reading could be placed"));
        assert!(readings(&r, &[]).is_empty());
        // So is a reading of a page of text, whole or of one of its regions.
        for crop in [Crop::Page, Crop::Region(0)] {
            let mut doc = page_with_regions(&[UPPER, LOWER]);
            let r = inject(&mut doc, &[(0, crop, page_units(off.clone()))]);
            let why = Some("none of the reading could be placed");
            assert_eq!(r[0].refused, why, "{crop:?}");
            assert!(readings(&r, &[]).is_empty(), "{crop:?}");
        }
        // A region's reading whose words land on the page, but in another
        // region: the origin is not the region's either. Also on a page
        // drawn turned, whose crops are cut in the display frame.
        for rotation in [0, 90] {
            let doc = || {
                let mut doc = page_with_regions(&[UPPER, LOWER]);
                doc.pages[0].rotation = rotation;
                doc
            };
            // A word in the lower region, as the image read shows it.
            let (cx, cy) = doc().pages[0].to_display_point((100.0, 508.0));
            let lower = vec![word("Account", [cx - 8.0, cy - 8.0, cx + 8.0, cy + 8.0])];
            let mut upper = doc();
            let r = inject(
                &mut upper,
                &[(0, Crop::Region(0), page_units(lower.clone()))],
            );
            let why = Some("none of the reading could be placed");
            assert_eq!(r[0].refused, why, "{rotation}");
            assert!(readings(&r, &[]).is_empty(), "{rotation}");
            // Read as the other region's, the word lands on what was read.
            let r = inject(&mut doc(), &[(0, Crop::Region(1), page_units(lower))]);
            assert_eq!((r[0].refused, r[0].taken), (None, 1), "{rotation}");
        }
        // Every word over the text layer of a scan with a stamped header:
        // the page is read, and holds nothing more. That reading is listed.
        let mut doc = Document {
            pages: vec![page(native("REF", 72.0, 60.0), vec![full_page_image()])],
        };
        let over = px(vec![word("REF", [144.0, 100.0, 180.0, 124.0])]);
        let r = inject(&mut doc, &[(0, Crop::Page, over)]);
        assert_eq!((r[0].refused, r[0].taken, r[0].dropped), (None, 0, 1));
        assert_eq!(readings(&r, &[]).len(), 1);
        // A reading with no word at all came back blank, and is listed.
        let mut doc = Document {
            pages: vec![page(Vec::new(), vec![full_page_image()])],
        };
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(Vec::new()))]);
        assert_eq!(r[0].refused, None);
        assert_eq!(readings(&r, &[]).len(), 1);
    }

    #[test]
    fn a_region_reading_most_of_whose_words_land_off_its_crop_is_set_aside() {
        // The upper region's crop, its margin included, is 502 x 212
        // units: read at 4 pixels a unit, its words boxed at 8, as a crop
        // rendered at twice the scale its reading says. A grid of 16 words
        // over the image read: the 4 of its top left corner land on the
        // crop, the rest off it.
        let (w, h) = (502.0 * 4.0, 212.0 * 4.0);
        let grid: Vec<OcrWord> = (0..16)
            .map(|k| {
                let (x, y) = (
                    f64::from(k % 4) * 2.0 * w / 4.0,
                    f64::from(k / 4) * 2.0 * h / 4.0,
                );
                word("Account", [x + 40.0, y + 60.0, x + 400.0, y + 140.0])
            })
            .collect();
        let at = |words: Vec<OcrWord>| WordReading {
            coords: Coords::Px,
            width: Some(w),
            height: Some(h),
            engine: None,
            words,
        };
        let mut doc = page_with_regions(&[UPPER, LOWER]);
        let glyphs = doc.pages[0].glyphs.len();
        let r = inject(&mut doc, &[(0, Crop::Region(0), at(grid.clone()))]);
        let why = Some("most of the reading could not be placed");
        assert_eq!(r[0].refused, why, "{r:?}");
        assert_eq!(doc.pages[0].glyphs.len(), glyphs);
        assert!(readings(&r, &[]).is_empty());
        // At its own scale, every word lands. A word in the crop's margin,
        // the text beside the image, is placed where it was read, though
        // not in the region: with one in the region, the reading is taken.
        let own: Vec<OcrWord> = (grid.iter())
            .map(|g| {
                let [x0, y0, x1, y1] = g.bbox;
                word("Account", [x0 / 2.0, y0 / 2.0, x1 / 2.0, y1 / 2.0])
            })
            .collect();
        let r = inject(
            &mut page_with_regions(&[UPPER, LOWER]),
            &[(0, Crop::Region(0), at(own))],
        );
        assert_eq!((r[0].refused, r[0].taken), (None, 16), "{r:?}");
        let margin = vec![
            word("Fig", [2.0, 2.0, 20.0, 20.0]),
            word("Account", [100.0, 100.0, 400.0, 160.0]),
        ];
        let r = inject(
            &mut page_with_regions(&[UPPER, LOWER]),
            &[(0, Crop::Region(0), at(margin))],
        );
        assert_eq!(
            (r[0].refused, r[0].taken, r[0].dropped),
            (None, 1, 1),
            "{r:?}"
        );
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
                    ppi: None,
                    bitonal: false,
                    grid: (1000, 1000),
                    key: None,
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
        // On the word's band, 319 to 329 on the page: laid in a line box
        // of its size, an em and a little, where a box over a word with no
        // descenders is a little under an em.
        let b = g.bbox.unwrap();
        assert!(b.y0 >= 318.5 && b.y1 <= 329.5, "{b:?}");
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

    /// A page of prose with, under it, a capture too small for a model
    /// (region 0, which an OCR engine reads) and a photograph a sixth of the
    /// page that the probe does not take for text (read by nothing).
    fn prose_with_a_photograph() -> Document {
        let image = |texty, (x0, y0, x1, y1)| ImagePlacement {
            bbox: BBox { x0, y0, x1, y1 },
            texty,
            ppi: None,
            bitonal: false,
            grid: (1000, 1000),
            key: None,
        };
        let line = "The account review and the record shown below";
        Document {
            pages: vec![page(
                (0..30)
                    .flat_map(|i| native(line, 56.0, 60.0 + 10.0 * f64::from(i)))
                    .collect(),
                vec![
                    image(true, (56.0, 380.0, 200.0, 400.0)),
                    image(false, (56.0, 450.0, 556.0, 600.0)),
                ],
            )],
        }
    }

    /// Words in PDF units, each scored `conf`.
    fn scored(words: &[(&str, [f64; 4])], conf: Option<f64>) -> WordReading {
        WordReading {
            coords: Coords::Page,
            width: None,
            height: None,
            engine: None,
            words: words
                .iter()
                .map(|&(text, bbox)| OcrWord {
                    text: text.into(),
                    bbox,
                    angle: None,
                    conf,
                })
                .collect(),
        }
    }

    #[test]
    fn a_reading_of_a_region_that_is_noise_is_set_aside() {
        // The capture's own box is 56,380 to 200,400; its crop, its margin
        // included, 50,374 to 206,406.
        let strokes = [
            ("M", [60.0, 382.0, 100.0, 398.0]),
            ("I7", [120.0, 384.0, 140.0, 398.0]),
        ];
        let whole_crop = [("7", [50.0, 374.0, 206.0, 406.0])];
        let run = |reading: WordReading, crop: Crop| {
            let mut doc = prose_with_a_photograph();
            assert_eq!(
                (doc.pages[0].routing().declined.iter())
                    .map(|d| (d.1, d.2))
                    .collect::<Vec<_>>(),
                [
                    (route::Declined::Small, Some(0)),
                    (route::Declined::NotTexty, None)
                ]
            );
            let glyphs = doc.pages[0].glyphs.len();
            let r = inject(&mut doc, &[(0, crop, reading)]);
            let unread: Vec<String> = crate::unread_regions(&doc, &[])
                .into_iter()
                .map(|u| u.reason)
                .collect();
            let added: String = (doc.pages[0].glyphs[glyphs..].iter())
                .map(|g| g.text.as_str())
                .collect();
            (r[0].clone(), added, unread)
        };
        // Unsure in the mean; fragments of one or two letters or figures,
        // more than two thirds of the words, not sure enough to be a
        // chart's or a code's; or one fragment boxed over the whole crop,
        // however sure, or with no confidence: noise, set aside, and the
        // capture is still listed.
        let mostly_fragments = [
            ("1", [60.0, 382.0, 66.0, 398.0]),
            ("I7", [70.0, 382.0, 80.0, 398.0]),
            ("J", [90.0, 382.0, 96.0, 398.0]),
            ("Total", [110.0, 382.0, 150.0, 398.0]),
        ];
        for reading in [
            scored(&strokes, Some(0.3)),
            scored(&strokes, Some(0.6)),
            scored(&mostly_fragments, Some(0.7)),
            scored(&whole_crop, None),
            scored(&whole_crop, Some(0.66)),
            scored(&whole_crop, Some(0.99)),
        ] {
            let (r, added, unread) = run(reading.clone(), Crop::Region(0));
            assert_eq!(
                (r.refused, r.taken, r.dropped, added.as_str()),
                (None, 0, reading.words.len(), ""),
                "{reading:?}"
            );
            assert_eq!(unread, ["Small", "NotTexty"]);
        }
        // A date, read unsure but as words; exactly two thirds of the words
        // fragments, not more; fragments read as surely as a chart's axis
        // or a code; a line of text over the whole of a tight crop, read
        // surely: text, taken, and no longer listed.
        let date = [
            ("1", [60.0, 382.0, 66.0, 398.0]),
            ("2026-03-14 10:42", [80.0, 384.0, 190.0, 398.0]),
        ];
        let two_thirds = &mostly_fragments[1..];
        let line = [("Account 4417", [50.0, 374.0, 206.0, 406.0])];
        for reading in [
            scored(&date, Some(0.66)),
            scored(two_thirds, Some(0.7)),
            scored(&strokes, Some(0.9)),
            scored(&line, Some(0.99)),
        ] {
            let (r, added, unread) = run(reading.clone(), Crop::Region(0));
            assert_eq!(r.taken, reading.words.len(), "{r:?}");
            assert!(!added.is_empty());
            assert_eq!(unread, ["NotTexty"]);
        }
        // The photograph is offered to no engine: a reading of it is
        // refused, and it stays listed.
        let (r, added, unread) = run(
            scored(&[("Sunset", [80.0, 470.0, 160.0, 490.0])], Some(0.99)),
            Crop::Region(1),
        );
        assert_eq!(r.refused, Some("region not routed to a reader"));
        assert!(added.is_empty());
        assert_eq!(unread, ["Small", "NotTexty"]);
        // A whole reading of the page is not judged: its words in the
        // capture are taken however unsure, and those in the photograph
        // land in no region it may fill.
        let acct = ("ACCT 4417", [70.0, 384.0, 150.0, 396.0]);
        let mut both = scored(&[("", [0.0; 4]), acct], Some(0.3));
        both.words
            .extend(scored(&[("M", [80.0, 470.0, 160.0, 520.0])], Some(0.3)).words);
        let (r, added, unread) = run(both, Crop::Page);
        assert_eq!(r.taken, 1, "{r:?}");
        assert_eq!(added, "ACCT 4417");
        assert_eq!(unread, ["NotTexty"]);
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
        // Most of the page level, so it is read as it stands.
        let level = vec![
            word("Name", [300.0, 400.0, 360.0, 420.0]),
            word("Date", [300.0, 440.0, 360.0, 460.0]),
        ];
        inject(&mut doc, &[(0, Crop::Page, px([vec![w], level].concat()))]);
        assert_eq!(doc.pages[0].rotation, 0);
        let g: Vec<_> = doc.pages[0]
            .glyphs
            .iter()
            .filter(|g| g.text == "A" || g.text == "B")
            .collect();
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
    fn a_heading_read_in_a_screenshot_on_a_page_of_text_is_a_paragraph() {
        // A page of text with a heading of its own, and a pasted screenshot
        // whose reading holds a large title line and a line of body text.
        let mut glyphs: Vec<Glyph> = native("Findings", 72.0, 60.0)
            .into_iter()
            .map(|g| Glyph {
                font_size: 18.0,
                ..g
            })
            .collect();
        // Lines whose spaces fall in different places, as prose's do.
        let prose = [
            "The quarterly figures are summarised in the screen shown below",
            "where each account carries its balance and its last reference",
            "as the branch recorded them when the period was closed for review",
        ];
        for row in 0..30 {
            glyphs.extend(native(prose[row % 3], 72.0, 90.0 + row as f64 * 12.0));
        }
        let shot = ImagePlacement {
            bbox: BBox {
                x0: 60.0,
                y0: 470.0,
                x1: 550.0,
                y1: 600.0,
            },
            texty: true,
            ppi: None,
            bitonal: false,
            grid: (1000, 1000),
            key: None,
        };
        let doc = Document {
            pages: vec![page(glyphs, vec![shot])],
        };
        let mut words = vec![
            word("Balance", [80.0, 480.0, 200.0, 510.0]),
            word("9,876.54", [210.0, 480.0, 340.0, 510.0]),
        ];
        let mut x = 80.0;
        for w in "Reference ABC-7781 opened at the branch office on the date shown".split(' ') {
            let len = w.len() as f64 * 6.0;
            words.push(word(w, [x, 540.0, x + len, 551.0]));
            x += len + 6.0;
        }
        // The screenshot's crop read, or the whole page, whose words land in
        // the same region: the rule is the same.
        for crop in [Crop::Region(0), Crop::Page] {
            let mut doc = doc.clone();
            let injected = inject(&mut doc, &[(0, crop, page_units(words.clone()))]);
            assert_eq!(injected[0].taken, 13, "{crop:?}");
            let els = analysed(&mut doc, &injected);
            let kind = |text: &str| {
                els.iter()
                    .find(|e| e.text.contains(text))
                    .map(|e| (e.kind.as_str(), e.level, e.provenance))
            };
            // The document's heading stays one; the screen's is a paragraph.
            assert_eq!(kind("Findings").unwrap().0, "doco:SectionTitle");
            assert_eq!(
                kind("9,876.54"),
                Some(("doco:Paragraph", None, "ocr")),
                "{crop:?} {els:?}"
            );
        }
    }

    #[test]
    fn a_heading_at_a_pasted_regions_edge_is_the_screens() {
        // A screen pasted at 60-550 × 470-600, and a heading OCR read at its
        // top: its words landed in the region by their centers, but letters
        // are laid by their line, and these sit up to 5 units above it.
        let shot = BBox {
            x0: 60.0,
            y0: 470.0,
            x1: 550.0,
            y1: 600.0,
        };
        let kind_at = |top: f64| {
            let chars = (0..8)
                .map(|i| BBox {
                    x0: 80.0 + 12.0 * f64::from(i),
                    y0: top,
                    x1: 92.0 + 12.0 * f64::from(i),
                    y1: top + 10.0,
                })
                .collect();
            let injected = Injected {
                chars,
                pasted: vec![shot],
                ..Injected::new(0, Crop::Region(0))
            };
            let mut heading =
                fluree_doc_model::Element::new("doco:SectionTitle", "Overview", "rust", "font");
            heading.level = Some(2);
            heading.bbox = Some(BBox {
                x0: 78.0,
                y0: top - 2.0,
                x1: 180.0,
                y1: top + 12.0,
            });
            let mut els = [heading];
            mark_provenance(&mut els, &[injected]);
            (els[0].kind.clone(), els[0].provenance)
        };
        // Within the crop's margin, the region's: the screen's heading.
        assert_eq!(kind_at(460.0), ("doco:Paragraph".to_string(), "ocr"));
        // Past it, the page's own.
        assert_eq!(kind_at(450.0), ("doco:SectionTitle".to_string(), "ocr"));
    }

    #[test]
    fn a_heading_read_in_a_screenshot_alone_on_its_page_is_a_paragraph() {
        // A page holding only a pasted screenshot (under a quarter of it)
        // and its caption: too few glyphs for a page of text, so it is read
        // whole as near blank. The screen's title is still the screen's.
        let glyphs = native("Figure 3", 72.0, 330.0);
        let shot = ImagePlacement {
            bbox: BBox {
                x0: 60.0,
                y0: 80.0,
                x1: 550.0,
                y1: 310.0,
            },
            texty: true,
            ppi: None,
            bitonal: false,
            grid: (1000, 1000),
            key: None,
        };
        let mut doc = Document {
            pages: vec![page(glyphs, vec![shot])],
        };
        assert!(route::share(&doc.pages[0], &shot.bbox) < route::IMAGE_DOMINANT_COVERAGE);
        assert_eq!(
            route::decide(&doc.pages[0]).0,
            route::Route::Vlm(route::Reason::NearBlank)
        );
        let mut words = vec![
            word("Account", [80.0, 100.0, 220.0, 130.0]),
            word("Overview", [230.0, 100.0, 380.0, 130.0]),
        ];
        let prose = [
            "Reference ABC-7781 opened at the branch office on the date shown",
            "with the balance carried forward from the previous closed period",
            "and the last payment recorded against it by the branch in March",
        ];
        for row in 0..9 {
            let mut x = 80.0;
            let y = 150.0 + row as f64 * 16.0;
            for w in prose[row % 3].split(' ') {
                let len = w.len() as f64 * 6.0;
                words.push(word(w, [x, y, x + len, y + 11.0]));
                x += len + 6.0;
            }
        }
        let injected = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        assert!(injected[0].taken > 0, "{injected:?}");
        assert_eq!(injected[0].pasted, [doc.pages[0].to_display(shot.bbox)]);
        let els = analysed(&mut doc, &injected);
        let title = els.iter().find(|e| e.text.contains("Overview")).unwrap();
        assert_eq!(
            (title.kind.as_str(), title.level, title.provenance),
            ("doco:Paragraph", None, "ocr"),
            "{els:?}"
        );
    }

    #[test]
    fn a_heading_read_in_a_scan_under_a_stamped_header_stays_one() {
        // Two stamped lines of text, too many glyphs for the page to be
        // routed a scan, over a scan covering most of the page: read in a
        // region, but the region is the page's content, and its title is the
        // document's.
        let scan = scan_strip(80.0, 760.0);
        assert!(
            route::share(&stamped_page(vec![scan]), &scan.bbox) >= route::IMAGE_DOMINANT_COVERAGE
        );
        assert_scan_title_stays_a_heading(vec![scan], &[0]);
    }

    #[test]
    fn a_heading_read_in_a_scan_stored_in_strips_under_a_stamped_header_stays_one() {
        // The same scan stored in two strips, each under half the page, as
        // some scanners and producers store one: the router routes a region
        // a strip, and the picture they make is still the page's content.
        let strips = vec![scan_strip(80.0, 420.0), scan_strip(420.0, 760.0)];
        let page = stamped_page(strips.clone());
        for s in &strips {
            assert!(route::share(&page, &s.bbox) < route::IMAGE_DOMINANT_COVERAGE);
        }
        assert_scan_title_stays_a_heading(strips, &[0, 1]);
    }

    /// A strip of a scan, `y0` to `y1`, the scan's width.
    fn scan_strip(y0: f64, y1: f64) -> ImagePlacement {
        ImagePlacement {
            bbox: BBox {
                x0: 40.0,
                y0,
                x1: 572.0,
                y1,
            },
            texty: true,
            ppi: None,
            bitonal: false,
            grid: (1000, 1000),
            key: None,
        }
    }

    /// Two stamped lines of a fax header over `scans`.
    fn stamped_page(scans: Vec<ImagePlacement>) -> Page {
        let mut glyphs = native(
            "RECEIVED 2026-03-14 BY FAX FROM BRANCH OFFICE 0042 PAGE 1 OF 3",
            40.0,
            30.0,
        );
        glyphs.extend(native(
            "FORWARDED TO THE LOANS DEPARTMENT FOR REVIEW AND FILING BY HAND",
            40.0,
            44.0,
        ));
        page(glyphs, scans)
    }

    /// OCR reads a loan agreement's title and body in the scan of a
    /// [`stamped_page`], one reading per region in `regions` (the words
    /// landing in each), then the whole page once: its title stays a
    /// heading either way.
    fn assert_scan_title_stays_a_heading(scans: Vec<ImagePlacement>, regions: &[usize]) {
        let mut words = vec![
            word("LOAN", [80.0, 100.0, 180.0, 130.0]),
            word("AGREEMENT", [190.0, 100.0, 400.0, 130.0]),
        ];
        let prose = [
            "This agreement is made between the lender and the borrower named",
            "below and sets out the terms on which the loan is advanced to them",
            "and the schedule on which it is to be repaid with interest accrued",
        ];
        for row in 0..30 {
            let mut x = 80.0;
            let y = 160.0 + row as f64 * 16.0;
            for w in prose[row % 3].split(' ') {
                let len = w.len() as f64 * 6.0;
                words.push(word(w, [x, y, x + len, y + 11.0]));
                x += len + 6.0;
            }
        }
        let route::Route::VlmRegions(routed) = route::decide(&stamped_page(scans.clone())).0 else {
            panic!("routed in regions");
        };
        assert_eq!(routed.len(), regions.len());
        let by_region: Vec<_> = regions
            .iter()
            .map(|&ri| {
                let b = routed[ri];
                let own = words
                    .iter()
                    .filter(|w| {
                        let y = (w.bbox[1] + w.bbox[3]) * 0.5;
                        y >= b.y0 && y < b.y1
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                (0, Crop::Region(ri), page_units(own))
            })
            .collect();
        for readings in [by_region, vec![(0, Crop::Page, page_units(words.clone()))]] {
            let mut doc = Document {
                pages: vec![stamped_page(scans.clone())],
            };
            let injected = inject(&mut doc, &readings);
            assert!(
                injected.iter().all(|r| r.taken > 0 && r.pasted.is_empty()),
                "{injected:?}"
            );
            let els = analysed(&mut doc, &injected);
            let title = els.iter().find(|e| e.text.contains("AGREEMENT")).unwrap();
            assert_eq!(
                (title.kind.as_str(), title.provenance),
                ("doco:SectionTitle", "ocr"),
                "{els:?}"
            );
        }
    }

    #[test]
    fn a_screens_heading_is_a_paragraph_beside_a_scan_on_its_page() {
        // A page of text holding both a pasted screenshot (a tenth of the
        // page) and a scan covering over half of it: each region's reading
        // is judged on its own, as a model's is. The screen's title is a
        // paragraph; the scan's stays a heading.
        let mut glyphs: Vec<Glyph> = native("Findings", 72.0, 40.0)
            .into_iter()
            .map(|g| Glyph {
                font_size: 18.0,
                ..g
            })
            .collect();
        let prose = [
            "The quarterly figures are summarised in the screen shown below",
            "where each account carries its balance and its last reference",
            "as the branch recorded them when the period was closed for review",
        ];
        for row in 0..6 {
            glyphs.extend(native(prose[row % 3], 72.0, 66.0 + row as f64 * 12.0));
        }
        let image = |x0, y0, x1, y1| ImagePlacement {
            bbox: BBox { x0, y0, x1, y1 },
            texty: true,
            ppi: None,
            bitonal: false,
            grid: (1000, 1000),
            key: None,
        };
        let shot = image(60.0, 150.0, 560.0, 260.0);
        let scan = image(30.0, 290.0, 590.0, 770.0);
        let doc = Document {
            pages: vec![page(glyphs, vec![shot, scan])],
        };
        assert!(route::share(&doc.pages[0], &scan.bbox) >= route::IMAGE_DOMINANT_COVERAGE);
        let route::Route::VlmRegions(regions) = route::decide(&doc.pages[0]).0 else {
            panic!("routed in regions");
        };
        let at = |b: BBox| regions.iter().position(|r| *r == b).unwrap();
        let mut shot_words = vec![
            word("Balance", [80.0, 160.0, 200.0, 190.0]),
            word("9,876.54", [210.0, 160.0, 340.0, 190.0]),
        ];
        let mut x = 80.0;
        for w in "Reference ABC-7781 opened at the branch office on the date shown".split(' ') {
            let len = w.len() as f64 * 6.0;
            shot_words.push(word(w, [x, 220.0, x + len, 231.0]));
            x += len + 6.0;
        }
        let mut scan_words = vec![
            word("LOAN", [80.0, 300.0, 180.0, 330.0]),
            word("AGREEMENT", [190.0, 300.0, 400.0, 330.0]),
        ];
        let body = [
            "This agreement is made between the lender and the borrower named",
            "below and sets out the terms on which the loan is advanced to them",
            "and the schedule on which it is to be repaid with interest accrued",
        ];
        for row in 0..20 {
            let mut x = 80.0;
            let y = 350.0 + row as f64 * 16.0;
            for w in body[row % 3].split(' ') {
                let len = w.len() as f64 * 6.0;
                scan_words.push(word(w, [x, y, x + len, y + 11.0]));
                x += len + 6.0;
            }
        }
        // Each region read on its own, or the whole page read once, its
        // words landing in the same two regions: the outline is the same.
        let by_region = [
            (
                0,
                Crop::Region(at(shot.bbox)),
                page_units(shot_words.clone()),
            ),
            (
                0,
                Crop::Region(at(scan.bbox)),
                page_units(scan_words.clone()),
            ),
        ];
        let whole = [(
            0,
            Crop::Page,
            page_units(shot_words.into_iter().chain(scan_words).collect()),
        )];
        for readings in [&by_region[..], &whole[..]] {
            let mut doc = doc.clone();
            let injected = inject(&mut doc, readings);
            assert!(injected.iter().all(|r| r.refused.is_none()), "{injected:?}");
            let els = analysed(&mut doc, &injected);
            let kind = |text: &str| {
                els.iter()
                    .find(|e| e.text.contains(text))
                    .map(|e| (e.kind.as_str(), e.provenance))
            };
            assert_eq!(kind("9,876.54"), Some(("doco:Paragraph", "ocr")), "{els:?}");
            assert_eq!(
                kind("AGREEMENT"),
                Some(("doco:SectionTitle", "ocr")),
                "{els:?}"
            );
            assert_eq!(kind("Findings").unwrap().0, "doco:SectionTitle");
        }
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
        let shots = [r0, r1].map(|bbox| ImagePlacement {
            bbox,
            texty: true,
            ppi: None,
            bitonal: false,
            grid: (1000, 1000),
            key: None,
        });
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
            words: vec![
                word("Account Holder", [600.0, 200.0, 640.0, 290.0]),
                // Most of the page level, so it is read as it stands.
                word("Name", [100.0, 500.0, 200.0, 520.0]),
                word("Date", [100.0, 540.0, 200.0, 560.0]),
            ],
        };
        inject(&mut doc, &[(0, Crop::Page, reading)]);
        assert_eq!(doc.pages[0].rotation, 0);
        let holder = doc.pages[0].glyphs.iter().filter(|g| g.origin.0 > 250.0);
        for g in holder.filter_map(|g| g.bbox) {
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
    fn a_region_reading_leaves_the_page_routed_as_drawn() {
        // A short page whose font resolves 45 of its 99 glyphs, under the
        // 100 glyph events the router needs before it judges their Unicode,
        // and two captures in glyph voids, A then B. A's reading adds glyph
        // events: counted, they route the page `BrokenText`, and B, which
        // nothing read, would lose its anchor.
        let mut glyphs = Vec::new();
        for row in 0..3 {
            glyphs.extend(native(&"a".repeat(33), 72.0, 40.0 + row as f64 * 14.0));
        }
        for g in glyphs.iter_mut().skip(45) {
            g.text = String::new();
        }
        for (i, g) in glyphs.iter_mut().enumerate() {
            g.draw_index = i;
        }
        let image = |y0, y1| ImagePlacement {
            bbox: BBox {
                x0: 60.0,
                y0,
                x1: 560.0,
                y1,
            },
            texty: true,
            ppi: None,
            bitonal: false,
            grid: (1000, 1000),
            key: None,
        };
        let (a, b) = (image(150.0, 300.0), image(400.0, 550.0));
        let mut doc = Document {
            pages: vec![page(glyphs, vec![a, b])],
        };
        let drawn = route::Route::VlmRegions(vec![a.bbox, b.bbox]);
        assert_eq!(route::decide(&doc.pages[0]).0, drawn);
        let words = vec![word("Balance", [80.0, 160.0, 200.0, 190.0])];
        let r = inject(&mut doc, &[(0, Crop::Region(0), page_units(words))]);
        assert!(r[0].taken > 0, "{r:?}");
        assert_eq!(
            route::decide(&doc.pages[0]).0,
            route::Route::Vlm(route::Reason::BrokenText),
            "the reading's glyphs tip the page as it now stands"
        );
        let a = crate::document::analyze_with(
            &mut doc,
            &[],
            &crate::document::AnalyzeOptions {
                emit_anchors: true,
                ..Default::default()
            },
        );
        assert_eq!(doc.pages[0].routing().route, drawn);
        let anchors: Vec<(&str, Option<BBox>)> = a
            .elements
            .iter()
            .filter(|e| e.text.starts_with("[[VLM"))
            .map(|e| (e.text.as_str(), e.bbox))
            .collect();
        assert_eq!(anchors, vec![("[[VLM:p0:r1]]", Some(b.bbox))]);
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
                    ppi: None,
                    bitonal: false,
                    grid: (1000, 1000),
                    key: None,
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

    /// Three lines of words on a landscape sheet scanned on a portrait page,
    /// their text running up the page (`angle` 90°) or down it (-90°), as an
    /// engine that says so reads them; `off` of them pushed off the page.
    fn sideways_words(angle: f64, off: usize) -> Vec<OcrWord> {
        let lines = [
            "The quick brown fox jumps over the dog",
            "Pack my box with five dozen liquor jugs",
            "How vexingly quick daft zebras jump",
        ];
        let mut words = Vec::new();
        for (k, line) in lines.iter().enumerate() {
            for w in run_words(line, 72.0, 100.0 + 14.0 * k as f64, 13.0, 6.0) {
                // The word's box on the 792 x 612 sheet, moved to where the
                // turned sheet shows its center: still the box, turned about
                // its center by `angle`.
                let [x0, y0, x1, y1] = w.bbox;
                let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                let (nx, ny) = if angle > 0.0 {
                    (cy, 792.0 - cx)
                } else {
                    (612.0 - cy, cx)
                };
                let (hw, hh) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
                let shift = if words.len() < off { 5000.0 } else { 0.0 };
                words.push(OcrWord {
                    bbox: [nx - hw + shift, ny - hh, nx + hw + shift, ny + hh],
                    angle: Some(angle),
                    ..w
                });
            }
        }
        words
    }

    #[test]
    fn a_page_read_sideways_reads_line_by_line() {
        let lines = [
            "The quick brown fox jumps over the dog",
            "Pack my box with five dozen liquor jugs",
            "How vexingly quick daft zebras jump",
        ];
        for (angle, turn) in [(90.0, 270), (-90.0, 90)] {
            let mut doc = blank_scan();
            let words = sideways_words(angle, 0);
            let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
            assert_eq!(r[0].taken, 22, "{angle}");
            assert_eq!(doc.pages[0].rotation, turn, "{angle}");
            // In the turned frame the words run level, and are laid as runs,
            // a space between them, not one by one about their centers.
            let glyphs = &doc.pages[0].glyphs;
            assert!(
                glyphs.iter().all(|g| g.rotation_deg.abs() < 1e-3),
                "{angle}"
            );
            let first: String = glyphs[..4].iter().map(|g| g.text.as_str()).collect();
            assert_eq!(first, "The ", "{angle}");
            assert_eq!(texts(&mut doc, &r), [lines.join(" ")], "{angle}");
        }
    }

    #[test]
    fn a_page_turned_a_quarter_for_a_reading_it_then_refuses_is_turned_back() {
        // Most of the words off the page, or all of them: the page, turned a
        // quarter for them, is turned back, not on by a further quarter.
        for angle in [90.0, -90.0] {
            for (off, refused) in [
                (15, Some("most of the reading could not be placed")),
                (22, Some("none of the reading could be placed")),
            ] {
                let mut doc = blank_scan();
                let before = doc.pages[0].images[0].bbox;
                let words = sideways_words(angle, off);
                let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
                assert_eq!((r[0].refused, r[0].taken), (refused, 0), "{angle} {off}");
                let p = &doc.pages[0];
                assert_eq!(p.rotation, 0, "{angle} {off}");
                assert_eq!((p.width, p.height), (612.0, 792.0), "{angle} {off}");
                let after = p.images[0].bbox;
                assert!(
                    (after.x0 - before.x0).abs() < 1e-9
                        && (after.y0 - before.y0).abs() < 1e-9
                        && (after.x1 - before.x1).abs() < 1e-9
                        && (after.y1 - before.y1).abs() < 1e-9,
                    "{angle} {off}: {after:?}"
                );
            }
        }
    }

    /// Four lines of upright text at the top of the page, a header over
    /// whatever the page holds below.
    fn upright_header() -> (Vec<OcrWord>, Vec<&'static str>) {
        let lines = vec![
            "Account number 4417-0093-21",
            "Date of birth 15-OCT-1997",
            "Policy holder Margaret Holloway",
            "Premium due on the first of March",
        ];
        let words = (lines.iter().enumerate())
            .flat_map(|(k, l)| run_words(l, 72.0, 60.0 + 20.0 * k as f64, 13.0, 6.0))
            .collect();
        (words, lines)
    }

    #[test]
    fn an_upright_page_holding_a_table_set_sideways_keeps_its_frame() {
        // Below the header, a table turned a quarter: eight rows of four
        // cells, each a word read along its angle, over two thirds of the
        // page's words and characters sideways.
        for angle in [90.0, -90.0] {
            let (mut words, header) = upright_header();
            let cells = ["SKU-60138", "Bracket", "Spindle", "Washer"];
            for row in 0..8 {
                for (col, cell) in cells.iter().enumerate() {
                    let (x, y) = (150.0 + 20.0 * row as f64, 300.0 + 80.0 * col as f64);
                    let len = 6.0 * cell.len() as f64;
                    words.push(OcrWord {
                        angle: Some(angle),
                        ..word(cell, [x - len / 2.0, y - 6.5, x + len / 2.0, y + 6.5])
                    });
                }
            }
            let mut doc = blank_scan();
            let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
            assert_eq!(r[0].taken, 32 + 18, "{angle}");
            assert_eq!(doc.pages[0].rotation, 0, "{angle}");
            let t = texts(&mut doc, &r).join("\n");
            let at = |s: &str| t.find(s).unwrap_or_else(|| panic!("{s} in {t}"));
            for pair in header.windows(2) {
                assert!(at(pair[0]) < at(pair[1]), "{angle}: {t}");
            }
            assert!(at(header[3]) < at("SKU-60138"), "{angle}: {t}");
        }
    }

    #[test]
    fn an_upright_page_holding_a_grid_of_digits_keeps_its_frame() {
        // A survey form's scores: single digits, each boxed taller than wide
        // and stated sideways by the engine, far more of them than lines.
        let (mut words, header) = upright_header();
        for row in 0..10 {
            for col in 0..8 {
                let (x, y) = (72.0 + 40.0 * col as f64, 200.0 + 25.0 * row as f64);
                let digit = ((row * 3 + col * 7) % 10).to_string();
                words.push(OcrWord {
                    angle: Some(-90.0),
                    ..word(&digit, [x, y, x + 10.0, y + 7.0])
                });
            }
        }
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        assert_eq!(doc.pages[0].rotation, 0);
        let t = texts(&mut doc, &r).join("\n");
        let at = |s: &str| t.find(s).unwrap_or_else(|| panic!("{s} in {t}"));
        for pair in header.windows(2) {
            assert!(at(pair[0]) < at(pair[1]), "{t}");
        }
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
    fn a_page_upside_down_holding_digits_is_turned_a_half() {
        // Single digits read at whatever angle the engine gives them, here
        // a quarter off, outnumbering the page's words: they do not count.
        let mut words = upside_down_words(0);
        for k in 0..60 {
            let (x, y) = (
                100.0 + 30.0 * (k % 12) as f64,
                250.0 - 25.0 * (k / 12) as f64,
            );
            words.push(OcrWord {
                angle: Some(90.0),
                ..word(&(k % 10).to_string(), [x, y, x + 10.0, y + 7.0])
            });
        }
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        assert_eq!(doc.pages[0].rotation, 180);
        let t = texts(&mut doc, &r).join("\n");
        assert!(t.starts_with("The quick brown fox"), "{t}");
    }

    #[test]
    fn a_page_upside_down_under_an_upright_fax_header_is_turned_a_half() {
        // A sheet fed upside down, two lines of it, and the header the fax
        // machine stamped upright on its pixels, read as one line: two fifths
        // of the text level, the rest upside down. Read in the page's frame,
        // its lines would come out last first.
        let header = "From: +1 555 0100 Nov 12 2025 14:02 Page 001 of 003";
        let mut words = vec![word(header, [30.0, 15.0, 330.0, 25.0])];
        words.extend(upside_down_words(0).into_iter().take(16));
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        assert_eq!(r[0].taken, 17);
        assert_eq!(doc.pages[0].rotation, 180);
        let t = texts(&mut doc, &r).join("\n");
        let at = |s: &str| t.find(s).unwrap_or_else(|| panic!("{s} in {t}"));
        assert!(at("The quick brown fox") < at("Pack my box with"), "{t}");
        assert!(t.contains(header), "{t}");
    }

    #[test]
    fn a_page_upside_down_with_a_table_set_sideways_is_turned_a_half() {
        // Three lines running up the page, as a table set sideways on it
        // would, and two upside down: more of the text sideways, but the
        // lines upside down are the page's own text. Kept in the page's
        // frame, they would come out last first.
        let mut words = sideways_words(90.0, 0);
        words.extend(upside_down_words(0).into_iter().take(16).map(|w| OcrWord {
            text: w.text.to_uppercase(),
            ..w
        }));
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        assert_eq!(doc.pages[0].rotation, 180);
        let t = texts(&mut doc, &r).join("\n");
        let at = |s: &str| t.find(s).unwrap_or_else(|| panic!("{s} in {t}"));
        assert!(at("THE QUICK BROWN FOX") < at("PACK MY BOX WITH"), "{t}");
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
                    ppi: None,
                    bitonal: false,
                    grid: (1000, 1000),
                    key: None,
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
                    .map(|&bbox| ImagePlacement {
                        bbox,
                        texty: true,
                        ppi: None,
                        bitonal: false,
                        grid: (1000, 1000),
                        key: None,
                    })
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

    /// An element a model's reading of a region is spliced in as: with the
    /// region's box, in the display frame.
    fn spliced(b: BBox, evidence: &'static str) -> fluree_doc_model::Element {
        fluree_doc_model::Element {
            page: 0,
            bbox: Some(b),
            ..fluree_doc_model::Element::new("doco:Paragraph", "Balance", "vlm", evidence)
        }
    }

    #[test]
    fn an_empty_region_reading_a_model_then_read_is_not_reported() {
        // Also on a page drawn turned: the model's reading is spliced in the
        // display frame, the region read in the page's own.
        for rotation in [0, 90] {
            let mut doc = page_with_regions(&[UPPER, LOWER]);
            doc.pages[0].rotation = rotation;
            let r = inject(
                &mut doc,
                &[
                    (0, Crop::Region(0), page_units(Vec::new())),
                    (0, Crop::Region(1), page_units(Vec::new())),
                ],
            );
            let shown = |b: BBox| doc.pages[0].to_display(b);
            assert_eq!(r[0].regions, [shown(UPPER)], "{rotation}");
            assert_eq!(readings(&r, &[]).len(), 2, "{rotation}");
            for evidence in ["region-splice", "region-chart"] {
                let listed = readings(&r, &[spliced(shown(UPPER), evidence)]);
                assert_eq!(
                    listed.iter().map(|o| o.crop.as_str()).collect::<Vec<_>>(),
                    ["r1"],
                    "{evidence} {rotation}"
                );
            }
            // Another element in the same box is not a model's reading of it.
            assert_eq!(
                readings(&r, &[spliced(shown(UPPER), "layout-block")]).len(),
                2,
                "{rotation}"
            );
        }
    }

    #[test]
    fn an_empty_whole_reading_of_regions_a_model_all_read_is_not_reported() {
        // A whole reading of a page of text is a reading of its regions: a
        // model that read each of them replaced it, one that read only one
        // did not.
        let mut doc = page_with_regions(&[UPPER, LOWER]);
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(Vec::new()))]);
        let shown = |b: BBox| doc.pages[0].to_display(b);
        assert_eq!(r[0].regions, [shown(UPPER), shown(LOWER)]);
        let listed = |els: &[fluree_doc_model::Element]| {
            readings(&r, els)
                .iter()
                .map(|o| (o.crop.clone(), o.words))
                .collect::<Vec<_>>()
        };
        assert_eq!(listed(&[]), [("full".to_string(), 0)]);
        let upper = spliced(shown(UPPER), "region-splice");
        assert_eq!(
            listed(std::slice::from_ref(&upper)),
            [("full".to_string(), 0)]
        );
        let lower = spliced(shown(LOWER), "region-chart");
        assert!(listed(&[upper, lower]).is_empty());
    }

    #[test]
    fn an_empty_whole_reading_of_a_region_no_model_reads_is_reported() {
        // A routed region and a capture too small for a model: a whole
        // reading is a reading of both, and a model's reading of the first
        // leaves the second read by nothing but the empty reading.
        let small = BBox {
            x0: 60.0,
            y0: 430.0,
            x1: 260.0,
            y1: 460.0,
        };
        let mut doc = page_with_regions(&[UPPER, small]);
        assert_eq!(
            route::decide(&doc.pages[0]).0,
            Route::VlmRegions(vec![UPPER])
        );
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(Vec::new()))]);
        let shown = |b: BBox| doc.pages[0].to_display(b);
        assert_eq!(r[0].regions, [shown(UPPER), shown(small)]);
        let model = spliced(shown(UPPER), "region-splice");
        let listed: Vec<(String, usize)> = readings(&r, &[model])
            .iter()
            .map(|o| (o.crop.clone(), o.words))
            .collect();
        assert_eq!(listed, [("full".to_string(), 0)]);
    }

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
            ppi: None,
            bitonal: false,
            grid: (1000, 1000),
            key: None,
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
                    ppi: None,
                    bitonal: false,
                    grid: (1000, 1000),
                    key: None,
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
                    ppi: None,
                    bitonal: false,
                    grid: (1000, 1000),
                    key: None,
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

    /// A one-page Letter PDF: twenty lines of Helvetica at the top, so the
    /// page has a text layer, then `content`, drawing the image masks
    /// `masks` names, each `(name, width, height)` pixels, all ink. `/F3`
    /// is a Type3 font, whose one glyph, `x`, a cross, has no outline.
    fn form_pdf(content: &str, masks: &[(&str, u32, u32)]) -> Vec<u8> {
        let text: String = (0..20)
            .map(|i| {
                format!(
                    "BT /F1 9 Tf 40 {} Td (The quarterly statement lists every account opened this year.) Tj ET\n",
                    770 - 12 * i
                )
            })
            .collect();
        let content = format!("{text}{content}");
        let xobjects: String = masks
            .iter()
            .enumerate()
            .map(|(i, (name, _, _))| format!("/{name} {} 0 R ", 5 + i))
            .collect();
        let mut objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << \
                 /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> \
                 /F3 << /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                 /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /x {cross} 0 R >> \
                 /Encoding << /Type /Encoding /Differences [120 /x] >> \
                 /FirstChar 120 /LastChar 120 /Widths [600] /ToUnicode {cmap} 0 R >> >> \
                 /XObject << {xobjects}>> >> /Contents 4 0 R >>",
                cross = 5 + masks.len(),
                cmap = 6 + masks.len()
            )
            .into_bytes(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            )
            .into_bytes(),
        ];
        for (_, w, h) in masks {
            let data = vec![0u8; w.div_ceil(8) as usize * *h as usize];
            objects.push(
                [
                    format!(
                        "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ImageMask true \
                         /BitsPerComponent 1 /Length {} >>\nstream\n",
                        data.len()
                    )
                    .into_bytes(),
                    data,
                    b"\nendstream".to_vec(),
                ]
                .concat(),
            );
        }
        let cross = "600 0 0 0 600 600 d1 100 100 m 500 500 l 100 500 m 500 100 l S";
        let cmap = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
                    1 begincodespacerange <00> <FF> endcodespacerange \
                    1 beginbfchar <78> <0078> endbfchar endcmap \
                    CMapName currentdict /CMap defineresource pop end end";
        for stream in [cross, cmap] {
            objects.push(
                format!(
                    "<< /Length {} >>\nstream\n{stream}\nendstream",
                    stream.len()
                )
                .into_bytes(),
            );
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n", i + 1).into_bytes());
            out.extend(body);
            out.extend(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).into_bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").into_bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .into_bytes(),
        );
        out
    }

    /// Every crop [`ocr_jobs`] asks for, as `(page, tag)`.
    fn crops_of(doc: &Document) -> Vec<(usize, String)> {
        ocr_jobs(doc, None, 0)
            .0
            .into_iter()
            .flat_map(|(page, crops)| match crops {
                None => vec![(page, "full".to_string())],
                Some(l) => l.into_iter().map(|(t, _)| (page, t)).collect(),
            })
            .collect()
    }

    /// A pasted capture, 200 × 30 pixels drawn at 200 × 30 pt, at `(x, y)`.
    fn capture_at(x: u32, y: u32) -> String {
        format!("q 200 0 0 30 {x} {y} cm /Cap Do Q\n")
    }

    #[test]
    fn a_form_of_checkbox_masks_asks_the_engine_for_nothing() {
        // 300 checkboxes, each an 8 × 8 image mask drawn 8 pt square, under
        // the text: each would be a crop of its own, and its reading a `■`.
        let boxes: String = (0..300)
            .map(|i| {
                format!(
                    "q 8 0 0 8 {} {} cm /Cb Do Q\n",
                    50 + (i % 15) * 32,
                    100 + (i / 15) * 24
                )
            })
            .collect();
        let doc =
            crate::extract_bytes(form_pdf(&boxes, &[("Cb", 8, 8), ("Cap", 200, 30)])).unwrap();
        assert_eq!(doc.pages[0].images.len(), 300);
        assert!(crops_of(&doc).is_empty(), "{:?}", crops_of(&doc).len());
        // The same form with a capture an engine can read: that one crop.
        let with = format!("{boxes}{}", capture_at(50, 40));
        let doc = crate::extract_bytes(form_pdf(&with, &[("Cb", 8, 8), ("Cap", 200, 30)])).unwrap();
        assert_eq!(crops_of(&doc), [(0, "r0".to_string())]);
    }

    #[test]
    fn rules_stretched_from_a_pixel_are_not_read() {
        // 40 underlines, each a 1 × 1 mask stretched 200 pt wide and 1 high.
        let rules: String = (0..40)
            .map(|i| {
                format!(
                    "q 200 0 0 1 {} {} cm /Ru Do Q\n",
                    50 + (i % 2) * 260,
                    100 + (i / 2) * 20
                )
            })
            .collect();
        let doc = crate::extract_bytes(form_pdf(&rules, &[("Ru", 1, 1)])).unwrap();
        assert_eq!(doc.pages[0].images.len(), 40);
        assert!(crops_of(&doc).is_empty());
    }

    #[test]
    fn an_image_drawn_twice_in_one_place_is_read_once() {
        let twice = format!(
            "{}{}{}",
            capture_at(50, 40),
            capture_at(50, 40),
            capture_at(300, 40)
        );
        let doc = crate::extract_bytes(form_pdf(&twice, &[("Cap", 200, 30)])).unwrap();
        assert_eq!(doc.pages[0].images.len(), 3);
        // Two regions, the third drawing a copy of the first, which
        // reads it for both ([`copies`]).
        assert_eq!(route::ocr_regions(&doc.pages[0]).len(), 2);
        assert_eq!(crops_of(&doc), [(0, "r0".to_string())]);
        assert_eq!(copies(&doc, None), [((0, 1), (0, 0))]);
    }

    #[test]
    fn an_image_drawn_again_at_its_size_is_read_once_for_every_drawing() {
        // The capture three times at one size, once wider, another image
        // drawn at the capture's size, the capture mirrored, and the
        // capture cut short by the page's edge.
        let content = format!(
            "{}{}{}q 220 0 0 30 50 200 cm /Cap Do Q\nq 200 0 0 30 300 200 cm /Two Do Q\n\
             q -200 0 0 30 250 300 cm /Cap Do Q\nq 200 0 0 30 500 400 cm /Cap Do Q\n",
            capture_at(50, 40),
            capture_at(300, 40),
            capture_at(50, 100),
        );
        let mut doc =
            crate::extract_bytes(form_pdf(&content, &[("Cap", 200, 30), ("Two", 200, 32)]))
                .unwrap();
        let tags =
            |doc: &Document| -> Vec<String> { crops_of(doc).into_iter().map(|(_, t)| t).collect() };
        assert_eq!(tags(&doc), ["r0", "r3", "r4", "r5", "r6"]);
        assert_eq!(copies(&doc, None), [((0, 1), (0, 0)), ((0, 2), (0, 0))]);
        assert!(copies(&doc, Some(&[1])).is_empty());

        // Past the limit, a copy counts only through what it copies: kept
        // with it, and left unread with it. A page routed a region of its
        // own comes first, and with one crop allowed the capture and its
        // two copies, the wider one, the other image, the mirrored one and
        // the one cut short are left unread.
        assert_eq!(ocr_jobs(&doc, None, 5), (ocr_jobs(&doc, None, 0).0, 0));
        let mut routed = page(
            native("The quarterly statement lists every account.", 40.0, 40.0),
            vec![ImagePlacement {
                bbox: BBox {
                    x0: 50.0,
                    y0: 400.0,
                    x1: 550.0,
                    y1: 600.0,
                },
                texty: true,
                ppi: None,
                bitonal: false,
                grid: (1000, 400),
                key: None,
            }],
        );
        routed.index = 1;
        let mut two = doc.clone();
        two.pages.push(routed);
        assert_eq!(ocr_jobs(&two, None, 1).1, 7);
        assert_eq!(ocr_jobs(&two, None, 2).1, 4);

        // The capture's reading, in its crop's pixels, is laid on each
        // drawing where it is drawn; one in page units is of its own
        // region only.
        let reading = |coords| WordReading {
            coords,
            width: Some(100.0),
            height: Some(100.0),
            engine: None,
            words: vec![OcrWord {
                text: "Account".into(),
                bbox: [10.0, 30.0, 90.0, 70.0],
                angle: None,
                conf: Some(0.99),
            }],
        };
        let page_units = with_copies(
            &doc,
            None,
            vec![(0, Crop::Region(0), reading(Coords::Page))],
        );
        assert_eq!(page_units.len(), 1);
        let readings = with_copies(&doc, None, vec![(0, Crop::Region(0), reading(Coords::Px))]);
        let crops: Vec<Crop> = readings.iter().map(|r| r.1).collect();
        assert_eq!(crops, [Crop::Region(0), Crop::Region(1), Crop::Region(2)]);
        let regions = route::ocr_regions(&doc.pages[0]);
        let r = inject(&mut doc, &readings);
        for (r, region) in r.iter().zip(&regions) {
            assert_eq!(r.taken, 1, "{r:?}");
            let c = r.chars[0];
            let (cx, cy) = ((c.x0 + c.x1) * 0.5, (c.y0 + c.y1) * 0.5);
            assert!(region.contains(cx, cy), "{r:?} {region:?}");
        }
        // A region with a reading of its own keeps it.
        let own = with_copies(
            &doc,
            None,
            vec![
                (0, Crop::Region(0), reading(Coords::Px)),
                (0, Crop::Region(2), reading(Coords::Page)),
            ],
        );
        assert_eq!(own.len(), 3);
        assert_eq!(own[2].2.coords, Coords::Page);
    }

    #[test]
    fn a_field_frame_with_a_value_typed_in_it_lends_no_copy_its_reading() {
        // One 200 × 40 stencil, a field's frame, drawn three times; only
        // the first holds a value, set as text over it. Its crop shows the
        // value: laid on the two empty fields, it would fill them with it.
        let content = "q 200 0 0 40 50 300 cm /Fr Do Q\n\
                       BT /F1 9 Tf 60 315 Td (Approved 2026) Tj ET\n\
                       q 200 0 0 40 50 200 cm /Fr Do Q\n\
                       q 200 0 0 40 50 100 cm /Fr Do Q\n";
        let doc = crate::extract_bytes(form_pdf(content, &[("Fr", 200, 40)])).unwrap();
        assert_eq!(route::ocr_regions(&doc.pages[0]).len(), 3);
        assert_eq!(doc.pages[0].images[0].key, None);
        // The two empty frames show the same pixels, and still read once.
        assert_eq!(copies(&doc, None), [((0, 2), (0, 1))]);
        let tags: Vec<String> = crops_of(&doc).into_iter().map(|(_, t)| t).collect();
        assert_eq!(tags, ["r0", "r1"]);

        // A tick drawn over a box, as a path, or another image drawn
        // across one, makes it a picture of its own too.
        let content = "q 200 0 0 40 50 300 cm /Fr Do Q\n\
                       q 200 0 0 40 50 200 cm /Fr Do Q\n\
                       q 200 0 0 40 50 100 cm /Fr Do Q\n\
                       q 200 0 0 40 300 100 cm /Fr Do Q\n\
                       60 310 m 70 330 l S\n\
                       q 20 0 0 20 60 210 cm /Ck Do Q\n";
        let doc =
            crate::extract_bytes(form_pdf(content, &[("Fr", 200, 40), ("Ck", 20, 20)])).unwrap();
        let keys: Vec<bool> = (doc.pages[0].images.iter())
            .map(|i| i.key.is_some())
            .collect();
        assert_eq!(keys, [false, false, true, true, false]);
        assert_eq!(copies(&doc, None), [((0, 3), (0, 2))]);
    }

    #[test]
    fn a_cross_a_type3_font_sets_on_a_box_lends_no_copy_its_reading() {
        // Four boxes from one stencil. A Type3 cross, a glyph with no
        // outline, is set at the first box's very corner, and another just
        // under the second, its letter rising into the box: those two boxes
        // show more than the stencil, the last two are one picture.
        let content = "q 200 0 0 40 50 300 cm /Fr Do Q\n\
                       q 200 0 0 40 50 200 cm /Fr Do Q\n\
                       q 200 0 0 40 50 100 cm /Fr Do Q\n\
                       q 200 0 0 40 300 100 cm /Fr Do Q\n\
                       BT /F3 12 Tf 50 300 Td (x) Tj ET\n\
                       BT /F3 12 Tf 120 198 Td (x) Tj ET\n";
        let doc = crate::extract_bytes(form_pdf(content, &[("Fr", 200, 40)])).unwrap();
        let crosses: Vec<&Glyph> = (doc.pages[0].glyphs.iter())
            .filter(|g| g.text == "x")
            .collect();
        assert_eq!(crosses.len(), 2);
        let keys: Vec<bool> = (doc.pages[0].images.iter())
            .map(|i| i.key.is_some())
            .collect();
        assert_eq!(keys, [false, false, true, true]);
        assert_eq!(copies(&doc, None), [((0, 3), (0, 2))]);
    }

    #[test]
    fn a_tick_over_a_box_on_a_page_set_sideways_lends_no_copy_its_reading() {
        // A page whose text runs up it, read turned a quarter, with three
        // boxes from one stencil and a tick painted over the second: the
        // tick is turned with the boxes, and still shows on its box.
        let sideways: String = (0..200)
            .map(|i| {
                format!(
                    "BT /F1 9 Tf 0 1 -1 0 {} 420 Tm (The quarterly statement lists every account.) Tj ET\n",
                    20 + i
                )
            })
            .collect();
        let content = format!(
            "{sideways}q 200 0 0 40 50 100 cm /Fr Do Q\n\
             q 200 0 0 40 300 100 cm /Fr Do Q\n\
             q 200 0 0 40 50 200 cm /Fr Do Q\n\
             310 110 m 330 130 l S\n"
        );
        let doc = crate::extract_bytes(form_pdf(&content, &[("Fr", 200, 40)])).unwrap();
        assert_ne!(doc.pages[0].rotation, 0);
        let keys: Vec<bool> = (doc.pages[0].images.iter())
            .map(|i| i.key.is_some())
            .collect();
        assert_eq!(keys, [true, false, true]);
        assert_eq!(copies(&doc, None), [((0, 2), (0, 0))]);
    }

    #[test]
    fn a_sprite_drawn_through_two_clips_is_read_twice() {
        // One 200 × 40 image drawn twice at one size, each time through a
        // clip showing one of its halves: two pictures. Drawn whole, twice,
        // and once through a clip that leaves it whole, it is one.
        let content = "q 50 300 200 20 re W n 200 0 0 40 50 300 cm /Sp Do Q\n\
                       q 50 220 200 20 re W n 200 0 0 40 50 200 cm /Sp Do Q\n\
                       q 200 0 0 40 50 100 cm /Sp Do Q\n\
                       q 290 90 220 60 re W n 200 0 0 40 300 100 cm /Sp Do Q\n\
                       q 300 300 m 500 300 l 400 340 l h W n 200 0 0 40 300 300 cm /Sp Do Q\n";
        let doc = crate::extract_bytes(form_pdf(content, &[("Sp", 200, 40)])).unwrap();
        let keys: Vec<bool> = (doc.pages[0].images.iter())
            .map(|i| i.key.is_some())
            .collect();
        assert_eq!(keys, [false, false, true, true, false]);
        assert_eq!(route::ocr_regions(&doc.pages[0]).len(), 5);
        assert_eq!(copies(&doc, None), [((0, 3), (0, 2))]);
    }

    #[test]
    fn a_form_of_checkboxes_drawn_from_one_mask_is_read_once() {
        // 200 checkboxes, one 24 × 24 stencil drawn 12 pt square under the
        // prose, a label beside each: one crop, its reading lent to all.
        let boxes: String = (0..200)
            .map(|i| {
                let (x, y) = (40 + (i % 8) * 70, 60 + (i / 8) * 18);
                format!(
                    "q 12 0 0 12 {x} {y} cm /Cb Do Q\nBT /F1 9 Tf {} {} Td (Yes) Tj ET\n",
                    x + 16,
                    y + 2
                )
            })
            .collect();
        let doc = crate::extract_bytes(form_pdf(&boxes, &[("Cb", 24, 24)])).unwrap();
        assert_eq!(route::ocr_regions(&doc.pages[0]).len(), 200);
        assert_eq!(copies(&doc, None).len(), 199);
        assert_eq!(crops_of(&doc), [(0, "r0".to_string())]);
    }

    #[test]
    fn an_image_on_pages_turned_unlike_lends_no_copy() {
        // One image, one key, one square box, on two pages: the second
        // turned a quarter to be read upright. Its crop shows the image
        // turned, and an upright reading's pixels would land in the wrong
        // frame.
        let square = ImagePlacement {
            bbox: BBox {
                x0: 100.0,
                y0: 200.0,
                x1: 160.0,
                y1: 260.0,
            },
            texty: true,
            ppi: None,
            bitonal: false,
            grid: (120, 120),
            key: Some(7),
        };
        let text = || native("The quarterly statement lists every account.", 40.0, 40.0);
        let p0 = page(text(), vec![square]);
        let mut p1 = page(text(), vec![square]);
        p1.index = 1;
        let mut doc = Document {
            pages: vec![p0, p1],
        };
        assert_eq!(copies(&doc, None), [((1, 0), (0, 0))]);
        doc.pages[1].rotation = 90;
        assert!(copies(&doc, None).is_empty());
    }

    #[test]
    fn past_its_limit_the_engine_reads_what_loses_most_unread() {
        // Page 0: three small captures. Page 1: a scan, read whole. Page 2:
        // a routed region and a small capture.
        let small = |y: f64| ImagePlacement {
            bbox: BBox {
                x0: 50.0,
                y0: y,
                x1: 150.0,
                y1: y + 30.0,
            },
            texty: true,
            ppi: None,
            bitonal: false,
            grid: (200, 60),
            key: None,
        };
        let text = || native("The quarterly statement lists every account.", 40.0, 40.0);
        let mut p0 = page(text(), vec![small(200.0), small(300.0), small(400.0)]);
        p0.index = 0;
        let mut p1 = page(Vec::new(), vec![full_page_image()]);
        p1.index = 1;
        let routed = ImagePlacement {
            bbox: BBox {
                x0: 50.0,
                y0: 400.0,
                x1: 550.0,
                y1: 600.0,
            },
            texty: true,
            ppi: None,
            bitonal: false,
            grid: (1000, 400),
            key: None,
        };
        let mut p2 = page(text(), vec![small(200.0), routed]);
        p2.index = 2;
        let doc = Document {
            pages: vec![p0, p1, p2],
        };
        let tags = |jobs: &OcrJobs| -> Vec<(usize, Vec<String>)> {
            jobs.iter()
                .map(|(p, c)| {
                    let tags = match c {
                        None => vec!["full".to_string()],
                        Some(l) => l.iter().map(|(t, _)| t.clone()).collect(),
                    };
                    (*p, tags)
                })
                .collect()
        };
        let (all, dropped) = ocr_jobs(&doc, None, 0);
        assert_eq!(dropped, 0);
        assert_eq!(
            all.iter()
                .map(|(_, c)| c.as_ref().map_or(1, Vec::len))
                .sum::<usize>(),
            6
        );
        let (kept, dropped) = ocr_jobs(&doc, None, 3);
        assert_eq!(dropped, 3);
        assert_eq!(
            tags(&kept),
            [
                (0, vec!["r0".to_string()]),
                (1, vec!["full".to_string()]),
                (2, vec!["r0".to_string()]),
            ],
            "the scan, the routed region, then the first small capture"
        );
        let (kept, _) = ocr_jobs(&doc, None, 2);
        assert_eq!(
            tags(&kept),
            [(1, vec!["full".to_string()]), (2, vec!["r0".to_string()])]
        );
    }

    /// Lines of the given sizes in points, each boxed as tall as a detector
    /// boxes its letters at that size, as long as [`lines_every`] boxes it.
    fn boxed<'a>(lines: &[(&'a str, f64)]) -> Vec<(&'a str, f64)> {
        lines
            .iter()
            .map(|&(t, pt)| (t, box_tall(t, pt, 6.0 * t.chars().count() as f64)))
            .collect()
    }

    /// How tall a detector boxes `text` set at `pt`, in a box `len` long:
    /// the height its aspect gives, found by iterating.
    fn box_tall(text: &str, pt: f64, len: f64) -> f64 {
        (0..32).fold(pt, |h, _| pt * box_height_em(text, len / h))
    }

    /// A form's lines as a detector boxes them, one box a line, each `h`
    /// tall, two lines' height apart.
    fn detected_lines(lines: &[(&str, f64)]) -> Vec<OcrWord> {
        lines_every(lines, 26.0)
    }

    /// Lines as a detector boxes them, one box a line, each `h` tall, their
    /// tops `pitch` apart.
    fn lines_every(lines: &[(&str, f64)], pitch: f64) -> Vec<OcrWord> {
        lines
            .iter()
            .enumerate()
            .map(|(k, &(text, h))| {
                let y = 72.0 + pitch * k as f64;
                let w = 6.0 * text.chars().count() as f64;
                word(text, [72.0, y, 72.0 + w, y + h])
            })
            .collect()
    }

    /// A document of one page per reading, every page a scan.
    fn scans(pages: usize) -> Document {
        Document {
            pages: (0..pages)
                .map(|index| Page {
                    index,
                    ..page(Vec::new(), vec![full_page_image()])
                })
                .collect(),
        }
    }

    const FORM: [&str; 6] = [
        "Account number 4417-0093-21",
        "Date of birth 15-OCT-1997",
        "Policy holder Margaret Holloway",
        "Premium due on the first of March",
        "Beneficiary Jonathan Pemberton",
        "Coverage amount 250,000 dollars",
    ];

    fn headings(els: &[fluree_doc_model::Element]) -> Vec<&str> {
        els.iter()
            .filter(|e| e.kind == "doco:SectionTitle")
            .map(|e| e.text.as_str())
            .collect()
    }

    #[test]
    fn a_line_boxed_taller_for_its_descenders_is_not_a_heading() {
        // One size of type throughout, and one line with descenders among
        // lines with none, boxed 13% taller than they are, as the built-in
        // engine boxes a form in 11 pt at 150 dpi (26 px over 23): over the
        // band of a size taken whatever the letters.
        let lines: Vec<(&str, f64)> = [
            "Account number 4417-0093-21",
            "Date of birth 15-OCT-1997",
            "Policy holder Margaret Holloway",
            "Premium due on the first of March",
            "Contract reference AB-1180",
            "Branch office North Street 14",
            "Renewal date 01-MAR-2027",
            "Total amount 1,250.00 EUR",
        ]
        .iter()
        .map(|&t| (t, if t.starts_with("Policy") { 14.7 } else { 13.0 }))
        .collect();
        let mut doc = blank_scan();
        let r = inject(
            &mut doc,
            &[(0, Crop::Page, page_units(detected_lines(&lines)))],
        );
        let els = analysed(&mut doc, &r);
        assert!(
            !headings(&els).iter().any(|h| h.contains("Policy holder")),
            "{els:?}"
        );
    }

    #[test]
    fn a_heading_set_larger_on_a_scan_is_still_a_heading() {
        // Below the page's first lines, where only its size makes it one.
        let mut lines: Vec<(&str, f64)> = FORM[..4].iter().map(|&t| (t, 12.0)).collect();
        // A sixth larger than the body, over what a heading set larger by
        // its size alone needs to be.
        lines.push(("Policy schedule", 14.0));
        lines.extend(FORM[4..].iter().map(|&t| (t, 12.0)));
        let mut doc = blank_scan();
        let r = inject(
            &mut doc,
            &[(0, Crop::Page, page_units(detected_lines(&boxed(&lines))))],
        );
        let els = analysed(&mut doc, &r);
        assert_eq!(headings(&els), ["Policy schedule"], "{els:?}");
    }

    /// A paragraph's lines, each as long as the others.
    const CLAUSE: [&str; 9] = [
        "This agreement is made between the lender and the borrower named",
        "below and sets out the terms on which the loan is advanced to them",
        "and the schedule on which it is to be repaid with interest accrued",
        "at the rate stated in the schedule attached to this agreement here",
        "and the borrower agrees to repay the loan in monthly instalments a",
        "set out in the schedule with the interest accrued on each of them",
        "until the whole of the loan and its interest have been repaid and",
        "the lender may call the loan in should any instalment go unpaid a",
        "month after the date on which it fell due under this agreement now",
    ];

    #[test]
    fn a_heading_with_no_descenders_over_prose_that_has_them_is_a_heading() {
        // A 14 pt heading over 11.5 pt prose. Its letters stop at the
        // baseline, and the engine boxes it 0.96 em tall, 13.4 pt: within a
        // tenth of the prose's boxes, which reach below their descenders,
        // and taken as one height whatever their letters, it was set to the
        // body.
        let mut lines = boxed(&CLAUSE[..4].iter().map(|&t| (t, 11.5)).collect::<Vec<_>>());
        lines.push(("Exclusions", 13.4));
        lines.extend(boxed(
            &CLAUSE[4..].iter().map(|&t| (t, 11.5)).collect::<Vec<_>>(),
        ));
        let mut doc = blank_scan();
        let r = inject(
            &mut doc,
            &[(0, Crop::Page, page_units(lines_every(&lines, 16.0)))],
        );
        let els = analysed(&mut doc, &r);
        assert_eq!(headings(&els), ["Exclusions"], "{els:?}");
    }

    #[test]
    fn a_detector_box_is_as_tall_as_its_letters_make_it() {
        // The median height, in ems, the built-in engine boxes each line of
        // prose at over five fonts, 8 to 18 pt, 100 to 300 dpi.
        for (text, measured) in [
            ("Policy holder Margaret Holloway", 1.09),
            ("Terms of cover and exclusions", 1.0),
            ("ACCOUNT SUMMARY", 0.96),
            ("paying your premium", 1.09),
            ("we serve on mere success", 0.87),
        ] {
            let em = box_height_em(text, 14.0);
            assert!((em - measured).abs() < 0.03, "{text}: {em}");
        }
    }

    #[test]
    fn a_short_or_accented_line_reads_at_the_size_of_the_prose() {
        // The median height, in ems of its type, and aspect the built-in
        // engine boxes each line at in paragraphs set 1.3 em apart, over
        // DejaVu Sans and Serif, Liberation Sans, Serif and Mono at 10 to 12
        // pt, 200 and 300 dpi. A word alone on its line boxes taller for its
        // type than a line of prose, and a mark over or under its letters, or
        // a bracket, reaches further than its letters: each must read within
        // a tenth of its type, short of the margin a heading needs.
        let prose = 1.08
            / box_height_em(
                "Nous vous informons que votre contrat arrive à échéance prochainement.",
                33.7,
            );
        for (text, aspect, measured) in [
            ("Reçu", 2.1, 1.24),
            ("Année", 3.0, 1.08),
            ("Été 2026", 3.9, 1.14),
            ("Änderung", 3.8, 1.26),
            ("Größe", 2.8, 1.13),
            ("Noël", 2.2, 1.15),
            ("Ça va", 2.5, 1.18),
            ("zéro", 2.0, 1.17),
            ("Total", 2.3, 1.13),
            ("Bill to:", 3.1, 1.08),
            ("x; y", 1.9, 1.15),
            ("(1,250.00)", 4.7, 1.09),
            ("$ 98.40", 3.6, 1.08),
            ("Name / Surname", 8.2, 0.91),
            (
                "le détail des garanties souscrites et la date limite de règlement.",
                29.0,
                1.06,
            ),
            // Set a fifth larger than the paragraphs around them.
            ("Échéance", 4.2, 1.11),
            ("Über uns", 3.8, 1.19),
            ("Terms", 3.0, 1.02),
        ] {
            let read = measured / box_height_em(text, aspect) / prose;
            assert!((read - 1.0).abs() < 0.1, "{text}: {read}");
        }
    }

    #[test]
    fn a_line_opening_on_an_accented_capital_reads_at_the_size_of_its_paragraph() {
        // The median height, in ems of its type, and aspect the built-in
        // engine boxes each line at, in 11 pt paragraphs set 1.3 em apart,
        // over DejaVu Sans and Serif, Liberation Sans and Serif, at 150, 200
        // and 300 dpi: each line with its capital accented, and the same
        // line without the accent. The mark over one capital of a line
        // raised its box about half as much as the mark over a word's: sized
        // as a word's, these lines read 3% under the others, and pulled the
        // page's median down until `Reçu` at body size read as a heading
        // over it.
        use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};
        let mut read: Vec<f64> = [
            (
                "À compter du premier janvier, votre contrat sera renouvelé par tacite reconduction.",
                1.109,
                1.076,
                36.0,
            ),
            (
                "Équipe de gestion : votre conseiller reste joignable par téléphone ou par courrier.",
                1.152,
                1.097,
                35.0,
            ),
            (
                "État des lieux : la visite aura lieu le jeudi suivant, à partir de quatorze heures.",
                1.129,
                1.076,
                33.6,
            ),
            (
                "Éventuellement, vous pouvez résilier avant la date d'échéance du contrat en cours.",
                1.146,
                1.043,
                36.1,
            ),
        ]
        .iter()
        .map(|&(accented, with, without, aspect)| {
            let plain: String = accented.nfd().filter(|c| !is_combining_mark(*c)).collect();
            (with / box_height_em(accented, aspect)) / (without / box_height_em(&plain, aspect))
        })
        .collect();
        read.sort_by(f64::total_cmp);
        let median = (read[1] + read[2]) / 2.0;
        assert!((median - 1.0).abs() < 0.02, "{read:?}");
        // Only a mark reaches over a capital: a line of CJK, as tall as its
        // characters, is not one with a mark.
        let cjk = box_height_em("保险合同续期通知书", 14.0);
        assert!(
            (cjk - (BOX_BASE_EM + BOX_INK_SHARE * CJK_HEIGHT_EM)).abs() < 1e-9,
            "{cjk}"
        );
    }

    #[test]
    fn a_word_s_ink_reaches_as_far_as_its_marks_and_punctuation() {
        let (cap, x, desc) = (CAP_HEIGHT_EM, X_HEIGHT_EM, DESCENDER_EM);
        for (text, reach) in [
            ("zero", Some((x, 0.0))),
            ("zéro", Some((cap, 0.0))),
            ("über", Some((cap, 0.0))),
            ("Été", Some((ACCENTED_CAP_EM, 0.0))),
            ("Über", Some((ACCENTED_CAP_EM, 0.0))),
            ("ça", Some((x, desc))),
            ("Reçu", Some((cap, desc))),
            ("ąę", Some((x, desc))),
            ("(annual)", Some((cap, desc))),
            ("[eur]", Some((cap, desc))),
            ("a|b", Some((cap, desc))),
            ("1,250.00", Some((cap, TAIL_EM))),
            ("x; z", Some((x, TAIL_EM))),
            ("$ 98.40", Some((cap, TAIL_EM))),
            ("a/c", Some((cap, 0.0))),
            ("'one'", Some((cap, 0.0))),
            ("“once”", Some((cap, 0.0))),
            ("now?", Some((cap, 0.0))),
            // A mark stands over the letter before it, not over a comma.
            ("x,\u{301}", Some((x, TAIL_EM))),
            ("(),;", None),
        ] {
            assert_eq!(ink_extent(text), reach, "{text}");
        }
    }

    #[test]
    fn a_word_alone_with_accents_at_body_size_is_not_a_heading() {
        // A letter in 11 pt with words alone on their lines between its
        // paragraphs, each boxed as tall and as long as the built-in engine
        // boxes it (`a_short_or_accented_line_reads_at_the_size_of_the_prose`).
        // Sized with no mark over or under their letters counted, or as a
        // line of prose is whatever its length, they read a fifth larger than
        // the paragraphs and came out as headings: `## Reçu`, `#### Année`.
        let prose = [
            (
                "Nous vous informons que votre contrat arrive à échéance prochainement.",
                1.08,
                33.7,
            ),
            (
                "Le montant de la prime pour la période à venir figure ci-dessous, avec",
                1.02,
                32.6,
            ),
            (
                "le détail des garanties souscrites et la date limite de règlement.",
                1.06,
                29.0,
            ),
        ];
        let lines: Vec<(&str, f64, f64)> = prose
            .iter()
            .copied()
            .chain([
                ("Reçu", 1.24, 2.1),
                ("Année", 1.08, 3.0),
                ("Été 2026", 1.14, 3.9),
                ("Änderung", 1.26, 3.8),
                ("Größe", 1.13, 2.8),
            ])
            .chain(prose.iter().copied())
            .collect();
        let words: Vec<OcrWord> = lines
            .iter()
            .enumerate()
            .map(|(k, &(text, em, aspect))| {
                let (y, h) = (72.0 + 14.3 * k as f64, 11.0 * em);
                word(text, [72.0, y, 72.0 + aspect * h, y + h])
            })
            .collect();
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let els = analysed(&mut doc, &r);
        assert_eq!(headings(&els), [""; 0], "{els:?}");
    }

    #[test]
    fn a_page_of_lines_boxed_a_pixel_apart_is_set_at_one_size() {
        // Capitals and figures only, no descenders: each line boxes 0.98 em
        // of its type, a pixel taller or shorter by the engine's noise. The
        // page's median is taken as the lines are sized, by their letters;
        // taken as a line of prose is sized, it was a tenth under every line,
        // none was set to it, and the page read at two sizes.
        let lines: Vec<(&str, f64)> = (0..10)
            .map(|k| {
                (
                    "ACCOUNT 4417 TOTAL 1250",
                    if k % 2 == 0 { 13.0 } else { 13.8 },
                )
            })
            .collect();
        let mut doc = blank_scan();
        let r = inject(
            &mut doc,
            &[(0, Crop::Page, page_units(lines_every(&lines, 20.0)))],
        );
        assert_eq!(r[0].taken, 10);
        let sizes: Vec<f32> = doc.pages[0].glyphs.iter().map(|g| g.font_size).collect();
        assert!(sizes.iter().all(|&f| f == sizes[0]), "{sizes:?}");
    }

    #[test]
    fn a_line_of_dashes_reads_at_the_size_of_the_lines_around_it() {
        // A rule typed as dashes holds no letter to size it by, and is taken
        // as a line of prose: boxed as tall as the prose around it, it is
        // set at its size. Taken as letters of the x-height, it read a
        // quarter larger.
        let mut lines: Vec<(&str, f64)> = CLAUSE[..4].iter().map(|&t| (t, 13.0)).collect();
        lines.push(("------------------------------------", 13.0));
        lines.extend(CLAUSE[4..].iter().map(|&t| (t, 13.0)));
        let mut doc = blank_scan();
        let r = inject(
            &mut doc,
            &[(0, Crop::Page, page_units(lines_every(&lines, 16.0)))],
        );
        let size = |c: &str| {
            doc.pages[0]
                .glyphs
                .iter()
                .find(|g| g.text == c)
                .map(|g| g.font_size)
        };
        assert_eq!(r[0].taken, 10);
        assert_eq!(size("-"), size("T"));
    }

    #[test]
    fn a_stamp_set_at_an_angle_is_sized_by_its_letters() {
        // `void` turned 30 degrees over level text: no capital and no
        // descender, a dot over its `i`, and four letters to a box under four
        // times as long as it is thick.
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
        words.push(OcrWord {
            angle: Some(30.0),
            ..word("void", [250.0, 300.0, 350.0, 326.0])
        });
        let mut doc = blank_scan();
        inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let v = doc.pages[0].glyphs.iter().find(|g| g.text == "v").unwrap();
        let font = 26.0 / box_height_em("void", 100.0 / 26.0);
        assert!(
            (f64::from(v.font_size) - font).abs() < 0.01,
            "{}",
            v.font_size
        );
    }

    #[test]
    fn a_baseline_a_blocker_s_reach_away_is_clear_of_it() {
        // `110.4 - 10.5315` is 10.531499999999994 below `110.4`: short of
        // the reach by rounding alone, and refused, the stamp's nearer
        // candidate gave way to the farther side.
        let (b, d) = (110.4, 10.5315);
        assert_eq!(clear_baseline(105.0, &[(b, d)]), b - d);
        // A baseline clear already stays.
        assert_eq!(clear_baseline(99.0, &[(b, d)]), 99.0);
    }

    #[test]
    fn a_run_is_snapped_within_a_tenth_of_its_page_s_size_and_no_further() {
        let run = |font: f64| Run {
            words: Vec::new(),
            font,
            baseline: 0.0,
            x0: 0.0,
            x1: 1.0,
        };
        let mut runs: Vec<Run> = [8.9, 9.1, 10.9, 11.1].into_iter().map(run).collect();
        snap_sizes(&mut runs, 10.0);
        let fonts: Vec<f64> = runs.iter().map(|r| r.font).collect();
        assert_eq!(fonts, [8.9, 10.0, 10.0, 11.1]);
    }

    #[test]
    fn a_page_set_larger_than_the_others_reads_as_its_own_body() {
        // Two pages of one body and a third set about 11% larger, its lines
        // boxed a little apart as a reading boxes them. Snapped to the
        // document's median, the lines of the third that fall within the
        // band were set down to it and the others kept their size: the
        // paragraph came apart line by line, and its larger lines read as
        // headings.
        let mut doc = scans(3);
        let readings: Vec<(usize, Crop, WordReading)> = (0..3)
            .map(|i| {
                let lines: Vec<(&str, f64)> = CLAUSE
                    .iter()
                    .enumerate()
                    .map(|(k, &t)| {
                        let pt = match (i, k % 2) {
                            (2, 0) => 12.95,
                            (2, _) => 13.7,
                            _ => 12.0,
                        };
                        (t, pt)
                    })
                    .collect();
                let pitch = if i == 2 { 17.0 } else { 15.0 };
                (
                    i,
                    Crop::Page,
                    page_units(lines_every(&boxed(&lines), pitch)),
                )
            })
            .collect();
        let r = inject(&mut doc, &readings);
        let els = analysed(&mut doc, &r);
        assert_eq!(headings(&els), [""; 0], "{:?}", headings(&els));
        let third: Vec<&str> = els
            .iter()
            .filter(|e| e.page == 2)
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(third.len(), 1, "{third:?}");
    }

    #[test]
    fn a_table_page_is_sized_by_its_text_not_its_cells() {
        // A paragraph over a table of figures. A box over a figure or a
        // word alone is taller for its length than one over a line of
        // prose, and the cells outnumber the lines: the median of the boxes
        // is the cells', the paragraph's lines are too far under it to be
        // set to it, and its last line, one word boxed a little taller,
        // was set to it alone and read as a heading.
        let mut lines: Vec<(&str, f64)> = CLAUSE[..3].iter().map(|&t| (t, 11.0)).collect();
        lines.push(("transfer.", 11.9));
        let mut words = lines_every(&boxed(&lines), 15.0);
        let cell = box_tall("1,250.00", 13.0, 48.0);
        for row in 0..6 {
            for col in 0..3 {
                let x = 72.0 + 120.0 * col as f64;
                let y = 180.0 + 22.0 * row as f64;
                words.push(word("1,250.00", [x, y, x + 48.0, y + cell]));
            }
        }
        let mut doc = blank_scan();
        let r = inject(&mut doc, &[(0, Crop::Page, page_units(words))]);
        let els = analysed(&mut doc, &r);
        assert_eq!(headings(&els), [""; 0], "{els:?}");
        assert!(
            els.iter()
                .any(|e| e.text.ends_with("interest accrued transfer.")),
            "{els:?}"
        );
    }

    #[test]
    fn a_page_of_few_lines_is_sized_with_the_document() {
        // Three short pages of one size of type, each boxed a little taller
        // than the one before by its reading, as pages read turned can be,
        // the last two fed upside down, and the lines set apart. Each page
        // sized by its own few lines, or none snapped, the lines of the
        // third would read as headings over the first's body.
        let mut doc = scans(3);
        let at = |h: f64| -> Vec<(&str, f64)> { FORM.iter().map(|&t| (t, h)).collect() };
        let readings: Vec<(usize, Crop, WordReading)> = [12.0, 12.9, 14.0]
            .into_iter()
            .enumerate()
            .map(|(i, pt)| {
                let mut words = lines_every(&boxed(&at(pt)), 60.0);
                if i > 0 {
                    for w in &mut words {
                        let [x0, y0, x1, y1] = w.bbox;
                        w.bbox = [612.0 - x1, 792.0 - y1, 612.0 - x0, 792.0 - y0];
                        w.angle = Some(180.0);
                    }
                }
                (i, Crop::Page, page_units(words))
            })
            .collect();
        let r = inject(&mut doc, &readings);
        assert_eq!(doc.pages[2].rotation, 180);
        let els = analysed(&mut doc, &r);
        // The first line is the document's title, as on the born-digital
        // page; no other line is a heading.
        assert_eq!(
            headings(&els),
            ["Account number 4417-0093-21"],
            "{:?}",
            headings(&els)
        );
    }

    /// A letter's lines at 10 pt on a leading of 1.35, boxed as the built-in
    /// engine boxes them: each line's box `h` ems tall (about 1.05 over
    /// prose, 1.0 over a line with no descenders), its baseline `down` of
    /// the box from its top. Measured on synthetic pages of Helvetica, Times
    /// and Courier at 8 to 24 pt, rendered at 100 to 300 dpi.
    fn letter_as_detected() -> Vec<OcrWord> {
        let lines: [(&str, f64, f64, usize); 8] = [
            ("January 10, 2026", 1.08, 0.79, 0),
            ("Quinn Testwell", 1.0, 0.86, 2),
            ("7 Orchard Road", 0.99, 0.86, 3),
            ("Lakeside, WA 98002", 1.02, 0.84, 4),
            ("Dear Policyholder,", 1.1, 0.78, 6),
            ("Thank you for your letter of last week.", 1.05, 0.79, 8),
            ("We have reviewed the account: the balance", 1.05, 0.79, 9),
            ("is due by August 7; quote the reference.", 1.05, 0.79, 10),
        ];
        lines
            .iter()
            .map(|&(text, h, down, k)| {
                let baseline = 120.0 + 13.5 * k as f64;
                let top = baseline - down * h * 10.0;
                let w = 5.0 * text.chars().count() as f64;
                word(text, [72.0, top, 72.0 + w, top + h * 10.0])
            })
            .collect()
    }

    #[test]
    fn a_letter_read_by_the_engine_keeps_its_size_and_its_blocks() {
        let mut doc = blank_scan();
        let r = inject(
            &mut doc,
            &[(0, Crop::Page, page_units(letter_as_detected()))],
        );
        let body = doc.pages[0]
            .glyphs
            .iter()
            .find(|g| g.text == "T")
            .unwrap()
            .font_size;
        assert!((f64::from(body) - 10.0).abs() < 0.6, "10 pt read as {body}");
        let els = analysed(&mut doc, &r);
        let texts: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();
        // The address is one block, as on the born-digital page: its lines,
        // judged one by one, made `7 Orchard Road` a numbered heading.
        assert!(
            texts.contains(&"Quinn Testwell 7 Orchard Road Lakeside, WA 98002"),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with("Thank you") && t.ends_with("the reference.")),
            "{texts:?}"
        );
        // The page's first line is its title, as on the born-digital page.
        assert_eq!(headings(&els), ["January 10, 2026"], "{texts:?}");
    }
}
