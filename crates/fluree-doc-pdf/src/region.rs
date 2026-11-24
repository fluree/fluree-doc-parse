//! Reading order over a page's regions.
//!
//! Columns are found once per page, from a projection of all of its ink,
//! and were then read one at a time from the page's top to its foot. Many
//! pages stack regions instead: a title band over two columns, two
//! two-column sections one above the other, two columns over full-width
//! notes, a box set in two columns under full-width text. Read column by
//! column over the whole page, a block at the foot of the left column came
//! before the top of the right one, and text beside a title came after all
//! of the left column.
//!
//! So the page is cut into regions first, at what runs across it: a line
//! that spans the columns, a rule across the text, a table across the
//! columns, a gap no column's text crosses that is wider than a paragraph's.
//! A cut runs through no paragraph of a column the thing across does not
//! cross. Each region is read column by column before the next, top to
//! bottom, by a recursive XY cut. A region set in columns the page-wide pass
//! could not see, because its gutter stands only over it, is cut at that
//! gutter. See `docs/design/reading-order.md`.

use crate::geom::BBox;
use crate::line::Line;
use crate::rule::{Orientation, Rule};

/// A gap no column's text crosses, in the page's line height, that ends a
/// region: wider than the space between paragraphs, or above a heading.
const WIDE_GAP: f64 = 2.0;

/// How much wider than the columns' own large gaps, their ninetieth
/// percentile, a gap across them must be to part them.
const GAP_DOMINANCE: f64 = 1.5;

/// Share of a level's widest gap another must reach to part it at the same
/// level; narrower gaps are judged within the parts.
const WIDEST_SHARE: f64 = 0.8;

/// Share of the text's width a rule must run across to divide regions.
const RULE_SPAN: f64 = 0.6;

/// Least white space, in the page's line height, a rule must stand in to
/// part the text it crosses.
const RULE_ROOM: f64 = 1.0;

/// Share of a column's core a line must cross to span it.
const SPAN_SHARE: f64 = 0.2;

/// Most space, in the region's line height, between two lines across the
/// columns for them to be one block.
const SPAN_RUN: f64 = 1.0;

/// Fewest lines on each side of a gutter found within a region.
const SIDE_LINES: usize = 2;

/// Narrowest such gutter, in the region's line height: a callout set
/// ragged-left can start within a few points of the column beside it.
const LOCAL_GUTTER: f64 = 0.5;

/// A run of lines read together, and the page column they came from.
pub struct Group {
    pub lines: Vec<Line>,
    /// Index of the page-wide pass's column the lines are from.
    pub column: usize,
    /// Read at a gutter that stands only over this region: one side of a
    /// box in two columns, whose measure is its own. Otherwise the lines are
    /// a stretch of their page column and its measure is the column's.
    pub own_measure: bool,
}

/// The page's lines, column by column as the page-wide pass found them,
/// regrouped into reading order: each region's columns in turn, top to
/// bottom. `tables` are the page's grids, whose lines are not among these.
///
/// An XY cut over the columns. A region is a run of adjacent columns over a
/// stretch of the page. It is cut across first, where something runs across
/// all its columns; failing that, down its widest gutter. Two sections set
/// in the same pair of columns, beside a third column of other matter, part
/// at the gap between them once the pair stands alone. A region of one
/// column is cut across too, and each piece is read at a gutter only it
/// has.
pub fn read_order(
    cols: Vec<Vec<Line>>,
    rules: &[Rule],
    tables: &[BBox],
    pictures: &[BBox],
) -> Vec<Group> {
    let total: usize = cols.iter().map(Vec::len).sum();
    if total < 2 {
        return cols
            .into_iter()
            .enumerate()
            .map(|(column, lines)| Group {
                lines,
                column,
                own_measure: false,
            })
            .collect();
    }
    let h = median(cols.iter().flatten().map(|l| l.font_size.max(1.0) as f64)).unwrap_or(10.0);
    let page = Page {
        cols: &cols,
        cores: cols.iter().map(|ls| core(ls)).collect(),
        rules,
        tables,
        pictures,
        h,
    };
    let mut out = Vec::new();
    page.read(0, cols.len() - 1, f64::MIN, f64::MAX, &mut out, 0);
    out
}

struct Page<'a> {
    cols: &'a [Vec<Line>],
    cores: Vec<Option<(f64, f64)>>,
    rules: &'a [Rule],
    tables: &'a [BBox],
    pictures: &'a [BBox],
    h: f64,
}

/// Deepest the cut recurses: regions in regions on one page.
const MAX_DEPTH: usize = 12;

