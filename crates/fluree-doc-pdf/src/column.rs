//! Column segmentation by vertical whitespace projection.
//!
//! Must run **before** line assembly. The line pass groups glyphs sharing a
//! baseline, and in a two-column layout the left and right columns share
//! baselines — so it concatenates them:
//!
//! ```text
//! "that integrates a low-resistance, high-side N-channel – TPS5430: 5.5V to 36V"
//!  └───────────── left column ─────────────┘ └──── right column ────┘
//! ```
//!
//! Raising the line-level block-gap threshold cannot fix this: that constant
//! was set from the corpus gap distribution and sits in an empty band, so
//! lowering it to catch a narrow gutter would split ordinary wide word spacing
//! everywhere. A gutter is not distinguished by being *wide* but by being
//! **empty down the page** — that is what this module measures.

#![allow(clippy::needless_range_loop)] // 2-D grid walks read clearer indexed

use crate::glyph::Glyph;

/// Width of one x-bin, in PDF units. Fine enough to locate a gutter edge
/// precisely, coarse enough to keep the projection cheap.
const BIN: f64 = 2.0;

/// A gutter must be empty across at least this fraction of the text rows it
/// spans. Not 100%: a heading or figure legitimately straddles the gutter, and
/// one such row must not veto an otherwise clean column break.
const MIN_EMPTY_ROW_FRACTION: f64 = 0.90;

/// A gutter may instead qualify by running *unbroken* across this fraction of
/// the page's text rows.
///
/// The plain emptiness test above cannot see a two-column paper with a large
/// full-width header: a title, five author lines and an affiliation set
/// across the full width leave the gutter blank in only ~85% of rows, so the
/// page reads as single-column — concatenating the two columns line by line
/// ("retaining the simplic-natural language processing (NLP)").
/// Lowering the fraction to 0.80 does find it, but also invents gutters in
/// single-column text and costs more elsewhere than it gains.
///
/// Contiguity is the property that actually distinguishes them. A real gutter
/// runs from where the columns begin to the foot of the page without a break;
/// a chance alignment of word spacing in single-column prose does not survive
/// many consecutive rows. So a run is measured, not a total.
const MIN_GUTTER_RUN_FRACTION: f64 = 0.75;

/// Minimum gutter width as a fraction of the page's text width. Below this it
/// is word spacing or a table cell boundary, not a column break.
const MIN_GUTTER_FRACTION: f64 = 0.012;

/// A page must have at least this many text rows before column detection is
/// attempted; on a sparse page any vertical band looks empty.
const MIN_ROWS: usize = 8;

/// Minimum width of a resulting column, as a fraction of the page's text width.
///
/// Without this, the whitespace between a bullet strip and its text reads as a
/// gutter down the whole page and every bullet becomes its own "column" — the
/// first thing that happened on a bulleted datasheet page. A column narrow
/// enough to hold only list markers is not a column; it is an indent.
const MIN_COLUMN_FRACTION: f64 = 0.15;

/// Row height for the occupancy grid, as a multiple of median font size.
const ROW_FACTOR: f64 = 1.0;

/// An x-range holding one column's content.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Column {
    pub x0: f64,
    pub x1: f64,
}

impl Column {
    fn contains(&self, x: f64) -> bool {
        x >= self.x0 && x < self.x1
    }
}

/// The page's ink, projected onto x-bins and y-rows.
///
/// Built once and read by both the segmentation and the doubt signal, so the
/// two can never disagree about what the page looks like.
struct Projection {
    /// `occupied[bin][row]`.
    occupied: Vec<Vec<bool>>,
    /// Indices of rows holding any ink, in order.
    live: Vec<usize>,
    n_bins: usize,
    min_x: f64,
    max_x: f64,
    width: f64,
}

