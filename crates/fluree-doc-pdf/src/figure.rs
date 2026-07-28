//! Chart and diagram regions, from the shapes a page draws.
//!
//! A chart's text extracts perfectly and still means the wrong thing. J&J's
//! employee donut prints four regions and four percentages; the reader
//! recovers all eight glyph runs and emits
//!
//! ```text
//! 20.0%  34.5%  Latin America  North America  17.5%  28.0%  EMEA  Asia Pacific
//! ```
//!
//! Paired in reading order that makes Latin America and North America right
//! and the other two backwards, with nothing to say which is which. The
//! association lives in wedge geometry and colour, not in position, so no
//! amount of care with the text stream recovers it. Marking the region is
//! what lets a consumer decline to read it as prose — and lets the
//! escalation tier send the one crop where a model can see the wedges.
//!
//! The signal is the drawing itself. A table's fills are row bands: they
//! share an x extent and align to the ruling lines around them. A chart's
//! are wedges, bars or segments at assorted positions inside a compact box.
//! Measured over 405 pages of four documents, that separation flags 12 pages,
//! and none at all across 66 pages of dense financial tables.

use crate::geom::BBox;
use crate::rule::{Fill, Rule};

/// Fewest fills that can describe a chart. Two shapes are a rule and its
/// shadow, or a header band and a highlight; three is the smallest pie worth
/// drawing.
const MIN_FILLS: usize = 3;

/// Fraction of fills that may share one x extent before the cluster reads as
/// row banding rather than a drawing. Zebra striping and header shading run
/// the full width of their table, so they agree almost perfectly; wedges and
/// bars do not.
const MAX_SHARED_X: f64 = 0.6;

/// Largest a figure may be, as a fraction of the page. Beyond this the fills
/// are page furniture — a full-bleed background or a cover panel — not a
/// chart sitting in a column of text.
const MAX_PAGE_FRACTION: f64 = 0.55;

/// Smallest a drawing may be, on its narrower side and by area, in points.
///
/// Set typography draws with fills too. A page of equations puts a filled
/// rectangle under every fraction and a path around every radical, and those
/// cluster exactly as a chart's shapes do — 13 "figures" on one page of a
/// fluid-mechanics paper, each wrapping an equation. What separates them is
/// scale, not shape: a fraction bar is 40x8 and a radical 14x13, while the
/// smallest real chart measured here is 92x89. Rejecting below half an inch
/// on the short side also gives up thin timeline graphics, which is the
/// right trade — a timeline reads correctly in sequence, and an equation
/// wrapped as a figure does not read at all.
const MIN_SIDE: f64 = 24.0;
const MIN_AREA: f64 = 2000.0;

/// A region of the page that draws a chart or diagram.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Figure {
    pub bbox: BBox,
    pub page: usize,
    /// Fills the region was inferred from, for the evidence trail.
    pub shapes: usize,
}

/// Group fills that touch or nearly touch into connected clusters, where
/// `reach` is how far apart two shapes may sit and still belong together.
fn cluster(boxes: &[BBox], reach: f64) -> Vec<Vec<usize>> {
    let n = boxes.len();
    let mut seen = vec![false; n];
    let mut out = Vec::new();
    for i in 0..n {
        if seen[i] {
            continue;
        }
        let mut group = vec![i];
        seen[i] = true;
        let mut k = 0;
        while k < group.len() {
            let a = boxes[group[k]];
            for (j, b) in boxes.iter().enumerate() {
                if seen[j] {
                    continue;
                }
                let dx = (b.x0 - a.x1).max(a.x0 - b.x1).max(0.0);
                let dy = (b.y0 - a.y1).max(a.y0 - b.y1).max(0.0);
                if dx <= reach && dy <= reach {
                    seen[j] = true;
                    group.push(j);
                }
            }
            k += 1;
        }
        out.push(group);
    }
    out
}

fn hull(boxes: &[BBox], idx: &[usize]) -> BBox {
    let mut b = boxes[idx[0]];
    for &i in &idx[1..] {
        let o = boxes[i];
        b.x0 = b.x0.min(o.x0);
        b.y0 = b.y0.min(o.y0);
        b.x1 = b.x1.max(o.x1);
        b.y1 = b.y1.max(o.y1);
    }
    b
}

/// Fraction of a cluster's width a rule must cover to be that cluster's own
/// structure rather than a line passing nearby.
const RULE_SPAN_FRACTION: f64 = 0.8;

/// How far outside a cluster a rule may sit and still bound it, in points.
const RULE_NEAR: f64 = 6.0;

/// How close two fills must be to belong to one drawing, relative to the
/// shapes themselves. A chart spaces its parts in proportion to their size:
/// a pie's wedges share an edge, and a bar chart leaves roughly a bar's width
/// between columns. A fixed distance cannot serve both — at 24pt the two
/// lower charts of J&J p40 split into pairs, because their bars are 31pt wide
/// and stand 30pt apart, and each pair fell under the three-shape minimum
/// while the chart above survived only because a connector line happened to
/// bridge its bars.
const CLUSTER_REACH_FACTOR: f64 = 1.2;

