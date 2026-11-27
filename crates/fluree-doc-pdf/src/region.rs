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
#[cfg(test)]
pub fn read_order(
    cols: Vec<Vec<Line>>,
    rules: &[Rule],
    tables: &[BBox],
    pictures: &[BBox],
    frames: &[BBox],
) -> Vec<Group> {
    read_order_with_panels(cols, rules, tables, pictures, frames, &[])
}

/// [`read_order`], with the page's `panels`: boxes drawn to be seen, shaded
/// or outlined, a sidebar set in one beside the column's text reading
/// apart from it ([`sidebar`], [`panel_after_its_sentence`]). A page's or
/// a half-spread's white background is no panel.
pub fn read_order_with_panels(
    cols: Vec<Vec<Line>>,
    rules: &[Rule],
    tables: &[BBox],
    pictures: &[BBox],
    frames: &[BBox],
    panels: &[BBox],
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
    let boxes = panels_in_the_flow(&cols, panels);
    let page = Page {
        cols: &cols,
        cores: cols.iter().map(|ls| core(ls)).collect(),
        rules,
        tables,
        pictures,
        frames,
        panels,
        h,
    };
    let mut out = Vec::new();
    page.read(0, cols.len() - 1, f64::MIN, f64::MAX, &mut out, 0);
    panel_after_its_sentence(&mut out, panels, h);
    for (panel, column, next) in boxes {
        // The panel's lines, out of the groups the region pass read them
        // in: the column over it, and the band its foot stands level with.
        let held = |l: &Line| panel.iter().any(|p| p.bbox == l.bbox && p.text == l.text);
        for g in out.iter_mut() {
            g.lines.retain(|l| !held(l));
        }
        out.retain(|g| !g.lines.is_empty());
        place_after_its_sentence(&mut out, panel, column, &next);
    }
    out
}

/// A panel set in a column under its text, between a sentence and the
/// next column it runs on into: a journal's box of its authors' names and
/// addresses at the foot of the first page's first column, the
/// Introduction's first sentence breaking off over it and going on at the
/// top of the second column (`… to continuously generate a 1 pulse` /
/// `per second (pps) on-time signal, …`). Read where it stands, the box
/// fell inside the sentence, and a region cut through it, at the band of
/// notes set level with its foot, read its lower lines among the notes'.
///
/// Read out of the groups the region pass read it in: a drawn
/// panel holding [`PANEL_LINES`] lines or more wholly inside it, half of
/// them [`PANEL_WORDS`] words or more, no line running into it from
/// outside. The line just over it in its column, [`PROSE_LINE_WORDS`]
/// words or more, closes no sentence, and the first line of the column to
/// its right, level with the top of the panel's column, opens in lower
/// case. Returned with the panel's lines, in order, the column the panel
/// stands in, and that first line: [`place_after_its_sentence`] reads the
/// panel after the line closing the sentence.
fn panels_in_the_flow(cols: &[Vec<Line>], panels: &[BBox]) -> Vec<(Vec<Line>, usize, Line)> {
    let mut out = Vec::new();
    let h = median(cols.iter().flatten().map(|l| l.font_size.max(1.0) as f64)).unwrap_or(10.0);
    let within = |b: &BBox, f: &BBox| {
        b.x0 >= f.x0 - BOX_SLACK
            && b.x1 <= f.x1 + BOX_SLACK
            && b.y0 >= f.y0 - BOX_SLACK
            && b.y1 <= f.y1 + BOX_SLACK
    };
    let closes = |l: &Line| l.text.trim_end().ends_with(['.', '!', '?', ':']);
    let opens_lower = |l: &Line| {
        l.text
            .trim_start()
            .chars()
            .next()
            .is_some_and(char::is_lowercase)
    };
    for f in panels {
        let all: Vec<&Line> = cols
            .iter()
            .flatten()
            .filter(|l| l.rotation_bucket == 0)
            .collect();
        let inside: Vec<&Line> = all.iter().copied().filter(|l| within(&l.bbox, f)).collect();
        let into = all
            .iter()
            .any(|l| overlap(&l.bbox, f) && !within(&l.bbox, f));
        if inside.len() < PANEL_LINES || !panel_text(&inside) || into {
            continue;
        }
        // The column's line just over the panel, across its width.
        let across = |l: &Line| l.bbox.x1.min(f.x1) - l.bbox.x0.max(f.x0) > 0.5 * l.bbox.width();
        let Some(last) = all
            .iter()
            .filter(|l| l.bbox.y1 <= f.y0 + BOX_SLACK && across(l))
            .max_by(|a, b| a.bbox.y1.total_cmp(&b.bbox.y1))
            .copied()
        else {
            continue;
        };
        // The panel goes on from the column, under its last line within a
        // paragraph's gap: a box further down is a drawing's.
        if closes(last)
            || words(last) < PROSE_LINE_WORDS
            || f.y0 - last.bbox.y1 > COLUMN_RUN_GAP * last.font_size as f64
        {
            continue;
        }
        // The top of the panel's column: its lines over the panel, one
        // under another, run up from the line over it to a gap wider than
        // [`COLUMN_RUN_GAP`] of their lines.
        let mut column: Vec<&Line> = all
            .iter()
            .copied()
            .filter(|l| l.bbox.y1 <= f.y0 + BOX_SLACK && across(l))
            .collect();
        column.sort_by(|a, b| b.bbox.y0.total_cmp(&a.bbox.y0));
        let mut top = last.bbox.y0;
        let mut run: Vec<&Line> = Vec::new();
        for l in &column {
            if l.bbox.y1 < top - COLUMN_RUN_GAP * l.font_size as f64 {
                break;
            }
            top = top.min(l.bbox.y0);
            run.push(l);
        }
        // A column of prose the panel stands in, as wide as it: a box in a
        // diagram, its labels over it, is no such panel.
        let prose = run.iter().filter(|l| words(l) >= PROSE_LINE_WORDS).count();
        let x0 = run.iter().map(|l| l.bbox.x0).fold(f64::MAX, f64::min);
        let x1 = run.iter().map(|l| l.bbox.x1).fold(f64::MIN, f64::max);
        // The line over the panel was wrapped at the column's edge, the
        // sentence breaking off to go on in the next column: a short line
        // over a box is a title or a label.
        if prose < PANEL_LINES
            || f.x0 < x0 - BOX_SLACK - h
            || f.x1 > x1 + BOX_SLACK + h
            || last.bbox.x1 < x1 - last.font_size as f64
        {
            continue;
        }
        // The first line of the column to its right: the topmost of the
        // lines right of the panel, from that top down to the panel's, the
        // leftmost of those on its row.
        let right: Vec<&Line> = all
            .iter()
            .copied()
            .filter(|l| {
                l.bbox.x0 >= f.x1 && l.bbox.y0 >= top - l.font_size as f64 && l.bbox.y0 < f.y0
            })
            .collect();
        let first = right.iter().map(|l| l.bbox.y0).fold(f64::MAX, f64::min);
        let next = right
            .iter()
            .filter(|l| l.bbox.y0 - first <= l.font_size as f64)
            .min_by(|a, b| a.bbox.x0.total_cmp(&b.bbox.x0))
            .copied();
        let Some(next) = next.filter(|n| opens_lower(n) && words(n) >= PROSE_LINE_WORDS) else {
            continue;
        };
        let next = next.clone();
        let column_of = cols
            .iter()
            .position(|col| col.iter().any(|l| within(&l.bbox, f)))
            .unwrap_or(0);
        let mut lines: Vec<Line> = inside.into_iter().cloned().collect();
        lines.sort_by(|a, b| a.bbox.y0.total_cmp(&b.bbox.y0));
        out.push((lines, column_of, next));
    }
    out
}

/// Widest gap, in a line's size, between two lines of one column read up
/// from a panel set in it ([`panels_in_the_flow`]): a paragraph's or a
/// heading's space, short of the gap over a section of columns.
const COLUMN_RUN_GAP: f64 = 2.5;

/// Read `panel` after the line closing the sentence `next` opens: the
/// group holding `next` is parted there, and the panel set between. Where
/// no group holds it, the panel reads last.
fn place_after_its_sentence(out: &mut Vec<Group>, panel: Vec<Line>, column: usize, next: &Line) {
    let closes = |l: &Line| l.text.trim_end().ends_with(['.', '!', '?', ':']);
    let group = Group {
        lines: panel,
        column,
        own_measure: true,
    };
    let at = out.iter().enumerate().find_map(|(g, grp)| {
        grp.lines
            .iter()
            .position(|l| l.bbox == next.bbox && l.text == next.text)
            .map(|k| (g, k))
    });
    let Some((g, k)) = at else {
        out.push(group);
        return;
    };
    let end = out[g].lines[k..]
        .iter()
        .position(closes)
        .map_or(out[g].lines.len(), |e| k + e + 1);
    let rest = out[g].lines.split_off(end);
    let (col, own) = (out[g].column, out[g].own_measure);
    out.insert(g + 1, group);
    if !rest.is_empty() {
        out.insert(
            g + 2,
            Group {
                lines: rest,
                column: col,
                own_measure: own,
            },
        );
    }
}

/// A panel read between a paragraph and the lines it runs on into: a
/// sidebar standing beside a column whose last paragraph goes on under the
/// panel across the page's width (`… implementing water efficiency` /
/// `improvements. Projects include …`). The region under the panel is cut
/// apart from the columns beside it, and the panel was read in the middle
/// of the sentence.
///
/// Where a group lies wholly in one drawn box, the group before it ends
/// without closing a sentence (`.`, `!`, `?`, `:`), and the group after it
/// opens in lower case at the left edge of the group before (within a
/// line's height), the lines that run on, to the one closing the sentence,
/// are read before the panel, and the panel after them.
fn panel_after_its_sentence(out: &mut Vec<Group>, frames: &[BBox], h: f64) {
    let within = |b: &BBox, f: &BBox| {
        b.x0 >= f.x0 - 1.0 && b.x1 <= f.x1 + 1.0 && b.y0 >= f.y0 - 1.0 && b.y1 <= f.y1 + 1.0
    };
    let closes = |l: &Line| l.text.trim_end().ends_with(['.', '!', '?', ':']);
    let opens_lower = |l: &Line| {
        l.text
            .trim_start()
            .trim_start_matches(['(', '[', '"', '\'', '“', '‘'])
            .chars()
            .next()
            .is_some_and(char::is_lowercase)
    };
    let mut i = 1;
    while i + 1 < out.len() {
        let panel = &out[i];
        let frame = if panel.lines.len() >= PANEL_LINES
            && panel_text(&panel.lines.iter().collect::<Vec<_>>())
        {
            frames
                .iter()
                .find(|f| panel.lines.iter().all(|l| within(&l.bbox, f)))
        } else {
            None
        };
        let (prev, next) = (&out[i - 1], &out[i + 1]);
        let left = prev
            .lines
            .iter()
            .map(|p| p.bbox.x0)
            .fold(f64::MAX, f64::min);
        // The paragraph stands beside the panel, level with it, and runs on
        // under it, across under it.
        let level = |a: &BBox, b: &BBox| a.y1.min(b.y1) - a.y0.max(b.y0) > 0.0;
        let runs_on = frame.is_some_and(|f| {
            prev.lines.last().is_some_and(|l| {
                !closes(l)
                    && words(l) >= PROSE_LINE_WORDS
                    && level(&l.bbox, f)
                    && (l.bbox.x1 <= f.x0 || l.bbox.x0 >= f.x1)
            }) && next.lines.first().is_some_and(|l| {
                opens_lower(l)
                    && words(l) >= PROSE_LINE_WORDS
                    && (l.bbox.x0 - left).abs() <= h
                    && (l.bbox.y0 + l.bbox.y1) / 2.0 >= f.y1
            }) && {
                // A line of the sentence running on reaches under the panel.
                let end = next
                    .lines
                    .iter()
                    .position(|l| closes(l))
                    .map_or(next.lines.len(), |k| k + 1);
                next.lines[..end]
                    .iter()
                    .any(|l| l.bbox.x1 > f.x0 && l.bbox.x0 < f.x1)
            }
        });
        if runs_on {
            let mut next = out.remove(i + 1);
            let end = next
                .lines
                .iter()
                .position(|l| closes(l))
                .map_or(next.lines.len(), |k| k + 1);
            let rest = next.lines.split_off(end);
            let run = Group {
                lines: std::mem::take(&mut next.lines),
                column: next.column,
                own_measure: next.own_measure,
            };
            out.insert(i, run);
            if !rest.is_empty() {
                next.lines = rest;
                out.insert(i + 2, next);
            }
            i += 1;
        }
        i += 1;
    }
}