impl Projection {
    fn build(boxed: &[&Glyph]) -> Option<Projection> {
        if boxed.len() < 20 {
            return None;
        }
        let (min_x, max_x, min_y, max_y) = extent(boxed);
        let width = max_x - min_x;
        if width <= 0.0 {
            return None;
        }

        let mut sizes: Vec<f64> = boxed.iter().map(|g| g.font_size as f64).collect();
        sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let row_h = (sizes[sizes.len() / 2] * ROW_FACTOR).max(1.0);
        let n_rows = (((max_y - min_y) / row_h).ceil() as usize).max(1);
        if n_rows < MIN_ROWS {
            return None;
        }

        let n_bins = ((width / BIN).ceil() as usize).max(1);
        let mut occupied = vec![vec![false; n_rows]; n_bins];
        for g in boxed {
            let b = g.bbox.unwrap();
            let r0 = (((b.y0 - min_y) / row_h).floor().max(0.0)) as usize;
            let r1 = (((b.y1 - min_y) / row_h).floor().max(0.0) as usize).min(n_rows - 1);
            let c0 = (((b.x0 - min_x) / BIN).floor().max(0.0)) as usize;
            let c1 = (((b.x1 - min_x) / BIN).ceil().max(0.0) as usize).min(n_bins);
            for c in c0..c1.max(c0 + 1) {
                if c >= n_bins {
                    break;
                }
                for r in r0..=r1 {
                    occupied[c][r] = true;
                }
            }
        }

        // Rows that hold any ink at all: blank rows must not count toward a
        // gutter's emptiness, or a page with wide vertical spacing looks
        // gutter-rich.
        let live: Vec<usize> = (0..n_rows)
            .filter(|&r| (0..n_bins).any(|c| occupied[c][r]))
            .collect();
        if live.len() < MIN_ROWS {
            return None;
        }
        Some(Projection {
            occupied,
            live,
            n_bins,
            min_x,
            max_x,
            width,
        })
    }

    /// A bin is "clear" when it is empty across nearly all live rows, or when
    /// it is empty across a long unbroken run of them.
    fn clear_bins(&self) -> Vec<bool> {
        let total = self.live.len();
        (0..self.n_bins)
            .map(|c| {
                let filled = self.live.iter().filter(|&&r| self.occupied[c][r]).count();
                if (total - filled) as f64 / total as f64 >= MIN_EMPTY_ROW_FRACTION {
                    return true;
                }
                let (mut run, mut best) = (0usize, 0usize);
                for &r in &self.live {
                    run = if self.occupied[c][r] { 0 } else { run + 1 };
                    best = best.max(run);
                }
                best as f64 / total as f64 >= MIN_GUTTER_RUN_FRACTION
            })
            .collect()
    }

    fn min_gutter_bins(&self) -> usize {
        ((self.width * MIN_GUTTER_FRACTION) / BIN).ceil().max(1.0) as usize
    }

    /// Interior runs of clear bins are gutters. Runs touching either edge are
    /// margins, not gutters.
    fn gutters(&self, clear: &[bool]) -> Vec<Gutter> {
        let min_bins = self.min_gutter_bins();
        let mut out = Vec::new();
        let mut run_start: Option<usize> = None;
        for c in 0..=self.n_bins {
            let is_clear = c < self.n_bins && clear[c];
            match (is_clear, run_start) {
                (true, None) => run_start = Some(c),
                (false, Some(s)) => {
                    let len = c - s;
                    if s > 0 && c < self.n_bins && len >= min_bins {
                        out.push(Gutter {
                            // Cut at the gutter's midpoint.
                            cut: self.min_x + (s + len / 2) as f64 * BIN,
                            bins: (s, c - 1),
                        });
                    }
                    run_start = None;
                }
                _ => {}
            }
        }
        out
    }
}

/// One gutter: where to cut, and which bins it spans.
struct Gutter {
    cut: f64,
    bins: (usize, usize),
}

impl Gutter {
    /// The longest unbroken run of live rows this gutter is clear through.
    ///
    /// This is the band over which the page is genuinely in columns. A gutter
    /// that runs the page's whole height returns every row, which is what
    /// makes the margins beside the text harmless to the caller below.
    fn band(&self, p: &Projection) -> Vec<usize> {
        let (s, e) = self.bins;
        let (mut start, mut run) = (0usize, 0usize);
        let mut best: Option<(usize, usize)> = None;
        for (i, &r) in p.live.iter().enumerate() {
            if (s..=e).any(|c| p.occupied[c][r]) {
                run = 0;
                start = i + 1;
            } else {
                run += 1;
                if best.is_none_or(|(b0, b1)| run > b1 - b0 + 1) {
                    best = Some((start, i));
                }
            }
        }
        best.map_or_else(Vec::new, |(b0, b1)| p.live[b0..=b1].to_vec())
    }
}

