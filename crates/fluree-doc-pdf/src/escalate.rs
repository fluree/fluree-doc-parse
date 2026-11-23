//! The escalation loop as a library: which crops a document asks for, the
//! pixels to send, the words to send with them, and the container a
//! reading comes back in.
//!
//! `fdoc convert --escalate` runs this loop with a configured Gemini
//! reader; an embedded consumer (a worker Lambda routing readings through
//! its own model proxy) runs the same loop with its own reader. What this
//! module owns is everything EXCEPT the reader call, so the two cannot
//! drift: the crop set, the render scale, and the prompts are the ones the
//! published benchmark scores describe.
//!
//! `fdoc` also has a layout detector's table boxes, from sidecar files an
//! embedded consumer does not have. They are a hint, [`CropHints`]: they
//! widen a table crop the grid under-measured and tell a region it holds a
//! table. [`crops_for`] and [`prompt_for_crop`] are the same selection and
//! prompts without them; [`crops_for_with`] and [`prompt_for_crop_with`]
//! take them.
//!
//! ```no_run
//! # use fluree_doc_pdf::{document, escalate, extract_bytes, outline};
//! # let bytes: Vec<u8> = vec![];
//! let pdf = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes.clone())).unwrap();
//! let ol = outline::extract(&pdf);
//! let mut doc = extract_bytes(bytes).unwrap();
//! let opts = document::AnalyzeOptions { emit_anchors: true, ..Default::default() };
//! let mut analysis = document::analyze_with(&mut doc, &ol, &opts);
//!
//! let jobs = escalate::crops_for(&doc, &analysis, false);
//! let crops = escalate::render_crops(&pdf, &doc, &jobs);
//! let links = fluree_doc_pdf::link::extract(&pdf);
//! let mut readings = escalate::Readings::default();
//! for crop in &crops {
//!     let hints = escalate::links_in(&links, crop, &doc);
//!     let prompt = escalate::prompt_for_crop(crop, &hints);
//!     // let text = your_reader(&crop.png, &prompt)?;  // any VLM
//!     # let text = Some(String::new());
//!     if let Some(text) = text {
//!         readings.insert(crop.name.clone(), text);
//!     }
//! }
//! if !readings.is_empty() {
//!     let page_text: Vec<Vec<String>> = doc
//!         .pages
//!         .iter()
//!         .map(|p| fluree_doc_pdf::fidelity::page_lines(&p.glyphs))
//!         .collect();
//!     fluree_doc_pdf::arbiter::splice_with_page(
//!         &mut analysis.elements, "doc", &readings, None, &page_text);
//!     fluree_doc_pdf::arbiter::scrub_furniture(
//!         &mut analysis.elements, &analysis.furniture);
//! }
//! ```

use crate::arbiter::{Block, TierBackend};
use crate::document::{Analysis, AnalyzeOptions};
use crate::extract::Document;
use crate::geom::BBox;
use crate::outline::OutlineItem;
use hayro::vello_cpu::color::{AlphaColor, Srgb};
use hayro::{render, RenderCache, RenderSettings};
use hayro_syntax::Pdf;
use std::collections::HashMap;

/// Crop oversampling. Measured against the deep reader: 1×, 1.5× and 2×
/// all bill the same input tokens because the API resizes below a
/// threshold, and 3× costs two and a half times as many and reads *worse*.
pub const VLM_RENDER_SCALE: f32 = 2.0;

/// The finest a crop rendered for its stored resolution is drawn: 300 dpi.
/// Past it, an OCR engine reads no better and costs more.
pub const MAX_STORED_PPI: f64 = 300.0;

/// The longest edge, in pixels, of a crop rendered for its stored
/// resolution, so a poster-sized page does not render at 300 dpi.
pub const MAX_LONG_EDGE_PX: f64 = 4000.0;

/// The most pixels a page is rendered with for crops at their stored
/// resolution: 64 million, 256 MB of RGBA, held once (the crops are cut from
/// the render itself; a whole page is copied out of it once). A crop is cut
/// from a render of its whole page, so a small capture on a large page would
/// otherwise draw the whole page at the capture's resolution. The bound is
/// absolute for [`CropScale::Stored`]: a page the model scale would already
/// draw larger is drawn below it, as large as the budget allows. The model's
/// crops ([`CropScale::Model`]) are drawn at the model scale whatever the
/// page's size.
pub const MAX_PAGE_PIXELS: f64 = 64.0e6;

/// The longest side, in pixels, the renderer can draw: it sizes its canvas
/// in 16 bits, and a page drawn wider loses what lies past the edge.
pub const MAX_RENDER_SIDE: f64 = 65_535.0;

/// The most renders of one page its crops are cut from, plus one per crop
/// held to its long-edge cap that no shared render fits. Crops at their
/// stored resolution each ask for their own scale, and a page of hundreds of
/// captures would be rendered hundreds of times; past this many scales,
/// crops share the next scale up, so none is drawn coarser than it asks,
/// nor past its long-edge cap ([`MAX_LONG_EDGE_PX`]): such a crop, when none
/// of the shared scales lies between its own and its cap, keeps its own
/// render. It takes a crop over about 960 points long, so it is rare.
pub const MAX_RENDERS_PER_PAGE: usize = 4;

/// How a crop is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CropScale {
    /// [`VLM_RENDER_SCALE`] whatever is under the crop: what a model reads,
    /// measured, and what it bills for.
    #[default]
    Model,
    /// The resolution of the image under the crop, between the model scale
    /// and [`MAX_STORED_PPI`]: what an OCR engine reads.
    ///
    /// A screenshot shrunk onto a page holds more pixels than the model
    /// scale draws: a 3,840-px capture pasted 6.5 inches wide comes out 960
    /// px wide at 2×, and its 22-px text at 5.5 px. Read at its own
    /// resolution it reads as captured. A one-bit image (a fax, an image
    /// mask) is the exception, kept at the model scale: smoothed, it reads
    /// better than its raw pixels.
    Stored,
}

/// The scale a crop of `page` renders at, in pixels per point: the whole
/// page when `region` is `None`.
///
/// With [`CropScale::Stored`] it is the resolution of the image the crop
/// shows, clamped between the model scale and [`MAX_STORED_PPI`] and held
/// back toward the model scale (never below it) by [`MAX_LONG_EDGE_PX`] on
/// the crop's long edge; the model scale where no image is under it, or the
/// image is one-bit or its resolution unknown. Whichever it is, the page it
/// is cut from is then drawn within [`MAX_PAGE_PIXELS`] and
/// [`MAX_RENDER_SIDE`], below the model scale if need be. It is not rounded:
/// small text in a capture reads worse a few dpi below the capture's own
/// resolution.
///
/// The image a crop shows is the one most of which lies inside it (a
/// region is cut around the image it was routed for), the largest of those;
/// failing any, the one covering most of the crop. A page background or a
/// letterhead under a small capture is not what the crop shows.
pub fn crop_scale(page: &crate::extract::Page, region: Option<&BBox>, mode: CropScale) -> f32 {
    let base = f64::from(VLM_RENDER_SCALE);
    if mode == CropScale::Model {
        return VLM_RENDER_SCALE;
    }
    // The page is drawn whole whatever the crop, so every crop read at the
    // stored resolution, the model-scale ones included, is held to what the
    // page may be drawn at: the pixel budget, and never wider than the
    // renderer draws. A pixel short of that, so the scale's rounding to f32
    // cannot cross it.
    let budget = (MAX_PAGE_PIXELS / (page.width * page.height).max(1.0)).sqrt();
    let widest = (MAX_RENDER_SIDE - 1.0) / page.width.max(page.height).max(1.0);
    let held = |s: f64| s.min(budget).min(widest) as f32;
    let area = match region {
        Some(b) => BBox {
            x0: b.x0 - CROP_MARGIN,
            y0: b.y0 - CROP_MARGIN,
            x1: b.x1 + CROP_MARGIN,
            y1: b.y1 + CROP_MARGIN,
        },
        // Page space, as the images' boxes are.
        None => BBox {
            x0: 0.0,
            y0: 0.0,
            x1: page.width,
            y1: page.height,
        },
    };
    let overlap = |b: &BBox| {
        let w = b.x1.min(area.x1) - b.x0.max(area.x0);
        let h = b.y1.min(area.y1) - b.y0.max(area.y0);
        if w > 0.0 && h > 0.0 {
            w * h
        } else {
            0.0
        }
    };
    let inside = |b: &BBox| overlap(b) / ((b.x1 - b.x0) * (b.y1 - b.y0)).max(f64::MIN_POSITIVE);
    let under: Vec<&crate::extract::ImagePlacement> = page
        .images
        .iter()
        .filter(|i| overlap(&i.bbox) > 0.0)
        .collect();
    let mostly_inside = under.iter().filter(|i| inside(&i.bbox) >= 0.5);
    let Some(image) = mostly_inside
        .max_by(|a, b| overlap(&a.bbox).total_cmp(&overlap(&b.bbox)))
        .or_else(|| {
            under
                .iter()
                .max_by(|a, b| overlap(&a.bbox).total_cmp(&overlap(&b.bbox)))
        })
    else {
        return held(base);
    };
    let Some(ppi) = image.ppi.filter(|_| !image.bitonal) else {
        return held(base);
    };
    let long_edge = (area.x1 - area.x0).max(area.y1 - area.y0).max(1.0);
    let scale = (ppi / 72.0)
        .min(MAX_STORED_PPI / 72.0)
        .min(MAX_LONG_EDGE_PX / long_edge);
    held(scale.max(base))
}

