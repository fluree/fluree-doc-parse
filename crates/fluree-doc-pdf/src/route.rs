//! Page routing: deterministic layout vs the VLM tier.
//!
//! The router's objective is asymmetric. A deterministic page costs
//! milliseconds; a VLM page costs seconds of GPU. So the question is never
//! "might the VLM do better?" — on ordinary born-digital pages it does not,
//! and routing them would multiply average latency for nothing. Measured on a
//! TI mechanical package drawing, the VLM returned the whole drawing as one
//! opaque image block and every dimension callout inside it as `<img>`, where
//! the deterministic path extracts that same text with per-glyph rotation: for
//! CAD the arrow points the other way. The question is "is the
//! deterministic output *unusable*?", which happens for exactly two reasons:
//!
//! 1. **The text is pixels.** A scanned page carries one large image and few
//!    or no glyphs. No geometric analysis of absent glyphs can parse it.
//! 2. **The text is garbage.** Broken CID fonts extract glyphs whose Unicode
//!    is unknown or wrong; the layout may be perfect and the text worthless.
//!
//! Both conditions are measured, not inferred: image coverage and glyph count
//! for the first, Unicode resolution rate for the second.

use crate::extract::{ImagePlacement, Page};
use crate::geom::BBox;

/// Fraction of the page a single image (or the union of images) must cover
/// before the page is image-dominated. Half the page: figures and photos in
/// ordinary documents run 10-30%; scans run 90%+.
pub const IMAGE_DOMINANT_COVERAGE: f64 = 0.5;

/// Below this many text-bearing glyphs, an image-dominated page has no
/// meaningful born-digital text layer. A caption under a full-page figure is
/// ~10-40 glyphs; a page of prose is 1500+.
const MIN_GLYPHS_FOR_TEXT_PAGE: usize = 100;

/// Unicode resolution below this means the text layer cannot be trusted even
/// though glyphs exist — broken CID fonts, symbol soup.
const MIN_UNICODE_RATE: f64 = 0.85;

/// Below this many glyphs a page has effectively no text layer; with an image
/// present, the image *is* the page.
const NEAR_BLANK_GLYPHS: usize = 20;

/// A raster region must cover at least this fraction of the page before it is
/// worth a VLM call: below it, whatever text it holds is a logo or ornament.
pub(crate) const MIN_REGION_FRACTION: f64 = 0.10;

/// Below this, in PDF units, on its longer side, a texty image too small for
/// a model is not listed as declined ([`Declined::Small`]): half an inch,
/// three lines of body text. Checkboxes, bullets and icons run 6 to 14
/// units a side, and a stencil (an image mask) is texty whatever it draws,
/// so a form would list every box; a pasted capture of a date or an account
/// number is one line high but a couple of inches wide. An OCR engine is
/// still offered them: the floor is about what is worth reporting.
const MIN_SMALL_REGION_SIDE: f64 = 36.0;

/// Above this many glyphs inside a candidate region, the text layer already
/// reads it — a figure with overlaid glyph labels, or text drawn on a
/// background image. A handful is tolerated for stray watermarks or a
/// caption grazing the box.
const MAX_GLYPHS_IN_UNREAD_REGION: usize = 15;

/// Fewest pixels an image needs on its short side to hold a line of text:
/// its own pixels, whatever size it is drawn at.
///
/// Measured with the built-in engine (PP-OCRv6 small) on a line of sans
/// serif set to fill images 5 to 20 px tall, a pixel of paper above and
/// below: from 10 px it reads the line exactly; at 9 it runs two words
/// together, at 8 it misreads letters and digits, at 6 and 7 every word,
/// and at 5 it reads nothing usable. An engine scales what it reads up
/// (to 736 px for detection, 48 px a line for recognition), which adds no
/// strokes. So a checkbox drawn from an 8 × 8 mask or a rule stretched from
/// a 1 × 1 one, which a page may hold by the hundred, is never read.
pub const MIN_TEXT_LINE_PX: u32 = 10;

/// Grid resolution for image-coverage union. Coarse is fine: the signal is
/// "half the page", not a measurement.
const COVERAGE_BINS: usize = 32;

/// Margin, in PDF units, drawn around a region crop so the reader sees a
/// clean edge rather than letters sliced mid-stroke. Part of the crop's
/// definition, so whoever maps a reading back onto the page — the model
/// splice, OCR words — measures from the same edge.
pub const CROP_MARGIN: f64 = 6.0;

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// The deterministic pipeline's output is trustworthy.
    Deterministic,
    /// Send the whole rendered page to the VLM.
    Vlm(Reason),
    /// The text layer is fine, but these raster regions carry text or table
    /// structure the deterministic path cannot read. Render each region, send
    /// it to the VLM, and splice the result into the deterministic output at
    /// the region's position.
    VlmRegions(Vec<BBox>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Image-dominated page with no meaningful text layer: a scan, with or
    /// without a few glyphs (a stamped header, a page number) on it.
    Scanned,
    /// Almost no glyphs and an image covering under half the page: a logo on
    /// a separator sheet, a signature. The content, if any, is the image.
    NearBlank,
    /// Glyphs exist but their Unicode cannot be trusted.
    BrokenText,
}

/// Per-page signals, exposed for diagnostics (`fdoc route`).
#[derive(Debug, Clone, Copy)]
pub struct Signals {
    pub glyphs: usize,
    pub unicode_rate: f64,
    pub image_coverage: f64,
}

pub fn signals(page: &Page) -> Signals {
    let glyphs = page
        .glyphs
        .iter()
        .filter(|g| !g.text.trim().is_empty())
        .count();
    // Unresolved means *no* Unicode at all. An explicit space glyph carries
    // " " as its text and is perfectly resolved — counting whitespace as
    // unresolved made every ordinary page look ~15% broken and routed half
    // the corpus.
    let resolved = page.glyphs.iter().filter(|g| !g.text.is_empty()).count();
    let total = page.glyphs.len();
    let unicode_rate = if total == 0 {
        1.0
    } else {
        resolved as f64 / total as f64
    };

    // Union of image areas on a coarse grid, as a fraction of the page.
    let mut grid = [false; COVERAGE_BINS * COVERAGE_BINS];
    let (pw, ph) = (page.width.max(1.0), page.height.max(1.0));
    for pl in &page.images {
        let b = &pl.bbox;
        let c0 = ((b.x0 / pw * COVERAGE_BINS as f64).floor().max(0.0)) as usize;
        let c1 = ((b.x1 / pw * COVERAGE_BINS as f64).ceil() as usize).min(COVERAGE_BINS);
        let r0 = ((b.y0 / ph * COVERAGE_BINS as f64).floor().max(0.0)) as usize;
        let r1 = ((b.y1 / ph * COVERAGE_BINS as f64).ceil() as usize).min(COVERAGE_BINS);
        for r in r0..r1 {
            for c in c0..c1 {
                grid[r * COVERAGE_BINS + c] = true;
            }
        }
    }
    let covered = grid.iter().filter(|x| **x).count();
    let image_coverage = covered as f64 / (COVERAGE_BINS * COVERAGE_BINS) as f64;

    Signals {
        glyphs,
        unicode_rate,
        image_coverage,
    }
}