/// A page whose columns this segmentation cannot represent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Doubt {
    /// Columns the page-global pass found.
    pub found: usize,
    /// Further gutters visible only within an accepted gutter's own band.
    pub missed: usize,
    /// Share of the page's live rows the largest such band covers.
    pub band: f64,
}

/// Does this page hold columns the segmentation cannot see?
///
/// A page's columns need not run its whole height. A slide sets three panels
/// under a full-width title and paragraph: the gutter between panels two and
/// three clears the emptiness test because a single line overhangs it, and
/// the gutter between one and two does not, because three lines do. One
/// gutter found, one missed, and the two panels it should have separated read
/// across each other — costing both the reading order and, because the panel
/// headings fuse into one full-width line, the heading hierarchy with it.
///
/// Acting on the missed gutter does not work. Splitting there is measurably
/// worse: over the evaluation corpus it changes ten documents, six for the
/// worse and none for the better, because a band short enough to hide a
/// gutter is also short enough for a chart's axis labels to look like one — a
/// page of bar charts gains four "columns" from the spacing between its tick
/// labels. And on the very page that motivates the change, splitting alters
/// nothing: the three panels then read as a table, which is still row-major,
/// so the reading order is identical.
///
/// Reporting it works very well. These are pages whose layout this model
/// cannot represent, whichever way the guess falls, and a reader that sees
/// the whole page has no such trouble: escalating all ten lifts nine of them
/// and lowers none, two by more than 0.33. So the doubt is a routing signal,
/// not a segmentation decision.
pub fn doubt(glyphs: &[Glyph]) -> Option<Doubt> {
    let boxed: Vec<&Glyph> = glyphs
        .iter()
        .filter(|g| g.bbox.is_some() && g.is_horizontal() && !g.text.trim().is_empty())
        .collect();
    let p = Projection::build(&boxed)?;
    let clear = p.clear_bins();
    let found = p.gutters(&clear);
    if found.is_empty() {
        return None;
    }

    let (mut missed, mut band) = (0usize, 0.0f64);
    for g in &found {
        let rows = g.band(&p);
        // A band covering every live row is the whole page, and searching it
        // can find nothing the page-global pass did not.
        if rows.len() < MIN_ROWS || rows.len() == p.live.len() {
            continue;
        }
        let in_band: Vec<bool> = (0..p.n_bins)
            .map(|c| {
                let filled = rows.iter().filter(|&&r| p.occupied[c][r]).count();
                (rows.len() - filled) as f64 / rows.len() as f64 >= MIN_EMPTY_ROW_FRACTION
            })
            .collect();
        for sibling in p.gutters(&in_band) {
            if found
                .iter()
                .all(|f| (f.cut - sibling.cut).abs() > p.width * MIN_GUTTER_FRACTION)
            {
                missed += 1;
                band = band.max(rows.len() as f64 / p.live.len() as f64);
            }
        }
    }
    (missed > 0).then_some(Doubt {
        found: found.len() + 1,
        missed,
        band,
    })
}

/// Fewest segments a rank of rules must carry to be read as a column ruler.
const MIN_RULER_SEGMENTS: usize = 3;

/// Shortest segment, in PDF units. Tick marks, bullet underlines and legend
/// swatches run 8-25pt; a rule under a column heading is much longer.
const MIN_RULER_SEGMENT: f64 = 40.0;

/// Gap between segments, below which they are one rule drawn in pieces
/// rather than a rank with gutters between them.
const MIN_RULER_GAP: f64 = 4.0;

/// Share of a ruler's own span that must be rule rather than gap.
const MIN_RULER_COVERAGE: f64 = 0.70;