/// The scales a page is rendered at for crops asking for `scales`, each at
/// most `most[k]`: each crop's own while there are at most
/// [`MAX_RENDERS_PER_PAGE`] distinct ones, else that many, each crop rendered
/// at the smallest of them not below its own and not above its most (a
/// crop held to [`MAX_LONG_EDGE_PX`] keeps its own scale if none fits). The
/// result is, for each crop, the scale it is cut at.
fn render_scales(scales: &[f32], most: &[f32]) -> Vec<f32> {
    let mut distinct = scales.to_vec();
    distinct.sort_by(f32::total_cmp);
    distinct.dedup();
    if distinct.len() <= MAX_RENDERS_PER_PAGE {
        return scales.to_vec();
    }
    // The top of each of `MAX_RENDERS_PER_PAGE` runs of the sorted scales,
    // as even in count as they divide.
    let n = distinct.len();
    let kept: Vec<f32> = (1..=MAX_RENDERS_PER_PAGE)
        .map(|k| distinct[(k * n).div_ceil(MAX_RENDERS_PER_PAGE) - 1])
        .collect();
    scales
        .iter()
        .zip(most)
        .map(|(s, m)| {
            kept.iter()
                .copied()
                .find(|k| k >= s && *k <= m.max(*s))
                .unwrap_or(*s)
        })
        .collect()
}

pub use crate::route::CROP_MARGIN;

/// A page's render jobs: `None` = the whole page, else named crops.
pub type CropJobs = Vec<(usize, Option<Vec<(String, BBox)>>)>;

/// Why a crop was asked for.
///
/// Two families with opposite needs. A scan, a near-blank page, a page of
/// broken Unicode and a raster region carry content the text layer does
/// not: any reader adds to them. A table the grid does not trust, and a page
/// asked for on a doubt about its headings, figures or columns, have all
/// their text already and want a reader that sees structure: an OCR engine
/// has nothing to add there, and should skip them. A table no grid covered
/// usually has its text too, and an OCR engine skips it as well; a raster
/// table too small for the router to send a reader is the known gap there.
/// A table on a page read whole for a doubt goes with its page, under the
/// doubt's reason.
///
/// The names ([`CropReason::as_str`]) are a stable contract: a producer may
/// filter on them, and renaming or removing one is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CropReason {
    /// A whole page with no meaningful text layer.
    Scanned,
    /// A whole page with almost no glyphs, whose content is its image.
    NearBlank,
    /// A whole page whose glyphs' Unicode cannot be trusted.
    BrokenText,
    /// A raster region of a text page that the text layer does not cover, or
    /// a whole page read for a doubt that also carries one.
    Region,
    /// A raster region too small to be worth a model's call, which an OCR
    /// engine still reads (listed only when crops are made for one).
    SmallRegion,
    /// A table the deterministic grid found but does not trust.
    Table,
    /// A table a layout detector found and no grid covered: text the grid
    /// missed, or a raster table too small to route as a region.
    TableMissing,
    /// A whole page whose hierarchy rests on nothing but font size.
    HeadingDoubt,
    /// A whole page whose text sits inside its drawings.
    FigureDoubt,
    /// A whole page whose column structure is in doubt (asked for only
    /// with [`CropHints::on_column_doubt`]).
    ColumnDoubt,
}

impl CropReason {
    /// The name the `render-routed` manifest records.
    pub fn as_str(self) -> &'static str {
        match self {
            CropReason::Scanned => "scanned",
            CropReason::NearBlank => "near_blank",
            CropReason::BrokenText => "broken_text",
            CropReason::Region => "region",
            CropReason::SmallRegion => "small_region",
            CropReason::Table => "table",
            CropReason::TableMissing => "table_missing",
            CropReason::HeadingDoubt => "heading_doubt",
            CropReason::FigureDoubt => "figure_doubt",
            CropReason::ColumnDoubt => "column_doubt",
        }
    }

    /// The reason for a region crop, from its tag: `r{i}` a routed region,
    /// `n{i}` a table only a layout detector found, `t{i}` a table the grid
    /// does not trust. `None` for a tag no region crop carries.
    pub fn of_region_tag(tag: &str) -> Option<Self> {
        match tag.chars().next() {
            Some('r') => Some(CropReason::Region),
            Some('n') => Some(CropReason::TableMissing),
            Some('t') => Some(CropReason::Table),
            _ => None,
        }
    }
}

impl From<crate::route::Reason> for CropReason {
    /// The router's verdict on a page read whole.
    fn from(r: crate::route::Reason) -> Self {
        match r {
            crate::route::Reason::Scanned => CropReason::Scanned,
            crate::route::Reason::NearBlank => CropReason::NearBlank,
            crate::route::Reason::BrokenText => CropReason::BrokenText,
        }
    }
}

/// Why each whole page in a job list was asked for, by page index. Region
/// crops carry theirs in their tag ([`CropReason::of_region_tag`]).
pub type PageReasons = std::collections::BTreeMap<usize, CropReason>;

/// One rendered crop, named the way the splice will ask for it.
pub struct Crop {
    /// `p{page}_{tag}` — the crop name a reading is filed under.
    pub name: String,
    pub page: usize,
    /// The region on the page, or `None` for a whole page.
    pub bbox: Option<BBox>,
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

/// Readings held in memory, keyed by crop name — the same contract
/// `FixtureBackend` serves from a directory, for a consumer that read the
/// crops itself rather than through sidecar files.
#[derive(Default)]
pub struct Readings {
    by_crop: HashMap<String, String>,
}

impl Readings {
    pub fn insert(&mut self, crop: String, content: String) {
        self.by_crop.insert(crop, content);
    }

    pub fn is_empty(&self) -> bool {
        self.by_crop.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_crop.len()
    }

    /// Serialise for replay: a later pass (or an audit) re-splices the
    /// exact readings this run paid for instead of calling a model again.
    pub fn to_map(&self) -> &HashMap<String, String> {
        &self.by_crop
    }