/// The raster regions of a text page whose pixels look like text, hold
/// enough of them for a line of it ([`MIN_TEXT_LINE_PX`]), and which the
/// glyph layer does not cover, at least `min_fraction` of the page each, in
/// the order the page places them. An image drawn twice in one place (a
/// stamp repeated, an image and its mask) is one region, at its first
/// placement: it is read once.
fn unread_regions(page: &Page, min_fraction: f64) -> Vec<BBox> {
    let mut text = None;
    let mut seen = std::collections::HashSet::new();
    page.images
        .iter()
        .filter(|p| {
            p.texty == Some(true) && holds_a_line(p) && share(page, &p.bbox) >= min_fraction
        })
        .filter(|p| {
            text.get_or_insert_with(|| TextLayer::of(page))
                .void(&p.bbox)
        })
        .map(|p| p.bbox)
        .filter(|b| seen.insert(box_key(b)))
        .collect()
}

/// Whether an image has the pixels for a line of text on its short side
/// ([`MIN_TEXT_LINE_PX`]), whatever size it is drawn at.
fn holds_a_line(p: &ImagePlacement) -> bool {
    p.grid.0.min(p.grid.1) >= MIN_TEXT_LINE_PX
}

/// A box as a key, to tell an image drawn twice in one place.
fn box_key(b: &BBox) -> [u64; 4] {
    [b.x0, b.y0, b.x1, b.y1].map(f64::to_bits)
}

/// What each of `regions`, boxes of `page`'s images, shows, as a key
/// ([`ImagePlacement::key`]): the key of the images drawn in that box, when
/// they all have the one; `None` when one has none, two in the box differ,
/// or no image is drawn there.
pub(crate) fn region_keys(page: &Page, regions: &[BBox]) -> Vec<Option<u64>> {
    let mut by_box: std::collections::HashMap<[u64; 4], Option<u64>> =
        std::collections::HashMap::new();
    for p in &page.images {
        by_box
            .entry(box_key(&p.bbox))
            .and_modify(|k| {
                if *k != p.key {
                    *k = None;
                }
            })
            .or_insert(p.key);
    }
    regions
        .iter()
        .map(|b| by_box.get(&box_key(b)).copied().flatten())
        .collect()
}

/// The share of the page a box covers.
pub(crate) fn share(page: &Page, b: &BBox) -> f64 {
    (b.x1 - b.x0) * (b.y1 - b.y0) / (page.width * page.height).max(1.0)
}

/// Widest gap, in PDF units, between two images the text layer leaves
/// void for them to be drawn as one picture ([`pasted`]): a scanner or a
/// producer that stores a scan in strips lays them edge to edge, give or
/// take the rounding of their placement.
const ABUTTING_GAP: f64 = 2.0;

/// Whether a raster region of `page` (a box of its images, in the page's
/// reading frame) is a screen pasted into it rather than the page's
/// content: the picture it is part of covers under half the page
/// ([`IMAGE_DOMINANT_COVERAGE`]). A heading read in a pasted region is the
/// screen's, not the document's, whoever reads it: OCR
/// ([`crate::ocr::Injected::pasted`]) or a model.
///
/// The picture is the region and every image the text layer leaves void
/// that touches it, as the router judges a page image-dominated on the
/// union of its images, not image by image: a scan stored in strips under
/// a fax header the text layer carries is routed as one region a strip,
/// each under half the page, and the scan they make is the page's content.
/// An image the text layer covers (a letterhead drawn behind the page's
/// text) is the page's background, and does not join a picture.
///
/// To judge several regions of one page, [`pasted_set`] measures the
/// page's pictures once for all of them.
pub fn pasted(page: &Page, b: &BBox) -> bool {
    pasted_set(page, std::slice::from_ref(b))[0]
}

/// [`pasted`] for each of `boxes`, raster regions of one `page`, in their
/// order. The page's pictures are measured once, whatever the number of
/// regions: a capture a printer driver stores in thousands of strips one
/// pixel high is thousands of regions, each part of one picture.
pub fn pasted_set(page: &Page, boxes: &[BBox]) -> Vec<bool> {
    let mut pictures = None;
    boxes
        .iter()
        .map(|b| {
            share(page, b) < IMAGE_DOMINANT_COVERAGE
                && pictures
                    .get_or_insert_with(|| Pictures::of(page))
                    .under_half(page, b)
        })
        .collect()
}

/// Whether two boxes are no further apart than [`ABUTTING_GAP`].
fn abut(a: &BBox, o: &BBox) -> bool {
    a.x0 <= o.x1 + ABUTTING_GAP
        && o.x0 <= a.x1 + ABUTTING_GAP
        && a.y0 <= o.y1 + ABUTTING_GAP
        && o.y0 <= a.y1 + ABUTTING_GAP
}

/// Cells a side of the grid [`Pictures`] files a page's images on.
const PICTURE_BINS: usize = 64;

/// The pictures of a page, for [`pasted_set`]: its images the text layer
/// leaves void, joined where they abut ([`ABUTTING_GAP`]), each picture
/// with the share of the page its union covers. Filed on a grid of the
/// page, so that a region is measured against the images near it.
struct Pictures {
    /// Cell size, in PDF units.
    cell: (f64, f64),
    /// The void images' boxes.
    boxes: Vec<BBox>,
    /// The picture each void image's box is part of, by the box.
    by_box: std::collections::HashMap<[u64; 4], usize>,
    /// Each picture's boxes, by index into `boxes`.
    members: Vec<Vec<usize>>,
    /// The share of the page each picture's union covers.
    covers: Vec<f64>,
    /// Each cell's boxes, reached past their edge by the gap, by picture:
    /// a capture stored in strips is one picture, which a region not part
    /// of it is measured against once a cell, not once a strip.
    grid: Vec<Vec<(usize, Vec<usize>)>>,
}