struct Page<'a> {
    cols: &'a [Vec<Line>],
    cores: Vec<Option<(f64, f64)>>,
    rules: &'a [Rule],
    tables: &'a [BBox],
    pictures: &'a [BBox],
    frames: &'a [BBox],
    panels: &'a [BBox],
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
        // A run across the columns, alone in its band, may open with a
        // title: lines of one size and type, set large or bold, one to a
        // row and no longer than a heading. They read top to bottom as one,
        // whatever columns they start in: a right-aligned display title
        // whose first line lies wholly right of the gutter and whose second
        // crosses it. The rest of the band reads after them.
        if a < z && lines.len() >= 2 {
            let inside = |r: &BBox, b: &BBox| {
                b.x0 >= r.x0 - self.h
                    && b.x1 <= r.x1 + self.h
                    && b.y0 >= r.y0 - 0.5
                    && b.y1 <= r.y1 + 0.5
            };
            let runs = self.runs(a, z, &lines);
            if runs
                .iter()
                .any(|r| lines.iter().all(|(_, l)| inside(r, &l.bbox)))
            {
                let mut by_top: Vec<(usize, &Line)> = lines.clone();
                by_top.sort_by(|p, q| {
                    p.1.bbox
                        .y0
                        .partial_cmp(&q.1.bbox.y0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(
                            p.1.bbox
                                .x0
                                .partial_cmp(&q.1.bbox.x0)
                                .unwrap_or(std::cmp::Ordering::Equal),
                        )
                });
                let size = by_top[0].1.font_size;
                let mut title: Vec<(usize, &Line)> = Vec::new();
                for &(c, l) in &by_top {
                    let same = (l.font_size - size).abs() <= 0.12 * size.max(l.font_size);
                    // One to a row: lines side by side are columns of text
                    // or a caption wrapped around a picture, even where
                    // their gutter is narrower than an em.
                    let alone = title
                        .iter()
                        .all(|(_, p)| p.bbox.y1.min(l.bbox.y1) - p.bbox.y0.max(l.bbox.y0) <= 0.0);
                    // Set as a title is set: words, bold or larger than
                    // the page's text. A chart's numbers stacked down its
                    // axis, or body lines run together across a gutter,
                    // are not.
                    let words = l
                        .text
                        .split_whitespace()
                        .any(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 2);
                    let display = words
                        && (l.bold
                            || l.font_size as f64
                                > self.h * (1.0 + crate::heading::HEADING_SIZE_MARGIN));
                    if !same || !display {
                        break;
                    }
                    // A line of the title's own type beside one of its
                    // lines is a column of text set large, a lead paragraph
                    // or a bold caption over two columns, not a title.
                    if !alone {
                        title.clear();
                        break;
                    }
                    title.push((c, l));
                }
                // As short as a heading set large may be: a caption, a
                // lead paragraph or body lines run together across the
                // gutter go on longer.
                let words: usize = title
                    .iter()
                    .map(|(_, l)| l.text.split_whitespace().count())
                    .sum();
                if words > crate::heading::MAX_LONG_HEADING_WORDS {
                    title.clear();
                }
                // The rest of the band reads after the title, from the
                // middle of its highest line: every one of them under the
                // title's lines, or the band is not a title over text.
                let last = title.iter().map(|(_, l)| mid(l)).fold(f64::MIN, f64::max);
                let rest = by_top[title.len()..]
                    .iter()
                    .map(|(_, l)| mid(l))
                    .fold(f64::MAX, f64::min);
                if title.len() >= 2 && last < rest {
                    let taken = title.len();
                    out.push(Group {
                        column: title[0].0,
                        lines: title.into_iter().map(|(_, l)| l.clone()).collect(),
                        own_measure: false,
                    });
                    if taken < lines.len() {
                        self.read(a, z, rest, foot, out, depth + 1);
                    }
                    return;
                }
            }
        }
        if a == z || depth >= MAX_DEPTH {
            for c in a..=z {
                let group: Vec<Line> = lines
                    .iter()
                    .filter(|(k, _)| *k == c)
                    .map(|(_, l)| (*l).clone())
                    .collect();
                if !group.is_empty() {
                    // A stretch all in drawn boxes reads box by box; any
                    // other at a gutter only it has, then block by block
                    // where two stand side by side.
                    let read_stretch = |group: Vec<Line>| -> Vec<(Vec<Line>, bool)> {
                        match boxed(&group, self.frames) {
                            Some(boxes) => boxes.into_iter().map(|b| (b, true)).collect(),
                            None => split_local(group, self.h, 3)
                                .into_iter()
                                .flat_map(|(lines, own)| {
                                    side_blocks(lines, own, self.h, self.frames, 2)
                                })
                                .collect(),
                        }
                    };
                    // A panel set beside the column's text reads apart from
                    // it, after the text beside it.
                    // So does a picture's caption set beside it. The column
                    // beside a caption keeps its measure: text runs on above
                    // and below the picture across the column.
                    let parts: Vec<(Vec<Line>, bool)> = match sidebar(group, self.panels) {
                        Ok((before, panel, after)) => {
                            // The column beside the panel is narrower than
                            // its page column: it sets its own measure.
                            let mut parts: Vec<(Vec<Line>, bool)> = read_stretch(before)
                                .into_iter()
                                .map(|(lines, _)| (lines, true))
                                .collect();
                            parts.push((panel, true));
                            parts.extend(read_stretch(after));
                            parts
                        }
                        Err(group) => match caption_beside(group, self.pictures) {
                            Ok((before, caption, after)) => {
                                let mut parts = read_stretch(before);
                                parts.push((caption, true));
                                parts.extend(read_stretch(after));
                                parts
                            }
                            Err(group) => read_stretch(group),
                        },
                    };
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

    /// The runs of lines across the columns `a..=z`: a title or an author
    /// list set full width over several lines, with a wrapped title's short
    /// last line under it and a label set just over it.
    fn runs(&self, a: usize, z: usize, lines: &[(usize, &Line)]) -> Vec<BBox> {
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
        runs.into_iter().map(|(r, ..)| r).collect()
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
        // all of it where the column has text on one side only. A picture in
        // the column fills it as its text does: a rule between the items of
        // the next column, level with a photo, parts nothing beside it.
        let space = |j: usize, y: f64| {
            let (mut above, mut below) = (f64::MIN, f64::MAX);
            let core = self.cores[j];
            let pictures = self.pictures.iter().filter(|p| {
                core.is_some_and(|(x0, x1)| p.x1.min(x1) - p.x0.max(x0) > 0.0)
                    && p.y1 > top
                    && p.y0 < foot
            });
            for b in lines
                .iter()
                .filter(|(c, l)| *c == j && l.rotation_bucket == 0)
                .map(|(_, l)| &l.bbox)
                .chain(pictures)
            {
                if b.y0 < y - 0.5 && b.y1 > y + 0.5 {
                    return 0.0;
                }
                if (b.y0 + b.y1) * 0.5 < y {
                    above = above.max(b.y1);
                } else {
                    below = below.min(b.y0);
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
            let runs = self.runs(a, z, lines);
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
        // own, where its columns' titles stand side by side. A gap nearly as
        // wide is taken at the same level, unless the part it falls in,
        // between the cuts already taken, opens with titles side by side and
        // not every column opens another under the gap: there it is a gap
        // inside one column's section that another column's next section
        // happens to share. Where every column opens a title under it, two
        // sections set in the same columns part there (a contract's article
        // set in two languages side by side, then the next).
        let widest = gaps
            .iter()
            .filter(|(y0, y1)| y1 - y0 >= floor)
            .map(|(y0, y1)| y1 - y0)
            .fold(0.0, f64::max);
        let mut wide: Vec<(f64, f64)> = gaps
            .into_iter()
            .filter(|(y0, y1)| y1 - y0 >= floor && y1 - y0 >= widest * WIDEST_SHARE)
            .collect();
        wide.sort_by(|p, q| {
            (q.1 - q.0)
                .partial_cmp(&(p.1 - p.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mid = |l: &Line| (l.bbox.y0 + l.bbox.y1) * 0.5;
        for (i, (y0, y1)) in wide.into_iter().enumerate() {
            let y = (y0 + y1) * 0.5;
            if i > 0 && a < z {
                let over = cuts
                    .iter()
                    .copied()
                    .filter(|&c| c < y)
                    .fold(f64::MIN, f64::max);
                let under = cuts
                    .iter()
                    .copied()
                    .filter(|&c| c > y)
                    .fold(f64::MAX, f64::min);
                let part: Vec<(usize, &Line)> = lines
                    .iter()
                    .filter(|(_, l)| mid(l) >= over && mid(l) < under)
                    .copied()
                    .collect();
                let below: Vec<(usize, &Line)> =
                    part.iter().filter(|(_, l)| mid(l) >= y).copied().collect();
                if opens_titles(a, z, &part) && !opens_titles(a, z, &below) {
                    continue;
                }
            }
            cuts.push(y);
        }
        cuts.retain(|&y| inside(y));
        cuts.sort_by(|p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));
        cuts.dedup_by(|p, q| (*p - *q).abs() < 0.5);
        cuts
    }
}

/// Do the columns `a..=z` with text in `lines` each open with a title, side
/// by side on one row: a line set bold, or larger than the line under it?
/// Body text carried over from the page before to the top of both columns
/// is no title, however small the references under it run.
fn opens_titles(a: usize, z: usize, lines: &[(usize, &Line)]) -> bool {
    let mut firsts: Vec<&Line> = Vec::new();
    for c in a..=z {
        let mut col: Vec<&Line> = lines
            .iter()
            .filter(|(k, l)| *k == c && l.rotation_bucket == 0)
            .map(|(_, l)| *l)
            .collect();
        col.sort_by(|p, q| {
            p.bbox
                .y0
                .partial_cmp(&q.bbox.y0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let Some(first) = col.first() else {
            continue;
        };
        let larger = col.get(1).is_some_and(|next| {
            first.font_size as f64
                > next.font_size as f64 * (1.0 + crate::heading::HEADING_SIZE_MARGIN)
        });
        if !(first.bold || larger) {
            return false;
        }
        firsts.push(first);
    }
    firsts.len() >= 2
        && firsts.windows(2).all(|w| {
            let (p, q) = (&w[0].bbox, &w[1].bbox);
            p.y1.min(q.y1) - p.y0.max(q.y0) > 0.0
        })
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
    // A row or two across the gutter at the region's top or foot: a title
    // over the columns wrapped to a second line, or the page's number set
    // under them. They are read apart, and the columns at the gutter.
    if let Some(out) = split_past_edges(&group, h, depth, own) {
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
    // Counted line by line, or as runs, one after another, where the lines
    // across are fewer than the lines beside one another: a caption's block
    // of lines across the columns parts the region once, however many lines
    // it wraps to. A column of paragraphs is mostly lines across, a run to
    // each paragraph, and is no columns under a caption.
    let allowed = MAX_SPANNERS.max(group.len() / SPANNER_SHARE);
    let lines_across = group.iter().filter(|l| wide(l)).count();
    let runs_across = group
        .iter()
        .enumerate()
        .filter(|(i, l)| wide(l) && (*i == 0 || !wide(&group[i - 1])))
        .count();
    let parts =
        lines_across <= allowed || (runs_across <= allowed && lines_across * 2 < group.len());
    if lines_across == 0 || !parts {
        return vec![(group, own)];
    }
    // The lines across, and the line alone on its row that ends one of
    // them: set as the line over it and under its start, the end of a
    // caption or title (`moyen à l'accouchement depuis 2005` under `4. Taux
    // de fécondité … conjoncturel`). A column's first line has the next
    // column's beside it. A line set sideways, a diagram's label, runs down
    // across the rows and stands on none of them.
    let alone = |i: usize| {
        let b = &group[i].bbox;
        !group.iter().enumerate().any(|(j, o)| {
            j != i && o.rotation_bucket == 0 && o.bbox.y1.min(b.y1) - o.bbox.y0.max(b.y0) > 0.0
        })
    };
    let mut across = vec![false; group.len()];
    for i in 0..group.len() {
        across[i] = wide(&group[i])
            || (i > 0 && across[i - 1] && goes_on(&group[i - 1], &group[i]) && alone(i));
    }
    let rest: Vec<Line> = group
        .iter()
        .zip(&across)
        .filter(|(_, a)| !**a)
        .map(|(l, _)| l.clone())
        .collect();
    let Some(x) = local_gutter(&rest, h) else {
        // No gutter through the whole region: its columns may stand only
        // under, or over, a figure whose caption runs across. Each band
        // between the lines across is read at a gutter of its own, where
        // one has one.
        return split_bands(group, &across, h, depth, own);
    };
    if !group
        .iter()
        .filter(|l| wide(l))
        .all(|l| l.bbox.x0 < x && l.bbox.x1 > x)
    {
        return vec![(group, own)];
    }
    parts_at(
        group,
        &across,
        |part| split_at_gutters(part, h, depth - 1, own),
        own,
    )
}

/// `group` parted at its lines across: each run of them read together, and
/// each stretch between read by `read`.
fn parts_at(
    group: Vec<Line>,
    across: &[bool],
    mut read: impl FnMut(Vec<Line>) -> Vec<(Vec<Line>, bool)>,
    own: bool,
) -> Vec<(Vec<Line>, bool)> {
    let mut out = Vec::new();
    let mut part: Vec<Line> = Vec::new();
    let mut run: Vec<Line> = Vec::new();
    for (l, a) in group.into_iter().zip(across) {
        if *a {
            if !part.is_empty() {
                out.extend(read(std::mem::take(&mut part)));
            }
            run.push(l);
        } else {
            if !run.is_empty() {
                out.push((std::mem::take(&mut run), own));
            }
            part.push(l);
        }
    }
    if !part.is_empty() {
        out.extend(read(part));
    }
    if !run.is_empty() {
        out.push((run, own));
    }
    out
}

/// Most rows at a region's top, and at its foot, that may cross its gutter
/// and be read apart from its columns: a title over them and the line it
/// wraps to, or the page's number under them.
const EDGE_ROWS: usize = 2;

/// Least size, as a share of the columns' text, a title over them is set at
/// where it is not bold: larger by as much as a heading is.
const TITLE_OVER_BODY: f64 = 1.0 + crate::heading::HEADING_SIZE_MARGIN;

/// Most space, in the title's size, between a title's line and the line it
/// goes on in: a line's leading, not a paragraph's gap.
const TITLE_LEADING: f64 = 0.6;

/// Most a title's next line may hang in from its first, in the title's
/// size: under its text past a number (`4. Taux …`), two ems at most.
const TITLE_HANG: f64 = 2.0;

/// Does `next` go on from `last`, the line of a title or caption over it:
/// set in its size and weight, under its start or hanging under its text,
/// a line's leading down?
fn goes_on(last: &Line, next: &Line) -> bool {
    let fs = (last.font_size as f64).max(1.0);
    next.bold == last.bold
        && (next.font_size - last.font_size).abs() as f64 <= fs * 0.05
        && next.bbox.x0 >= last.bbox.x0 - fs
        && next.bbox.x0 <= last.bbox.x0 + fs * TITLE_HANG
        && next.bbox.y0 > last.bbox.y0
        && next.bbox.y0 - last.bbox.y1 <= fs * TITLE_LEADING
}

/// [`split_at_gutters`] past a region's edge rows: the fewest rows at its
/// top and foot, each crossing the gutter the rest stand at, set apart.
/// `None` where no such rows leave a gutter.
fn split_past_edges(
    group: &[Line],
    h: f64,
    depth: usize,
    own: bool,
) -> Option<Vec<(Vec<Line>, bool)>> {
    // Rows, top to bottom: lines that share a height.
    let mut order: Vec<usize> = (0..group.len()).collect();
    order.sort_by(|&a, &b| group[a].bbox.y0.total_cmp(&group[b].bbox.y0));
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for i in order {
        let b = &group[i].bbox;
        match rows.last_mut() {
            Some(row)
                if row
                    .iter()
                    .any(|&j| group[j].bbox.y1.min(b.y1) - group[j].bbox.y0.max(b.y0) > 0.0) =>
            {
                row.push(i)
            }
            _ => rows.push(vec![i]),
        }
    }
    for edges in 1..=EDGE_ROWS * 2 {
        for top in 0..=edges.min(EDGE_ROWS) {
            let foot = edges - top;
            if foot > EDGE_ROWS || edges + SIDE_LINES * 2 > rows.len() {
                continue;
            }
            let apart = |k: usize| k < top || k >= rows.len() - foot;
            let rest: Vec<Line> = (0..rows.len())
                .filter(|&k| !apart(k))
                .flat_map(|k| rows[k].iter().map(|&i| group[i].clone()))
                .collect();
            let Some(x) = local_gutter(&rest, h) else {
                continue;
            };
            let crosses = (0..rows.len())
                .filter(|&k| apart(k))
                .flat_map(|k| &rows[k])
                .all(|&i| group[i].bbox.x0 < x && group[i].bbox.x1 > x);
            if !crosses {
                continue;
            }
            // A title over the columns goes on in the line under it, however
            // short: set as the title is, under its start, a line's space
            // down. The columns' text is no title's, so the title stands out
            // from it, bold or larger.
            let mut top = top;
            let mut rest = rest;
            if top > 0 {
                let body = median(
                    (top..rows.len() - foot)
                        .flat_map(|k| &rows[k])
                        .map(|&i| group[i].font_size as f64),
                )
                .unwrap_or(h);
                let mut more = top;
                while more + foot < rows.len() && rows[more].len() == 1 {
                    let last = &group[*rows[more - 1].last().unwrap()];
                    let next = &group[rows[more][0]];
                    let titled = last.bold || last.font_size as f64 > body * TITLE_OVER_BODY;
                    if !(titled && goes_on(last, next)) {
                        break;
                    }
                    more += 1;
                }
                // Taken only where the columns still stand at their gutter
                // without them.
                if more > top {
                    let shorter: Vec<Line> = (more..rows.len() - foot)
                        .flat_map(|k| rows[k].iter().map(|&i| group[i].clone()))
                        .collect();
                    if local_gutter(&shorter, h).is_some() {
                        top = more;
                        rest = shorter;
                    }
                }
            }
            let lines = |ks: std::ops::Range<usize>| -> Vec<Line> {
                ks.flat_map(|k| rows[k].iter().map(|&i| group[i].clone()))
                    .collect()
            };
            let mut out = Vec::new();
            if top > 0 {
                out.push((lines(0..top), own));
            }
            out.extend(split_at_gutters(rest, h, depth, own));
            if foot > 0 {
                out.push((lines(rows.len() - foot..rows.len()), own));
            }
            return Some(out);
        }
    }
    None
}

/// [`split_at_gutters`] band by band: `group` parted at its lines across,
/// each band read at a gutter of its own. The region stays whole unless
/// some band has one.
fn split_bands(
    group: Vec<Line>,
    across: &[bool],
    h: f64,
    depth: usize,
    own: bool,
) -> Vec<(Vec<Line>, bool)> {
    let mut parted = false;
    let out = parts_at(
        group.clone(),
        across,
        |band| {
            let parts = split_at_gutters(band, h, depth, own);
            parted |= parts.len() > 1;
            parts
        },
        own,
    );
    if parted {
        out
    } else {
        vec![(group, own)]
    }
}

/// Most runs of lines that may cross a region's gutter and part it, rather
/// than show it has none: two, or one in five of its lines. A title and a
/// caption across the columns, a callout's line joined to the column's.
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

/// Narrowest gap, in the region's line height, between two blocks set side
/// by side: wider than any word space, as a gutter is.
const SIDE_GAP: f64 = 1.0;

/// Share of the shorter of two rows' heights they overlap by when set on
/// one baseline. Two blocks set each on its own baselines, a fraction of a
/// line apart, overlap less, and drift through each other where their
/// leadings differ.
const LEVEL_SHARE: f64 = 0.8;

/// Fewest rows the left of two blocks goes on alone past the right's last:
/// a block of its own, not a column of labels beside their values, whose
/// values may wrap past their labels but whose labels never run on alone.
const RUN_PAST: usize = 2;

/// Share of the shorter of two boxes' heights they overlap by to stand in
/// one row.
const BOX_LEVEL: f64 = 0.5;

/// Widest space, in the region's line height, between two boxes that are a
/// table's cells, drawn one box each: they touch, or stand a rule's width
/// apart. A diagram's boxes and a page's cards stand a line or more apart.
const GRID_GAP: f64 = 0.5;

/// How far, in points, a line's ink may stand over a box's edge and still
/// be inside it: the width of the box's stroke.
const BOX_SLACK: f64 = 1.0;

/// Rows of `side`: its lines grouped where one's middle is within another's
/// height, top to bottom.
fn side_rows<'a>(side: &[&'a Line]) -> Vec<Vec<&'a Line>> {
    let mut v: Vec<&Line> = side.to_vec();
    v.sort_by(|a, b| {
        a.bbox
            .y0
            .partial_cmp(&b.bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut rows: Vec<Vec<&Line>> = Vec::new();
    for l in v {
        let mid = (l.bbox.y0 + l.bbox.y1) * 0.5;
        match rows.last_mut() {
            Some(last) if last.iter().any(|p| mid > p.bbox.y0 && mid < p.bbox.y1) => last.push(l),
            _ => rows.push(vec![l]),
        }
    }
    rows
}

/// A row's top and foot.
fn row_extent(r: &[&Line]) -> (f64, f64) {
    (
        r.iter().map(|l| l.bbox.y0).fold(f64::MAX, f64::min),
        r.iter().map(|l| l.bbox.y1).fold(f64::MIN, f64::max),
    )
}

/// Half its lines or more hold two words: text, not a column of figures or
/// a chart's ticks.
fn wordy<'a>(lines: impl Iterator<Item = &'a Line>) -> bool {
    let (mut n, mut words) = (0, 0);
    for l in lines {
        n += 1;
        if l.text.split_whitespace().count() >= 2 {
            words += 1;
        }
    }
    words * 2 >= n
}

/// Two blocks of text side by side in one stretch of a column, which the
/// gutter between them did not part: neither runs on as prose does, so the
/// region pass took them for a column of labels beside their values, and
/// read them row by row. An invoice's seller beside its customer, an
/// article's facts beside its abstract, prose beside a chart's title and
/// notes. Each is read whole, the one set higher first, the left where they
/// start together.
///
/// The widest gap no line crosses, [`SIDE_GAP`] or wider, with text on both
/// sides, parts them when their rows do not pair as a list's rows do:
/// - **Set on baselines of their own:** fewer than half the rows of the
///   side with fewer stand level with one across ([`LEVEL_SHARE`]).
/// - **Run on past the other:** the rows stand level, but the left goes on
///   alone [`RUN_PAST`] rows or more past the right's last, the right holds
///   two rows or more, and the left's level lines are not labels (ending in
///   `:`, or bold).
///
/// A row of drawn boxes ([`framed_row`]) is read box by box first.
fn side_blocks(
    group: Vec<Line>,
    own: bool,
    h: f64,
    frames: &[BBox],
    depth: usize,
) -> Vec<(Vec<Line>, bool)> {
    if depth == 0 || group.len() < 3 {
        return vec![(group, own)];
    }
    if let Some((over, boxes, rest)) = framed_row(&group, frames, h) {
        let mut out = Vec::new();
        if !over.is_empty() {
            out.extend(side_blocks(over, own, h, frames, depth));
        }
        for b in boxes {
            out.extend(side_blocks(b, true, h, frames, depth - 1));
        }
        if !rest.is_empty() {
            out.extend(side_blocks(rest, own, h, frames, depth));
        }
        return out;
    }
    let upright: Vec<&Line> = group.iter().filter(|l| l.rotation_bucket == 0).collect();
    if upright.len() < 3 {
        return vec![(group, own)];
    }
    let mut spans: Vec<(f64, f64)> = upright.iter().map(|l| (l.bbox.x0, l.bbox.x1)).collect();
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut widest: Option<(f64, f64)> = None;
    let mut reach = spans[0].1;
    for &(x0, x1) in &spans[1..] {
        if x0 - reach >= h * SIDE_GAP && widest.is_none_or(|(a, b)| x0 - reach > b - a) {
            widest = Some((reach, x0));
        }
        reach = reach.max(x1);
    }
    let Some((a, b)) = widest else {
        return vec![(group, own)];
    };
    let x = (a + b) * 0.5;
    let (l, r): (Vec<&Line>, Vec<&Line>) = upright.iter().partition(|l| l.bbox.x1 <= x);
    if !wordy(l.iter().copied()) || !wordy(r.iter().copied()) {
        return vec![(group, own)];
    }
    let (lr, rr) = (side_rows(&l), side_rows(&r));
    if lr.len().max(rr.len()) < 2 {
        return vec![(group, own)];
    }
    let level = |a: &[&Line], b: &[&Line]| {
        let ((a0, a1), (b0, b1)) = (row_extent(a), row_extent(b));
        a1.min(b1) - a0.max(b0) >= LEVEL_SHARE * (a1 - a0).min(b1 - b0)
    };
    let paired = |row: &[&Line], other: &[Vec<&Line>]| other.iter().any(|o| level(row, o));
    let (ltop, lfoot) = row_extent(&l);
    let (rtop, rfoot) = row_extent(&r);
    if lfoot.min(rfoot) <= ltop.max(rtop) {
        return vec![(group, own)];
    }
    let (fewer, more) = if lr.len() <= rr.len() {
        (&lr, &rr)
    } else {
        (&rr, &lr)
    };
    let offset = fewer.iter().filter(|row| paired(row, more)).count() * 2 < fewer.len();
    let runs_on = || {
        let last = rr.last().map(|row| row_extent(row).1).unwrap_or(f64::MAX);
        let past = lr.iter().filter(|row| row_extent(row).0 >= last).count();
        let level_rows: Vec<&Vec<&Line>> = lr.iter().filter(|row| paired(row, &rr)).collect();
        let labels = level_rows
            .iter()
            .filter(|row| {
                row.iter().all(|l| l.bold)
                    || row.last().is_some_and(|l| l.text.trim_end().ends_with(':'))
            })
            .count();
        past >= RUN_PAST && rr.len() >= 2 && labels * 2 < level_rows.len().max(1)
    };
    if !offset && !runs_on() {
        return vec![(group, own)];
    }
    // The left first, unless the right starts a line or more above it: a
    // title set over the right-hand block.
    let left_first = ltop <= rtop + h;
    let (lv, rv): (Vec<Line>, Vec<Line>) = group.into_iter().partition(|l| l.bbox.x1 <= x);
    let (first, second) = if left_first { (lv, rv) } else { (rv, lv) };
    let mut out = side_blocks(first, true, h, frames, depth - 1);
    out.extend(side_blocks(second, true, h, frames, depth - 1));
    out
}

fn holds(b: &BBox, l: &Line) -> bool {
    l.bbox.x0 >= b.x0 - BOX_SLACK
        && l.bbox.x1 <= b.x1 + BOX_SLACK
        && l.bbox.y0 >= b.y0 - BOX_SLACK
        && l.bbox.y1 <= b.y1 + BOX_SLACK
}

/// The drawn boxes that wholly hold some of `group`'s upright lines but not
/// all of them, a box round the whole stretch parting nothing, each with
/// the lines it holds, nested boxes' lines too. Largest first.
fn holding(group: &[Line], frames: &[BBox]) -> Vec<(BBox, Vec<usize>)> {
    let upright = group.iter().filter(|l| l.rotation_bucket == 0).count();
    let mut held: Vec<(BBox, Vec<usize>)> = Vec::new();
    for b in frames {
        if held.iter().any(|(k, _)| k == b) {
            continue;
        }
        let v: Vec<usize> = (0..group.len())
            .filter(|&i| group[i].rotation_bucket == 0 && holds(b, &group[i]))
            .collect();
        if !v.is_empty() && v.len() < upright {
            held.push((*b, v));
        }
    }
    held.sort_by(|p, q| {
        (q.0.width() * q.0.height())
            .partial_cmp(&(p.0.width() * p.0.height()))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    held
}

/// The rows the lines `v` of `group` stand in.
fn rows_of(group: &[Line], v: &[usize]) -> usize {
    let ls: Vec<&Line> = v.iter().map(|&i| &group[i]).collect();
    side_rows(&ls).len()
}

fn boxes_level(a: &BBox, b: &BBox) -> bool {
    a.y1.min(b.y1) - a.y0.max(b.y0) >= BOX_LEVEL * a.height().min(b.height())
}

fn overlap(a: &BBox, b: &BBox) -> bool {
    a.x1.min(b.x1) > a.x0.max(b.x0) && a.y1.min(b.y1) > a.y0.max(b.y0)
}

/// Most share of a stretch's width a sidebar's panel takes: a box across
/// the column is a banner or a callout in the text's flow.
const PANEL_WIDTH: f64 = 0.75;

/// Least share of a stretch's width a sidebar's panel takes: a chart's bar
/// or a key's swatch holding a label is narrower.
const PANEL_MIN_WIDTH: f64 = 0.2;

/// Fewest lines a sidebar's panel holds, and fewest lines of the column's
/// prose beside it: a chart's bars hold a label or a figure each.
const PANEL_LINES: usize = 3;

/// Fewest words in a line of prose; a panel's lines of text hold
/// [`PANEL_WORDS`] or more, half of them.
const PROSE_LINE_WORDS: usize = 5;
const PANEL_WORDS: usize = 3;

fn words(l: &Line) -> usize {
    l.text.split_whitespace().count()
}

/// Half or more of the panel's lines are lines of words, as a sidebar's
/// list or paragraph is: not a chart's labels and figures.
fn panel_text(lines: &[&Line]) -> bool {
    lines.iter().filter(|l| words(l) >= PANEL_WORDS).count() * 2 >= lines.len()
}

/// A drawn box, shaded or outlined, set beside the column's text and holding
/// lines of its own: a fact sheet's `Highlights` beside its project summary.
/// The page-wide pass sees one column where the text under the box runs the
/// page's width, and read row by row, the box's lines fell between the
/// column's. Split out of `group`: the column's lines before the panel, the
/// panel's lines, and the column's lines after it.
///
/// A panel holds two lines or more wholly inside it, no line runs into it
/// from outside, a line outside it shares a row with one inside, it is
/// narrower than the stretch ([`PANEL_WIDTH`]), and it holds under half the
/// stretch's lines. A banner across the column has nothing beside it; a tint
/// laid under the page's body text holds most of it; a table's cells are
/// read apart before this.
///
/// The panel reads after the column's text that stands beside it, and after
/// the paragraph running on past its foot: the column's lines go on until
/// one closes a sentence (`.`, `!`, `?`, `:`).
fn sidebar(
    group: Vec<Line>,
    frames: &[BBox],
) -> Result<(Vec<Line>, Vec<Line>, Vec<Line>), Vec<Line>> {
    let upright: Vec<&Line> = group.iter().filter(|l| l.rotation_bucket == 0).collect();
    if upright.len() < 4 {
        return Err(group);
    }
    let left = upright.iter().map(|l| l.bbox.x0).fold(f64::MAX, f64::min);
    let right = upright.iter().map(|l| l.bbox.x1).fold(f64::MIN, f64::max);
    let within = |b: &BBox, f: &BBox| {
        b.x0 >= f.x0 - 1.0 && b.x1 <= f.x1 + 1.0 && b.y0 >= f.y0 - 1.0 && b.y1 <= f.y1 + 1.0
    };
    let level = |a: &BBox, b: &BBox| a.y1.min(b.y1) - a.y0.max(b.y0) > 0.0;
    let panel = frames.iter().find(|f| {
        if f.width() > (right - left) * PANEL_WIDTH {
            return false;
        }
        if f.width() < (right - left) * PANEL_MIN_WIDTH {
            return false;
        }
        let inside: Vec<&Line> = group.iter().filter(|l| within(&l.bbox, f)).collect();
        let into = group
            .iter()
            .any(|l| overlap(&l.bbox, f) && !within(&l.bbox, f));
        let beside = group
            .iter()
            .filter(|o| {
                !overlap(&o.bbox, f)
                    && (o.bbox.x1 <= f.x0 || o.bbox.x0 >= f.x1)
                    && words(o) >= PROSE_LINE_WORDS
                    && inside.iter().any(|i| level(&i.bbox, &o.bbox))
            })
            .count();
        inside.len() >= PANEL_LINES
            && panel_text(&inside)
            && inside.len() * 2 < group.len()
            && !into
            && beside >= PANEL_LINES
    });
    let Some(f) = panel.copied() else {
        return Err(group);
    };
    // At the column's edge, the column beside it one column of text: a
    // panel between two columns, or one in a spread of two pages, is read
    // as the region pass reads them.
    let h = upright
        .iter()
        .map(|l| l.font_size as f64)
        .fold(0.0, f64::max)
        .max(1.0);
    let at_edge = f.x0 <= left + h || f.x1 >= right - h;
    let beside: Vec<Line> = group
        .iter()
        .filter(|l| !within(&l.bbox, &f))
        .cloned()
        .collect();
    // Text over or under the panel on its own side of the column is the
    // panel's column's (a heading over a box, the text after it): the box
    // is part of a column, not set beside one. A line reaching across into
    // the column beside it, the page's full width, says nothing.
    let right_side = f.x0 - left > right - f.x1;
    let own_side = beside.iter().any(|l| {
        l.bbox.x1 > f.x0
            && l.bbox.x0 < f.x1
            && if right_side {
                l.bbox.x0 >= f.x0 - h
            } else {
                l.bbox.x1 <= f.x1 + h
            }
    });
    if !at_edge || own_side || local_gutter(&beside, h).is_some() {
        return Err(group);
    }
    // The column's lines keep their order.
    let (panel, mut rest): (Vec<Line>, Vec<Line>) =
        group.into_iter().partition(|l| within(&l.bbox, &f));
    let closes = |l: &Line| l.text.trim_end().ends_with(['.', '!', '?', ':']);
    let mut at = rest.iter().take_while(|l| l.bbox.y0 < f.y1).count();
    while at > 0 && at < rest.len() && !closes(&rest[at - 1]) {
        at += 1;
    }
    let after = rest.split_off(at);
    Ok((rest, panel, after))
}

/// Farthest, in a caption's line height, a picture's foot stands over its
/// caption's first line: the space a caption is set under its picture.
const CAPTION_REACH: f64 = 1.5;

/// A picture's caption set beside a column's text: the lines under a
/// picture, or a row of pictures, within its width, set smaller than the
/// text level with them. A photo set at the column's side, the column's
/// text running on full width under it, leaves no gutter down the stretch,
/// and read row by row, the caption's lines fell between the text's: a
/// paragraph's sentence broken by `Luna's flagship fiber optic sensing
/// instruments; the`, a list's first item parted from its last word by a
/// radar image's caption.
///
/// Split out of `group` as [`sidebar`] splits a panel: the column's lines
/// before the caption, and the paragraph running on past its foot to a line
/// closing a sentence; the caption's lines; and the column's lines after.
///
/// The caption's lines follow on from each other, a line's leading apart at
/// most. A line level with one of them stands beside it, outside the
/// picture's width, and every such line is larger: a caption is set a size
/// down from its text. Captions set at their text's size are read as the
/// region pass reads them.
fn caption_beside(
    group: Vec<Line>,
    pictures: &[BBox],
) -> Result<(Vec<Line>, Vec<Line>, Vec<Line>), Vec<Line>> {
    let level = |a: &BBox, b: &BBox| a.y1.min(b.y1) - a.y0.max(b.y0) > 0.0;
    let mut found: Option<(Vec<usize>, BBox)> = None;
    for p in pictures {
        // The row of pictures this one stands in: those level with it and
        // ending where it ends, its foot.
        let row: Vec<&BBox> = pictures
            .iter()
            .filter(|q| level(p, q) && (q.y1 - p.y1).abs() <= BOX_SLACK)
            .collect();
        let x0 = row.iter().map(|q| q.x0).fold(f64::MAX, f64::min);
        let x1 = row.iter().map(|q| q.x1).fold(f64::MIN, f64::max);
        let under = |l: &Line| {
            l.rotation_bucket == 0
                && l.bbox.x0 >= x0 - BOX_SLACK
                && l.bbox.x1 <= x1 + BOX_SLACK
                && l.bbox.y0 >= p.y1 - BOX_SLACK
        };
        let mut order: Vec<usize> = (0..group.len()).filter(|&i| under(&group[i])).collect();
        order.sort_by(|&i, &j| group[i].bbox.y0.total_cmp(&group[j].bbox.y0));
        let mut caption: Vec<usize> = Vec::new();
        for i in order {
            let l = &group[i];
            let fs = (l.font_size as f64).max(1.0);
            let reach = match caption.last() {
                None => p.y1 + CAPTION_REACH * fs,
                Some(&k) => group[k].bbox.y1 + fs,
            };
            if l.bbox.y0 > reach {
                break;
            }
            caption.push(i);
        }
        if caption.is_empty() {
            continue;
        }
        let size = caption
            .iter()
            .map(|&i| group[i].font_size)
            .fold(0.0f32, f32::max) as f64;
        let beside: Vec<&Line> = group
            .iter()
            .enumerate()
            .filter(|(i, o)| {
                !caption.contains(i)
                    && o.rotation_bucket == 0
                    && (o.bbox.x1 <= x0 || o.bbox.x0 >= x1)
                    && caption.iter().any(|&k| level(&group[k].bbox, &o.bbox))
            })
            .map(|(_, o)| o)
            .collect();
        let smaller = !beside.is_empty()
            && beside
                .iter()
                .all(|o| o.font_size as f64 > size * (1.0 + CAPTION_SIZE_STEP));
        if smaller
            && caption
                .iter()
                .all(|&k| group[k].text.split_whitespace().count() >= 2)
        {
            // The picture and its caption, as one box.
            let top = row.iter().map(|q| q.y0).fold(f64::MAX, f64::min);
            let frame = caption.iter().map(|&k| group[k].bbox).fold(
                BBox {
                    x0,
                    y0: top,
                    x1,
                    y1: p.y1,
                },
                |a, b| a.union(&b),
            );
            found = Some((caption, frame));
            break;
        }
    }
    let Some((caption, frame)) = found else {
        return Err(group);
    };
    // As a panel is read apart ([`sidebar`]): no line runs into the caption
    // from outside it, a paragraph's line fused with one of its own, and no
    // text stands over or under the picture and its caption on their own
    // side of the column, as a column's own text, or its page's folio, does.
    let into = group.iter().enumerate().any(|(i, l)| {
        !caption.contains(&i)
            && l.rotation_bucket == 0
            && overlap(&l.bbox, &frame)
            && !(l.bbox.x0 >= frame.x0 - BOX_SLACK && l.bbox.x1 <= frame.x1 + BOX_SLACK)
    });
    let left = group.iter().map(|l| l.bbox.x0).fold(f64::MAX, f64::min);
    let right = group.iter().map(|l| l.bbox.x1).fold(f64::MIN, f64::max);
    let h = group
        .iter()
        .map(|l| l.font_size as f64)
        .fold(0.0, f64::max)
        .max(1.0);
    let right_side = frame.x0 - left > right - frame.x1;
    let own_side = group.iter().enumerate().any(|(i, l)| {
        !caption.contains(&i)
            && l.rotation_bucket == 0
            && !overlap(&l.bbox, &frame)
            && (l.bbox.y1 <= frame.y0 || l.bbox.y0 >= frame.y1)
            && l.bbox.x1 > frame.x0
            && l.bbox.x0 < frame.x1
            && if right_side {
                l.bbox.x0 >= frame.x0 - h
            } else {
                l.bbox.x1 <= frame.x1 + h
            }
    });
    if into || own_side {
        return Err(group);
    }
    let foot = caption
        .iter()
        .map(|&i| group[i].bbox.y1)
        .fold(f64::MIN, f64::max);
    let original = group.clone();
    let mut lines: Vec<(bool, Line)> = group
        .into_iter()
        .enumerate()
        .map(|(i, l)| (caption.contains(&i), l))
        .collect();
    let panel: Vec<Line> = lines
        .iter()
        .filter(|(c, _)| *c)
        .map(|(_, l)| l.clone())
        .collect();
    lines.retain(|(c, _)| !*c);
    let mut rest: Vec<Line> = lines.into_iter().map(|(_, l)| l).collect();
    let closes = |l: &Line| l.text.trim_end().ends_with(['.', '!', '?', ':']);
    let item = |l: &Line| {
        l.text
            .trim_start()
            .chars()
            .next()
            .is_some_and(crate::block::is_bullet)
    };
    let mut at = rest.iter().take_while(|l| l.bbox.y0 < foot).count();
    while at > 0 && at < rest.len() && !closes(&rest[at - 1]) {
        at += 1;
    }
    // A list beside the caption runs on to its last item: the caption
    // follows the list, not one of its items.
    if at > 0 && rest[..at].iter().any(item) {
        while at < rest.len() && item(&rest[at]) {
            at += 1;
            while at < rest.len() && !item(&rest[at]) && !closes(&rest[at - 1]) {
                at += 1;
            }
        }
    }
    // The paragraph beside the caption may close a line under the picture,
    // as the column widens there. Where it runs on across the column for
    // more lines, its narrow lines and its wide ones read as one paragraph
    // of two measures, and against the wider one its narrow lines read as
    // ended by hand. Read as the region pass reads it, the caption's lines
    // part the two measures, and the paragraph keeps its lines.
    let narrow = rest[..at]
        .iter()
        .filter(|l| l.bbox.y1 > frame.y0 && l.bbox.y0 < frame.y1)
        .map(|l| l.bbox.width())
        .fold(0.0, f64::max);
    let widened = rest[..at]
        .iter()
        .filter(|l| l.bbox.y0 >= frame.y1 && !item(l) && l.bbox.width() > narrow + h)
        .count();
    if widened > CAPTION_RUN_ON_LINES {
        return Err(original);
    }
    let after = rest.split_off(at);
    Ok((rest, panel, after))
}

/// Most lines the paragraph beside a caption may run on across the column
/// under the picture ([`caption_beside`]): the line that closes it.
const CAPTION_RUN_ON_LINES: usize = 1;

/// How much smaller, as a share of the caption's size, the text beside a
/// caption is set larger by at the least: a size step, as a heading's.
const CAPTION_SIZE_STEP: f64 = 0.1;

/// A stretch of a column whose every upright line stands in a drawn box,
/// the outermost boxes two or more and apart, one holding two rows of text:
/// a box diagram, a flowchart, a page of cards. The region pass reads its
/// lines row by row across the boxes, or parts it at a gutter between
/// them. Read box by box instead, a box with the boxes inside it, in flow
/// order: top to bottom, and left to right across a row of boxes level
/// with each other ([`BOX_LEVEL`]). A branch set off a box, lower down at
/// its side, reads after it. Turned lines read last.
fn boxed(group: &[Line], frames: &[BBox]) -> Option<Vec<Vec<Line>>> {
    let held = holding(group, frames);
    let within = |b: &BBox, c: &BBox| c.x0 <= b.x0 && c.x1 >= b.x1 && c.y0 <= b.y0 && c.y1 >= b.y1;
    let mut outer: Vec<&(BBox, Vec<usize>)> = held
        .iter()
        .filter(|(b, _)| !held.iter().any(|(c, _)| c != b && within(b, c)))
        .collect();
    if outer.len() < 2 {
        return None;
    }
    for (i, (a, _)) in outer.iter().enumerate() {
        if outer[i + 1..].iter().any(|(b, _)| overlap(a, b)) {
            return None;
        }
    }
    let upright = group.iter().filter(|l| l.rotation_bucket == 0).count();
    if outer.iter().map(|(_, v)| v.len()).sum::<usize>() != upright {
        return None;
    }
    let text = |v: &[usize]| wordy(v.iter().map(|&i| &group[i]));
    if !outer.iter().any(|(_, v)| rows_of(group, v) >= 2)
        || outer
            .iter()
            .any(|(_, v)| rows_of(group, v) >= 2 && !text(v))
    {
        return None;
    }
    outer.sort_by(|p, q| {
        p.0.y0
            .partial_cmp(&q.0.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut rows: Vec<Vec<&(BBox, Vec<usize>)>> = Vec::new();
    for b in outer {
        match rows.last_mut() {
            Some(row) if boxes_level(&row[0].0, &b.0) => row.push(b),
            _ => rows.push(vec![b]),
        }
    }
    let mut out = Vec::new();
    for mut row in rows {
        row.sort_by(|p, q| {
            p.0.x0
                .partial_cmp(&q.0.x0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (_, v) in row {
            out.push(v.iter().map(|&i| group[i].clone()).collect());
        }
    }
    let turned: Vec<Line> = group
        .iter()
        .filter(|l| l.rotation_bucket != 0)
        .cloned()
        .collect();
    if !turned.is_empty() {
        out.push(turned);
    }
    Some(out)
}

/// Lines over a row of boxes, the boxes' lines, and the rest.
type FramedRow = (Vec<Line>, Vec<Vec<Line>>, Vec<Line>);

/// A row of two or more drawn boxes side by side in a stretch of a column,
/// level with each other ([`BOX_LEVEL`]), each holding text, one of them two
/// rows or more: two boxes of a flowchart, two charts' panels with their
/// titles set in bands. The region pass reads their lines row by row
/// across the boxes; each box is read whole instead, left to right. A box
/// holds the boxes inside it. Of such rows, the one holding most lines.
///
/// Not a table's cells drawn one box each, which touch across the row or
/// down a column ([`GRID_GAP`]): read box by box, a table's rows would read
/// as columns.
///
/// The lines over the row read before it, and so does one beside it on its
/// left, a title set level with a row of boxes. A box outside the row goes
/// whole, before or after it by its top.
fn framed_row(group: &[Line], frames: &[BBox], h: f64) -> Option<FramedRow> {
    let held = holding(group, frames);
    if held.len() < 2 {
        return None;
    }
    let apart = |a: &BBox, b: &BBox| a.x1 <= b.x0 || b.x1 <= a.x0;
    let near = |a: f64, b: f64| (a - b).abs() < GRID_GAP * h;
    let mut best: Option<(usize, Vec<usize>)> = None;
    for k in 0..held.len() {
        // Largest first: a box beside `k`'s before the boxes within it.
        let mut row: Vec<usize> = vec![k];
        for j in 0..held.len() {
            let b = &held[j].0;
            if j != k && boxes_level(&held[k].0, b) && row.iter().all(|&m| apart(&held[m].0, b)) {
                row.push(j);
            }
        }
        if row.len() < 2
            || !row.iter().any(|&m| rows_of(group, &held[m].1) >= 2)
            || !row
                .iter()
                .all(|&m| wordy(held[m].1.iter().map(|&i| &group[i])))
        {
            continue;
        }
        row.sort_by(|&p, &q| {
            held[p]
                .0
                .x0
                .partial_cmp(&held[q].0.x0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let across = row
            .windows(2)
            .any(|w| held[w[1]].0.x0 - held[w[0]].0.x1 < GRID_GAP * h);
        let down = row.iter().any(|&m| {
            let a = &held[m].0;
            held.iter().any(|(b, _)| {
                b != a
                    && (b.x0 - a.x0).abs() <= BOX_SLACK
                    && (b.x1 - a.x1).abs() <= BOX_SLACK
                    && (near(b.y0, a.y1) || near(a.y0, b.y1))
            })
        });
        if across || down {
            continue;
        }
        let n: usize = row.iter().map(|&m| held[m].1.len()).sum();
        if best.as_ref().is_none_or(|(most, _)| n > *most) {
            best = Some((n, row));
        }
    }
    let (_, row) = best?;
    let top = row.iter().map(|&m| held[m].0.y0).fold(f64::MAX, f64::min);
    let foot = row.iter().map(|&m| held[m].0.y1).fold(f64::MIN, f64::max);
    let first = held[row[0]].0.x0;
    let mut taken = vec![false; group.len()];
    let mut boxes: Vec<Vec<Line>> = Vec::new();
    for &m in &row {
        boxes.push(held[m].1.iter().map(|&i| group[i].clone()).collect());
        for &i in &held[m].1 {
            taken[i] = true;
        }
    }
    let (mut over, mut rest) = (Vec::new(), Vec::new());
    for (i, l) in group.iter().enumerate() {
        if taken[i] {
            continue;
        }
        // The largest box clear of the row's that holds the line.
        let outside = held
            .iter()
            .find(|(b, v)| v.contains(&i) && !row.iter().any(|&m| overlap(b, &held[m].0)))
            .map(|(b, _)| b.y0);
        let at = outside.unwrap_or((l.bbox.y0 + l.bbox.y1) * 0.5);
        if at < top || (at <= foot && l.bbox.x1 <= first) {
            over.push(l.clone());
        } else {
            rest.push(l.clone());
        }
    }
    Some((over, boxes, rest))
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
            italic: false,
            ocr: None,
            x_height: None,
            compounds: Vec::new(),
            opens_bold: false,
        }
    }

    /// A column of prose beside a shaded panel, the column's last paragraph
    /// running on under the panel across the page.
    fn page_with_sidebar() -> (Vec<Line>, BBox) {
        let mut lines: Vec<Line> = (0..8)
            .map(|i| {
                line(
                    &format!("the column of prose goes on here {i} words"),
                    50.0,
                    100.0 + 10.0 * i as f64,
                    330.0,
                )
            })
            .collect();
        let panel = BBox {
            x0: 355.0,
            y0: 95.0,
            x1: 545.0,
            y1: 175.0,
        };
        for i in 0..4 {
            lines.push(line(
                &format!("Highlight item number {i}"),
                360.0,
                102.0 + 12.0 * i as f64,
                520.0,
            ));
        }
        lines.push(line(
            "still going on under the panel across the page.",
            50.0,
            180.0,
            540.0,
        ));
        lines.push(line(
            "A new paragraph across the page opens here",
            50.0,
            195.0,
            540.0,
        ));
        lines.sort_by(|a, b| a.bbox.y0.total_cmp(&b.bbox.y0));
        (lines, panel)
    }

    #[test]
    fn a_panel_beside_the_column_reads_after_the_text_beside_it() {
        let (lines, panel) = page_with_sidebar();
        let out = texts(&read_order_with_panels(
            vec![lines],
            &[],
            &[],
            &[],
            &[panel],
            &[panel],
        ));
        let at = |t: &str| out.iter().position(|l| l.starts_with(t)).unwrap();
        // The column, then the sentence running on under the panel, then
        // the panel, then what follows.
        assert!(at("the column of prose goes on here 7") < at("still going on"));
        assert!(at("still going on") < at("Highlight item number 0"));
        assert!(at("Highlight item number 3") < at("A new paragraph"));
        let items: Vec<usize> = (0..4)
            .map(|i| at(&format!("Highlight item number {i}")))
            .collect();
        assert!(
            items.windows(2).all(|w| w[1] == w[0] + 1),
            "the panel reads whole"
        );
    }

    /// A column of prose at 10pt beside a photo, its 8pt caption under the
    /// photo level with the column's last lines, the paragraph running on
    /// across the page under it.
    fn page_with_caption(caption_size: f32) -> (Vec<Line>, BBox) {
        let sized = |mut l: Line, fs: f32| {
            l.font_size = fs;
            l
        };
        let mut lines: Vec<Line> = (0..8)
            .map(|i| {
                sized(
                    line(
                        &format!("the column of prose goes on here {i} words"),
                        50.0,
                        100.0 + 12.0 * i as f64,
                        330.0,
                    ),
                    10.0,
                )
            })
            .collect();
        let photo = BBox {
            x0: 355.0,
            y0: 95.0,
            x1: 545.0,
            y1: 180.0,
        };
        lines.push(sized(
            line("The photo's caption, its first line", 360.0, 182.0, 540.0),
            caption_size,
        ));
        lines.push(sized(
            line("and its second line under it", 360.0, 191.0, 520.0),
            caption_size,
        ));
        lines.push(sized(
            line(
                "still going on under the photo across the page.",
                50.0,
                200.0,
                540.0,
            ),
            10.0,
        ));
        lines.push(sized(
            line(
                "A new paragraph across the page opens here",
                50.0,
                215.0,
                540.0,
            ),
            10.0,
        ));
        lines.sort_by(|a, b| a.bbox.y0.total_cmp(&b.bbox.y0));
        (lines, photo)
    }

    #[test]
    fn a_pictures_caption_beside_the_column_reads_after_its_sentence() {
        let (lines, photo) = page_with_caption(8.0);
        let out = texts(&read_order_with_panels(
            vec![lines],
            &[],
            &[],
            &[photo],
            &[],
            &[],
        ));
        let at = |t: &str| out.iter().position(|l| l.starts_with(t)).unwrap();
        // The column, its sentence running on under the photo, the caption
        // whole, then the next paragraph.
        assert!(at("the column of prose goes on here 7") < at("still going on"));
        assert!(at("still going on") < at("The photo's caption"));
        assert_eq!(at("and its second line"), at("The photo's caption") + 1);
        assert!(at("and its second line") < at("A new paragraph"));
        // A caption set at its text's size is left to the region pass, and
        // so is text under no picture.
        let (lines, photo) = page_with_caption(10.0);
        assert!(caption_beside(lines, &[photo]).is_err());
        let (lines, _) = page_with_caption(8.0);
        assert!(caption_beside(lines, &[]).is_err());
    }

    #[test]
    fn a_panel_at_a_columns_foot_reads_after_the_sentence_it_breaks() {
        // A journal's first column breaks off mid-sentence over its authors'
        // box, and the sentence goes on at the top of the second column.
        let mut lines = vec![
            line(
                "There is a small but growing demand for",
                60.0,
                100.0,
                213.0,
            ),
            line("calibration laboratories and research", 60.0, 110.0, 213.0),
            line("facilities to maintain a high accuracy", 60.0, 120.0, 213.0),
            line(
                "tory to continuously generate a 1 pulse",
                60.0,
                130.0,
                213.0,
            ),
            line("Michael A. Lombardi", 68.0, 150.0, 140.0),
            line("Time and Frequency Division", 68.0, 160.0, 167.0),
            line("National Institute of Standards", 68.0, 170.0, 173.0),
            line("Boulder, CO 80305 USA", 68.0, 180.0, 155.0),
            line(
                "per second (pps) on-time signal, and for",
                230.0,
                100.0,
                382.0,
            ),
            line(
                "laboratories in the United States, to be",
                230.0,
                110.0,
                382.0,
            ),
            line(
                "able to state the uncertainty of that signal.",
                230.0,
                120.0,
                382.0,
            ),
            line(
                "Once the uncertainty of the 1 pps signal",
                230.0,
                130.0,
                382.0,
            ),
            line("is known it can be used as a standard", 230.0, 140.0, 382.0),
        ];
        lines.sort_by(|a, b| a.bbox.y0.total_cmp(&b.bbox.y0));
        let panel = BBox {
            x0: 60.0,
            y0: 145.0,
            x1: 213.0,
            y1: 192.0,
        };
        let out = texts(&read_order_with_panels(
            vec![lines],
            &[],
            &[],
            &[],
            &[panel],
            &[panel],
        ));
        let at = |t: &str| out.iter().position(|l| l.starts_with(t)).unwrap();
        // The sentence whole, then the box whole, then the rest.
        assert!(at("tory to continuously") < at("per second"));
        assert!(at("able to state") < at("Michael A. Lombardi"));
        assert!(at("Boulder") < at("Once the uncertainty"));
        let names: Vec<usize> = ["Michael", "Time and", "National", "Boulder"]
            .iter()
            .map(|t| at(t))
            .collect();
        assert!(
            names.windows(2).all(|w| w[1] == w[0] + 1),
            "the box reads whole: {out:?}"
        );
    }

    #[test]
    fn a_box_in_a_column_of_its_own_is_no_sidebar() {
        // A heading over the box on its own side: the box is part of that
        // column.
        let (mut lines, panel) = page_with_sidebar();
        lines.push(line("A heading over the box", 360.0, 85.0, 500.0));
        assert!(sidebar(lines.clone(), &[panel]).is_err());
        // No panels, no sidebar.
        let (lines, _) = page_with_sidebar();
        assert!(sidebar(lines, &[]).is_err());
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
        let out = texts(&read_order(vec![left, right], &[], &[], &[], &[]));
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
        let out = read_order(vec![left, right], &[], &[], &[], &[]);
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
        let out = read_order(vec![left, right], &[], &[], &[], &[]);
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
        let out = texts(&read_order(vec![left, right], &[], &[], &[], &[]));
        let at = |t: &str| out.iter().position(|o| o.starts_with(t)).unwrap();
        assert!(at("right text 4") < at("Notes:"), "{out:?}");
        assert_eq!(at("Notes:") + 1, at("1 A note"), "{out:?}");
    }

    #[test]
    fn a_title_over_a_run_reads_first_and_with_it() {
        // A right-aligned display title: its first line wholly in the right
        // column, its second across the gutter, over two columns of text.
        let big = |t: &str, x0: f64, y: f64, x1: f64| {
            let mut l = line(t, x0, y, x1);
            l.font_size = 32.0;
            l.bbox.y1 = y + 23.0;
            l
        };
        let mut left = vec![big("LOOSEFILL INSULATION SYSTEM", 188.0, 63.0, 580.0)];
        left.extend(col("left", 30.0, 150.0, 6));
        let mut right = vec![big("PROFESSIONAL PRODUCT", 279.0, 29.0, 581.0)];
        right.extend(col("right", 230.0, 150.0, 6));
        let out = read_order(vec![left, right], &[], &[], &[], &[]);
        assert_eq!(
            texts(&out)[..2],
            ["PROFESSIONAL PRODUCT", "LOOSEFILL INSULATION SYSTEM"],
            "{:?}",
            texts(&out)
        );
        assert_eq!(out[0].lines.len(), 2, "one group: {:?}", texts(&out));
    }

    #[test]
    fn a_caption_over_a_run_is_no_title() {
        // The same lines in bold at the text's size, a caption's: longer
        // than a heading, so not read as one title.
        let bold = |t: &str, x0: f64, y: f64, x1: f64| {
            let mut l = line(t, x0, y, x1);
            l.bold = true;
            l
        };
        let one = "Figure 1. Mean values of the four measures taken over the whole year";
        let two = "for each of the twelve sites in the north and south of the region";
        let mut left = vec![bold(two, 188.0, 40.0, 580.0)];
        left.extend(col("left", 30.0, 80.0, 6));
        let mut right = vec![bold(one, 279.0, 30.0, 581.0)];
        right.extend(col("right", 230.0, 80.0, 6));
        let out = read_order(vec![left, right], &[], &[], &[], &[]);
        assert!(
            out.iter().all(|g| g.lines.len() < 2
                || !(g.lines.iter().any(|l| l.text == one)
                    && g.lines.iter().any(|l| l.text == two))),
            "{:?}",
            texts(&out)
        );
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
        let groups = read_order(vec![lines], &[], &[], &[], &[]);
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

    /// Two columns of running text, `n` lines each, from `y`: lines that
    /// go on in the next, at 72–290 and 307–525.
    fn two_columns(y: f64, n: usize) -> Vec<Line> {
        let mut out = Vec::new();
        for i in 0..n {
            let y = y + 12.0 * i as f64;
            out.push(line(
                &format!("left column goes on here {i}"),
                72.0,
                y,
                290.0,
            ));
            out.push(line(
                &format!("right column goes on here {i}"),
                307.0,
                y,
                525.0,
            ));
        }
        out
    }

    fn reads_columns_whole(out: &[String], n: usize) {
        let left: Vec<usize> = (0..n)
            .map(|i| {
                out.iter()
                    .position(|t| *t == format!("left column goes on here {i}"))
                    .unwrap()
            })
            .collect();
        let right: Vec<usize> = (0..n)
            .map(|i| {
                out.iter()
                    .position(|t| *t == format!("right column goes on here {i}"))
                    .unwrap()
            })
            .collect();
        assert!(left.windows(2).all(|w| w[1] == w[0] + 1), "{out:?}");
        assert!(right.windows(2).all(|w| w[1] == w[0] + 1), "{out:?}");
        assert!(left[n - 1] < right[0], "{out:?}");
    }

    #[test]
    fn a_page_number_under_two_columns_parts_nothing() {
        // The page's number, set under the gutter, is the one line across
        // it: the columns read each whole, the number after them.
        // One line's space under the columns, as a page's foot is set.
        let mut lines = two_columns(100.0, 6);
        lines.push(line("7872", 288.0, 172.0, 309.0));
        let groups = read_order(vec![lines], &[], &[], &[], &[]);
        let out = texts(&groups);
        reads_columns_whole(&out, 6);
        assert_eq!(out.last().map(String::as_str), Some("7872"));
    }

    #[test]
    fn a_title_wrapped_over_two_columns_reads_whole_before_them() {
        // Bold, over both columns, its second line crossing the gutter
        // (`adversely affecting young people.`) or hanging short of it
        // under its text past a number (`moyen à l'accouchement …`).
        for (second, x0, x1) in [
            ("adversely affecting the young", 86.0, 307.0),
            ("moyen depuis 2005", 82.0, 200.0),
        ] {
            let mut first = line(
                "4. Labour force participation continues to fall,",
                72.0,
                76.0,
                370.0,
            );
            first.bold = true;
            let mut next = line(second, x0, 86.0, x1);
            next.bold = true;
            let mut lines = vec![first, next];
            lines.extend(two_columns(100.0, 6));
            let groups = read_order(vec![lines], &[], &[], &[], &[]);
            let out = texts(&groups);
            assert_eq!(out[1], second, "{out:?}");
            reads_columns_whole(&out, 6);
            // The title's two lines are read together.
            assert!(groups
                .iter()
                .any(|g| g.lines.len() == 2 && g.lines[1].text == second));
        }
    }

    #[test]
    fn columns_under_a_caption_across_read_each_whole() {
        // A figure's labels across the gutter, its caption across both
        // columns, and two columns of text under it, all at one leading: the
        // columns read each whole, though no gutter runs through the figure.
        let mut lines = vec![
            line("Dataset Examples", 200.0, 40.0, 330.0),
            line("ImageNet 76.2 76.2 0%", 230.0, 52.0, 520.0),
            line("ImageNetV2 64.3 70.1 +5.8%", 230.0, 64.0, 520.0),
            line(
                "Figure 7. Zero-shot models are much more robust to distribution",
                72.0,
                76.0,
                525.0,
            ),
            line(
                "shift than the standard models trained on the one dataset.",
                72.0,
                88.0,
                525.0,
            ),
        ];
        lines.extend(two_columns(100.0, 10));
        let groups = read_order(vec![lines], &[], &[], &[], &[]);
        let out = texts(&groups);
        reads_columns_whole(&out, 10);
        assert_eq!(
            out[4],
            "shift than the standard models trained on the one dataset."
        );
    }

    #[test]
    fn a_captions_last_line_beside_a_sideways_label_goes_on_from_it() {
        // A diagram's label set sideways runs down past every row beside the
        // caption and the columns. It stands on none of those rows, so the
        // caption's short last line is still alone on its row, and is read
        // with the line it goes on from, not as the left column's first.
        let mut lines = vec![
            line(
                "Figure 3. Timing of the cycle across both of the columns here",
                72.0,
                40.0,
                525.0,
            ),
            line("and its standby period.", 72.0, 50.0, 200.0),
        ];
        let mut label = line("Measurement T Measurement T", 535.0, 30.0, 543.0);
        label.rotation_bucket = 90;
        label.bbox.y1 = 200.0;
        lines.push(label);
        lines.extend(two_columns(70.0, 8));
        let groups = read_order(vec![lines], &[], &[], &[], &[]);
        reads_columns_whole(&texts(&groups), 8);
        assert!(
            groups.iter().any(|g| {
                g.lines.iter().any(|l| l.text.starts_with("Figure 3."))
                    && g.lines.iter().any(|l| l.text == "and its standby period.")
            }),
            "{:?}",
            groups
                .iter()
                .map(|g| texts(std::slice::from_ref(g)))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_captions_block_across_two_short_columns_parts_them_once() {
        // A figure's caption wraps to six lines across both columns over
        // eight rows of two-column text. Counted line by line, six lines
        // across outnumbered a region this size's allowance; as one run of
        // lines across, it parts the region once.
        let mut lines = Vec::new();
        for i in 0..6 {
            lines.push(line(
                &format!("Figure 2. Bar graphs and scatterplots showing the caption {i}"),
                72.0,
                40.0 + 9.0 * i as f64,
                525.0,
            ));
        }
        lines.extend(two_columns(100.0, 8));
        let groups = read_order(vec![lines], &[], &[], &[], &[]);
        let out = texts(&groups);
        reads_columns_whole(&out, 8);
        assert!(out[0].starts_with("Figure 2."), "{out:?}");
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
        let out = texts(&read_order(vec![left, right], &rules, &[], &[], &[]));
        let at = |t: &str| out.iter().position(|o| o.starts_with(t)).unwrap();
        assert!(at("left item 7") < at("right feature 0"), "{out:?}");
    }

    #[test]
    fn a_rule_level_with_a_photo_parts_nothing_beside_it() {
        // A photo at the left over a heading and its list; at the right,
        // benefits with a rule under each. The rules stand level with the
        // photo, where the left column has no text.
        let mut left = col("intro", 40.0, 60.0, 3);
        left.push(line("Best-Fit Applications", 40.0, 214.0, 180.0));
        left.extend(col("left item", 40.0, 228.0, 6));
        let right: Vec<Line> = (0..8)
            .map(|i| {
                line(
                    &format!("right benefit {i}"),
                    220.0,
                    100.0 + 24.0 * i as f64,
                    520.0,
                )
            })
            .collect();
        let rules: Vec<Rule> = (0..7)
            .map(|i| rule(220.0, 568.0, 112.0 + 24.0 * i as f64))
            .collect();
        let photo = BBox {
            x0: 40.0,
            y0: 92.0,
            x1: 178.0,
            y1: 210.0,
        };
        let out = texts(&read_order(vec![left, right], &rules, &[], &[photo], &[]));
        let at = |t: &str| out.iter().position(|o| o.starts_with(t)).unwrap();
        assert!(at("left item 5") < at("right benefit 0"), "{out:?}");
    }

    #[test]
    fn a_gap_inside_a_section_beside_the_next_title_parts_nothing() {
        // Two sections open side by side under a running head. The left
        // one's text has a gap in it, level with the right column's next
        // title; the left column's text under the gap is still its own.
        let big = |t: &str, x: f64, y: f64| {
            let mut l = line(t, x, y, x + 150.0);
            l.font_size = 13.0;
            l.bbox.y1 = y + 13.0;
            l
        };
        let mut left = vec![big("Codes/Standards", 40.0, 90.0)];
        left.extend(col("compliance", 40.0, 110.0, 2));
        left.extend(col("standard", 40.0, 160.0, 8));
        // The gap under the running head is no wider than the one inside.
        let mut right = vec![line("Running head", 300.0, 55.0, 450.0)];
        right.push(big("Recommended Products", 300.0, 90.0));
        right.extend(col("product", 300.0, 110.0, 1));
        right.push(big("Warranty", 300.0, 155.0));
        right.extend(col("warranty", 300.0, 175.0, 2));
        let out = texts(&read_order(vec![left, right], &[], &[], &[], &[]));
        let at = |t: &str| out.iter().position(|o| o.starts_with(t)).unwrap();
        assert!(at("standard 7") < at("Recommended"), "{out:?}");
    }

    #[test]
    fn a_gap_under_which_every_column_opens_a_title_parts_them() {
        // An article in two languages side by side, then the next: each
        // column opens a title under the gap. The English column opens on
        // the end of the article before.
        let head = |t: &str, x: f64, y: f64| {
            let mut l = line(t, x, y, x + 150.0);
            l.bold = true;
            l
        };
        let mut left = vec![head("ARTICLE 9 - MATERIEL", 40.0, 80.0)];
        left.extend(col("fr nine", 40.0, 100.0, 4));
        left.push(head("ARTICLE 10 - CONFIDENTIALITE", 40.0, 180.0));
        left.extend(col("fr ten", 40.0, 200.0, 4));
        let mut right = vec![line("carried over", 220.0, 30.0, 370.0)];
        right.push(head("ARTICLE 9 - EQUIPMENT", 220.0, 80.0));
        right.extend(col("en nine", 220.0, 100.0, 4));
        right.push(head("ARTICLE 10 - CONFIDENTIALITY", 220.0, 180.0));
        right.extend(col("en ten", 220.0, 200.0, 4));
        let out = texts(&read_order(vec![left, right], &[], &[], &[], &[]));
        let at = |t: &str| out.iter().position(|o| o.starts_with(t)).unwrap();
        assert!(
            at("en nine 3") < at("ARTICLE 10 - CONFIDENTIALITE"),
            "{out:?}"
        );
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
        let out = read_order(vec![lines], &[rule(40.0, 380.0, 129.0)], &[], &[], &[]);
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
        let out = texts(&read_order(vec![first, second, third], &[], &[], &[], &[]));
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
        let out = texts(&read_order(vec![lines], &[], &[], &[], &[]));
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
        let out = texts(&read_order(vec![lines], &[], &[], &[], &[]));
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
        let out = texts(&read_order(vec![lines], &[], &[], &[], &[]));
        assert_eq!(out[3], "300 lb/136 kg", "{out:?}");
    }

    /// Lines from `y`, one every `pitch`, at `x`, `w` wide.
    fn block(texts: &[&str], x: f64, y: f64, pitch: f64, w: f64) -> Vec<Line> {
        texts
            .iter()
            .enumerate()
            .map(|(i, t)| line(t, x, y + pitch * i as f64, x + w))
            .collect()
    }

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> BBox {
        BBox { x0, y0, x1, y1 }
    }

    #[test]
    fn blocks_set_on_their_own_baselines_read_each_whole() {
        // A delivery address at the left and the buyer's at the right, each
        // on its own leading: their lines drift through each other.
        let mut lines = block(
            &[
                "Delivery address",
                "Ma jolie boutique",
                "35 rue Victor Hugo",
                "69001 Lyon France",
                "Phone 04 72 07 08",
            ],
            36.0,
            100.0,
            11.5,
            120.0,
        );
        lines.extend(block(
            &[
                "Ma jolie boutique",
                "Alexandre Payet here",
                "35 rue Victor Hugo",
                "69001 Lyon France",
            ],
            300.0,
            106.0,
            12.6,
            120.0,
        ));
        lines.sort_by(|a, b| a.bbox.y0.partial_cmp(&b.bbox.y0).unwrap());
        let out = texts(&read_order(vec![lines], &[], &[], &[], &[]));
        assert_eq!(
            out[..5]
                .iter()
                .filter(|t| *t != "Alexandre Payet here")
                .count(),
            5,
            "{out:?}"
        );
        assert_eq!(out[5], "Ma jolie boutique", "{out:?}");
        assert_eq!(out[6], "Alexandre Payet here", "{out:?}");
    }

    #[test]
    fn a_block_running_on_past_the_one_beside_it_reads_whole() {
        // The seller's six lines beside the customer's three, level with
        // its first three.
        let mut lines = block(
            &[
                "Seller company b.v.",
                "Main street 100, Utrecht",
                "Chamber of Commerce 3214",
                "VAT no. NL8204716",
                "IBAN NL27 INGB 0000",
                "BIC code INGBNL2A",
            ],
            36.0,
            100.0,
            10.0,
            150.0,
        );
        lines.extend(block(
            &["Mr. M. Customer", "Kapittel road 409", "1216 JD Hilversum"],
            300.0,
            100.0,
            10.0,
            100.0,
        ));
        lines.sort_by(|a, b| a.bbox.y0.partial_cmp(&b.bbox.y0).unwrap());
        let out = texts(&read_order(vec![lines], &[], &[], &[], &[]));
        assert_eq!(out[5], "BIC code INGBNL2A", "{out:?}");
        assert_eq!(out[6], "Mr. M. Customer", "{out:?}");
    }

    #[test]
    fn labels_beside_their_values_stay_rows() {
        // Labels whose last ones have no value: a form, read row by row.
        let mut lines = block(
            &[
                "Invoice date:",
                "Due date:",
                "Order number:",
                "Customer ref:",
                "Delivery note:",
            ],
            36.0,
            100.0,
            10.0,
            70.0,
        );
        lines.extend(block(
            &["29 October 2015", "12 November 2015", "Order 8587 3338"],
            200.0,
            100.0,
            10.0,
            90.0,
        ));
        lines.sort_by(|a, b| {
            a.bbox
                .y0
                .partial_cmp(&b.bbox.y0)
                .unwrap()
                .then(a.bbox.x0.partial_cmp(&b.bbox.x0).unwrap())
        });
        let out = texts(&read_order(vec![lines], &[], &[], &[], &[]));
        assert_eq!(out[1], "29 October 2015", "{out:?}");
    }

    #[test]
    fn boxes_side_by_side_read_each_whole() {
        // Two boxes of a flowchart, their lines level across the row.
        let mut lines = block(
            &["With comorbidity", "(N=44,770) patients"],
            200.0,
            215.0,
            9.0,
            57.0,
        );
        lines.extend(block(
            &["Without comorbidity", "(N=57,006) patients"],
            366.0,
            215.0,
            9.0,
            68.0,
        ));
        lines.push(line(
            "Figure 1: Flowchart of the study population.",
            150.0,
            245.0,
            400.0,
        ));
        lines.sort_by(|a, b| a.bbox.y0.partial_cmp(&b.bbox.y0).unwrap());
        let boxes = [
            rect(184.0, 211.0, 270.0, 233.0),
            rect(355.0, 211.0, 441.0, 233.0),
        ];
        let out = texts(&read_order(vec![lines.clone()], &[], &[], &[], &boxes));
        assert_eq!(out[1], "(N=44,770) patients", "{out:?}");
        assert_eq!(out[2], "Without comorbidity", "{out:?}");
        // A table's cells drawn one box each touch: their rows stay rows.
        let cells = [
            rect(184.0, 211.0, 270.0, 233.0),
            rect(270.0, 211.0, 441.0, 233.0),
        ];
        assert!(framed_row(&lines, &cells, 8.0).is_none());
    }

    #[test]
    fn a_flowchart_reads_box_by_box_in_flow_order() {
        // A step, a branch off it at the left set lower, and the next step.
        let mut lines = block(
            &["Lung cancer patients", "during 1995-2010"],
            280.0,
            77.0,
            9.0,
            66.0,
        );
        lines.extend(block(
            &[
                "Exclusion criteria:",
                "1. Ambulatory visits <5",
                "2. Comorbidity before",
            ],
            164.0,
            107.0,
            9.0,
            73.0,
        ));
        lines.extend(block(
            &["Lung cancer cohort", "selected in this study"],
            280.0,
            157.0,
            9.0,
            66.0,
        ));
        let boxes = [
            rect(270.0, 74.0, 356.0, 105.0),
            rect(158.0, 104.0, 244.0, 162.0),
            rect(270.0, 154.0, 356.0, 185.0),
        ];
        let mut sorted = lines.clone();
        sorted.sort_by(|a, b| a.bbox.y0.partial_cmp(&b.bbox.y0).unwrap());
        let out = texts(&read_order(vec![sorted], &[], &[], &[], &boxes));
        let at = |t: &str| out.iter().position(|o| o == t).unwrap();
        assert!(
            at("during 1995-2010") < at("Exclusion criteria:"),
            "{out:?}"
        );
        assert!(
            at("2. Comorbidity before") < at("Lung cancer cohort"),
            "{out:?}"
        );
    }
}