    pub fn from_map(by_crop: HashMap<String, String>) -> Self {
        Self { by_crop }
    }
}

impl TierBackend for Readings {
    fn read(&self, _stem: &str, crop: &str) -> Option<Vec<Block>> {
        let content = self.by_crop.get(crop)?;
        Some(vec![Block {
            label: if content.trim_start().starts_with("<table") {
                "table".into()
            } else {
                "text".into()
            },
            content: content.clone(),
        }])
    }
}

/// What a caller knows beyond the document, for choosing and prompting crops.
#[derive(Debug, Clone, Default)]
pub struct CropHints {
    /// Read every page whose column structure is in doubt as a whole page.
    /// Off unless asked for: whether it helps depends on the corpus — see
    /// [`crops_for_with`].
    pub on_column_doubt: bool,
    /// Tables a layout detector boxed, by page, in PDF units.
    pub layout_tables: HashMap<usize, Vec<BBox>>,
    /// The caller crops a page's routed regions apart even when a doubt
    /// asks for the page whole (`render-routed --ocr-regions`), so the whole
    /// page keeps its doubt's reason: the regions' pixels have crops of their
    /// own, and a reader of characters reads those instead.
    pub regions_apart: bool,
}

/// The crops a document asks for, chosen from the document as extracted.
///
/// The selection reads table anchors, which an analysis emits only with
/// `emit_anchors` on, and an analysis changes the pages it reads (doubled
/// glyphs removed, layout rules stripped). So a copy is analysed here with
/// anchors on and `opts` otherwise, and pages are routed as extracted, as
/// every caller of this sees them: the crops `fdoc dev render-routed` writes
/// for a reader, the ones `fdoc convert --escalate` sends one, and the count
/// `fdoc triage` prices are one set.
pub fn plan(
    doc: &Document,
    outline: &[OutlineItem],
    opts: &AnalyzeOptions,
    hints: &CropHints,
) -> CropJobs {
    plan_explained(doc, outline, opts, hints).0
}

/// As [`plan`], with the reason each whole page was asked for.
pub fn plan_explained(
    doc: &Document,
    outline: &[OutlineItem],
    opts: &AnalyzeOptions,
    hints: &CropHints,
) -> (CropJobs, PageReasons) {
    let mut copy = doc.clone();
    let opts = AnalyzeOptions {
        emit_anchors: true,
        ..opts.clone()
    };
    let analysis = crate::document::analyze_with(&mut copy, outline, &opts);
    select(doc, &analysis, hints)
}

/// Every crop this document asks for, in page order, without hints.
///
/// `analysis` MUST come from [`crate::document::analyze_with`] with
/// `emit_anchors: true` — the table anchors this scans for are only
/// emitted then, and the splice later replaces those same anchors.
pub fn crops_for(doc: &Document, analysis: &Analysis, on_column_doubt: bool) -> CropJobs {
    crops_for_with(
        doc,
        analysis,
        &CropHints {
            on_column_doubt,
            ..Default::default()
        },
    )
}

/// Every crop this document asks for, in page order.
///
/// Empty when it is read deterministically end to end, which is the common
/// case: over the evaluation corpus 113 of 200 documents ask for nothing.
/// `analysis` comes with anchors on, as for [`crops_for`].
pub fn crops_for_with(doc: &Document, analysis: &Analysis, hints: &CropHints) -> CropJobs {
    select(doc, analysis, hints).0
}

/// The selection itself, with the reason for each whole page. A page keeps
/// the first reason that asked for it: the router's verdict before any
/// doubt, a heading doubt before a figure doubt.
fn select(doc: &Document, analysis: &Analysis, hints: &CropHints) -> (CropJobs, PageReasons) {
    use crate::route::Route;
    let mut jobs: CropJobs = Vec::new();
    let mut reasons = PageReasons::new();
    for p in &doc.pages {
        // What an OCR reading already read is not asked for again. A region
        // keeps its number either way, so a crop named before the reading
        // still names the same region.
        match crate::route::decide(p).0 {
            Route::Vlm(r) => {
                jobs.push((p.index, None));
                reasons.insert(p.index, CropReason::from(r));
            }
            Route::VlmRegions(r) => {
                let unread: Vec<(String, BBox)> = r
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !p.ocr_read_region(*i))
                    .map(|(i, b)| (format!("r{i}"), *b))
                    .collect();
                if !unread.is_empty() {
                    jobs.push((p.index, Some(unread)));
                }
            }
            Route::Deterministic => {}
        }
    }
    // The whole page, superseding any region crop on it (table crops are
    // added after the doubts, and none is added to a page read whole).
    // A page that had routed regions keeps `Region` as its reason: the whole
    // page now carries pixels only a reader can read, and a reader skipping
    // doubt pages must not skip those.
    let whole = |jobs: &mut CropJobs, reasons: &mut PageReasons, page: usize, why: CropReason| {
        let why = match jobs.iter_mut().find(|(pi, _)| *pi == page) {
            Some((_, slot)) => {
                // A slot here holds routed regions only: `VlmRegions` is never
                // empty, and table crops are added after the doubts.
                let had_regions = !hints.regions_apart && slot.is_some();
                *slot = None;
                if had_regions {
                    CropReason::Region
                } else {
                    why
                }
            }
            None => {
                jobs.push((page, None));
                why
            }
        };
        reasons.entry(page).or_insert(why);
    };

    // A hierarchy resting on nothing but font size is a whole-page
    // problem, not a region one: the text is all there and legible, and
    // what is wrong is how it is organised. So the escalation is the whole
    // page, and it supersedes any region or table crop on it — a reading
    // that owns the page owns its structure too.
    //
    // Measured: the six documents this fires on gain 1.459 between them,
    // five of six better, the worst loss 0.087. Three of them more than
    // triple. See `column::doubt` for the signal that was tried first and
    // rejected: it flags 22 documents for the same escalation, gains less
    // in total, and makes five worse.
    //
    // Only the doubtful pages, not the whole document. The signal is per
    // page, and a long document with one badly organised page should cost
    // one reading rather than all of them.
    for d in &analysis.suspect_headings {
        whole(&mut jobs, &mut reasons, d.page, CropReason::HeadingDoubt);
    }
    // A page whose text sits inside its drawings is a designed layout, and
    // a designed layout is where every geometric inference this library
    // makes is least trustworthy at once — reading order, heading rank,
    // figure boundaries. Recognising it and handing it to a reader is
    // cheaper than decoding it, and correct more often.
    for d in &analysis.suspect_figures {
        whole(&mut jobs, &mut reasons, d.page, CropReason::FigureDoubt);
    }
    // Column doubt escalates only when asked for, because whether it helps
    // depends on the document and nothing on the page tells you which kind
    // you have. Measured over the evaluation corpus it is net negative --
    // fourteen documents better, seven worse, -0.0016 -- and the ones it
    // hurts have hierarchies that were already sound. On layout-heavy
    // material the same signal marks exactly the pages that read across
    // their panels.
    //
    // Four discriminators were tried and none separates the two
    // populations: band coverage, missed-gutter count, whether our lines
    // are concatenations of the reading's, and whether the document carries
    // a PDF outline. Until one is found, this is a choice the caller makes
    // about their corpus rather than one the page can make for them.
    if hints.on_column_doubt {
        for p in &doc.pages {
            if crate::document::column_doubt(p, analysis).is_some() {
                whole(&mut jobs, &mut reasons, p.index, CropReason::ColumnDoubt);
            }
        }
    }

    for e in &analysis.elements {
        if e.evidence == "table-missing" {
            // Insert anchor: the box is the crop, nothing to re-derive.
            let tag = e
                .text
                .trim_start_matches("[[VLMNEW:")
                .trim_end_matches("]]");
            let Some((_, n)) = tag.split_once(':') else {
                continue;
            };
            match jobs.iter_mut().find(|(pi, _)| *pi == e.page) {
                Some((_, Some(list))) => list.push((n.to_string(), e.rect())),
                Some((_, None)) => {}
                None => jobs.push((e.page, Some(vec![(n.to_string(), e.rect())]))),
            }
            continue;
        }
        if e.evidence != "table-confidence" {
            continue;
        }
        // text is "[[VLMTAB:pN:tK]]" — the crop takes the tK name.
        let tag = e
            .text
            .trim_start_matches("[[VLMTAB:")
            .trim_end_matches("]]");
        let Some((_, t)) = tag.split_once(':') else {
            continue;
        };
        let mut bbox = e.rect();
        bbox.y1 = bbox.y0 + 0.02; // undo the ordering nudge…
                                  // …by re-deriving the real box from the matching grid: the
                                  // anchor kept only a hairline.
        if let Some(tb) = analysis.elements.iter().find(|x| {
            x.kind == "doco:Table" && x.page == e.page && (x.rect().y0 - e.rect().y0).abs() < 1.0
        }) {
            bbox = tb.rect();
            // Where the layout detector boxes the same table with different
            // bounds, crop the union: our grid may cover only part of the
            // real table, and the reader cannot read pixels it is not shown.
            for lt in hints.layout_tables.get(&e.page).into_iter().flatten() {
                let ix = (bbox.x1.min(lt.x1) - bbox.x0.max(lt.x0)).max(0.0);
                let iy = (bbox.y1.min(lt.y1) - bbox.y0.max(lt.y0)).max(0.0);
                if ix * iy <= 0.0 {
                    continue;
                }
                let ux0 = bbox.x0.min(lt.x0);
                let uy0 = bbox.y0.min(lt.y0);
                let ux1 = bbox.x1.max(lt.x1);
                let uy1 = bbox.y1.max(lt.y1);
                // Expand only when the detector says the table is
                // substantially larger than our grid; small disagreements
                // are box jitter and would only bust crop caches.
                let a0 = (bbox.x1 - bbox.x0) * (bbox.y1 - bbox.y0);
                let au = (ux1 - ux0) * (uy1 - uy0);
                if au >= 1.15 * a0 {
                    bbox.x0 = ux0;
                    bbox.y0 = uy0;
                    bbox.x1 = ux1;
                    bbox.y1 = uy1;
                }
            }
        }
        match jobs.iter_mut().find(|(pi, _)| *pi == e.page) {
            Some((_, Some(list))) => list.push((t.to_string(), bbox)),
            Some((_, None)) => {} // whole page already renders
            None => jobs.push((e.page, Some(vec![(t.to_string(), bbox)]))),
        }
    }
    // A page OCR read whole asks for nothing more here: its headings and
    // tables were assembled from the reading's own boxes, whose sizes are
    // estimates, so a doubt about them says more about the estimate than
    // about the page, and a model reading would replace the OCR text
    // outright. Escalating past OCR is a decision of its own, taken on the
    // reading's confidence.
    let whole_ocr = |pi: usize| {
        doc.pages
            .iter()
            .any(|p| p.index == pi && p.ocr.as_ref().is_some_and(|o| o.whole))
    };
    jobs.retain(|(pi, _)| !whole_ocr(*pi));
    reasons.retain(|pi, _| !whole_ocr(*pi));
    (jobs, reasons)
}