impl Page<'_> {
    /// Lines of columns `a..=b` with their middles in `top..foot`.
    fn lines(&self, a: usize, b: usize, top: f64, foot: f64) -> Vec<(usize, &Line)> {
        (a..=b)
            .flat_map(|c| self.cols[c].iter().map(move |l| (c, l)))
            .filter(|(_, l)| {
                let m = (l.bbox.y0 + l.bbox.y1) * 0.5;
                m >= top && m < foot
            })
            .collect()
    }

    /// Does `b` run across columns of `a..=b` other than `c`'s?
    fn crosses(&self, a: usize, z: usize, c: usize, b: &BBox) -> bool {
        (a..=z).filter(|&j| j != c).any(|j| {
            self.cores[j]
                .is_some_and(|(x0, x1)| b.x1.min(x1) - b.x0.max(x0) > (x1 - x0) * SPAN_SHARE)
        })
    }

    fn read(&self, a: usize, z: usize, top: f64, foot: f64, out: &mut Vec<Group>, depth: usize) {
        let lines = self.lines(a, z, top, foot);
        if lines.is_empty() {
            return;
        }
        // A region's lines are its by their middles; a cut that leaves them
        // all on one side parts nothing.
        let mid = |l: &Line| (l.bbox.y0 + l.bbox.y1) * 0.5;
        let mut cuts = self.cuts(a, z, &lines);
        cuts.retain(|&y| {
            lines.iter().any(|(_, l)| mid(l) < y) && lines.iter().any(|(_, l)| mid(l) >= y)
        });
        if !cuts.is_empty() && depth < MAX_DEPTH {
            let mut edges = vec![top];
            edges.extend(cuts);
            edges.push(foot);
            for w in edges.windows(2) {
                self.read(a, z, w[0], w[1], out, depth + 1);
            }
            return;
        }
        if a == z || depth >= MAX_DEPTH {
            for c in a..=z {
                let group: Vec<Line> = lines
                    .iter()
                    .filter(|(k, _)| *k == c)
                    .map(|(_, l)| (*l).clone())
                    .collect();
                if !group.is_empty() {
                    let parts = split_local(group, self.h, 2);
                    out.extend(parts.into_iter().map(|(lines, own_measure)| Group {
                        lines,
                        column: c,
                        own_measure,
                    }));
                }
            }
            return;
        }
        // Down the widest gutter between the region's columns.
        let mut best: Option<(usize, f64)> = None;
        for k in a..z {
            let left = lines
                .iter()
                .filter(|(c, _)| *c <= k)
                .map(|(_, l)| l.bbox.x1)
                .fold(f64::MIN, f64::max);
            let right = lines
                .iter()
                .filter(|(c, _)| *c > k)
                .map(|(_, l)| l.bbox.x0)
                .fold(f64::MAX, f64::min);
            let gap = right - left;
            if best.is_none_or(|(_, g)| gap > g) {
                best = Some((k, gap));
            }
        }
        let (k, _) = best.expect("a region of two columns or more");
        self.read(a, k, top, foot, out, depth + 1);
        self.read(k + 1, z, top, foot, out, depth + 1);
    }

    /// Do the columns `a..=z` start together, each with a line at least its
    /// text's size, side by side on one row? Then each is a flow of its own,
    /// a chart with its title and notes, a card, a column of an article,
    /// read whole before the next; the gaps that line up across them are
    /// each column's own. Not when the lines are a list's items run on from
    /// one column into the next (`1. 8Z Pellicle` beside `4. Three sizes`):
    /// the list and the section after it part at the gap between them.
    fn titled(&self, a: usize, z: usize, lines: &[(usize, &Line)]) -> bool {
        let mut firsts: Vec<(&Line, f64)> = Vec::new();
        for c in a..=z {
            let col: Vec<&Line> = lines
                .iter()
                .filter(|(k, l)| *k == c && l.rotation_bucket == 0)
                .map(|(_, l)| *l)
                .collect();
            let Some(first) = col.iter().min_by(|p, q| {
                p.bbox
                    .y0
                    .partial_cmp(&q.bbox.y0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) else {
                continue;
            };
            let size = median(col.iter().map(|l| l.font_size as f64)).unwrap_or(0.0);
            firsts.push((first, size));
        }
        // A list's items set side by side open with their numbers or
        // bullets (`1. 8Z Pellicle`, `4. Three sizes`): one list run on
        // across the columns, not titled units.
        let item = |t: &str| {
            let t = t.trim_start();
            crate::block::strip_leading_bullet(t).is_some()
                || t.chars().next().is_some_and(|c| c.is_ascii_digit())
                    && t.split_whitespace()
                        .next()
                        .is_some_and(|w| w.ends_with(['.', ')']))
        };
        firsts.len() >= 2
            && firsts
                .iter()
                .all(|(l, size)| l.font_size as f64 >= *size && !item(&l.text))
            && firsts.windows(2).all(|w| {
                let (p, q) = (&w[0].0.bbox, &w[1].0.bbox);
                p.y1.min(q.y1) - p.y0.max(q.y0) > 0.0
            })
    }

    /// Heights at which the region `a..=z` parts: under and over a line
    /// across its columns, at a rule or a table across them, at a gap none
    /// of its lines cross that is wider than a paragraph's.
    fn cuts(&self, a: usize, z: usize, lines: &[(usize, &Line)]) -> Vec<f64> {
        let left = lines
            .iter()
            .map(|(_, l)| l.bbox.x0)
            .fold(f64::MAX, f64::min);
        let right = lines
            .iter()
            .map(|(_, l)| l.bbox.x1)
            .fold(f64::MIN, f64::max);
        let top = lines
            .iter()
            .map(|(_, l)| l.bbox.y0)
            .fold(f64::MAX, f64::min);
        let foot = lines
            .iter()
            .map(|(_, l)| l.bbox.y1)
            .fold(f64::MIN, f64::max);
        let inside = |y: f64| y > top && y < foot;
        let mut cuts: Vec<f64> = Vec::new();
        let mut here: Vec<usize> = lines.iter().map(|(c, _)| *c).collect();
        here.dedup();
        let crossed = |j: usize, b: &BBox| {
            self.cores[j]
                .is_some_and(|(x0, x1)| b.x1.min(x1) - b.x0.max(x0) > (x1 - x0) * SPAN_SHARE)
        };
        // A cut runs through no paragraph. A column the thing across does
        // not cross has white space where it falls, as wide as a section's,
        // or no text on one side of it: a caption under a picture set over
        // two of four columns leaves the paragraphs beside it whole, and so
        // do the rules a column draws between its own items.
        // The white space column `j` leaves at `y`: none through a line,
        // all of it where the column has text on one side only.
        let space = |j: usize, y: f64| {
            let (mut above, mut below) = (f64::MIN, f64::MAX);
            for (_, l) in lines
                .iter()
                .filter(|(c, l)| *c == j && l.rotation_bucket == 0)
            {
                if l.bbox.y0 < y - 0.5 && l.bbox.y1 > y + 0.5 {
                    return 0.0;
                }
                if (l.bbox.y0 + l.bbox.y1) * 0.5 < y {
                    above = above.max(l.bbox.y1);
                } else {
                    below = below.min(l.bbox.y0);
                }
            }
            if above == f64::MIN || below == f64::MAX {
                f64::MAX
            } else {
                below - above
            }
        };
        let clear = |j: usize, y: f64| space(j, y) >= self.h * WIDE_GAP;
        let parts = |b: &BBox, y: f64| here.iter().all(|&j| crossed(j, b) || clear(j, y));
        if a < z {
            // Lines across the columns, in runs: a title or an author list
            // set full width over several lines is one block, and parts the
            // region above its first line and below its last.
            // A line's space is the larger type's: a display title's lines
            // stand a body line's height and more apart.
            let mut spans: Vec<(BBox, f64, f32)> = lines
                .iter()
                .filter(|(c, l)| self.crosses(a, z, *c, &l.bbox))
                .map(|(_, l)| (l.bbox, (l.font_size as f64).max(self.h), l.font_size))
                .collect();
            spans.sort_by(|p, q| {
                p.0.y0
                    .partial_cmp(&q.0.y0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            // Each run with its line space and its first line's size.
            let mut runs: Vec<(BBox, f64, f32)> = Vec::new();
            for (b, size, fs) in spans {
                match runs.last_mut() {
                    Some((r, s, _)) if b.y0 - r.y1 <= s.max(size) * SPAN_RUN => {
                        *r = r.union(&b);
                        *s = s.max(size);
                    }
                    _ => runs.push((b, size, fs)),
                }
            }
            // A run goes on through a line set under it, within its width
            // and a line's space below: a wrapped title's last, short line.
            let mut under: Vec<&Line> = lines.iter().map(|(_, l)| *l).collect();
            under.sort_by(|p, q| {
                p.bbox
                    .y0
                    .partial_cmp(&q.bbox.y0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            for (r, s, _) in runs.iter_mut() {
                for l in &under {
                    let b = &l.bbox;
                    if b.y0 > r.y1 - 0.5
                        && b.y0 - r.y1 <= s.max(l.font_size as f64) * SPAN_RUN
                        && b.x0 >= r.x0 - self.h
                        && b.x1 <= r.x1 + self.h
                    {
                        r.y1 = r.y1.max(b.y1);
                    }
                }
            }
            // And it takes in a line set just over it, at its size, nearer
            // it than to the text above: the label of a list across the
            // columns, `Notes:` over the notes, set in one column.
            for (r, s, fs) in runs.iter_mut() {
                let over = under.iter().rev().find(|l| {
                    let b = &l.bbox;
                    b.y1 <= r.y0 + 0.5
                        && r.y0 - b.y1 <= s.max(l.font_size as f64) * SPAN_RUN
                        && b.x0 >= r.x0 - self.h
                        && b.x1 <= r.x1 + self.h
                        && (l.font_size - *fs).abs() <= 0.5
                });
                let Some(l) = over else { continue };
                let below = r.y0 - l.bbox.y1;
                let above = under
                    .iter()
                    .filter(|o| {
                        o.bbox.y1 <= l.bbox.y0 + 0.5
                            && o.bbox.x1.min(l.bbox.x1) - o.bbox.x0.max(l.bbox.x0) > 0.0
                    })
                    .map(|o| l.bbox.y0 - o.bbox.y1)
                    .fold(f64::MAX, f64::min);
                if below < above {
                    r.y0 = r.y0.min(l.bbox.y0);
                }
            }
            let runs: Vec<BBox> = runs.into_iter().map(|(r, ..)| r).collect();
            for r in runs {
                cuts.extend([r.y0, r.y1].into_iter().filter(|&y| parts(&r, y)));
            }
            for t in self.tables {
                if here.iter().filter(|&&j| crossed(j, t)).count() >= 2 {
                    cuts.extend([t.y0, t.y1].into_iter().filter(|&y| parts(t, y)));
                }
            }
        }
        // A rule parts the text it crosses where it stands in a line's
        // space at least: a link's underline, in the leading under its
        // line, is the line's.
        for r in self.rules {
            let y = (r.bbox.y0 + r.bbox.y1) * 0.5;
            if r.orientation == Orientation::Horizontal
                && r.bbox.x1.min(right) - r.bbox.x0.max(left) >= (right - left) * RULE_SPAN
                && here.iter().all(|&j| {
                    if crossed(j, &r.bbox) {
                        space(j, y) >= self.h * RULE_ROOM
                    } else {
                        clear(j, y)
                    }
                })
            {
                cuts.push(y);
            }
        }
        // Turned text, a side tab's `CONNECTORS`, runs down a margin across
        // every gap beside it; the regions are the upright text's.
        // A picture fills its column as text does: the gap a photo stands
        // in is no gap.
        let mut by_top: Vec<&BBox> =
            lines
                .iter()
                .filter(|(_, l)| l.rotation_bucket == 0)
                .map(|(_, l)| &l.bbox)
                .chain(self.pictures.iter().filter(|p| {
                    p.x1.min(right) - p.x0.max(left) > 0.0 && p.y1 > top && p.y0 < foot
                }))
                .collect();
        by_top.sort_by(|p, q| p.y0.partial_cmp(&q.y0).unwrap_or(std::cmp::Ordering::Equal));
        let mut gaps: Vec<(f64, f64)> = Vec::new();
        let mut reach = f64::MIN;
        for b in by_top {
            if reach > f64::MIN && b.y0 - reach >= self.h * WIDE_GAP {
                gaps.push((reach, b.y0));
            }
            reach = reach.max(b.y1);
        }
        // Across columns, a gap parts the region only where it is wider than
        // the gaps the columns leave inside themselves. Two charts side by
        // side, each a title, a plot and notes, leave gaps that line up row
        // for row, and are read each whole; two sections set in the same
        // columns are parted by a gap wider than any between their items.
        let floor = if a < z && self.titled(a, z, lines) {
            f64::MAX
        } else if a < z {
            let mut inner: Vec<f64> = Vec::new();
            for c in a..=z {
                let core = self.cores[c];
                let mut col: Vec<&BBox> = lines
                    .iter()
                    .filter(|(k, l)| *k == c && l.rotation_bucket == 0)
                    .map(|(_, l)| &l.bbox)
                    .chain(self.pictures.iter().filter(|p| {
                        core.is_some_and(|(x0, x1)| p.x1.min(x1) - p.x0.max(x0) > 0.0)
                            && p.y1 > top
                            && p.y0 < foot
                    }))
                    .collect();
                col.sort_by(|p, q| p.y0.partial_cmp(&q.y0).unwrap_or(std::cmp::Ordering::Equal));
                let mut reach = f64::MIN;
                for b in col {
                    let g = (reach, b.y0);
                    let across = gaps.iter().any(|&(y0, y1)| g.0 < y1 && g.1 > y0);
                    if reach > f64::MIN && b.y0 > reach && !across {
                        inner.push(b.y0 - reach);
                    }
                    reach = reach.max(b.y1);
                }
            }
            inner.sort_by(|p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));
            inner
                .get(inner.len() * 9 / 10)
                .map_or(0.0, |g| g * GAP_DOMINANCE)
        } else {
            0.0
        };
        // The widest first: the region under a page's title is judged on its
        // own, where its columns' titles stand side by side.
        let widest = gaps
            .iter()
            .filter(|(y0, y1)| y1 - y0 >= floor)
            .map(|(y0, y1)| y1 - y0)
            .fold(0.0, f64::max);
        for (y0, y1) in gaps {
            if y1 - y0 >= floor && y1 - y0 >= widest * WIDEST_SHARE {
                cuts.push((y0 + y1) * 0.5);
            }
        }
        cuts.retain(|&y| inside(y));
        cuts.sort_by(|p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));
        cuts.dedup_by(|p, q| (*p - *q).abs() < 0.5);
        cuts
    }
}

/// A region's column cut at a gutter that stands only over it: two columns
/// of text side by side the page-wide pass took for one. Each side is read
/// whole before the other, and is marked as setting its own measure.
fn split_local(group: Vec<Line>, h: f64, depth: usize) -> Vec<(Vec<Line>, bool)> {
    split_at_gutters(group, h, depth, false)
}

fn split_at_gutters(group: Vec<Line>, h: f64, depth: usize, own: bool) -> Vec<(Vec<Line>, bool)> {
    if depth == 0 || group.len() < SIDE_LINES * 2 {
        return vec![(group, own)];
    }
    if let Some(x) = local_gutter(&group, h) {
        let (l, r): (Vec<Line>, Vec<Line>) = group.into_iter().partition(|l| l.bbox.x1 <= x);
        let mut out = split_at_gutters(l, h, depth - 1, true);
        out.extend(split_at_gutters(r, h, depth - 1, true));
        return out;
    }
    // A line or two across the gutter, where a callout's line joined the
    // column's beside it: they part the region, and each part is read at
    // the gutter.
    // Over the upright text: a side tab's turned labels at the page's edge
    // widen nothing.
    let upright = || group.iter().filter(|l| l.rotation_bucket == 0);
    let left = upright().map(|l| l.bbox.x0).fold(f64::MAX, f64::min);
    let right = upright().map(|l| l.bbox.x1).fold(f64::MIN, f64::max);
    let wide = |l: &Line| l.rotation_bucket == 0 && l.bbox.width() > (right - left) * SPANNER_WIDTH;
    let spanners = group.iter().filter(|l| wide(l)).count();
    if spanners == 0 || spanners > MAX_SPANNERS.max(group.len() / SPANNER_SHARE) {
        return vec![(group, own)];
    }
    let rest: Vec<Line> = group.iter().filter(|l| !wide(l)).cloned().collect();
    let Some(x) = local_gutter(&rest, h) else {
        return vec![(group, own)];
    };
    if !group
        .iter()
        .filter(|l| wide(l))
        .all(|l| l.bbox.x0 < x && l.bbox.x1 > x)
    {
        return vec![(group, own)];
    }
    let mut out = Vec::new();
    let mut part: Vec<Line> = Vec::new();
    for l in group {
        if wide(&l) {
            if !part.is_empty() {
                out.extend(split_at_gutters(
                    std::mem::take(&mut part),
                    h,
                    depth - 1,
                    own,
                ));
            }
            out.push((vec![l], own));
        } else {
            part.push(l);
        }
    }
    if !part.is_empty() {
        out.extend(split_at_gutters(part, h, depth - 1, own));
    }
    out
}

/// Most lines that may cross a region's gutter and part it, rather than
/// show it has none: two, or one line in five. A title and a subtitle
/// across the top, a callout's line joined to the column's.
const MAX_SPANNERS: usize = 2;
const SPANNER_SHARE: usize = 5;

/// Share of a region's width a line must run across to be one of those.
const SPANNER_WIDTH: f64 = 0.6;

/// The middle of the widest gap no line of `group` crosses, when lines of
/// text stand on both sides of it, side by side. Turned text in a margin
/// is not weighed.
fn local_gutter(group: &[Line], h: f64) -> Option<f64> {
    let upright: Vec<&Line> = group.iter().filter(|l| l.rotation_bucket == 0).collect();
    if upright.len() < SIDE_LINES * 2 {
        return None;
    }
    let mut spans: Vec<(f64, f64)> = upright.iter().map(|l| (l.bbox.x0, l.bbox.x1)).collect();
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut gaps: Vec<(f64, f64)> = Vec::new();
    let mut reach = spans[0].1;
    for &(x0, x1) in &spans[1..] {
        if x0 - reach >= h * LOCAL_GUTTER {
            gaps.push((reach, x0));
        }
        reach = reach.max(x1);
    }
    // Widest first; the first with text on both sides.
    gaps.sort_by(|p, q| {
        (q.1 - q.0)
            .partial_cmp(&(p.1 - p.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    gaps.into_iter().map(|(a, b)| (a + b) * 0.5).find(|&x| {
        let (l, r): (Vec<&Line>, Vec<&Line>) = upright.iter().partition(|l| l.bbox.x1 <= x);
        // Text on both sides: lines of words, not a chart's ticks.
        let wordy = |side: &[&Line]| {
            side.len() >= SIDE_LINES
                && side
                    .iter()
                    .filter(|l| l.text.split_whitespace().count() >= 2)
                    .count()
                    * 2
                    >= side.len()
        };
        // Side by side: some line of each shares a row with one of the other.
        let beside = l.iter().any(|a| {
            r.iter()
                .any(|b| a.bbox.y1.min(b.bbox.y1) - a.bbox.y0.max(b.bbox.y0) > 0.0)
        });
        // Running text on a side: a line goes on in the next, which opens in
        // lower case. A column of values beside their labels does not: each
        // value starts afresh on a label's row, and read apart they would be
        // all the labels, then all the values. So a side of more than two
        // lines that never runs on and has each line level with one across
        // the gap is the other's rows, even where a sentence over the labels
        // runs on. Two such lines are a title or a caption beside text.
        // The next line's first word opens in lower case, past an opening
        // bracket or quote: `350lbs` under `300 lb/136 kg` is a value of
        // its own.
        let level = |a: &Line, other: &[&Line]| {
            other
                .iter()
                .any(|b| a.bbox.y1.min(b.bbox.y1) - a.bbox.y0.max(b.bbox.y0) > 0.0)
        };
        let runs = |side: &[&Line]| {
            side.windows(2).any(|w| {
                w[1].bbox.y0 > w[0].bbox.y0
                    && w[1]
                        .text
                        .split_whitespace()
                        .next()
                        .and_then(|t| {
                            t.trim_start_matches(['(', '[', '"', '\'', '“', '‘'])
                                .chars()
                                .next()
                        })
                        .is_some_and(char::is_lowercase)
            })
        };
        let rows = |side: &[&Line], other: &[&Line]| {
            side.len() > SIDE_LINES && !runs(side) && side.iter().all(|a| level(a, other))
        };
        wordy(&l) && wordy(&r) && beside && (runs(&l) || runs(&r)) && !rows(&l, &r) && !rows(&r, &l)
    })
}

/// Where most of a column's lines start and end.
fn core(lines: &[Line]) -> Option<(f64, f64)> {
    let x0 = median(lines.iter().map(|l| l.bbox.x0))?;
    let x1 = median(lines.iter().map(|l| l.bbox.x1))?;
    (x1 > x0).then_some((x0, x1))
}

fn median(v: impl Iterator<Item = f64>) -> Option<f64> {
    let mut v: Vec<f64> = v.collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(v[v.len() / 2])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, x0: f64, y0: f64, x1: f64) -> Line {
        Line {
            text: text.into(),
            bbox: BBox {
                x0,
                y0,
                x1,
                y1: y0 + 8.0,
            },
            page: 0,
            rotation_bucket: 0,
            glyphs: vec![],
            font_size: 8.0,
            bold: false,
            opens_with_script: false,
            opens_bold: false,
        }
    }

    fn texts(groups: &[Group]) -> Vec<String> {
        groups
            .iter()
            .flat_map(|g| &g.lines)
            .map(|l| l.text.clone())
            .collect()
    }

    /// A column of `n` lines at `x`, from `y`, one every 10pt.
    fn col(prefix: &str, x: f64, y: f64, n: usize) -> Vec<Line> {
        (0..n)
            .map(|i| {
                line(
                    &format!("{prefix} {i} words here"),
                    x,
                    y + 10.0 * i as f64,
                    x + 150.0,
                )
            })
            .collect()
    }

    /// A list's items from `first` on, two lines each, at `x` from `y`.
    fn items(first: usize, x: f64, y: f64, n: usize) -> Vec<Line> {
        (0..n)
            .flat_map(|i| {
                let y = y + 20.0 * i as f64;
                [
                    line(&format!("{}. Item title", first + i), x, y, x + 150.0),
                    line("and its description", x, y + 10.0, x + 150.0),
                ]
            })
            .collect()
    }

    #[test]
    fn two_sections_in_one_pair_of_columns_part_at_their_gap() {
        // Items 1-3 at the left and 4-6 at the right, then a second
        // section in the same columns, 30pt below: wider than any gap
        // inside the columns.
        let mut left = items(1, 36.0, 100.0, 3);
        left.extend(col("second-left", 36.0, 190.0, 6));
        let mut right = items(4, 200.0, 100.0, 3);
        right.extend(col("second-right", 200.0, 190.0, 6));
        let out = texts(&read_order(vec![left, right], &[], &[], &[]));
        let at = |t: &str| out.iter().position(|o| o.starts_with(t)).unwrap();
        assert!(at("6. Item") < at("second-left"), "{out:?}");
    }

    #[test]
    fn titled_columns_are_read_each_whole() {
        // Two charts side by side, each a title, a run of labels and notes,
        // with gaps that line up across them.
        let unit = |name: &str, x: f64| {
            let mut c = vec![line(&format!("{name} chart title"), x, 100.0, x + 150.0)];
            c.extend(col(&format!("{name} label"), x, 130.0, 5));
            c.extend(col(&format!("{name} note"), x, 210.0, 2));
            c
        };
        let out = texts(&read_order(
            vec![unit("left", 36.0), unit("right", 200.0)],
            &[],
            &[],
            &[],
        ));
        let at = |t: &str| out.iter().position(|o| o.starts_with(t)).unwrap();
        assert!(at("left note") < at("right chart"), "{out:?}");
    }

    #[test]
    fn a_title_across_the_columns_is_one_region() {
        // A two-line title over two columns: its lines stay together, read
        // before both columns.
        let mut left = vec![
            line("A long title set across the page", 36.0, 40.0, 360.0),
            line("and its last line", 36.0, 50.0, 150.0),
        ];
        left.extend(col("left", 36.0, 80.0, 4));
        let right = col("right", 200.0, 80.0, 4);
        let out = read_order(vec![left, right], &[], &[], &[]);
        assert_eq!(out[0].lines.len(), 2, "{:?}", texts(&out));
        assert!(texts(&out)[2].starts_with("left"));
    }

    #[test]
    fn a_display_titles_lines_stay_one_run() {
        // A 22pt title over two columns of 8pt text, its lines 15pt apart:
        // more than a line of the text, less than a line of the title.
        let big = |t: &str, y: f64| {
            let mut l = line(t, 40.0, y, 330.0);
            l.font_size = 22.0;
            l.bbox.y1 = y + 16.0;
            l
        };
        let mut left = vec![
            big("A display title set across the page", 40.0),
            big("and its second line", 71.0),
        ];
        left.extend(col("left", 40.0, 120.0, 6));
        let right = col("right", 200.0, 120.0, 6);
        let out = read_order(vec![left, right], &[], &[], &[]);
        assert_eq!(out[0].lines.len(), 2, "{:?}", texts(&out));
    }

    #[test]
    fn a_label_over_a_run_across_the_columns_goes_with_it() {
        // Two columns of 11pt text; under them, notes set across both at
        // 9pt, and their label in the left column just over them.
        let at = |t: &str, x0: f64, y: f64, x1: f64, fs: f32| {
            let mut l = line(t, x0, y, x1);
            l.font_size = fs;
            l
        };
        let mut left: Vec<Line> = (0..5)
            .map(|i| {
                at(
                    &format!("left text {i} of the column"),
                    40.0,
                    100.0 + 14.0 * i as f64,
                    280.0,
                    11.0,
                )
            })
            .collect();
        left.push(at("Notes:", 44.0, 182.0, 74.0, 9.0));
        left.push(at(
            "1 A note set across the whole of the page's width",
            40.0,
            194.0,
            550.0,
            9.0,
        ));
        left.push(at(
            "2 Another note set across the page's whole width",
            40.0,
            206.0,
            550.0,
            9.0,
        ));
        let right: Vec<Line> = (0..5)
            .map(|i| {
                at(
                    &format!("right text {i} of the column"),
                    320.0,
                    100.0 + 14.0 * i as f64,
                    560.0,
                    11.0,
                )
            })
            .collect();
        let out = texts(&read_order(vec![left, right], &[], &[], &[]));
        let at = |t: &str| out.iter().position(|o| o.starts_with(t)).unwrap();
        assert!(at("right text 4") < at("Notes:"), "{out:?}");
        assert_eq!(at("Notes:") + 1, at("1 A note"), "{out:?}");
    }

    #[test]
    fn a_box_in_two_columns_is_read_at_its_gutter() {
        // One page column; a box below in two, whose lines run on.
        let lines = vec![
            line("Kits for the production of a", 70.0, 200.0, 200.0),
            line("The housings in the series are", 230.0, 200.0, 360.0),
            line("connector with matched parts", 70.0, 210.0, 200.0),
            line("the smallest, for use where", 230.0, 210.0, 360.0),
            line("to reduce ordering effort", 70.0, 220.0, 200.0),
            line("space is limited on the rail", 230.0, 220.0, 360.0),
        ];
        let groups = read_order(vec![lines], &[], &[], &[]);
        assert!(groups.iter().all(|g| g.own_measure && g.column == 0));
        let out = texts(&groups);
        assert_eq!(
            out,
            [
                "Kits for the production of a",
                "connector with matched parts",
                "to reduce ordering effort",
                "The housings in the series are",
                "the smallest, for use where",
                "space is limited on the rail",
            ]
        );
    }

    fn rule(x0: f64, x1: f64, y: f64) -> Rule {
        Rule {
            bbox: BBox {
                x0,
                y0: y,
                x1,
                y1: y,
            },
            orientation: Orientation::Horizontal,
            page: 0,
        }
    }

    #[test]
    fn rules_a_column_draws_between_its_items_leave_the_next_whole() {
        // A list at the left; at the right, features with a rule under
        // each, as wide as most of the page.
        let left: Vec<Line> = (0..8)
            .map(|i| {
                line(
                    &format!("left item {i}"),
                    40.0,
                    100.0 + 12.0 * i as f64,
                    180.0,
                )
            })
            .collect();
        let right: Vec<Line> = (0..6)
            .map(|i| {
                let y = 100.0 + 28.0 * (i / 2) as f64 + 12.0 * (i % 2) as f64;
                line(&format!("right feature {i}"), 220.0, y, 500.0)
            })
            .collect();
        let rules = [rule(220.0, 520.0, 124.0), rule(220.0, 520.0, 152.0)];
        let out = texts(&read_order(vec![left, right], &rules, &[], &[]));
        let at = |t: &str| out.iter().position(|o| o.starts_with(t)).unwrap();
        assert!(at("left item 7") < at("right feature 0"), "{out:?}");
    }

    #[test]
    fn an_underline_in_the_leading_parts_nothing() {
        // A link underlined across most of the measure, under its line.
        let lines: Vec<Line> = (0..6)
            .map(|i| {
                line(
                    &format!("line {i} of the paragraph"),
                    40.0,
                    100.0 + 10.0 * i as f64,
                    400.0,
                )
            })
            .collect();
        let out = read_order(vec![lines], &[rule(40.0, 380.0, 129.0)], &[], &[]);
        assert_eq!(out.len(), 1, "{:?}", texts(&out));
    }

    #[test]
    fn a_caption_over_two_columns_leaves_the_third_whole() {
        // A picture's caption set across the second and third columns,
        // beside the first column's running paragraph.
        let first = col("first", 40.0, 100.0, 12);
        let mut second = col("second", 200.0, 100.0, 5);
        second.push(line("A caption across two columns", 200.0, 160.0, 480.0));
        second.extend(col("second after", 200.0, 180.0, 4));
        let mut third = col("third", 360.0, 100.0, 5);
        third.extend(col("third after", 360.0, 180.0, 4));
        let out = texts(&read_order(vec![first, second, third], &[], &[], &[]));
        let firsts: Vec<usize> = out
            .iter()
            .enumerate()
            .filter(|(_, o)| o.starts_with("first"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(firsts, (0..12).collect::<Vec<_>>(), "{out:?}");
    }

    #[test]
    fn a_side_tab_widens_nothing() {
        // Lines across a box in two columns, and a side tab's turned label
        // at the page's edge: the lines still part the box from what is
        // over it.
        let mut tab = line("CONNECTORS", 557.0, 100.0, 563.0);
        tab.rotation_bucket = 1;
        tab.bbox.y1 = 300.0;
        let lines = vec![
            tab,
            line(
                "Plastic connector kits with a housing and gland",
                58.0,
                150.0,
                338.0,
            ),
            line(
                "contacts and a housing for the rail mount",
                58.0,
                160.0,
                300.0,
            ),
            line("Kits for the production of a", 70.0, 200.0, 200.0),
            line("The housings in the series are", 230.0, 200.0, 360.0),
            line("connector with matched parts", 70.0, 210.0, 200.0),
            line("the smallest, for use where", 230.0, 210.0, 360.0),
            line("to reduce ordering effort", 70.0, 220.0, 200.0),
            line("space is limited on the rail", 230.0, 220.0, 360.0),
        ];
        let out = texts(&read_order(vec![lines], &[], &[], &[]));
        let at = |t: &str| out.iter().position(|o| o.starts_with(t)).unwrap();
        assert!(at("to reduce") < at("The housings"), "{out:?}");
    }

    #[test]
    fn labels_beside_their_values_stay_in_rows() {
        // A field list: each label and value starts afresh, so it is no pair
        // of text columns.
        let lines = vec![
            line("Rated voltage", 70.0, 200.0, 140.0),
            line("Three hundred volts", 230.0, 200.0, 320.0),
            line("Rated current", 70.0, 210.0, 140.0),
            line("Sixteen amperes max", 230.0, 210.0, 320.0),
            line("Core insulation", 70.0, 220.0, 140.0),
            line("Polyvinyl chloride compound", 230.0, 220.0, 340.0),
        ];
        let out = texts(&read_order(vec![lines], &[], &[], &[]));
        assert_eq!(out[1], "Three hundred volts", "{out:?}");
        // Values with their units, under a sentence that runs on: a number
        // opening a line is no running text, whatever letter follows it,
        // and the sentence stands beside nothing.
        let lines = vec![
            line("The following information applies", 36.0, 414.0, 140.0),
            line("only to the work chairs.", 36.0, 422.0, 120.0),
            line("Maximum user weight size A", 36.0, 438.0, 130.0),
            line("300 lb/136 kg", 145.0, 438.0, 190.0),
            line("Maximum user weight size B", 36.0, 446.0, 130.0),
            line("350lbs", 145.0, 446.0, 170.0),
            line("Population range", 36.0, 454.0, 100.0),
            line("1st–99th percentile", 142.0, 454.0, 200.0),
        ];
        let out = texts(&read_order(vec![lines], &[], &[], &[]));
        assert_eq!(out[3], "300 lb/136 kg", "{out:?}");
    }
}