impl Pictures {
    fn of(page: &Page) -> Self {
        let text = TextLayer::of(page);
        let boxes: Vec<BBox> = (page.images.iter())
            .map(|p| p.bbox)
            .filter(|b| text.void(b))
            .collect();
        let mut pictures = Pictures {
            cell: (
                page.width.max(1.0) / PICTURE_BINS as f64,
                page.height.max(1.0) / PICTURE_BINS as f64,
            ),
            boxes,
            by_box: std::collections::HashMap::new(),
            members: Vec::new(),
            covers: Vec::new(),
            grid: vec![Vec::new(); PICTURE_BINS * PICTURE_BINS],
        };
        // Union-find over the boxes, each joined to the groups it abuts
        // among those filed before it in the cells it reaches. A cell keeps
        // its boxes by group, so that a box is not measured against the
        // boxes of its own group: thousands of strips in one cell are one
        // group, and a strip is measured against a few.
        let mut parent: Vec<usize> = (0..pictures.boxes.len()).collect();
        for i in 0..pictures.boxes.len() {
            let b = pictures.boxes[i];
            for c in pictures.cells(&b) {
                let groups = &mut pictures.grid[c];
                regroup(groups, &mut parent);
                for (r, members) in groups.iter() {
                    let own = root(&mut parent, i);
                    if *r != own && members.iter().any(|&j| abut(&b, &pictures.boxes[j])) {
                        parent[own] = *r;
                    }
                }
                regroup(groups, &mut parent);
                let own = root(&mut parent, i);
                match groups.iter_mut().find(|(r, _)| *r == own) {
                    Some((_, members)) => members.push(i),
                    None => groups.push((own, vec![i])),
                }
            }
        }
        let mut id = std::collections::HashMap::new();
        let mut picture = Vec::with_capacity(pictures.boxes.len());
        for i in 0..pictures.boxes.len() {
            let r = root(&mut parent, i);
            let next = id.len();
            let p = *id.entry(r).or_insert(next);
            if p == pictures.members.len() {
                pictures.members.push(Vec::new());
            }
            pictures.members[p].push(i);
            picture.push(p);
        }
        for groups in pictures.grid.iter_mut() {
            regroup(groups, &mut parent);
            for (r, members) in groups.iter_mut() {
                *r = picture[members[0]];
            }
        }
        pictures.by_box = (pictures.boxes.iter().zip(&picture))
            .map(|(b, &p)| (box_key(b), p))
            .collect();
        pictures.covers = (pictures.members.iter())
            .map(|m| {
                let boxes: Vec<BBox> = m.iter().map(|&i| pictures.boxes[i]).collect();
                cover(page, &boxes)
            })
            .collect();
        pictures
    }

    /// The cells a box reaches, past its edges by the gap.
    fn cells(&self, b: &BBox) -> impl Iterator<Item = usize> {
        let bin = |v: f64, size: f64| ((v / size).floor().max(0.0) as usize).min(PICTURE_BINS - 1);
        let g = ABUTTING_GAP;
        let cols = bin(b.x0 - g, self.cell.0)..=bin(b.x1 + g, self.cell.0);
        let rows = bin(b.y0 - g, self.cell.1)..=bin(b.y1 + g, self.cell.1);
        rows.flat_map(move |r| cols.clone().map(move |c| r * PICTURE_BINS + c))
    }

    /// Whether region `b` and the pictures it abuts cover under half the
    /// page: the picture it is part of, or, for a region the text layer
    /// does not leave void, every picture it touches.
    fn under_half(&self, page: &Page, b: &BBox) -> bool {
        // A void image's box is part of its picture, and every box it
        // abuts is too: the picture's union is the region's.
        if let Some(&p) = self.by_box.get(&box_key(b)) {
            return self.covers[p] < IMAGE_DOMINANT_COVERAGE;
        }
        let mut touched: Vec<usize> = Vec::new();
        for c in self.cells(b) {
            for (p, members) in &self.grid[c] {
                if !touched.contains(p) && members.iter().any(|&j| abut(b, &self.boxes[j])) {
                    touched.push(*p);
                }
            }
        }
        // Bounds first: the union is at least its largest picture, and at
        // most the region and the pictures' unions summed.
        let largest = touched.iter().map(|&p| self.covers[p]).fold(0.0, f64::max);
        if largest >= IMAGE_DOMINANT_COVERAGE {
            return false;
        }
        let sum: f64 = share(page, b) + touched.iter().map(|&p| self.covers[p]).sum::<f64>();
        if sum < IMAGE_DOMINANT_COVERAGE {
            return true;
        }
        let picture: Vec<BBox> = std::iter::once(*b)
            .chain(
                touched
                    .iter()
                    .flat_map(|&p| self.members[p].iter().map(|&j| self.boxes[j])),
            )
            .collect();
        union_share(page, &picture) < IMAGE_DOMINANT_COVERAGE
    }
}