/// Bounds on that reach. The floor keeps thin shapes from fragmenting; the
/// ceiling keeps one oversized fill from swallowing the page.
const CLUSTER_REACH_MIN: f64 = 24.0;
const CLUSTER_REACH_MAX: f64 = 80.0;

/// Reach for one page's shapes: proportional to the largest shape's narrower
/// side, which is a bar's width or a wedge's thickness.
fn cluster_reach(boxes: &[BBox]) -> f64 {
    let widest = boxes
        .iter()
        .map(|b| b.width().min(b.height()))
        .fold(0.0f64, f64::max);
    (widest * CLUSTER_REACH_FACTOR).clamp(CLUSTER_REACH_MIN, CLUSTER_REACH_MAX)
}

/// Figures drawn on one page.
///
/// `rules` are consulted only to reject: a cluster whose fills sit on the
/// ruling lines of a table is that table's shading, however irregular the
/// fills look on their own.
pub fn detect(fills: &[Fill], rules: &[Rule], page: usize, size: (f64, f64)) -> Vec<Figure> {
    if fills.len() < MIN_FILLS {
        return Vec::new();
    }
    // Identical fills are drawn twice by many producers (fill then stroke).
    let mut boxes: Vec<BBox> = Vec::new();
    for f in fills {
        let b = f.bbox;
        if !boxes.iter().any(|o| {
            (o.x0 - b.x0).abs() < 0.5
                && (o.y0 - b.y0).abs() < 0.5
                && (o.x1 - b.x1).abs() < 0.5
                && (o.y1 - b.y1).abs() < 0.5
        }) {
            boxes.push(b);
        }
    }
    if boxes.len() < MIN_FILLS {
        return Vec::new();
    }

    let (pw, ph) = size;
    let mut out = Vec::new();
    for group in cluster(&boxes, cluster_reach(&boxes)) {
        if group.len() < MIN_FILLS {
            continue;
        }
        let b = hull(&boxes, &group);
        if b.width().min(b.height()) < MIN_SIDE || b.width() * b.height() < MIN_AREA {
            continue;
        }
        if pw > 0.0
            && ph > 0.0
            && (b.width() / pw > MAX_PAGE_FRACTION)
            && (b.height() / ph > MAX_PAGE_FRACTION)
        {
            continue;
        }
        // Row banding: most fills starting and ending at the same x.
        let mut shared = 0usize;
        for &i in &group {
            let a = boxes[i];
            let n = group
                .iter()
                .filter(|&&j| (boxes[j].x0 - a.x0).abs() < 1.0 && (boxes[j].x1 - a.x1).abs() < 1.0)
                .count();
            shared = shared.max(n);
        }
        if shared as f64 / group.len() as f64 > MAX_SHARED_X {
            continue;
        }
        // A ruled table's shading is bounded by its rules rather than crossed
        // by them, so the test is proximity, not intersection: a band from
        // y=100 to y=112 sits between rules at 98 and 114 and touches
        // neither. Two such rules spanning the cluster's width make it a
        // table's shading. One is an axis, which is what a chart draws.
        let spanning = rules
            .iter()
            .filter(|r| {
                let rb = r.bbox;
                let covers = rb.x1.min(b.x1) - rb.x0.max(b.x0) >= b.width() * RULE_SPAN_FRACTION;
                let near = rb.y1 >= b.y0 - RULE_NEAR && rb.y0 <= b.y1 + RULE_NEAR;
                covers && near
            })
            .count();
        if spanning >= 2 {
            continue;
        }
        out.push(Figure {
            bbox: b,
            page,
            shapes: group.len(),
        });
    }
    out
}

/// How far outside the drawing a label may sit and still belong to it, as a
/// fraction of the drawing's smaller dimension. A donut sets its labels a
/// little clear of the ring; a bar chart sets its tick labels just under the
/// axis and its value labels just over the bars.
const LABEL_REACH: f64 = 0.5;

/// Widest a block may be, as a fraction of the page, to be read as a label
/// rather than as prose the figure happens to sit beside. Chart labels are
/// short by construction — a region name, a value, an axis tick.
const LABEL_MAX_WIDTH: f64 = 0.45;