/// How many readings a job list asks for: one per whole page, one per
/// region.
pub fn crop_count(jobs: &CropJobs) -> usize {
    jobs.iter()
        .map(|(_, regions)| regions.as_ref().map_or(1, Vec::len))
        .sum()
}

/// The `max_crops` most valuable crops of a job list, in page order, and
/// how many were left out.
///
/// A consumer paying per reading needs a ceiling, and a ceiling that fails
/// the document is the wrong kind: the deterministic reading is already
/// complete, and every crop only improves on it. So the cap truncates.
/// What it keeps is ranked by what would be lost without it:
///
/// 1. whole pages the router sent — a scan, a near-blank page, glyphs
///    whose Unicode cannot be trusted — where nothing else reads the page;
/// 2. routed regions, the same loss confined to part of a page;
/// 3. tables the deterministic pass found no structure for;
/// 4. tables it found but does not trust;
/// 5. whole pages asked for on a doubt about their hierarchy or layout,
///    whose every word the deterministic pass already has.
///
/// Within a rank, earlier pages first. A whole-page job that superseded
/// region crops on its page is one crop, kept or dropped as one, and ranks
/// with routed regions: the pixels it carries are still the text layer's gap.
pub fn within_budget(doc: &Document, jobs: CropJobs, max_crops: usize) -> (CropJobs, usize) {
    let total = crop_count(&jobs);
    if total <= max_crops {
        return (jobs, 0);
    }
    // One entry per crop.
    struct Ranked {
        rank: u8,
        order: usize,
        page: usize,
        region: Option<(String, BBox)>,
    }
    let mut crops: Vec<Ranked> = Vec::new();
    for (order, (page, regions)) in jobs.into_iter().enumerate() {
        match regions {
            None => {
                // Ranked by what the router sent the page: a page whose
                // routed regions a doubt widened to the whole page still
                // holds pixels only a reader can read, so it ranks with
                // those regions, not with the doubt pages.
                let routed = doc
                    .pages
                    .iter()
                    .find(|p| p.index == page)
                    .map(|p| crate::route::decide(p).0);
                let rank = match routed {
                    Some(crate::route::Route::Vlm(_)) => 0,
                    Some(crate::route::Route::VlmRegions(_)) => 1,
                    _ => 4,
                };
                crops.push(Ranked {
                    rank,
                    order,
                    page,
                    region: None,
                });
            }
            Some(list) => {
                for (tag, bbox) in list {
                    let rank = match tag.chars().next() {
                        Some('r') => 1,
                        Some('n') => 2,
                        _ => 3,
                    };
                    crops.push(Ranked {
                        rank,
                        order,
                        page,
                        region: Some((tag, bbox)),
                    });
                }
            }
        }
    }
    crops.sort_by_key(|c| (c.rank, c.order));
    let dropped = crops.len() - max_crops;
    crops.truncate(max_crops);
    crops.sort_by_key(|c| c.order);
    let mut out: CropJobs = Vec::new();
    for Ranked { page, region, .. } in crops {
        match (out.last_mut(), region) {
            (Some((p, Some(list))), Some(r)) if *p == page => list.push(r),
            (_, Some(r)) => out.push((page, Some(vec![r]))),
            (_, None) => out.push((page, None)),
        }
    }
    (out, dropped)
}

/// Render every job to PNG bytes, in page order, each region cut where it
/// shows on its page as displayed (see [`for_each_crop_of`]).
pub fn render_crops(pdf: &Pdf, doc: &Document, jobs: &CropJobs) -> Vec<Crop> {
    let mut out = Vec::new();
    for_each_crop_of(pdf, doc, jobs, CropScale::Model, |c| out.extend(png_of(c)));
    out
}

/// As [`render_crops`], for jobs whose regions are already in the display
/// frame, as an element's `doc:bbox` is: each cut where it says.
pub fn render_crops_displayed(pdf: &Pdf, jobs: &CropJobs) -> Vec<Crop> {
    let mut out = Vec::new();
    for_each_crop(pdf, jobs, |c| out.extend(png_of(c)));
    out
}

/// A rendered crop encoded as PNG.
fn png_of(c: RawCrop) -> Option<Crop> {
    let (w, h) = (c.pixels.width as usize, c.pixels.height as usize);
    // Rendered crops are RGBA (see `for_each_crop`).
    let png = encode(&c.pixels.data, w, 0, 0, w, h)?;
    Some(Crop {
        name: c.name,
        page: c.page,
        bbox: c.bbox,
        png,
    })
}

/// One rendered crop, as pixels: what an OCR engine reads.
pub struct RawCrop {
    /// `p{page}_{tag}`, as for [`Crop`].
    pub name: String,
    pub page: usize,
    /// The region, in PDF units; `None` for the whole page.
    pub bbox: Option<BBox>,
    /// Pixels per PDF unit it was rendered at.
    pub scale: f32,
    pub pixels: fluree_doc_vision::Pixels,
}

/// Render each job's crops and hand them over one at a time, so a long
/// scan never holds more than one page's pixels. The same pixels
/// [`render_crops`] encodes: same renderer, scale and margin, and RGBA, as
/// the renderer gives them.
pub fn for_each_crop(pdf: &Pdf, jobs: &CropJobs, f: impl FnMut(RawCrop)) {
    for_each_crop_scaled(pdf, jobs, |_, _| VLM_RENDER_SCALE, f)
}

/// As [`for_each_crop`], each crop at the scale `scale_of(page, region)`
/// gives it (see [`crop_scale`]). A page is rendered once per scale its
/// crops ask for (at most [`MAX_RENDERS_PER_PAGE`] times, plus one per crop
/// held to its long-edge cap), one render at a time, and each crop is handed
/// over as soon as it is cut: a page's crops come in the order of its
/// renders, coarsest first, and in the jobs' order within each.
/// [`crop_order`] gives the jobs' order back.
///
/// A region is cut where it shows on the page as rendered: on a page read
/// in a turned frame (`/Rotate`, or text drawn sideways), pass
/// [`for_each_crop_of`] the document, or give the regions in the display
/// frame.
pub fn for_each_crop_scaled(
    pdf: &Pdf,
    jobs: &CropJobs,
    scale_of: impl Fn(usize, Option<&BBox>) -> f32,
    f: impl FnMut(RawCrop),
) {
    cut_crops(pdf, jobs, scale_of, |_, b| *b, f)
}

/// Every crop of `jobs`, rendered at the scale `mode` gives it on `doc`'s
/// pages ([`crop_scale`]) and cut where its region shows once the page is
/// rendered as displayed. What an OCR engine reads.
pub fn for_each_crop_of(
    pdf: &Pdf,
    doc: &Document,
    jobs: &CropJobs,
    mode: CropScale,
    f: impl FnMut(RawCrop),
) {
    let page = |pi: usize| doc.pages.iter().find(|p| p.index == pi);
    cut_crops(
        pdf,
        jobs,
        |pi, r| page(pi).map_or(VLM_RENDER_SCALE, |p| crop_scale(p, r, mode)),
        |pi, b| page(pi).map_or(*b, |p| p.to_display(*b)),
        f,
    )
}

/// Each crop's place in `jobs`, by its name (`p{page}_{tag}`, `p{page}_full`
/// for a whole page): the order a caller of [`for_each_crop_scaled`] or
/// [`for_each_crop_of`] sorts by to have the crops of a page rendered at
/// several scales back in the order it asked for them.
pub fn crop_order(jobs: &CropJobs) -> HashMap<String, usize> {
    jobs.iter()
        .flat_map(|(page, regions)| match regions {
            None => vec![format!("p{page}_full")],
            Some(list) => list
                .iter()
                .map(|(tag, _)| format!("p{page}_{tag}"))
                .collect(),
        })
        .enumerate()
        .map(|(i, name)| (name, i))
        .collect()
}