/// The root of `i` in a union-find, halving the path to it.
fn root(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// A cell's groups, each under its root now, those joined since merged:
/// the smaller into the larger, so a box moves a few times at most.
fn regroup(groups: &mut Vec<(usize, Vec<usize>)>, parent: &mut [usize]) {
    for g in groups.iter_mut() {
        g.0 = root(parent, g.0);
    }
    let mut i = 0;
    while i < groups.len() {
        match (0..i).find(|&k| groups[k].0 == groups[i].0) {
            Some(k) => {
                let (_, mut moved) = groups.swap_remove(i);
                if moved.len() > groups[k].1.len() {
                    std::mem::swap(&mut moved, &mut groups[k].1);
                }
                groups[k].1.append(&mut moved);
            }
            None => i += 1,
        }
    }
}

/// The share of the page `boxes` cover together: their areas summed while
/// that is under half the page, which bounds their union's, else their
/// union's exactly ([`union_share`]).
fn cover(page: &Page, boxes: &[BBox]) -> f64 {
    let sum: f64 = boxes.iter().map(|o| share(page, o)).sum();
    if sum < IMAGE_DOMINANT_COVERAGE {
        sum
    } else {
        union_share(page, boxes)
    }
}

/// The share of the page the union of `boxes` covers, exactly: column by
/// column between the boxes' vertical edges, the lengths of the merged
/// spans of the boxes crossing it.
fn union_share(page: &Page, boxes: &[BBox]) -> f64 {
    let mut xs: Vec<f64> = boxes.iter().flat_map(|b| [b.x0, b.x1]).collect();
    xs.sort_by(f64::total_cmp);
    xs.dedup();
    let mut area = 0.0;
    for w in xs.windows(2) {
        let (x0, x1) = (w[0], w[1]);
        let mut spans: Vec<(f64, f64)> = boxes
            .iter()
            .filter(|b| b.x0 <= x0 && b.x1 >= x1)
            .map(|b| (b.y0, b.y1))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut covered = 0.0;
        let mut open: Option<(f64, f64)> = None;
        for (y0, y1) in spans {
            match open {
                Some((o0, o1)) if y0 <= o1 => open = Some((o0, o1.max(y1))),
                _ => {
                    if let Some((o0, o1)) = open {
                        covered += o1 - o0;
                    }
                    open = Some((y0, y1));
                }
            }
        }
        if let Some((o0, o1)) = open {
            covered += o1 - o0;
        }
        area += covered * (x1 - x0);
    }
    area / (page.width * page.height).max(1.0)
}

/// Cells a side of the grid [`TextLayer`] files glyphs on.
const TEXT_LAYER_BINS: usize = 64;

/// A page's own text layer, each text-bearing glyph by its centre, filed on
/// a grid of the page so that a box is measured against the glyphs of the
/// cells it covers, not against every glyph of the page. A page may carry
/// icons or checkboxes by the thousand, each measured: one pass over the
/// glyphs, then a few cells an image.
struct TextLayer {
    /// Cell size, in PDF units.
    cell: (f64, f64),
    /// Where each cell's centres start in `centres`, and, last, their count.
    starts: Vec<usize>,
    /// The centres, cell by cell, row by row.
    centres: Vec<(f64, f64)>,
}

impl TextLayer {
    fn of(page: &Page) -> Self {
        let cell = (
            page.width.max(1.0) / TEXT_LAYER_BINS as f64,
            page.height.max(1.0) / TEXT_LAYER_BINS as f64,
        );
        let own: Vec<(f64, f64)> = page
            .glyphs
            .iter()
            // An OCR reading's glyphs fill the void they read, and are not
            // the text layer: counted, the region would drop out of the list
            // and every region after it renumber.
            .filter(|g| !g.text.trim().is_empty() && !page.is_ocr(g))
            .map(|g| match g.bbox {
                Some(b) => ((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5),
                None => g.origin,
            })
            .collect();
        let at = |(x, y): (f64, f64)| Self::bin(y, cell.1) * TEXT_LAYER_BINS + Self::bin(x, cell.0);
        let mut starts = vec![0; TEXT_LAYER_BINS * TEXT_LAYER_BINS + 1];
        for &c in &own {
            starts[at(c) + 1] += 1;
        }
        for i in 1..starts.len() {
            starts[i] += starts[i - 1];
        }
        let mut next = starts.clone();
        let mut centres = vec![(0.0, 0.0); own.len()];
        for c in own {
            let i = at(c);
            centres[next[i]] = c;
            next[i] += 1;
        }
        TextLayer {
            cell,
            starts,
            centres,
        }
    }

    /// The cell a coordinate falls in along one axis; a glyph off the page
    /// is filed in the edge cell, where a box reaching past the edge finds
    /// it.
    fn bin(v: f64, size: f64) -> usize {
        ((v / size).floor().max(0.0) as usize).min(TEXT_LAYER_BINS - 1)
    }

    /// Whether the text layer leaves this box all but empty: what is drawn
    /// there, nothing but the pixels hold.
    fn void(&self, b: &BBox) -> bool {
        let (c0, c1) = (Self::bin(b.x0, self.cell.0), Self::bin(b.x1, self.cell.0));
        let (r0, r1) = (Self::bin(b.y0, self.cell.1), Self::bin(b.y1, self.cell.1));
        let mut inside = 0;
        for r in r0..=r1 {
            let row = r * TEXT_LAYER_BINS;
            let cells = self.starts[row + c0]..self.starts[row + c1 + 1];
            for &(x, y) in &self.centres[cells] {
                if b.contains(x, y) {
                    inside += 1;
                    if inside >= MAX_GLYPHS_IN_UNREAD_REGION {
                        return false;
                    }
                }
            }
        }
        true
    }
}

/// Why the router sends no model to a raster region whose pixels the text
/// layer does not cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Declined {
    /// Its pixels look like text, but it covers under a tenth of the page
    /// ([`MIN_REGION_FRACTION`]): a logo or an ornament as often as a pasted
    /// capture. An OCR engine reads it ([`ocr_regions`]).
    Small,
    /// A tenth of the page or more, but its pixels do not look like text or
    /// a table to the probe ([`fluree_doc_vision::texty`]): a photograph, a
    /// chart, an illustration. Nothing reads it, neither a model nor an OCR
    /// engine: it is listed, for a caller to judge.
    NotTexty,
    /// Not examined: the parse did not decode the image
    /// ([`crate::ExtractOptions::probe_images`] off, as a deterministic run
    /// that neither escalates nor reads with an OCR engine sets it, or
    /// [`crate::ExtractOptions::probe_small_images`] off for one under a
    /// tenth of the page, as a run without OCR sets it), so nothing tells a
    /// capture of text from a photograph. Listed at any
    /// share of the page, from [`MIN_SMALL_REGION_SIDE`] on its longer side:
    /// a run that probes may find text in it.
    Unprobed,
}

/// The raster regions of a page that the text layer does not cover and the
/// router sends no model to, each with why, in the order the page places
/// them: what a caller may still want read.
///
/// Empty for a page read whole, whose regions go with the page. An image
/// that is neither texty nor a tenth of the page (an icon, a bullet, a
/// small photo) is not listed: nothing in it suggests content. Nor is a
/// texty or unprobed one under [`MIN_SMALL_REGION_SIDE`] (a checkbox, an
/// icon), one
/// whose own pixels cannot hold a line of text ([`MIN_TEXT_LINE_PX`], a
/// mask stretched into a rule), or a second drawing of an image in one
/// place.
pub fn declined_regions(page: &Page) -> Vec<(BBox, Declined)> {
    declined_regions_for(page, &page.routing().route)
}

/// [`declined_regions`] of a page whose route the caller already has from
/// [`decide`], to route it once.
pub fn declined_regions_for(page: &Page, route: &Route) -> Vec<(BBox, Declined)> {
    declined_regions_numbered(page, route)
        .into_iter()
        .map(|(b, why, _)| (b, why))
        .collect()
}

/// [`declined_regions_for`], each [`Declined::Small`] region with its
/// number among the regions an OCR engine reads ([`ocr_regions_for`]): what
/// a caller needs to tell whether an engine read it, without listing those
/// regions again. A [`Declined::NotTexty`] or [`Declined::Unprobed`] one has
/// none: no engine reads it.
pub fn declined_regions_numbered(
    page: &Page,
    route: &Route,
) -> Vec<(BBox, Declined, Option<usize>)> {
    let routed: &[BBox] = match route {
        Route::Vlm(_) => return Vec::new(),
        Route::VlmRegions(r) => r,
        Route::Deterministic => &[],
    };
    // Looked at only when one is worth listing, and only up to the last
    // such: a page may carry icons and bullets by the thousand. A small
    // texty image is counted whether listed or not, for the number of those
    // after it: an OCR engine reads every one.
    let large = |p: &ImagePlacement| share(page, &p.bbox) >= MIN_REGION_FRACTION;
    let sized = |p: &ImagePlacement| longer_side(&p.bbox) >= MIN_SMALL_REGION_SIDE;
    let Some(last) = page.images.iter().rposition(|p| {
        holds_a_line(p)
            && match p.texty {
                Some(true) => !large(p) && sized(p),
                Some(false) => large(p),
                None => sized(p),
            }
    }) else {
        return Vec::new();
    };
    let mut text = None;
    let mut seen = std::collections::HashSet::new();
    let mut read = routed.len();
    let mut out = Vec::new();
    for p in &page.images[..=last] {
        let worth = match p.texty {
            Some(true) => true,
            Some(false) => large(p),
            None => sized(p),
        };
        if !worth || !holds_a_line(p) || routed.contains(&p.bbox) || !seen.insert(box_key(&p.bbox))
        {
            continue;
        }
        if !text
            .get_or_insert_with(|| TextLayer::of(page))
            .void(&p.bbox)
        {
            continue;
        }
        match p.texty {
            None => {
                out.push((p.bbox, Declined::Unprobed, None));
                continue;
            }
            Some(false) => {
                out.push((p.bbox, Declined::NotTexty, None));
                continue;
            }
            Some(true) => {}
        }
        let number = read;
        read += 1;
        // A checkbox or an icon; or, texty and large, a region the page's
        // routing does not route, which only a routing kept from before the
        // page changed can leave.
        if !large(p) && sized(p) {
            out.push((p.bbox, Declined::Small, Some(number)));
        }
    }
    out
}

/// The images an OCR engine reads on a page past its routed regions, in
/// the order the page places them: those the text layer does not cover
/// whose pixels look like text and hold a line of it, whatever size they
/// are drawn at, each box once.
fn ocr_extras<'a>(page: &'a Page, routed: &'a [BBox]) -> impl Iterator<Item = BBox> + 'a {
    let mut text = None;
    let mut seen = std::collections::HashSet::new();
    page.images
        .iter()
        .filter(move |p| p.texty == Some(true) && holds_a_line(p) && !routed.contains(&p.bbox))
        .filter(move |p| {
            text.get_or_insert_with(|| TextLayer::of(page))
                .void(&p.bbox)
        })
        .map(|p| p.bbox)
        .filter(move |b| seen.insert(box_key(b)))
}

/// A box's longer side.
fn longer_side(b: &BBox) -> f64 {
    (b.x1 - b.x0).max(b.y1 - b.y0)
}

/// The regions an OCR engine reads on a page: the routed ones, numbered as
/// [`decide`] numbers them, then, in the order the page places them, the
/// texty ones too small to be worth a model's call ([`Declined::Small`]).
///
/// [`MIN_REGION_FRACTION`] is a price, not a property of the pixels: a model
/// call costs the same for a logo as for a page, and below a tenth of the
/// page it rarely pays. An OCR engine runs here and bills nothing, and a
/// pasted capture an inch high still holds a date or an account number.
/// What it reads in any region is held to the noise rule
/// ([`crate::ocr::MIN_PICTURE_CONFIDENCE`]), a net behind the probe. A
/// region the pixel probe does not take for text ([`Declined::NotTexty`])
/// is read by neither: the probe is what tells a reader where text is.
/// Numbered after the routed regions, so a model's crop names (`r0`, `r1`,
/// …) mean the same regions whether or not an engine also reads the small
/// ones.
///
/// Empty for a page read whole (scanned, near-blank, broken text).
pub fn ocr_regions(page: &Page) -> Vec<BBox> {
    ocr_regions_for(page, &page.routing().route)
}

/// [`ocr_regions`] of a page whose route the caller already has from
/// [`decide`], to route it once.
pub fn ocr_regions_for(page: &Page, route: &Route) -> Vec<BBox> {
    let routed: &[BBox] = match route {
        Route::Vlm(_) => return Vec::new(),
        Route::VlmRegions(r) => r,
        Route::Deterministic => &[],
    };
    let mut all = routed.to_vec();
    all.extend(ocr_extras(page, routed));
    all
}

pub fn decide(page: &Page) -> (Route, Signals) {
    let s = signals(page);

    // The scan rule first: a page that is one large image is a scan whether
    // or not it has a stamped number on it. Both verdicts send the page to
    // the same reader, so the order changes only the reason, and the reason
    // is what a caller sorts by (`fdoc triage`, `doc:unreadPages`, the crop
    // manifest): scans to an OCR engine, a logo on a separator sheet to
    // nothing. Checked the other way round, every image-only scan came out
    // near-blank.
    if s.image_coverage >= IMAGE_DOMINANT_COVERAGE && s.glyphs < MIN_GLYPHS_FOR_TEXT_PAGE {
        return (Route::Vlm(Reason::Scanned), s);
    }
    if s.glyphs < NEAR_BLANK_GLYPHS && !page.images.is_empty() {
        return (Route::Vlm(Reason::NearBlank), s);
    }
    // Gate on glyph *events*, not resolved glyphs: a fully broken font (a
    // Type3 subset with no ToUnicode, say) resolves nothing, so counting only
    // usable glyphs would make the most broken page of all look blank and
    // sail through as a confident empty answer.
    if page.glyphs.len() >= MIN_GLYPHS_FOR_TEXT_PAGE && s.unicode_rate < MIN_UNICODE_RATE {
        return (Route::Vlm(Reason::BrokenText), s);
    }

    // Region routing: a healthy text page carrying sizable raster regions
    // whose pixels look like text or table structure *and which the glyph
    // layer does not already cover*. The deterministic output stands; the VLM
    // reads only the pixels it alone can read.
    //
    // The glyph test is what makes this minimal. Many figures are drawn as an
    // image with their labels overlaid as real glyphs — we already read
    // those, and a VLM pass would re-transcribe content we have. A raster
    // table is the opposite: text above and below, a glyph void where the
    // content sits. Pixel statistics cannot make this distinction (measured:
    // texty-row fractions of helped and unhelped documents overlap
    // completely); glyph coverage can.
    let regions = unread_regions(page, MIN_REGION_FRACTION);
    if !regions.is_empty() {
        return (Route::VlmRegions(regions), s);
    }
    (Route::Deterministic, s)
}

/// What the router makes of a page, as a page keeps it ([`Page::routing`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Routing {
    /// [`decide`]'s verdict, which numbers the routed regions.
    pub route: Route,
    /// The regions it declines, each small one with its number among the
    /// regions an OCR engine reads ([`declined_regions_numbered`]).
    pub declined: Vec<(BBox, Declined, Option<usize>)>,
}