/// Grow each figure to cover the labels drawn around it.
///
/// Detection finds the drawing, and a drawing does not contain its own
/// labels: J&J's donut occupies x 238-356 while `Latin America` sits at 185.
/// Taking the wedges alone would mark a figure with nothing in it and leave
/// every label loose in the prose — the exact failure this is meant to end.
///
/// Growth is iterative because labels chain: a value sits above its bar and
/// the tick label below it, each reachable only once the other is claimed.
/// Only short blocks are absorbed, so a paragraph beside the chart stays
/// prose however close it is set.
pub fn attach(figures: &mut [Figure], blocks: &[(BBox, usize)], page_width: f64) {
    let max_w = page_width * LABEL_MAX_WIDTH;
    for f in figures.iter_mut() {
        loop {
            let reach = f.bbox.width().min(f.bbox.height()) * LABEL_REACH;
            let mut grew = false;
            for (b, _) in blocks {
                if b.width() > max_w {
                    continue;
                }
                let dx = (b.x0 - f.bbox.x1).max(f.bbox.x0 - b.x1).max(0.0);
                let dy = (b.y0 - f.bbox.y1).max(f.bbox.y0 - b.y1).max(0.0);
                if dx > reach || dy > reach {
                    continue;
                }
                let (x0, y0, x1, y1) = (f.bbox.x0, f.bbox.y0, f.bbox.x1, f.bbox.y1);
                f.bbox.x0 = x0.min(b.x0);
                f.bbox.y0 = y0.min(b.y0);
                f.bbox.x1 = x1.max(b.x1);
                f.bbox.y1 = y1.max(b.y1);
                if f.bbox.x0 < x0 || f.bbox.y0 < y0 || f.bbox.x1 > x1 || f.bbox.y1 > y1 {
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::Orientation;

    fn bb(x0: f64, y0: f64, x1: f64, y1: f64) -> BBox {
        BBox { x0, y0, x1, y1 }
    }

    fn fills(v: &[BBox]) -> Vec<Fill> {
        v.iter().map(|b| Fill { bbox: *b, page: 0 }).collect()
    }

    #[test]
    fn wedges_around_a_centre_are_a_figure() {
        // J&J p17: four donut wedges in a 118pt box, no rules.
        let f = fills(&[
            bb(297.0, 285.0, 356.0, 377.0),
            bb(290.0, 361.0, 346.0, 403.0),
            bb(238.0, 326.0, 293.0, 403.0),
            bb(241.0, 285.0, 297.0, 335.0),
        ]);
        let got = detect(&f, &[], 17, (612.0, 792.0));
        assert_eq!(got.len(), 1, "the four wedges are one figure");
        assert_eq!(got[0].shapes, 4);
    }

    #[test]
    fn row_bands_of_equal_width_are_not_a_figure() {
        // Zebra striping: same x extent on every band.
        let f = fills(&[
            bb(45.0, 100.0, 550.0, 112.0),
            bb(45.0, 124.0, 550.0, 136.0),
            bb(45.0, 148.0, 550.0, 160.0),
            bb(45.0, 172.0, 550.0, 184.0),
        ]);
        assert!(detect(&f, &[], 3, (612.0, 792.0)).is_empty());
    }

    #[test]
    fn shading_on_a_ruled_grid_stays_with_its_table() {
        let f = fills(&[
            bb(45.0, 100.0, 200.0, 112.0),
            bb(210.0, 100.0, 330.0, 112.0),
            bb(340.0, 100.0, 550.0, 112.0),
        ]);
        let rules: Vec<Rule> = [98.0, 114.0, 130.0]
            .iter()
            .map(|&y| Rule {
                bbox: bb(45.0, y, 550.0, y + 0.5),
                orientation: Orientation::Horizontal,
                page: 0,
            })
            .collect();
        assert!(detect(&f, &rules, 0, (612.0, 792.0)).is_empty());
    }

    #[test]
    fn set_mathematics_is_not_a_figure() {
        // A fluid-mechanics page draws a filled bar under every fraction and
        // a path around every radical; they cluster like a chart's shapes and
        // are an order of magnitude smaller.
        let f = fills(&[
            bb(57.0, 100.0, 97.0, 108.0),
            bb(60.0, 101.0, 70.0, 109.0),
            bb(80.0, 100.0, 92.0, 107.0),
            bb(125.0, 98.0, 139.0, 112.0),
        ]);
        assert!(detect(&f, &[], 0, (612.0, 792.0)).is_empty());
    }

    #[test]
    fn two_shapes_are_not_enough() {
        let f = fills(&[bb(10.0, 10.0, 40.0, 40.0), bb(50.0, 10.0, 80.0, 40.0)]);
        assert!(detect(&f, &[], 0, (612.0, 792.0)).is_empty());
    }

    #[test]
    fn distant_drawings_are_separate_figures() {
        let f = fills(&[
            bb(60.0, 100.0, 90.0, 200.0),
            bb(100.0, 130.0, 130.0, 200.0),
            bb(140.0, 90.0, 170.0, 200.0),
            bb(400.0, 100.0, 430.0, 200.0),
            bb(440.0, 140.0, 470.0, 200.0),
            bb(480.0, 80.0, 510.0, 200.0),
        ]);
        assert_eq!(detect(&f, &[], 0, (612.0, 792.0)).len(), 2);
    }
}