/// Share of the page's text width the ruler must span.
///
/// This is the condition that separates a column ruler from an equation. A
/// row of fraction bars is co-linear, evenly spaced and of similar length —
/// every other test passes — but it sits in the middle of the measure, where
/// a heading rule runs the width of the content.
const MIN_RULER_WIDTH: f64 = 0.70;

/// Widest a segment may be relative to its neighbours, and vice versa.
const RULER_LENGTH_RATIO: f64 = 0.5;

/// Most uneven the gaps may be before the rank is decoration, not a grid.
const RULER_GAP_RATIO: f64 = 3.0;

/// A rank of rules drawn under column headings, and the columns it states.
#[derive(Debug, Clone, PartialEq)]
pub struct Ruler {
    /// The rank's own y. The band is the run of rows around it, not
    /// everything below it — a footer crossing a gutter at the page foot
    /// must close the band, not empty it.
    pub axis: f64,
    /// Where to cut, at each gutter's midpoint.
    pub cuts: Vec<f64>,
    /// The gutters themselves, so a caller can find the rows they run clear
    /// through — the band over which these columns actually govern.
    pub gutters: Vec<(f64, f64)>,
}

/// Columns stated by a rank of rules drawn under their headings.
///
/// Decks, brochures and datasheets set panels under a rule per column. The
/// whitespace projection cannot see those panels when they occupy only part
/// of the page — a full-width title above them fills the gutters for enough
/// rows to hide them — but the geometry says exactly where they are, and it
/// is drawn rather than inferred.
///
/// Deliberately unwilling, because the same shape is drawn by things that are
/// not columns. Each test rejects a real page seen in evaluation: short
/// segments are tick marks and legend swatches, uneven gaps are decoration,
/// and a rank that does not span the measure is an equation's fraction bars.
pub fn ruler(rules: &[crate::rule::Rule], page: usize, text: (f64, f64)) -> Option<Ruler> {
    use std::collections::HashMap;
    let text_width = text.1 - text.0;
    if text_width <= 0.0 {
        return None;
    }
    let mut ranks: HashMap<i64, (f64, Vec<(f64, f64)>)> = HashMap::new();
    for r in rules
        .iter()
        .filter(|r| r.page == page && r.orientation == crate::rule::Orientation::Horizontal)
    {
        let e = ranks
            .entry((r.axis_pos() / 2.0).round() as i64)
            .or_insert_with(|| (r.axis_pos(), Vec::new()));
        e.1.push((r.bbox.x0, r.bbox.x1));
    }

    let mut best: Option<Ruler> = None;
    for (_, (axis, mut segs)) in ranks {
        if segs.len() < MIN_RULER_SEGMENTS {
            continue;
        }
        segs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let lens: Vec<f64> = segs.iter().map(|(a, b)| b - a).collect();
        let gaps: Vec<f64> = segs.windows(2).map(|w| w[1].0 - w[0].1).collect();
        let (min_len, max_len) = (
            lens.iter().copied().fold(f64::MAX, f64::min),
            lens.iter().copied().fold(f64::MIN, f64::max),
        );
        let (min_gap, max_gap) = (
            gaps.iter().copied().fold(f64::MAX, f64::min),
            gaps.iter().copied().fold(f64::MIN, f64::max),
        );
        let span = segs[segs.len() - 1].1 - segs[0].0;
        if min_gap <= MIN_RULER_GAP
            || min_len < MIN_RULER_SEGMENT
            || min_len < max_len * RULER_LENGTH_RATIO
            || max_gap > min_gap * RULER_GAP_RATIO
            || lens.iter().sum::<f64>() < span * MIN_RULER_COVERAGE
            || span < text_width * MIN_RULER_WIDTH
        {
            continue;
        }
        // Cut at each gutter's midpoint, as the projection does.
        let found = Ruler {
            axis,
            cuts: segs.windows(2).map(|w| (w[0].1 + w[1].0) * 0.5).collect(),
            gutters: segs.windows(2).map(|w| (w[0].1, w[1].0)).collect(),
        };
        if best
            .as_ref()
            .is_none_or(|b| found.cuts.len() > b.cuts.len())
        {
            best = Some(found);
        }
    }
    best
}

