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
use crate::glyph::Glyph;
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

/// A fill at least this fraction of its cluster's width wide is a box
/// around text, not a shape in a drawing…
const PANEL_WIDTH: f64 = 0.9;

/// …provided it is a strip: at least this many times wider than tall. A
/// plot's background rectangle spans its cluster too, and is not one.
const STRIP_ASPECT: f64 = 4.0;

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
    /// The drawing and the labels taken around it.
    pub bbox: BBox,
    /// The drawing alone: the shapes' extent, before any label was taken.
    pub drawing: BBox,
    pub page: usize,
    /// Fills the region was inferred from, for the evidence trail: none
    /// for a picture with its labels set over it (see [`pictures`]).
    pub shapes: usize,
    /// The box drawn around the drawing, where one is: a panel the
    /// drawing's shapes lie within, holding its labels.
    pub frame: Option<BBox>,
    /// Found from a trace bound by its axes ([`axis_plots`]). Its labels
    /// stay in the text the body's size is measured from; see `drawn` in
    /// the document pass.
    pub axes: bool,
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
pub fn detect(
    fills: &[Fill],
    rules: &[Rule],
    glyphs: &[Glyph],
    page: usize,
    size: (f64, f64),
) -> Vec<Figure> {
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
    let page_scale = |b: &BBox| {
        pw > 0.0
            && ph > 0.0
            && b.width() / pw > MAX_PAGE_FRACTION
            && b.height() / ph > MAX_PAGE_FRACTION
    };
    // What is not a drawing's shape is set aside before the shapes are
    // grouped, so that it neither joins drawings into one nor dilutes the
    // tests a group is judged by.
    let ink = Ink::new(glyphs);
    // A form's fields are no drawing's shapes: an empty box a line or two
    // high, wide enough to write in, under or after its label's colon
    // (`Job Name:`), and the boxes after it in its row or under it in its
    // column at its height, a form's second line or its right half. A
    // form's box holding them held no drawing, and with a logo set over it
    // the form was taken for one drawing, its labels for the logo's.
    // A form's labels are set at the text's size: a slide's title ending
    // on a colon over its diagram is no label.
    let colons: Vec<BBox> = glyphs
        .iter()
        .filter(|g| g.text.trim() == ":" && g.font_size < ink.prose * LABEL_DISPLAY)
        .filter_map(|g| g.bbox)
        .collect();
    let line = (ink.prose as f64).max(1.0);
    let shaped: Vec<bool> = boxes
        .iter()
        .map(|b| field_shaped(b, line) && ink.on(b).letters == 0)
        .collect();
    let mut fields: Vec<bool> = (0..boxes.len())
        .map(|i| shaped[i] && labelled(&boxes[i], &colons, line))
        .collect();
    loop {
        let found: Vec<BBox> = (0..boxes.len())
            .filter(|&i| fields[i])
            .map(|i| boxes[i])
            .collect();
        let mut grew = false;
        for i in 0..boxes.len() {
            if fields[i] || !shaped[i] {
                continue;
            }
            let b = boxes[i];
            let in_line = found.iter().any(|o| {
                (o.height() - b.height()).abs() <= 1.0
                    && (((o.y0 - b.y0).abs() <= 1.0
                        && (b.x0 - o.x1).max(o.x0 - b.x1) <= line * 2.0)
                        || ((o.x0 - b.x0).abs() <= 1.0
                            && (b.y0 - o.y1).max(o.y0 - b.y1) <= line * 2.0))
            });
            if in_line {
                fields[i] = true;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let mut backgrounds: Vec<bool> = (0..boxes.len())
        .map(|i| fields[i] || page_scale(&boxes[i]) || background(i, &boxes, &fields, &ink))
        .collect();
    // A table's cells go together. A cell set with a figure stands beside
    // a cell set with words, sharing its row's edges, or under it, sharing
    // its column's, or beside or under another such cell: `6,125` in the row
    // `CEBU` heads. An empty cell does too, beside or under a cell of either
    // kind. Left in, a table's numbers and blanks were a drawing. A
    // drawing's parts may line up with a labelled box, and its outlines abut
    // one another; the blanks do not carry the table on from one to the
    // next, so a drawing is not unpicked from one box with a word on it.
    let abuts = |b: &BBox, o: &BBox| {
        let gap_x = (o.x0 - b.x1).max(b.x0 - o.x1);
        let gap_y = (o.y0 - b.y1).max(b.y0 - o.y1);
        let row = (o.y0 - b.y0).abs() <= 1.0
            && (o.y1 - b.y1).abs() <= 1.0
            && (-1.0..=CELL_TOUCH).contains(&gap_x);
        let column = (o.x0 - b.x0).abs() <= 1.0
            && (o.x1 - b.x1).abs() <= 1.0
            && (-1.0..=CELL_TOUCH).contains(&gap_y);
        row || column
    };
    let (mut figured, blank): (Vec<usize>, Vec<usize>) = (0..boxes.len())
        .filter(|&i| !backgrounds[i] && !page_scale(&boxes[i]))
        .partition(|&i| ink.read_on(&ink.on(&boxes[i])));
    loop {
        let before = figured.len();
        figured.retain(|&i| {
            let cell = (0..boxes.len())
                .any(|j| backgrounds[j] && !page_scale(&boxes[j]) && abuts(&boxes[i], &boxes[j]));
            if cell {
                backgrounds[i] = true;
            }
            !cell
        });
        if figured.len() == before {
            break;
        }
    }
    let cells: Vec<BBox> = (0..boxes.len())
        .filter(|&j| backgrounds[j] && !page_scale(&boxes[j]))
        .map(|j| boxes[j])
        .collect();
    for i in blank {
        if cells.iter().any(|o| abuts(&boxes[i], o)) {
            backgrounds[i] = true;
        }
    }
    // A panel's border and accent bars go with it: a strip drawn along one
    // of its edges, as long as that edge.
    let panels: Vec<BBox> = (0..boxes.len())
        .filter(|&i| backgrounds[i] && !page_scale(&boxes[i]))
        .map(|i| boxes[i])
        .collect();
    for (i, b) in boxes.iter().enumerate() {
        if !backgrounds[i] && panels.iter().any(|p| edging(b, p)) {
            backgrounds[i] = true;
        }
    }
    // Any fill set aside may be a drawing's frame, however large: a wiring
    // diagram's box can take most of the page.
    let frames: Vec<BBox> = boxes
        .iter()
        .zip(&backgrounds)
        .filter(|(_, &set_aside)| set_aside)
        .map(|(b, _)| *b)
        .collect();
    let boxes: Vec<BBox> = boxes
        .iter()
        .zip(&backgrounds)
        .filter(|(_, &set_aside)| !set_aside)
        .map(|(b, _)| *b)
        .collect();
    if boxes.len() < MIN_FILLS {
        return Vec::new();
    }

    let text = PageText {
        prose: ink.prose,
        numbers: numbers(glyphs),
    };
    let mut out = Vec::new();
    let fit = |b: &BBox| !page_scale(b);
    let reach = cluster_reach(&boxes);
    // A word drawn as shapes, a logo's lettering, is grouped on its own: a
    // submittal's wordmark under its wiring diagrams joined them by reach,
    // and the region took the form's title under it as their label.
    let marks = wordmarks(&boxes);
    let rest: Vec<usize> = (0..boxes.len())
        .filter(|i| !marks.iter().any(|m| m.contains(i)))
        .collect();
    for part in marks.iter().chain(std::iter::once(&rest)) {
        split(&boxes, part, reach, &fit, &mut |group, b| {
            if let Some(f) = judge(&boxes, group, b, rules, page, size, &text) {
                out.push(f);
            }
        });
    }
    // A box drawn around a drawing frames it: the smallest panel its shapes
    // lie within, and larger than they are.
    for f in &mut out {
        let d = f.drawing;
        f.frame = frames
            .iter()
            .filter(|p| {
                p.x0 <= d.x0 + 1.0
                    && p.y0 <= d.y0 + 1.0
                    && p.x1 >= d.x1 - 1.0
                    && p.y1 >= d.y1 - 1.0
                    && p.width() * p.height() > d.width() * d.height() * 1.05
            })
            .min_by(|p, q| {
                (p.width() * p.height())
                    .partial_cmp(&(q.width() * q.height()))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied();
    }
    out
}

/// Words drawn as shapes: three or more set on one baseline at one height,
/// each no wider than two of its height, side by side as letters are, and
/// standing clear of every other shape by more than their height. A
/// stacked bar's segments and a diagram's row of boxes stand among the
/// rest of their drawing.
fn wordmarks(boxes: &[BBox]) -> Vec<Vec<usize>> {
    let mut used = vec![false; boxes.len()];
    let mut out = Vec::new();
    for i in 0..boxes.len() {
        if used[i] {
            continue;
        }
        let a = boxes[i];
        let h = a.height();
        if h <= 0.0 {
            continue;
        }
        let mut row: Vec<usize> = (0..boxes.len())
            .filter(|&j| {
                let b = boxes[j];
                !used[j]
                    && (b.y1 - a.y1).abs() <= 1.5
                    && (b.height() - h).abs() <= h * 0.2
                    && b.width() <= h * 2.0
            })
            .collect();
        row.sort_by(|&p, &q| {
            boxes[p]
                .x0
                .partial_cmp(&boxes[q].x0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // Side by side as letters: each starting within its height of the
        // last one's end.
        let mut chain: Vec<usize> = Vec::new();
        for &j in &row {
            match chain.last() {
                Some(&k) if boxes[j].x0 - boxes[k].x1 > h => {
                    if chain.contains(&i) {
                        break;
                    }
                    chain = vec![j];
                }
                _ => chain.push(j),
            }
        }
        if chain.len() < 3 || !chain.contains(&i) {
            continue;
        }
        let hull = chain
            .iter()
            .fold(boxes[chain[0]], |r, &j| r.union(&boxes[j]));
        let apart = (0..boxes.len()).filter(|j| !chain.contains(j)).all(|j| {
            let o = boxes[j];
            let dx = (o.x0 - hull.x1).max(hull.x0 - o.x1);
            let dy = (o.y0 - hull.y1).max(hull.y0 - o.y1);
            dx.max(dy) > h
        });
        if apart {
            for &j in &chain {
                used[j] = true;
            }
            out.push(chain);
        }
    }
    out
}

/// Group shapes `idx` at `reach` and hand each group of drawing size to
/// `found`, in the order the groups are found.
///
/// A group too large to be one drawing is drawings set close together — a
/// page's wiring diagrams stacked 50pt apart, joined at the widest reach —
/// and is grouped again at half the reach, where they part, rather than
/// thrown away with every drawing in it.
fn split(
    boxes: &[BBox],
    idx: &[usize],
    reach: f64,
    fit: &dyn Fn(&BBox) -> bool,
    found: &mut dyn FnMut(&[usize], BBox),
) {
    let sub: Vec<BBox> = idx.iter().map(|&i| boxes[i]).collect();
    for g in cluster(&sub, reach) {
        let group: Vec<usize> = g.iter().map(|&k| idx[k]).collect();
        if group.len() < MIN_FILLS {
            continue;
        }
        let b = hull(boxes, &group);
        if fit(&b) {
            found(&group, b);
        } else if reach > CLUSTER_REACH_MIN {
            split(
                boxes,
                &group,
                (reach * 0.5).max(CLUSTER_REACH_MIN),
                fit,
                found,
            );
        }
    }
}

/// What a page's text tells drawings apart by: the size of its prose, and
/// where it sets numbers.
struct PageText {
    prose: f32,
    numbers: Vec<BBox>,
}

/// The numbers a page sets, each as one box: runs of glyphs along one
/// baseline, a space apart at most, saying digits with only a point, a
/// comma, a sign, a percent or a currency sign between them: `1,000`,
/// `0.04`, `−20`, `45%`.
fn numbers(glyphs: &[Glyph]) -> Vec<BBox> {
    numbers_said(glyphs).into_iter().map(|(b, _)| b).collect()
}

/// [`numbers`], each with what it says.
fn numbers_said(glyphs: &[Glyph]) -> Vec<(BBox, String)> {
    let mut out = Vec::new();
    let mut run: Option<(BBox, f64, String)> = None;
    let number = |t: &str| {
        t.chars().any(|c| c.is_ascii_digit())
            && t.chars().count() <= 10
            && t.chars()
                .all(|c| c.is_ascii_digit() || ".,+-\u{2212}%$".contains(c))
    };
    for g in glyphs {
        let Some(b) = g.bbox else { continue };
        let size = (g.font_size as f64).max(1.0);
        let t = g.text.trim();
        let joins = run.as_ref().is_some_and(|(r, y, _)| {
            (b.y0 + b.y1) * 0.5 - y <= size * 0.3
                && y - (b.y0 + b.y1) * 0.5 <= size * 0.3
                && b.x0 - r.x1 <= size * 0.4
                && b.x0 - r.x1 >= -size * 0.5
        });
        if !joins || t.is_empty() {
            if let Some((r, _, s)) = run.take() {
                if number(&s) {
                    out.push((r, s));
                }
            }
        }
        if t.is_empty() {
            continue;
        }
        match run.as_mut() {
            Some((r, _, s)) => {
                *r = r.union(&b);
                s.push_str(t);
            }
            None => run = Some((b, (b.y0 + b.y1) * 0.5, t.to_string())),
        }
    }
    if let Some((r, _, s)) = run {
        if number(&s) {
            out.push((r, s));
        }
    }
    out
}

/// Fewest values an axis sets.
const AXIS_VALUES: usize = 3;

/// Lines of text, around a drawing, an axis's values may stand.
const AXIS_REACH: f64 = 4.0;

/// Is the drawing plotted against an axis: [`AXIS_VALUES`] numbers or
/// more around it, within [`AXIS_REACH`] lines, set in a column on one
/// right edge (`1,000` over `100` over `10`) or in a row on one baseline?
fn axis(b: &BBox, numbers: &[BBox], line: f64) -> bool {
    let reach = line * AXIS_REACH;
    let near: Vec<&BBox> = numbers
        .iter()
        .filter(|n| {
            n.x1 >= b.x0 - reach
                && n.x0 <= b.x1 + reach
                && n.y1 >= b.y0 - reach
                && n.y0 <= b.y1 + reach
        })
        .collect();
    near.iter().any(|a| {
        let column = near
            .iter()
            .filter(|n| (n.x1 - a.x1).abs() <= 2.0 && (n.y0 - a.y0).abs() >= line * 0.5)
            .count();
        let row = near
            .iter()
            .filter(|n| {
                ((n.y0 + n.y1) - (a.y0 + a.y1)).abs() <= 3.0 && (n.x0 - a.x0).abs() >= line * 0.5
            })
            .count();
        column + 1 >= AXIS_VALUES || row + 1 >= AXIS_VALUES
    })
}

/// Share of a scatter's marks its few symbols account for: one size to a
/// series, a few series to a plot.
const SYMBOL_SHARE: f64 = 0.9;

/// Marks drawn as a few symbols: the three commonest sizes, to the point,
/// account for [`SYMBOL_SHARE`] of them. A drawing's arrowheads and pieces
/// come in many.
fn symbols(marks: &[BBox]) -> bool {
    let mut sizes: std::collections::BTreeMap<(i64, i64), usize> = Default::default();
    for m in marks {
        *sizes
            .entry((m.width().round() as i64, m.height().round() as i64))
            .or_default() += 1;
    }
    let mut counts: Vec<usize> = sizes.into_values().collect();
    counts.sort_unstable_by(|a, b| b.cmp(a));
    let top: usize = counts.iter().take(3).sum();
    top as f64 >= marks.len() as f64 * SYMBOL_SHARE
}

/// Most of the places a matrix's rows and columns cross that its marks
/// fill: a scatter's points, set at a series' values, fill few.
const MATRIX_FILL: f64 = 0.3;

/// Marks set at scattered places, not a matrix's. A matrix sets one mark
/// to a place, in rows a line of text apart, each beside its row's label;
/// it repeats both its coordinates, two places or more to a row and to a
/// column on average, and fills [`MATRIX_FILL`] of the places they cross.
/// A scatter's values run on along one axis at least, or stand closer
/// than lines of text: a calibration's points at a few levels across,
/// each at nearly the same height.
fn scattered(marks: &[BBox], line: f64) -> bool {
    let snap = |v: f64| v.round() as i64;
    let mut rows: Vec<i64> = marks.iter().map(|m| snap(m.y0)).collect();
    rows.sort_unstable();
    rows.dedup();
    let mut columns: Vec<i64> = marks.iter().map(|m| snap(m.x0)).collect();
    columns.sort_unstable();
    columns.dedup();
    let mut places: Vec<(i64, i64)> = marks.iter().map(|m| (snap(m.x0), snap(m.y0))).collect();
    places.sort_unstable();
    places.dedup();
    let n = places.len();
    let (xs, ys) = (columns.len(), rows.len());
    if xs < 3 || ys < 3 {
        return false;
    }
    let mut gaps: Vec<i64> = rows.windows(2).map(|w| w[1] - w[0]).collect();
    gaps.sort_unstable();
    let pitch = gaps.get(gaps.len() / 2).map_or(0.0, |&g| g as f64);
    let matrix =
        n >= 2 * xs && n >= 2 * ys && n as f64 >= (xs * ys) as f64 * MATRIX_FILL && pitch >= line;
    !matrix
}

/// A group of shapes judged as one drawing: the figure it is, or `None`
/// when it is a table's shading, a band of the page, or too small.
fn judge(
    boxes: &[BBox],
    group: &[usize],
    b: BBox,
    rules: &[Rule],
    page: usize,
    size: (f64, f64),
    text: &PageText,
) -> Option<Figure> {
    if b.width().min(b.height()) < MIN_SIDE || b.width() * b.height() < MIN_AREA {
        return None;
    }
    if band(&b, size) {
        return None;
    }
    // A scatter's points line up in rows and columns as a table's cells
    // do, set at a series' values. They are marks smaller than a line of
    // the page's text, most of the group, drawn as a few symbols, spread
    // both ways and scattered: they fill few of the places their rows and
    // columns cross, where a matrix's dots fill most. And they are plotted
    // against an axis, its values set in a row or a column beside them. A
    // table's shading holds its text, a line high at least; a column of
    // drawn bullets, a seal's ring of dots, have no axis.
    let line = (text.prose as f64).max(1.0);
    let marks: Vec<BBox> = group
        .iter()
        .map(|&i| boxes[i])
        .filter(|a| a.width().max(a.height()) < line)
        .collect();
    let scatter = marks.len() * 2 > group.len()
        && symbols(&marks)
        && scattered(&marks, line)
        && axis(&b, &text.numbers, line);
    if !scatter {
        // Row banding: most fills starting and ending at the same x.
        let mut shared = 0usize;
        for &i in group {
            let a = boxes[i];
            let n = group
                .iter()
                .filter(|&&j| (boxes[j].x0 - a.x0).abs() < 1.0 && (boxes[j].x1 - a.x1).abs() < 1.0)
                .count();
            shared = shared.max(n);
        }
        if shared as f64 / group.len() as f64 > MAX_SHARED_X {
            return None;
        }
        // Cells, not a drawing: a table's shading set in rows and columns,
        // each shape sharing its column's edges with one and its row's with
        // another. A table that greys out its empty cells draws nothing else.
        let same = |a: f64, b: f64| (a - b).abs() < 1.0;
        let cells = group
            .iter()
            .filter(|&&i| {
                let a = boxes[i];
                let column = group
                    .iter()
                    .any(|&j| j != i && same(boxes[j].x0, a.x0) && same(boxes[j].x1, a.x1));
                let row = group
                    .iter()
                    .any(|&j| j != i && same(boxes[j].y0, a.y0) && same(boxes[j].y1, a.y1));
                column && row
            })
            .count();
        if cells as f64 / group.len() as f64 > MAX_SHARED_X {
            return None;
        }
    }
    // Strips, not a drawing. A section's header panel with its accent
    // bar and the identification box beneath it are three shapes, and
    // two of them are strips running the whole width of the cluster.
    // A chart's shapes are not: bars stop at their values, wedges at
    // their share, and a plot's background is as tall as it is wide.
    // When half the shapes are strips spanning the cluster, the cluster
    // is a stack of boxes around text, and the text is headings and
    // fields — not labels.
    let strips = group
        .iter()
        .filter(|&&i| {
            let f = boxes[i];
            f.width() >= b.width() * PANEL_WIDTH && f.width() >= f.height() * STRIP_ASPECT
        })
        .count();
    if strips * 2 >= group.len() {
        return None;
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
        return None;
    }
    Some(Figure {
        bbox: b,
        drawing: b,
        page,
        shapes: group.len(),
        frame: None,
        axes: false,
    })
}

/// Is fill `i` the background of text set on it, rather than a shape of a
/// drawing?
///
/// Spec sheets and reports set text on drawn backgrounds: a grey band
/// behind each specification row, a coloured callout box, a table's shaded
/// header cells, a form's field boxes, a side tab. Grouped with the shapes
/// of a drawing beside them, they made one "drawing" of the drawing and the
/// text around it, and every heading, paragraph and row on them was typed
/// as its labels. A panel says something in words; a chart's bar, wedge or
/// plot area carries at most a value or a few scattered labels.
///
/// So a fill holding words set to be read is a background when nothing
/// else is drawn on it — a band, a cell, a box around a line — or, when
/// other shapes are drawn on it, when its text covers it as a panel's does.
/// A plot's background holds the bars and only scattered tick and value
/// labels; a diagram's frame holds the diagram and a label here and there.
/// And a diagram's boxes hold its labels, set well under the page's text:
/// a wiring diagram names its devices at 4pt beside 10pt prose.
fn background(i: usize, boxes: &[BBox], fields: &[bool], ink: &Ink) -> bool {
    let b = boxes[i];
    let on = ink.on(&b);
    if on.letters < TEXT_LETTERS || !ink.read_on(&on) {
        return false;
    }
    let area = b.width() * b.height();
    // A form's fields drawn on a box are no drawing on it.
    let held: Vec<&BBox> = boxes
        .iter()
        .enumerate()
        .filter(|(j, o)| {
            let (cx, cy) = ((o.x0 + o.x1) * 0.5, (o.y0 + o.y1) * 0.5);
            *j != i
                && !fields[*j]
                && b.contains(cx, cy)
                && o.width() * o.height() < area * HELD_AREA
        })
        .map(|(_, o)| o)
        .collect();
    let Some(&first) = held.first() else {
        return true;
    };
    // Shapes kept to one end of a box, with words set on it beside them,
    // are a logo on a panel: a form's footer box with its certification
    // marks at its left end and `APPROVALS:` along it. A plot's bars and a
    // diagram's parts spread across the box that holds them, or stand in
    // its middle with their labels round them.
    let hull = held.iter().fold(*first, |r, o| r.union(o));
    let beside = on.letters.saturating_sub(ink.on(&hull).letters);
    let at_an_end =
        hull.x0 - b.x0 <= b.width() * CORNER_END || b.x1 - hull.x1 <= b.width() * CORNER_END;
    // A band along the page, as a footer's box is; a diagram's circle
    // holding a mark near its edge is no panel.
    let band = b.width() >= b.height() * BAND_ASPECT;
    on.cover >= TEXT_COVER
        || (band && hull.width() < b.width() * CORNER_SHARE && at_an_end && beside >= TEXT_LETTERS)
}

/// Least width, in heights, of a box that holds a logo at one end and
/// words along the rest: a band, as a footer's form box is.
const BAND_ASPECT: f64 = 3.0;

/// Farthest, as a share of a box's width, shapes kept to one end of it
/// stand from that end. A figure's box holding a diagram in its middle,
/// with labels around it, holds a drawing.
const CORNER_END: f64 = 0.1;

/// Widest share of a box shapes drawn on it may span and be a logo set in
/// its corner rather than a drawing the box holds.
const CORNER_SHARE: f64 = 0.5;

/// Size, against the page's text, from which a colon is display type's, a
/// title's, not a form label's.
const LABEL_DISPLAY: f32 = 1.3;

/// Is `b` shaped as a form's field: a line or two high at the page's text
/// size `size`, and at least three times as wide.
fn field_shaped(b: &BBox, size: f64) -> bool {
    b.height() >= size * 0.8 && b.height() <= size * 3.0 && b.width() >= b.height() * 3.0
}

/// Does a label's colon stand just before `b` on its row, or just over its
/// start?
fn labelled(b: &BBox, colons: &[BBox], size: f64) -> bool {
    colons.iter().any(|c| {
        let cy = (c.y0 + c.y1) * 0.5;
        let before = cy >= b.y0 && cy <= b.y1 && c.x1 <= b.x0 + 1.0 && c.x1 >= b.x0 - size * 3.0;
        let over = c.y1 <= b.y0 + 1.0
            && c.y1 >= b.y0 - size * 2.0
            && c.x1 >= b.x0 - size
            && c.x1 <= b.x0 + b.width() * 0.5;
        before || over
    })
}

/// Is `b` drawn along an edge of panel `p`: touching it, and running the
/// length of that edge? The coloured bar over a table's header, the accent
/// down a callout's side, a box's frame drawn as four strips.
fn edging(b: &BBox, p: &BBox) -> bool {
    let touches = b.x0 <= p.x1 + EDGE_TOUCH
        && p.x0 <= b.x1 + EDGE_TOUCH
        && b.y0 <= p.y1 + EDGE_TOUCH
        && p.y0 <= b.y1 + EDGE_TOUCH;
    let across = (b.x0 - p.x0).abs() <= EDGE_TOUCH && (b.x1 - p.x1).abs() <= EDGE_TOUCH;
    let down = (b.y0 - p.y0).abs() <= EDGE_TOUCH && (b.y1 - p.y1).abs() <= EDGE_TOUCH;
    touches && (across && b.height() < p.height() || down && b.width() < p.width())
}

/// How far, in points, a panel's edging may stand off it or fall short of
/// its corners.
const EDGE_TOUCH: f64 = 1.5;

/// Most space, in points, between two cells of one table: they abut, or a
/// hairline of white parts them.
const CELL_TOUCH: f64 = 1.5;

/// Fewest letters that make text on a shape words rather than a value: a
/// short word. `37%`, `2021` and `$4.2bn` are what a chart sets on its bars.
const TEXT_LETTERS: usize = 3;

/// Smallest the text on a panel is set, against the page's prose: text set
/// to be read, as a table's cells and a callout's paragraph are (0.82 and
/// up, measured), not a diagram's labels (0.42).
const PANEL_TEXT: f32 = 0.75;

/// Share of a shape that holds others the text on it must cover for it to
/// be a panel: a callout box or a table's body behind its rows, measured
/// from 0.12 up, where a plot's background with its labels covers 0.02-0.09.
const TEXT_COVER: f64 = 0.1;

/// A shape drawn on another is smaller than it by at least this share; a
/// fill and its own outline drawn a hair inside it are one shape.
const HELD_AREA: f64 = 0.8;

/// The page's inked glyphs by their centres, sorted down the page, for
/// asking what text is set on a shape.
struct Ink {
    marks: Vec<Mark>,
    /// The size most of the page's letters are set at: its prose.
    prose: f32,
}

/// One inked glyph, as [`Ink`] keeps it.
struct Mark {
    y: f64,
    x: f64,
    /// The glyph's advance by its size: the room it takes on the line.
    area: f64,
    letters: usize,
    /// Letters and digits: what says something, as against punctuation.
    says: usize,
    size: f32,
}

/// What text is set on a shape.
struct OnShape {
    /// Share of the shape the text covers.
    cover: f64,
    letters: usize,
    says: usize,
    /// Size of the largest of its letters and digits.
    size: f32,
}

impl Ink {
    fn new(glyphs: &[Glyph]) -> Ink {
        let mut marks: Vec<Mark> = glyphs
            .iter()
            .filter_map(|g| {
                let b = g.bbox?;
                let w = g.advance.map(f64::abs).unwrap_or(0.0).max(b.width());
                Some(Mark {
                    y: (b.y0 + b.y1) * 0.5,
                    x: (b.x0 + b.x1) * 0.5,
                    area: w * g.font_size as f64,
                    letters: g.text.chars().filter(|c| c.is_alphabetic()).count(),
                    says: g.text.chars().filter(|c| c.is_alphanumeric()).count(),
                    size: g.font_size,
                })
            })
            .collect();
        marks.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
        let mut by_size: Vec<(i32, usize)> = Vec::new();
        for m in &marks {
            let k = (m.size * 2.0).round() as i32;
            match by_size.iter_mut().find(|(s, _)| *s == k) {
                Some((_, n)) => *n += m.letters,
                None => by_size.push((k, m.letters)),
            }
        }
        let prose = by_size
            .iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
            .map_or(0.0, |&(k, _)| k as f32 / 2.0);
        Ink { marks, prose }
    }

    /// The glyphs centred on `b`.
    fn on(&self, b: &BBox) -> OnShape {
        let start = self.marks.partition_point(|m| m.y < b.y0);
        let mut on = OnShape {
            cover: 0.0,
            letters: 0,
            says: 0,
            size: 0.0,
        };
        for m in &self.marks[start..] {
            if m.y > b.y1 {
                break;
            }
            if m.x < b.x0 || m.x > b.x1 {
                continue;
            }
            on.cover += m.area;
            on.letters += m.letters;
            on.says += m.says;
            if m.says > 0 {
                on.size = on.size.max(m.size);
            }
        }
        on.cover /= (b.width() * b.height()).max(1.0);
        on
    }

    /// Is text set to be read on `b`: at the size of the page's text, not
    /// a diagram's labels?
    fn read_on(&self, on: &OnShape) -> bool {
        on.says > 0 && on.size >= self.prose * PANEL_TEXT
    }
}

/// A band of the page's design rather than a drawing on it: a sidebar run
/// down the page from its edge, or a banner or footer strip across it.
///
/// A drawing sits on the page, inside its margins. A spec sheet's grey
/// sidebar runs off the page's top and foot, and a catalogue's header
/// banner across the page from its top edge; both draw a few shapes, and
/// both hold body text: side-heads, a product's name and its key figures.
/// Taken for drawings, that text was typed as their labels and its
/// headings were lost.
fn band(b: &BBox, (pw, ph): (f64, f64)) -> bool {
    if pw <= 0.0 || ph <= 0.0 {
        return false;
    }
    let side = b.x0 <= PAGE_EDGE || b.x1 >= pw - PAGE_EDGE;
    let end = b.y0 <= PAGE_EDGE || b.y1 >= ph - PAGE_EDGE;
    (b.height() >= ph * BAND_SPAN && side) || (b.width() >= pw * BAND_SPAN && end)
}

/// Share of the page a band runs along.
const BAND_SPAN: f64 = 0.9;

/// How near the page's edge, in points, a band reaches.
const PAGE_EDGE: f64 = 2.0;

/// How far outside the drawing a label may sit and still belong to it, as a
/// fraction of the drawing's smaller dimension. A donut sets its labels a
/// little clear of the ring; a bar chart sets its tick labels just under the
/// axis and its value labels just over the bars.
const LABEL_REACH: f64 = 0.5;

/// Widest a block may be, as a fraction of the page, to be read as a label
/// rather than as prose the figure happens to sit beside. Chart labels are
/// short by construction — a region name, a value, an axis tick.
const LABEL_MAX_WIDTH: f64 = 0.45;

/// A page whose text is mostly inside its drawings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Doubt {
    pub page: usize,
    /// Share of the page's text characters sitting inside a figure region.
    pub share: f64,
    /// Characters on the page, so a caller can see how much is at stake.
    pub chars: usize,
}

/// Least text on a page before the share means anything. A caption under a
/// photograph is entirely inside a figure and entirely correct.
const MIN_PAGE_CHARS: usize = 200;

/// Share of a page's text inside drawings above which the page is a designed
/// layout rather than a column of prose.
///
/// This is a *recognition* signal, not a defect report. A page built as a
/// composition — an infographic, a magazine spread, a report cover — is where
/// reading order, heading rank and figure boundaries are all inferred from
/// geometry that was never arranged to be read linearly, and it is where a
/// deterministic pass is least trustworthy. That the text landed inside the
/// drawings is the most direct evidence available that the page is one of
/// those.
///
/// Measured: the two documents a field report faulted sit at 42% and 92%,
/// against 0-1% for ordinary reports, papers and handbooks. On the
/// 200-document evaluation corpus this flags six.
const MIN_FIGURE_SHARE: f64 = 0.30;

/// Pages where the drawings hold most of the text.
///
/// Reported, never acted on here — the same contract as
/// [`crate::heading::doubt`] and [`crate::column::doubt`]. Whether a page this
/// describes is worth a reader is a decision about a corpus, not a page.
pub fn doubt(elements: &[fluree_doc_model::Element], page: usize) -> Option<Doubt> {
    let (mut total, mut inside) = (0usize, 0usize);
    for e in elements.iter().filter(|e| e.page == page) {
        let n = e.text.trim().chars().count();
        total += n;
        if e.figure.is_some() {
            inside += n;
        }
    }
    if total < MIN_PAGE_CHARS {
        return None;
    }
    let share = inside as f64 / total as f64;
    (share >= MIN_FIGURE_SHARE).then_some(Doubt {
        page,
        share,
        chars: total,
    })
}

/// Take the labels drawn around each figure, and grow it to cover them.
/// Returns, for each figure, the blocks it took, by index into `blocks`:
/// those are its labels, and no others are.
///
/// Detection finds the drawing, and a drawing does not contain its own
/// labels: J&J's donut occupies x 238-356 while `Latin America` sits at 185.
/// Taking the wedges alone would mark a figure with nothing in it and leave
/// every label loose in the prose — the exact failure this is meant to end.
///
/// Labels chain: a value sits above its bar and the tick label below it,
/// and a chart drawn in strokes leaves only its legend's swatches to be
/// found, its axis labels reachable one from the next. Text set at the size
/// of the page's prose does not chain. A spec sheet's logo or photo stands
/// beside a column of short blocks, and walked from block to block the
/// region crossed the page: a fact sheet's paragraphs and table titles, a
/// sell sheet's headings and bullets, all typed as the logo's labels. Text
/// at prose size is the drawing's only when it is near the drawing itself,
/// and beside, above or below it rather than off a corner: a cover's
/// description set clear of its logo's corner was typed as the logo's
/// label. A label set smaller, as tick labels, legends and source notes
/// are, may be reached from a label already taken — from the label, not
/// from the box around all of them, and never further from the drawing
/// than the drawing's own size. Measured from the growing box, fine print
/// walked from a drawing in a page's corner up through the table and the
/// footnote beside it, 300pt away.
///
/// The rectangle around a drawing and its labels is not a label: a
/// paragraph or heading inside it that no rule here took stays the page's.
/// Only short, terse blocks are taken, so a paragraph beside the chart
/// stays prose however close and however small it is set.
///
/// Numbered callouts on a photograph reach for nothing (see [`callouts`]):
/// their labels are the marks in them, inside the drawing already.
pub fn attach(
    figures: &mut [Figure],
    blocks: &[(BBox, f32, &str)],
    images: &[BBox],
    page: (f64, f64),
    prose: f32,
) -> Vec<Vec<usize>> {
    attach_beside(figures, blocks, images, page, prose, &[])
}

/// The page's words drawn as shapes, a logo's lettering: each run of letters
/// [`wordmarks`] finds among `fills`, by its extent.
pub fn marks(fills: &[Fill]) -> Vec<BBox> {
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
    wordmarks(&boxes)
        .iter()
        .map(|m| m.iter().fold(boxes[m[0]], |r, &j| r.union(&boxes[j])))
        .collect()
}

/// As [`attach`], leaving to it the text set beside a logo's lettering,
/// `marks`, on its row.
pub fn attach_beside(
    figures: &mut [Figure],
    blocks: &[(BBox, f32, &str)],
    images: &[BBox],
    page: (f64, f64),
    prose: f32,
    marks: &[BBox],
) -> Vec<Vec<usize>> {
    let (page_width, page_height) = page;
    let max_w = page_width * LABEL_MAX_WIDTH;
    // The same bound detection applies: a region this large is the page, not a
    // drawing on it. Detection already refuses to *find* one, and attachment
    // must not be able to grow one either.
    let too_big = |b: &BBox| {
        page_width > 0.0
            && page_height > 0.0
            && b.width() / page_width > MAX_PAGE_FRACTION
            && b.height() / page_height > MAX_PAGE_FRACTION
    };
    // A page-sized image is the page's background, not a picture on it.
    let photos: Vec<BBox> = images.iter().copied().filter(|b| !too_big(b)).collect();
    let mut labels = Vec::with_capacity(figures.len());
    let gap = |b: &BBox, r: &BBox| {
        let dx = (b.x0 - r.x1).max(r.x0 - b.x1).max(0.0);
        let dy = (b.y0 - r.y1).max(r.y0 - b.y1).max(0.0);
        dx.max(dy)
    };
    // A frame drawn around drawings bounds their figures, where all it
    // holds is labels: a wiring diagram's box. Text outside it is the
    // page's whatever its distance (a note in the column beside, the title
    // over it), and what it holds goes to the drawing nearest within it. A
    // panel that holds a paragraph beside its chart is no frame.
    //
    // The drawings fill a frame: a form's box with a logo in its corner, or
    // a panel with a chart beside its text, is no frame for them.
    let framed: Vec<Option<BBox>> = figures
        .iter()
        .map(|f| {
            f.frame.filter(|fr| {
                let held = figures
                    .iter()
                    .filter(|g| g.frame == Some(*fr))
                    .fold(f.drawing, |r, g| r.union(&g.drawing));
                let inside: Vec<&(BBox, f32, &str)> = blocks
                    .iter()
                    .filter(|(b, ..)| share_inside(b, fr) >= MARK_INSIDE)
                    .collect();
                // Its labels stand in its margins, round the drawings: a
                // plot's own background box, filled by its bars, holds only
                // values, and its axes' names and legend lie outside it.
                let margin = inside.iter().any(|(b, ..)| {
                    figures
                        .iter()
                        .filter(|g| g.frame == Some(*fr))
                        .all(|g| share_inside(b, &g.drawing) < MARK_INSIDE)
                });
                // And it is no page's background, and holds no display
                // type: a slide's background holds its title.
                let page_wide = page_width > 0.0
                    && page_height > 0.0
                    && fr.width() >= page_width * FRAME_PAGE
                    && fr.height() >= page_height * FRAME_PAGE;
                !page_wide
                    && held.width() >= fr.width() * FRAME_FILL
                    && held.height() >= fr.height() * FRAME_FILL
                    && margin
                    && inside.iter().all(|(_, size, t)| {
                        t.split_whitespace().count() <= LABEL_MAX_WORDS
                            && *size < prose * FRAME_DISPLAY
                    })
            })
        })
        .collect();
    let drawings: Vec<BBox> = figures.iter().map(|f| f.drawing).collect();
    for (k, f) in figures.iter_mut().enumerate() {
        let drawing = f.drawing;
        if let Some(fr) = framed[k] {
            let mut taken: Vec<usize> = (0..blocks.len())
                .filter(|&i| {
                    let b = &blocks[i].0;
                    share_inside(b, &fr) >= MARK_INSIDE
                        && (0..drawings.len())
                            .filter(|&j| framed[j] == Some(fr))
                            .min_by(|&p, &q| {
                                gap(b, &drawings[p])
                                    .partial_cmp(&gap(b, &drawings[q]))
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                            == Some(k)
                })
                .collect();
            // Its caption is the figure's all the same: `Figure 17. …` set
            // over or under the frame, a `Note:` or `Source:` under it, and
            // the lines that run on from them, within a line or two of the
            // frame or of each other. A title that is no caption, and a note
            // in the column beside, are the page's.
            let over_under = |b: &BBox| b.x1.min(fr.x1) - b.x0.max(fr.x0) >= b.width() * 0.5;
            let mut captions: Vec<usize> = Vec::new();
            let free = |i: usize, b: &BBox| {
                !taken.contains(&i) && share_inside(b, &fr) < MARK_INSIDE && over_under(b)
            };
            loop {
                let mut grew = false;
                for (i, (b, size, text)) in blocks.iter().enumerate() {
                    if captions.contains(&i)
                        || !free(i, b)
                        || !(crate::heading::is_caption(text) || note_label(text))
                    {
                        continue;
                    }
                    // The caption with the lines that run on from it, as one.
                    let mut unit = vec![i];
                    loop {
                        let next = (0..blocks.len()).find(|&j| {
                            !unit.contains(&j)
                                && !captions.contains(&j)
                                && free(j, &blocks[j].0)
                                && unit.iter().any(|&u| {
                                    runs_on(&blocks[u].0, blocks[u].1, &blocks[j].0, blocks[j].1)
                                })
                        });
                        match next {
                            Some(j) => unit.push(j),
                            None => break,
                        }
                    }
                    let line = (*size as f64).max(1.0);
                    let near = |a: &BBox, r: &BBox| {
                        (-0.5 * line..=CAPTION_GAP * line).contains(&(a.y0 - r.y1).max(r.y0 - a.y1))
                    };
                    let reached = unit.iter().any(|&u| {
                        let a = &blocks[u].0;
                        near(a, &fr) || captions.iter().any(|&c| near(a, &blocks[c].0))
                    });
                    if reached {
                        captions.extend(unit);
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
            taken.extend(captions);
            taken.sort_unstable();
            f.bbox = taken.iter().fold(drawing, |r, &t| r.union(&blocks[t].0));
            labels.push(taken);
            continue;
        }
        // A picture's labels are the blocks set on it (see [`pictures`]),
        // with those hung over its edge, a layer's name set half off the
        // cutaway: what lies round it is the page's.
        if f.shapes == 0 {
            let taken: Vec<usize> = (0..blocks.len())
                .filter(|&i| {
                    let (b, size, text) = blocks[i];
                    let on = share_inside(&b, &drawing);
                    on >= MARK_INSIDE || (on > 0.0 && picture_label(size, text, prose))
                })
                .collect();
            f.bbox = taken.iter().fold(drawing, |r, &t| r.union(&blocks[t].0));
            labels.push(taken);
            continue;
        }
        // The drawing's own marks: text set inside it.
        let mut taken: Vec<usize> = (0..blocks.len())
            .filter(|&i| share_inside(&blocks[i].0, &drawing) >= MARK_INSIDE)
            .collect();
        // A photo's callouts take the marks in them, and reach for nothing.
        if callouts(&drawing, blocks, &photos) {
            labels.push(taken);
            continue;
        }
        let reach = drawing.width().min(drawing.height()) * LABEL_REACH;
        // Beside, above or below the drawing: sharing its height or its
        // width. Text off one of its corners is the page's.
        let beside = |b: &BBox| {
            b.x1.min(drawing.x1) > b.x0.max(drawing.x0)
                || b.y1.min(drawing.y1) > b.y0.max(drawing.y0)
        };
        // The page's head band holds its running heads. A line set there at
        // the prose's size is a drawing's only when it stands against it, a
        // title over a chart at the top of the page; a badge's small words
        // there are labels as anywhere: a running head over a page of plots took
        // the region to the page's top, and the plots' legend under them
        // then grew it past the bound. A chart's source and notes stand
        // under it, in the foot band as often as not.
        let banded = |b: &BBox, size: f32| {
            page_height > 0.0
                && b.y1 <= page_height * PAGE_BAND
                && gap(b, &drawing) > (size as f64).max(1.0) * BAND_TITLE_LINES
        };
        loop {
            let mut grew = false;
            for (i, (b, size, text)) in blocks.iter().enumerate() {
                // Nor is an address: a line closing on a web address, an
                // e-mail address or a phone number says where to reach the
                // page's maker, not what the drawing shows.
                if b.width() > max_w
                    || text.split_whitespace().count() > LABEL_MAX_WORDS
                    || taken.contains(&i)
                    || crate::heading::ends_with_contact(text)
                {
                    continue;
                }
                let label = *size < prose * LABEL_SIZE;
                let near = gap(b, &drawing) <= reach;
                let chained = label
                    && gap(b, &drawing) <= reach * CHAIN_LIMIT
                    && taken.iter().any(|&t| gap(b, &blocks[t].0) <= reach);
                // Text at the prose's size is the drawing's only as a title,
                // a legend or an axis's name: no wider than the drawing, give
                // or take its tick labels. A sentence beside a small code or
                // an icon runs well past it, and is the page's.
                let fits = b.width() <= drawing.width() * PROSE_LABEL_WIDTH;
                if !(near && (label || (beside(b) && fits))) && !chained {
                    continue;
                }
                // Nor is a line of words whose sentence runs on into the
                // page's prose under it: a paragraph's, parted where a
                // justified line's space falls in line with the plots'
                // gutter. A legend's entry says a word or two.
                if !label && words(text) >= RUN_WORDS && runs_into_lower(b, *size, blocks) {
                    continue;
                }
                // Text set beside a logo's lettering on its row is the
                // logo's company: a form's title beside the wordmark in a
                // footer, under the page's diagrams, is none of theirs.
                if marks.iter().any(|m| {
                    m.y1.min(b.y1) > m.y0.max(b.y0)
                        && (b.x0 - m.x1).max(m.x0 - b.x1) <= *size as f64 * MARK_REACH
                }) {
                    continue;
                }
                if !label && banded(b, *size) {
                    continue;
                }
                let grown = f.bbox.union(b);
                if too_big(&grown) {
                    continue;
                }
                f.bbox = grown;
                taken.push(i);
                grew = true;
            }
            if !grew {
                break;
            }
        }
        // A label that runs on from text the figure did not take is that
        // text's: a list of notes set above a form, a drawing's neighbour
        // in a column of bullets. Its first lines were out of reach and its
        // last were in, and the lines are one run all the same.
        //
        // And text in a column of its own beside the drawing, sharing its
        // rows, is the page's: a heading and a note set flush with the
        // page's paragraphs in the column beside a drawing's box (`FCC`,
        // `CAUTION: …`), or a run of instructions wrapped from line to line
        // (`a. … connect the black fixture lead to the` over `voltage
        // supply …`). A label is terse, and stands among the drawing's.
        let outside = |t: usize| share_inside(&blocks[t].0, &drawing) < MARK_INSIDE;
        let beside_rows = |t: usize| {
            let b = &blocks[t].0;
            b.y1.min(drawing.y1) > b.y0.max(drawing.y0)
                && (b.x1 <= drawing.x0 + 1.0 || b.x0 >= drawing.x1 - 1.0)
        };
        let wraps = |a: usize, b: usize| {
            let ((ba, sa, ta), (bb, sb, tb)) = (blocks[a], blocks[b]);
            let line = sa.max(sb).max(1.0) as f64;
            let gap = bb.y0 - ba.y1;
            // Lines of running text, a few words each: a diagram's labels
            // (`psbT` over `nR`) say a word.
            (-0.3 * line..=line * RUN_GAP).contains(&gap)
                && (sa - sb).abs() <= RUN_SIZE
                && (bb.x0 - ba.x0).abs() <= line * 2.0
                && words(ta) >= RUN_WORDS
                && words(tb) >= RUN_WORDS
                && !ends_sentence(ta)
                && opens_lower(tb)
        };
        let wrapped: Vec<usize> = taken
            .iter()
            .copied()
            .filter(|&a| outside(a) && beside_rows(a))
            .flat_map(|a| {
                taken
                    .iter()
                    .copied()
                    .filter(move |&b| b != a && wraps(a, b))
                    .flat_map(move |b| [a, b])
            })
            .collect();
        let mut released: Vec<usize> = Vec::new();
        // Two wraps in one column, not one: a diagram's label can break
        // over two lines.
        for &w in &wrapped {
            let x = blocks[w].0.x0;
            let line = blocks[w].1.max(1.0) as f64;
            let pairs = wrapped
                .iter()
                .filter(|&&o| (blocks[o].0.x0 - x).abs() <= line * 2.0)
                .count()
                / 2;
            if pairs >= 2 && !released.contains(&w) {
                released.push(w);
            }
        }
        taken.retain(|t| !released.contains(t));
        let paragraph = |j: usize| blocks[j].2.split_whitespace().count() > LABEL_MAX_WORDS;
        loop {
            let free = |j: usize, taken: &[usize]| !taken.contains(&j);
            // Set in a column with the page's text: flush with a paragraph
            // the drawing did not take, or with a line already released.
            let column = |t: usize, j: usize| {
                let (b, size, _) = blocks[t];
                let (o, osize, _) = blocks[j];
                let line = size.max(osize).max(1.0) as f64;
                let gap = (o.y0 - b.y1).max(b.y0 - o.y1);
                // A line released with its column says a few words too: a
                // chart's ticks and axis's name stand by its plot, whoever
                // stands beside them.
                gap <= line * COLUMN_GAP
                    && if released.contains(&j) {
                        (o.x0 - b.x0).abs() <= line * 2.0 && words(blocks[t].2) >= RUN_WORDS
                    } else {
                        paragraph(j) && beside_rows(j) && (o.x0 - b.x0).abs() <= RUN_ALIGN
                    }
            };
            // The line introducing a list, on its colon, over the list's
            // first item set in under it, wherever it stands by the
            // drawing.
            let introduces = |t: usize, j: usize| {
                let (b, size, text) = blocks[t];
                let (o, osize, _) = blocks[j];
                let line = size.max(osize).max(1.0) as f64;
                text.trim_end().ends_with(':')
                    && (paragraph(j) || released.contains(&j))
                    && (0.0..=line * COLUMN_GAP).contains(&(o.y0 - b.y1))
                    && (0.0..=line * 6.0).contains(&(o.x0 - b.x0))
            };
            let next = taken.iter().position(|&t| {
                let (b, size, _) = blocks[t];
                outside(t)
                    && (0..blocks.len()).any(|j| {
                        free(j, &taken)
                            && (runs_on(&b, size, &blocks[j].0, blocks[j].1)
                                || (beside_rows(t) && column(t, j))
                                || introduces(t, j))
                    })
            });
            match next {
                Some(k) => {
                    released.push(taken.remove(k));
                }
                None => break,
            }
        }
        // A label taken only through others that are released goes with
        // them: the line introducing a run of instructions, set off the
        // drawing's corner.
        if !released.is_empty() {
            loop {
                let orphan = taken.iter().position(|&t| {
                    let (b, size, _) = blocks[t];
                    let label = size < prose * LABEL_SIZE;
                    let fits = b.width() <= drawing.width() * PROSE_LABEL_WIDTH;
                    let own = gap(&b, &drawing) <= reach && (label || (beside(&b) && fits));
                    outside(t)
                        && !own
                        && !taken
                            .iter()
                            .any(|&u| u != t && gap(&b, &blocks[u].0) <= reach)
                });
                match orphan {
                    Some(k) => {
                        released.push(taken.remove(k));
                    }
                    None => break,
                }
            }
        }
        f.bbox = taken.iter().fold(drawing, |r, &t| r.union(&blocks[t].0));
        taken.sort_unstable();
        labels.push(taken);
    }
    labels
}

/// Give back to the page each label that introduces a list on its colon,
/// `Make the following Electrical Connections to supply cord:` over its
/// lettered steps, within [`COLUMN_GAP`] lines above an item and no more
/// than six ems left of it. A list item is no label, so the steps are the
/// page's, and the line leading into them is theirs. `items` are the
/// page's list items, by box and size.
pub fn release_lead_ins(
    taken: &mut [Vec<usize>],
    blocks: &[(BBox, f32, &str)],
    items: &[(BBox, f32)],
) {
    for labels in taken.iter_mut() {
        labels.retain(|&k| {
            let (b, size, text) = blocks[k];
            !(text.trim_end().ends_with(':')
                && items.iter().any(|(o, osize)| {
                    let line = size.max(*osize).max(1.0) as f64;
                    (0.0..=line * COLUMN_GAP).contains(&(o.y0 - b.y1))
                        && (0.0..=line * 6.0).contains(&(o.x0 - b.x0))
                }))
        });
    }
}

/// Give back to the page each label set outside its drawing with a list's
/// item between the two: a column's heading over its bullets, the drawing
/// under them (`CABINET CONSTRUCTION` over its items, over a cabinet's
/// drawing). A list item is no label, so the reach went over it to the
/// heading beyond; a drawing's title or axis name stands against it, with
/// nothing of the page's between. `items` are the page's list items.
pub fn release_behind_items(
    taken: &mut [Vec<usize>],
    figures: &[Figure],
    blocks: &[(BBox, f32, &str)],
    items: &[BBox],
) {
    for (labels, f) in taken.iter_mut().zip(figures) {
        let d = f.drawing;
        labels.retain(|&k| {
            let b = blocks[k].0;
            if share_inside(&b, &d) >= MARK_INSIDE {
                return true;
            }
            let across = |o: &BBox| o.x1.min(b.x1) > o.x0.max(b.x0);
            let level = |o: &BBox| o.y1.min(b.y1) > o.y0.max(b.y0);
            !items.iter().any(|o| {
                (b.y1 <= d.y0 && across(o) && o.y0 >= b.y1 && o.y1 <= d.y0)
                    || (b.y0 >= d.y1 && across(o) && o.y1 <= b.y0 && o.y0 >= d.y1)
                    || (b.x1 <= d.x0 && level(o) && o.x0 >= b.x1 && o.x1 <= d.x0)
                    || (b.x0 >= d.x1 && level(o) && o.x1 <= b.x0 && o.x0 >= d.x1)
            })
        });
    }
}

/// How much larger than the prose a drawing's title is set, at least, for
/// [`reassign_between`]: the step the heading rules take for display type.
const TITLE_SIZE: f64 = 1.15;

/// Give each title set between two drawings, under one and over the other
/// and across both, to the one it stands nearer, measured to the drawing
/// with its other labels: a diagram's title under the diagram above it
/// heads the one below, whose labels start a line under the title. Even,
/// it goes to the lower: a title stands over what it names. Where the lower
/// drawing is framed, and the label stands outside its frame and is no
/// caption or note, it is the page's, as a title over a framed drawing is
/// (see the frame rule in [`attach`]).
pub fn reassign_between(
    taken: &mut [Vec<usize>],
    figures: &[Figure],
    blocks: &[(BBox, f32, &str)],
    prose: f32,
) {
    // A figure's drawing with the labels it alone took: a label both took,
    // an axis's ticks within either's reach, says nothing of which it is.
    let region = |taken: &[Vec<usize>], fi: usize, other: usize, skip: usize| {
        taken[fi]
            .iter()
            .filter(|&&t| t != skip && !taken[other].contains(&t))
            .fold(figures[fi].drawing, |r, &t| r.union(&blocks[t].0))
    };
    let across = |a: &BBox, b: &BBox| a.x1.min(b.x1) > a.x0.max(b.x0);
    for a in 0..figures.len() {
        let mut moves: Vec<(usize, usize)> = Vec::new();
        for &k in &taken[a] {
            let b = blocks[k].0;
            let upper = figures[a].drawing;
            // Display titles only: set larger than the prose, in words, and
            // no caption. An axis's ticks and name under a panel are the
            // panel's however close the next panel's labels stand, and a
            // paper sets its figure's caption under the figure.
            let (_, size, text) = blocks[k];
            let title = (size as f64) > prose as f64 * TITLE_SIZE
                && text.chars().filter(|c| c.is_alphabetic()).count() >= 2
                && !crate::heading::is_caption(text);
            if b.y0 < upper.y1 || !across(&b, &upper) || !title {
                continue;
            }
            let lower = (0..figures.len())
                .filter(|&o| {
                    o != a && figures[o].drawing.y0 >= b.y1 && across(&b, &figures[o].drawing)
                })
                .map(|o| {
                    let r = region(taken, o, a, k);
                    (o, (r.y0 - b.y1).max(0.0))
                })
                .min_by(|x, y| x.1.total_cmp(&y.1));
            // To the upper, from the lowest of its own that stand over the
            // label: its drawing, or a label of its own between the two.
            let own_above = lower.map_or(upper.y1, |(o, _)| {
                taken[a]
                    .iter()
                    .filter(|&&t| t != k && !taken[o].contains(&t))
                    .map(|&t| blocks[t].0)
                    .filter(|r| r.y1 <= b.y0)
                    .fold(upper.y1, |y, r| y.max(r.y1))
            });
            let above = (b.y0 - own_above).max(0.0);
            if let Some((o, below)) = lower {
                if below <= above {
                    // Framed as the frame rule frames it: every label it took
                    // is inside the frame, or a caption or a note.
                    let framed_out = figures[o].frame.is_some_and(|fr| {
                        let outside = |t: usize| {
                            share_inside(&blocks[t].0, &fr) < MARK_INSIDE
                                && !crate::heading::is_caption(blocks[t].2)
                                && !note_label(blocks[t].2)
                        };
                        outside(k) && !taken[o].iter().any(|&t| outside(t))
                    });
                    moves.push((k, if framed_out { usize::MAX } else { o }));
                }
            }
        }
        for (k, o) in moves {
            taken[a].retain(|&t| t != k);
            if o != usize::MAX && !taken[o].contains(&k) {
                taken[o].push(k);
                taken[o].sort_unstable();
            }
        }
    }
}

/// Most space, in lines, between a framed figure's caption or note and the
/// frame, or the caption before it.
const CAPTION_GAP: f64 = 2.5;

/// `Note:`, `Notes:`, `Source:`, `Sources:`: a figure's note, under it.
fn note_label(text: &str) -> bool {
    let first = text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        first.trim_end_matches(['.', ':']),
        "note" | "notes" | "source" | "sources"
    ) && first.ends_with([':', '.'])
}

/// Least share of a frame's width and height its drawings span: a wiring
/// diagram's box holds the diagrams and their labels around them.
const FRAME_FILL: f64 = 0.6;

/// Share of the page, each way, from which a box is the page's background
/// rather than a frame on it.
const FRAME_PAGE: f64 = 0.9;

/// Size, against the page's prose, from which text is display type: a
/// title, not a frame's label.
const FRAME_DISPLAY: f32 = 1.3;

/// Pictures with labels set over them, as figures: a product's cutaway
/// with its layers named on it (`Heat Sealable PLA Layer`, `PLA Core`).
/// Drawn as an image, it draws no shapes, and its labels were read as the
/// page's text, in whatever section the reading order put them.
///
/// A picture is one when two blocks or more lie on it, every block over it
/// is a label (a few words, no display type), and they do not all start
/// from one left edge. A photo with a title or a paragraph set over it, a
/// slide's or a brochure's background, is not; nor is a signature under the
/// name it signs, a picture the size of the page, or one a drawing already
/// covers.
pub fn pictures(
    images: &[BBox],
    blocks: &[(BBox, f32, &str)],
    page: usize,
    (page_width, page_height): (f64, f64),
    prose: f32,
    figures: &[Figure],
) -> Vec<Figure> {
    images
        .iter()
        .filter(|im| {
            let page_scale = page_width > 0.0
                && page_height > 0.0
                && im.width() / page_width > MAX_PAGE_FRACTION
                && im.height() / page_height > MAX_PAGE_FRACTION;
            let on: Vec<&(BBox, f32, &str)> = blocks
                .iter()
                .filter(|(b, ..)| share_inside(b, im) >= MARK_INSIDE)
                .collect();
            !page_scale
                && im.width().min(im.height()) >= MIN_SIDE
                && im.width() * im.height() >= MIN_AREA
                && !figures.iter().any(|f| {
                    share_inside(&f.drawing, im) > 0.0 || share_inside(im, &f.drawing) > 0.0
                })
                && on.len() >= 2
                // Every block over it, wholly or in part, is a label: a
                // brochure's paragraphs run on and off the picture behind
                // them, and its headings over it are theirs.
                && blocks
                    .iter()
                    .filter(|(b, ..)| share_inside(b, im) > 0.0)
                    .all(|(_, size, text)| picture_label(*size, text, prose))
                // Labels lie about the picture, each by what it names. Lines
                // set one under another from one left edge are the page's
                // text over a picture under it: a signature over the line
                // and the name it signs.
                && !on.iter().all(|(b, size, _)| {
                    (b.x0 - on[0].0.x0).abs() <= *size as f64 * PICTURE_EDGE
                })
        })
        .map(|im| Figure {
            bbox: *im,
            drawing: *im,
            page,
            shapes: 0,
            frame: None,
            axes: false,
        })
        .collect()
}

/// Fewest lines a plot's graticule draws each way.
const GRATICULE_LINES: usize = 4;

/// Widest difference between two pitches of one graticule, as a share of
/// its pitch: the rounding of where its lines are drawn, well short of the
/// differing heights of a table's rows.
const GRATICULE_PITCH: f64 = 0.1;

/// Largest share of a graticule's cells that hold text: a plot's curve
/// labels and notes stand in under a third of them, each crossing a line
/// or two, where a ruled table's text fills over half its cells.
const GRATICULE_TEXT: f64 = 0.4;

/// Do the places, ascending, stand a pitch apart: a graticule's lines or an
/// axis's ticks? Each gap is the pitch or a whole number of it (a line broken
/// off for a label, a tick under a label left out), most of them the pitch
/// itself, within [`GRATICULE_PITCH`] of it.
fn evenly_pitched(at: &[f64]) -> bool {
    if at.len() < 2 {
        return false;
    }
    let pitches: Vec<f64> = at.windows(2).map(|w| w[1] - w[0]).collect();
    let mut sorted = pitches.clone();
    sorted.sort_by(f64::total_cmp);
    let pitch = sorted[sorted.len() / 2];
    let near = |p: f64, k: f64| (p - k * pitch).abs() <= pitch * GRATICULE_PITCH;
    let whole = |p: f64| {
        let k = (p / pitch).round();
        k >= 1.0 && near(p, k)
    };
    let at_pitch = pitches.iter().filter(|p| near(**p, 1.0)).count();
    pitch > 0.0 && pitches.iter().all(|p| whole(*p)) && at_pitch * 2 >= pitches.len()
}

/// Fewest ticks an axis carries, each way.
const AXIS_TICKS: usize = 3;

/// Longest tick, in the page's text size: a mark beside a value, short of
/// any table's rule.
const TICK_LENGTH: f64 = 1.2;

/// Narrowest trace, in the page's text size, a plot is read from: an arrow
/// in a diagram or a check mark is narrower.
const TRACE_WIDTH: f64 = 3.0;

/// Least share of a trace's segments running one way across the page.
const TRACE_ONWARD: f64 = 0.9;

/// Farthest, in the page's text size, an axis or its ticks stand from the
/// trace they measure.
const TRACE_AXIS_REACH: f64 = 3.0;

/// Plots drawn as lines with no graticule: a trace (a stroked line of
/// diagonal segments, see [`crate::rule::Trace`]) bound by an axis. The axis
/// is a rule along the trace's foot carrying [`AXIS_TICKS`] or more ticks at
/// an even pitch, or a column of such ticks beside it, a scale's marks, or
/// two rules meeting at the plot's corner. A table's rules, a statement's
/// underlines and a form's boxes run square and draw no trace; a line of
/// diagonal segments with no axis by it (a map's border, a diagram's arrow)
/// is no plot either.
pub fn axis_plots(
    rules: &[Rule],
    marks: &[Rule],
    traces: &[crate::rule::Trace],
    glyphs: &[Glyph],
    page: usize,
    figures: &[Figure],
) -> Vec<Figure> {
    use crate::rule::Orientation;
    let em = {
        let mut sizes: Vec<f32> = glyphs
            .iter()
            .filter(|g| g.bbox.is_some())
            .map(|g| g.font_size)
            .collect();
        sizes.sort_by(f32::total_cmp);
        sizes.get(sizes.len() / 2).copied().unwrap_or(10.0).max(1.0) as f64
    };
    let len = |r: &Rule| match r.orientation {
        Orientation::Horizontal => r.bbox.width(),
        Orientation::Vertical => r.bbox.height(),
    };
    let ticks: Vec<&Rule> = rules
        .iter()
        .chain(marks)
        .filter(|r| len(r) <= em * TICK_LENGTH && len(r) >= em * 0.15)
        .collect();
    // Places of a row or a column of ticks, ascending, deduplicated.
    let places = |mut at: Vec<f64>| {
        at.sort_by(f64::total_cmp);
        at.dedup_by(|a, b| (*a - *b).abs() <= 0.5);
        at
    };
    let mut plots: Vec<BBox> = Vec::new();
    // A series plotted against its axis runs one way across the page; an
    // outline, a symbol or an arrow turns back on itself.
    let series = |t: &&crate::rule::Trace| {
        t.bbox.width() >= em * TRACE_WIDTH && t.onward as f64 >= t.segments as f64 * TRACE_ONWARD
    };
    for t in traces.iter().filter(series) {
        let b = t.bbox;
        let reach = em * TRACE_AXIS_REACH;
        let mut region: Option<BBox> = None;
        // A rule along its foot, spanning it, ticked at an even pitch.
        for h in rules.iter().filter(|r| {
            r.orientation == Orientation::Horizontal
                && r.axis_pos() >= b.y1 - 1.0
                && r.axis_pos() - b.y1 <= reach
                && r.bbox.x0 <= b.x0 + em
                && r.bbox.x1 >= b.x1 - em
        }) {
            let y = h.axis_pos();
            let at = places(
                ticks
                    .iter()
                    .filter(|k| {
                        k.orientation == Orientation::Vertical
                            && k.bbox.y0 <= y + 1.0
                            && k.bbox.y1 >= y - 1.0
                            && k.bbox.x0 >= h.bbox.x0 - 1.0
                            && k.bbox.x1 <= h.bbox.x1 + 1.0
                    })
                    .map(|k| k.axis_pos())
                    .collect(),
            );
            if at.len() >= AXIS_TICKS && evenly_pitched(&at) {
                region = Some(region.map_or(b.union(&h.bbox), |r: BBox| r.union(&h.bbox)));
            }
        }
        // A column of ticks beside it, a scale's marks, at an even pitch.
        let mut columns: Vec<(f64, Vec<&Rule>)> = Vec::new();
        for k in ticks
            .iter()
            .filter(|k| k.orientation == Orientation::Horizontal)
        {
            let kb = k.bbox;
            let beside = (kb.x1 <= b.x0 + 1.0 && b.x0 - kb.x1 <= reach)
                || (kb.x0 >= b.x1 - 1.0 && kb.x0 - b.x1 <= reach);
            if !beside || kb.y0 < b.y0 - reach || kb.y1 > b.y1 + reach {
                continue;
            }
            match columns.iter_mut().find(|(x, _)| (x - kb.x0).abs() <= 1.0) {
                Some((_, v)) => v.push(k),
                None => columns.push((kb.x0, vec![k])),
            }
        }
        for (_, col) in &columns {
            let at = places(col.iter().map(|k| k.axis_pos()).collect());
            if at.len() >= AXIS_TICKS && evenly_pitched(&at) {
                let span = col.iter().fold(b, |r, k| r.union(&k.bbox));
                region = Some(region.map_or(span, |r: BBox| r.union(&span)));
            }
        }
        // Two rules meeting at the plot's corner, along its foot and side.
        if region.is_none() {
            let foot = rules.iter().find(|r| {
                r.orientation == Orientation::Horizontal
                    && r.axis_pos() >= b.y1 - 1.0
                    && r.axis_pos() - b.y1 <= reach
                    && r.bbox.x0 <= b.x0 + em
                    && r.bbox.x1 >= b.x1 - em
            });
            let side = rules.iter().find(|r| {
                r.orientation == Orientation::Vertical
                    && r.bbox.y0 <= b.y0 + em
                    && r.bbox.y1 >= b.y1 - em
                    && ((r.axis_pos() <= b.x0 + 1.0 && b.x0 - r.axis_pos() <= reach)
                        || (r.axis_pos() >= b.x1 - 1.0 && r.axis_pos() - b.x1 <= reach))
            });
            if let (Some(f), Some(s)) = (foot, side) {
                let corner = (f.bbox.x0 - s.axis_pos()).abs() <= 2.0
                    || (f.bbox.x1 - s.axis_pos()).abs() <= 2.0;
                let meets = (s.bbox.y1 - f.axis_pos()).abs() <= 2.0;
                if corner && meets {
                    region = Some(b.union(&f.bbox).union(&s.bbox));
                }
            }
        }
        // The box its axes bound is the plot's drawing: an axis standing at
        // its side, along the trace, rises to the scale's top value, past
        // the series' highest point.
        if let Some(r) = region.as_mut() {
            let bound = *r;
            for s in rules.iter().filter(|s| {
                s.orientation == Orientation::Vertical
                    && ((s.axis_pos() - bound.x0).abs() <= 2.0
                        || (s.axis_pos() - bound.x1).abs() <= 2.0)
                    && s.bbox.y0 <= b.y0 + em
                    && s.bbox.y1 >= b.y1 - em
                    && s.bbox.y0 >= bound.y0 - reach
                    && s.bbox.y1 <= bound.y1 + reach
            }) {
                *r = r.union(&s.bbox);
            }
        }
        if let Some(r) = region {
            match plots
                .iter_mut()
                .find(|p| share_inside(&r, p) > 0.0 || share_inside(p, &r) > 0.0)
            {
                Some(p) => *p = p.union(&r),
                None => plots.push(r),
            }
        }
    }
    plots
        .into_iter()
        .filter(|p| {
            !figures
                .iter()
                .any(|f| share_inside(&f.drawing, p) > 0.0 || share_inside(p, &f.drawing) > 0.0)
        })
        .map(|p| Figure {
            bbox: p,
            drawing: p,
            page,
            shapes: traces
                .iter()
                .filter(|t| share_inside(&t.bbox, &p) > 0.5)
                .count()
                .max(1),
            frame: None,
            axes: true,
        })
        .collect()
}

/// Fewest bars a chart is read from.
const CHART_BARS: usize = 3;

/// Least run of a chart's bars across, in the page's text size: a signal
/// icon's bars stand within an em or two.
const BAR_RUN: f64 = 3.0;

/// Least length of a chart's longest bar, in the page's text size.
const BAR_LENGTH: f64 = 2.0;

/// Share of a chart's bars that stand on its base or hang from it.
const BAR_BASE_SHARE: f64 = 0.8;

/// A chart's bars, at most one in this many with text on or against it.
const BAR_WORDED: usize = 3;

/// Pieces to a stack, on average, from which stacks whose breaks line up are
/// a table's columns of cells: a stacked bar holds a piece to a series.
const CELL_PIECES: f64 = 4.0;

/// Stacks that are a table's columns of cells: most breaks between their
/// pieces (where one ends and the next starts) stand level with a break in
/// half the other stacks or more, at the table's rows. A stacked bar's
/// pieces end at their values.
fn cells<'a>(stacks: impl Iterator<Item = &'a [BBox]>) -> bool {
    // Each break, and the stack it is in, along the bars.
    let mut breaks: Vec<(f64, usize)> = Vec::new();
    let mut broken = 0usize;
    for (i, stack) in stacks.enumerate() {
        let mut v = stack.to_vec();
        v.sort_by(|a, b| a.y0.total_cmp(&b.y0));
        let before = breaks.len();
        breaks.extend(v.windows(2).map(|p| (p[0].y1, i)));
        broken += usize::from(breaks.len() > before);
    }
    if breaks.is_empty() || broken < 2 {
        return false;
    }
    breaks.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut level = 0usize;
    for (k, &(y, i)) in breaks.iter().enumerate() {
        let mut with: Vec<usize> = Vec::new();
        for &(e, j) in breaks[..k].iter().rev().take_while(|(e, _)| y - e <= 0.5) {
            if j != i && (y - e).abs() <= 0.5 {
                with.push(j);
            }
        }
        for &(_, j) in breaks[k + 1..].iter().take_while(|(e, _)| e - y <= 0.5) {
            if j != i {
                with.push(j);
            }
        }
        with.sort_unstable();
        with.dedup();
        if with.len() * 2 >= broken - 1 {
            level += 1;
        }
    }
    level * 2 > breaks.len()
}

/// Bar charts drawn as fills: [`CHART_BARS`] or more bars of one width at
/// an even pitch, each standing on one base or hanging from it, of lengths
/// that differ, bound by an axis: the base drawn as a rule across them, a
/// column of ticks beside them at an even pitch, or a scale's numbers
/// beside them. Bars set across the page likewise, turned. A table's
/// shading bands its rows at one length, a rating's cells are one size,
/// and an icon's bars have no axis.
pub fn bar_plots(
    fills: &[crate::rule::Fill],
    rules: &[Rule],
    marks: &[Rule],
    glyphs: &[Glyph],
    page: usize,
    figures: &[Figure],
) -> Vec<Figure> {
    use crate::rule::Orientation;
    let em = {
        let mut sizes: Vec<f32> = glyphs
            .iter()
            .filter(|g| g.bbox.is_some())
            .map(|g| g.font_size)
            .collect();
        sizes.sort_by(f32::total_cmp);
        sizes.get(sizes.len() / 2).copied().unwrap_or(10.0).max(1.0) as f64
    };
    let said = numbers_said(glyphs);
    // Each fill once: many producers fill a shape and stroke it again.
    let mut drawn: Vec<BBox> = fills
        .iter()
        .map(|f| f.bbox)
        .filter(|b| b.width() >= 0.5 && b.height() >= 0.5)
        .collect();
    drawn.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let mut boxes: Vec<BBox> = Vec::new();
    for b in drawn {
        let again = boxes
            .iter()
            .rev()
            .take_while(|o| b.x0 - o.x0 < 0.5)
            .any(|o| {
                (o.y0 - b.y0).abs() < 0.5 && (o.x1 - b.x1).abs() < 0.5 && (o.y1 - b.y1).abs() < 0.5
            });
        if !again {
            boxes.push(b);
        }
    }
    let len = |r: &Rule| match r.orientation {
        Orientation::Horizontal => r.bbox.width(),
        Orientation::Vertical => r.bbox.height(),
    };
    let ticks: Vec<&Rule> = rules
        .iter()
        .chain(marks)
        .filter(|r| len(r) <= em * TICK_LENGTH && len(r) >= em * 0.15)
        .collect();
    let mut plots: Vec<BBox> = Vec::new();
    for turned in [false, true] {
        // Bars across the page are read as bars up it, turned.
        let t = |b: &BBox| {
            if turned {
                BBox {
                    x0: b.y0,
                    y0: b.x0,
                    x1: b.y1,
                    y1: b.x1,
                }
            } else {
                *b
            }
        };
        let along = if turned {
            Orientation::Vertical
        } else {
            Orientation::Horizontal
        };
        let bars: Vec<BBox> = boxes.iter().map(t).collect();
        let mut order: Vec<usize> = (0..bars.len()).collect();
        order.sort_by(|&a, &b| bars[a].width().total_cmp(&bars[b].width()));
        let mut k = 0;
        while k < order.len() {
            let w = bars[order[k]].width();
            let mut j = k;
            while j < order.len() && bars[order[j]].width() - w <= 0.5 {
                j += 1;
            }
            let same: Vec<BBox> = order[k..j].iter().map(|&i| bars[i]).collect();
            k = j;
            if same.len() < CHART_BARS {
                continue;
            }
            // Its bars: the pieces at one place across that abut, each
            // ending where the next starts, a stack. Pieces a gap apart are
            // bars of their own: a chart set over another, its bars at the
            // same places, or a form's boxes in two rows.
            let mut sorted = same.clone();
            sorted.sort_by(|a, b| a.x0.total_cmp(&b.x0).then(a.y0.total_cmp(&b.y0)));
            let mut columns: Vec<(f64, Vec<BBox>)> = Vec::new();
            for b in sorted {
                match columns.last_mut() {
                    Some((x, v))
                        if (b.x0 - *x).abs() <= 0.5
                            && b.y0 <= v.iter().fold(f64::MIN, |e, o| e.max(o.y1)) + 1.0 =>
                    {
                        v.push(b)
                    }
                    _ => columns.push((b.x0, vec![b])),
                }
            }
            let extent = |v: &[BBox]| {
                v.iter()
                    .fold((f64::MAX, f64::MIN), |(a, b), o| (a.min(o.y0), b.max(o.y1)))
            };
            // A chart to a band of bars level with one another, and along
            // it a chart to a run: a gap past a few pitches starts another.
            let mut by_top: Vec<usize> = (0..columns.len()).collect();
            by_top.sort_by(|&a, &b| extent(&columns[a].1).0.total_cmp(&extent(&columns[b].1).0));
            let mut bands: Vec<Vec<usize>> = Vec::new();
            let mut low = f64::MIN;
            for c in by_top {
                let (y0, y1) = extent(&columns[c].1);
                match bands.last_mut() {
                    Some(band) if y0 <= low + 1.0 => band.push(c),
                    _ => bands.push(vec![c]),
                }
                low = if bands.last().unwrap().len() == 1 {
                    y1
                } else {
                    low.max(y1)
                };
            }
            let mut runs: Vec<Vec<usize>> = Vec::new();
            for mut band in bands {
                band.sort_by(|&a, &b| columns[a].0.total_cmp(&columns[b].0));
                let mut run: Vec<usize> = Vec::new();
                for c in band {
                    if let Some(&p) = run.last() {
                        if columns[c].0 - columns[p].0 > w * 2.0 + em * 4.0 {
                            runs.push(std::mem::take(&mut run));
                        }
                    }
                    run.push(c);
                }
                runs.push(run);
            }
            for run in runs.iter().filter(|r| r.len() >= CHART_BARS) {
                let xs: Vec<f64> = run.iter().map(|&c| columns[c].0).collect();
                if !evenly_pitched(&xs) || xs[1] - xs[0] < w - 0.5 {
                    continue;
                }
                // A stack holds a piece to a series, a few, each ending at its
                // value; a table's column of cells a piece to a row, row after
                // row, ending at its rows, the same in every column. Where the
                // stacks hold many pieces and most of their breaks line up
                // across them, they are cells. Few pieces may line up by their
                // values alone: a histogram's counts are whole.
                let pieces_each = run.iter().map(|&c| columns[c].1.len()).sum::<usize>() as f64
                    / run.len() as f64;
                if pieces_each >= CELL_PIECES && cells(run.iter().map(|&c| &columns[c].1[..])) {
                    continue;
                }
                // The base: an edge most bars share, standing on it or
                // hanging from it.
                // Most bars share it, so one of the first few bars has it.
                let first = run.len() - (run.len() as f64 * BAR_BASE_SHARE).ceil() as usize + 1;
                let edges: Vec<f64> = run
                    .iter()
                    .take(first.max(1))
                    .flat_map(|&c| columns[c].1.iter().flat_map(|b| [b.y0, b.y1]))
                    .collect();
                let support = |y: f64| {
                    run.iter()
                        .filter(|&&c| {
                            columns[c]
                                .1
                                .iter()
                                .any(|b| (b.y0 - y).abs() <= 1.0 || (b.y1 - y).abs() <= 1.0)
                        })
                        .count()
                };
                let Some(base) = edges
                    .iter()
                    .copied()
                    .max_by_key(|&y| support(y))
                    .filter(|&y| support(y) as f64 >= run.len() as f64 * BAR_BASE_SHARE)
                else {
                    continue;
                };
                // Of lengths that differ, the longest of a chart's size.
                let lengths: Vec<f64> = run
                    .iter()
                    .map(|&c| {
                        let (y0, y1) = extent(&columns[c].1);
                        y1 - y0
                    })
                    .collect();
                let longest = lengths.iter().copied().fold(0.0, f64::max);
                let differ = lengths.iter().any(|l| (l - lengths[0]).abs() > 1.0);
                let hull = run
                    .iter()
                    .flat_map(|&c| columns[c].1.iter())
                    .fold(columns[run[0]].1[0], |r, b| r.union(b));
                if !differ || longest < em * BAR_LENGTH || hull.width() < em * BAR_RUN {
                    continue;
                }
                // Bare: a chart's bars carry a value here and there, a
                // table's shading its rows' text, and an underline sits
                // under a figure.
                let around = t(&hull);
                let inked: Vec<(f64, f64)> = glyphs
                    .iter()
                    .filter(|g| !g.text.trim().is_empty())
                    .filter_map(|g| g.bbox)
                    .map(|gb| ((gb.x0 + gb.x1) / 2.0, (gb.y0 + gb.y1) / 2.0))
                    .filter(|&(x, y)| {
                        x >= around.x0 - em
                            && x <= around.x1 + em
                            && y >= around.y0 - em
                            && y <= around.y1 + em
                    })
                    .collect();
                let worded = run
                    .iter()
                    .flat_map(|&c| columns[c].1.iter())
                    .filter(|b| {
                        let o = t(b);
                        inked.iter().any(|&(x, y)| {
                            x >= o.x0 && x <= o.x1 && y >= o.y0 - em * 0.5 && y <= o.y1 + em * 0.5
                        })
                    })
                    .count();
                let pieces = run.iter().map(|&c| columns[c].1.len()).sum::<usize>();
                if worded * BAR_WORDED > pieces {
                    continue;
                }
                let mut region = hull;
                let mut bound = false;
                // The base drawn as a rule across the bars.
                for r in rules.iter().filter(|r| r.orientation == along) {
                    let rb = t(&r.bbox);
                    let at = (rb.y0 + rb.y1) / 2.0;
                    if (at - base).abs() <= 1.5 && rb.x0 <= hull.x0 + em && rb.x1 >= hull.x1 - em {
                        region = region.union(&rb);
                        bound = true;
                    }
                }
                // A column of ticks beside the bars, at an even pitch.
                let reach = em * TRACE_AXIS_REACH;
                let mut cols: Vec<(f64, Vec<BBox>)> = Vec::new();
                for k in ticks.iter().filter(|k| k.orientation == along) {
                    let kb = t(&k.bbox);
                    let beside = (kb.x1 <= hull.x0 + 1.0 && hull.x0 - kb.x1 <= reach)
                        || (kb.x0 >= hull.x1 - 1.0 && kb.x0 - hull.x1 <= reach);
                    if !beside || kb.y0 < hull.y0 - reach || kb.y1 > hull.y1 + reach {
                        continue;
                    }
                    match cols.iter_mut().find(|(x, _)| (x - kb.x0).abs() <= 1.0) {
                        Some((_, v)) => v.push(kb),
                        None => cols.push((kb.x0, vec![kb])),
                    }
                }
                for (_, col) in &cols {
                    let mut at: Vec<f64> = col.iter().map(|k| (k.y0 + k.y1) / 2.0).collect();
                    at.sort_by(f64::total_cmp);
                    at.dedup_by(|a, b| (*a - *b).abs() <= 0.5);
                    if at.len() >= AXIS_TICKS && evenly_pitched(&at) {
                        region = col.iter().fold(region, |r, k| r.union(k));
                        bound = true;
                    }
                }
                let back = |b: &BBox| t(b);
                // A scale beside them: its numbers in a column along the
                // bars, stepping evenly. A table's figures step unevenly.
                if !bound {
                    let reach = em * AXIS_REACH;
                    let near: Vec<(BBox, &str)> = said
                        .iter()
                        .map(|(b, s)| (t(b), s.as_str()))
                        .filter(|(b, _)| {
                            let beside = (b.x1 <= hull.x0 + 1.0 && hull.x0 - b.x1 <= reach)
                                || (b.x0 >= hull.x1 - 1.0 && b.x0 - hull.x1 <= reach);
                            beside && b.y1 >= hull.y0 - reach && b.y0 <= hull.y1 + reach
                        })
                        .collect();
                    for (a, _) in &near {
                        let mut column: Vec<&(BBox, &str)> = near
                            .iter()
                            .filter(|(n, _)| {
                                (n.x1 - a.x1).abs() <= 2.0 || (n.x0 - a.x0).abs() <= 2.0
                            })
                            .collect();
                        column.sort_by(|p, q| p.0.y0.total_cmp(&q.0.y0));
                        let values: Vec<String> = column
                            .iter()
                            .map(|(_, s)| s.replace(['%', '$'], ""))
                            .collect();
                        if column.len() >= AXIS_VALUES && crate::document::axis_scale(&values) {
                            bound = true;
                            break;
                        }
                    }
                }
                if !bound {
                    continue;
                }
                let r = back(&region);
                match plots
                    .iter_mut()
                    .find(|p| share_inside(&r, p) > 0.0 || share_inside(p, &r) > 0.0)
                {
                    Some(p) => *p = p.union(&r),
                    None => plots.push(r),
                }
            }
        }
    }
    plots
        .into_iter()
        .filter(|p| {
            !figures
                .iter()
                .any(|f| share_inside(&f.drawing, p) > 0.0 || share_inside(p, &f.drawing) > 0.0)
        })
        .map(|p| Figure {
            bbox: p,
            drawing: p,
            page,
            shapes: boxes
                .iter()
                .filter(|b| share_inside(b, &p) > 0.5)
                .count()
                .max(1),
            frame: None,
            axes: true,
        })
        .collect()
}

/// Plots drawn as lines: a graticule of rules each spanning the plot, four
/// or more each way, evenly pitched (a line broken off for a label leaving
/// its neighbours whole pitches apart), with text in few of its cells. A
/// scope's trace or a log plot's curves draw no fills, and their grids
/// were read as tables of the notes set in them (`RA = 9.1 kΩ`). A ruled
/// table holds its text in most cells, and its rows and columns differ.
pub fn graticules(
    rules: &[Rule],
    glyphs: &[Glyph],
    page: usize,
    (page_width, page_height): (f64, f64),
    figures: &[Figure],
) -> Vec<Figure> {
    use crate::rule::Orientation;
    let along = |o: Orientation| -> Vec<BBox> {
        rules
            .iter()
            .filter(|r| r.orientation == o)
            .map(|r| r.bbox)
            .collect()
    };
    let (hs, vs) = (along(Orientation::Horizontal), along(Orientation::Vertical));
    // Rules sharing one extent, the lines of one graticule, by their place.
    let sets = |lines: &[BBox], horizontal: bool| -> Vec<(BBox, Vec<f64>)> {
        let mut out: Vec<(BBox, Vec<f64>)> = Vec::new();
        for l in lines {
            let (a, b, at) = if horizontal {
                (l.x0, l.x1, (l.y0 + l.y1) * 0.5)
            } else {
                (l.y0, l.y1, (l.x0 + l.x1) * 0.5)
            };
            let span = |r: &BBox| {
                if horizontal {
                    (r.x0, r.x1)
                } else {
                    (r.y0, r.y1)
                }
            };
            match out.iter_mut().find(|(r, _)| {
                let (p, q) = span(r);
                (p - a).abs() <= 1.5 && (q - b).abs() <= 1.5
            }) {
                Some((r, at_list)) => {
                    *r = r.union(l);
                    at_list.push(at);
                }
                None => out.push((*l, vec![at])),
            }
        }
        for (_, at) in out.iter_mut() {
            at.sort_by(f64::total_cmp);
            at.dedup_by(|p, q| (*p - *q).abs() <= 1.0);
        }
        out.into_iter()
            .filter(|(_, at)| at.len() >= GRATICULE_LINES)
            .collect()
    };
    // Lines a pitch apart, or a whole number of pitches where a line breaks
    // off for a label or a trace and so spans less than the rest; most of
    // the gaps at the pitch itself.
    let even = |at: &[f64]| evenly_pitched(at);
    let mut out = Vec::new();
    for (hbox, hat) in sets(&hs, true) {
        for (vbox, vat) in sets(&vs, false) {
            // One lattice: the verticals span the horizontals' height, and
            // these the verticals' width.
            let lattice = hbox.union(&vbox);
            let fits = (vbox.y0 - hbox.y0).abs() <= 2.0
                && (vbox.y1 - hbox.y1).abs() <= 2.0
                && (hbox.x0 - vbox.x0).abs() <= 2.0
                && (hbox.x1 - vbox.x1).abs() <= 2.0;
            if !fits || !even(&hat) || !even(&vat) {
                continue;
            }
            let page_scale = page_width > 0.0
                && page_height > 0.0
                && lattice.width() / page_width > MAX_PAGE_FRACTION
                && lattice.height() / page_height > MAX_PAGE_FRACTION;
            if page_scale
                || figures
                    .iter()
                    .any(|f| share_inside(&f.drawing, &lattice) > 0.0)
                || out
                    .iter()
                    .any(|f: &Figure| share_inside(&f.drawing, &lattice) > 0.0)
            {
                continue;
            }
            let cells = (hat.len() - 1) * (vat.len() - 1);
            let mut held = vec![false; cells];
            for g in glyphs {
                let Some(b) = g.bbox else { continue };
                if g.text.trim().is_empty() {
                    continue;
                }
                let (x, y) = ((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5);
                let row = hat.windows(2).position(|w| y > w[0] && y < w[1]);
                let col = vat.windows(2).position(|w| x > w[0] && x < w[1]);
                if let (Some(r), Some(c)) = (row, col) {
                    held[r * (vat.len() - 1) + c] = true;
                }
            }
            let text = held.iter().filter(|h| **h).count() as f64 / cells as f64;
            if text > GRATICULE_TEXT {
                continue;
            }
            out.push(Figure {
                bbox: lattice,
                drawing: lattice,
                page,
                shapes: hat.len() + vat.len(),
                frame: None,
                axes: false,
            });
        }
    }
    out
}

/// Widest difference, in ems, between the left edges of lines set as one
/// column.
const PICTURE_EDGE: f64 = 0.5;

/// A label as a picture's: a few words, no display type.
fn picture_label(size: f32, text: &str, prose: f32) -> bool {
    text.split_whitespace().count() <= LABEL_MAX_WORDS && size < prose * FRAME_DISPLAY
}

/// Share of the page's height, at its top, its running heads stand in.
const PAGE_BAND: f64 = 0.08;

/// Lines, at most, between a drawing and a title over it in the page's head
/// band.
const BAND_TITLE_LINES: f64 = 2.0;

/// Is the text in `b` carried on, its sentence unfinished, into the page's
/// prose set within a line under it: more words than a label says, its
/// first letter a small one? A chart's label wrapped onto a second line
/// goes on in a word or two, and a panel's caption opens on its letter,
/// `(a)`.
fn runs_into_lower(b: &BBox, size: f32, blocks: &[(BBox, f32, &str)]) -> bool {
    let line = (size as f64).max(1.0);
    blocks.iter().any(|(o, osize, text)| {
        (osize - size).abs() <= 0.5
            && (0.0..=line).contains(&(o.y0 - b.y1))
            && o.x1.min(b.x1) > o.x0.max(b.x0)
            && text.chars().next().is_some_and(char::is_lowercase)
            && text.split_whitespace().count() > LABEL_MAX_WORDS
    })
}

/// Are two blocks one run of text: set at one size from one left edge, one
/// under the other with no more than a paragraph's space between?
fn runs_on(a: &BBox, a_size: f32, b: &BBox, b_size: f32) -> bool {
    let size = a_size.max(b_size) as f64;
    let gap = (b.y0 - a.y1).max(a.y0 - b.y1);
    (a_size - b_size).abs() <= RUN_SIZE
        && (a.x0 - b.x0).abs() <= RUN_ALIGN
        && gap >= -size * 0.5
        && gap <= size * RUN_GAP
}

/// Most space, in lines, between text in a column beside a drawing and the
/// page's paragraph it is set with.
const COLUMN_GAP: f64 = 2.5;

/// Fewest words in a line of running text, as against a label's.
const RUN_WORDS: usize = 3;

/// The words `text` says.
fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// `text` ends a sentence: a full stop, a question or exclamation mark, or
/// a colon, inside any closing bracket or quote.
fn ends_sentence(text: &str) -> bool {
    text.trim_end()
        .trim_end_matches(['"', '\'', '\u{201D}', '\u{2019}', ')'])
        .ends_with(['.', '!', '?', ':'])
}

/// `text` opens in lower case: its first letter is small.
fn opens_lower(text: &str) -> bool {
    text.trim_start()
        .chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(char::is_lowercase)
}

/// How far apart, in points, two blocks' sizes and left edges may be and the
/// blocks still be one run.
const RUN_SIZE: f32 = 0.5;
const RUN_ALIGN: f64 = 1.5;

/// Most space between two blocks of one run, in their font size: a
/// paragraph's gap, not a section's.
const RUN_GAP: f64 = 1.5;

/// Most words a label says. A chart's labels are terse — a region's name, a
/// value, a tick, a source — and a sentence or a paragraph beside a drawing
/// is the page's, however small it is set: a data sheet's disclaimer in
/// 6pt beside a logo, a footnote under a drawing.
const LABEL_MAX_WORDS: usize = 15;

/// Furthest a label reached through other labels may lie from its drawing,
/// in reaches: the drawing's own size on its narrower side.
const CHAIN_LIMIT: f64 = 2.0;

/// Largest size, against the page's prose, at which text is a drawing's
/// label wherever it stands among the others.
const LABEL_SIZE: f32 = 0.95;

/// Widest, against its drawing, text at the prose's size may be and still
/// be the drawing's label.
const PROSE_LABEL_WIDTH: f64 = 1.5;

/// Furthest, in its own ems, text may stand from a logo's lettering on its
/// row and be set beside it.
const MARK_REACH: f64 = 3.0;

/// Is the drawing a photograph's numbered callouts, rather than a chart?
///
/// A product sheet numbers the parts of its photo with circles drawn over
/// the picture, and they cluster as a chart's shapes do. But the photo is
/// the figure and the numbers in the circles its only labels. Grown by a
/// chart's reach, the region took the page's title and the heading set over
/// the photo, set as the page's other headings are, and the key set under
/// it.
///
/// Callouts are drawn on a picture: photographs lie under most of the
/// drawing. And a callout says nothing but its mark: every block inside the
/// drawing is a number of one or two figures or a single letter. A
/// scientific figure's annotations over its panels say `sky` and `near
/// coast`, and a chart drawn over a picture sets values and years in it, so
/// both reach for their labels as charts do.
fn callouts(drawing: &BBox, blocks: &[(BBox, f32, &str)], photos: &[BBox]) -> bool {
    let inside: Vec<&str> = blocks
        .iter()
        .filter(|(b, ..)| share_inside(b, drawing) >= MARK_INSIDE)
        .map(|(_, _, t)| *t)
        .collect();
    inside.len() >= 2
        && inside.iter().all(|t| callout_mark(t))
        && photo_cover(drawing, photos) >= PHOTO_COVER
}

/// A callout's mark: `3`, `12`, `B`, `⑤`.
fn callout_mark(text: &str) -> bool {
    let t = text.trim();
    let n = t.chars().count();
    (1..=2).contains(&n) && t.chars().all(|c| c.is_ascii_digit())
        || (n == 1
            && t.chars()
                .all(|c| c.is_ascii_alphabetic() || ('\u{2460}'..='\u{2473}').contains(&c)))
}

/// Share of `b` inside `r`.
pub(crate) fn share_inside(b: &BBox, r: &BBox) -> f64 {
    let w = (b.x1.min(r.x1) - b.x0.max(r.x0)).max(0.0);
    let h = (b.y1.min(r.y1) - b.y0.max(r.y0)).max(0.0);
    w * h / (b.width() * b.height()).max(1e-9)
}

/// Share of the drawing that photographs lie under, counted once where
/// they overlap: sampled on a grid, since a figure's pictures are often
/// drawn in overlapping pieces.
fn photo_cover(drawing: &BBox, photos: &[BBox]) -> f64 {
    if drawing.width() <= 0.0 || drawing.height() <= 0.0 {
        return 0.0;
    }
    let steps = PHOTO_SAMPLES;
    let mut under = 0usize;
    for i in 0..steps {
        for j in 0..steps {
            let x = drawing.x0 + (i as f64 + 0.5) * drawing.width() / steps as f64;
            let y = drawing.y0 + (j as f64 + 0.5) * drawing.height() / steps as f64;
            if photos
                .iter()
                .any(|p| p.x0 <= x && x <= p.x1 && p.y0 <= y && y <= p.y1)
            {
                under += 1;
            }
        }
    }
    under as f64 / (steps * steps) as f64
}

/// Share of a drawing that photographs must lie under for it to be drawn on
/// them.
const PHOTO_COVER: f64 = 0.8;

/// Points to a side of the grid [`photo_cover`] samples.
const PHOTO_SAMPLES: usize = 20;

/// Share of a block inside a drawing for it to be one of the drawing's own
/// marks.
const MARK_INSIDE: f64 = 0.6;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::Orientation;

    fn bb(x0: f64, y0: f64, x1: f64, y1: f64) -> BBox {
        BBox { x0, y0, x1, y1 }
    }

    #[test]
    fn a_frame_drawn_round_a_diagram_bounds_its_figure() {
        // A wiring diagram in a box, a label in the box's margin left of it,
        // a note in the column beside the box, and the title over it.
        let fig = |frame: Option<BBox>| Figure {
            bbox: bb(115.0, 126.0, 418.0, 209.0),
            drawing: bb(115.0, 126.0, 418.0, 209.0),
            page: 0,
            shapes: 16,
            frame,
            axes: false,
        };
        let blocks = vec![
            (bb(48.0, 150.0, 70.0, 166.0), 8.0, "Line / Hot"),
            (
                bb(428.0, 120.0, 589.0, 150.0),
                8.0,
                "Note: Dimmer can be installed on the",
            ),
            (
                bb(46.0, 70.0, 260.0, 82.0),
                10.0,
                "4-Way Wiring (Using MA-R Companion Dimmer)",
            ),
        ];
        let frame = bb(46.0, 95.0, 421.0, 230.0);
        let mut figs = vec![fig(Some(frame))];
        let taken = attach(&mut figs, &blocks, &[], (612.0, 792.0), 8.0);
        assert_eq!(taken[0], [0], "{taken:?}");
        // Its caption under the box, and the line that runs on from it, are
        // the figure's.
        let mut captioned = blocks.clone();
        captioned.push((
            bb(46.0, 236.0, 400.0, 244.0),
            8.0,
            "Figure 3. Four-way wiring with a",
        ));
        captioned.push((bb(46.0, 246.0, 200.0, 254.0), 8.0, "companion dimmer"));
        let mut figs = vec![fig(Some(frame))];
        let taken = attach(&mut figs, &captioned, &[], (612.0, 792.0), 8.0);
        assert_eq!(taken[0], [0, 3, 4], "{taken:?}");
        // Unframed, reach takes the note beside the diagram, not the label
        // in the margin.
        let mut figs = vec![fig(None)];
        let taken = attach(&mut figs, &blocks, &[], (612.0, 792.0), 8.0);
        assert!(taken[0].contains(&1), "{taken:?}");
        // A page's background is no frame, nor one holding display type,
        // nor a plot's own box, filled by its drawing with nothing in its
        // margins.
        for (frame, blocks) in [
            (bb(0.0, 0.0, 612.0, 792.0), blocks.clone()),
            (frame, vec![(bb(48.0, 100.0, 300.0, 120.0), 24.0, "Wiring")]),
            (bb(114.0, 125.0, 419.0, 210.0), blocks.clone()),
        ] {
            let mut figs = vec![fig(Some(frame))];
            let framed = attach(&mut figs, &blocks, &[], (612.0, 792.0), 8.0);
            let mut figs = vec![fig(None)];
            let open = attach(&mut figs, &blocks, &[], (612.0, 792.0), 8.0);
            assert_eq!(framed, open, "{frame:?}");
        }
    }

    #[test]
    fn a_plot_with_a_spanning_background_is_still_a_figure() {
        // Nested plot areas as tall as they are wide, plus two markers.
        let f = fills(&[
            bb(132.0, 382.0, 524.0, 518.0),
            bb(141.0, 405.0, 503.0, 535.0),
            bb(152.0, 426.0, 515.0, 535.0),
            bb(293.0, 570.0, 299.0, 575.0),
            bb(327.0, 570.0, 331.0, 575.0),
        ]);
        assert_eq!(detect(&f, &[], &[], 0, (612.0, 792.0)).len(), 1);
    }

    #[test]
    fn a_stack_of_boxes_around_text_is_not_a_figure() {
        // A header panel, its accent bar, and an identification box below.
        let f = fills(&[
            bb(46.0, 100.0, 578.0, 163.0),
            bb(46.0, 100.0, 55.0, 163.0),
            bb(40.0, 171.0, 572.0, 216.0),
        ]);
        assert!(detect(&f, &[], &[], 0, (612.0, 792.0)).is_empty());
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
        let got = detect(&f, &[], &[], 17, (612.0, 792.0));
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
        assert!(detect(&f, &[], &[], 3, (612.0, 792.0)).is_empty());
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
        assert!(detect(&f, &rules, &[], 0, (612.0, 792.0)).is_empty());
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
        assert!(detect(&f, &[], &[], 0, (612.0, 792.0)).is_empty());
    }

    #[test]
    fn two_shapes_are_not_enough() {
        let f = fills(&[bb(10.0, 10.0, 40.0, 40.0), bb(50.0, 10.0, 80.0, 40.0)]);
        assert!(detect(&f, &[], &[], 0, (612.0, 792.0)).is_empty());
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
        assert_eq!(detect(&f, &[], &[], 0, (612.0, 792.0)).len(), 2);
    }

    /// A banner graphic across the top of a text page, as one report draws it:
    /// 585x138 of a 612x792 page.
    fn banner() -> Vec<Figure> {
        vec![Figure {
            bbox: bb(0.0, 18.0, 585.0, 156.0),
            drawing: bb(0.0, 18.0, 585.0, 156.0),
            page: 0,
            shapes: 43,
            frame: None,
            axes: false,
        }]
    }

    #[test]
    fn a_banner_does_not_walk_down_the_page() {
        // Short blocks every 40pt down the page — a column of headings and
        // captions. Each is within reach of the last, so absorption chains,
        // and the chain used to cross the whole page: the region reached
        // y=768 and every paragraph under the banner came out as a figure
        // fragment with no prose emitted at all.
        let blocks: Vec<(BBox, f32, &str)> = (0..16)
            .map(|i| {
                (
                    bb(
                        27.0,
                        200.0 + 40.0 * i as f64,
                        160.0,
                        210.0 + 40.0 * i as f64,
                    ),
                    7.0,
                    "caption",
                )
            })
            .collect();
        let mut figs = banner();
        attach(&mut figs, &blocks, &[], (612.0, 792.0), 10.0);
        let grown = figs[0].bbox;
        assert!(
            grown.height() / 792.0 <= MAX_PAGE_FRACTION,
            "figure grew to {:.0}pt of a 792pt page",
            grown.height()
        );
    }

    #[test]
    fn prose_off_a_drawings_corner_is_not_its_label() {
        // A cover's logo, and the description set clear of its corner, at
        // the page's prose size; a caption under it at prose size is the
        // logo's.
        let mut figs = vec![Figure {
            bbox: bb(222.0, 78.0, 277.0, 126.0),
            drawing: bb(222.0, 78.0, 277.0, 126.0),
            page: 0,
            shapes: 15,
            frame: None,
            axes: false,
        }];
        let blocks = vec![(
            bb(294.0, 150.0, 548.0, 181.0),
            10.0,
            "The insulation is a loose-fill thermal fiberglass",
        )];
        attach(&mut figs, &blocks, &[], (612.0, 792.0), 10.0);
        assert_eq!(figs[0].bbox.x1, 277.0, "the description stays prose");
        let blocks = vec![(bb(230.0, 132.0, 270.0, 142.0), 10.0, "Logo")];
        attach(&mut figs, &blocks, &[], (612.0, 792.0), 10.0);
        assert_eq!(figs[0].bbox.y1, 142.0, "the caption under it is its label");
    }

    #[test]
    fn a_sentence_beside_a_small_code_is_not_its_label() {
        // A QR code, 49pt wide, and two lines of prose set at its left in
        // the same tinted box, the first ending within its reach.
        let mut figs = vec![Figure {
            bbox: bb(500.0, 689.0, 549.0, 739.0),
            drawing: bb(500.0, 689.0, 549.0, 739.0),
            page: 0,
            shapes: 8,
            frame: None,
            axes: false,
        }];
        let blocks = vec![(
            bb(271.0, 703.0, 490.0, 739.0),
            10.0,
            "For more information and article numbers, visit www.example.com/E3",
        )];
        attach(&mut figs, &blocks, &[], (595.0, 842.0), 9.0);
        assert_eq!(figs[0].bbox.x0, 500.0, "the sentence stays prose");
        // A chart's title at the prose's size, as wide as the chart, is its.
        let mut figs = vec![Figure {
            bbox: bb(100.0, 300.0, 400.0, 460.0),
            drawing: bb(100.0, 300.0, 400.0, 460.0),
            page: 0,
            shapes: 30,
            frame: None,
            axes: false,
        }];
        let blocks = vec![(
            bb(100.0, 280.0, 350.0, 292.0),
            10.0,
            "Renewable energy purchases by year",
        )];
        attach(&mut figs, &blocks, &[], (612.0, 792.0), 10.0);
        assert_eq!(figs[0].bbox.y0, 280.0, "the title is the chart's");
    }

    #[test]
    fn a_chart_still_collects_the_labels_around_it() {
        // The case attachment exists for: a drawing with its tick and value
        // labels set just outside it.
        let mut figs = vec![Figure {
            bbox: bb(100.0, 300.0, 300.0, 460.0),
            drawing: bb(100.0, 300.0, 300.0, 460.0),
            page: 0,
            shapes: 12,
            frame: None,
            axes: false,
        }];
        let blocks = vec![
            (bb(96.0, 470.0, 150.0, 480.0), 7.0, "2021"), // tick label below
            (bb(210.0, 470.0, 260.0, 480.0), 7.0, "2022"), // another tick
            (bb(120.0, 285.0, 180.0, 295.0), 7.0, "37%"), // value label above
        ];
        attach(&mut figs, &blocks, &[], (612.0, 792.0), 10.0);
        assert!(
            figs[0].bbox.y1 >= 480.0,
            "labels below the axis belong to it"
        );
        assert!(
            figs[0].bbox.y0 <= 285.0,
            "labels above the bars belong to it"
        );
    }

    #[test]
    fn a_band_of_the_pages_design_is_not_a_drawing() {
        // A sidebar run down the page from its left edge, off its top and
        // foot: the band and two boxes set in it.
        let sidebar = fills(&[
            bb(-9.0, -9.0, 205.0, 801.0),
            bb(20.0, 300.0, 190.0, 340.0),
            bb(20.0, 480.0, 190.0, 520.0),
        ]);
        assert!(detect(&sidebar, &[], &[], 0, (612.0, 792.0)).is_empty());
        // A banner across the top of the page.
        let banner = fills(&[
            bb(0.0, 0.0, 560.0, 80.0),
            bb(20.0, 20.0, 120.0, 60.0),
            bb(400.0, 30.0, 540.0, 70.0),
        ]);
        assert!(detect(&banner, &[], &[], 0, (595.0, 842.0)).is_empty());
        // The same shapes inside the margins are a drawing.
        let inset = fills(&[
            bb(72.0, 300.0, 520.0, 380.0),
            bb(92.0, 320.0, 192.0, 360.0),
            bb(400.0, 330.0, 500.0, 370.0),
        ]);
        assert_eq!(detect(&inset, &[], &[], 0, (612.0, 792.0)).len(), 1);
    }

    #[test]
    fn text_at_the_pages_size_is_not_reached_through_other_text() {
        // A logo, and a column of short blocks below it at the size of the
        // page's prose: the first is near the logo, the rest are only near
        // each other.
        let mut figs = vec![Figure {
            bbox: bb(33.0, 33.0, 131.0, 117.0),
            drawing: bb(33.0, 33.0, 131.0, 117.0),
            page: 0,
            shapes: 33,
            frame: None,
            axes: false,
        }];
        let column: Vec<(BBox, f32, &str)> = (0..6)
            .map(|i| {
                let y = 130.0 + 30.0 * i as f64;
                (bb(30.0, y, 210.0, y + 20.0), 10.0, "heading")
            })
            .collect();
        attach(&mut figs, &column, &[], (612.0, 792.0), 10.0);
        assert!(figs[0].bbox.y1 <= 150.0, "grew to {:?}", figs[0].bbox);
        // Set smaller, as a chart's tick labels are, blocks chain from one
        // to the next: three ticks spaced down from the logo, each 30pt on.
        let logo = || {
            vec![Figure {
                bbox: bb(33.0, 33.0, 131.0, 117.0),
                drawing: bb(33.0, 33.0, 131.0, 117.0),
                page: 0,
                shapes: 33,
                frame: None,
                axes: false,
            }]
        };
        let ticks: Vec<(BBox, f32, &str)> = (0..3)
            .map(|i| {
                let y = 130.0 + 30.0 * i as f64;
                (
                    bb(30.0 + 40.0 * i as f64, y, 70.0 + 40.0 * i as f64, y + 10.0),
                    7.0,
                    "1",
                )
            })
            .collect();
        let mut figs = logo();
        let taken = attach(&mut figs, &ticks, &[], (612.0, 792.0), 10.0);
        assert_eq!(taken, vec![vec![0, 1, 2]]);
        assert_eq!(figs[0].bbox.y1, 200.0);
        // But not past the drawing's own size from it: fine print 300pt
        // away is not reached through the labels between.
        let far: Vec<(BBox, f32, &str)> = (0..12)
            .map(|i| {
                let y = 130.0 + 25.0 * i as f64;
                (
                    bb(
                        30.0 + 40.0 * (i % 2) as f64,
                        y,
                        60.0 + 40.0 * (i % 2) as f64,
                        y + 8.0,
                    ),
                    7.0,
                    "1",
                )
            })
            .collect();
        let mut figs = logo();
        attach(&mut figs, &far, &[], (612.0, 792.0), 10.0);
        assert!(
            figs[0].bbox.y1 <= 117.0 + 84.0,
            "walked to {:?}",
            figs[0].bbox
        );
    }

    #[test]
    fn a_column_running_on_past_the_drawing_is_not_its_labels() {
        // Notes 1-6 set alike above a form: the last three are within the
        // form's reach, the first three are not, and all six are one run.
        let mut figs = vec![Figure {
            bbox: bb(46.0, 690.0, 585.0, 756.0),
            drawing: bb(46.0, 690.0, 585.0, 756.0),
            page: 0,
            shapes: 14,
            frame: None,
            axes: false,
        }];
        let notes: Vec<(BBox, f32, &str)> = (0..6)
            .map(|i| {
                let y = 608.0 + 12.0 * i as f64;
                (bb(46.0, y, 260.0, y + 9.0), 9.0, "note")
            })
            .collect();
        let taken = attach(&mut figs, &notes, &[], (612.0, 792.0), 10.0);
        assert_eq!(taken, vec![Vec::<usize>::new()]);
        assert_eq!(figs[0].bbox, figs[0].drawing);
    }

    #[test]
    fn text_between_labels_is_not_a_label() {
        // A label left of the drawing and one under it stretch the region
        // over a paragraph too wide to be a label: it stays the page's.
        let mut figs = vec![Figure {
            bbox: bb(300.0, 100.0, 500.0, 300.0),
            drawing: bb(300.0, 100.0, 500.0, 300.0),
            page: 0,
            shapes: 20,
            frame: None,
            axes: false,
        }];
        let blocks = vec![
            (bb(250.0, 150.0, 290.0, 158.0), 7.0, "Axis"),
            (bb(300.0, 310.0, 340.0, 318.0), 7.0, "2021"),
            (
                bb(40.0, 200.0, 360.0, 250.0),
                10.0,
                "A paragraph of the page",
            ),
        ];
        let taken = attach(&mut figs, &blocks, &[], (612.0, 792.0), 10.0);
        assert_eq!(taken, vec![vec![0, 1]]);
    }

    #[test]
    fn text_set_on_panels_is_not_a_drawing() {
        // Specification rows on grey bands beside a fixture drawing: the
        // bands carry the rows' words and are set aside, and the drawing is
        // found alone.
        let mut boxes = vec![];
        for i in 0..5 {
            let y = 160.0 + 29.0 * i as f64;
            boxes.push(bb(54.0, y, 376.0, y + 14.0));
        }
        for i in 0..6 {
            let x = 400.0 + 25.0 * i as f64;
            boxes.push(bb(x, 240.0 + 10.0 * i as f64, x + 20.0, 300.0));
        }
        let glyphs: Vec<Glyph> = (0..5)
            .flat_map(|i| {
                let y = 160.0 + 29.0 * i as f64;
                (0..20).map(move |k| glyph("e", 60.0 + 5.0 * k as f64, y + 3.0, 8.0))
            })
            .collect();
        let got = detect(&fills(&boxes), &[], &glyphs, 0, (612.0, 792.0));
        assert_eq!(got.len(), 1);
        assert!(got[0].bbox.x0 >= 400.0, "{:?}", got[0].bbox);
        // Without the words, the bands are shapes and join the drawing.
        let got = detect(&fills(&boxes), &[], &[], 0, (612.0, 792.0));
        assert!(got.iter().any(|f| f.bbox.x0 < 100.0));
    }

    #[test]
    fn a_tables_cells_go_together() {
        // A table drawn as filled cells: the first column names its rows in
        // words, the others hold figures. All are its cells, and there is
        // no drawing.
        let mut boxes = vec![];
        let mut glyphs = vec![];
        for r in 0..5 {
            let y = 200.0 + 30.0 * r as f64;
            boxes.push(bb(72.0, y, 173.0, y + 15.0));
            boxes.push(bb(173.0, y, 231.0, y + 15.0));
            boxes.push(bb(231.0, y, 303.0, y + 15.0));
            for (k, t) in ["C", "E", "B", "U"].iter().enumerate() {
                glyphs.push(glyph(t, 80.0 + 6.0 * k as f64, y + 3.0, 9.0));
            }
            for k in 0..4 {
                glyphs.push(glyph("6", 190.0 + 6.0 * k as f64, y + 3.0, 9.0));
                glyphs.push(glyph("1", 250.0 + 6.0 * k as f64, y + 3.0, 9.0));
            }
        }
        assert!(detect(&fills(&boxes), &[], &glyphs, 0, (612.0, 792.0)).is_empty());
        // Bars set with their values alone are a chart's.
        let bars: Vec<BBox> = (0..5)
            .map(|i| {
                bb(
                    100.0 + 40.0 * i as f64,
                    300.0 - 25.0 * i as f64,
                    130.0 + 40.0 * i as f64,
                    400.0,
                )
            })
            .collect();
        let values: Vec<Glyph> = (0..5)
            .map(|i| glyph("7", 110.0 + 40.0 * i as f64, 380.0, 9.0))
            .collect();
        assert_eq!(
            detect(&fills(&bars), &[], &values, 0, (612.0, 792.0)).len(),
            1
        );
    }

    #[test]
    fn a_tables_grey_cells_are_not_a_drawing() {
        // A table greys out the cells it has nothing for, and draws no other
        // shape: cells in rows and columns.
        let mut boxes = vec![];
        for r in 0..4 {
            let y = 160.0 + 18.0 * r as f64;
            for (x0, x1) in [(294.0, 386.0), (387.0, 470.0), (471.0, 582.0)] {
                boxes.push(bb(x0, y, x1, y + 17.0));
            }
        }
        assert!(detect(&fills(&boxes), &[], &[], 0, (612.0, 792.0)).is_empty());
    }

    #[test]
    fn a_plot_keeps_its_background_and_scattered_labels() {
        // A plot area holding bars and a few tick labels is the chart's.
        let mut boxes = vec![bb(100.0, 100.0, 400.0, 300.0)];
        for i in 0..5 {
            let x = 120.0 + 50.0 * i as f64;
            boxes.push(bb(x, 300.0 - 30.0 * (i + 1) as f64, x + 30.0, 300.0));
        }
        let glyphs: Vec<Glyph> = ["Q", "u", "a", "r", "t"]
            .iter()
            .enumerate()
            .map(|(k, t)| glyph(t, 105.0 + 5.0 * k as f64, 105.0, 7.0))
            .collect();
        let got = detect(&fills(&boxes), &[], &glyphs, 0, (612.0, 792.0));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].bbox, bb(100.0, 100.0, 400.0, 300.0));
    }

    #[test]
    fn a_panels_edging_goes_with_it() {
        // A 3.2pt accent strip over a table's header bar is the bar's.
        let bar = bb(54.0, 366.0, 376.0, 380.0);
        assert!(edging(&bb(54.0, 362.8, 376.0, 366.0), &bar));
        assert!(edging(&bb(50.0, 366.0, 54.0, 380.0), &bar), "down its side");
        assert!(
            !edging(&bb(54.0, 340.0, 376.0, 343.0), &bar),
            "apart from it"
        );
        assert!(
            !edging(&bb(100.0, 362.8, 200.0, 366.0), &bar),
            "short of its length"
        );
    }

    #[test]
    fn drawings_set_close_together_part_rather_than_vanish() {
        // Three framed diagrams 50pt apart down a page: joined at the
        // widest reach they are too large to be one drawing.
        let mut boxes = vec![];
        for d in 0..3 {
            let y = 80.0 + 230.0 * d as f64;
            boxes.push(bb(50.0, y, 560.0, y + 180.0));
            for k in 0..4 {
                let x = 100.0 + 100.0 * k as f64;
                boxes.push(bb(x, y + 40.0, x + 60.0, y + 140.0));
            }
        }
        let got = detect(&fills(&boxes), &[], &[], 0, (612.0, 792.0));
        assert_eq!(got.len(), 3, "{got:?}");
    }

    #[test]
    fn a_page_sized_fill_joins_nothing() {
        // A background as large as the page, and a chart on it.
        let f = fills(&[
            bb(0.0, 0.0, 595.0, 842.0),
            bb(297.0, 285.0, 356.0, 377.0),
            bb(290.0, 361.0, 346.0, 403.0),
            bb(238.0, 326.0, 293.0, 403.0),
            bb(241.0, 285.0, 297.0, 335.0),
        ]);
        let got = detect(&f, &[], &[], 0, (595.0, 842.0));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].shapes, 4);
    }

    fn glyph(t: &str, x: f64, y: f64, size: f32) -> Glyph {
        Glyph {
            text: t.to_string(),
            bbox: Some(bb(x, y, x + size as f64 * 0.5, y + size as f64 * 0.7)),
            origin: (x, y + size as f64 * 0.7),
            font_size: size,
            advance: Some(size as f64 * 0.55),
            page: 0,
            rotation_deg: 0.0,
            weight: None,
            draw_index: 0,
        }
    }

    #[test]
    fn a_forms_box_and_fields_are_no_drawing() {
        // A submittal's footer: a logo's letters over a form's box, the box
        // holding six fields under or after their labels' colons.
        let text = |t: &str, x: f64, y: f64| -> Vec<Glyph> {
            t.chars()
                .enumerate()
                .filter(|(_, c)| !c.is_whitespace())
                .map(|(k, c)| glyph(&c.to_string(), x + 5.5 * k as f64, y, 10.0))
                .collect()
        };
        let mut glyphs = text("Job Name:", 54.0, 710.0);
        glyphs.extend(text("Model Numbers:", 216.6, 710.0));
        glyphs.extend(text("Job Number:", 54.0, 745.3));
        let logo: Vec<BBox> = (0..6)
            .map(|k| bb(57.8 + 10.0 * k as f64, 693.6, 66.0 + 10.0 * k as f64, 702.5))
            .collect();
        let mut shapes = vec![
            bb(46.1, 706.7, 585.1, 755.5),
            bb(51.8, 721.4, 194.0, 735.6),
            bb(119.1, 737.9, 194.0, 752.1),
            bb(209.9, 721.4, 392.9, 735.6),
            bb(209.9, 739.2, 392.9, 753.4),
            bb(397.9, 721.4, 571.3, 735.6),
            bb(397.9, 739.2, 571.3, 753.4),
        ];
        shapes.extend(logo.iter().copied());
        let got = detect(&fills(&shapes), &[], &glyphs, 0, (612.0, 792.0));
        assert!(got.iter().all(|f| f.drawing.y1 < 705.0), "{got:?}");
        // A box holding marks at one end, with words along it, is a panel:
        // the marks are not a drawing the size of the box.
        let mut glyphs = text("APPROVALS:", 328.0, 724.3);
        glyphs.extend(text("AVAILABLE AT:", 431.5, 724.4));
        let marks = vec![
            bb(54.0, 721.0, 593.0, 764.0),
            bb(59.0, 726.0, 74.0, 739.0),
            bb(63.0, 734.0, 82.0, 747.0),
            bb(132.0, 725.0, 154.0, 742.0),
            bb(162.0, 730.0, 174.0, 742.0),
            bb(183.0, 726.0, 206.0, 746.0),
            bb(208.0, 727.0, 217.0, 745.0),
        ];
        let got = detect(&fills(&marks), &[], &glyphs, 0, (612.0, 792.0));
        assert!(got.iter().all(|f| f.drawing.x1 < 300.0), "{got:?}");
    }

    #[test]
    fn a_wordmark_under_diagrams_is_grouped_alone() {
        // Two diagrams' shapes, and a logo's letters 36pt under them, within
        // the reach that groups the diagrams.
        let mut shapes = vec![
            bb(320.0, 200.0, 356.0, 254.0),
            bb(340.0, 215.0, 389.0, 224.0),
            bb(300.0, 234.0, 389.0, 251.0),
            bb(319.0, 286.0, 355.0, 340.0),
            bb(364.0, 286.0, 400.0, 340.0),
            bb(299.0, 320.0, 479.0, 342.0),
        ];
        for k in 0..6 {
            let x = 310.0 + 8.5 * k as f64;
            shapes.push(bb(x, 378.0, x + 7.0, 388.0));
        }
        let got = detect(&fills(&shapes), &[], &[], 0, (612.0, 792.0));
        assert!(!got.is_empty(), "the diagrams are a figure");
        assert!(got.iter().all(|f| f.drawing.y1 < 350.0), "{got:?}");
    }

    #[test]
    fn text_beside_a_wordmark_is_not_a_drawings_label() {
        // A form's title beside the wordmark in a footer, under the page's
        // diagrams: within their reach, but the logo's company.
        let fig = || {
            vec![Figure {
                bbox: bb(300.0, 200.0, 480.0, 342.0),
                drawing: bb(300.0, 200.0, 480.0, 342.0),
                page: 0,
                shapes: 20,
                frame: None,
                axes: false,
            }]
        };
        let blocks = vec![
            (bb(300.0, 188.0, 420.0, 196.0), 8.0, "Wiring Diagram"),
            (
                bb(372.0, 377.0, 560.0, 388.0),
                11.0,
                "PRODUCT DATA SHEET 12-345",
            ),
        ];
        let mark = bb(310.0, 378.0, 359.5, 388.0);
        let mut figs = fig();
        let taken = attach_beside(&mut figs, &blocks, &[], (612.0, 792.0), 9.0, &[mark]);
        assert_eq!(taken[0], [0], "{taken:?}");
        let mut figs = fig();
        let taken = attach(&mut figs, &blocks, &[], (612.0, 792.0), 9.0);
        assert_eq!(taken[0], [0, 1], "{taken:?}");
    }

    #[test]
    fn a_column_of_the_pages_text_beside_a_drawing_is_the_pages() {
        // A drawing in a box, and in the column beside it a heading and a
        // note set flush with the page's paragraph above them.
        let fig = |d: BBox| {
            vec![Figure {
                bbox: d,
                drawing: d,
                page: 0,
                shapes: 7,
                frame: None,
                axes: false,
            }]
        };
        let para = "This product can be adjusted for wattage from fifty to seventy five watts by adjusting the three position switch on the back housing.";
        let blocks = vec![
            (bb(391.0, 40.0, 560.0, 85.0), 8.0, para),
            (bb(391.4, 99.0, 410.0, 108.0), 9.0, "FCC"),
            (
                bb(391.0, 114.0, 560.0, 132.0),
                8.0,
                "CAUTION: Changes or modifications could void your authority.",
            ),
            (bb(150.0, 330.0, 170.0, 338.0), 8.0, "5"),
        ];
        let mut figs = fig(bb(37.0, 37.0, 354.0, 325.0));
        let taken = attach(&mut figs, &blocks, &[], (612.0, 792.0), 8.0);
        assert_eq!(taken[0], [3], "{taken:?}");
        // A run of steps wrapped from line to line in the column beside a
        // wiring diagram, under the line introducing them.
        let blocks = vec![
            (
                bb(47.5, 481.0, 270.0, 489.0),
                7.5,
                "Make the following Electrical Connections to supply cord:",
            ),
            (
                bb(74.0, 504.5, 300.0, 512.5),
                7.5,
                "a. For 120/277/347V connect the black fixture lead to the",
            ),
            (
                bb(87.2, 514.4, 290.0, 522.4),
                7.5,
                "voltage supply or Hot 1 (for 208/240/480V wiring).",
            ),
            (
                bb(74.0, 528.0, 300.0, 536.0),
                7.5,
                "b. For 120/277/347V connect the white fixture lead to the",
            ),
            (
                bb(87.2, 538.0, 290.0, 546.0),
                7.5,
                "neutral supply or Hot 2 (for 208/240/480V wiring).",
            ),
            (bb(340.0, 470.0, 380.0, 477.0), 6.0, "LINE"),
        ];
        let mut figs = fig(bb(319.0, 459.0, 567.0, 735.0));
        let taken = attach(&mut figs, &blocks, &[], (612.0, 792.0), 8.0);
        assert_eq!(taken[0], [5], "{taken:?}");
    }

    #[test]
    fn prose_beside_a_chart_stays_prose() {
        let mut figs = vec![Figure {
            bbox: bb(100.0, 300.0, 300.0, 460.0),
            drawing: bb(100.0, 300.0, 300.0, 460.0),
            page: 0,
            shapes: 12,
            frame: None,
            axes: false,
        }];
        // A full-measure paragraph: wider than a label can be.
        let blocks = vec![(bb(27.0, 470.0, 560.0, 520.0), 10.0, "prose")];
        attach(&mut figs, &blocks, &[], (612.0, 792.0), 10.0);
        assert_eq!(figs[0].bbox.y1, 460.0, "a paragraph is not a label");
    }

    #[test]
    fn numbered_callouts_on_a_photo_reach_for_nothing() {
        // Callout circles over two product photos, numbered 1 to 3, under a
        // heading set 12pt above them and over a key set 14pt below.
        let photos = [bb(40.0, 87.0, 172.0, 220.0), bb(174.0, 87.0, 303.0, 221.0)];
        let callouts = || {
            vec![Figure {
                bbox: bb(41.0, 88.0, 302.0, 219.0),
                drawing: bb(41.0, 88.0, 302.0, 219.0),
                page: 0,
                shapes: 8,
                frame: None,
                axes: false,
            }]
        };
        let around = vec![
            (bb(37.0, 66.0, 110.0, 77.0), 11.0, "Key Features"),
            (bb(37.0, 233.0, 120.0, 240.0), 7.0, "1. 8Z Pellicle"),
            (bb(172.0, 91.0, 175.0, 96.0), 4.5, "1"),
            (bb(109.0, 125.0, 112.0, 130.0), 4.5, "2"),
            (bb(214.0, 167.0, 217.0, 172.0), 4.5, "3"),
        ];
        let mut figs = callouts();
        attach(&mut figs, &around, &photos, (792.0, 612.0), 7.0);
        assert_eq!(figs[0].bbox, bb(41.0, 88.0, 302.0, 219.0));
        // The same drawing on the page, with no photo under it, is a chart's.
        let mut figs = callouts();
        attach(&mut figs, &around, &[], (792.0, 612.0), 7.0);
        assert!(figs[0].bbox.y0 <= 66.0, "kept to {:?}", figs[0].bbox);
    }

    #[test]
    fn annotations_over_panels_reach_as_a_chart_does() {
        // A scientific figure's arrows over its image panels, labelled in
        // words: its labels around the drawing are still its own.
        let panels = [bb(212.0, 234.0, 462.0, 446.0)];
        let mut figs = vec![Figure {
            bbox: bb(212.0, 297.0, 462.0, 430.0),
            drawing: bb(212.0, 297.0, 462.0, 430.0),
            page: 0,
            shapes: 9,
            frame: None,
            axes: false,
        }];
        let labels = vec![
            (bb(310.0, 300.0, 347.0, 305.0), 5.0, "near coast"),
            (bb(309.0, 310.0, 322.0, 315.0), 5.0, "sea"),
            (bb(408.0, 262.0, 420.0, 270.0), 5.0, "sky"),
        ];
        attach(&mut figs, &labels, &panels, (612.0, 792.0), 9.0);
        assert!(figs[0].bbox.y0 <= 262.0, "kept to {:?}", figs[0].bbox);
    }

    #[test]
    fn a_picture_with_labels_set_on_it_is_a_figure_of_them() {
        // A film's cutaway drawn as an image, its layers named on it, the
        // last half off its lower edge, and the next section's heading
        // under it.
        let picture = bb(298.0, 247.0, 525.0, 385.0);
        let blocks = vec![
            (
                bb(437.0, 320.0, 534.0, 328.0),
                8.0,
                "Heat Sealable PLA Layer",
            ),
            (bb(310.0, 362.0, 345.0, 368.0), 8.0, "PLA Core"),
            (
                bb(430.0, 383.0, 527.0, 391.0),
                8.0,
                "Heat Sealable PLA Layer",
            ),
            (
                bb(291.0, 414.0, 424.0, 422.0),
                10.0,
                "Environmental Certifications",
            ),
        ];
        let mut figs = pictures(&[picture], &blocks, 0, (612.0, 792.0), 10.0, &[]);
        assert_eq!(figs.len(), 1);
        let labels = attach(&mut figs, &blocks, &[picture], (612.0, 792.0), 10.0);
        assert_eq!(labels, vec![vec![0, 1, 2]]);
    }

    #[test]
    fn a_picture_is_no_figure_for_a_title_a_paragraph_a_signature_or_a_credit() {
        let picture = bb(298.0, 247.0, 525.0, 385.0);
        let label = (bb(310.0, 362.0, 345.0, 368.0), 8.0, "PLA Core");
        let none = |blocks: &[(BBox, f32, &str)], im: BBox| {
            pictures(&[im], blocks, 0, (612.0, 792.0), 10.0, &[]).is_empty()
        };
        // A slide's photo with its title set over it in display type.
        let title = (bb(310.0, 260.0, 500.0, 284.0), 24.0, "Our Mission");
        assert!(none(&[title, label], picture));
        // A paragraph set over it.
        let prose = "Our films are made from a renewable resource and are heat \
                     sealable on both sides, with good mechanical properties";
        let paragraph = (bb(310.0, 260.0, 500.0, 300.0), 8.0, prose);
        assert!(none(&[paragraph, label], picture));
        // A paragraph run on and off it, with a heading over it: a
        // brochure's background.
        let heading = (bb(400.0, 300.0, 470.0, 312.0), 10.0, "GROWTH");
        let running = (bb(310.0, 370.0, 560.0, 400.0), 10.0, prose);
        assert!(none(&[heading, label, running], picture));
        // A signature, over the line and the name it signs, from one edge.
        let line = (bb(300.0, 330.0, 460.0, 342.0), 11.0, "_________________");
        let name = (bb(300.0, 344.0, 380.0, 356.0), 11.0, "Kirk Sanders");
        assert!(none(&[line, name], picture));
        // A credit alone in its corner.
        let credit = (bb(440.0, 375.0, 520.0, 382.0), 6.0, "Photo: J. Doe");
        assert!(none(&[credit], picture));
        // The page's own background, whatever is set on it.
        let page = bb(0.0, 0.0, 612.0, 792.0);
        assert!(none(&[credit, label], page));
    }

    #[test]
    fn an_address_is_no_drawings_label() {
        // A mascot and a logo drawn as one cluster, reaching over the
        // company's address set under the logo: its phone and web line says
        // where to reach the maker, and the lines it runs on from go with it.
        let drawing = bb(27.0, 664.0, 160.0, 756.0);
        let mut figs = vec![Figure {
            bbox: drawing,
            drawing,
            page: 0,
            shapes: 39,
            frame: None,
            axes: false,
        }];
        let blocks = vec![
            (bb(124.0, 708.0, 232.0, 716.0), 7.0, "ACME SALES, LLC"),
            (
                bb(124.0, 718.0, 244.0, 735.0),
                7.0,
                "ONE ACME PARKWAY TOLEDO, OH 43659 USA",
            ),
            (
                bb(124.0, 741.0, 212.0, 758.0),
                7.0,
                "1-800-555-0100 www.acme.com",
            ),
        ];
        let labels = attach(&mut figs, &blocks, &[], (612.0, 792.0), 9.0);
        assert_eq!(labels, vec![Vec::<usize>::new()]);
    }

    #[test]
    fn a_line_introducing_a_list_is_no_label() {
        // A wiring diagram took the line over its lettered steps; the steps
        // are list items, and the line leads into them. A legend's title on
        // its colon, over nothing but the drawing's labels, stays.
        let blocks = vec![
            (
                bb(300.0, 100.0, 520.0, 108.0),
                8.0,
                "Make the following Electrical Connections to supply cord:",
            ),
            (bb(300.0, 200.0, 340.0, 208.0), 8.0, "Legend:"),
        ];
        let items = vec![(bb(310.0, 112.0, 540.0, 130.0), 8.0)];
        let mut taken = vec![vec![0, 1]];
        release_lead_ins(&mut taken, &blocks, &items);
        assert_eq!(taken, vec![vec![1]]);
    }

    /// A number's glyphs, one to a digit, from `x` on a baseline at `y`.
    fn number(t: &str, x: f64, y: f64) -> Vec<Glyph> {
        t.chars()
            .enumerate()
            .map(|(i, c)| glyph(&c.to_string(), x + i as f64 * 4.4, y, 8.0))
            .collect()
    }

    /// A calibration's points: five levels across, the replicates a few
    /// points apart in height, so every point shares a row and a column
    /// with others.
    fn calibration() -> Vec<Fill> {
        let mut v = Vec::new();
        for x in [100.0, 120.0, 140.0, 160.0, 180.0] {
            for k in 0..11 {
                let y = 200.0 + 3.0 * k as f64;
                v.push(bb(x, y, x + 5.0, y + 5.0));
            }
        }
        fills(&v)
    }

    #[test]
    fn a_scatter_plotted_against_an_axis_is_a_figure() {
        let ticks: Vec<Glyph> = [("30", 180.0), ("20", 200.0), ("10", 220.0)]
            .iter()
            .flat_map(|(t, y)| number(t, 80.0, *y))
            .collect();
        let got = detect(&calibration(), &[], &ticks, 0, (612.0, 792.0));
        assert_eq!(got.len(), 1);
        // With no axis beside them, points in rows and columns are a
        // table's shading as before.
        assert!(detect(&calibration(), &[], &[], 0, (612.0, 792.0)).is_empty());
    }

    #[test]
    fn a_matrix_of_dots_is_no_scatter() {
        // A feature matrix's dots, a row of text apart, every place filled.
        let mut v = Vec::new();
        for x in [300.0, 320.0, 340.0, 360.0] {
            for y in [100.0, 114.0, 128.0, 142.0] {
                v.push(bb(x, y, x + 5.0, y + 5.0));
            }
        }
        assert!(!scattered(&v, 8.0));
        let ticks: Vec<Glyph> = [("1", 100.0), ("2", 114.0), ("3", 128.0)]
            .iter()
            .flat_map(|(t, y)| number(t, 280.0, *y))
            .collect();
        assert!(detect(&fills(&v), &[], &ticks, 0, (612.0, 792.0)).is_empty());
        // Nor a column of drawn bullets.
        let bullets: Vec<BBox> = (0..8)
            .map(|i| bb(90.0, 100.0 + 13.0 * i as f64, 95.0, 105.0 + 13.0 * i as f64))
            .collect();
        assert!(!scattered(&bullets, 10.0));
    }

    #[test]
    fn a_line_its_sentence_runs_on_from_is_no_title() {
        // A paragraph's first line, parted at a justified space, over the
        // plots: its sentence runs on into the next line, in lower case.
        let drawing = bb(86.0, 227.0, 555.0, 529.0);
        let mut figs = vec![Figure {
            bbox: drawing,
            drawing,
            page: 0,
            shapes: 140,
            frame: None,
            axes: false,
        }];
        let blocks = vec![
            (
                bb(62.0, 136.0, 260.0, 146.0),
                10.0,
                "This appendix contains supplementary figures for",
            ),
            (
                bb(42.0, 148.0, 556.0, 182.0),
                10.0,
                "report. Figures 1.1 to 1.34 plot measured concentration versus expected \
                 concentration in validation experiments for each compound in each matrix.",
            ),
            (
                bb(86.0, 217.0, 177.0, 225.0),
                9.0,
                "A. Reagent water spikes",
            ),
        ];
        let labels = attach(&mut figs, &blocks, &[], (612.0, 792.0), 10.0);
        assert_eq!(labels, vec![vec![2]]);
    }

    #[test]
    fn a_running_head_is_no_title_but_a_chart_title_at_the_top_is() {
        // Plots under a running head set in the page's head band, 33pt
        // over them; and a chart drawn at the top of a page, its title
        // standing against it in the band.
        let plots = bb(98.0, 86.0, 567.0, 388.0);
        let mut figs = vec![Figure {
            bbox: plots,
            drawing: plots,
            page: 0,
            shapes: 150,
            frame: None,
            axes: false,
        }];
        let head = vec![(
            bb(397.0, 43.0, 541.0, 53.0),
            10.0,
            "Appendix 1. Supplemental Figures",
        )];
        assert_eq!(
            attach(&mut figs, &head, &[], (612.0, 792.0), 10.0),
            vec![Vec::<usize>::new()]
        );
        let chart = bb(98.0, 58.0, 400.0, 300.0);
        let mut figs = vec![Figure {
            bbox: chart,
            drawing: chart,
            page: 0,
            shapes: 20,
            frame: None,
            axes: false,
        }];
        let title = vec![(bb(98.0, 44.0, 260.0, 54.0), 10.0, "Figure 3. Net income")];
        assert_eq!(
            attach(&mut figs, &title, &[], (612.0, 792.0), 10.0),
            vec![vec![0]]
        );
    }

    #[test]
    fn a_heading_over_a_list_over_a_drawing_is_no_label() {
        // A column's heading, its bulleted items under it, and a drawing
        // under them; a title set against the drawing stays its own.
        let figs = vec![Figure {
            bbox: bb(300.0, 400.0, 500.0, 560.0),
            drawing: bb(300.0, 400.0, 500.0, 560.0),
            page: 0,
            shapes: 12,
            frame: None,
            axes: false,
        }];
        let blocks = vec![
            (bb(300.0, 300.0, 420.0, 310.0), 9.0, "HOUSING"),
            (bb(300.0, 386.0, 440.0, 396.0), 9.0, "Side elevation"),
        ];
        let items = vec![bb(300.0, 314.0, 520.0, 370.0)];
        let mut taken = vec![vec![0, 1]];
        release_behind_items(&mut taken, &figs, &blocks, &items);
        assert_eq!(taken, vec![vec![1]]);
    }

    #[test]
    fn a_title_between_two_drawings_heads_the_lower() {
        // A title under one drawing, its labels a line under it heading the
        // next: the lower drawing's, or the page's where a frame bounds it.
        let fig = |y0: f64, y1: f64, frame: Option<BBox>| Figure {
            bbox: bb(100.0, y0, 400.0, y1),
            drawing: bb(100.0, y0, 400.0, y1),
            page: 0,
            shapes: 10,
            frame,
            axes: false,
        };
        let blocks = vec![
            (bb(100.0, 330.0, 300.0, 341.0), 11.0, "Second arrangement"),
            (bb(110.0, 352.0, 150.0, 358.0), 6.0, "Valve"),
        ];
        let figs = vec![fig(100.0, 300.0, None), fig(370.0, 500.0, None)];
        let mut taken = vec![vec![0], vec![1]];
        reassign_between(&mut taken, &figs, &blocks, 9.0);
        assert_eq!(taken, vec![vec![], vec![0, 1]]);
        // Framed, the lower drawing takes nothing over its frame but a
        // caption: the title is the page's.
        let framed = bb(95.0, 345.0, 405.0, 505.0);
        let figs = vec![fig(100.0, 300.0, None), fig(370.0, 500.0, Some(framed))];
        let mut taken = vec![vec![0], vec![1]];
        reassign_between(&mut taken, &figs, &blocks, 9.0);
        assert_eq!(taken, vec![vec![], vec![1]]);
        // Nearer the drawing above, a note under it stays its own.
        let note = vec![(
            bb(100.0, 304.0, 300.0, 312.0),
            7.0,
            "Shown without its cover",
        )];
        let figs = vec![fig(100.0, 300.0, None), fig(370.0, 500.0, None)];
        let mut taken = vec![vec![0], vec![]];
        reassign_between(&mut taken, &figs, &note, 9.0);
        assert_eq!(taken, vec![vec![0], vec![]]);
    }

    #[test]
    fn a_callout_mark_is_a_short_number_or_a_letter() {
        for t in ["3", "12", "B", "\u{2464}"] {
            assert!(callout_mark(t), "{t}");
        }
        for t in ["123", "37%", "2021", "sky", "1.", "AB"] {
            assert!(!callout_mark(t), "{t}");
        }
    }

    #[test]
    fn a_series_over_a_ticked_axis_is_a_plot() {
        use crate::rule::Trace;
        let rule = |o: Orientation, x0: f64, y0: f64, x1: f64, y1: f64| Rule {
            bbox: bb(x0, y0, x1, y1),
            orientation: o,
            page: 0,
        };
        // Text at 8 pt on the page: an em is 8.
        let text: Vec<Glyph> = (0..5)
            .map(|k| glyph("x", 400.0 + 6.0 * k as f64, 600.0, 8.0))
            .collect();
        let series = Trace {
            bbox: bb(104.0, 196.0, 298.0, 262.0),
            segments: 48,
            diagonal: 41,
            onward: 48,
            page: 0,
        };
        let axis = rule(Orientation::Horizontal, 96.0, 269.8, 306.0, 270.2);
        let ticks: Vec<Rule> = (0..5)
            .map(|k| {
                let x = 104.0 + 48.5 * k as f64;
                rule(Orientation::Vertical, x - 0.2, 270.0, x + 0.2, 274.5)
            })
            .collect();
        let got = axis_plots(&[axis], &ticks, &[series], &text, 0, &[]);
        assert_eq!(got.len(), 1);
        assert!(got[0].axes);
        assert_eq!(got[0].drawing.y0, 196.0);
        // Axes standing at its sides, on the foot, bound the plot up to the
        // scale's top, past the series' highest point; a rule standing
        // further off the plot is none of its frame.
        let sides = [
            rule(Orientation::Vertical, 95.8, 182.0, 96.2, 270.0),
            rule(Orientation::Vertical, 305.8, 182.0, 306.2, 270.0),
            rule(Orientation::Vertical, 305.8, 40.0, 306.2, 270.0),
        ];
        let got = axis_plots(
            &[axis, sides[0], sides[1]],
            &ticks,
            &[series],
            &text,
            0,
            &[],
        );
        assert_eq!(got[0].drawing.y0, 182.0);
        let got = axis_plots(&[axis, sides[2]], &ticks, &[series], &text, 0, &[]);
        assert_eq!(got[0].drawing.y0, 196.0);
        // A rule under it with no ticks is no axis.
        assert!(axis_plots(&[axis], &[], &[series], &text, 0, &[]).is_empty());
        // An outline turning back on itself is no series.
        let outline = Trace {
            onward: 30,
            ..series
        };
        assert!(axis_plots(&[axis], &ticks, &[outline], &text, 0, &[]).is_empty());
        // A scale's ticks in a column beside it bind it as well.
        let scale: Vec<Rule> = (0..4)
            .map(|k| {
                let y = 200.0 + 20.0 * k as f64;
                rule(Orientation::Horizontal, 92.0, y - 0.2, 98.0, y + 0.2)
            })
            .collect();
        assert_eq!(axis_plots(&[], &scale, &[series], &text, 0, &[]).len(), 1);
        // Where a drawing is already found, nothing more is.
        let found = Figure {
            bbox: bb(100.0, 190.0, 310.0, 280.0),
            drawing: bb(100.0, 190.0, 310.0, 280.0),
            page: 0,
            shapes: 12,
            frame: None,
            axes: false,
        };
        assert!(axis_plots(&[axis], &ticks, &[series], &text, 0, &[found]).is_empty());
    }

    #[test]
    fn bars_bound_by_an_axis_are_a_chart() {
        use crate::rule::Fill;
        let fill = |x0: f64, y0: f64, x1: f64, y1: f64| Fill {
            bbox: bb(x0, y0, x1, y1),
            page: 0,
        };
        let rule = |o: Orientation, x0: f64, y0: f64, x1: f64, y1: f64| Rule {
            bbox: bb(x0, y0, x1, y1),
            orientation: o,
            page: 0,
        };
        // Text at 8 pt on the page: an em is 8.
        let text: Vec<Glyph> = (0..5)
            .map(|k| glyph("x", 400.0 + 6.0 * k as f64, 600.0, 8.0))
            .collect();
        // Six bars 10 wide, 20 apart, standing on a base at 300.
        let heights = [40.0, 65.0, 30.0, 80.0, 55.0, 20.0];
        let bars: Vec<Fill> = heights
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let x = 100.0 + 20.0 * i as f64;
                fill(x, 300.0 - h, x + 10.0, 300.0)
            })
            .collect();
        let base = rule(Orientation::Horizontal, 95.0, 299.8, 215.0, 300.2);
        let got = bar_plots(&bars, &[base], &[], &text, 0, &[]);
        assert_eq!(got.len(), 1);
        assert!(got[0].axes);
        // No axis by them, no chart.
        assert!(bar_plots(&bars, &[], &[], &text, 0, &[]).is_empty());
        // A column of ticks beside them binds them as well.
        let ticks: Vec<Rule> = (0..5)
            .map(|k| {
                let y = 300.0 - 20.0 * k as f64;
                rule(Orientation::Horizontal, 88.0, y - 0.2, 94.0, y + 0.2)
            })
            .collect();
        assert_eq!(bar_plots(&bars, &[], &ticks, &text, 0, &[]).len(), 1);
        // Stacked: a piece to a series, ending at its value.
        let stacked: Vec<Fill> = heights
            .iter()
            .enumerate()
            .flat_map(|(i, h)| {
                let x = 100.0 + 20.0 * i as f64;
                let cut = 300.0 - h * 0.4;
                [
                    fill(x, cut, x + 10.0, 300.0),
                    fill(x, 300.0 - h, x + 10.0, cut),
                ]
            })
            .collect();
        assert_eq!(bar_plots(&stacked, &[base], &[], &text, 0, &[]).len(), 1);
        // A table's shading bands its rows at one length.
        let bands: Vec<Fill> = (0..6)
            .map(|i| {
                let y = 100.0 + 20.0 * i as f64;
                fill(100.0, y, 400.0, y + 10.0)
            })
            .collect();
        let edge = rule(Orientation::Vertical, 99.8, 95.0, 100.2, 215.0);
        assert!(bar_plots(&bands, &[edge], &[], &text, 0, &[]).is_empty());
        // Columns of cells: a piece to a row, their breaks level across the
        // columns, however long each column runs.
        let cells: Vec<Fill> = (0..6)
            .flat_map(|i| {
                let x = 100.0 + 20.0 * i as f64;
                let rows = if i == 2 { 4 } else { 5 };
                (0..rows).map(move |r| {
                    let y = 300.0 - 12.0 * (r + 1) as f64;
                    fill(x, y, x + 10.0, y + 12.0)
                })
            })
            .collect();
        assert!(bar_plots(&cells, &[base], &[], &text, 0, &[]).is_empty());
    }

    /// Lines drawn across a plot: a horizontal one at each `ys` from the
    /// first `xs` to the last, a vertical one at each `xs` likewise.
    fn lattice(xs: &[f64], ys: &[f64]) -> Vec<Rule> {
        let (x0, x1) = (xs[0], xs[xs.len() - 1]);
        let (y0, y1) = (ys[0], ys[ys.len() - 1]);
        let h = ys.iter().map(|&y| Rule {
            bbox: bb(x0, y - 0.2, x1, y + 0.2),
            orientation: Orientation::Horizontal,
            page: 0,
        });
        let v = xs.iter().map(|&x| Rule {
            bbox: bb(x - 0.2, y0, x + 0.2, y1),
            orientation: Orientation::Vertical,
            page: 0,
        });
        h.chain(v).collect()
    }

    /// A word set from (x, y), a glyph to each letter.
    fn word(t: &str, x: f64, y: f64, size: f32) -> Vec<Glyph> {
        t.chars()
            .enumerate()
            .map(|(i, c)| glyph(&c.to_string(), x + i as f64 * size as f64 * 0.55, y, size))
            .collect()
    }

    #[test]
    fn a_graticule_with_a_few_labels_is_a_plot() {
        // Six divisions across, five down, and two curve labels set over
        // the lines: a plot, though no fill draws it.
        let xs: Vec<f64> = (0..7).map(|k| 236.0 + 21.5 * k as f64).collect();
        let ys: Vec<f64> = (0..6).map(|k| 412.0 + 17.0 * k as f64).collect();
        let rules = lattice(&xs, &ys);
        let mut glyphs = word("Vin = 3 V", 262.0, 431.0, 6.5);
        glyphs.extend(word("Vin = 12 V", 300.0, 470.0, 6.5));
        let got = graticules(&rules, &glyphs, 0, (612.0, 792.0), &[]);
        assert_eq!(got.len(), 1);
        let d = got[0].drawing;
        assert!((d.x0 - 236.0).abs() < 0.5 && (d.x1 - 365.0).abs() < 0.5);
        assert!((d.y0 - 412.0).abs() < 0.5 && (d.y1 - 497.0).abs() < 0.5);
    }

    #[test]
    fn a_graticule_broken_for_a_label_keeps_its_pitch() {
        // The third vertical stops short of a note, drawn in two pieces
        // that span less than the rest: the others stand two pitches apart
        // there, and the lattice is still a plot's.
        let xs = [118.0, 133.5, 149.0, 164.5, 180.0, 195.5];
        let ys = [600.0, 612.0, 624.0, 636.0, 648.0];
        let mut rules: Vec<Rule> = lattice(&xs, &ys)
            .into_iter()
            .filter(|r| {
                !(r.orientation == Orientation::Vertical && r.bbox.x0 < 149.5 && r.bbox.x1 > 148.5)
            })
            .collect();
        for (y0, y1) in [(600.0, 618.0), (630.0, 648.0)] {
            rules.push(Rule {
                bbox: bb(148.8, y0, 149.2, y1),
                orientation: Orientation::Vertical,
                page: 0,
            });
        }
        let glyphs = word("t = 2 ms", 140.0, 620.5, 6.0);
        let got = graticules(&rules, &glyphs, 0, (612.0, 792.0), &[]);
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn a_ruled_grid_with_text_in_its_cells_is_no_plot() {
        // The same even lattice with a value in each cell: a table's.
        let xs: Vec<f64> = (0..7).map(|k| 236.0 + 21.5 * k as f64).collect();
        let ys: Vec<f64> = (0..6).map(|k| 412.0 + 17.0 * k as f64).collect();
        let rules = lattice(&xs, &ys);
        let mut glyphs = Vec::new();
        for y in &ys[..5] {
            for x in &xs[..6] {
                glyphs.extend(word("42", x + 4.0, y + 5.0, 7.0));
            }
        }
        assert!(graticules(&rules, &glyphs, 0, (612.0, 792.0), &[]).is_empty());
        // Nor rows of differing heights, a line or two of text each, though
        // their cells stand empty.
        let ys = [412.0, 426.0, 452.0, 466.0, 488.0, 502.0];
        let rules = lattice(&xs, &ys);
        assert!(graticules(&rules, &[], 0, (612.0, 792.0), &[]).is_empty());
    }
}