/// Render each job's pages and cut its crops, each region where `shown`
/// says it lies on the render (in PDF units, display frame).
fn cut_crops(
    pdf: &Pdf,
    jobs: &CropJobs,
    scale_of: impl Fn(usize, Option<&BBox>) -> f32,
    shown: impl Fn(usize, &BBox) -> BBox,
    mut f: impl FnMut(RawCrop),
) {
    use fluree_doc_vision::Layout;
    let pages = pdf.pages();
    let cache = RenderCache::new();
    let draw = |page, scale| render_page(page, &cache, scale);
    for (page_idx, regions) in jobs {
        let Some(page) = pages.get(*page_idx) else {
            continue;
        };
        match regions {
            None => {
                let scale = scale_of(*page_idx, None);
                let (w, h, rgba) = {
                    let r = draw(page, scale);
                    (r.width, r.height, r.rgba().to_vec())
                };
                if let Some(pixels) =
                    fluree_doc_vision::Pixels::new(w as u32, h as u32, Layout::Rgba8, rgba)
                {
                    f(RawCrop {
                        name: format!("p{page_idx}_full"),
                        page: *page_idx,
                        bbox: None,
                        scale,
                        pixels,
                    });
                }
            }
            Some(regions) => {
                // One render per scale the page's crops ask for, each dropped
                // once its crops are cut: never more than one render held,
                // and each crop handed over as cut, never held past it.
                // A crop shares a finer render only within its long-edge cap.
                let most: Vec<f32> = regions
                    .iter()
                    .map(|(_, b)| {
                        let long = (b.x1 - b.x0).max(b.y1 - b.y0) + 2.0 * CROP_MARGIN;
                        (MAX_LONG_EDGE_PX / long.max(1.0)) as f32
                    })
                    .collect();
                let scales = render_scales(
                    &regions
                        .iter()
                        .map(|(_, b)| scale_of(*page_idx, Some(b)))
                        .collect::<Vec<_>>(),
                    &most,
                );
                let mut distinct = scales.clone();
                distinct.sort_by(f32::total_cmp);
                distinct.dedup();
                for scale in distinct {
                    let render = draw(page, scale);
                    let (w, h, rgba) = (render.width, render.height, render.rgba());
                    for (k, (tag, b)) in regions.iter().enumerate() {
                        if scales[k] != scale {
                            continue;
                        }
                        let sc = scale as f64;
                        let d = shown(*page_idx, b);
                        let x0 = (((d.x0 - CROP_MARGIN) * sc).floor().max(0.0)) as usize;
                        let y0 = (((d.y0 - CROP_MARGIN) * sc).floor().max(0.0)) as usize;
                        let x1 = ((((d.x1 + CROP_MARGIN) * sc).ceil()) as usize).min(w);
                        let y1 = ((((d.y1 + CROP_MARGIN) * sc).ceil()) as usize).min(h);
                        if x1 <= x0 || y1 <= y0 {
                            continue;
                        }
                        let mut buf = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
                        for row in y0..y1 {
                            let s = (row * w + x0) * 4;
                            buf.extend_from_slice(&rgba[s..s + (x1 - x0) * 4]);
                        }
                        if let Some(pixels) = fluree_doc_vision::Pixels::new(
                            (x1 - x0) as u32,
                            (y1 - y0) as u32,
                            Layout::Rgba8,
                            buf,
                        ) {
                            f(RawCrop {
                                name: format!("p{page_idx}_{tag}"),
                                page: *page_idx,
                                bbox: Some(*b),
                                scale,
                                pixels,
                            });
                        }
                    }
                }
            }
        }
    }
}

/// A rendered page, its pixels held as the renderer drew them.
struct Rendered {
    width: usize,
    height: usize,
    pixmap: hayro::vello_cpu::Pixmap,
}

impl Rendered {
    /// The pixels as RGBA rows. Drawn over opaque white, every pixel is
    /// opaque, so the renderer's premultiplied colours are the colours.
    fn rgba(&self) -> &[u8] {
        self.pixmap.data_as_u8_slice()
    }
}