/// What the router makes of a page as it stands.
pub fn routing(page: &Page) -> Routing {
    let route = decide(page).0;
    let declined = declined_regions_numbered(page, &route);
    Routing { route, declined }
}

impl Page {
    /// What the router made of this page as it was drawn, once
    /// [`Page::keep_routing`] has run; until then, of the page as it stands.
    ///
    /// Analysis cleans a page's glyphs (a word drawn twice for bold, a
    /// struck underline), and a caption drawn twice over a capture can leave
    /// it, cleaned, under the handful of glyphs that make a glyph void: routed
    /// again, the page would gain a region, and every region after it a new
    /// number. Crop names, OCR word files and model readings are named on the
    /// page as drawn, so whatever reads a page after its analysis reads this.
    /// [`crate::ocr::inject`] keeps it before it lays a word on the page: an
    /// OCR reading's glyphs count in the page's signals, and could route it
    /// another way.
    pub fn routing(&self) -> std::borrow::Cow<'_, Routing> {
        match &self.routing {
            Some(r) => std::borrow::Cow::Borrowed(r),
            None => std::borrow::Cow::Owned(routing(self)),
        }
    }

    /// Keep what the router makes of this page as it stands, unless a
    /// routing is kept already: the first is of the page as drawn.
    pub fn keep_routing(&mut self) {
        if self.routing.is_none() {
            self.routing = Some(routing(self));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::Page;
    use crate::glyph::Glyph;

    use crate::extract::ImagePlacement;

    fn page_with(glyphs: usize, unresolved: usize, images: Vec<(BBox, bool)>) -> Page {
        let mut v = Vec::new();
        for i in 0..glyphs + unresolved {
            v.push(Glyph {
                text: if i < glyphs {
                    "a".into()
                } else {
                    String::new()
                },
                bbox: Some(BBox {
                    x0: 10.0,
                    y0: 10.0,
                    x1: 15.0,
                    y1: 20.0,
                }),
                page: 0,
                origin: (10.0, 20.0),
                rotation_deg: 0.0,
                font_size: 10.0,
                weight: None,
                italic: false,
                advance: None,
                draw_index: i,
                ocr: None,
            });
        }
        Page {
            index: 0,
            glyphs: v,
            images: images
                .into_iter()
                .map(|(bbox, texty)| ImagePlacement {
                    bbox,
                    texty: Some(texty),
                    ppi: None,
                    bitonal: false,
                    grid: (1000, 1000),
                    key: None,
                })
                .collect(),
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

    fn b(x0: f64, y0: f64, x1: f64, y1: f64) -> BBox {
        BBox { x0, y0, x1, y1 }
    }

    #[test]
    fn a_scan_stored_in_strips_is_one_picture_not_pasted_screens() {
        // A text page (its glyphs in the top corner) carrying a scan in two
        // strips, each under half the page, laid a point's rounding apart:
        // routed a region each, and together the page's content.
        let top = b(40.0, 80.0, 572.0, 420.0);
        let bottom = b(40.0, 421.0, 572.0, 760.0);
        let p = page_with(1500, 0, vec![(top, true), (bottom, true)]);
        assert!(share(&p, &top) < IMAGE_DOMINANT_COVERAGE);
        assert!(matches!(decide(&p).0, Route::VlmRegions(r) if r.len() == 2));
        assert!(!pasted(&p, &top) && !pasted(&p, &bottom));
        // One strip alone, or two an inch apart, are two screens.
        let alone = page_with(1500, 0, vec![(top, true)]);
        assert!(pasted(&alone, &top));
        let apart = b(40.0, 500.0, 572.0, 760.0);
        let p = page_with(1500, 0, vec![(top, true), (apart, true)]);
        assert!(pasted(&p, &top) && pasted(&p, &apart));
        // A region covering half the page or more is never pasted.
        let scan = b(40.0, 80.0, 572.0, 760.0);
        assert!(!pasted(&page_with(1500, 0, vec![(scan, true)]), &scan));
    }

    #[test]
    fn a_screen_on_a_letterhead_drawn_behind_the_text_is_pasted() {
        // A letterhead image under the whole page, the page's text drawn on
        // it, and a screenshot pasted on both: the letterhead is the page's
        // background, not part of the screen's picture.
        let letterhead = b(0.0, 0.0, 600.0, 800.0);
        let shot = b(60.0, 300.0, 540.0, 400.0);
        let p = page_with(1500, 0, vec![(letterhead, false), (shot, true)]);
        assert!(pasted(&p, &shot));
    }

    /// [`pasted`] as a search from the region, image by image, which
    /// [`pasted_set`] must agree with.
    fn pasted_by_search(page: &Page, b: &BBox) -> bool {
        if share(page, b) >= IMAGE_DOMINANT_COVERAGE {
            return false;
        }
        let text = TextLayer::of(page);
        let mut rest: Vec<BBox> = page.images.iter().map(|p| p.bbox).collect();
        let mut picture = vec![*b];
        let mut i = 0;
        while i < picture.len() {
            let at = picture[i];
            let (near, far): (Vec<BBox>, Vec<BBox>) =
                rest.into_iter().partition(|o| abut(&at, o) && text.void(o));
            picture.extend(near);
            rest = far;
            i += 1;
        }
        union_share(page, &picture) < IMAGE_DOMINANT_COVERAGE
    }

    #[test]
    fn pictures_measured_once_a_page_judge_as_a_search_from_each_region() {
        // Pages of boxes laid at random, some abutting, some apart, some
        // under a cluster of glyphs (not void, and joining no picture), and
        // regions that are not an image's box: each judged as a search from
        // it would.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: f64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 10_000) as f64 / 10_000.0 * n
        };
        let mut pasted_seen = (0, 0);
        for _ in 0..300 {
            let images: Vec<(BBox, bool)> = (0..2 + next(30.0) as usize)
                .map(|_| {
                    let (x, y) = (next(560.0), next(760.0));
                    let (w, h) = (5.0 + next(300.0), 1.0 + next(300.0));
                    (b(x, y, x + w, y + h), true)
                })
                .collect();
            let mut p = page_with(0, 0, images);
            let (gx, gy) = (next(500.0), next(700.0));
            p.glyphs = (0..40)
                .map(|i| Glyph {
                    text: "a".into(),
                    bbox: Some(b(gx + i as f64, gy, gx + i as f64 + 1.0, gy + 2.0)),
                    page: 0,
                    origin: (gx, gy),
                    rotation_deg: 0.0,
                    font_size: 10.0,
                    weight: None,
                    italic: false,
                    advance: None,
                    draw_index: i,
                    ocr: None,
                })
                .collect();
            let mut regions: Vec<BBox> = p.images.iter().map(|i| i.bbox).collect();
            regions.push(b(gx - 5.0, gy - 5.0, gx + 60.0, gy + 30.0));
            regions.push(b(next(500.0), next(700.0), 600.0, 800.0));
            let got = pasted_set(&p, &regions);
            for (r, got) in regions.iter().zip(got) {
                assert_eq!(got, pasted_by_search(&p, r), "{r:?} on {:?}", p.images);
                if got {
                    pasted_seen.0 += 1;
                } else {
                    pasted_seen.1 += 1;
                }
            }
        }
        assert!(
            pasted_seen.0 > 100 && pasted_seen.1 > 100,
            "{pasted_seen:?}"
        );
    }

    #[test]
    fn a_capture_stored_in_thousands_of_strips_is_judged_in_near_linear_time() {
        // A 300 × 260 pt capture stored in strips one pixel row high, each a
        // region of its own: every strip is part of one picture, measured
        // once for all of them.
        let time = |n: usize| {
            let h = 260.0 / n as f64;
            let strips: Vec<(BBox, bool)> = (0..n)
                .map(|i| {
                    let y = 300.0 + i as f64 * h;
                    (b(150.0, y, 450.0, y + h), true)
                })
                .collect();
            let p = page_with(1, 0, strips);
            let regions: Vec<BBox> = p.images.iter().map(|i| i.bbox).collect();
            let t = std::time::Instant::now();
            let pasted = pasted_set(&p, &regions);
            let took = t.elapsed().as_secs_f64();
            assert!(
                pasted.iter().all(|p| *p),
                "a capture of a sixth of the page"
            );
            took
        };
        let (small, large) = (time(1_000), time(4_000));
        assert!(
            large < 8.0 * small.max(0.01),
            "1k strips {small:.3}s, 4k {large:.3}s"
        );
        assert!(large < 1.0, "4k strips {large:.3}s");
    }

    #[test]
    fn small_regions_are_read_by_ocr_after_the_routed_ones() {
        // A text page with a capture over a tenth of the page, and one a
        // date's height high: the model reads the first, the engine both,
        // numbered so `r0` is the same region for either.
        let big = b(50.0, 300.0, 550.0, 500.0);
        let small = b(50.0, 100.0, 250.0, 130.0);
        let p = page_with(1500, 0, vec![(small, true), (big, true)]);
        assert_eq!(decide(&p).0, Route::VlmRegions(vec![big]));
        assert_eq!(ocr_regions(&p), vec![big, small]);

        // Alone, the small one routes nothing, and the engine still reads it.
        let p = page_with(1500, 0, vec![(small, true)]);
        assert_eq!(decide(&p).0, Route::Deterministic);
        assert_eq!(ocr_regions(&p), vec![small]);
    }

    #[test]
    fn a_page_read_whole_declines_no_region() {
        // A scan with a stamp pasted on it: the stamp goes with the page.
        let p = page_with(
            0,
            0,
            vec![
                (b(0.0, 0.0, 600.0, 800.0), false),
                (b(50.0, 100.0, 150.0, 130.0), true),
            ],
        );
        assert!(matches!(decide(&p).0, Route::Vlm(_)));
        assert!(declined_regions(&p).is_empty());
        // The same stamp on a page of prose is declined as small.
        let p = page_with(1500, 0, vec![(b(50.0, 100.0, 150.0, 130.0), true)]);
        assert_eq!(
            declined_regions(&p),
            [(b(50.0, 100.0, 150.0, 130.0), Declined::Small)]
        );
    }

    #[test]
    fn a_checkbox_is_not_declined_but_keeps_its_ocr_number() {
        // Two checkboxes a stencil draws (texty whatever they hold), then a
        // capture a date's height high, then a routed capture. The boxes are
        // under the side worth listing; an engine is still offered them, so
        // the small capture's number counts them.
        let boxes = [b(50.0, 100.0, 60.0, 110.0), b(50.0, 120.0, 60.0, 130.0)];
        let small = b(100.0, 100.0, 300.0, 130.0);
        let big = b(50.0, 300.0, 550.0, 500.0);
        let p = page_with(
            1500,
            0,
            vec![
                (boxes[0], true),
                (boxes[1], true),
                (small, true),
                (big, true),
            ],
        );
        let route = decide(&p).0;
        assert_eq!(
            ocr_regions_for(&p, &route),
            vec![big, boxes[0], boxes[1], small]
        );
        assert_eq!(
            declined_regions_numbered(&p, &route),
            [(small, Declined::Small, Some(3))]
        );
        // A form with nothing but its boxes declines nothing.
        let p = page_with(1500, 0, vec![(boxes[0], true), (boxes[1], true)]);
        assert!(declined_regions(&p).is_empty());
        assert_eq!(ocr_regions(&p), boxes);
    }

    #[test]
    fn an_image_the_probe_does_not_take_for_text_is_listed_and_read_by_nothing() {
        // A capture a date's height high, a large image the probe does not
        // take for text (a photograph), and a routed capture. A model reads
        // only the last; an engine the first and the last, the small one
        // numbered after the routed one. The photograph is listed with no
        // number: nothing reads it.
        let small = b(50.0, 100.0, 250.0, 130.0);
        let photo = b(50.0, 150.0, 550.0, 290.0);
        let big = b(50.0, 300.0, 550.0, 500.0);
        let p = page_with(1500, 0, vec![(small, true), (photo, false), (big, true)]);
        let route = decide(&p).0;
        assert_eq!(route, Route::VlmRegions(vec![big]));
        assert_eq!(ocr_regions_for(&p, &route), vec![big, small]);
        assert_eq!(
            declined_regions_numbered(&p, &route),
            [
                (small, Declined::Small, Some(1)),
                (photo, Declined::NotTexty, None)
            ]
        );
        // Before a small capture, it takes no number from it.
        let p = page_with(1500, 0, vec![(photo, false), (small, true)]);
        let route = decide(&p).0;
        assert_eq!(ocr_regions_for(&p, &route), vec![small]);
        assert_eq!(
            declined_regions_numbered(&p, &route),
            [
                (photo, Declined::NotTexty, None),
                (small, Declined::Small, Some(0))
            ]
        );
        // Under text the page's own glyphs cover (they sit at 10-15 ×
        // 10-20), nothing is offered: the text layer reads it.
        let covered = b(0.0, 0.0, 300.0, 300.0);
        let p = page_with(1500, 0, vec![(covered, false)]);
        assert!(ocr_regions(&p).is_empty());
        assert!(declined_regions(&p).is_empty());
        // A small photograph is offered to nothing, and listed nowhere.
        let p = page_with(1500, 0, vec![(small, false)]);
        assert!(ocr_regions(&p).is_empty());
        assert!(declined_regions(&p).is_empty());
    }

    #[test]
    fn the_text_layer_grid_finds_what_a_walk_of_every_glyph_finds() {
        // Glyphs strewn over the page and past its edges, some blank, a few
        // more on a cell boundary; boxes of every size, some reaching off
        // the page. The grid must answer as counting every glyph does.
        let mut p = page_with(0, 0, Vec::new());
        let mut seed = 7u64;
        let mut next = |m: f64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 11) as f64 / (1u64 << 53) as f64 * m
        };
        for i in 0..4000 {
            let (x, y) = (next(700.0) - 50.0, next(900.0) - 50.0);
            let mut g = page_with(1, 0, Vec::new()).glyphs.remove(0);
            g.text = if i % 9 == 0 { " ".into() } else { "a".into() };
            g.bbox = Some(b(x, y, x + 4.0, y + 6.0));
            g.draw_index = i;
            p.glyphs.push(g);
        }
        let cell = 600.0 / TEXT_LAYER_BINS as f64;
        for k in 0..20 {
            let mut g = p.glyphs[0].clone();
            g.text = "b".into();
            g.bbox = Some(b(
                cell * 3.0 - 2.0,
                100.0 + k as f64,
                cell * 3.0 + 2.0,
                102.0 + k as f64,
            ));
            p.glyphs.push(g);
        }
        let walk = |b: &BBox| {
            p.glyphs
                .iter()
                .filter(|g| {
                    let gb = g.bbox.unwrap();
                    !g.text.trim().is_empty()
                        && b.contains((gb.x0 + gb.x1) * 0.5, (gb.y0 + gb.y1) * 0.5)
                })
                .count()
                < MAX_GLYPHS_IN_UNREAD_REGION
        };
        let text = TextLayer::of(&p);
        let mut both = [0, 0];
        for _ in 0..3000 {
            let (x, y) = (next(760.0) - 80.0, next(960.0) - 80.0);
            let (w, h) = (next(120.0), next(120.0));
            let bx = b(x, y, x + w, y + h);
            assert_eq!(text.void(&bx), walk(&bx), "{bx:?}");
            both[usize::from(walk(&bx))] += 1;
        }
        // On the boundary column: the 20 stacked glyphs fill it.
        let edge = b(cell * 3.0, 90.0, cell * 3.0 + 1.0, 130.0);
        assert_eq!(text.void(&edge), walk(&edge));
        assert!(both[0] > 100 && both[1] > 100, "{both:?}");
    }

    #[test]
    fn a_page_read_whole_has_no_ocr_regions() {
        let p = page_with(5, 0, vec![(b(0.0, 0.0, 600.0, 800.0), true)]);
        assert!(ocr_regions(&p).is_empty());
    }

    #[test]
    fn a_small_image_without_text_or_under_glyphs_is_not_read() {
        // A photograph (not texty), and a small region the text layer
        // already covers (its glyphs sit at 10-15 × 10-20).
        let photo = b(50.0, 100.0, 150.0, 130.0);
        let covered = b(0.0, 0.0, 30.0, 30.0);
        let p = page_with(1500, 0, vec![(photo, false), (covered, true)]);
        assert!(ocr_regions(&p).is_empty());
    }

    #[test]
    fn a_full_page_scan_routes_to_the_vlm() {
        // One full-page image and no glyph at all: a scan, not a near-blank
        // page. The two go to the same reader; only the reason differs, and
        // callers sort scans from blank sheets by it.
        let full = b(0.0, 0.0, 600.0, 800.0);
        let p = page_with(0, 0, vec![(full, true)]);
        assert_eq!(decide(&p).0, Route::Vlm(Reason::Scanned));
        // A stamped page number on it changes nothing.
        let p = page_with(5, 0, vec![(full, true)]);
        assert_eq!(decide(&p).0, Route::Vlm(Reason::Scanned));
        // With a thin caption layer it is a scan too.
        let p = page_with(
            60,
            0,
            vec![(
                BBox {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 600.0,
                    y1: 800.0,
                },
                true,
            )],
        );
        assert!(matches!(decide(&p).0, Route::Vlm(Reason::Scanned)));
    }

    #[test]
    fn a_small_image_on_an_empty_page_is_near_blank() {
        // A logo on a separator sheet: well under half the page, no text.
        let logo = b(250.0, 350.0, 350.0, 450.0);
        let p = page_with(0, 0, vec![(logo, false)]);
        assert_eq!(decide(&p).0, Route::Vlm(Reason::NearBlank));
        // Just under half the page is still not a scan; just over is.
        let under = b(0.0, 0.0, 600.0, 375.0);
        let p = page_with(0, 0, vec![(under, true)]);
        assert!(signals(&p).image_coverage < IMAGE_DOMINANT_COVERAGE);
        assert_eq!(decide(&p).0, Route::Vlm(Reason::NearBlank));
        let over = b(0.0, 0.0, 600.0, 425.0);
        let p = page_with(0, 0, vec![(over, true)]);
        assert!(signals(&p).image_coverage >= IMAGE_DOMINANT_COVERAGE);
        assert_eq!(decide(&p).0, Route::Vlm(Reason::Scanned));
    }

    #[test]
    fn a_photograph_in_a_text_page_stays_deterministic() {
        // Third of a page of image, plenty of text — but the pixels read as a
        // photograph, so there is nothing there the VLM can transcribe.
        let p = page_with(
            1500,
            0,
            vec![(
                BBox {
                    x0: 50.0,
                    y0: 50.0,
                    x1: 550.0,
                    y1: 300.0,
                },
                false,
            )],
        );
        assert!(matches!(decide(&p).0, Route::Deterministic));
    }

    #[test]
    fn a_texty_raster_region_the_glyphs_cannot_read_routes() {
        // Same geometry, but the pixels carry text structure and the glyph
        // layer has a void there: a raster table.
        let p = page_with(
            1500,
            0,
            vec![(
                BBox {
                    x0: 50.0,
                    y0: 50.0,
                    x1: 550.0,
                    y1: 300.0,
                },
                true,
            )],
        );
        assert!(matches!(decide(&p).0, Route::VlmRegions(_)));
    }

    #[test]
    fn broken_cid_text_routes_to_the_vlm() {
        let p = page_with(300, 300, Vec::new());
        assert!(matches!(decide(&p).0, Route::Vlm(Reason::BrokenText)));
    }

    #[test]
    fn fully_unresolved_text_routes_to_the_vlm() {
        // Every glyph event unresolved and no images: the page is full of ink
        // the text layer cannot read. Silent empty output is the one wrong
        // answer.
        let p = page_with(0, 300, Vec::new());
        assert!(matches!(decide(&p).0, Route::Vlm(Reason::BrokenText)));
    }

    #[test]
    fn an_ordinary_text_page_stays_deterministic() {
        let p = page_with(1500, 10, Vec::new());
        assert!(matches!(decide(&p).0, Route::Deterministic));
    }

    #[test]
    fn a_vector_only_drawing_stays_deterministic() {
        // CAD: no images, few glyphs — the VLM loses dimension text (§9.4);
        // sparse vector pages must not route.
        let p = page_with(40, 0, Vec::new());
        assert!(matches!(decide(&p).0, Route::Deterministic));
    }
}