/// Detect column regions on a page. Returns a single full-width column when the
/// page is single-column, which is the common case and costs one projection.
pub fn detect(glyphs: &[Glyph]) -> Vec<Column> {
    detect_with_rules(glyphs, &[], 0)
}

/// As [`detect`], also given the page's drawn rules so a rank of heading
/// rules can state columns the whitespace projection cannot see.
pub fn detect_with_rules(
    glyphs: &[Glyph],
    rules: &[crate::rule::Rule],
    page: usize,
) -> Vec<Column> {
    let boxed: Vec<&Glyph> = glyphs
        .iter()
        .filter(|g| g.bbox.is_some() && g.is_horizontal() && !g.text.trim().is_empty())
        .collect();
    if !boxed.is_empty() {
        let (min_x, max_x, _, _) = extent(&boxed);
        if let Some(r) = ruler(rules, page, (min_x, max_x)) {
            let mut cols = Vec::with_capacity(r.cuts.len() + 1);
            let mut left = min_x;
            for c in r.cuts {
                cols.push(Column { x0: left, x1: c });
                left = c;
            }
            cols.push(Column {
                x0: left,
                x1: max_x + BIN,
            });
            return cols;
        }
    }
    let Some(p) = Projection::build(&boxed) else {
        return full_width(&boxed);
    };
    let (min_x, max_x, width) = (p.min_x, p.max_x, p.width);

    let cuts: Vec<f64> = p.gutters(&p.clear_bins()).iter().map(|g| g.cut).collect();
    if cuts.is_empty() {
        return full_width(&boxed);
    }

    let mut cols = Vec::with_capacity(cuts.len() + 1);
    let mut left = min_x;
    for c in cuts {
        cols.push(Column { x0: left, x1: c });
        left = c;
    }
    cols.push(Column {
        x0: left,
        x1: max_x + BIN,
    });

    // Fold away columns too narrow to be columns. Merging into the *following*
    // region keeps a bullet strip attached to the text it introduces.
    let min_w = width * MIN_COLUMN_FRACTION;
    let mut merged: Vec<Column> = Vec::with_capacity(cols.len());
    for c in cols {
        match merged.last_mut() {
            Some(prev) if prev.x1 - prev.x0 < min_w => prev.x1 = c.x1,
            _ => merged.push(c),
        }
    }
    // A trailing narrow column folds back into its predecessor.
    while merged.len() > 1 {
        let last = *merged.last().unwrap();
        if last.x1 - last.x0 < min_w {
            merged.pop();
            merged.last_mut().unwrap().x1 = last.x1;
        } else {
            break;
        }
    }
    if merged.len() <= 1 {
        return full_width(&boxed);
    }
    merged
}

fn extent(g: &[&Glyph]) -> (f64, f64, f64, f64) {
    let mut r = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for x in g {
        let b = x.bbox.unwrap();
        r.0 = r.0.min(b.x0);
        r.1 = r.1.max(b.x1);
        r.2 = r.2.min(b.y0);
        r.3 = r.3.max(b.y1);
    }
    r
}

fn full_width(g: &[&Glyph]) -> Vec<Column> {
    if g.is_empty() {
        return vec![Column {
            x0: 0.0,
            x1: f64::MAX,
        }];
    }
    let (x0, x1, _, _) = extent(g);
    vec![Column { x0, x1: x1 + BIN }]
}