/// One page rendered on white at `scale`.
fn render_page<'a>(
    page: &'a hayro_syntax::page::Page<'a>,
    cache: &'a RenderCache<'a>,
    scale: f32,
) -> Rendered {
    let white: AlphaColor<Srgb> = AlphaColor::new([1.0, 1.0, 1.0, 1.0]);
    let settings = RenderSettings {
        x_scale: scale,
        y_scale: scale,
        bg_color: white,
        ..Default::default()
    };
    let pix = render(
        page,
        cache,
        &hayro::hayro_interpret::InterpreterSettings::default(),
        &settings,
    );
    Rendered {
        width: pix.width() as usize,
        height: pix.height() as usize,
        pixmap: pix,
    }
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

// ---- prompts -------------------------------------------------------------
//
// Every rule below was put in by a measurement; see the CLI's
// `escalate::prompt` for the full accounting. The same text drives
// `eval/llm-tier/run_tier.py` — if one changes and the other does not, the
// committed scores stop describing this code.

/// A table crop: markup, because a grid is the whole content.
const TABLE: &str = "This image is one table cropped from a document page.

Transcribe it as a single HTML table.

Requirements:
- Every row and every column that is printed, in the order printed.
- Transcribe values exactly as printed, including currency symbols, commas,
  decimals, percent signs, and parentheses for negatives.
- NEVER infer, compute, complete or correct a value. If a cell is blank in
  the image, emit an empty cell. If a value is unreadable, emit an empty cell.
- Merged cells: use rowspan / colspan.
- Use <th> for header cells, <td> for data cells.

Respond with the table markup only, starting with <table> and nothing else.";

/// A routed region is whatever the text layer could not read; the router
/// does not know what it is, so the prompt must not assume.
const REGION: &str = "Transcribe what is printed in this image, exactly as printed,
in the order printed: top to bottom, then left to right.

Give each thing in the image the form that fits it.

- A TABLE -- values arranged in rows and columns -- becomes HTML:
  <table><tr><td>...</td></tr></table>, one <tr> per printed row and one <td>
  per printed column. It is still a table when it has no ruling lines, when
  it runs to many rows, and when it repeats its headers side by side to form
  a second pair of columns.
- A chart, plot or diagram is NOT a table. Give its text only, one item per
  line: its title, its legend, every axis tick label, and both axis titles.
- A caption or note printed beside the table or chart becomes a plain text
  line, in the position it is printed.

In every case:
- This image is a crop of a larger page. Skip any line that runs off its left
  or right edge, and any line the top or bottom edge cuts through so that the
  letters are only part-height. That text belongs to something outside the
  crop and is transcribed elsewhere; transcribing it here duplicates it.
- Copy text exactly, including punctuation and decimal marks as printed.
- NEVER invent a value. Do not read a number off a bar's height or a wedge's
  angle: if it is not printed as text, it is not there.
- NEVER invent a link. Text may be coloured or underlined to show it is one,
  but the address is not printed and you cannot see it. Write the text alone.
- If nothing is printed in the image, return nothing at all.
- Do not describe the image and do not add commentary.";

/// A whole page is not a big region: a page *is* the shape, so its reading
/// has to carry headings and reading order.
/// Prepended when a layout detector boxed a table inside a region crop. It
/// says what the image holds; the form rules still decide how to write it.
const TABLE_HINT: &str = "This image contains a table: values arranged in rows and columns.
Transcribe that table as HTML markup, and any text printed outside it as plain
lines.

";

const FULL: &str = "Transcribe this page exactly as printed, as Markdown.

Reading order follows the page's own layout. Where the page is laid out in
columns or panels, read each column to its end before starting the next; do
not read straight across the page.

Mark structure as the page marks it:

- A heading -- a line set apart by size, weight, colour, or its own banner --
  becomes a Markdown heading, `#` for the most prominent rank and `##`, `###`
  below it.
- A bulleted or numbered list becomes Markdown list items.
- A TABLE -- values arranged in rows and columns -- becomes HTML:
  <table><tr><td>...</td></tr></table>, one <tr> per printed row and one <td>
  per printed column.
- A chart, plot or diagram is NOT a table. Give its text only, one item per
  line: its title, its legend, every axis tick label, and both axis titles.
- Everything else is a paragraph.

In every case:
- Copy text exactly, including punctuation and dashes as printed.
- NEVER invent a value. Do not read a number off a bar's height or a wedge's
  angle: if it is not printed as text, it is not there.
- NEVER invent a link. Text may be coloured or underlined to show it is one,
  but the address is not printed and you cannot see it. Write the text alone.
- If nothing is printed on the page, return nothing at all.
- Do not describe the page and do not add commentary.";

/// A link's anchor is visible and its address is not — where the file
/// states the addresses, they are given, so the model does not invent one.
fn links_hint(listing: &str) -> String {
    format!(
        "This image contains links. Their addresses are not printed on the\n\
         page, so you cannot read them from the image; they are given here:\n\n\
         {listing}\n\n\
         Where you transcribe one of those texts, write it as a Markdown link:\n\
         [text](address), using the address exactly as given. Any other text,\n\
         however it is styled, is not a link.\n\n"
    )
}

/// The prompt for one crop, without hints. `links` are the anchors and
/// targets the file states inside the crop — see [`links_in`].
pub fn prompt_for_crop(crop: &Crop, links: &[(String, String)]) -> String {
    prompt_for_crop_with(crop, &CropHints::default(), links)
}

/// The prompt for one crop: a region a layout detector boxed a table in is
/// told it holds one.
pub fn prompt_for_crop_with(crop: &Crop, hints: &CropHints, links: &[(String, String)]) -> String {
    if crop.is_table() {
        // Table markup has no place to put a Markdown link, so the listing
        // is not offered here.
        return TABLE.to_string();
    }
    let base = if crop.is_page() {
        FULL.to_string()
    } else {
        let boxed = crop.bbox.is_some_and(|b| {
            hints
                .layout_tables
                .get(&crop.page)
                .is_some_and(|tables| tables.iter().any(|t| overlaps(&b, t)))
        });
        format!("{}{REGION}", if boxed { TABLE_HINT } else { "" })
    };
    if links.is_empty() {
        return base;
    }
    let listing = links
        .iter()
        .map(|(text, target)| format!("  \"{text}\" links to {target}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}{base}", links_hint(&listing))
}

/// The links inside a crop, one entry per target with wrapped fragments
/// rejoined — the listing [`prompt_for_crop`] wants.
pub fn links_in(links: &[crate::link::Link], crop: &Crop, doc: &Document) -> Vec<(String, String)> {
    let Some(page) = doc.pages.iter().find(|p| p.index == crop.page) else {
        return Vec::new();
    };
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for l in links.iter().filter(|l| l.page == crop.page) {
        if !crop.bbox.is_none_or(|b| overlaps(&b, &l.bbox)) {
            continue;
        }
        let text = crate::link::anchor_text(page, l.bbox);
        if text.is_empty() {
            continue;
        }
        let target = match &l.target {
            fluree_doc_model::Target::Uri { uri } => uri.clone(),
            fluree_doc_model::Target::Page { page } => format!(
                "page {} of this document -- write the text alone, with no link",
                page + 1
            ),
        };
        match grouped.iter_mut().find(|(t, _)| *t == target) {
            Some((_, fragments)) => fragments.push(text),
            None => grouped.push((target, vec![text])),
        }
    }
    grouped
        .into_iter()
        .map(|(target, fragments)| {
            let tight = fragments.concat();
            let text = if fragments.len() > 1 && target.contains(tight.trim()) {
                tight
            } else {
                fragments.join(" ")
            };
            (text, target)
        })
        .collect()
}

fn overlaps(a: &BBox, b: &BBox) -> bool {
    let ix = (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0);
    let iy = (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0);
    ix * iy > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crop(name: &str) -> Crop {
        Crop {
            name: name.into(),
            page: 0,
            bbox: None,
            png: Vec::new(),
        }
    }

    fn page(index: usize, glyphs: usize, scanned: bool) -> crate::extract::Page {
        let bbox = BBox {
            x0: 0.0,
            y0: 0.0,
            x1: 600.0,
            y1: 800.0,
        };
        crate::extract::Page {
            index,
            glyphs: (0..glyphs)
                .map(|i| crate::glyph::Glyph {
                    text: "a".into(),
                    bbox: Some(BBox {
                        x0: i as f64,
                        y0: 0.0,
                        x1: i as f64 + 1.0,
                        y1: 10.0,
                    }),
                    page: index,
                    origin: (i as f64, 10.0),
                    rotation_deg: 0.0,
                    font_size: 10.0,
                    weight: None,
                    advance: Some(1.0),
                    draw_index: i,
                })
                .collect(),
            images: if scanned {
                vec![crate::extract::ImagePlacement {
                    bbox,
                    texty: true,
                    ppi: None,
                    bitonal: false,
                    grid: (1000, 1000),
                }]
            } else {
                Vec::new()
            },
            rules: Vec::new(),
            fills: Vec::new(),
            width: 600.0,
            height: 800.0,
            rotation: 0,
            ocr: None,
        }
    }

    fn region(tag: &str) -> (String, BBox) {
        (
            tag.into(),
            BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 10.0,
                y1: 10.0,
            },
        )
    }

    /// The crops and their reasons for `doc`, with a figure doubt on each
    /// of `doubted`.
    fn reasons_with_doubts(doc: &Document, doubted: &[usize]) -> (CropJobs, PageReasons) {
        let mut copy = doc.clone();
        let opts = AnalyzeOptions {
            emit_anchors: true,
            ..Default::default()
        };
        let mut analysis = crate::document::analyze_with(&mut copy, &[], &opts);
        analysis.suspect_figures = doubted
            .iter()
            .map(|&page| crate::figure::Doubt {
                page,
                share: 0.9,
                chars: 500,
            })
            .collect();
        select(doc, &analysis, &CropHints::default())
    }

    #[test]
    fn a_page_read_whole_says_why() {
        let doc = Document {
            pages: vec![page(0, 0, true), page(1, 500, false), page(2, 500, false)],
        };
        let (jobs, reasons) = reasons_with_doubts(&doc, &[1]);
        assert_eq!(jobs.len(), 2, "the blank scan and the doubt page: {jobs:?}");
        // No glyph at all under a full-page image: the router's near-blank.
        assert_eq!(reasons.get(&0), Some(&CropReason::NearBlank));
        assert_eq!(reasons.get(&1), Some(&CropReason::FigureDoubt));
        assert_eq!(reasons.get(&2), None);
        assert_eq!(CropReason::FigureDoubt.as_str(), "figure_doubt");
    }

    #[test]
    fn each_reason_has_its_documented_name() {
        // The names are the manifest's contract (escalation-tiers.md).
        let all = [
            (CropReason::Scanned, "scanned"),
            (CropReason::NearBlank, "near_blank"),
            (CropReason::BrokenText, "broken_text"),
            (CropReason::Region, "region"),
            (CropReason::Table, "table"),
            (CropReason::TableMissing, "table_missing"),
            (CropReason::HeadingDoubt, "heading_doubt"),
            (CropReason::FigureDoubt, "figure_doubt"),
            (CropReason::ColumnDoubt, "column_doubt"),
        ];
        for (reason, name) in all {
            assert_eq!(reason.as_str(), name);
        }
        let names: std::collections::HashSet<&str> = all.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(names.len(), all.len());
    }

    /// A page of `glyphs` glyphs, none of whose Unicode resolves.
    fn broken_page(index: usize, glyphs: usize) -> crate::extract::Page {
        let mut p = page(index, glyphs, false);
        for g in &mut p.glyphs {
            g.text = String::new();
        }
        p
    }

    #[test]
    fn each_router_verdict_names_its_reason() {
        let doc = Document {
            pages: vec![page(0, 0, true), page(1, 50, true), broken_page(2, 300)],
        };
        let (_, reasons) = reasons_with_doubts(&doc, &[]);
        assert_eq!(reasons.get(&0), Some(&CropReason::NearBlank));
        assert_eq!(reasons.get(&1), Some(&CropReason::Scanned));
        assert_eq!(reasons.get(&2), Some(&CropReason::BrokenText));
    }

    #[test]
    fn the_routers_verdict_comes_before_a_doubt() {
        // A scan or a page of broken Unicode that is also doubted is still
        // read for its text: the doubt must not relabel it as a page an OCR
        // engine may skip.
        let doc = Document {
            pages: vec![page(0, 50, true), broken_page(1, 300)],
        };
        let (jobs, reasons) = reasons_with_doubts(&doc, &[0, 1]);
        assert_eq!(jobs, vec![(0, None), (1, None)]);
        assert_eq!(reasons.get(&0), Some(&CropReason::Scanned));
        assert_eq!(reasons.get(&1), Some(&CropReason::BrokenText));
    }

    #[test]
    fn a_heading_doubt_comes_before_a_figure_doubt() {
        let doc = Document {
            pages: vec![page(0, 500, false), page(1, 500, false)],
        };
        let mut copy = doc.clone();
        let opts = AnalyzeOptions {
            emit_anchors: true,
            ..Default::default()
        };
        let mut analysis = crate::document::analyze_with(&mut copy, &[], &opts);
        analysis.suspect_headings = vec![crate::heading::Doubt {
            page: 0,
            titles: 10,
            elements: 12,
            density: 0.8,
            corroborated: 0,
            template: 0,
        }];
        analysis.suspect_figures = [0, 1]
            .iter()
            .map(|&page| crate::figure::Doubt {
                page,
                share: 0.9,
                chars: 500,
            })
            .collect();
        let (jobs, reasons) = select(&doc, &analysis, &CropHints::default());
        assert_eq!(jobs, vec![(0, None), (1, None)]);
        assert_eq!(reasons.get(&0), Some(&CropReason::HeadingDoubt));
        assert_eq!(reasons.get(&1), Some(&CropReason::FigureDoubt));
    }

    #[test]
    fn a_column_doubt_names_its_reason_when_asked_for() {
        // A full-width header over three panels: the page-wide pass sees one
        // gutter of two, which is the column doubt.
        let mut p = page(0, 0, false);
        let mut i = 0;
        let mut put = |p: &mut crate::extract::Page, x: f64, y: f64| {
            p.glyphs.push(crate::glyph::Glyph {
                text: "a".into(),
                bbox: Some(BBox {
                    x0: x,
                    y0: y,
                    x1: x + 6.0,
                    y1: y + 9.0,
                }),
                page: 0,
                origin: (x, y + 9.0),
                rotation_deg: 0.0,
                font_size: 9.0,
                weight: None,
                advance: Some(6.0),
                draw_index: i,
            });
            i += 1;
        };
        for row in 0..7 {
            for k in 0..30 {
                put(&mut p, 50.0 + k as f64 * 10.0, 100.0 + row as f64 * 12.0);
            }
        }
        for k in 0..45 {
            put(&mut p, 50.0 + k as f64 * 10.0, 184.0);
        }
        for row in 0..14 {
            for x0 in [50.0, 250.0, 460.0] {
                for k in 0..12 {
                    put(&mut p, x0 + k as f64 * 10.0, 220.0 + row as f64 * 12.0);
                }
            }
        }
        let doc = Document { pages: vec![p] };
        let mut copy = doc.clone();
        let opts = AnalyzeOptions {
            emit_anchors: true,
            ..Default::default()
        };
        let analysis = crate::document::analyze_with(&mut copy, &[], &opts);
        assert!(crate::document::column_doubt(&doc.pages[0], &analysis).is_some());
        let off = select(&doc, &analysis, &CropHints::default());
        assert!(off.0.is_empty(), "not asked for: {:?}", off.0);
        let hints = CropHints {
            on_column_doubt: true,
            ..Default::default()
        };
        let (jobs, reasons) = select(&doc, &analysis, &hints);
        assert_eq!(jobs, vec![(0, None)]);
        assert_eq!(reasons.get(&0), Some(&CropReason::ColumnDoubt));
    }

    #[test]
    fn a_doubt_page_with_raster_regions_keeps_their_reason() {
        // A text page with a raster table the glyphs do not cover, doubted
        // as a whole: read whole now, it still holds pixels only a reader
        // can read, so a reader skipping doubt pages must not skip it.
        let mut p = page(0, 1500, false);
        p.images = vec![crate::extract::ImagePlacement {
            bbox: BBox {
                x0: 50.0,
                y0: 50.0,
                x1: 550.0,
                y1: 300.0,
            },
            texty: true,
            ppi: None,
            bitonal: false,
            grid: (1000, 1000),
        }];
        assert!(matches!(
            crate::route::decide(&p).0,
            crate::route::Route::VlmRegions(_)
        ));
        let doc = Document { pages: vec![p] };
        let (jobs, reasons) = reasons_with_doubts(&doc, &[0]);
        assert_eq!(jobs, vec![(0, None)], "the doubt reads the page whole");
        assert_eq!(reasons.get(&0), Some(&CropReason::Region));
    }

    /// A 600 × 800 page with one image over `b`, at `ppi`.
    fn imaged(b: BBox, ppi: Option<f64>, bitonal: bool) -> crate::extract::Page {
        let mut p = page(0, 0, false);
        p.images = vec![crate::extract::ImagePlacement {
            bbox: b,
            texty: true,
            ppi,
            bitonal,
            grid: (1000, 1000),
        }];
        p
    }

    #[test]
    fn a_crop_renders_at_its_images_resolution_within_bounds() {
        let shot = BBox {
            x0: 72.0,
            y0: 100.0,
            x1: 540.0,
            y1: 363.0,
        };
        let at = |ppi, bitonal, mode| crop_scale(&imaged(shot, ppi, bitonal), Some(&shot), mode);
        // The model's scale whatever is under it.
        assert_eq!(at(Some(288.0), false, CropScale::Model), VLM_RENDER_SCALE);
        // A capture at 288 ppi reads at 4 px per point.
        assert_eq!(at(Some(288.0), false, CropScale::Stored), 4.0);
        // Never finer than 300 dpi, never coarser than the model's scale.
        assert!((at(Some(600.0), false, CropScale::Stored) - 300.0 / 72.0).abs() < 1e-5);
        assert_eq!(at(Some(96.0), false, CropScale::Stored), VLM_RENDER_SCALE);
        // A one-bit image, or one of unknown resolution, at the model's.
        assert_eq!(at(Some(288.0), true, CropScale::Stored), VLM_RENDER_SCALE);
        assert_eq!(at(None, false, CropScale::Stored), VLM_RENDER_SCALE);
        // A page with no image under the crop.
        let bare = page(0, 0, false);
        assert_eq!(crop_scale(&bare, None, CropScale::Stored), VLM_RENDER_SCALE);
        // Not rounded: 150 ppi reads at 150 dpi, not the model's 144.
        assert!((at(Some(150.0), false, CropScale::Stored) - 150.0 / 72.0).abs() < 1e-5);
    }

    #[test]
    fn a_crop_held_to_the_long_edge_is_not_drawn_past_it() {
        // Eight small crops asking 2.1 .. 4.15, and one large crop held to
        // 3.38 by its long edge: it would share the 3.9 render, past 4,000 px.
        let mut scales: Vec<f32> = (0..8).map(|i| 2.1 + i as f32 * 0.29).collect();
        scales.push(3.38);
        let mut most = vec![f32::MAX; 8];
        most.push(3.38);
        let at = render_scales(&scales, &most);
        assert_eq!(at[8], 3.38);
        assert!(scales.iter().zip(&at).all(|(asked, got)| got >= asked));
    }

    #[test]
    fn a_page_of_many_scales_renders_a_few_times_none_coarser() {
        let few = [2.5f32, 3.0, 2.5, 4.0];
        let free = [f32::MAX; 300];
        assert_eq!(
            render_scales(&few, &free[..4]),
            few,
            "a few scales are each their own"
        );
        let many: Vec<f32> = (0..300).map(|i| 2.0 + (i % 97) as f32 * 0.02).collect();
        let at = render_scales(&many, &free);
        let mut distinct = at.clone();
        distinct.sort_by(f32::total_cmp);
        distinct.dedup();
        assert!(distinct.len() <= MAX_RENDERS_PER_PAGE, "{distinct:?}");
        assert!(many.iter().zip(&at).all(|(asked, got)| got >= asked));
        assert_eq!(
            distinct.last(),
            many.iter().copied().reduce(f32::max).as_ref()
        );
    }

    #[test]
    fn a_page_is_never_drawn_wider_than_the_renderer_can() {
        // A capture at 300 ppi on a page 20,000 points wide and a few high:
        // within the pixel budget, past the 16-bit canvas.
        let shot = BBox {
            x0: 18_500.0,
            y0: 20.0,
            x1: 18_700.0,
            y1: 60.0,
        };
        let mut p = imaged(shot, Some(300.0), false);
        p.width = 20_000.0;
        p.height = 200.0;
        let s = f64::from(crop_scale(&p, Some(&shot), CropScale::Stored));
        assert!(s * 20_000.0 <= MAX_RENDER_SIDE, "{s}");
        // A page past the canvas at the model scale, nothing under the
        // crop: drawn below the model scale for an engine.
        let mut bare = page(0, 0, false);
        bare.width = 40_000.0;
        bare.height = 100.0;
        let s = f64::from(crop_scale(&bare, None, CropScale::Stored));
        assert!(s * 40_000.0 <= MAX_RENDER_SIDE, "{s}");
    }

    #[test]
    fn a_crop_takes_the_resolution_of_its_capture_not_the_background() {
        // A 288-ppi capture over a full-page one-bit background: the crop
        // shows the capture.
        let shot = BBox {
            x0: 72.0,
            y0: 100.0,
            x1: 300.0,
            y1: 200.0,
        };
        let mut p = imaged(shot, Some(288.0), false);
        p.images.insert(
            0,
            crate::extract::ImagePlacement {
                bbox: BBox {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 600.0,
                    y1: 800.0,
                },
                texty: false,
                ppi: Some(200.0),
                bitonal: true,
                grid: (1000, 1000),
            },
        );
        assert_eq!(crop_scale(&p, Some(&shot), CropScale::Stored), 4.0);
        // The whole page shows the background.
        assert_eq!(crop_scale(&p, None, CropScale::Stored), VLM_RENDER_SCALE);
    }

    #[test]
    fn a_small_capture_on_a_huge_page_keeps_the_page_render_bounded() {
        // A 300-ppi capture on a 200-inch square page: its own resolution
        // would render the page with 3.6 billion pixels.
        let shot = BBox {
            x0: 72.0,
            y0: 100.0,
            x1: 216.0,
            y1: 129.0,
        };
        let within = |p: &crate::extract::Page, s: f32| {
            let s = f64::from(s);
            // Up to the f32 the scale is given in.
            s * s * p.width * p.height <= MAX_PAGE_PIXELS * 1.000_01
        };
        let mut p = imaged(shot, Some(300.0), false);
        p.width = 14_400.0;
        p.height = 14_400.0;
        // The model scale would draw it with 829 million pixels: below it,
        // as large as the budget allows, whatever is under the crop.
        let s = crop_scale(&p, Some(&shot), CropScale::Stored);
        assert!(within(&p, s) && s < VLM_RENDER_SCALE, "{s}");
        assert!(!within(&p, s * 1.001), "as large as the budget allows: {s}");
        for (ppi, bitonal) in [(None, false), (Some(300.0), true)] {
            let mut q = imaged(shot, ppi, bitonal);
            q.width = p.width;
            q.height = p.height;
            assert_eq!(crop_scale(&q, Some(&shot), CropScale::Stored), s);
            assert_eq!(crop_scale(&q, None, CropScale::Stored), s);
        }
        let mut bare = page(0, 0, false);
        bare.width = p.width;
        bare.height = p.height;
        assert_eq!(crop_scale(&bare, None, CropScale::Stored), s);
        // The model's crops stay at the model scale.
        assert_eq!(
            crop_scale(&p, Some(&shot), CropScale::Model),
            VLM_RENDER_SCALE
        );
        // On an A0 page it is drawn within the budget.
        p.width = 2384.0;
        p.height = 3370.0;
        let s = crop_scale(&p, Some(&shot), CropScale::Stored);
        assert!(within(&p, s), "{s}");
        assert!(s > VLM_RENDER_SCALE, "{s}");
    }

    #[test]
    fn a_large_page_is_held_to_the_long_edge_cap() {
        // A poster 2,000 points high, all one 300-ppi image: 300 dpi would
        // draw 8,333 px, the cap allows 4,000.
        let mut p = imaged(
            BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 1400.0,
                y1: 2000.0,
            },
            Some(300.0),
            false,
        );
        p.width = 1400.0;
        p.height = 2000.0;
        let s = crop_scale(&p, None, CropScale::Stored);
        assert!(
            (f64::from(s) - MAX_LONG_EDGE_PX / 2000.0).abs() < 1e-3,
            "{s}"
        );
    }

    #[test]
    fn a_region_ocr_read_is_not_asked_for_again() {
        // A text page with a raster table the glyphs do not cover: one crop
        // for it, until OCR has read the region.
        let mut p = page(0, 1500, false);
        p.images = vec![crate::extract::ImagePlacement {
            bbox: BBox {
                x0: 50.0,
                y0: 50.0,
                x1: 550.0,
                y1: 300.0,
            },
            texty: true,
            ppi: None,
            bitonal: false,
            grid: (1000, 1000),
        }];
        let doc = Document { pages: vec![p] };
        let (jobs, _) = reasons_with_doubts(&doc, &[]);
        assert_eq!(jobs.len(), 1);
        let mut read = doc.clone();
        read.pages[0].ocr = Some(crate::extract::OcrRead {
            first_glyph: 1500,
            whole: false,
            regions: vec![0],
        });
        let (jobs, _) = reasons_with_doubts(&read, &[]);
        assert!(jobs.is_empty(), "{jobs:?}");
    }

    #[test]
    fn a_doubt_page_whose_regions_are_cropped_apart_keeps_its_doubt() {
        let mut p = page(0, 1500, false);
        p.images = vec![crate::extract::ImagePlacement {
            bbox: BBox {
                x0: 50.0,
                y0: 50.0,
                x1: 550.0,
                y1: 300.0,
            },
            texty: true,
            ppi: None,
            bitonal: false,
            grid: (1000, 1000),
        }];
        let doc = Document { pages: vec![p] };
        let mut copy = doc.clone();
        let opts = AnalyzeOptions {
            emit_anchors: true,
            ..Default::default()
        };
        let mut analysis = crate::document::analyze_with(&mut copy, &[], &opts);
        analysis.suspect_figures = vec![crate::figure::Doubt {
            page: 0,
            share: 0.9,
            chars: 500,
        }];
        let hints = CropHints {
            regions_apart: true,
            ..Default::default()
        };
        let (jobs, reasons) = select(&doc, &analysis, &hints);
        assert_eq!(jobs, vec![(0, None)]);
        assert_eq!(reasons.get(&0), Some(&CropReason::FigureDoubt));
    }

    #[test]
    fn the_budget_keeps_a_doubt_page_with_raster_regions_before_a_table() {
        // The page above, read whole for a doubt, and an untrusted table on
        // another page: under a budget of one, the pixels only a reader can
        // read come before a table whose text the deterministic pass has.
        let mut p = page(0, 1500, false);
        p.images = vec![crate::extract::ImagePlacement {
            bbox: BBox {
                x0: 50.0,
                y0: 50.0,
                x1: 550.0,
                y1: 300.0,
            },
            texty: true,
            ppi: None,
            bitonal: false,
            grid: (1000, 1000),
        }];
        let doc = Document {
            pages: vec![p, page(1, 500, false)],
        };
        let (mut jobs, reasons) = reasons_with_doubts(&doc, &[0]);
        assert_eq!(reasons.get(&0), Some(&CropReason::Region));
        jobs.push((1, Some(vec![region("t0")])));
        let (kept, dropped) = within_budget(&doc, jobs, 1);
        assert_eq!(dropped, 1);
        assert_eq!(kept, vec![(0, None)], "the region page before the table");
    }

    #[test]
    fn region_tags_name_their_reason() {
        assert_eq!(CropReason::of_region_tag("r0"), Some(CropReason::Region));
        assert_eq!(
            CropReason::of_region_tag("n1"),
            Some(CropReason::TableMissing)
        );
        assert_eq!(CropReason::of_region_tag("t2"), Some(CropReason::Table));
        assert_eq!(CropReason::of_region_tag("full"), None);
    }

    #[test]
    fn the_budget_keeps_what_would_otherwise_be_lost() {
        let doc = Document {
            pages: vec![page(0, 0, true), page(1, 500, false), page(2, 500, false)],
        };
        // A scanned page, a page escalated on heading doubt, and a page
        // with one untrusted table and one missing one.
        let jobs: CropJobs = vec![
            (0, None),
            (1, None),
            (2, Some(vec![region("t0"), region("n0")])),
        ];
        assert_eq!(crop_count(&jobs), 4);
        let (kept, dropped) = within_budget(&doc, jobs.clone(), 2);
        assert_eq!(dropped, 2);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].0, 0, "the scanned page is read before anything");
        assert!(kept[0].1.is_none());
        assert_eq!(kept[1].0, 2);
        assert_eq!(
            kept[1].1.as_ref().unwrap()[0].0,
            "n0",
            "a missing table before a doubted one"
        );

        let (all, none) = within_budget(&doc, jobs, 4);
        assert_eq!(none, 0);
        assert_eq!(crop_count(&all), 4);
    }

    #[test]
    fn each_crop_kind_gets_its_own_prompt() {
        assert!(prompt_for_crop(&crop("p0_t0"), &[]).starts_with("This image is one table"));
        assert!(prompt_for_crop(&crop("p0_full"), &[]).starts_with("Transcribe this page"));
        assert!(prompt_for_crop(&crop("p0_r0"), &[]).starts_with("Transcribe what is printed"));
    }

    #[test]
    fn a_region_a_detector_boxed_a_table_in_is_told_so() {
        let mut c = crop("p0_r0");
        c.bbox = Some(BBox {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 100.0,
        });
        let mut hints = CropHints::default();
        hints.layout_tables.insert(
            0,
            vec![BBox {
                x0: 50.0,
                y0: 50.0,
                x1: 150.0,
                y1: 150.0,
            }],
        );
        assert!(prompt_for_crop_with(&c, &hints, &[]).starts_with("This image contains a table"));
        assert!(prompt_for_crop(&c, &[]).starts_with("Transcribe what is printed"));
        // A whole page and a table crop are prompted as they always are.
        assert!(
            prompt_for_crop_with(&crop("p0_full"), &hints, &[]).starts_with("Transcribe this page")
        );
        assert!(prompt_for_crop_with(&crop("p0_t0"), &hints, &[])
            .starts_with("This image is one table"));
    }

    #[test]
    fn every_prose_prompt_forbids_inventing_a_link() {
        for c in ["p0_full", "p0_r0"] {
            assert!(prompt_for_crop(&crop(c), &[]).contains("NEVER invent a link"));
        }
    }

    #[test]
    fn known_links_are_listed_and_tables_are_not_offered_them() {
        let links = vec![(
            "the filing".to_string(),
            "https://sec.example/x".to_string(),
        )];
        let p = prompt_for_crop(&crop("p0_full"), &links);
        assert!(p.contains("\"the filing\" links to https://sec.example/x"));
        assert!(!prompt_for_crop(&crop("p0_t0"), &links).contains("sec.example"));
    }

    #[test]
    fn readings_round_trip_and_label_tables() {
        let mut r = Readings::default();
        r.insert("p0_t0".into(), "<table><tr><td>a</td></tr></table>".into());
        r.insert("p3_full".into(), "# Title".into());
        assert_eq!(r.len(), 2);
        assert_eq!(r.read("doc", "p0_t0").unwrap()[0].label, "table");
        assert_eq!(r.read("doc", "p3_full").unwrap()[0].label, "text");
        assert!(r.read("doc", "p9_full").is_none());

        let replay = Readings::from_map(r.to_map().clone());
        assert_eq!(replay.read("doc", "p3_full").unwrap()[0].content, "# Title");
    }
}