/// Partition glyphs into columns by the centre of each glyph's box.
///
/// Rotated glyphs and outline-less spaces go to the column their position falls
/// in, so a column's glyph sequence stays complete for line assembly.
pub fn partition(glyphs: &[Glyph], cols: &[Column]) -> Vec<Vec<Glyph>> {
    if cols.len() <= 1 {
        return vec![glyphs.to_vec()];
    }
    let mut out = vec![Vec::new(); cols.len()];
    for g in glyphs {
        let x = match g.bbox {
            Some(b) => (b.x0 + b.x1) * 0.5,
            None => g.origin.0,
        };
        let idx = cols
            .iter()
            .position(|c| c.contains(x))
            .unwrap_or(cols.len() - 1);
        out[idx].push(g.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::BBox;

    fn g(x: f64, y: f64, w: f64) -> Glyph {
        Glyph {
            text: "x".into(),
            bbox: Some(BBox {
                x0: x,
                y0: y,
                x1: x + w,
                y1: y + 10.0,
            }),
            page: 0,
            origin: (x, y + 10.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            advance: None,
            draw_index: 0,
        }
    }

    /// Two columns, 40 rows, a 30pt gutter at x=250.
    fn two_column_page() -> Vec<Glyph> {
        let mut v = Vec::new();
        for row in 0..40 {
            let y = row as f64 * 12.0;
            for i in 0..20 {
                v.push(g(50.0 + i as f64 * 10.0, y, 9.0));
            }
            for i in 0..20 {
                v.push(g(280.0 + i as f64 * 10.0, y, 9.0));
            }
        }
        v
    }

    #[test]
    fn finds_a_two_column_gutter() {
        let cols = detect(&two_column_page());
        assert_eq!(cols.len(), 2, "expected two columns, got {cols:?}");
        assert!(
            cols[0].x1 > 250.0 && cols[0].x1 < 285.0,
            "cut in the gutter: {cols:?}"
        );
    }

    #[test]
    fn partitions_glyphs_into_the_right_columns() {
        let page = two_column_page();
        let cols = detect(&page);
        let parts = partition(&page, &cols);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].iter().all(|g| g.bbox.unwrap().x0 < 250.0));
        assert!(parts[1].iter().all(|g| g.bbox.unwrap().x0 >= 250.0));
    }

    #[test]
    fn a_bullet_strip_is_not_a_column() {
        // Markers in a narrow band at the left margin, with clear whitespace
        // between them and the text they introduce, down the whole page.
        let mut v = Vec::new();
        for row in 0..40 {
            let y = row as f64 * 12.0;
            v.push(g(50.0, y, 5.0));
            for i in 0..40 {
                v.push(g(90.0 + i as f64 * 10.0, y, 9.0));
            }
        }
        assert_eq!(detect(&v).len(), 1, "an indent is not a column break");
    }

    #[test]
    fn single_column_page_is_left_whole() {
        let mut v = Vec::new();
        for row in 0..40 {
            for i in 0..45 {
                v.push(g(50.0 + i as f64 * 10.0, row as f64 * 12.0, 9.0));
            }
        }
        assert_eq!(detect(&v).len(), 1, "must not invent a gutter");
    }

    #[test]
    fn a_heading_spanning_the_gutter_does_not_veto_it() {
        // Real pages put a full-width title above two columns; one straddling
        // row must not suppress the break.
        let mut v = two_column_page();
        for i in 0..40 {
            v.push(g(50.0 + i as f64 * 10.0, -20.0, 9.0));
        }
        assert_eq!(detect(&v).len(), 2);
    }

    #[test]
    fn a_full_width_header_does_not_hide_the_gutter() {
        // An academic paper: title, authors and affiliation set across the full
        // width, then two columns. The gutter is blank in only ~85% of rows, so
        // the plain emptiness test misses it; the run test sees it because the
        // blank rows are consecutive.
        let mut v = two_column_page();
        for row in 0..8 {
            let y = -20.0 - row as f64 * 12.0;
            for i in 0..40 {
                v.push(g(50.0 + i as f64 * 10.0, y, 9.0));
            }
        }
        let cols = detect(&v);
        assert_eq!(cols.len(), 2, "a header must not mask the gutter: {cols:?}");
    }

    #[test]
    fn sparse_pages_are_left_whole() {
        // Too little text to trust a projection.
        let v: Vec<Glyph> = (0..10).map(|i| g(50.0, i as f64 * 12.0, 9.0)).collect();
        assert_eq!(detect(&v).len(), 1);
    }

    /// The shape this signal exists for: a full-width header, then three
    /// panels occupying only the lower half of the page.
    ///
    /// Most header lines reach over the first gutter but stop short of the
    /// second, and the last one — the longest — crosses both. So the second
    /// gutter is blocked in a single row and clears the emptiness test, while
    /// the first is blocked in eight and clears neither test.
    fn three_panels_under_a_header() -> Vec<Glyph> {
        let mut v = Vec::new();
        for row in 0..7 {
            for i in 0..30 {
                v.push(g(50.0 + i as f64 * 10.0, 100.0 + row as f64 * 12.0, 9.0));
            }
        }
        for i in 0..45 {
            v.push(g(50.0 + i as f64 * 10.0, 184.0, 9.0));
        }
        for row in 0..14 {
            let y = 220.0 + row as f64 * 12.0;
            for x0 in [50.0, 250.0, 460.0] {
                for i in 0..12 {
                    v.push(g(x0 + i as f64 * 10.0, y, 9.0));
                }
            }
        }
        v
    }

    #[test]
    fn a_gutter_the_page_wide_test_cannot_see_is_reported() {
        let v = three_panels_under_a_header();
        // The segmentation misses it...
        assert_eq!(detect(&v).len(), 2, "the page-global pass finds one gutter");
        // ...and the doubt says so.
        let d = doubt(&v).expect("a missed gutter");
        assert_eq!(d.found, 2);
        assert!(d.missed >= 1, "{d:?}");
        assert!(d.band < 1.0, "a band, not the whole page: {d:?}");
    }

    #[test]
    fn a_clean_two_column_page_is_not_doubted() {
        // The gutter runs the full height, so its band is every row and the
        // in-band search can find nothing new.
        assert_eq!(doubt(&two_column_page()), None);
    }

    #[test]
    fn a_single_column_page_is_not_doubted() {
        let mut v = Vec::new();
        for row in 0..40 {
            for i in 0..45 {
                v.push(g(50.0 + i as f64 * 10.0, row as f64 * 12.0, 9.0));
            }
        }
        assert_eq!(doubt(&v), None, "no gutter, nothing to doubt");
    }

    fn hrule(x0: f64, x1: f64, y: f64) -> crate::rule::Rule {
        crate::rule::Rule {
            bbox: BBox {
                x0,
                y0: y,
                x1,
                y1: y + 1.0,
            },
            orientation: crate::rule::Orientation::Horizontal,
            page: 0,
        }
    }

    #[test]
    fn a_rank_of_heading_rules_states_its_columns() {
        // Five rules under five panel headings, spanning the measure.
        let rules: Vec<_> = [
            (48.0, 131.0),
            (145.0, 227.0),
            (241.0, 324.0),
            (338.0, 438.0),
            (452.0, 534.0),
        ]
        .iter()
        .map(|(a, b)| hrule(*a, *b, 309.0))
        .collect();
        let r = ruler(&rules, 0, (48.0, 566.0)).expect("a column ruler");
        assert_eq!(r.cuts.len(), 4, "four gutters between five columns");
        assert_eq!(r.gutters.len(), 4);
        assert!((r.axis - 309.0).abs() < 1.0);
    }

    #[test]
    fn an_equations_fraction_bars_are_not_columns() {
        // Co-linear, evenly spaced, similar length -- and mid-measure, which
        // is the only thing that separates them from a rank of heading rules.
        let rules: Vec<_> = [(270.0, 324.0), (344.0, 408.0), (430.0, 488.0)]
            .iter()
            .map(|(a, b)| hrule(*a, *b, 603.0))
            .collect();
        assert_eq!(ruler(&rules, 0, (57.0, 558.0)), None);
    }

    #[test]
    fn short_marks_are_not_a_ruler() {
        // Legend swatches and bullet underlines run 8-25pt.
        let rules: Vec<_> = [
            (97.0, 105.0),
            (142.0, 150.0),
            (212.0, 220.0),
            (400.0, 408.0),
        ]
        .iter()
        .map(|(a, b)| hrule(*a, *b, 457.0))
        .collect();
        assert_eq!(ruler(&rules, 0, (43.0, 558.0)), None);
    }

    #[test]
    fn one_rule_drawn_in_touching_pieces_is_not_a_ruler() {
        let rules: Vec<_> = [(48.0, 200.0), (200.0, 350.0), (350.0, 534.0)]
            .iter()
            .map(|(a, b)| hrule(*a, *b, 309.0))
            .collect();
        assert_eq!(ruler(&rules, 0, (48.0, 566.0)), None);
    }
}
