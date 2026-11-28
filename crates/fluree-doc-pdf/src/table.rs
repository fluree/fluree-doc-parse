//! Table grid detection from ruling lines.
//!
//! The key observation, found by dumping a table's raw geometry rather than
//! trusting how it looks: a table that reads as rule-less on the page is
//! **not** actually rule-less.
//! Its grid is drawn as *per-cell segments*, one short rule per cell edge:
//!
//! ```text
//! Horizontal axis=77.5  (68,77)-(183,78)
//! Horizontal axis=77.5  (183,77)-(297,78)
//! Horizontal axis=77.5  (297,77)-(412,78)
//! Horizontal axis=77.5  (412,77)-(528,78)
//! ```
//!
//! Read individually those are four short lines and nothing looks like a table.
//! Clustered by axis position they are one horizontal grid line, and their
//! endpoints — 68, 183, 297, 412, 528 — are exactly the four column boundaries.
//!
//! Every engine we benchmarked failed on this table — one recovered 3 of 4
//! columns, one lost the header row, one collapsed it to a single column —
//! while working from a grid that was fully present in the file.

use crate::geom::BBox;
use crate::rule::{Orientation, Rule};
use fluree_doc_model::merges::Merges;
pub use fluree_doc_model::merges::{denormalize, Merges as CellMerges};

/// Rules within this distance on their perpendicular axis belong to the same
/// grid line, in PDF units. Generous enough for hairline stroke offsets.
const AXIS_TOLERANCE: f64 = 2.5;

/// Endpoints within this distance are the same grid position.
pub(crate) const EDGE_TOLERANCE: f64 = 3.0;

/// Fewest lines of text, between two rules that share no stretch of the
/// page and under one of them, that are a column's running text and not a
/// table's last row. Two tables set side by side end on rules a little
/// apart, and between those rules stands what the longer one runs on past
/// the other: its last row's wrapped lines. A cell wraps to two or three
/// lines in a table narrow enough to stand beside another; more is a
/// section's list or paragraphs between the dividers of a page set in
/// columns.
const SECTION_LINES: usize = 4;

/// A grid needs at least this many lines each way. Two horizontals and two
/// verticals is a single cell — a box, not a table.
const MIN_GRID_LINES: usize = 3;

/// Furthest, in sizes, a lone currency sign reaches to the figure it
/// prices: a statement pins its sign at the column's left edge.
const CURRENCY_REACH: f64 = 8.0;

/// Widest gap, in sizes, between a figure and a currency sign set after it
/// as its own: a word space, breaking or not.
const SIGN_SPACE: f64 = 0.6;

/// Narrowest cell that can hold content, in PDF units. Boundaries closer than
/// this are a border rule adjacent to a cell edge, not two cells.
const MIN_CELL_EXTENT: f64 = 8.0;

/// Least distance, in points, from a cell's edge for a rule drawn down
/// through the cell to part its text ([`Grid::cell_texts_parted`]). A
/// column's own rule stands on its edge, within a stroke's width of it.
const PARTING_RULE_MARGIN: f64 = 3.0;

/// Fewest lines, and fewest words in one of them, each side of a rule
/// parting a cell holds ([`Grid::cell_texts_parted`]): a box of text runs
/// over its lines in phrases, where a small table's column holds a value or
/// a word a row.
const PARTED_SIDE_LINES: usize = 2;
const PARTED_LINE_WORDS: usize = 3;

/// How far, in points, a glyph's ink may reach over a parting rule: the
/// rule's stroke and the glyph's side bearing.
const RULE_CLEARANCE: f64 = 0.5;

/// Fraction of the grid's span a clustered line must cover to count. A short
/// rule under a heading is not a table row boundary.
const MIN_LINE_COVERAGE: f64 = 0.30;

/// Fraction of horizontal grid lines whose segment endpoints must agree on a
/// column boundary before it counts.
///
/// This is what separates a table from a chart. In a real segmented grid every
/// horizontal line breaks at the *same* x positions — one such table's
/// endpoints 68/183/297/412/528 repeat on all six lines. A bar chart also has
/// several long horizontal gridlines, but their endpoints are just the plot
/// edges, so no interior x is supported. Without this test a chart's gridlines
/// formed a full-page 9x2 "table" that shredded the entire document into cells.
const MIN_BOUNDARY_SUPPORT: f64 = 0.5;

#[derive(Debug, Clone)]
pub struct Grid {
    pub page: usize,
    /// Column boundaries, ascending. `n` boundaries means `n-1` columns.
    pub xs: Vec<f64>,
    /// Row boundaries, ascending.
    pub ys: Vec<f64>,
    pub bbox: BBox,
    /// The header, spans and bands of a table read from its text
    /// ([`crate::textgrid`]), which knows them as it finds the rows; a
    /// ruled grid leaves them to be read from its ruling.
    pub layout: Option<std::sync::Arc<TableLayout>>,
}

impl TableLayout {
    /// Each cell's text, a span's read whole into its first cell.
    ///
    /// Blank glyphs off the cell's ink, above or under it, are left out: a
    /// line's spaces under the cell (a description's under a row of
    /// figures) are no line of it, and read with it they were a line of
    /// gaps that parted the cell's own line as a gutter would
    /// (`26.0 7.2012`, `Delivery da te`).
    pub fn texts(&self, glyphs: &[crate::glyph::Glyph], cols: usize) -> Vec<String> {
        let mut out: Vec<String> = self
            .cells
            .iter()
            .map(|ids| {
                let inked: Vec<&crate::glyph::Glyph> = ids
                    .iter()
                    .map(|&i| &glyphs[i])
                    .filter(|g| !g.text.trim().is_empty())
                    .collect();
                // The cell's ink, top to foot: a space set between a
                // subscript and the line it hangs from (`ŜrT (%)`) is in it.
                let span = inked.iter().filter_map(|g| {
                    let b = g.bbox?;
                    let slack = f64::from(g.font_size.max(1.0)) * 0.3;
                    Some((b.y0 - slack, b.y1 + slack))
                });
                let (top, foot) =
                    span.fold((f64::MAX, f64::MIN), |(t, f), (a, b)| (t.min(a), f.max(b)));
                let in_ink = |g: &crate::glyph::Glyph| g.origin.1 >= top && g.origin.1 <= foot;
                let gs: Vec<crate::glyph::Glyph> = ids
                    .iter()
                    .map(|&i| &glyphs[i])
                    .filter(|g| !g.text.trim().is_empty() || inked.is_empty() || in_ink(g))
                    .cloned()
                    .collect();
                join_cell_lines(crate::line::assemble(&gs).iter().map(|l| l.text.as_str()))
            })
            .collect();
        for &(r, c0, c1) in &self.spans {
            let parts: Vec<String> = (c0..c1.min(cols))
                .map(|c| std::mem::take(&mut out[r * cols + c]))
                .filter(|t| !t.trim().is_empty())
                .collect();
            out[r * cols + c0] = parts.join(" ");
        }
        out
    }

    /// The spans as a grid's merges: each span's cells continue its first.
    pub fn merges(&self, rows: usize, cols: usize) -> Merges {
        let mut m = Merges {
            continues_above: vec![false; rows * cols],
            continues_left: vec![false; rows * cols],
            full_width_row: vec![false; rows],
        };
        for &(r, c0, c1) in &self.spans {
            if r >= rows {
                continue;
            }
            for c in (c0 + 1)..c1.min(cols) {
                m.continues_left[r * cols + c] = true;
            }
            if c0 == 0 && c1 >= cols {
                m.full_width_row[r] = true;
            }
        }
        for &(r0, c, r1) in &self.downs {
            if c >= cols {
                continue;
            }
            for r in (r0 + 1)..r1.min(rows) {
                m.continues_above[r * cols + c] = true;
            }
        }
        m
    }
}

/// What a table read from its text says of its own structure.
#[derive(Debug, Clone, Default)]
pub struct TableLayout {
    /// Rows at the top that name the columns.
    pub header_rows: usize,
    /// Cells read as one: `(row, first column, end column)`.
    pub spans: Vec<(usize, usize, usize)>,
    /// Spans whose value is each of their columns', set centred across the
    /// gutter between them: `(row, first column)`.
    pub spread: Vec<(usize, usize)>,
    /// Header labels set across the header's rows, read into the top one:
    /// `(first row, column, end row)`.
    pub downs: Vec<(usize, usize, usize)>,
    /// Body rows that label the rows beneath them.
    pub sub_headers: Vec<usize>,
    /// Body rows that name the columns again for the rows beneath them, in
    /// the header's lowest row's place: a block's own line of column names.
    pub restated_headers: Vec<usize>,
    /// Per cell, row-major, the page glyphs read into it.
    pub cells: Vec<Vec<usize>>,
    /// The `draw_index` of every glyph the cells hold, sorted: the glyphs the
    /// table takes from the page. A glyph inside its box that no cell holds
    /// (a note set on a row's baseline beside it) stays the page's.
    pub claimed: Vec<usize>,
}

impl Grid {
    /// Assemble the text of every cell, in row-major order.
    ///
    /// Glyphs are routed into cells and lines are assembled *within* each cell.
    /// Routing whole page-lines instead interleaves neighbouring columns —
    /// `"More than 10 years Less than 1 % of or five to 10 years"` — because a
    /// page-level line spans the full grid width. This is the same ordering
    /// constraint as column segmentation: segment first, assemble second.
    pub fn cell_texts(&self, glyphs: &[crate::glyph::Glyph]) -> Vec<String> {
        crate::cells::texts(glyphs, &self.xs, &self.ys, || self.read_cell_texts(glyphs))
    }

    /// Each cell's text as [`Self::cell_texts`] reads it, but a cell a drawn
    /// rule parts is read a side at a time.
    ///
    /// A rule drawn down through a cell, with text on both sides of it
    /// within its height, parts two boxes the grid's columns do not: a form
    /// sizes each row's boxes on its own, so a rule drawn in one row only is
    /// no column of the grid. Read as one, the boxes' lines were joined
    /// across the rule, one line of each by turns (`Ordered on behalf of the
    /// Secretary of LONDON 102 PETTY FRANCE (MOJ), Ministry of … State for
    /// Justice:`). Read a side at a time: the lines over the rule, each
    /// side's, left to right, then the lines under it.
    pub fn cell_texts_parted(&self, glyphs: &[crate::glyph::Glyph], rules: &[Rule]) -> Vec<String> {
        let mut texts = self.cell_texts(glyphs);
        let cols = self.cols();
        let homes = self.glyph_cells(glyphs);
        for (cell, text) in texts.iter_mut().enumerate() {
            let ids: Vec<usize> = (0..glyphs.len())
                .filter(|&i| homes[i] == Some(cell))
                .collect();
            let (x0, x1) = (self.xs[cell % cols], self.xs[cell % cols + 1]);
            if let Some(t) = parted_text(glyphs, &ids, rules, x0, x1) {
                *text = t;
            }
        }
        texts
    }

    /// The text of a merged block of cells, rows `rows` by columns `cols`,
    /// read a side at a time where a rule drawn down through it parts it, as
    /// [`Self::cell_texts_parted`] reads a cell. `None` where no rule does.
    pub fn region_parted(
        &self,
        glyphs: &[crate::glyph::Glyph],
        rules: &[Rule],
        rows: std::ops::Range<usize>,
        cols: std::ops::Range<usize>,
    ) -> Option<String> {
        let n = self.cols();
        let homes = self.glyph_cells(glyphs);
        let ids: Vec<usize> = (0..glyphs.len())
            .filter(|&i| {
                homes[i].is_some_and(|h| rows.contains(&(h / n)) && cols.contains(&(h % n)))
            })
            .collect();
        parted_text(glyphs, &ids, rules, self.xs[cols.start], self.xs[cols.end])
    }

    fn read_cell_texts(&self, glyphs: &[crate::glyph::Glyph]) -> Vec<String> {
        let mut buckets: Vec<Vec<crate::glyph::Glyph>> =
            vec![Vec::new(); self.rows() * self.cols()];
        for (g, home) in glyphs.iter().zip(self.glyph_cells(glyphs)) {
            if let Some(cell) = home {
                buckets[cell].push(g.clone());
            }
        }
        buckets
            .iter()
            .map(|gs| {
                join_cell_lines(
                    turned_in_order(crate::line::assemble(gs))
                        .iter()
                        .map(|l| l.text.as_str()),
                )
            })
            .collect()
    }

    /// Which of `glyphs` the grid takes from the page's prose: each glyph
    /// its cells read ([`Self::glyph_cells`]), and none they leave out.
    ///
    /// A cell reads a word whole, in the cell holding its centre of mass,
    /// and the prose was given every glyph whose own centre fell outside the
    /// grid. A word an edge cut through was then read twice or lost: with
    /// its centre of mass inside, its letters outside were in the prose as
    /// well (`WARNING!` in the cell, `WAR` in the paragraph); with it
    /// outside, its letters inside were in neither (`See` read `S`). A
    /// glyph a cell never considers, without ink, still goes by where its
    /// own centre is; a turned glyph no cell reads stays in the prose.
    pub(crate) fn takes(&self, glyphs: &[crate::glyph::Glyph]) -> Vec<bool> {
        glyphs
            .iter()
            .zip(self.glyph_cells(glyphs))
            .map(|(g, home)| {
                if home.is_some() {
                    return true;
                }
                // A turned glyph no cell reads stays in the prose: a label
                // set on its side across the grid is still text.
                if g.bbox.is_some() && !g.text.trim().is_empty() && !g.is_horizontal() {
                    return false;
                }
                let read = g.bbox.is_some() && !g.text.trim().is_empty() && g.is_horizontal();
                let (x, y) = g
                    .bbox
                    .map_or(g.origin, |b| ((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5));
                !read && self.cell_at(x, y).is_some()
            })
            .collect()
    }

    /// The cell each glyph is read into, as `row * cols + col`; `None` for
    /// a glyph outside the grid or set across it.
    pub(crate) fn glyph_cells(&self, glyphs: &[crate::glyph::Glyph]) -> Vec<Option<usize>> {
        crate::cells::homes(glyphs, &self.xs, &self.ys, || self.read_glyph_cells(glyphs))
    }

    fn read_glyph_cells(&self, glyphs: &[crate::glyph::Glyph]) -> Vec<Option<usize>> {
        let cols = self.cols();
        let mut home: Vec<Option<usize>> = vec![None; glyphs.len()];

        // Assign *words*, not glyphs. A per-glyph assignment lets a column
        // boundary slice through a word whenever an inferred boundary lands
        // inside a long value — right-aligned money columns are the classic
        // case: `$ 7,8 | 35,559`. A pen-contiguous run is indivisible; it
        // goes to the cell holding its centre of mass.
        // Rotated glyphs are excluded: a cell's text is set in the table's
        // own reading direction, so 90°-turned glyphs crossing the grid are a
        // sidebar or watermark drawn over it, not content. Sorting them in by
        // origin interleaves their letters through real cell text
        // (`S Investment Property`, `ec Single family`), and no downstream
        // furniture scrub can recover the split.
        let mut idx: Vec<usize> = (0..glyphs.len())
            .filter(|&i| {
                glyphs[i].bbox.is_some()
                    && !glyphs[i].text.trim().is_empty()
                    && glyphs[i].is_horizontal()
            })
            .collect();
        idx.sort_by(|&a, &b| glyphs[a].origin.1.total_cmp(&glyphs[b].origin.1));
        // Then left to right within each line. A line's baselines jitter by
        // fractions of a point, and a label is set a little higher or lower
        // than its row's others, so sorting by baseline alone interleaves the
        // words of one line: `Co` and `lor`, a column apart, with another
        // word between them.
        let mut start = 0;
        while start < idx.len() {
            let first = &glyphs[idx[start]];
            let reach = first.font_size.max(1.0) as f64 * 0.3;
            let end = (start..idx.len())
                .find(|&k| glyphs[idx[k]].origin.1 - first.origin.1 >= reach)
                .unwrap_or(idx.len());
            idx[start..end].sort_by(|&a, &b| glyphs[a].origin.0.total_cmp(&glyphs[b].origin.0));
            start = end;
        }
        let mut runs: Vec<Vec<usize>> = Vec::new();
        let mut run: Vec<usize> = Vec::new();
        for &i in &idx {
            let g = &glyphs[i];
            if let Some(&prev) = run.last() {
                let pg = &glyphs[prev];
                let fs = g.font_size.max(pg.font_size).max(1.0) as f64;
                let same_line = (g.origin.1 - pg.origin.1).abs() < fs * 0.3;
                let pen_end = pg
                    .advance
                    .map(|a| pg.origin.0 + a)
                    .unwrap_or(pg.bbox.unwrap().x1);
                let start = g.bbox.unwrap().x0.min(g.origin.0);
                // Backwards is a break too: where two lines' glyphs fall
                // within one line's reach of each other, the next glyph can
                // start well behind the pen.
                let gap = start - pen_end;
                if !same_line || gap > fs * 0.2 || gap < -fs * 0.5 {
                    runs.push(std::mem::take(&mut run));
                }
            }
            run.push(i);
        }
        if !run.is_empty() {
            runs.push(run);
        }
        let lone_sign = |run: &[usize]| {
            run.len() == 1 && matches!(glyphs[run[0]].text.trim(), "$" | "€" | "£" | "¥")
        };
        let numeric = |run: &[usize]| {
            run.iter().all(|&i| {
                glyphs[i]
                    .text
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '(' | ')' | '%' | '-'))
            })
        };
        // The gap from run `a`'s ink to run `b`'s on its line, in sizes;
        // `None` across lines.
        let apart = |a: &[usize], b: &[usize]| {
            let (p, n) = (&glyphs[*a.last().unwrap()], &glyphs[b[0]]);
            let fs = p.font_size.max(n.font_size).max(1.0) as f64;
            ((n.origin.1 - p.origin.1).abs() < fs * 0.3)
                .then(|| (n.bbox.unwrap().x0 - p.bbox.unwrap().x1) / fs)
        };
        // A sign a word space after a figure is that figure's where its line
        // sets signs after figures (`4,85 €`), as a sign that prices no
        // figure after it says: the line's last, or one before a label.
        // Elsewhere it is the next figure's, as below.
        // The column a glyph's ink is centred in.
        let column = |i: usize| {
            let b = glyphs[i].bbox.unwrap();
            let x = (b.x0 + b.x1) * 0.5;
            self.xs.windows(2).position(|w| x >= w[0] && x < w[1])
        };
        // In its figure's column: a sign past a cut is the next column's.
        let after_figure = |m: usize| {
            m > 0
                && lone_sign(&runs[m])
                && numeric(&runs[m - 1])
                && apart(&runs[m - 1], &runs[m]).is_some_and(|g| g <= SIGN_SPACE)
                && column(runs[m][0]) == column(*runs[m - 1].last().unwrap())
        };
        let prices_next = |m: usize| {
            runs.get(m + 1).is_some_and(|n| {
                numeric(n) && apart(&runs[m], n).is_some_and(|g| g < CURRENCY_REACH)
            })
        };
        let line_of: Vec<usize> = {
            let mut line = 0;
            (0..runs.len())
                .map(|m| {
                    if m > 0 && apart(&runs[m - 1], &runs[m]).is_none() {
                        line += 1;
                    }
                    line
                })
                .collect()
        };
        let suffixed: rustc_hash::FxHashSet<usize> = (0..runs.len())
            .filter(|&m| after_figure(m) && !prices_next(m))
            .map(|m| line_of[m])
            .collect();
        let suffix: Vec<bool> = (0..runs.len())
            .map(|m| after_figure(m) && suffixed.contains(&line_of[m]))
            .collect();
        let mut kept: Vec<Vec<usize>> = Vec::with_capacity(runs.len());
        for (m, run) in std::mem::take(&mut runs).into_iter().enumerate() {
            match kept.last_mut() {
                Some(prev) if suffix[m] => prev.extend(run),
                _ => kept.push(run),
            }
        }
        runs = kept;
        // A lone currency symbol belongs to the number it prices: merge it
        // into the following numeric run so `$` and `7,835,559` land in the
        // same cell. Financial statements pin the symbol at the column's
        // left edge with the value right-aligned, so the gap between them
        // can be wide — several ems — while still being one cell.
        let mut m = 0;
        while m + 1 < runs.len() {
            let is_currency = lone_sign(&runs[m]);
            let next_numeric = numeric(&runs[m + 1]);
            let close = {
                let a = &glyphs[*runs[m].last().unwrap()];
                let b = &glyphs[runs[m + 1][0]];
                let fs = a.font_size.max(1.0) as f64;
                (b.origin.1 - a.origin.1).abs() < fs * 0.3
                    && b.bbox.unwrap().x0 - a.bbox.unwrap().x1 < fs * CURRENCY_REACH
            };
            if is_currency && next_numeric && close {
                let tail = runs.remove(m + 1);
                runs[m].extend(tail);
            } else {
                m += 1;
            }
        }
        for run in &runs {
            let (mut cx, mut cy, mut n) = (0.0, 0.0, 0.0);
            let (mut lo, mut hi) = (f64::MAX, f64::MIN);
            for &i in run.iter() {
                let b = glyphs[i].bbox.unwrap();
                cx += (b.x0 + b.x1) * 0.5;
                cy += (b.y0 + b.y1) * 0.5;
                lo = lo.min(b.x0);
                hi = hi.max(b.x1);
                n += 1.0;
            }
            let (cx, cy) = (cx / n, cy / n);
            // A run is indivisible only while it could be one cell's content.
            // Crossing *two* interior boundaries means it spans three
            // columns, which no single value does — it is separate values
            // the run-builder failed to split, because the gaps carried no
            // break evidence (a row-spanning label sharing a line with the
            // figures beside it). Assigning it whole would move all of them
            // into one cell chosen by a centre of mass lying in none of their
            // columns. One crossing is left alone: a right-aligned money
            // value with its symbol pinned at the column's left edge legally
            // straddles its own boundary.
            let crossings = self.xs[1..self.xs.len().saturating_sub(1)]
                .iter()
                .filter(|x| **x > lo && **x < hi)
                .count();
            if crossings >= 2 {
                // Dealt by its words, each to the column of its middle: a
                // word whose letters touch is not cut between columns.
                let mut part: Vec<usize> = Vec::new();
                let deal = |part: &[usize], home: &mut Vec<Option<usize>>| {
                    let (mut x, mut n) = (0.0, 0.0);
                    for &i in part {
                        let b = glyphs[i].bbox.unwrap();
                        x += (b.x0 + b.x1) * 0.5;
                        n += 1.0;
                    }
                    if let Some((r, c)) = self.cell_at(x / n, cy) {
                        for &i in part {
                            home[i] = Some(r * cols + c);
                        }
                    }
                };
                for &i in run.iter() {
                    if let Some(&prev) = part.last() {
                        let (p, g) = (&glyphs[prev], &glyphs[i]);
                        let pen = p.advance.map_or(p.bbox.unwrap().x1, |a| p.origin.0 + a);
                        let fs = p.font_size.max(g.font_size).max(1.0) as f64;
                        if g.bbox.unwrap().x0.min(g.origin.0) - pen > fs * WORD_TOUCH {
                            deal(&part, &mut home);
                            part.clear();
                        }
                    }
                    part.push(i);
                }
                deal(&part, &mut home);
                continue;
            }
            if let Some((r, c)) = self.cell_at(cx, cy) {
                for &i in run.iter() {
                    home[i] = Some(r * cols + c);
                }
            }
        }
        // A label set on its side in a cell of its own (a column's name
        // turned up a tall header row) is that cell's text: its line lies
        // whole inside one cell, and the cell holds no level text. A sidebar
        // or watermark drawn over a table runs through several cells, or
        // across text, and stays out of them.
        {
            let occupied: rustc_hash::FxHashSet<usize> = home.iter().flatten().copied().collect();
            let mut turned: Vec<usize> = (0..glyphs.len())
                .filter(|&i| {
                    let g = &glyphs[i];
                    g.bbox.is_some()
                        && !g.text.trim().is_empty()
                        && matches!(g.rotation_bucket().rem_euclid(360), 90 | 270)
                })
                .collect();
            turned.sort_by(|&a, &b| {
                let (ga, gb) = (&glyphs[a], &glyphs[b]);
                ga.origin
                    .0
                    .total_cmp(&gb.origin.0)
                    .then(ga.origin.1.total_cmp(&gb.origin.1))
            });
            // Lines up the page: glyphs on one vertical, no gap along it
            // wider than a size and a half.
            let mut lines: Vec<Vec<usize>> = Vec::new();
            for &i in &turned {
                let g = &glyphs[i];
                let fs = g.font_size.max(1.0) as f64;
                match lines.last_mut() {
                    Some(line)
                        if {
                            let p = &glyphs[*line.last().unwrap()];
                            (g.origin.0 - p.origin.0).abs() < fs * 0.3
                                && g.bbox.unwrap().y0 - p.bbox.unwrap().y1 < fs * 1.5
                        } =>
                    {
                        line.push(i)
                    }
                    _ => lines.push(vec![i]),
                }
            }
            // Labels turned in a table set level, not a table set on its
            // side: there the grid's rows are the table's columns, and its
            // text is read with the page's prose instead. Fewer cells of
            // level text than turned lines in the grid is a table on its
            // side.
            let in_grid = |line: &Vec<usize>| {
                line.iter().any(|&i| {
                    let b = glyphs[i].bbox.unwrap();
                    self.cell_at((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5)
                        .is_some()
                })
            };
            if occupied.len() < lines.iter().filter(|l| in_grid(l)).count() {
                lines.clear();
            }
            for line in lines {
                let at: Vec<Option<(usize, usize)>> = line
                    .iter()
                    .map(|&i| {
                        let b = glyphs[i].bbox.unwrap();
                        self.cell_at((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5)
                    })
                    .collect();
                // Every cell it touches free of level text. Down one column,
                // a group label set beside the rows it groups: the topmost
                // cell. In one row, a label straddling a boundary its row
                // never draws: the cell holding most of it.
                let Some(Some((row, col))) = at.first() else {
                    continue;
                };
                if at
                    .iter()
                    .any(|a| a.is_none_or(|(r, c)| occupied.contains(&(r * cols + c))))
                {
                    continue;
                }
                if at.iter().flatten().all(|(_, c)| c == col) {
                    let top = at.iter().flatten().map(|(r, _)| *r).min().unwrap_or(*row);
                    let cell = top * cols + col;
                    line.iter().for_each(|&i| home[i] = Some(cell));
                    continue;
                }
                if at.iter().flatten().any(|(r, _)| r != row) {
                    continue;
                }
                let mut count: std::collections::BTreeMap<usize, usize> = Default::default();
                for (_, c) in at.iter().flatten() {
                    *count.entry(*c).or_default() += 1;
                }
                if let Some((&c, _)) = count
                    .iter()
                    .max_by_key(|(c, n)| (**n, std::cmp::Reverse(**c)))
                {
                    let cell = row * cols + c;
                    line.iter().for_each(|&i| home[i] = Some(cell));
                }
            }
        }
        // The page's own spaces go back in. Words are assigned by their ink,
        // and a space glyph has none, so a cell's text was rebuilt from gaps
        // alone: a Helvetica space beside a digit is narrower than the gap
        // that reads as one (`paint,1 gal`, `SPF50200ml`), and a cell has too
        // few gaps to measure its own. A space joins the cell of the word it
        // ends, when ink follows it on its line. Between two cells it is
        // that cell's trailing space and trims away; where the two cells
        // are one merged cell read whole, it is the space between its words
        // (`Évolution depuis le T2 2026`, cut by the grid after `T2`, read
        // `T22026` without it).
        let mut drawn: Vec<usize> = (0..glyphs.len())
            .filter(|&i| !glyphs[i].text.is_empty() && glyphs[i].is_horizontal())
            .collect();
        drawn.sort_by(|&a, &b| {
            let (ga, gb) = (&glyphs[a], &glyphs[b]);
            ga.origin
                .1
                .partial_cmp(&gb.origin.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(
                    ga.origin
                        .0
                        .partial_cmp(&gb.origin.0)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });
        let is_space = |i: usize| glyphs[i].bbox.is_none() || glyphs[i].text.trim().is_empty();
        for k in 0..drawn.len() {
            let s = drawn[k];
            if !is_space(s) {
                continue;
            }
            let fs = glyphs[s].font_size.max(1.0) as f64;
            let same_line = |j: usize| (glyphs[j].origin.1 - glyphs[s].origin.1).abs() < fs * 0.3;
            let before = drawn[..k]
                .iter()
                .rev()
                .copied()
                .take_while(|&j| same_line(j))
                .find(|&j| !is_space(j));
            let after = drawn[k + 1..]
                .iter()
                .copied()
                .take_while(|&j| same_line(j))
                .find(|&j| !is_space(j));
            if let (Some(p), Some(n)) = (before, after) {
                if let (Some(a), Some(_)) = (home[p], home[n]) {
                    home[s] = Some(a);
                }
            }
        }
        home
    }

    /// Derive columns for a one-column grid from the alignment of its text.
    ///
    /// A key/value list ruled with horizontals only — a billing summary's
    /// `MS-DRG … 207`, `DRG Weight … 9.1742` — draws no vertical, so its grid
    /// is one column and every row reads label and value as one cell. The
    /// column is there all the same: a gap of empty space that runs down
    /// the rows with text on both sides of it. Each such gap becomes a
    /// boundary at its midpoint.
    ///
    /// Returns the rows whose text crosses a new boundary — a boxed notice
    /// or a banner welded into the grid — so the caller can mark them as
    /// spanning rather than let them read as two cells. Empty when nothing
    /// was split.
    pub fn split_by_alignment(&mut self, glyphs: &[crate::glyph::Glyph]) -> Vec<usize> {
        if self.cols() != 1 || self.rows() < ALIGNED_SPLIT_MIN_ROWS {
            return Vec::new();
        }
        let rows = self.rows();
        let mut ink: Vec<Vec<(f64, f64)>> = vec![Vec::new(); rows];
        let mut sizes: Vec<f32> = Vec::new();
        for g in glyphs {
            let Some(b) = g.bbox else { continue };
            if g.text.trim().is_empty() || !g.is_horizontal() {
                continue;
            }
            if let Some((r, _)) = self.cell_at((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5) {
                ink[r].push((b.x0, b.x1));
                sizes.push(g.font_size);
            }
        }
        let text_rows: Vec<usize> = (0..rows).filter(|r| !ink[*r].is_empty()).collect();
        if text_rows.len() < ALIGNED_SPLIT_MIN_ROWS {
            return Vec::new();
        }
        sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let font = sizes[sizes.len() / 2].max(1.0) as f64;
        let min_gap = (font * ALIGNED_SPLIT_GAP_EMS).max(ALIGNED_SPLIT_GAP_MIN);
        let (x0, x1) = (self.xs[0], self.xs[1]);

        // Walk the width in one-point steps; a step is free when almost no
        // row inks it.
        let covered_at = |x: f64| {
            text_rows
                .iter()
                .filter(|r| ink[**r].iter().any(|(a, b)| *a <= x && x <= *b))
                .count()
        };
        let allowed = text_rows.len() * ALIGNED_SPLIT_CROSSING_PCT / 100;
        let mut gaps: Vec<(f64, f64)> = Vec::new();
        let mut start: Option<f64> = None;
        let mut x = x0;
        while x <= x1 {
            let free = covered_at(x) <= allowed;
            match (free, start) {
                (true, None) => start = Some(x),
                (false, Some(s)) => {
                    gaps.push((s, x - 1.0));
                    start = None;
                }
                _ => {}
            }
            x += 1.0;
        }
        if let Some(s) = start {
            gaps.push((s, x1));
        }
        let mut cuts: Vec<f64> = Vec::new();
        for (s, e) in gaps {
            if e - s < min_gap {
                continue;
            }
            // Text on both sides in most rows, or the gap is a margin.
            let both = text_rows
                .iter()
                .filter(|r| {
                    ink[**r].iter().any(|(_, b)| *b < s) && ink[**r].iter().any(|(a, _)| *a > e)
                })
                .count();
            if both * 100 < text_rows.len() * ALIGNED_SPLIT_BOTH_SIDES_PCT {
                continue;
            }
            cuts.push((s + e) / 2.0);
        }
        if cuts.is_empty() {
            return Vec::new();
        }
        // A row spans a cut when its text runs across it: ink on both sides
        // with no more than a word space between — a glyph straddling the
        // cut, or a word gap that happens to fall on it.
        let spanning: Vec<usize> = (0..rows)
            .filter(|r| {
                cuts.iter().any(|c| {
                    let left = ink[*r]
                        .iter()
                        .filter(|(a, _)| *a < *c)
                        .map(|(_, b)| *b)
                        .fold(f64::MIN, f64::max);
                    let right = ink[*r]
                        .iter()
                        .filter(|(_, b)| *b > *c)
                        .map(|(a, _)| *a)
                        .fold(f64::MAX, f64::min);
                    left > f64::MIN && right < f64::MAX && right - left < min_gap
                })
            })
            .collect();
        let mut xs = vec![x0];
        xs.extend(cuts);
        xs.push(x1);
        self.xs = xs;
        spanning
    }

    /// The text of one row read across the whole grid, ignoring columns —
    /// for a row that spans them.
    pub fn row_text(&self, glyphs: &[crate::glyph::Glyph], row: usize) -> String {
        crate::line::assemble(&self.row_glyphs(glyphs, row))
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The inked glyphs row `row`'s cells read ([`Self::glyph_cells`]), and
    /// so take from the page ([`Self::takes`]).
    ///
    /// Not each glyph by its own centre. A cell reads a word whole, in the
    /// cell holding its centre of mass. Picked glyph by glyph, a word an
    /// edge of the grid runs through lost letters or was read twice. With
    /// its centre of mass inside, its letters outside the edge were neither
    /// the row's nor the prose's: a head line set out past a table's edge,
    /// read out as the table's title, read `ECIMAL` for `DECIMAL`. With it
    /// outside, the prose kept the word whole and the row read its letters
    /// inside the edge again: `World` in the prose, `ld` in the title.
    ///
    /// Ink only, the words spaced by their gaps as before: a space the text
    /// draws between two cells' labels (`Quota Measures ␠ 0-50%`) joins
    /// them into one line, run across the cut between them.
    pub(crate) fn row_glyphs(
        &self,
        glyphs: &[crate::glyph::Glyph],
        row: usize,
    ) -> Vec<crate::glyph::Glyph> {
        let cols = self.cols().max(1);
        glyphs
            .iter()
            .zip(self.glyph_cells(glyphs))
            .filter(|(g, home)| {
                home.is_some_and(|i| i / cols == row)
                    && g.bbox.is_some()
                    && !g.text.trim().is_empty()
            })
            .map(|(g, _)| g.clone())
            .collect()
    }

    /// The text of a block of cells read as one label, rows `rows` by
    /// columns `cols`, when its text runs across the cuts between them.
    ///
    /// A merged cell's text belongs to the whole block, not to the cells the
    /// grid cuts it into. A header spanning two rows is centred across their
    /// boundary, so its one line puts `Staff` above the cut and `member` below
    /// it, and two lines can split the other way round; a banner across two
    /// columns puts `Weight` in one and `(kg)` in the next.
    ///
    /// A cut with text on both sides must be crossed by it: a line set
    /// across it, a word space apart either side of a column cut, or the
    /// lines either side of a row cut a paragraph's leading apart. `None`
    /// otherwise, because then the cut falls cleanly between values. A
    /// merge read from where the ruling is absent also takes in columns
    /// that were simply never ruled apart (`Foreign | Domestic`), and their
    /// values are separate.
    pub fn region_text(
        &self,
        glyphs: &[crate::glyph::Glyph],
        merges: &Merges,
        rows: std::ops::Range<usize>,
        cols: std::ops::Range<usize>,
    ) -> Option<String> {
        let n = self.cols();
        let homes = self.glyph_cells(glyphs);
        let inside: Vec<crate::glyph::Glyph> = glyphs
            .iter()
            .zip(&homes)
            .filter(|(_, home)| {
                home.is_some_and(|i| rows.contains(&(i / n)) && cols.contains(&(i % n)))
            })
            .map(|(g, _)| g.clone())
            .collect();
        let lines = crate::line::assemble(&inside);
        // A line's words, each as the span of its ink. Words, not glyphs: a
        // cell is dealt whole words, and a boundary inferred from alignment
        // can run through one (`M|eiosis`) without parting anything.
        let words = |l: &crate::line::Line| -> Vec<BBox> {
            let fs = l.font_size.max(1.0) as f64;
            let mut words: Vec<BBox> = Vec::new();
            let mut open = false;
            for g in l.glyphs.iter().map(|&i| &inside[i]) {
                let Some(b) = g.bbox.filter(|_| !g.text.trim().is_empty()) else {
                    open = false;
                    continue;
                };
                match words.last_mut() {
                    Some(w) if open && b.x0 - w.x1 <= fs * 0.2 => {
                        w.x1 = w.x1.max(b.x1);
                        w.y0 = w.y0.min(b.y0);
                        w.y1 = w.y1.max(b.y1);
                    }
                    _ => words.push(b),
                }
                open = true;
            }
            words
        };
        let ink: Vec<BBox> = lines.iter().flat_map(words).collect();
        let column_cut = |x: f64| {
            let parted = ink.iter().any(|b| (b.x0 + b.x1) * 0.5 < x)
                && ink.iter().any(|b| (b.x0 + b.x1) * 0.5 >= x);
            !parted
                || lines.iter().any(|l| {
                    let boxes = words(l);
                    let left = boxes
                        .iter()
                        .filter(|b| (b.x0 + b.x1) * 0.5 < x)
                        .map(|b| b.x1)
                        .fold(f64::MIN, f64::max);
                    let right = boxes
                        .iter()
                        .filter(|b| (b.x0 + b.x1) * 0.5 >= x)
                        .map(|b| b.x0)
                        .fold(f64::MAX, f64::min);
                    right - left < l.font_size.max(1.0) as f64
                })
        };
        let baseline = |l: &crate::line::Line| inside[l.glyphs[0]].origin.1;
        // Per column beside the block that rules row `row` off from the one
        // above it: whether ink in `row` sits on this baseline.
        let beside = |row: usize, cut: usize, base: f64, fs: f64| -> Vec<bool> {
            (0..n)
                .filter(|j| {
                    !cols.contains(j)
                        && !merges
                            .continues_above
                            .get(cut * n + j)
                            .copied()
                            .unwrap_or(false)
                })
                .map(|j| {
                    glyphs.iter().zip(&homes).any(|(g, home)| {
                        *home == Some(row * n + j)
                            && g.bbox.is_some()
                            && !g.text.trim().is_empty()
                            && (g.origin.1 - base).abs() <= fs * SHARED_BASELINE
                    })
                })
                .collect()
        };
        let row_cut = |k: usize| {
            let y = self.ys[k];
            let mid = |l: &&crate::line::Line| (l.bbox.y0 + l.bbox.y1) * 0.5;
            let above = lines
                .iter()
                .filter(|l| mid(l) < y)
                .max_by(|a, b| baseline(a).total_cmp(&baseline(b)));
            let below = lines
                .iter()
                .filter(|l| mid(l) >= y)
                .min_by(|a, b| baseline(a).total_cmp(&baseline(b)));
            // All of the text on one side: nothing for the cut to part.
            let (Some(a), Some(b)) = (above, below) else {
                return true;
            };
            let fs = a.font_size.max(b.font_size).max(1.0) as f64;
            let leading = baseline(b) - baseline(a);
            // Two lines each on the baseline of a value beside them, in rows
            // the ruling parts, are those rows' labels however tightly the
            // rows are set: `Revision Level | L` over `Revision Date |
            // 06/14/2026`. A merged label's lines are centred across its
            // rows and sit on neither. Another merged label beside it is no
            // evidence: wrapped the same way, its lines share the baselines.
            let rowed = beside(k - 1, k, baseline(a), fs)
                .into_iter()
                .zip(beside(k, k, baseline(b), fs))
                .any(|(up, down)| up && down);
            // Unless the lower line plainly carries on the upper one. A label
            // top-aligned over single-line rows puts each of its lines on a
            // row's baseline too: `KEYTRUDA in combination` over `with
            // axitinib`, `Infusion-related reactions` over `[see Warnings…]`.
            // Its lines are then as far apart as the rows are, so the
            // paragraph's leading is no bound on them; a blank line between
            // them is.
            let carries_on = b
                .text
                .starts_with(|c: char| c.is_lowercase() || c == '(' || c == '[')
                || a.text.ends_with([',', '-', '/']);
            lines.iter().any(|l| l.bbox.y0 < y && l.bbox.y1 > y)
                || (leading <= PARAGRAPH_LEADING * fs && !rowed)
                || (carries_on && leading <= CARRIED_LEADING * fs)
        };
        let crossed = (cols.start + 1..cols.end).all(|k| column_cut(self.xs[k]))
            && (rows.start + 1..rows.end).all(row_cut);
        crossed.then(|| join_cell_lines(lines.iter().map(|l| l.text.as_str())))
    }

    pub fn cols(&self) -> usize {
        self.xs.len().saturating_sub(1)
    }
    pub fn rows(&self) -> usize {
        self.ys.len().saturating_sub(1)
    }

    /// Which cells are continuations of the cell above, and which rows are a
    /// single full-width cell.
    ///
    /// Both are read from the ruling geometry rather than guessed from
    /// content: a cell is separated from the one above it only where a
    /// horizontal rule actually runs across that column, and a row is one
    /// merged cell when no vertical rule crosses its interior. Grids drawn
    /// with a full border but no interior rules would report every cell as
    /// merged, so a grid whose interior is unruled reports nothing.
    pub fn merges(&self, rules: &[Rule], fills: &[crate::rule::Fill]) -> Merges {
        let (rows, cols) = (self.rows(), self.cols());
        let mut m = Merges {
            continues_above: vec![false; rows * cols],
            continues_left: vec![false; rows * cols],
            full_width_row: vec![false; rows],
        };
        if rows == 0 || cols == 0 {
            return m;
        }
        let hs: Vec<&Rule> = rules
            .iter()
            .filter(|r| r.orientation == Orientation::Horizontal)
            .collect();
        let vs: Vec<&Rule> = rules
            .iter()
            .filter(|r| r.orientation == Orientation::Vertical)
            .collect();

        // A merge is only claimed where the ruling positively shows one: the
        // boundary must carry a horizontal rule that separates *some* column
        // while leaving others open. A boundary with no rule at all is not
        // evidence of merging — it is a grid whose row boundaries came from
        // elsewhere (segmented verticals, alignment), and reading absence as
        // a span blanks most of the table (measured: TEDS −0.13).
        for r in 1..rows {
            let y = self.ys[r];
            let mut at_y: Vec<(f64, f64)> = hs
                .iter()
                .filter(|rule| (rule.bbox.y0 - y).abs() <= EDGE_TOLERANCE * 2.0)
                .map(|rule| (rule.bbox.x0, rule.bbox.x1))
                .collect();
            if at_y.is_empty() {
                continue;
            }
            // Where a rule runs under some columns, a fill ending on its line
            // parts the others: a shaded row whose shading is its bottom
            // edge over the columns the rule leaves open. Never alone: a
            // boundary drawn by no rule claims no span. A cell's shading,
            // its sides on column edges; not a highlight set inside a cell
            // behind a line of its text.
            let on_edge = |x: f64| self.xs.iter().any(|e| (e - x).abs() <= EDGE_TOLERANCE);
            let striped = striped(&shaded_bands(fills, &self.xs, &self.ys));
            at_y.extend(
                fills
                    .iter()
                    .filter(|f| {
                        !striped
                            && ((f.bbox.y1 - y).abs() <= EDGE_TOLERANCE * 2.0
                                || (f.bbox.y0 - y).abs() <= EDGE_TOLERANCE * 2.0)
                            && on_edge(f.bbox.x0)
                            && on_edge(f.bbox.x1)
                            && shading_ends(fills, f, y)
                    })
                    .map(|f| (f.bbox.x0, f.bbox.x1)),
            );
            let open: Vec<bool> = (0..cols)
                .map(|c| {
                    let (x0, x1) = (self.xs[c], self.xs[c + 1]);
                    span_coverage(at_y.iter().copied(), x0, x1) < 0.6 * (x1 - x0)
                })
                .collect();
            // All open means the rule sits somewhere else entirely; none open
            // means an ordinary full-width row separator.
            if open.iter().all(|x| *x) || !open.iter().any(|x| *x) {
                continue;
            }
            for (c, o) in open.iter().enumerate() {
                m.continues_above[r * cols + c] = *o;
            }
        }

        // Horizontal merges: a column boundary only separates the rows it is
        // actually drawn across. A nested table rules its own interior
        // columns, and those boundaries must not slice the outer table's
        // prose rows, which span the whole content column.
        //
        // Only claimed in a grid whose columns are *drawn*. Where boundaries
        // came from elsewhere (segmented horizontal endpoints, alignment) no
        // row is crossed anywhere, and reading that as "everything is merged"
        // collapses the table into one column (measured: one bench table
        // 10x4 -> 10x1, TEDS 0.82 -> 0.23).
        if cols > 1 && !vs.is_empty() {
            let open: Vec<bool> = (0..rows * cols)
                .map(|i| {
                    let (r, c) = (i / cols, i % cols);
                    if c == 0 {
                        return false;
                    }
                    let mid = (self.ys[r] + self.ys[r + 1]) / 2.0;
                    let x = self.xs[c];
                    !vs.iter().any(|rule| {
                        (rule.bbox.x0 - x).abs() <= EDGE_TOLERANCE * 2.0
                            && rule.bbox.y0 <= mid
                            && rule.bbox.y1 >= mid
                    })
                })
                .collect();
            let any_drawn = open.iter().enumerate().any(|(i, o)| i % cols != 0 && !o);
            if any_drawn {
                m.continues_left = open;
                let open_row = |r: usize| (1..cols).all(|c| m.continues_left[r * cols + c]);
                let ruled_row = |r: usize| (1..cols).any(|c| !m.continues_left[r * cols + c]);
                let edges_drawn = |r: usize| {
                    let mid = (self.ys[r] + self.ys[r + 1]) / 2.0;
                    let at = |x: f64| {
                        vs.iter().any(|rule| {
                            (rule.bbox.x0 - x).abs() <= EDGE_TOLERANCE * 2.0
                                && rule.bbox.y0 <= mid
                                && rule.bbox.y1 >= mid
                        })
                    };
                    at(self.xs[0]) && at(self.xs[cols])
                };
                for r in 0..rows {
                    // A banner is the whole-row case, and must also be drawn
                    // as one: un-ruled rows equally occur where two separate
                    // tables were welded into a single grid component, and
                    // those rows are not banners. Shading makes a band read
                    // as a label; so does a band boxed into the table, its
                    // two outer edges drawn, between rows that draw the
                    // table's columns above and below it. A welded table's
                    // rows are not set between the other table's rows.
                    let boxed = r > 0
                        && r + 1 < rows
                        && edges_drawn(r)
                        && (0..r).any(ruled_row)
                        && ruled_row(r + 1);
                    m.full_width_row[r] = open_row(r) && (self.band_has_fill(fills, r) || boxed);
                }
            }
        }
        m
    }

    /// Spans the ruling leaves unsaid: rows grouped between rules, where one
    /// column gives each row its own value and another sets one value across
    /// the group.
    ///
    /// A word processor draws a table's rows whether it rules them or not.
    /// Three samples of one product are three rows with no rule between
    /// them, and the product's name and formulation are each one cell
    /// spanning all three, centred on the group and wrapped where the column
    /// is narrow. [`Grid::merges`] reads nothing from an unruled boundary,
    /// and rightly: absence alone is no span. The text is the evidence here.
    /// A column whose rows each hold text placed the same way (each centred
    /// in its row, or each at its top) gives every row a value. Another
    /// column whose text runs across the rows' cuts as one wrapped value
    /// ([`Grid::region_text`]), or sits as one line centred on the whole
    /// group, is one cell spanning it.
    ///
    /// Claimed only for groups between rules drawn across the whole table,
    /// below the first row, and only where there are two such groups or
    /// more. A table ruled under its header and at its foot and nowhere else
    /// is one group, and a sparse column in it is values missing from some
    /// rows, not a span. And only where the rows inside a group are drawn:
    /// the border stroked row by row, breaking at each. A cut the grid took
    /// from the gap between a label's wrapped lines has the border run
    /// straight through it, and reading it as rows would copy the values
    /// beside the label onto a row of its second line.
    pub fn text_merges(&self, glyphs: &[crate::glyph::Glyph], rules: &[Rule], merges: &mut Merges) {
        let (rows, cols) = (self.rows(), self.cols());
        if rows < 3 || cols < 2 || merges.continues_above.len() != rows * cols {
            return;
        }
        let (left, right) = (self.xs[0], self.xs[cols]);
        let on = |k: usize| {
            rules
                .iter()
                .filter(|r| {
                    r.orientation == Orientation::Horizontal
                        && (r.bbox.y0 - self.ys[k]).abs() <= EDGE_TOLERANCE * 2.0
                        && r.bbox.x1 > left
                        && r.bbox.x0 < right
                })
                .collect::<Vec<_>>()
        };
        let at = |k: usize| {
            on(k)
                .iter()
                .map(|r| (r.bbox.x0, r.bbox.x1))
                .collect::<Vec<_>>()
        };
        // Underlines under a row's figures, a piece under each column parted
        // at its gutters ([`cluster`]), close a subtotal's figures and leave
        // the labels' column open. A label on each side of them is two
        // rows' labels, not one span (`Japanese yen` over `Total`); a stub
        // head over a header's underlined groups, its text on one side
        // alone, still spans. Not a cell boxed across the boundary, a
        // vertical on each of its column's edges running through it: no
        // rule closes under it, and it spans its rows whatever the rules
        // beside it are drawn in (a datasheet's cells, each bordered a cell
        // at a time, `Min | Typ | Max` ruled apart under a condition
        // wrapped over two rows of them).
        // A vertical on the edge, above the boundary and below it: one
        // rule through it, or a row's border meeting the next row's there.
        let through = |x: f64, y: f64| {
            let on_edge = |r: &&Rule| {
                r.orientation == Orientation::Vertical
                    && (r.axis_pos() - x).abs() <= EDGE_TOLERANCE * 2.0
            };
            let tol = EDGE_TOLERANCE * 2.0;
            rules
                .iter()
                .filter(on_edge)
                .any(|r| r.bbox.y0 < y - tol && r.bbox.y1 >= y - tol)
                && rules
                    .iter()
                    .filter(on_edge)
                    .any(|r| r.bbox.y1 > y + tol && r.bbox.y0 <= y + tol)
        };
        let boxed = |k: usize, c: usize| {
            through(self.xs[c], self.ys[k]) && through(self.xs[c + 1], self.ys[k])
        };
        let underlined: Vec<usize> = (1..rows)
            .filter(|&k| merges.continues_above[k * cols..(k + 1) * cols].contains(&true))
            .filter(|&k| cluster(&on(k)).iter().any(|l| !l.gutters.is_empty()))
            .collect();
        if !underlined.is_empty() {
            let homes = self.glyph_cells(glyphs);
            let held = |cell: usize| {
                glyphs
                    .iter()
                    .zip(&homes)
                    .any(|(g, h)| *h == Some(cell) && !g.text.trim().is_empty())
            };
            for k in underlined {
                for i in k * cols..(k + 1) * cols {
                    if merges.continues_above[i] && held(i) && held(i - cols) && !boxed(k, i % cols)
                    {
                        merges.continues_above[i] = false;
                    }
                }
            }
        }
        let across =
            |k: usize| span_coverage(at(k).into_iter(), left, right) >= 0.9 * (right - left);
        // The rows are drawn at an unruled boundary when the table's
        // verticals are stroked row by row and break there: some vertical on
        // a column edge ends at it, and none runs on across it. A boundary
        // the grid took from elsewhere (another column's rows, the gap
        // between a label's wrapped lines) has the border run straight
        // through it, and the rows it parts are one row.
        let drawn = |k: usize| {
            let y = self.ys[k];
            let edge = |r: &&Rule| {
                r.orientation == Orientation::Vertical
                    && self
                        .xs
                        .iter()
                        .any(|x| (r.bbox.x0 - x).abs() <= EDGE_TOLERANCE * 2.0)
            };
            let verticals: Vec<&Rule> = rules.iter().filter(edge).collect();
            verticals.iter().any(|r| {
                (r.bbox.y0 - y).abs() <= EDGE_TOLERANCE || (r.bbox.y1 - y).abs() <= EDGE_TOLERANCE
            }) && !verticals
                .iter()
                .any(|r| r.bbox.y0 < y - EDGE_TOLERANCE && r.bbox.y1 > y + EDGE_TOLERANCE)
        };
        // Groups: rows between boundaries that carry a rule, each closed by
        // one drawn across the whole table, with every row inside drawn.
        let mut cuts: Vec<usize> = vec![0];
        cuts.extend((1..rows).filter(|&k| !at(k).is_empty()));
        cuts.push(rows);
        let groups: Vec<(usize, usize)> = cuts
            .windows(2)
            .map(|w| (w[0], w[1]))
            .filter(|&(a, b)| a > 0 && across(a) && across(b) && (a + 1..b).all(drawn))
            .collect();
        if groups.len() < 2 {
            return;
        }

        let homes = self.glyph_cells(glyphs);
        let font = {
            let mut sizes: Vec<f32> = glyphs
                .iter()
                .zip(&homes)
                .filter(|(_, h)| h.is_some())
                .map(|(g, _)| g.font_size)
                .collect();
            if sizes.is_empty() {
                return;
            }
            sizes.sort_by(f32::total_cmp);
            sizes[sizes.len() / 2].max(1.0) as f64
        };
        let tolerance = font * PLACEMENT_TOLERANCE;
        // The vertical extent of the ink in cell (r, c).
        let ink = |r: usize, c: usize| -> Option<(f64, f64)> {
            glyphs
                .iter()
                .zip(&homes)
                .filter(|(_, h)| **h == Some(r * cols + c))
                .filter_map(|(g, _)| g.bbox)
                .fold(None, |acc, b| match acc {
                    None => Some((b.y0, b.y1)),
                    Some((y0, y1)) => Some((y0.min(b.y0), y1.max(b.y1))),
                })
        };
        let spread = |v: &[f64]| {
            v.iter().copied().fold(f64::MIN, f64::max) - v.iter().copied().fold(f64::MAX, f64::min)
        };

        for (a, b) in groups.into_iter().filter(|&(a, b)| b - a >= 2) {
            let inks: Vec<Vec<Option<(f64, f64)>>> = (0..cols)
                .map(|c| (a..b).map(|r| ink(r, c)).collect())
                .collect();
            // A value in every row, each placed in its row as the others are.
            let rowed = |c: usize| -> bool {
                let Some(blocks) = inks[c].iter().copied().collect::<Option<Vec<_>>>() else {
                    return false;
                };
                let rows_of = |f: &dyn Fn(usize, (f64, f64)) -> f64| -> Vec<f64> {
                    blocks
                        .iter()
                        .enumerate()
                        .map(|(i, &blk)| f(a + i, blk))
                        .collect()
                };
                let tops = rows_of(&|r, (y0, _)| y0 - self.ys[r]);
                let middles = rows_of(&|r, (y0, y1)| (y0 + y1 - self.ys[r] - self.ys[r + 1]) / 2.0);
                let bottoms = rows_of(&|r, (_, y1)| self.ys[r + 1] - y1);
                [tops, middles, bottoms]
                    .iter()
                    .any(|v| spread(v) <= tolerance)
            };
            if !(0..cols).any(rowed) {
                continue;
            }
            let middle = (self.ys[a] + self.ys[b]) / 2.0;
            let spanning: Vec<usize> = (0..cols)
                .filter(|&c| !rowed(c))
                .filter(|&c| {
                    let held: Vec<(f64, f64)> = inks[c].iter().flatten().copied().collect();
                    if held.is_empty() {
                        return false;
                    }
                    // Text on both sides of a cut, which `region_text` then
                    // has to find crossed; otherwise one line, centred.
                    let first = inks[c].iter().position(Option::is_some);
                    let last = inks[c].iter().rposition(Option::is_some);
                    let parted = first != last;
                    let (y0, y1) = held.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &(t, u)| {
                        (lo.min(t), hi.max(u))
                    });
                    let centred = ((y0 + y1) / 2.0 - middle).abs() <= tolerance;
                    (parted || centred)
                        && self.region_text(glyphs, merges, a..b, c..c + 1).is_some()
                })
                .collect();
            for c in spanning {
                for r in a + 1..b {
                    merges.continues_above[r * cols + c] = true;
                }
            }
        }
    }

    /// Whether the grid's boxes are numbered as a form's are: numbered
    /// fields ([`numbered_field`]), or a field's lettered parts (`a. NAME`,
    /// `b. STREET ADDRESS`), in half its rows under the first or more, two
    /// or more, across two columns or more, one of them numbered; and the
    /// first row holding a numbered field of its own.
    fn numbered_boxes(&self, rows: &[Vec<String>]) -> bool {
        let lettered = |t: &str| {
            let mut chars = t.chars();
            chars.next().is_some_and(|c| c.is_ascii_lowercase())
                && t[1..].starts_with(". ")
                && t[3..].chars().next().is_some_and(char::is_uppercase)
        };
        let boxed = |t: &str| numbered_field(t) || lettered(t);
        let below = rows.get(1..).unwrap_or(&[]);
        let boxed_rows = below
            .iter()
            .filter(|r| r.iter().any(|c| boxed(c.trim())))
            .count();
        let columns: std::collections::BTreeSet<usize> = below
            .iter()
            .flat_map(|r| {
                r.iter()
                    .enumerate()
                    .filter(|(_, c)| boxed(c.trim()))
                    .map(|(i, _)| i)
            })
            .collect();
        let numbered = below.iter().flatten().any(|c| numbered_field(c.trim()));
        // The first row a box of the form too, holding a numbered field in
        // its text (`… 1. REQUISITION NUMBER`, `18. SHIPPING POINT`): a
        // row of plain names over numbered entries (`Institutioneel |
        // Statistische Processen` over `1. Professionele …`, `7. …`) heads
        // the columns it lists them in.
        let first_boxed = rows.first().is_some_and(|r| {
            r.iter().any(|c| {
                let c = c.trim();
                c.char_indices().any(|(i, ch)| {
                    ch.is_ascii_digit()
                        && (i == 0 || c[..i].ends_with(char::is_whitespace))
                        && numbered_field(&c[i..])
                })
            })
        });
        numbered
            && first_boxed
            && boxed_rows >= 2
            && boxed_rows * 2 >= below.len()
            && columns.len() >= 2
    }

    /// How many leading rows are column headers.
    ///
    /// Row 0 is presumed to be the header — that is overwhelmingly the layout
    /// — and *demoted* when it is typed like the data below it: numeric in
    /// the columns whose data is numeric (a financial statement's first data
    /// row, or the continuation of a table split across pages). Style
    /// evidence vetoes the demotion, because a bold or banded first row is a
    /// header even when it is all numbers. Bare 4-digit years never count as
    /// numeric here: a `2023 | 2024` row over money columns is a header, and
    /// the year-vs-amount distinction is exactly what the demotion test must
    /// not blur.
    ///
    /// A list of fields has no header at all: a label column, bold on every
    /// row, or a ruled `Label | value` list whose first row is set like the
    /// rest (`lists_fields`).
    ///
    /// Stacked headers (spanning banner bands over the column-name row — the
    /// eligibility-matrix layout) are read from the fills: the header block
    /// is shaded as a run of banded rows and the data region is not, so a
    /// leading run of filled bands *is* the header depth. A run covering the
    /// whole grid says the shading carries no contrast and falls back to the
    /// single-row logic.
    pub fn header_rows(
        &self,
        rows: &[Vec<String>],
        glyphs: &[crate::glyph::Glyph],
        rules: &[Rule],
        fills: &[crate::rule::Fill],
    ) -> usize {
        if rows.len() < 2 {
            return 0;
        }
        let row0 = &rows[0];
        if row0.iter().all(|c| c.trim().is_empty()) {
            return 0;
        }
        // A first row of numbered fields (`1. DATE OF ORDER`, `2. CONTRACT
        // NO. …`, `6. SHIP TO:`) is a form's, its boxes numbered on down the
        // rows under it (`3. ORDER NO.`): each names its own box, and no
        // column under it. Numbered heads over columns of text (`1. Ingest |
        // 2. Classify` over each step's notes) number nothing under them.
        let filled: Vec<&str> = row0
            .iter()
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .collect();
        if filled.len() >= 2
            && filled.iter().all(|c| numbered_field(c))
            && rows[1..].iter().flatten().any(|c| numbered_field(c.trim()))
        {
            return 0;
        }
        // So is a grid whose boxes are numbered down most of its rows and
        // across its columns, whatever its first row holds: a form's title
        // set in the first box (`SOLICITATION/CONTRACT/ORDER … 1.
        // REQUISITION NUMBER`) beside its page box, over `2. CONTRACT NO. |
        // 3. AWARD DATE | 4. ORDER NUMBER`. A list numbered down one column
        // (`1. Introduction | 3`) numbers its entries, not boxes.
        if self.numbered_boxes(rows) {
            return 0;
        }
        let mut run = self.leading_fill_run(fills);
        // The run's last row is the body's first stripe where the rows from
        // it on are shaded every other row and it holds figures as they do:
        // an exposure table's `ethanol | 1000 | - | 1000` shaded under its
        // header's own cells, the next row bare, the one after shaded. Or
        // where it is shaded as the stripes under it are, and the header
        // over it in another paint: a schedule's dark header over its
        // lines striped pale, a title and a code in each.
        if run >= 2
            && self.stripes_from(fills, run - 1)
            && (figures_row(&rows[run - 1]) || {
                let stripe = self.row_paints(fills, run - 1);
                let head = self.row_paints(fills, run - 2);
                !stripe.is_empty()
                    && stripe == self.row_paints(fills, run + 1)
                    && !stripe.iter().any(|p| head.contains(p))
            })
        {
            run -= 1;
        }
        // The run's last row is the table's first band where it holds a
        // label in its first cell alone, as a row further down does under a
        // fill of its own: a funding table's `Months 1 – 12`, shaded as its
        // header is, over the CLINs it groups, and `Months 13-24` over the
        // next. A band labels the rows under it; it names no column.
        let band = |r: usize| {
            rows[r].first().is_some_and(|t| !t.trim().is_empty())
                && rows[r].iter().skip(1).all(|t| t.trim().is_empty())
        };
        // Beside other columns only, and in a run that leaves rows bare: a
        // one-column listing shaded throughout holds nothing but such rows.
        if run >= 2
            && run < rows.len()
            && self.cols() >= 2
            && band(run - 1)
            && (run..rows.len()).any(|r| band(r) && self.band_has_fill(fills, r))
        {
            run -= 1;
        }
        if run >= 2 && run < rows.len() && run <= MAX_HEADER_ROWS {
            return run;
        }
        // Label columns — the leading columns bold on every row, the rest
        // bold on none, the first row's included — make a list of fields,
        // whose first row is a field like the others: a field's name, or a
        // name and the size each value is for (`PRODUCT CODE | 8′2" | …`). A
        // header row names every column in its weight, the values' too.
        let bold = |(weighted, heavy): (usize, usize)| weighted > 0 && heavy * 2 > weighted;
        let weights: Vec<Vec<(usize, usize)>> = (0..self.rows())
            .map(|r| self.cell_weights(glyphs, r))
            .collect();
        let labels = (0..self.cols())
            .take_while(|&c| weights.iter().all(|w| bold(w[c])))
            .count();
        let label_column = self.cols() >= 2
            && run == 0
            && labels >= 1
            && labels < self.cols()
            && weights
                .iter()
                .all(|w| w[labels..].iter().all(|c| !bold(*c)));
        // So does one whose labels a colon ends, in any weight.
        let colon_labels =
            self.cols() == 2 && rows.iter().all(|r| r.len() == 2) && colon_fields(&rows.concat());
        // So does one whose first row is set as the fields under it are.
        let set_as_field = run == 0 && self.opens_as_field(rows, &weights, rules, fills);
        if label_column
            || colon_labels
            || set_as_field
            || self.lists_fields(rows, glyphs, rules, fills)
        {
            return 0;
        }
        // run == 1 is itself the exclusive-stripe evidence: band 0 filled,
        // band 1 not (where the run stopped).
        let styled = self.cells_are_bold(glyphs, 0) && !self.row_is_bold(glyphs, 1) || run == 1;
        decide_header_rows(rows, styled)
    }

    /// The first row is set as a field, and so is a row under it: a bold
    /// label, and beside it one value, plain, in the next cell or across
    /// every cell after the label.
    ///
    /// A header row is set alike across its cells, bold or plain. A bold
    /// label beside a plain value is how a field is set, and where a row
    /// under it is set that way too, the first row is one more field: a
    /// parameter table's `Instrument | <model>` over `Detector | <model>`,
    /// its leading labels bold and the rest plain. Its value can span the
    /// columns a table nested further down rules for itself (a gradient's
    /// `Time | A% | B%` in its value column), as the next field's does.
    ///
    /// Not over figures: where every row set that way holds figures
    /// beside its label and the first row holds words, the first row
    /// names them (`Item | Amount` over `Bolt | 0.40` and a bold `Total |
    /// 0.50`).
    fn opens_as_field(
        &self,
        rows: &[Vec<String>],
        weights: &[Vec<(usize, usize)>],
        rules: &[Rule],
        fills: &[crate::rule::Fill],
    ) -> bool {
        let cols = self.cols();
        if cols < 2 || rows.len() != weights.len() || rows.iter().any(|r| r.len() != cols) {
            return false;
        }
        let bold = |(weighted, heavy): (usize, usize)| weighted > 0 && heavy * 2 > weighted;
        let field = |w: &[(usize, usize)]| {
            bold(w[0]) && w[1..].iter().any(|c| c.0 > 0) && w[1..].iter().all(|c| !bold(*c))
        };
        let label = rows[0][0].trim();
        if !field(&weights[0]) || !label.chars().any(char::is_alphabetic) || is_numeric_cell(label)
        {
            return false;
        }
        let alike: Vec<usize> = (1..rows.len()).filter(|&r| field(&weights[r])).collect();
        if alike.is_empty() {
            return false;
        }
        let values = |r: usize| -> Vec<&str> {
            rows[r][1..]
                .iter()
                .map(|t| t.trim())
                .filter(|t| !t.is_empty())
                .collect()
        };
        let figures = |r: usize| {
            let v = values(r);
            !v.is_empty() && v.iter().all(|t| is_numeric_cell(t))
        };
        if alike.iter().all(|&r| figures(r)) && !figures(0) {
            return false;
        }
        // One value: no rule parts the cells after the label, in the first
        // row or the one under it. A label spanning columns the next row
        // names one by one is a banner over them (`Payment Due Dates` over
        // `(a) 4/15` and `(b) 6/15`).
        cols == 2 || {
            let m = self.merges(rules, fills);
            (0..2).all(|r| (2..cols).all(|c| m.continues_left[r * cols + c]))
        }
    }

    /// A list of fields ruled into rows, `Label | value` on every row, the
    /// first included: nothing sets the first row apart as a header.
    ///
    /// A spec sheet sets its data this way, with no header row. Taken for
    /// one, its first field named every value under it (`1 µL – 50,000 µL`
    /// was a `Positive displacement dispenser`) and was lost as a field. A
    /// header is set apart from the rows under it, and here nothing does
    /// that:
    ///
    /// - every row is ruled off from the next across both columns, and the
    ///   rule under the first is no heavier than the others;
    /// - the first row is shaded like some row under it, or bare like one;
    /// - in each column, the first row is set in the size and weight of
    ///   the rows under it;
    /// - a label stands in the first column of every row.
    ///
    /// A header set plain still names its column, and in fewer words than
    /// any value under it: `Settings | Recommendations` over sentences of
    /// advice. A field's value is one more value, `Positive displacement
    /// dispenser` beside `1` and `No`.
    fn lists_fields(
        &self,
        rows: &[Vec<String>],
        glyphs: &[crate::glyph::Glyph],
        rules: &[Rule],
        fills: &[crate::rule::Fill],
    ) -> bool {
        let n = self.rows();
        if self.cols() != 2 || n < MIN_FIELDS || rows.len() != n {
            return false;
        }
        let text = |r: usize, c: usize| rows[r].get(c).map_or("", |t| t.trim());
        let labelled = (0..n).all(|r| {
            let t = text(r, 0);
            t.chars().any(char::is_alphabetic) && !is_numeric_cell(t)
        });
        let words = |t: &str| t.split_whitespace().count();
        let first = text(0, 1);
        let names = first.chars().any(char::is_alphabetic)
            && (1..n)
                .map(|r| text(r, 1))
                .filter(|t| !t.is_empty())
                .all(|t| words(first) < words(t));
        // Nor over figures: a value column of figures on most rows under
        // the first, and a phrase in the first, is a header naming them
        // however plainly it is set (`Benzene %RSD Peak Area Ratios (n =
        // 6)` over `0.94`, `1.35`). A quantity is a value like the figures
        // (`38.7 ± 9.89`, `5 Years`), and one word as much a value as a
        // name (`APCI` over an ion source's temperatures): the list stands.
        let below: Vec<&str> = (1..n)
            .map(|r| text(r, 1))
            .filter(|t| !t.is_empty())
            .collect();
        let figures = below.iter().filter(|t| is_numeric_cell(t)).count();
        let heads_figures =
            figures * 2 > below.len() && !reads_as_quantity(first) && words(first) >= 2;
        if !labelled || names || heads_figures {
            return false;
        }
        let Some(strokes) = (1..n)
            .map(|r| self.rule_between(rules, r))
            .collect::<Option<Vec<f64>>>()
        else {
            return false;
        };
        let others = strokes[1..].iter().copied().fold(0.0, f64::max);
        if strokes[0] > others * HEAVIER_RULE {
            return false;
        }
        let shaded = |r: usize| self.band_has_fill(fills, r);
        if (1..n).all(|r| shaded(r) != shaded(0)) {
            return false;
        }
        let styles = self.cell_styles(glyphs);
        let bold = |(_, weighted, heavy): (f32, usize, usize)| weighted > 0 && heavy * 2 > weighted;
        (0..2).all(|c| {
            let Some(head) = styles[c] else {
                return true;
            };
            let below: Vec<(f32, usize, usize)> =
                (1..n).filter_map(|r| styles[r * 2 + c]).collect();
            if below.is_empty() {
                return true;
            }
            let mut sizes: Vec<f32> = below.iter().map(|s| s.0).collect();
            sizes.sort_by(f32::total_cmp);
            let size = sizes[sizes.len() / 2];
            let heavy = below.iter().filter(|s| bold(**s)).count() * 2 > below.len();
            (head.0 - size).abs() <= size * SAME_SIZE && bold(head) == heavy
        })
    }

    /// The stroke of the rule drawn under `row` across every column, or
    /// `None` where some column is left open.
    fn rule_between(&self, rules: &[Rule], row: usize) -> Option<f64> {
        let y = *self.ys.get(row)?;
        let at: Vec<&Rule> = rules
            .iter()
            .filter(|r| r.orientation == Orientation::Horizontal)
            .filter(|r| (r.axis_pos() - y).abs() <= AXIS_TOLERANCE)
            .collect();
        let ruled = self.xs.windows(2).all(|w| {
            let spans = at.iter().map(|r| (r.bbox.x0, r.bbox.x1));
            span_coverage(spans, w[0], w[1]) >= 0.6 * (w[1] - w[0])
        });
        ruled.then(|| at.iter().map(|r| r.bbox.height()).fold(0.0, f64::max))
    }

    /// Per cell, row by row: the size most of its glyphs are set in, how
    /// many of them declare a weight, and how many of those are bold.
    /// `None` for a cell with no ink.
    fn cell_styles(&self, glyphs: &[crate::glyph::Glyph]) -> Vec<Option<(f32, usize, usize)>> {
        let cols = self.cols();
        let mut sizes: Vec<Vec<f32>> = vec![Vec::new(); self.rows() * cols];
        let mut weights = vec![(0usize, 0usize); self.rows() * cols];
        for g in glyphs {
            if g.bbox.is_none() || g.text.trim().is_empty() {
                continue;
            }
            let (x, y) = g.center();
            let Some((r, c)) = self.cell_at(x, y) else {
                continue;
            };
            sizes[r * cols + c].push(g.font_size);
            if let Some(w) = g.weight {
                weights[r * cols + c].0 += 1;
                if w >= 600 {
                    weights[r * cols + c].1 += 1;
                }
            }
        }
        sizes
            .into_iter()
            .zip(weights)
            .map(|(mut s, (weighted, heavy))| {
                if s.is_empty() {
                    return None;
                }
                s.sort_by(f32::total_cmp);
                Some((s[s.len() / 2], weighted, heavy))
            })
            .collect()
    }

    /// The first row is set apart the way a header is: bold over a row
    /// that is not, or the first of a run of shaded bands over the rest.
    ///
    /// Presence of a header, not its absence: a header set like its data
    /// says nothing either way. A single shaded first row is no evidence
    /// here, since a table striped row by row opens a page with one.
    pub fn opens_styled(
        &self,
        glyphs: &[crate::glyph::Glyph],
        fills: &[crate::rule::Fill],
    ) -> bool {
        let run = self.leading_fill_run(fills);
        (self.cells_are_bold(glyphs, 0) && !self.row_is_bold(glyphs, 1))
            || (run >= 2 && run < self.rows())
    }

    /// Whether the `header` rows measured over a table whose title was read
    /// out above it head its rows.
    ///
    /// With the title gone, whatever row opens the table is presumed its
    /// header, and that presumption is what read a parts list's first
    /// product, or a spec sheet's first field (`Form Factor | SFF (2.5")`),
    /// as the name of every value under it. Here the row has to earn it, by
    /// being set apart from the rows under it in one of these ways:
    ///
    /// - bold over the first row under the header, which is not;
    /// - shaded over that row, which is bare, and not one stripe of many;
    /// - ruled off more heavily than the rows under it are from each other,
    ///   or ruled off where they are not;
    /// - a stacked header, more than one row deep;
    /// - words over quantities: in most of the columns whose cells below
    ///   are numbers, the first row names the column instead (`Zone |
    ///   Lumens` over `0-30 | 298`), and it is not set like the row under
    ///   it. In most of the columns both fill, that would mean one type
    ///   (quantities or words) and one weight, as a field list's rows are,
    ///   whose values are figures in some fields and words in others.
    ///
    /// The row it is set apart from is the first under it filling two
    /// cells or more, and more than half as many as the header names. A
    /// section's band under the header (`MAS Contract Year 4 …`, its words
    /// dealt into two cells by the cuts), bold or shaded as a header is,
    /// or the header's own second line (`run` of `Longer run`) is no row of
    /// the table's values to tell it from.
    pub(crate) fn heads_its_rows(
        &self,
        header: usize,
        rows: &[Vec<String>],
        glyphs: &[crate::glyph::Glyph],
        rules: &[Rule],
        fills: &[crate::rule::Fill],
    ) -> bool {
        let n = rows.len().min(self.rows());
        let h = header;
        if header == 0 || h >= n {
            return false;
        }
        let cols = self.cols();
        let text = |r: usize, c: usize| rows[r].get(c).map_or("", |t| t.trim());
        let filled = |r: usize| (0..cols).filter(|&c| !text(r, c).is_empty()).count();
        let first = (h..n)
            .find(|&r| filled(r) >= 2 && filled(r) * 2 > filled(0))
            .unwrap_or(h);
        let bold = self.cells_are_bold(glyphs, 0) && !self.row_is_bold(glyphs, first);
        let shaded = self.band_has_fill(fills, 0)
            && !self.band_has_fill(fills, first)
            && !(first + 1 < n && self.band_has_fill(fills, first + 1));
        let under = self.rule_between(rules, h);
        let below: Vec<Option<f64>> = (h + 1..n).map(|r| self.rule_between(rules, r)).collect();
        let others = below.iter().flatten().copied().fold(0.0, f64::max);
        let ruled = under.is_some_and(|u| {
            (others > 0.0 && u > others * HEAVIER_RULE)
                || (!below.is_empty() && below.iter().all(Option::is_none))
        });
        let stacked = header > 1;
        let (mut typed, mut named) = (0usize, 0usize);
        for c in 0..cols {
            let cells: Vec<&str> = (h..n)
                .map(|r| text(r, c))
                .filter(|t| !t.is_empty())
                .collect();
            if cells.is_empty()
                || cells.iter().filter(|t| reads_as_quantity(t)).count() * 2 <= cells.len()
            {
                continue;
            }
            typed += 1;
            let head = text(0, c);
            if head.chars().any(char::is_alphanumeric) && !reads_as_quantity(head) {
                named += 1;
            }
        }
        let contrast = typed > 0 && named * 2 > typed;
        let weights0 = self.cell_weights(glyphs, 0);
        let weights1 = self.cell_weights(glyphs, first);
        let heavy = |(w, h): (usize, usize)| w > 0 && h * 2 > w;
        let (mut compared, mut alike) = (0usize, 0usize);
        for c in 0..cols {
            let (a, b) = (text(0, c), text(first, c));
            if a.is_empty() || b.is_empty() {
                continue;
            }
            compared += 1;
            if reads_as_quantity(a) == reads_as_quantity(b)
                && heavy(weights0[c]) == heavy(weights1[c])
            {
                alike += 1;
            }
        }
        let same_kind = compared > 0 && alike * 2 > compared;
        bold || shaded || ruled || stacked || (contrast && !same_kind)
    }

    /// The first row's cells are set bold, whatever the row below is.
    pub fn opens_bold(&self, glyphs: &[crate::glyph::Glyph]) -> bool {
        self.cells_are_bold(glyphs, 0)
    }

    /// Most weighted glyphs in the row's band are bold — the same
    /// most-not-any predicate line assembly uses.
    fn row_is_bold(&self, glyphs: &[crate::glyph::Glyph], row: usize) -> bool {
        let (Some(&y0), Some(&y1)) = (self.ys.get(row), self.ys.get(row + 1)) else {
            return false;
        };
        let (mut weighted, mut heavy) = (0usize, 0usize);
        for g in glyphs {
            let (x, y) = g.center();
            if y < y0 || y >= y1 || x < self.bbox.x0 || x > self.bbox.x1 {
                continue;
            }
            if g.weight.is_some() {
                weighted += 1;
            }
            if g.weight.unwrap_or(400) >= 600 {
                heavy += 1;
            }
        }
        weighted > 0 && heavy * 2 > weighted
    }

    /// Rows below the header block set like a header row: they name the
    /// columns afresh for the rows beneath them.
    ///
    /// A table can rename its columns partway down: a shaded bold `By wt |
    /// By vol` under a `LB/Gal | g/L` header, or a SKU table's header row
    /// under a block of fields in the same box. Such a row sets names in two
    /// cells or more, each its own, and no figure or code, over two rows or
    /// more with something in a column it names, and is set apart from the
    /// rows under it one of three ways:
    ///
    /// - bold in two value cells or more, and every value cell (a first
    ///   column of row labels keeps its own weight: `VOC Content` set as
    ///   `Total` is, beside a bold `LB/Gal | g/L`), over a row that is not;
    /// - shaded as the header row is, over two rows that are not (striped
    ///   rows alternate);
    /// - its names turned on their side in two cells or more, as narrow
    ///   columns' names are.
    ///
    /// A band across the row is a section's label, not this, and so is a
    /// row under another bold row of names: the two are the table's top
    /// block, a header and its first row set alike (`Class | Mission
    /// Critical | Business Critical` over `Workload | High | Medium |
    /// Low`), and nothing above them names the columns for one to rename.
    pub fn restated_header_rows(
        &self,
        rows: &[Vec<String>],
        header_rows: usize,
        bands: &[usize],
        glyphs: &[crate::glyph::Glyph],
        fills: &[crate::rule::Fill],
    ) -> Vec<usize> {
        let n = rows.len().min(self.rows());
        // A code is one token of letters and digits: a part number, not a
        // name (`AFP2632`, `P40430-B21`).
        let code = |t: &str| {
            !t.contains(char::is_whitespace)
                && t.chars().any(|c| c.is_ascii_digit())
                && t.chars().any(char::is_alphabetic)
        };
        // Names, each its own: no figure, no code, none repeated (a row of
        // values under a banner repeats itself).
        let named: Vec<Vec<usize>> = (0..n)
            .map(|r| {
                (0..rows[r].len())
                    .filter(|&c| !rows[r][c].trim().is_empty())
                    .collect()
            })
            .collect();
        // One name alone stands for a unit restated over the one column the
        // rows under it fill (`By wt` over `LVP-VOC | 0.0%`) where it stands
        // in a value column; alone in the first column, it is a group's
        // label.
        let single = |r: usize| named[r].len() == 1 && named[r][0] > 0;
        let labels = |r: usize| {
            let cells: Vec<&str> = named[r].iter().map(|&c| rows[r][c].trim()).collect();
            let distinct: std::collections::HashSet<&&str> = cells.iter().collect();
            (cells.len() >= 2 || single(r))
                && distinct.len() == cells.len()
                && cells
                    .iter()
                    .all(|t| t.chars().any(char::is_alphabetic) && !is_numeric_cell(t) && !code(t))
        };
        let heavy = |(weighted, heavy): (usize, usize)| weighted > 0 && heavy * 2 > weighted;
        // Value cells holding weighted text, and how many of them are bold.
        let bold_values = |r: usize| {
            let w = self.cell_weights(glyphs, r);
            let values: Vec<&(usize, usize)> = w.iter().skip(1).filter(|c| c.0 > 0).collect();
            let bold = values.iter().filter(|c| heavy(***c)).count();
            (values.len(), bold)
        };
        // Every value cell bold, two or more, or the one name of a single
        // unit row: one bold word beside a label is emphasis.
        let bold = |r: usize| {
            let (values, bold) = bold_values(r);
            bold == values && (bold >= 2 || (bold == 1 && single(r)))
        };
        let plain = |r: usize| {
            let (values, bold) = bold_values(r);
            bold < values || values == 0
        };
        // The row and the header shaded by fills of their own: a page's
        // watermark drawn under the table's top rows, and ending above the
        // rest, shades none of them. The rows under it unshaded by any fill:
        // a table laid on a background is shaded throughout, and no row of
        // it stands apart by its shading.
        let shaded = |r: usize| self.rows_have_own_fill(fills, r..r + 1);
        let header_shaded = self.rows_have_own_fill(fills, 0..header_rows.max(1));
        let unshaded = |r: usize| !self.band_has_fill(fills, r);
        // Turned glyphs inside the table, each with the cell its ink is in.
        let turned_at: Vec<(&crate::glyph::Glyph, (usize, usize))> = glyphs
            .iter()
            .filter(|g| {
                g.bbox.is_some()
                    && !g.text.trim().is_empty()
                    && matches!(g.rotation_bucket().rem_euclid(360), 90 | 270)
            })
            .filter_map(|g| {
                let b = g.bbox.unwrap();
                Some((g, self.cell_at((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5)?))
            })
            .collect();
        // Cells of row `r` naming their column on its side: a turned line
        // wholly inside the row. A group label turned down the rows it
        // groups runs on past the row, and names none.
        let turned = |r: usize| {
            let cells: std::collections::HashSet<usize> = turned_at
                .iter()
                .filter(|(_, (row, _))| *row == r)
                .filter(|(g, _)| {
                    let fs = g.font_size.max(1.0) as f64;
                    !turned_at.iter().any(|(o, (row, _))| {
                        *row != r && (o.origin.0 - g.origin.0).abs() < fs * 0.3
                    })
                })
                .map(|(_, (_, c))| *c)
                .collect();
            cells.len() >= 2
        };
        // A row under it at least. One row under a row of names is as
        // likely its value, but the row is set apart as a header is (bold or
        // shaded over a plain row), and a header names one row's columns as
        // surely as ten: a short VOC table's `VOC Content | LB/Gal | g/L`
        // over its one `Total`.
        (header_rows.max(1)..n.saturating_sub(1))
            .filter(|&r| {
                !bands.contains(&r)
                    && labels(r)
                    // One name straight under the header carries on the
                    // header's own label, wrapped (`Body weight` over
                    // `(kg)`): a unit restated stands partway down, under a
                    // row of the data.
                    && !(single(r) && r == header_rows.max(1))
                    && !(r > 0 && labels(r - 1) && bold(r - 1))
                    && rows[r]
                        .iter()
                        .zip(&rows[r + 1])
                        .any(|(a, b)| !a.trim().is_empty() && !b.trim().is_empty())
                    && ((bold(r) && plain(r + 1))
                        || (shaded(r)
                            && header_shaded
                            && unshaded(r + 1)
                            && (r + 2 >= n || unshaded(r + 2)))
                        || turned(r))
            })
            .collect()
    }

    /// The header block grown down through a stacked header's rows.
    ///
    /// A header cell ruled open into the row beneath it — `Item` spanning
    /// two rows beside `Price` over `Unit` and `Line` — makes that row part
    /// of the header. Open cells alone do not say so: a stub column ruled
    /// nowhere is open on every row, and following it made a 30-row table
    /// all header. So the rows taken in must read as labels (no numbers),
    /// the block stays within [`MAX_HEADER_ROWS`], and a rule drawn across
    /// the table must close it, the way a header is ruled off from its
    /// data: across every column but those whose cell runs on across the
    /// line, a stub labelling header and body alike. Otherwise the measured
    /// count stands.
    pub fn stacked_header_rows(
        &self,
        header_rows: usize,
        rows: &[Vec<String>],
        merges: &Merges,
        rules: &[Rule],
    ) -> usize {
        let cols = self.cols();
        if header_rows == 0 || cols == 0 || merges.continues_above.len() != rows.len() * cols {
            return header_rows;
        }
        let mut n = header_rows;
        while n < MAX_HEADER_ROWS
            && n + 1 < rows.len()
            && (0..cols).any(|c| merges.continues_above[n * cols + c])
            && rows[n]
                .iter()
                .all(|t| t.trim().is_empty() || !is_numeric_cell(t.trim()))
        {
            n += 1;
        }
        if n == header_rows {
            return header_rows;
        }
        // Ruled off under the columns the header ends in. A stub whose
        // label spans the header and the body alike, every row of the table
        // (`Tolerance on Length` beside every row), runs on across the line
        // and is ruled under nowhere there.
        let y = self.ys[n];
        // The stub runs on through every row of the table, from its first:
        // it labels the whole table. A group's label spanning some rows is
        // no reason to leave the header open.
        let runs_on = |c: usize| (1..rows.len()).all(|r| merges.continues_above[r * cols + c]);
        let runs: Vec<(f64, f64)> = rules
            .iter()
            .filter(|r| {
                r.orientation == Orientation::Horizontal
                    && (r.bbox.y0 - y).abs() <= EDGE_TOLERANCE * 2.0
            })
            .map(|r| (r.bbox.x0, r.bbox.x1))
            .collect();
        let closed_under = |skip: &dyn Fn(usize) -> bool| {
            let (mut closed, mut width) = (0.0, 0.0);
            for c in (0..cols).filter(|&c| !skip(c)) {
                let (x0, x1) = (self.xs[c], self.xs[c + 1]);
                closed += span_coverage(runs.iter().copied(), x0, x1);
                width += x1 - x0;
            }
            width > 0.0 && closed >= 0.9 * width
        };
        if closed_under(&|_| false) {
            return n;
        }
        // Closed under all but the stub: the rows taken in name their
        // columns, each label its own, none repeated among them or in the
        // rows of data under them. A table's data repeats itself (`Yes` row
        // after row) under a stub run down a continued page.
        let trimmed = |r: usize| -> Vec<&str> {
            rows[r]
                .iter()
                .map(|t| t.trim())
                .filter(|t| !t.is_empty())
                .collect()
        };
        let taken: Vec<&str> = (header_rows..n).flat_map(|r| trimmed(r)).collect();
        let below: Vec<&str> = (n..rows.len()).flat_map(|r| trimmed(r)).collect();
        // A stub labels a table's rows: three of data or more under the
        // header, not a form's box of two.
        let named = rows.len() >= n + 3
            && (header_rows..n).all(|r| trimmed(r).len() >= 2)
            && taken
                .iter()
                .enumerate()
                .all(|(i, a)| !taken[..i].contains(a) && !below.contains(a));
        if named && closed_under(&runs_on) {
            n
        } else {
            header_rows
        }
    }

    /// Every cell of the row that holds weighted text is mostly bold.
    ///
    /// Asked of the whole row instead, a key/value table passes: its label
    /// column is bold on every row, and in the first row a long bold label
    /// beside a short plain amount (`Subtotal | 38.60`) is a bold majority,
    /// so the amount was named a column header. A header row is bold in
    /// each of its cells.
    fn cells_are_bold(&self, glyphs: &[crate::glyph::Glyph], row: usize) -> bool {
        let counts = self.cell_weights(glyphs, row);
        counts.iter().any(|(w, _)| *w > 0)
            && counts
                .iter()
                .all(|(weighted, heavy)| *weighted == 0 || heavy * 2 > *weighted)
    }

    /// The lines of text in `x0..x1`, `y0..y1`, top down: each line's
    /// baseline, its text (words parted where the glyphs part by a quarter
    /// of an em) and its size.
    fn lines_in(
        glyphs: &[crate::glyph::Glyph],
        (x0, x1, y0, y1): (f64, f64, f64, f64),
    ) -> Vec<(f64, String, f64)> {
        let mut inside: Vec<&crate::glyph::Glyph> = glyphs
            .iter()
            .filter(|g| g.bbox.is_some() && !g.text.trim().is_empty())
            .filter(|g| {
                let (x, y) = g.center();
                x >= x0 && x < x1 && y >= y0 && y < y1
            })
            .collect();
        inside.sort_by(|a, b| a.origin.1.total_cmp(&b.origin.1));
        let mut lines: Vec<Vec<&crate::glyph::Glyph>> = Vec::new();
        for g in inside {
            let size = f64::from(g.font_size).max(1.0);
            match lines.last_mut() {
                Some(l) if (g.origin.1 - l[0].origin.1).abs() <= 0.5 * size => l.push(g),
                _ => lines.push(vec![g]),
            }
        }
        lines
            .into_iter()
            .map(|mut l| {
                l.sort_by(|a, b| a.origin.0.total_cmp(&b.origin.0));
                let size = l.iter().map(|g| f64::from(g.font_size)).fold(1.0, f64::max);
                let base = l.iter().map(|g| g.origin.1).sum::<f64>() / l.len() as f64;
                let mut text = String::new();
                let mut end: Option<f64> = None;
                for g in &l {
                    let b = g.bbox.unwrap();
                    if end.is_some_and(|e| b.x0 - e > 0.25 * size) {
                        text.push(' ');
                    }
                    text.push_str(&g.text);
                    end = Some(end.map_or(b.x1, |e: f64| e.max(b.x1)));
                }
                (base, text, size)
            })
            .collect()
    }

    /// Rows that open a section of a list of fields by weight alone: a bold
    /// label with nothing beside it, over a field whose label is set plain
    /// (`Chromatographic Conditions` over `Carrier Gas | Helium`).
    ///
    /// Set plain, a label with an empty value is a field left blank. A bold
    /// label beside a value is a field too (`Headspace Parameters |
    /// <sampler>` over its settings), and so is one over another bold label
    /// (a list whose labels are all bold). Right under a header that leaves
    /// its stub empty, it is that header's stub head.
    pub fn bold_section_rows(
        &self,
        rows: &[Vec<String>],
        glyphs: &[crate::glyph::Glyph],
        header_rows: usize,
    ) -> Vec<usize> {
        let n = rows.len().min(self.rows());
        // A list of fields: a label and its value. A wider table's bands over
        // its rows are `group_label`'s, read from full rows beneath them.
        if self.cols() != 2 {
            return Vec::new();
        }
        let bold = |(weighted, heavy): (usize, usize)| weighted > 0 && heavy * 2 > weighted;
        let label = |r: usize| rows[r].first().map_or("", |t| t.trim());
        let valued = |r: usize| rows[r].iter().skip(1).any(|t| !t.trim().is_empty());
        let blank = |r: usize| rows[r].iter().all(|t| t.trim().is_empty());
        let bold_label = |r: usize| {
            self.cell_weights(glyphs, r)
                .first()
                .is_some_and(|w| bold(*w))
        };
        (header_rows..n)
            .filter(|&r| {
                let l = label(r);
                if l.is_empty() || !l.chars().any(char::is_alphabetic) || is_numeric_cell(l) {
                    return false;
                }
                // A heading's few words, not a sentence: a note set bold over
                // the rows (`Steps 7 to 9 are only necessary …`) is no section.
                let sentence = l
                    .strip_suffix('.')
                    .and_then(|w| w.split_whitespace().last())
                    .is_some_and(|w| w.chars().count() >= 3 && w.chars().all(char::is_lowercase));
                if l.split_whitespace().count() > SECTION_LABEL_WORDS || sentence {
                    return false;
                }
                if valued(r) || !bold_label(r) {
                    return false;
                }
                // Under a header whose stub is empty, it is the stub's head
                // (`Conference Name` beside `Organizer(s)` over the rows).
                if r == header_rows && r > 0 && label(r - 1).is_empty() {
                    return false;
                }
                (r + 1..n)
                    .find(|&k| !blank(k))
                    .is_some_and(|k| valued(k) && !label(k).is_empty() && !bold_label(k))
            })
            .collect()
    }

    /// Where the band `y0..y1` of the table holds values stacked one per
    /// line, a line of each beside a line of the others: the cuts between
    /// those lines, and the columns stacked so, every line of theirs on one
    /// of the values' lines (those with none, empty, among them).
    ///
    /// An invoice's totals set in one ruled cell, `TOTAL (Excluding VAT)`
    /// over `TOTAL VAT`, beside one cell of their amounts, `17,937.50` over
    /// `3,587.50`. Each line is a row of the table, and read whole, each
    /// cell ran its rows together. A column of figures stacked so is the
    /// evidence (two lines or more, each a figure); the columns beside it
    /// stack lines on the same baselines, one by one, none carrying the line
    /// over it on in lower case. Wrapped text beside wrapped text lines up
    /// too, and holds no column of figures.
    fn stacked_cuts(
        &self,
        glyphs: &[crate::glyph::Glyph],
        y0: f64,
        y1: f64,
    ) -> Option<(Vec<f64>, Vec<usize>)> {
        let cols = self.cols();
        let lines: Vec<Vec<(f64, String, f64)>> = (0..cols)
            .map(|c| Self::lines_in(glyphs, (self.xs[c], self.xs[c + 1], y0, y1)))
            .collect();
        // A figure whole on its line: digits and their dressing alone, no
        // word or unit run on (`36,0 à` over `38,0` is a range wrapped).
        let figure = |t: &str| is_numeric_cell(t.trim());
        // A figure grouped by spaces wraps at a space: `453` over `500,00`
        // is one amount, the second line a group of three and its decimals.
        let broken_group = |c: usize| {
            lines[c].windows(2).any(|w| {
                let (a, b) = (w[0].1.trim(), w[1].1.trim());
                let mut rest = b.chars();
                (1..=3).contains(&a.len())
                    && a.chars().all(|ch| ch.is_ascii_digit())
                    && rest.by_ref().take(3).filter(char::is_ascii_digit).count() == 3
                    && rest.next().is_none_or(|ch| ".,".contains(ch) || ch == ' ')
            })
        };
        // A value wrapped onto the next line ends on its joiner (`2026-08-`
        // over `20`, `ST-` over `5647`, `Krüger,` over a given name) or goes
        // on in lower case: one value, not two.
        let runs_on = |c: usize| {
            lines[c].iter().skip(1).any(|l| {
                l.1.trim_start()
                    .starts_with(|ch: char| ch.is_lowercase() || ch == '(' || ch == '[')
            }) || broken_group(c)
                || lines[c]
                    .iter()
                    .rev()
                    .skip(1)
                    .any(|l| l.1.trim_end().ends_with(['-', '\u{2013}', '/', ',']))
        };
        let key = (0..cols).find(|&c| {
            lines[c].len() >= 2 && lines[c].iter().all(|l| figure(&l.1)) && !runs_on(c)
        })?;
        let bases: Vec<f64> = lines[key].iter().map(|l| l.0).collect();
        let pairs = |c: usize| {
            lines[c].len() == bases.len()
                && lines[c]
                    .iter()
                    .zip(&bases)
                    .all(|(l, b)| (l.0 - b).abs() <= 0.5 * l.2)
        };
        let paired: Vec<usize> = (0..cols).filter(|&c| pairs(c)).collect();
        if paired.len() < 2 || paired.iter().any(|&c| runs_on(c)) {
            return None;
        }
        // A column with lines on some of the values' baselines, a later
        // one among them, and none between them stacks with them too, its
        // cells empty in the other rows (a note mark beside the second
        // figure). One line on the first baseline alone is a label over all
        // the band's rows (`Al` beside `INAA` over `WDXRF`).
        let on_bases = |c: usize| {
            let at: Option<Vec<usize>> = lines[c]
                .iter()
                .map(|l| bases.iter().position(|b| (l.0 - b).abs() <= 0.5 * l.2))
                .collect();
            at.is_some_and(|at| at.is_empty() || at.iter().any(|&i| i > 0))
        };
        let stacked: Vec<usize> = (0..cols).filter(|&c| on_bases(c)).collect();
        let cuts = bases.windows(2).map(|w| (w[0] + w[1]) / 2.0).collect();
        Some((cuts, stacked))
    }

    /// Part each ruled row holding values stacked one per line
    /// ([`Grid::stacked_cuts`]) into one row per line. The cells beside them
    /// that do not stack so are read whole across the new rows
    /// ([`Grid::stacked_merges`]).
    ///
    /// Returns each row parted and how many rows it added, top down.
    pub fn split_stacked_rows(&mut self, glyphs: &[crate::glyph::Glyph]) -> Vec<(usize, usize)> {
        let mut added = Vec::new();
        if self.layout.is_some() || self.cols() < 2 {
            return added;
        }
        for r in (0..self.rows()).rev() {
            if let Some((cuts, _)) = self.stacked_cuts(glyphs, self.ys[r], self.ys[r + 1]) {
                added.push((r, cuts.len()));
                for (i, y) in cuts.into_iter().enumerate() {
                    self.ys.insert(r + 1 + i, y);
                }
            }
        }
        added.reverse();
        added
    }

    /// The cells of rows [`Grid::split_stacked_rows`] parted that do not
    /// stack their lines with the values: each spans the rows its band was
    /// parted into, read whole (a block of text beside the totals).
    ///
    /// The parted bands are found again as the split found them: rows
    /// between boundaries a rule runs along, the boundaries inside them
    /// unruled, inside one of the bands `parted` (`y0..y1`) the split
    /// parted, and exactly where the values' lines part. Rows the table
    /// set apart itself are none of them.
    ///
    /// Returns each block merged so: its rows, and its column.
    pub fn stacked_merges(
        &self,
        glyphs: &[crate::glyph::Glyph],
        rules: &[Rule],
        parted: &[(f64, f64)],
        merges: &mut Merges,
    ) -> Vec<(std::ops::Range<usize>, usize)> {
        let mut blocks = Vec::new();
        let (rows, cols) = (self.rows(), self.cols());
        if self.layout.is_some() || cols < 2 || merges.continues_above.len() != rows * cols {
            return blocks;
        }
        let (left, right) = (self.xs[0], self.xs[cols]);
        let ruled = |k: usize| {
            let y = self.ys[k];
            rules.iter().any(|r| {
                r.orientation == Orientation::Horizontal
                    && (r.axis_pos() - y).abs() <= EDGE_TOLERANCE * 2.0
                    && r.bbox.x1.min(right) - r.bbox.x0.max(left) > 0.0
            })
        };
        let mut a = 0;
        while a < rows {
            let mut b = a + 1;
            while b < rows && !ruled(b) {
                b += 1;
            }
            let inner = &self.ys[a + 1..b];
            let split = parted
                .iter()
                .any(|&(y0, y1)| inner.iter().all(|&y| y > y0 && y < y1));
            if b - a >= 2 && split {
                if let Some((cuts, stacked)) = self.stacked_cuts(glyphs, self.ys[a], self.ys[b]) {
                    let at = |i: usize| self.ys[a + 1 + i];
                    if cuts.len() == b - a - 1
                        && cuts
                            .iter()
                            .enumerate()
                            .all(|(i, y)| (y - at(i)).abs() <= 0.5)
                    {
                        for c in (0..cols).filter(|c| !stacked.contains(c)) {
                            for k in a + 1..b {
                                merges.continues_above[k * cols + c] = true;
                            }
                            blocks.push((a..b, c));
                        }
                    }
                }
            }
            a = b;
        }
        blocks
    }

    /// Per cell of the row: glyphs with a known weight, and how many of
    /// those are bold.
    fn cell_weights(&self, glyphs: &[crate::glyph::Glyph], row: usize) -> Vec<(usize, usize)> {
        let mut counts = vec![(0usize, 0usize); self.cols()];
        let (Some(&y0), Some(&y1)) = (self.ys.get(row), self.ys.get(row + 1)) else {
            return counts;
        };
        for g in glyphs {
            let (x, y) = g.center();
            if y < y0 || y >= y1 || g.weight.is_none() {
                continue;
            }
            let Some(c) = self.xs.windows(2).position(|w| x >= w[0] && x < w[1]) else {
                continue;
            };
            counts[c].0 += 1;
            if g.weight.unwrap_or(400) >= 600 {
                counts[c].1 += 1;
            }
        }
        counts
    }

    /// Whether fills cover most of the row band: their union across the
    /// band's vertical midpoint spans at least half the grid width. Union,
    /// not any-single-fill — a header row shaded cell-by-cell is five narrow
    /// fills, none individually wide.
    fn band_has_fill(&self, fills: &[crate::rule::Fill], row: usize) -> bool {
        self.shaded_across(fills, row, None)
    }

    /// Rows `a` and `b` are set alike: in one size ([`SAME_SIZE`]) and one
    /// weight, shaded or bare alike, and with the table's outer edges drawn
    /// through both or through neither. A table sets its section bands one
    /// way throughout; a title over it, or a header, is set apart.
    pub(crate) fn rows_set_alike(
        &self,
        glyphs: &[crate::glyph::Glyph],
        rules: &[Rule],
        fills: &[crate::rule::Fill],
        a: usize,
        b: usize,
    ) -> bool {
        if a + 1 >= self.ys.len() || b + 1 >= self.ys.len() {
            return false;
        }
        let size = |r: usize| {
            let (top, bottom) = (self.ys[r], self.ys[r + 1]);
            let mut sizes: Vec<f32> = glyphs
                .iter()
                .filter(|g| g.bbox.is_some() && !g.text.trim().is_empty())
                .filter(|g| {
                    let (x, y) = g.center();
                    y > top && y < bottom && x > self.bbox.x0 && x < self.bbox.x1
                })
                .map(|g| g.font_size)
                .collect();
            sizes.sort_by(f32::total_cmp);
            sizes.get(sizes.len() / 2).copied()
        };
        let edges = |r: usize| {
            let mid = (self.ys[r] + self.ys[r + 1]) / 2.0;
            let at = |x: f64| {
                rules.iter().any(|rule| {
                    rule.orientation == Orientation::Vertical
                        && (rule.axis_pos() - x).abs() <= EDGE_TOLERANCE
                        && rule.bbox.y0 <= mid
                        && rule.bbox.y1 >= mid
                })
            };
            (at(self.xs[0]), at(self.xs[self.xs.len() - 1]))
        };
        let (Some(sa), Some(sb)) = (size(a), size(b)) else {
            return false;
        };
        (sa - sb).abs() <= sb * SAME_SIZE
            && self.row_is_bold(glyphs, a) == self.row_is_bold(glyphs, b)
            && self.band_has_fill(fills, a) == self.band_has_fill(fills, b)
            && edges(a) == edges(b)
    }

    /// Whether rows `rows` are shaded by fills of their own, across half the
    /// table at the first of them: each fill starting and ending within
    /// those rows. A background laid under the page or the whole table (a
    /// watermark's grey drawing) runs on past them, and shades no row of its
    /// own.
    fn rows_have_own_fill(
        &self,
        fills: &[crate::rule::Fill],
        rows: std::ops::Range<usize>,
    ) -> bool {
        let (Some(&top), Some(&bottom)) = (self.ys.get(rows.start), self.ys.get(rows.end)) else {
            return false;
        };
        self.shaded_across(fills, rows.start, Some((top, bottom)))
    }

    /// Whether fills cover half the table's width across row `row`'s middle,
    /// counting only those lying `within` the given heights where set.
    fn shaded_across(
        &self,
        fills: &[crate::rule::Fill],
        row: usize,
        within: Option<(f64, f64)>,
    ) -> bool {
        let (Some(&y0), Some(&y1)) = (self.ys.get(row), self.ys.get(row + 1)) else {
            return false;
        };
        let mid = (y0 + y1) / 2.0;
        let mut spans: Vec<(f64, f64)> = fills
            .iter()
            .filter(|f| f.bbox.y0 <= mid && f.bbox.y1 >= mid)
            .filter(|f| {
                within.is_none_or(|(top, bottom)| {
                    f.bbox.y0 >= top - EDGE_TOLERANCE && f.bbox.y1 <= bottom + EDGE_TOLERANCE
                })
            })
            .map(|f| (f.bbox.x0.max(self.bbox.x0), f.bbox.x1.min(self.bbox.x1)))
            .filter(|(a, b)| b > a)
            .collect();
        spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut covered = 0.0;
        let mut cursor = f64::MIN;
        for (a, b) in spans {
            let a = a.max(cursor);
            if b > a {
                covered += b - a;
                cursor = b;
            }
        }
        covered >= 0.5 * (self.bbox.x1 - self.bbox.x0)
    }

    /// The paints of the shading across row `row`'s middle within the
    /// table, each once, in the order they are drawn.
    fn row_paints(&self, fills: &[crate::rule::Fill], row: usize) -> Vec<crate::rule::Paint> {
        let (Some(&y0), Some(&y1)) = (self.ys.get(row), self.ys.get(row + 1)) else {
            return Vec::new();
        };
        let mid = (y0 + y1) / 2.0;
        let mut paints = Vec::new();
        for p in fills
            .iter()
            .filter(|f| {
                f.bbox.y0 <= mid
                    && f.bbox.y1 >= mid
                    && f.bbox.x1 > self.bbox.x0
                    && f.bbox.x0 < self.bbox.x1
            })
            .filter_map(|f| f.paint)
        {
            if !paints.contains(&p) {
                paints.push(p);
            }
        }
        paints
    }

    /// Length of the leading run of filled row bands, stopping at the first
    /// unfilled band. Zebra striping self-limits: its bands alternate, so the
    /// run never exceeds 1.
    fn leading_fill_run(&self, fills: &[crate::rule::Fill]) -> usize {
        (0..self.rows())
            .take_while(|r| self.band_has_fill(fills, *r))
            .count()
    }

    /// Whether the rows from `r` on are shaded every other row, `r` first:
    /// two shaded rows at least, a bare one between.
    fn stripes_from(&self, fills: &[crate::rule::Fill], r: usize) -> bool {
        let n = self.rows();
        r + 2 < n && (r..n.min(r + 5)).all(|k| self.band_has_fill(fills, k) == ((k - r) % 2 == 0))
    }
    /// Cell index containing a point, as (row, col).
    pub fn cell_at(&self, x: f64, y: f64) -> Option<(usize, usize)> {
        let c = self.xs.windows(2).position(|w| x >= w[0] && x < w[1])?;
        let r = self.ys.windows(2).position(|w| y >= w[0] && y < w[1])?;
        Some((r, c))
    }
}

/// Most words a column head's line holds in one column: more is a sentence.
const MAX_HEAD_WORDS: usize = 8;

/// Baseline-to-baseline distance, in font sizes, up to which two lines are
/// one wrapped label rather than the values of two rows. Wrapped lines sit
/// about 1.1–1.25 apart; a padded table row is 1.5 or more.
const PARAGRAPH_LEADING: f64 = 1.35;

/// Baseline-to-baseline distance, in font sizes, up to which a line that
/// carries on the one above it (starts lowercase, or follows a comma) is
/// still that line's continuation. Set on the baselines of padded rows its
/// lines are a row apart; two lines' worth is a blank line between them.
const CARRIED_LEADING: f64 = 2.0;

/// How far, in font sizes, a value's text may sit from the middle of the
/// rows it is centred across, or vary in where it sits in each of its rows,
/// and still be placed the same way.
const PLACEMENT_TOLERANCE: f64 = 0.3;

/// Baseline difference, in font sizes, within which a label and a value
/// beside it are set on one line. A label centred across merged rows is off
/// its neighbours' baselines by a fifth of its size or more.
const SHARED_BASELINE: f64 = 0.1;

/// A cell's lines, top to bottom, as one value.
///
/// A line ending in a hyphen joins the next with no space. A narrow column
/// breaks a value at a hyphen it already has, so the hyphen is the value's
/// own: a product code `FR-` over `15T` is `FR-15T`, a name `Ben-` over
/// `David` is `Ben-David`. Where the break could be a typesetter's, inside a
/// word (`Fluo-` over `rescent`, as [`HyphenJoin`](crate::block::HyphenJoin)
/// reads it), the hyphen is set down as [`CELL_BREAK`] instead, for the
/// document to settle: a narrow cell breaks a compound at its own hyphen
/// (`long-` over `term`) more often than prose does, and only the rest of
/// the document says which of the two this is. See
/// `document::settle_cell_breaks`.
/// The text of glyphs `ids`, set between `x0` and `x1`, read a side at a
/// time where a rule drawn down between those edges parts them: the lines
/// over the rules, each side's lines left to right, then the lines under
/// them. `None` where no rule stands there with ink on both sides of it
/// within its height. See [`Grid::cell_texts_parted`].
fn parted_text(
    glyphs: &[crate::glyph::Glyph],
    ids: &[usize],
    rules: &[Rule],
    x0: f64,
    x1: f64,
) -> Option<String> {
    let centre = |g: &crate::glyph::Glyph| {
        g.bbox
            .map_or(g.origin, |b| ((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5))
    };
    let within = |r: &Rule, i: usize| {
        let y = centre(&glyphs[i]).1;
        y > r.bbox.y0 && y < r.bbox.y1
    };
    // A side of the rule reads as text set in a box of its own: two lines
    // or more, one of them three words or more. The rule between two boxes
    // of text parts them; the rules of a small table set in the cell part
    // its columns of values, which read across its rows.
    let boxed = |r: &Rule, left: bool| {
        let side: Vec<crate::glyph::Glyph> = ids
            .iter()
            .filter(|&&i| within(r, i) && (centre(&glyphs[i]).0 < r.axis_pos()) == left)
            .map(|&i| glyphs[i].clone())
            .collect();
        let lines = crate::line::assemble(&side);
        lines.len() >= PARTED_SIDE_LINES
            && lines
                .iter()
                .any(|l| l.text.split_whitespace().count() >= PARTED_LINE_WORDS)
    };
    // The rule runs between the two: through no glyph's ink. A stroke
    // drawn through a word is no box's edge.
    let clear = |r: &Rule| {
        let x = r.axis_pos();
        !ids.iter().any(|&i| {
            within(r, i)
                && glyphs[i]
                    .bbox
                    .is_some_and(|b| b.x0 < x - RULE_CLEARANCE && b.x1 > x + RULE_CLEARANCE)
        })
    };
    let parting: Vec<&Rule> = rules
        .iter()
        .filter(|r| {
            let x = r.axis_pos();
            r.orientation == Orientation::Vertical
                && x > x0 + PARTING_RULE_MARGIN
                && x < x1 - PARTING_RULE_MARGIN
                && clear(r)
                && boxed(r, true)
                && boxed(r, false)
        })
        .collect();
    if parting.is_empty() {
        return None;
    }
    let top = parting.iter().map(|r| r.bbox.y0).fold(f64::MAX, f64::min);
    let foot = parting.iter().map(|r| r.bbox.y1).fold(f64::MIN, f64::max);
    // Each glyph's place: over the rules, between them on the side it stands,
    // or under them.
    let mut groups: std::collections::BTreeMap<(u8, usize), Vec<crate::glyph::Glyph>> =
        Default::default();
    for &i in ids {
        let (x, y) = centre(&glyphs[i]);
        let key = if y <= top {
            (0, 0)
        } else if y >= foot {
            (2, 0)
        } else {
            let side = parting
                .iter()
                .filter(|r| r.axis_pos() < x && y > r.bbox.y0 && y < r.bbox.y1)
                .count();
            (1, side)
        };
        groups.entry(key).or_default().push(glyphs[i].clone());
    }
    let lines: Vec<String> = groups
        .values()
        .flat_map(|gs| {
            turned_in_order(crate::line::assemble(gs))
                .into_iter()
                .map(|l| l.text)
        })
        .collect();
    Some(join_cell_lines(lines.iter().map(String::as_str)))
}

fn join_cell_lines<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    use crate::block::HyphenJoin;
    let mut out = String::new();
    for l in lines {
        if !out.is_empty() {
            match out.strip_suffix('-') {
                Some(stem)
                    if stem.chars().next_back().is_some_and(char::is_alphanumeric)
                        && l.starts_with(char::is_alphanumeric) =>
                {
                    if HyphenJoin::of(stem, l) == HyphenJoin::Word {
                        out.pop();
                        out.push(CELL_BREAK);
                    }
                }
                _ if crate::block::closed_join(&out, l) => {}
                _ => out.push(' '),
            }
        }
        out.push_str(l);
    }
    out
}

/// A cell's lines in reading order where they are all set on their side:
/// across the page, left to right for text reading up (90°), right to left
/// for text reading down (270°). Level lines keep the order they come in.
fn turned_in_order(mut lines: Vec<crate::line::Line>) -> Vec<crate::line::Line> {
    let turn = |l: &crate::line::Line| l.rotation_bucket.rem_euclid(360);
    if lines.len() > 1 && lines.iter().all(|l| matches!(turn(l), 90 | 270)) {
        let down = lines.iter().all(|l| turn(l) == 270);
        lines.sort_by(|a, b| {
            let o = a.bbox.x0.total_cmp(&b.bbox.x0);
            if down {
                o.reverse()
            } else {
                o
            }
        });
    }
    lines
}

/// A hyphen ending one of a cell's lines inside what may be one word broken
/// across them, until the document settles it: a soft hyphen, which no
/// glyph's text carries (extraction prints or drops each one it reads).
pub const CELL_BREAK: char = '\u{AD}';

/// Total length of `[x0, x1]` covered by the union of `spans`.
fn span_coverage(spans: impl Iterator<Item = (f64, f64)>, x0: f64, x1: f64) -> f64 {
    let mut v: Vec<(f64, f64)> = spans
        .map(|(a, b)| (a.max(x0), b.min(x1)))
        .filter(|(a, b)| b > a)
        .collect();
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let (mut covered, mut cursor) = (0.0, f64::MIN);
    for (a, b) in v {
        let a = a.max(cursor);
        if b > a {
            covered += b - a;
            cursor = b;
        }
    }
    covered
}

/// Fewest rows a ruled field list is read from. With one field under the
/// first there is too little to say how the fields are set.
const MIN_FIELDS: usize = 3;

/// How many times heavier than the rules between the rows below it the rule
/// under a first row has to be to rule it off as a header.
const HEAVIER_RULE: f64 = 1.5;

/// Relative difference within which two cells are set in one size.
const SAME_SIZE: f32 = 0.05;

/// Most words in a section's label over a list of fields: a heading
/// (`Chromatographic Conditions`, `Federal estimated tax payments you've
/// already made`), not a note or a line run on across the cells.
const SECTION_LABEL_WORDS: usize = 8;

/// Deepest stacked header the fill-run signal may claim. Real matrices run
/// two banners plus the column-name row; anything deeper is a fully shaded
/// table masquerading as a header block.
const MAX_HEADER_ROWS: usize = 4;

/// The type-contrast half of header detection, on cell text alone.
///
/// `styled` is the style veto: a bold-against-the-data or exclusively-banded
/// first row keeps its header status regardless of content.
fn decide_header_rows(rows: &[Vec<String>], styled: bool) -> usize {
    if styled {
        return 1;
    }
    let cols = rows[0].len();
    let mut numeric_cols = 0usize;
    let mut row0_numeric_in_those = 0usize;
    for c in 0..cols {
        let (mut nonempty, mut numeric) = (0usize, 0usize);
        for row in rows.iter().skip(1) {
            let cell = row.get(c).map(|s| s.trim()).unwrap_or("");
            if cell.is_empty() {
                continue;
            }
            nonempty += 1;
            if is_numeric_cell(cell) {
                numeric += 1;
            }
        }
        if nonempty > 0 && numeric * 2 > nonempty {
            numeric_cols += 1;
            let head = rows[0].get(c).map(|s| s.trim()).unwrap_or("");
            if is_numeric_cell(head) {
                row0_numeric_in_those += 1;
            }
        }
    }
    // Typed like the data in most of the columns that have a type: not a
    // header. No numeric columns means the text gives no contrast at all,
    // and the presumption stands.
    if numeric_cols > 0 && row0_numeric_in_those * 2 > numeric_cols {
        0
    } else {
        1
    }
}

/// A cell that opens on a number, units and all: `17,000 xg`, `0-30`,
/// `4' on Center`, as well as every [`is_numeric_cell`]. A year is a
/// label, as there. A dash with a space after it is a bullet's, not a sign.
fn reads_as_quantity(s: &str) -> bool {
    if is_numeric_cell(s) {
        return true;
    }
    let digits = s.trim_start_matches([
        '$', '€', '£', '¥', '±', '~', '<', '>', '≤', '≥', '+', '-', '\u{2212}', '.',
    ]);
    digits.starts_with(|c: char| c.is_ascii_digit())
        && !(s.len() == 4 && s.parse::<u32>().is_ok_and(|y| (1900..2100).contains(&y)))
}

/// A cell that reads as a quantity: digits with currency/percent/grouping
/// dressing and nothing alphabetic. Bare 4-digit years are *not* numeric for
/// header purposes — `2023 | 2024` over money columns is a header row.
/// A row of figures past its label: every value cell with text holds
/// figures and their signs alone (`1000`, `0.25 – 21.4`, `±17.0`) or a
/// dash, one at least a figure.
fn figures_row(row: &[String]) -> bool {
    let figures = |t: &str| {
        t.chars().any(|c| c.is_ascii_digit())
            && t.chars().all(|c| {
                c.is_ascii_digit() || c.is_whitespace() || ".,%-−–—+±×/()$€£<>≤≥*".contains(c)
            })
    };
    let values: Vec<&str> = row
        .iter()
        .skip(1)
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    values.iter().any(|t| figures(t))
        && values
            .iter()
            .all(|t| figures(t) || t.chars().all(|c| "-–—".contains(c)))
}

/// Of a gap a column of text crossed by a page's gutter leaves, less than
/// this many ems between the last word before the gutter and the first after
/// is text running across it: a word space, or the wide space of a
/// justified line, is under three quarters of an em, and a gutter between
/// two columns of text over one.
const ACROSS_GUTTER: f64 = 0.75;

/// Within this many ems of a reading's top or bottom, a line of a page
/// column's text runs on past it: lines of a paragraph or rows of a table
/// stand a fifth of an em apart or less, ink to ink, and a caption or
/// heading set over a table stands off it by half an em or more.
const RUNS_PAST: f64 = 0.35;

/// Words to a line of a paragraph set in a page's column, at the least: a
/// column of a two-column page sets ten or more, a table cell's text two or
/// three before it wraps.
const COLUMN_PROSE_WORDS: usize = 8;

/// Shares of a reading's rows, and of those rows' baselines, that make its
/// two sides one table's halves: a long table wrapped into two halves side
/// by side holds a row on both sides on every line but perhaps its last.
const HALVES: f64 = 0.8;

/// Where a gutter between two of the page's columns could run down through
/// a reading: the gaps of three quarters of an em or more
/// (`ACROSS_GUTTER`) its rows leave before text that starts at one place,
/// as the span from where the text before them ends, at the latest, to
/// where that text starts; the places most rows start at first. A column
/// after a gutter starts each of its lines at the same place, however far
/// the text before the gutter reaches; a row with text on one side alone
/// leaves no gap there.
pub(crate) fn channels_in(rows: &[ReadingRow]) -> Vec<(f64, f64)> {
    let mut gaps: Vec<(f64, f64, f64)> = Vec::new();
    for row in rows.iter().filter(|r| r.len() >= 2) {
        let mut end = row[0].1.x1;
        for &(q, bb) in &row[1..] {
            let em = q.font_size.max(1.0) as f64;
            if bb.x0 - end >= em * ACROSS_GUTTER {
                gaps.push((bb.x0, end, em));
            }
            end = end.max(bb.x1);
        }
    }
    gaps.sort_by(|a, c| a.0.total_cmp(&c.0));
    // (rows, latest end before, earliest start after)
    let mut found: Vec<(usize, f64, f64)> = Vec::new();
    let mut last = f64::MIN;
    for (x, open, em) in gaps {
        match found.last_mut() {
            Some((n, ends, starts)) if x - last <= em * ACROSS_GUTTER => {
                *n += 1;
                *ends = ends.max(open);
                *starts = starts.min(x);
            }
            _ => found.push((1, open, x)),
        }
        last = x;
    }
    // The places most rows start at first.
    found.sort_by(|a, c| c.0.cmp(&a.0).then(a.2.total_cmp(&c.2)));
    found
        .into_iter()
        .filter(|(_, ends, starts)| starts > ends)
        .map(|(_, ends, starts)| (ends, starts))
        .collect()
}

/// A reading's row: its glyphs, left to right, with their boxes.
pub(crate) type ReadingRow<'g> = Vec<(&'g crate::glyph::Glyph, BBox)>;

/// A reading's rows of text, each its glyphs left to right: read once for
/// every place a gutter is looked for ([`channels_in`],
/// [`crosses_gutter`]).
pub(crate) fn reading_rows<'g>(g: &Grid, glyphs: &'g [crate::glyph::Glyph]) -> Vec<ReadingRow<'g>> {
    let b = g.bbox;
    let mut inside: Vec<(&crate::glyph::Glyph, BBox)> = glyphs
        .iter()
        .filter(|q| q.is_horizontal() && !q.text.trim().is_empty())
        .filter_map(|q| q.bbox.map(|bb| (q, bb)))
        .filter(|(_, bb)| {
            let (cx, cy) = ((bb.x0 + bb.x1) / 2.0, (bb.y0 + bb.y1) / 2.0);
            cx >= b.x0 && cx <= b.x1 && cy >= b.y0 && cy <= b.y1
        })
        .collect();
    inside.sort_by(|a, c| a.1.x0.total_cmp(&c.1.x0));
    g.ys.windows(2)
        .map(|w| {
            inside
                .iter()
                .filter(|(_, bb)| {
                    let cy = (bb.y0 + bb.y1) / 2.0;
                    cy >= w[0] && cy < w[1]
                })
                .copied()
                .collect()
        })
        .collect()
}

/// A run of text set together on a line: its box, its words, its size.
#[derive(Debug, Clone)]
pub(crate) struct TextRun {
    pub bbox: BBox,
    pub text: String,
    pub size: f64,
}

/// The lines a page column sets between `c0` and `c1`, from `top` to
/// `bottom`: its runs on one baseline, joined.
fn column_lines(runs: &[&TextRun], (c0, c1): (f64, f64), top: f64, bottom: f64) -> Vec<TextRun> {
    let mut side: Vec<&&TextRun> = runs
        .iter()
        .filter(|r| {
            r.bbox.x0 >= c0 - 2.0
                && r.bbox.x1 <= c1 + 2.0
                && r.bbox.y0 >= top
                && r.bbox.y1 <= bottom
        })
        .collect();
    side.sort_by(|a, b| {
        a.bbox
            .y1
            .total_cmp(&b.bbox.y1)
            .then(a.bbox.x0.total_cmp(&b.bbox.x0))
    });
    let mut out: Vec<TextRun> = Vec::new();
    for r in side {
        match out.last_mut() {
            Some(l) if (l.bbox.y1 - r.bbox.y1).abs() <= l.size.max(r.size) * 0.3 => {
                l.bbox = l.bbox.union(&r.bbox);
                l.text.push(' ');
                l.text.push_str(&r.text);
                l.size = l.size.max(r.size);
            }
            _ => out.push((*r).clone()),
        }
    }
    out
}

/// Whether some of `lines` run as a paragraph does: a line of a column's
/// prose, open at its end, carried on in lowercase by the line set under it
/// at its indent, both full lines of words (`COLUMN_PROSE_WORDS`).
fn runs_as_prose(lines: &[TextRun]) -> bool {
    lines.windows(2).any(|w| {
        let (a, c) = (&w[0], &w[1]);
        let em = a.size.max(1.0);
        let open = a
            .text
            .trim_end()
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_lowercase() || matches!(ch, ',' | '-'));
        let carried = c
            .text
            .trim_start()
            .chars()
            .next()
            .is_some_and(char::is_lowercase);
        let words = |l: &TextRun| l.text.split_whitespace().count() >= COLUMN_PROSE_WORDS;
        c.bbox.y0 - a.bbox.y1 <= em * RUNS_PAST
            && (c.bbox.x0 - a.bbox.x0).abs() <= em * 2.0
            && open
            && carried
            && words(a)
            && words(c)
    })
}

/// Whether a reading set across a page's gutter, between the page columns
/// `left` and `right`, crosses between two of the page's columns of text:
/// two tables set side by side, a table and the prose beside it, a block of
/// facts and a title.
///
/// - On none of its rows does text run across the gutter, and on most rows
///   the first words past it are a label, not a figure. A table's columns
///   after its labels hold figures; a second column of labels past the
///   page's gutter is another table's stub, the keys of a block of facts, or
///   prose.
/// - Its two sides are not one table's halves: a long table wrapped into
///   two halves side by side sets a row on both sides on one baseline, and
///   neither half runs on past the other.
///
/// `rows` are the reading's ([`reading_rows`]); `runs` gives the page's runs
/// of text, read only for a reading that passes the first test. The gutter
/// is looked for in the span its rows leave open between `ends` and
/// `starts` ([`channels_in`]), where fewest runs cross it; the place found
/// is returned.
pub(crate) fn crosses_gutter<'a>(
    g: &Grid,
    rows: &[ReadingRow],
    runs: impl Fn() -> &'a [TextRun],
    (ends, starts): (f64, f64),
    width: f64,
) -> Option<f64> {
    let mut gutter = (ends + starts) / 2.0;
    let b = g.bbox;
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let mut firsts: Vec<String> = Vec::new();
    let (mut held, mut both, mut level) = (0usize, 0usize, 0usize);
    let mut fs = 0.0f64;
    for row in rows {
        if row.is_empty() {
            continue;
        }
        held += 1;
        let split = row.partition_point(|(_, bb)| (bb.x0 + bb.x1) / 2.0 < gutter);
        let (before, after) = row.split_at(split);
        let Some(&(first, fb)) = after.first() else {
            continue;
        };
        let em = first.font_size.max(1.0) as f64;
        fs = fs.max(em);
        if let Some(&(_, lb)) = before.last() {
            if fb.x0 - lb.x1 < em * ACROSS_GUTTER {
                return None;
            }
            both += 1;
            let mut l: Vec<f64> = before.iter().map(|(q, _)| q.origin.1).collect();
            let mut r: Vec<f64> = after.iter().map(|(q, _)| q.origin.1).collect();
            if (median(&mut l) - median(&mut r)).abs() <= em * 0.25 {
                level += 1;
            }
        }
        let mut text = first.text.clone();
        let mut end = fb.x1;
        for &(q, qb) in &after[1..] {
            if qb.x0 - end > em * ACROSS_GUTTER {
                break;
            }
            text.push_str(&q.text);
            end = qb.x1;
        }
        firsts.push(text);
    }
    // A label: words, or a key set against its value (`Z = 2`).
    let labels = firsts
        .iter()
        .filter(|t| {
            let t = t.trim();
            let keyed = t
                .split_once('=')
                .is_some_and(|(k, _)| k.chars().any(char::is_alphabetic));
            keyed || t.chars().filter(|c| c.is_alphabetic()).count() >= 2 && !is_numeric_cell(t)
        })
        .count();
    if both == 0 || firsts.len() < 2 || labels * 2 <= firsts.len() {
        return None;
    }
    // The page's runs of text outside the reading, read only for a reading
    // that passes the tests above.
    let runs: Vec<&TextRun> = runs()
        .iter()
        .filter(|r| {
            let (cx, cy) = ((r.bbox.x0 + r.bbox.x1) / 2.0, (r.bbox.y0 + r.bbox.y1) / 2.0);
            !(cx >= b.x0 && cx <= b.x1 && cy >= b.y0 && cy <= b.y1)
        })
        .collect();
    // The gutter stands where fewest of the page's runs cross the span its
    // rows leave open: a column's prose starts at its edge, and a table set
    // in it may start its labels further in.
    let crossing = |x: f64| {
        runs.iter()
            .filter(|r| r.bbox.x0 < x - 0.5 && r.bbox.x1 > x + 0.5)
            .count()
    };
    let mut places: Vec<f64> = vec![gutter, ends + 0.5, starts - 0.5];
    places.extend(
        runs.iter()
            .flat_map(|r| [r.bbox.x0 - 0.5, r.bbox.x1 + 0.5])
            .filter(|&x| x > ends && x < starts),
    );
    gutter = places
        .into_iter()
        .map(|x| (crossing(x), (x - gutter).abs(), x))
        .min_by(|a, c| a.0.cmp(&c.0).then(a.1.total_cmp(&c.1)))
        .map_or(gutter, |(_, _, x)| x);
    let (left, right) = ((0.0, gutter), (gutter, width));
    // The gutter parts two columns of the page's text: between the nearest
    // runs set across it over and under the reading, each side sets a
    // paragraph of its own, a line running on into the next. A gap between
    // a table's own columns parts nothing but the table: the page's prose
    // runs across it, or the page holds none.
    let across = |r: &TextRun| r.bbox.x0 < gutter - 1.0 && r.bbox.x1 > gutter + 1.0;
    let top = runs
        .iter()
        .filter(|r| across(r) && r.bbox.y1 <= b.y0)
        .map(|r| r.bbox.y1)
        .fold(f64::MIN, f64::max);
    let bottom = runs
        .iter()
        .filter(|r| across(r) && r.bbox.y0 >= b.y1)
        .map(|r| r.bbox.y0)
        .fold(f64::MAX, f64::min);
    if !runs_as_prose(&column_lines(&runs, left, top, bottom))
        || !runs_as_prose(&column_lines(&runs, right, top, bottom))
    {
        return None;
    }
    // A side's column text running on past the reading's top or bottom.
    let runs_past = |(c0, c1): (f64, f64), top: bool| {
        runs.iter().any(|r| {
            let rb = r.bbox;
            let gap = if top { b.y0 - rb.y1 } else { rb.y0 - b.y1 };
            rb.x0 >= c0 - 2.0
                && rb.x1 <= c1 + 2.0
                && gap >= -0.5 * fs
                && gap <= RUNS_PAST * r.size.max(1.0)
        })
    };
    let alone = [true, false]
        .iter()
        .any(|&top| runs_past(left, top) != runs_past(right, top));
    let halves = both as f64 >= held as f64 * HALVES && level as f64 >= both as f64 * HALVES;
    (alone || !halves).then_some(gutter)
}

/// Text that stands as a label of its own, not one carrying on the label
/// over it: `100%` or `LM` under a header's head, where `size` under
/// `Frame`, `(7.2K rpm)` under `Business Critical` or `x = E/U` under a
/// type's name go on with it.
pub(crate) fn opens_a_label(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && !t.starts_with(|c: char| c.is_lowercase() || c == '(' || c == '[')
}

pub(crate) fn is_numeric_cell(s: &str) -> bool {
    if s.is_empty() || !s.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    if s.len() == 4 && s.parse::<u32>().is_ok_and(|y| (1900..2100).contains(&y)) {
        return false;
    }
    s.chars().all(|c| {
        c.is_ascii_digit()
            || matches!(
                c,
                '$' | '€' | '£' | '¥' | '%' | ',' | '.' | '(' | ')' | '-' | '+' | ' ' | '\u{2212}'
            )
    })
}

/// One grid line: rules sharing a perpendicular-axis position, with the span
/// they collectively cover.
#[derive(Clone)]
struct GridLine {
    axis: f64,
    lo: f64,
    hi: f64,
    /// Endpoints of the individual segments — these carry the cross-axis
    /// boundaries, which is why segmented rules are more informative than
    /// continuous ones.
    ends: Vec<f64>,
    /// Along a row's rule drawn in pieces, the middles of the gaps between
    /// them narrower than a cell: the gutters a statement's underlines part
    /// at, one piece under each column of figures.
    gutters: Vec<f64>,
    /// Where such a rule's run of pieces starts and ends: the first column
    /// of figures' left edge and the last one's right, the labels' column
    /// standing before them unruled.
    gutter_ends: Vec<f64>,
}

/// Cluster rules of one orientation into grid lines by axis position.
fn cluster(rules: &[&Rule]) -> Vec<GridLine> {
    let mut sorted: Vec<&&Rule> = rules.iter().collect();
    sorted.sort_by(|a, b| {
        a.axis_pos()
            .partial_cmp(&b.axis_pos())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut out: Vec<GridLine> = Vec::new();
    let mut pieces: Vec<Vec<(f64, f64)>> = Vec::new();
    for r in sorted {
        let (lo, hi) = match r.orientation {
            Orientation::Horizontal => (r.bbox.x0, r.bbox.x1),
            Orientation::Vertical => (r.bbox.y0, r.bbox.y1),
        };
        match out.last_mut() {
            Some(g) if (g.axis - r.axis_pos()).abs() <= AXIS_TOLERANCE => {
                g.lo = g.lo.min(lo);
                g.hi = g.hi.max(hi);
                g.ends.push(lo);
                g.ends.push(hi);
                pieces.last_mut().unwrap().push((lo, hi));
            }
            _ => {
                out.push(GridLine {
                    axis: r.axis_pos(),
                    lo,
                    hi,
                    ends: vec![lo, hi],
                    gutters: Vec::new(),
                    gutter_ends: Vec::new(),
                });
                pieces.push(vec![(lo, hi)]);
            }
        }
    }
    // A row's rule drawn a cell at a time, each cell's piece run on past its
    // edge into the next, overlaps its neighbours: the two ends inside an
    // overlap are one column's edge, at its middle, not two edges a few
    // points or a column's padding apart (an empty column between every
    // pair). Pieces that meet end to end keep their ends, and so do pieces a
    // longer rule is drawn over whole (group underlines under a rule across
    // all of them).
    // Only along a row's rule, where the ends are columns' edges: a column's
    // rule drawn in overlapping pieces still parts its rows where it breaks.
    let horizontal = rules
        .first()
        .is_some_and(|r| r.orientation == Orientation::Horizontal);
    let t = EDGE_TOLERANCE;
    for (g, parts) in out.iter_mut().zip(&pieces).filter(|_| horizontal) {
        g.ends.retain(end_kept(parts, t));
        g.ends.extend(overlap_middles(parts, t));
        // The rule's runs: its pieces joined where they meet or overlap.
        let mut sorted = parts.clone();
        sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut runs: Vec<(f64, f64)> = Vec::new();
        for (a, b) in sorted {
            match runs.last_mut() {
                Some(r) if a <= r.1 + 0.5 => r.1 = r.1.max(b),
                _ => runs.push((a, b)),
            }
        }
        // A gutter parts two underlines, each under a column's figures and
        // as wide as a cell can be. A dashed line's dashes, a chart's
        // curve drawn a point at a time and its axis's ticks are narrower.
        let wide = |r: &(f64, f64)| r.1 - r.0 >= MIN_CELL_EXTENT;
        let parted: Vec<usize> = (1..runs.len())
            .filter(|&i| {
                let gap = runs[i].0 - runs[i - 1].1;
                gap <= MAX_GUTTER && wide(&runs[i - 1]) && wide(&runs[i])
            })
            .collect();
        // A row's underlines under each of its figures: three gutters or
        // more. Group labels' underlines, one under each group, part at a
        // gutter or two, and between groups, not columns. Counted along one
        // chain of pieces: a gap wider than a gutter parts two tables'
        // rules (each of two tables side by side ruled a cell at a time,
        // their rows level across the page's gutter), and their gutters
        // are not one row's.
        let mut chains: Vec<Vec<usize>> = Vec::new();
        for &i in &parted {
            match chains.last_mut() {
                Some(c) if c.last().is_some_and(|&j| j + 1 == i) => c.push(i),
                _ => chains.push(vec![i]),
            }
        }
        let parted: Vec<usize> = chains
            .into_iter()
            .filter(|c| c.len() >= MIN_GUTTERS_PER_LINE)
            .flatten()
            .collect();
        if parted.len() >= MIN_GUTTERS_PER_LINE {
            g.gutters = parted
                .iter()
                .map(|&i| (runs[i - 1].1 + runs[i].0) / 2.0)
                .collect();
            let first = runs.iter().find(|r| wide(r)).map_or(g.lo, |r| r.0);
            let last = runs.iter().rev().find(|r| wide(r)).map_or(g.hi, |r| r.1);
            g.gutter_ends = vec![first, last];
        }
    }
    out
}

/// The middles of the overlaps between a row rule's pieces, in the order of
/// the pairs (first piece, second piece) as the pieces were drawn: one for
/// each pair overlapping by more than `t` where neither piece holds the
/// other.
///
/// A chart drawn in strokes can lay thousands of pieces along one line, most
/// of them stacked over each other. So the pairs are found by a sweep over
/// the pieces sorted by start, testing only the pieces that start inside
/// another's span. A pair overlaps by more than `t` only if the later piece
/// starts more than `t` before the earlier one ends, and that bound only
/// falls as the start moves right.
fn overlap_middles(parts: &[(f64, f64)], t: f64) -> Vec<f64> {
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by(|&i, &j| parts[i].0.total_cmp(&parts[j].0).then(i.cmp(&j)));
    let mut found: Vec<(usize, usize, f64)> = Vec::new();
    for (p, &i) in order.iter().enumerate() {
        let end = parts[i].1;
        for &j in &order[p + 1..] {
            if end - parts[j].0 <= t {
                break;
            }
            let (first, second) = (i.min(j), i.max(j));
            let ((a, b), (c, d)) = (parts[first], parts[second]);
            let nested = (c <= a + t && d >= b - t) || (a <= c + t && b >= d - t);
            let (lo, hi) = (a.max(c), b.min(d));
            if hi - lo > t && !nested {
                found.push((first, second, (lo + hi) / 2.0));
            }
        }
    }
    found.sort_by_key(|&(i, j, _)| (i, j));
    found.into_iter().map(|(_, _, m)| m).collect()
}

/// Whether a row rule keeps an end of one of its pieces: not when the end
/// falls inside another piece that overlaps the end's own piece without
/// holding it (two cells' pieces run on past each other's edge).
///
/// Many pieces of a chart drawn in strokes repeat one another and share
/// their ends, so past [`FEW_PIECES`] the test reads the distinct pieces,
/// sorted by start, and is worked out once for each distinct end.
fn end_kept(parts: &[(f64, f64)], t: f64) -> impl FnMut(&f64) -> bool + '_ {
    let few = parts.len() <= FEW_PIECES;
    let mut distinct = if few { Vec::new() } else { parts.to_vec() };
    distinct.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.total_cmp(&y.1)));
    distinct.dedup_by(|x, y| x.0.to_bits() == y.0.to_bits() && x.1.to_bits() == y.1.to_bits());
    let mut kept: rustc_hash::FxHashMap<u64, bool> = Default::default();
    let ends_inside = move |pieces: &[(f64, f64)], e: f64, sorted: bool| {
        let mut own = pieces
            .iter()
            .filter(|(a, b)| (a - e).abs() <= 0.01 || (b - e).abs() <= 0.01);
        !own.any(|&(a, b)| {
            pieces
                .iter()
                .take_while(|&&(c, _)| !sorted || c < e - t)
                .any(|&(c, d)| {
                    let contains = c <= a + t && d >= b - t;
                    let overlaps = c < b - t && d > a + t;
                    overlaps && !contains && c < e - t && d > e + t
                })
        })
    };
    move |&e| {
        if few {
            return ends_inside(parts, e, false);
        }
        *kept
            .entry(e.to_bits())
            .or_insert_with(|| ends_inside(&distinct, e, true))
    }
}

/// How many pieces a rule, or rules a component, holds before the readings
/// of their overlaps file them first: below it, testing each against each
/// costs less than the filing.
const FEW_PIECES: usize = 32;

/// Drop a rule's gutters when text runs across one of them: a caption or a
/// line of prose over a rule whose pieces happen to part under it, or a
/// row of cells drawn a cell at a time under a title. A row of underlines
/// under figures has a column's gap at every gutter; one gutter run across
/// shows the pieces are not under columns, and none of the rule's gutters
/// is kept. Only the text on the rule's own row counts, a baseline within
/// [`GUTTER_ROW_EMS`] of it, so a head set over two columns a line above
/// their own underlines does not close the gutter between them. Text runs
/// across a gutter when a glyph spans it, or two glyphs on one baseline
/// either side of it stand a word space or less apart
/// ([`GUTTER_WORD_GAP_EMS`]).
fn clear_crossed_gutters(lines: &mut [GridLine], glyphs: &[crate::glyph::Glyph]) {
    if lines.iter().all(|g| g.gutters.is_empty()) {
        return;
    }
    // The inked glyphs by baseline, (baseline, x0, x1, size), so each rule
    // reads only the rows near it.
    let mut inked: Vec<(f64, f64, f64, f64)> = glyphs
        .iter()
        .filter(|c| c.is_horizontal() && !c.text.trim().is_empty())
        .filter_map(|c| {
            let b = c.bbox?;
            Some((c.origin.1, b.x0, b.x1, (c.font_size as f64).max(1.0)))
        })
        .collect();
    inked.sort_by(|a, b| a.0.total_cmp(&b.0));
    let largest = inked.iter().map(|c| c.3).fold(1.0, f64::max);
    for g in lines.iter_mut().filter(|g| !g.gutters.is_empty()) {
        let reach = GUTTER_ROW_EMS * largest;
        let from = inked.partition_point(|c| c.0 < g.axis - reach);
        let row: Vec<(f64, f64, f64, f64)> = inked[from..]
            .iter()
            .take_while(|c| c.0 <= g.axis + reach)
            .filter(|&&(y, x0, x1, fs)| {
                (y - g.axis).abs() <= GUTTER_ROW_EMS * fs && x1 > g.lo - fs && x0 < g.hi + fs
            })
            .map(|&(y, x0, x1, fs)| (x0, x1, y, fs))
            .collect();
        let crossed = |x: f64| {
            row.iter().any(|&(a0, a1, ay, fs)| {
                (a0 < x - 0.5 && a1 > x + 0.5)
                    || (a1 <= x + 0.5
                        && row.iter().any(|&(b0, _, by, _)| {
                            b0 >= x - 0.5
                                && (by - ay).abs() < 0.3 * fs
                                && b0 - a1 <= GUTTER_WORD_GAP_EMS * fs
                        }))
            })
        };
        if g.gutters.iter().any(|&x| crossed(x)) {
            g.gutters.clear();
            g.gutter_ends.clear();
        }
        // A label run on over where the figures' underlines begin is the
        // label's, and that end is no column's edge; their gutters still are.
        g.gutter_ends.retain(|&x| !crossed(x));
    }
}

/// A grid ruled by its figures' underlines: one of its column edges is a
/// gutter the pieces of two of its row rules or more part at, with no text
/// run across it ([`cluster`], [`clear_crossed_gutters`]), and those rules
/// start after the rows' labels: after the grid's first column, the labels'
/// column left unruled, or after labels set left of the grid on two of the
/// lines its figures are on, a grid of the figures' columns alone. Its rules
/// close groups of rows, not rows ([`crate::textgrid::split_underlined_bands`]).
/// A row of cells drawn a cell at a time is ruled from the table's edge,
/// its labels' cell and all, and each band it rules is a row.
pub(crate) fn ruled_by_underlines(
    g: &Grid,
    rules: &[Rule],
    glyphs: &[crate::glyph::Glyph],
) -> bool {
    let (Some(&x0), Some(&x1)) = (g.xs.first(), g.xs.last()) else {
        return false;
    };
    let (Some(&y0), Some(&y1)) = (g.ys.first(), g.ys.last()) else {
        return false;
    };
    let hs: Vec<&Rule> = rules
        .iter()
        .filter(|r| {
            r.orientation == Orientation::Horizontal
                && r.axis_pos() >= y0 - EDGE_TOLERANCE
                && r.axis_pos() <= y1 + EDGE_TOLERANCE
                && r.bbox.x1 > x0
                && r.bbox.x0 < x1
        })
        .collect();
    let mut lines = cluster(&hs);
    if lines.iter().all(|l| l.gutters.is_empty()) {
        return false;
    }
    clear_crossed_gutters(&mut lines, glyphs);
    // The baselines of the grid's figures, and of its rows' labels set left
    // of it: a label's line carries its row's figures.
    let on_rows = |c: &&crate::glyph::Glyph| {
        c.is_horizontal() && c.bbox.is_some() && c.origin.1 > y0 && c.origin.1 < y1
    };
    let figures: Vec<(f64, f64)> = glyphs
        .iter()
        .filter(on_rows)
        .filter(|c| {
            let b = c.bbox.unwrap_or_default();
            let m = (b.x0 + b.x1) / 2.0;
            m > x0 && m < x1 && c.text.chars().any(|ch| ch.is_ascii_digit())
        })
        .map(|c| (c.origin.1, (c.font_size as f64).max(1.0)))
        .collect();
    let labelled_before = |lo: f64| {
        let mut bases: Vec<f64> = glyphs
            .iter()
            .filter(on_rows)
            .filter(|c| {
                c.text.chars().any(char::is_alphabetic)
                    && c.bbox.is_some_and(|b| b.x1 <= lo)
                    && figures
                        .iter()
                        .any(|&(y, fs)| (y - c.origin.1).abs() < 0.3 * fs)
            })
            .map(|c| c.origin.1)
            .collect();
        bases.sort_by(f64::total_cmp);
        bases.dedup_by(|a, b| (*a - *b).abs() < 1.0);
        bases.len() >= 2
    };
    g.xs[1..g.xs.len() - 1].iter().any(|&x| {
        lines
            .iter()
            .filter(|l| {
                l.gutters.iter().any(|e| (e - x).abs() <= EDGE_TOLERANCE)
                    && (l.lo > x0 + EDGE_TOLERANCE || labelled_before(l.lo))
            })
            .count()
            >= MIN_GUTTER_LINES
    })
}

/// How near a rule, in the text's ems, a baseline stands to be on the
/// rule's row: an underline sits a fraction of an em under its text, and
/// a row's first line under a rule stands its cap height and padding
/// below it. The line above an underlined one stands a line's leading,
/// 1.2 em or more, above that line's own underline.
const GUTTER_ROW_EMS: f64 = 1.0;

/// The widest gap, in ems, between two glyphs of one run of text: a word
/// space is a quarter to a third of an em, a justified line's stretch
/// up to about twice that. Columns of figures stand further apart.
const GUTTER_WORD_GAP_EMS: f64 = 0.6;

/// Widest gap between two pieces of a row's rule that is a gutter between
/// columns: narrower than a cell can be. A wider gap leaves columns
/// unruled, an underline under some columns only.
const MAX_GUTTER: f64 = MIN_CELL_EXTENT * 1.5;

/// Fewest row rules a gutter between their pieces must recur on to be a
/// column's edge: one rule parted once is a broken line.
const MIN_GUTTER_LINES: usize = 2;

/// Fewest gutters a row's rule parts at for its gutters to be columns':
/// underlines under each figure of a row, four columns or more.
const MIN_GUTTERS_PER_LINE: usize = 3;

/// Collapse near-equal positions into one, taking the mean of each group.
fn coalesce(mut vals: Vec<f64>, tol: f64) -> Vec<f64> {
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<f64> = Vec::new();
    let mut group: Vec<f64> = Vec::new();
    for v in vals {
        if group.is_empty() || (v - group[group.len() - 1]).abs() <= tol {
            group.push(v);
        } else {
            out.push(group.iter().sum::<f64>() / group.len() as f64);
            group = vec![v];
        }
    }
    if !group.is_empty() {
        out.push(group.iter().sum::<f64>() / group.len() as f64);
    }
    out
}

impl Grid {
    /// Drop empty rows and columns at the edges of the grid.
    ///
    /// A page's ruling lines do not stop at the table: a footer separator far
    /// below still clusters as a horizontal grid line, so the grid reaches past
    /// the table and picks up an empty row plus the page furniture. Trimming to
    /// the outermost rows and columns that actually hold content bounds the
    /// table to its real extent.
    pub fn trim_to_content(&mut self, glyphs: &[crate::glyph::Glyph]) {
        self.trim_to_content_ruled(glyphs, &[]);
    }

    /// [`Grid::trim_to_content`], reading the page's `rules` for blank rows
    /// set inside the table.
    ///
    /// A table may set a blank row between its groups of rows, ruled as
    /// every other row is: a parameter table's spacer before each
    /// instrument's settings, or before its last field. Ended there, the
    /// table lost the rows under it, read as paragraphs or as a table of
    /// their own. A blank row is the table's own where every column edge
    /// the grid draws runs through it, as it runs through the rows on both
    /// sides of it, and the rows under it hold text. A footer's separator
    /// below a table is joined to it by no rule.
    pub fn trim_to_content_ruled(&mut self, glyphs: &[crate::glyph::Glyph], rules: &[Rule]) {
        let (rows, cols) = (self.rows(), self.cols());
        if rows == 0 || cols == 0 {
            return;
        }
        let cells = self.cell_texts(glyphs);
        let row_used: Vec<bool> = (0..rows)
            .map(|r| (0..cols).any(|c| !cells[r * cols + c].trim().is_empty()))
            .collect();
        let col_used: Vec<bool> = (0..cols)
            .map(|c| (0..rows).any(|r| !cells[r * cols + c].trim().is_empty()))
            .collect();

        let r0 = row_used.iter().position(|x| *x).unwrap_or(0);
        // The table ends at the first empty row after its content, not at the
        // last populated row anywhere in the grid. Ruling lines below a table —
        // a footer separator — extend the grid, and taking the last populated
        // row would pull the page footer in as a final table row. A blank row
        // the table rules as its own is no end.
        let r1 = self.last_row_through_spacers(&row_used, r0, rules, glyphs);
        let c0 = col_used.iter().position(|x| *x).unwrap_or(0);
        let c1 = col_used.iter().rposition(|x| *x).unwrap_or(cols - 1);

        self.ys = self.ys[r0..=r1 + 1].to_vec();
        self.xs = self.xs[c0..=c1 + 1].to_vec();
        self.bbox = BBox {
            x0: self.xs[0],
            y0: self.ys[0],
            x1: self.xs[self.xs.len() - 1],
            y1: self.ys[self.ys.len() - 1],
        };
    }

    /// The last row of the rows holding text from `r0` on, read on past
    /// each run of blank rows the table rules as its own: two columns or
    /// more, every column edge drawn down through the blank rows and
    /// through the rows with text either side of them. See
    /// [`Grid::trim_to_content_ruled`].
    ///
    /// The rows taken in past a blank one must each hold their text as a
    /// table's cells do ([`Grid::row_holds_text`]). A chart's gridlines
    /// rule every band of the plot, blank or not, and run on down through
    /// its legend; the legend's words straddle the gridlines.
    fn last_row_through_spacers(
        &self,
        used: &[bool],
        r0: usize,
        rules: &[Rule],
        glyphs: &[crate::glyph::Glyph],
    ) -> usize {
        let rows = used.len();
        let first = (r0..rows).take_while(|&r| used[r]).last().unwrap_or(r0);
        let drawn = |r: usize| {
            let mid = (self.ys[r] + self.ys[r + 1]) / 2.0;
            self.cols() >= 2
                && self.xs.iter().all(|&x| {
                    rules.iter().any(|rule| {
                        rule.orientation == Orientation::Vertical
                            && (rule.axis_pos() - x).abs() <= EDGE_TOLERANCE
                            && rule.bbox.y0 <= mid
                            && rule.bbox.y1 >= mid
                    })
                })
        };
        let mut last = first;
        let mut r = first + 1;
        while r < rows {
            if used[r] {
                last = r;
                r += 1;
                continue;
            }
            let mut e = r;
            while e < rows && !used[e] && drawn(e) {
                e += 1;
            }
            if e == r || e == rows || !used[e] || !drawn(last) || !drawn(e) {
                break;
            }
            r = e;
        }
        // A table has begun over the blank row, two rows of it, and goes on
        // under it in a row of two cells or more: one line over a blank
        // band is as often a chart's labels over its plot, and a column of
        // single values under one its legend. Every row taken in holds its
        // text as a cell does.
        let filled = |r: usize| {
            let (top, bottom) = (self.ys[r], self.ys[r + 1]);
            self.xs
                .windows(2)
                .filter(|w| {
                    glyphs.iter().any(|g| {
                        !g.text.trim().is_empty() && {
                            let (x, y) = g.center();
                            x > w[0] && x < w[1] && y > top && y < bottom
                        }
                    })
                })
                .count()
        };
        let taken: Vec<usize> = (first + 1..=last).filter(|&r| used[r]).collect();
        if last > first
            && first > r0
            && taken.iter().any(|&r| filled(r) >= 2)
            && taken.iter().all(|&r| self.row_holds_text(glyphs, r))
        {
            last
        } else {
            first
        }
    }

    /// Row `r` holds its text as a table's cells do: every glyph whose
    /// middle lies in the row lies within it, and no line of text runs on
    /// across a column edge, its glyphs either side of the edge set closer
    /// than [`CELL_GAP_EMS`].
    fn row_holds_text(&self, glyphs: &[crate::glyph::Glyph], r: usize) -> bool {
        let (top, bottom) = (self.ys[r], self.ys[r + 1]);
        let (left, right) = (self.xs[0], self.xs[self.xs.len() - 1]);
        let inner = &self.xs[1..self.xs.len() - 1];
        let mut row: Vec<(BBox, f64)> = glyphs
            .iter()
            .filter(|g| g.is_horizontal() && !g.text.trim().is_empty())
            .filter_map(|g| Some((g.bbox?, g.font_size.max(1.0) as f64)))
            .filter(|(b, _)| {
                let (x, y) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
                y > top && y < bottom && x > left && x < right
            })
            .collect();
        // A glyph's ink may reach a little past the rule over or under it.
        let outside = |b: &BBox| {
            let slack = 0.2 * (b.y1 - b.y0);
            b.y0 < top - slack || b.y1 > bottom + slack
        };
        if row.iter().any(|(b, _)| outside(b)) {
            return false;
        }
        row.sort_by(|a, b| a.0.x0.total_cmp(&b.0.x0));
        !inner.iter().any(|&x| {
            row.iter().any(|(a, size)| {
                (a.x0 < x - 0.5 && a.x1 > x + 0.5)
                    || row.iter().any(|(b, _)| {
                        let same_line = (a.y1.min(b.y1) - a.y0.max(b.y0)) > 0.5 * (a.y1 - a.y0);
                        (a.x0 + a.x1) / 2.0 < x
                            && (b.x0 + b.x1) / 2.0 > x
                            && same_line
                            && b.x0 - a.x1 < CELL_GAP_EMS * size
                    })
            })
        })
    }

    /// The column groups ruled along row boundary `k`: each piece of rule
    /// drawn there, as the columns it runs under, where the pieces group
    /// the columns. That is two pieces or more, parted where the groups
    /// part, or one that leaves a column unruled, and one of them at least
    /// runs under two columns. A boundary ruled across the whole table,
    /// ruled column by column, or not ruled at all groups nothing.
    fn column_groups(&self, rules: &[Rule], k: usize) -> Option<Vec<ColumnGroup>> {
        let cols = self.cols();
        let (left, right) = (*self.xs.first()?, *self.xs.last()?);
        let y = *self.ys.get(k)?;
        let mut runs: Vec<(f64, f64)> = rules
            .iter()
            .filter(|r| {
                r.orientation == Orientation::Horizontal
                    && (r.bbox.y0 - y).abs() <= EDGE_TOLERANCE * 2.0
            })
            .map(|r| (r.bbox.x0.max(left), r.bbox.x1.min(right)))
            .filter(|(a, b)| b > a)
            .collect();
        runs.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Segments that meet are one piece: a rule drawn cell by cell.
        let mut pieces: Vec<(f64, f64)> = Vec::new();
        for (a, b) in runs {
            match pieces.last_mut() {
                Some(p) if a <= p.1 + RULE_JOIN => p.1 = p.1.max(b),
                _ => pieces.push((a, b)),
            }
        }
        let groups: Vec<ColumnGroup> = pieces
            .iter()
            .filter_map(|&(x0, x1)| {
                let under: Vec<usize> = (0..cols)
                    .filter(|&c| {
                        let (a, b) = (self.xs[c], self.xs[c + 1]);
                        x1.min(b) - x0.max(a) >= 0.6 * (b - a)
                    })
                    .collect();
                Some(ColumnGroup {
                    cols: *under.first()?..*under.last()? + 1,
                    x0,
                    x1,
                })
            })
            .collect();
        let ruled: usize = groups.iter().map(|g| g.cols.len()).sum();
        let grouped = groups.len() >= 2 || (groups.len() == 1 && ruled < cols);
        (grouped && groups.iter().any(|g| g.cols.len() >= 2)).then_some(groups)
    }

    /// Take in the line of column-group labels set over the grid's top rule.
    ///
    /// A table that groups its columns (`2024`, `2023` and `Percent
    /// Change`, each over `Underground | Surface | Total`) sets each group's
    /// label over a rule drawn under that group alone. Ruled with
    /// horizontals only, its grid begins at those rules, so the labels over
    /// them were left outside it, read as paragraphs, and the columns lost
    /// the level that tells them apart. The line over the rules is the
    /// grid's top row when it sits within a line of them, every mark on it
    /// across the table's width stands over one of the rules, each group's
    /// part of it is one run of text, and every rule has a label over it.
    /// Lines a label wraps onto above come with it. A line set as close that
    /// also stands over an unruled column, or across two groups, says the
    /// header is more than a line of group labels, and none of it is taken:
    /// part of it would be a fragment. A line further off is the table's
    /// title or a caption, and stays outside.
    pub fn take_group_labels(&mut self, rules: &[Rule], glyphs: &[crate::glyph::Glyph]) {
        // A table that draws its columns bounds its header's cells itself.
        let (left, right, top) = (self.xs[0], self.xs[self.cols()], self.ys[0]);
        if rules.iter().any(|r| {
            r.orientation == Orientation::Vertical
                && r.bbox.x1 > left
                && r.bbox.x0 < right
                && r.bbox.y1 > top - 2.0 * EDGE_TOLERANCE
                && r.bbox.y0 < self.bbox.y1
        }) {
            return;
        }
        let Some(groups) = self.column_groups(rules, 0) else {
            return;
        };
        let mut over: Vec<(f64, f64, BBox)> = glyphs
            .iter()
            .filter(|g| g.is_horizontal() && !g.text.trim().is_empty())
            .filter_map(|g| Some((g.origin.1, g.font_size.max(1.0) as f64, g.bbox?)))
            .filter(|(_, _, b)| b.x1 > left && b.x0 < right && b.y1 <= top + 1.0)
            .collect();
        // Nearest first, then a line at a time by baseline.
        over.sort_by(|a, b| b.0.total_cmp(&a.0));
        let within = |b: &BBox, g: &ColumnGroup| b.x0 >= g.x0 - 1.0 && b.x1 <= g.x1 + 1.0;
        let mut taken: Vec<BBox> = Vec::new();
        let mut below = top;
        let mut i = 0;
        while i < over.len() {
            let (base, fs, _) = over[i];
            let end = (i..over.len())
                .find(|&k| base - over[k].0 > 0.3 * fs)
                .unwrap_or(over.len());
            let line: Vec<BBox> = over[i..end].iter().map(|o| o.2).collect();
            // On the rules, or a label's line wrapped above the one below.
            let reach = if taken.is_empty() { 1.0 } else { 1.6 };
            if below - base > reach * fs {
                break;
            }
            let over_groups = line.iter().all(|b| groups.iter().any(|g| within(b, g)));
            let one_run = groups.iter().all(|g| {
                let mut xs: Vec<(f64, f64)> = line
                    .iter()
                    .filter(|b| within(b, g))
                    .map(|b| (b.x0, b.x1))
                    .collect();
                xs.sort_by(|a, b| a.0.total_cmp(&b.0));
                xs.windows(2).all(|w| w[1].0 - w[0].1 <= fs)
            });
            // A line set as close that is not labels over the groups (a
            // label over the row labels, a box's wrapped text) says the
            // header above is more than one line of group labels, and the
            // part of it taken would be a fragment.
            if !(over_groups && one_run) {
                return;
            }
            taken.extend(line);
            below = base;
            i = end;
        }
        let labelled = groups.iter().all(|g| taken.iter().any(|b| within(b, g)));
        if taken.is_empty() || !labelled {
            return;
        }
        let y0 = taken.iter().map(|b| b.y0).fold(f64::MAX, f64::min) - 1.0;
        self.ys.insert(0, y0);
        self.bbox.y0 = y0;
    }

    /// Take in the column heads set over the grid's top rule, the table open
    /// above them.
    ///
    /// A table ruled under its header and not over it (an exposure-limit
    /// table's `OSHA` over `TWA`, `NIOSH` over `STEL`, each over its column,
    /// a heavy rule under them) begins its grid at that rule: the heads
    /// were left outside it, read as text, the bold ones as headings, and
    /// no value was named. The lines over the rule are the grid's top row
    /// when the nearest sits within a line of it and each line reads as a
    /// row of heads: every run of text on it stands within one column, one
    /// run to a column, inside the table's width, in two columns or more.
    /// A line further up is taken while it is set as close as a head's
    /// wrapped lines are and stands in the columns alike. One run alone
    /// there is a head's wrapped line over a column past the first; over
    /// the first, only where it starts where the head under it does, in its
    /// size and weight (`Supp. Figure 3I: Levels of α-KG` over `with WT or
    /// mutant IDH1`, beside `Mean | SD | N`): a table's title set close over
    /// its stub's head is set apart from it.
    ///
    /// What stops the header, and is left outside:
    ///
    /// - a line across a column's edge or past the table: a title, a
    ///   caption or prose;
    /// - two runs in one column: the heads part where the grid does not,
    ///   and it is no grid of theirs (two tables side by side read as one,
    ///   a statement read with half its columns);
    /// - a row of data: a label in the first column with figures beside it,
    ///   a year apart (`South East | 57,384 | 37,525`), or a field, its
    ///   label ending in a colon (`APPLICABILITY: | The working range is …`);
    /// - a sentence: a head runs to a few words;
    /// - a line set much smaller than the table's text: a running head over
    ///   the page's rule, a note.
    pub fn take_column_heads(&mut self, rules: &[Rule], glyphs: &[crate::glyph::Glyph]) {
        let cols = self.cols();
        if cols < 2 || self.xs.len() != cols + 1 || self.ys.len() < 3 {
            return;
        }
        let (left, right, top) = (self.xs[0], self.xs[cols], self.ys[0]);
        let width = right - left;
        // The top is a rule across the table.
        let at_top = rules
            .iter()
            .filter(|r| {
                r.orientation == Orientation::Horizontal
                    && (r.axis_pos() - top).abs() <= EDGE_TOLERANCE
            })
            .map(|r| (r.bbox.x0, r.bbox.x1));
        if span_coverage(at_top, left, right) < 0.9 * width {
            return;
        }
        // The size the table's own text is set in.
        let mut inside: Vec<f64> = glyphs
            .iter()
            .filter(|g| g.is_horizontal() && !g.text.trim().is_empty())
            .filter(|g| {
                g.bbox.is_some_and(|b| {
                    let (cx, cy) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
                    cx > left && cx < right && cy > top && cy < self.bbox.y1
                })
            })
            .map(|g| g.font_size as f64)
            .collect();
        inside.sort_by(f64::total_cmp);
        let Some(&body) = inside.get(inside.len() / 2) else {
            return;
        };
        // Each mark over the top: baseline, size, ink, weight, text.
        let mut over: Vec<(f64, f64, BBox, bool, &str)> = glyphs
            .iter()
            .filter(|g| g.is_horizontal() && !g.text.trim().is_empty())
            .filter_map(|g| {
                let bold = g.weight.is_some_and(|w| w >= 600);
                Some((
                    g.origin.1,
                    g.font_size.max(1.0) as f64,
                    g.bbox?,
                    bold,
                    g.text.as_str(),
                ))
            })
            .filter(|(_, _, b, _, _)| b.y1 <= top + 1.0 && b.y1 > top - 8.0 * MIN_CELL_EXTENT)
            .collect();
        // Nearest first, then a line at a time by baseline.
        over.sort_by(|a, b| b.0.total_cmp(&a.0));
        let column_of = |x0: f64, x1: f64| {
            (0..cols).find(|&c| x0 >= self.xs[c] - 1.0 && x1 <= self.xs[c + 1] + 1.0)
        };
        let year = |t: &str| {
            t.len() == 4
                && t.chars().all(|c| c.is_ascii_digit())
                && (t.starts_with("19") || t.starts_with("20"))
        };
        let mut taken: Vec<BBox> = Vec::new();
        // The runs of the line last taken: start, column, size, weight.
        let mut under: Vec<(f64, usize, f64, bool)> = Vec::new();
        let mut below = top;
        let mut i = 0;
        while i < over.len() {
            let (base, fs, _, _, _) = over[i];
            let end = (i..over.len())
                .find(|&k| base - over[k].0 > 0.3 * fs)
                .unwrap_or(over.len());
            let mut marks: Vec<(BBox, f64, bool, &str)> =
                over[i..end].iter().map(|o| (o.2, o.1, o.3, o.4)).collect();
            i = end;
            let first = taken.is_empty();
            let reach = if first { 1.0 } else { 1.6 };
            if below - base > reach * fs {
                break;
            }
            // Text of the line the table does not reach: prose beside it.
            if marks
                .iter()
                .all(|m| m.0.x1 <= left - 1.0 || m.0.x0 >= right + 1.0)
            {
                continue;
            }
            if marks
                .iter()
                .any(|m| m.0.x0 < left - 1.0 || m.0.x1 > right + 1.0)
            {
                break;
            }
            let mut sizes: Vec<f64> = marks.iter().map(|m| m.1).collect();
            sizes.sort_by(f64::total_cmp);
            if sizes[sizes.len() / 2] < 0.75 * body {
                break;
            }
            // Runs of text: glyphs a word space apart or closer, each with
            // its first glyph's size and weight, and its text.
            marks.sort_by(|a, b| a.0.x0.total_cmp(&b.0.x0));
            let mut runs: Vec<(f64, f64, f64, bool, String)> = Vec::new();
            for (b, size, bold, text) in &marks {
                match runs.last_mut() {
                    Some(r) if b.x0 - r.1 <= 0.6 * fs => {
                        if b.x0 - r.1 > 0.15 * fs {
                            r.4.push(' ');
                        }
                        r.4.push_str(text);
                        r.1 = r.1.max(b.x1);
                    }
                    _ => runs.push((b.x0, b.x1, *size, *bold, text.to_string())),
                }
            }
            let Some(homes) = runs
                .iter()
                .map(|r| column_of(r.0, r.1))
                .collect::<Option<Vec<usize>>>()
            else {
                break;
            };
            let mut columns = homes.clone();
            columns.dedup();
            let label = runs.first().filter(|_| homes[0] == 0).map(|r| r.4.trim());
            let data = label.is_some_and(|l| {
                l.ends_with(':')
                    || (l.chars().any(char::is_alphabetic)
                        && runs[1..].iter().any(|r| {
                            let t = r.4.trim();
                            is_numeric_cell(t) && !year(t)
                        }))
            });
            if columns.len() < homes.len()
                || data
                || runs
                    .iter()
                    .any(|r| r.4.split_whitespace().count() > MAX_HEAD_WORDS)
            {
                break;
            }
            let close = !first && below - base <= PARAGRAPH_LEADING * fs;
            let wrapped = close
                && columns.len() == 1
                && (columns[0] > 0
                    || under.iter().any(|&(x0, c, size, bold)| {
                        c == 0
                            && (x0 - runs[0].0).abs() <= 1.0
                            && (size - runs[0].2).abs() <= 0.25
                            && bold == runs[0].3
                    }));
            if columns.len() < 2 && !wrapped {
                break;
            }
            under = runs
                .iter()
                .zip(&homes)
                .map(|(r, &c)| (r.0, c, r.2, r.3))
                .collect();
            taken.extend(marks.iter().map(|m| m.0));
            below = base;
        }
        if taken.is_empty() {
            return;
        }
        let y0 = taken.iter().map(|b| b.y0).fold(f64::MAX, f64::min) - 1.0;
        self.ys.insert(0, y0);
        self.bbox.y0 = y0;
    }

    /// The spans of a top row of column-group labels: each label spans the
    /// columns the rule under it runs under ([`Grid::column_groups`]).
    ///
    /// Read only where no column boundary is drawn through the row, so
    /// nothing else says where its cells part, and where each group's part
    /// of the row is a label: one run of text over the group. Values over a
    /// rule that happens to leave a column out are set column by column,
    /// with the columns' gutters between them.
    pub fn group_label_merges(
        &self,
        glyphs: &[crate::glyph::Glyph],
        rules: &[Rule],
        merges: &mut Merges,
    ) {
        let (rows, cols) = (self.rows(), self.cols());
        if rows < 3 || cols < 3 || merges.continues_left.len() != rows * cols {
            return;
        }
        let mid = (self.ys[0] + self.ys[1]) / 2.0;
        let drawn = (1..cols).any(|c| {
            rules.iter().any(|r| {
                r.orientation == Orientation::Vertical
                    && (r.bbox.x0 - self.xs[c]).abs() <= EDGE_TOLERANCE * 2.0
                    && r.bbox.y0 <= mid
                    && r.bbox.y1 >= mid
            })
        });
        if drawn {
            return;
        }
        let Some(groups) = self.column_groups(rules, 1) else {
            return;
        };
        // The row's ink, by line.
        let ink: Vec<(f64, f64, BBox)> = glyphs
            .iter()
            .filter(|g| g.is_horizontal() && !g.text.trim().is_empty())
            .filter_map(|g| Some((g.origin.1, g.font_size.max(1.0) as f64, g.bbox?)))
            .filter(|(_, _, b)| {
                let y = (b.y0 + b.y1) / 2.0;
                y > self.ys[0] && y < self.ys[1]
            })
            .collect();
        let homes = self.glyph_cells(glyphs);
        let label = |g: &ColumnGroup| {
            let (x0, x1) = (self.xs[g.cols.start], self.xs[g.cols.end]);
            let mut held: Vec<&(f64, f64, BBox)> = ink
                .iter()
                .filter(|(_, _, b)| {
                    let x = (b.x0 + b.x1) / 2.0;
                    x > x0 && x < x1
                })
                .collect();
            held.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut i = 0;
            while i < held.len() {
                let (base, fs, _) = *held[i];
                let end = (i..held.len())
                    .find(|&k| held[k].0 - base > 0.3 * fs)
                    .unwrap_or(held.len());
                let mut line: Vec<BBox> = held[i..end].iter().map(|h| h.2).collect();
                line.sort_by(|a, b| a.x0.total_cmp(&b.x0));
                if line.windows(2).any(|w| w[1].x0 - w[0].x1 > fs) {
                    return false;
                }
                i = end;
            }
            // One text: in one of the group's cells, or set across the cuts
            // between them. Text the cuts part cleanly is a value a column.
            let cells: std::collections::BTreeSet<usize> = homes
                .iter()
                .flatten()
                .copied()
                .filter(|i| g.cols.contains(i))
                .collect();
            match cells.len() {
                0 => false,
                1 => true,
                _ => self
                    .region_text(glyphs, merges, 0..1, g.cols.clone())
                    .is_some(),
            }
        };
        let wide: Vec<&ColumnGroup> = groups.iter().filter(|g| g.cols.len() >= 2).collect();
        if !wide.iter().all(|g| label(g)) {
            return;
        }
        for g in wide {
            for c in g.cols.start + 1..g.cols.end {
                merges.continues_left[c] = true;
            }
        }
        merges.full_width_row[0] = false;
    }
}

/// Segments of rule along one boundary at most this far apart are one
/// piece, drawn in parts.
const RULE_JOIN: f64 = 1.0;

/// Glyphs either side of a column edge set closer than this, in ems of the
/// left one's size, are one line run on across the edge: a word space is a
/// quarter to a third of an em, and a cell's padding either side of its
/// rule sets the values of two cells further apart than that.
const CELL_GAP_EMS: f64 = 0.4;

/// A run of columns a piece of rule along a row boundary runs under, and
/// the piece's extent.
#[derive(Debug, Clone)]
struct ColumnGroup {
    cols: std::ops::Range<usize>,
    x0: f64,
    x1: f64,
}

/// Detect one table grid from a related set of ruling lines.
fn detect_group(
    rules: &[Rule],
    fills: &[crate::rule::Fill],
    glyphs: &[crate::glyph::Glyph],
    page: usize,
    frames: &[BBox],
) -> Vec<Grid> {
    detect_group_trimmed(rules, fills, glyphs, page, frames, true)
}

/// [`detect_group`], trimming the grid's ends of bands no vertical crosses
/// or not: a stack of row lines ruled alike ([`ruled_stacks`]) is the
/// table's rows by how they are drawn, and a heading's underline is not
/// ruled like them.
fn detect_group_trimmed(
    rules: &[Rule],
    fills: &[crate::rule::Fill],
    glyphs: &[crate::glyph::Glyph],
    page: usize,
    frames: &[BBox],
    trim: bool,
) -> Vec<Grid> {
    let hs: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.orientation == Orientation::Horizontal)
        .collect();
    let vs: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.orientation == Orientation::Vertical)
        .collect();
    if hs.len() < MIN_GRID_LINES && vs.len() < MIN_GRID_LINES {
        return Vec::new();
    }

    let mut h_lines = cluster(&hs);
    clear_crossed_gutters(&mut h_lines, glyphs);
    let v_lines = cluster(&vs);

    // Overall extent of the ruled area, from both orientations.
    let x_lo = h_lines
        .iter()
        .map(|g| g.lo)
        .chain(v_lines.iter().map(|g| g.axis))
        .fold(f64::MAX, f64::min);
    let x_hi = h_lines
        .iter()
        .map(|g| g.hi)
        .chain(v_lines.iter().map(|g| g.axis))
        .fold(f64::MIN, f64::max);
    let y_lo = v_lines
        .iter()
        .map(|g| g.lo)
        .chain(h_lines.iter().map(|g| g.axis))
        .fold(f64::MAX, f64::min);
    let y_hi = v_lines
        .iter()
        .map(|g| g.hi)
        .chain(h_lines.iter().map(|g| g.axis))
        .fold(f64::MIN, f64::max);
    let span_x = x_hi - x_lo;
    let span_y = y_hi - y_lo;
    if span_x <= 0.0 || span_y <= 0.0 {
        return Vec::new();
    }

    // The two axes are symmetric, and both directions occur in real files:
    //
    //   one table   segmented *horizontal* rules; column boundaries come
    //               from their endpoints.
    //   another     segmented *vertical* rules, one per row; row boundaries
    //               come from *their* endpoints.
    //
    // Handling only the first case rejected every table built the second way,
    // which was 4 of the 17 documents scoring TEDS 0.000.
    // Rows thinner than a cell are degenerate, save one: whitespace between
    // two boxes, which no vertical crosses while the verticals stop at its
    // top and start again at its bottom. Two ruled tables set a margin
    // apart (`margin: 10px`, 7.9pt) leave exactly that band, and without
    // it the second table's top rule is dropped and the two read as one.
    let xs = boundaries(&v_lines, &h_lines, span_y, span_x);
    let first = (xs.len() >= 2).then_some(xs.as_slice());
    let ys = {
        let mut out: Vec<f64> = Vec::new();
        for y in boundary_candidates(
            &h_lines,
            &row_breakers(&h_lines, &v_lines, span_x, fills, glyphs, first),
            span_x,
            span_y,
        ) {
            match out.last() {
                Some(&prev) if y - prev < MIN_CELL_EXTENT => {
                    let mid = (prev + y) / 2.0;
                    let crossed = vs.iter().any(|r| r.bbox.y0 <= mid && r.bbox.y1 >= mid);
                    let stop = vs
                        .iter()
                        .any(|r| (r.bbox.y1 - prev).abs() <= EDGE_TOLERANCE);
                    let restart = vs.iter().any(|r| (r.bbox.y0 - y).abs() <= EDGE_TOLERANCE);
                    if !crossed && stop && restart && y - prev > EDGE_TOLERANCE {
                        out.push(y);
                    }
                }
                _ => out.push(y),
            }
        }
        out
    };
    if ys.len() < 2 || xs.len() < 2 {
        return Vec::new();
    }
    // One-column tables are legitimate (boxed competency lists and forms),
    // but require a sustained stack of ruled rows. This keeps ordinary boxes
    // and callouts out of the table path.
    if xs.len() == 2 && (ys.len() < 4 || v_lines.len() != 2) {
        return Vec::new();
    }
    // The mirror case: one row of cells ruled side by side — a letterhead or
    // title banner. Rejecting one-row grids outright left the banner's rules
    // to cluster into whatever table sat below it, and that table's column
    // boundaries then sliced the banner's text mid-word. `is_banner` carries
    // the test; the extra condition here is that the page's horizontals be
    // just the banner's own two, since a third belongs to something else that
    // the page-wide hypothesis has folded in.
    if ys.len() == 2 && (!is_banner(&xs, &ys, rules) || h_lines.len() != 2) {
        return Vec::new();
    }

    // Trim leading/trailing row bands no vertical rule crosses. A page
    // decoration or a heading's underline is a full-width horizontal rule
    // sitting just above the table's box; clustering welds it in and the
    // heading text above the table becomes "row 0". A band genuinely inside
    // the table lies within its border box, so some vertical crosses it;
    // a band outside the box has none. Only applies when the grid has
    // verticals among its rows at all — tables ruled purely with horizontals
    // carry no such evidence in either direction and are left whole. A
    // vertical far off the grid (a logo's strokes at the page foot, the
    // page-wide reading taking in every rule) is no evidence about its rows:
    // only verticals within a row's height of them count.
    let mut ys = ys;
    let (top, bottom) = (ys[0], ys[ys.len() - 1]);
    let row = {
        let mut gaps: Vec<f64> = ys.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_by(f64::total_cmp);
        gaps[gaps.len() / 2]
    };
    let among: Vec<&&Rule> = vs
        .iter()
        .filter(|r| r.bbox.y1 > top - row && r.bbox.y0 < bottom + row)
        .collect();
    let boxed = vs
        .iter()
        .any(|r| r.bbox.y1 > top + EDGE_TOLERANCE && r.bbox.y0 < bottom - EDGE_TOLERANCE);
    let crossed = |lo: f64, hi: f64| {
        let mid = (lo + hi) / 2.0;
        among.iter().any(|r| r.bbox.y0 <= mid && r.bbox.y1 >= mid)
    };
    if trim && !among.is_empty() {
        while ys.len() > MIN_GRID_LINES && !crossed(ys[0], ys[1]) {
            ys.remove(0);
        }
        while ys.len() > MIN_GRID_LINES && !crossed(ys[ys.len() - 2], ys[ys.len() - 1]) {
            ys.pop();
        }
        // Trimmed as far as a grid goes and still no band crossed, the
        // verticals say nothing of these rows, and the rules alone must
        // bound them. Lines of text between two rules drawn over different
        // stretches of the page, sharing none, are no row: the rules divide
        // the sections of a page set in columns, one column's rule over the
        // other's. Two tables side by side, each ending on its own rule a
        // little above or below the other's, leave between those rules a
        // sliver holding nothing, or one table's last line.
        let extent = |y: f64| {
            h_lines
                .iter()
                .filter(|l| (l.axis - y).abs() <= EDGE_TOLERANCE)
                .fold(None, |acc: Option<(f64, f64)>, l| {
                    Some(acc.map_or((l.lo, l.hi), |(a, b)| (a.min(l.lo), b.max(l.hi))))
                })
        };
        // Lines of text under one of the two rules, in its stretch: a
        // column's paragraphs or list ([`SECTION_LINES`] or more), not one
        // table's last row, wrapped or not, beside the other's.
        let lines_under = |lo: f64, hi: f64, (x0, x1): (f64, f64)| {
            let under: Vec<&crate::glyph::Glyph> = glyphs
                .iter()
                .filter(|gl| {
                    !gl.text.trim().is_empty()
                        && gl.bbox.is_some_and(|b| {
                            let (x, y) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
                            y > lo && y < hi && x > x0 && x < x1
                        })
                })
                .collect();
            let mut sizes: Vec<f64> = under.iter().map(|gl| gl.font_size as f64).collect();
            sizes.sort_by(f64::total_cmp);
            // A line's raised marks (`***`, a note's letter) are on it.
            let tol = sizes
                .get(sizes.len() / 2)
                .map_or(2.0, |s| (s * 0.5).max(2.0));
            let mut bases: Vec<f64> = under.iter().map(|gl| gl.origin.1).collect();
            bases.sort_by(f64::total_cmp);
            coalesce(bases, tol).len()
        };
        let divides = |a: f64, b: f64| match (extent(a), extent(b)) {
            (Some((a0, a1)), Some((b0, b1))) => {
                a1.min(b1) <= a0.max(b0)
                    && (lines_under(a, b, (a0, a1)) >= SECTION_LINES
                        || lines_under(a, b, (b0, b1)) >= SECTION_LINES)
            }
            _ => false,
        };
        if !ys.windows(2).any(|w| crossed(w[0], w[1])) && ys.windows(2).any(|w| divides(w[0], w[1]))
        {
            return Vec::new();
        }
    }

    // Boundaries closer together than this hold no content — an artefact of a
    // border rule sitting alongside a cell edge, which produced a spurious
    // empty first column on one such table.
    // The rules make the table; a shaded band's edge only parts its rows.
    let verticals: Vec<BBox> = vs.iter().map(|r| r.bbox).collect();
    let ys = with_fill_edges(ys, &xs, fills, glyphs, !boxed, &verticals);
    split_row_bands(xs, ys, rules, page, frames, glyphs)
}

/// The verticals whose ends say where rows part.
///
/// A table drawn with verticals segmented row by row and nothing between
/// its rows says where they part only by where its verticals break. A table
/// that rules its rows with horizontals says it with those: a break in a
/// vertical with no horizontal there is the drawing's own (each line of a
/// two-line cell drawn as a piece), not a row. Its verticals' outer ends
/// still close the first and last rows.
fn row_breakers(
    h_lines: &[GridLine],
    v_lines: &[GridLine],
    span_x: f64,
    fills: &[crate::rule::Fill],
    glyphs: &[crate::glyph::Glyph],
    first: Option<&[f64]>,
) -> Vec<GridLine> {
    let ruled: Vec<f64> = h_lines
        .iter()
        .filter(|g| (g.hi - g.lo) >= span_x * MIN_LINE_COVERAGE)
        .map(|g| g.axis)
        .collect();
    let (top, bottom) = v_lines
        .iter()
        .fold((f64::MAX, f64::MIN), |(t, b), g| (t.min(g.lo), b.max(g.hi)));
    let interior = ruled
        .iter()
        .filter(|&&y| y > top + EDGE_TOLERANCE && y < bottom - EDGE_TOLERANCE)
        .count();
    let (left, right) = v_lines.iter().fold((f64::MAX, f64::MIN), |(l, r), g| {
        (l.min(g.axis), r.max(g.axis))
    });
    // The line under the break goes on the row above it where every
    // vertical breaks there, the first column, which names each row, sets
    // text just above the break and none just below (`54 1/8` over `1375`,
    // the model named once), and the lines either side are one cell's.
    let continues_row = |e: f64| {
        let Some(cols) = first else {
            return false;
        };
        let (x0, x1) = (cols[0], cols[1]);
        let near = |below: bool| {
            glyphs.iter().any(|g| {
                let Some(b) = g.bbox else {
                    return false;
                };
                let cx = (b.x0 + b.x1) / 2.0;
                let reach = 2.0 * g.font_size as f64;
                let dy = g.origin.1 - e;
                g.is_horizontal()
                    && !g.text.trim().is_empty()
                    && cx > x0
                    && cx < x1
                    && if below {
                        dy > 0.0 && dy < reach
                    } else {
                        dy < 0.0 && -dy < reach
                    }
            })
        };
        // Every vertical breaks there: a span across the line (a label
        // centred over three samples) runs on through it, and the break is
        // only the samples' own.
        let all_break = !v_lines.iter().any(|g| {
            g.ends
                .chunks(2)
                .any(|s| s.len() == 2 && s[0] < e - EDGE_TOLERANCE && s[1] > e + EDGE_TOLERANCE)
        });
        // And in every column with text either side, the lines either side
        // are a paragraph's leading apart: each cell's two lines, where rows
        // are set further apart (`Stir stressed` over `Light stressed`
        // beside a formulation wrapped across them).
        let tight = || {
            let mut any = false;
            for w in cols.windows(2) {
                let line = |below: bool| {
                    glyphs
                        .iter()
                        .filter(|g| {
                            g.bbox.is_some()
                                && g.is_horizontal()
                                && !g.text.trim().is_empty()
                                && g.origin.0 >= w[0]
                                && g.origin.0 < w[1]
                                && if below {
                                    g.origin.1 > e && g.origin.1 - e < 2.0 * g.font_size as f64
                                } else {
                                    g.origin.1 < e && e - g.origin.1 < 2.0 * g.font_size as f64
                                }
                        })
                        .map(|g| (g.origin.1, g.font_size as f64))
                        .reduce(|p, q| if (q.0 > p.0) != below { q } else { p })
                };
                if let (Some((a, fa)), Some((b, fb))) = (line(false), line(true)) {
                    if b - a > PARAGRAPH_LEADING * fa.max(fb) {
                        return false;
                    }
                    any = true;
                }
            }
            any
        };
        all_break && near(false) && !near(true) && tight()
    };
    if interior == 0 {
        return v_lines.to_vec();
    }
    v_lines
        .iter()
        .map(|g| GridLine {
            axis: g.axis,
            lo: g.lo,
            hi: g.hi,
            gutters: g.gutters.clone(),
            gutter_ends: g.gutter_ends.clone(),
            ends: g
                .ends
                .iter()
                .copied()
                .filter(|&e| {
                    (e - g.lo).abs() <= EDGE_TOLERANCE
                        || (e - g.hi).abs() <= EDGE_TOLERANCE
                        || h_lines.iter().any(|h| (h.axis - e).abs() <= EDGE_TOLERANCE)
                        || fills.iter().any(|f| {
                            f.bbox.x1 > left
                                && f.bbox.x0 < right
                                && ((f.bbox.y0 - e).abs() <= EDGE_TOLERANCE
                                    || (f.bbox.y1 - e).abs() <= EDGE_TOLERANCE)
                        })
                        || !continues_row(e)
                })
                .collect(),
        })
        .collect()
}

/// Column boundaries for one row band, from the rules that lie within it.
///
/// Returns `None` when the band's own rules do not yield a usable set, so the
/// caller can fall back to the page-wide boundaries.
fn band_columns(ys: &[f64], rules: &[Rule], glyphs: &[crate::glyph::Glyph]) -> Option<Vec<f64>> {
    let (lo, hi) = (ys[0] - EDGE_TOLERANCE, ys[ys.len() - 1] + EDGE_TOLERANCE);
    let inside: Vec<Rule> = rules
        .iter()
        .filter(|r| r.bbox.y1 >= lo && r.bbox.y0 <= hi)
        // A vertical must cross one of the band's rows to rule its columns.
        // The tolerance that admits a horizontal drawn a hair outside the
        // band also admitted the verticals of the *next* table down, whose
        // top rule is this band's last boundary: they begin exactly where
        // it ends, and every column of that table was imposed on rows it
        // never touched.
        .filter(|r| {
            r.orientation != Orientation::Vertical
                || ys.windows(2).any(|w| {
                    let mid = (w[0] + w[1]) / 2.0;
                    r.bbox.y0 <= mid && r.bbox.y1 >= mid
                })
        })
        .copied()
        .collect();
    let hs: Vec<&Rule> = inside
        .iter()
        .filter(|r| r.orientation == Orientation::Horizontal)
        .collect();
    let vs: Vec<&Rule> = inside
        .iter()
        .filter(|r| r.orientation == Orientation::Vertical)
        .collect();
    if hs.is_empty() && vs.is_empty() {
        return None;
    }
    let mut h_lines = cluster(&hs);
    clear_crossed_gutters(&mut h_lines, glyphs);
    let v_lines = cluster(&vs);
    let x_lo = h_lines
        .iter()
        .map(|g| g.lo)
        .chain(v_lines.iter().map(|g| g.axis))
        .fold(f64::MAX, f64::min);
    let x_hi = h_lines
        .iter()
        .map(|g| g.hi)
        .chain(v_lines.iter().map(|g| g.axis))
        .fold(f64::MIN, f64::max);
    let span_x = x_hi - x_lo;
    let span_y = hi - lo;
    if span_x <= 0.0 || span_y <= 0.0 {
        return None;
    }
    let xs = with_outer_edges(
        boundaries(&v_lines, &h_lines, span_y, span_x),
        &h_lines,
        span_x,
    );
    (xs.len() >= 2).then(|| with_partial_columns(xs, ys, &vs))
}

/// `xs`, with the table's outer edges where its first and last rules end.
///
/// A first column of labels each spanning several rows (`Ceilings` over
/// four) leaves those rows ruled from the second column on: only the rules
/// round each group reach the table's edge, too few for the edge to be a
/// boundary by the rules' ends. The first and last rules ending together
/// say where the table's box is. One table's rules only: across a page, the
/// first and last are as often its running head and foot.
fn with_outer_edges(xs: Vec<f64>, h_lines: &[GridLine], span_x: f64) -> Vec<f64> {
    let wide: Vec<&GridLine> = h_lines
        .iter()
        .filter(|g| (g.hi - g.lo) >= span_x * MIN_LINE_COVERAGE)
        .collect();
    let [first, .., last] = wide.as_slice() else {
        return xs;
    };
    let mut out = xs;
    for (a, b) in [(first.lo, last.lo), (first.hi, last.hi)] {
        let edge = (a + b) / 2.0;
        if (a - b).abs() <= EDGE_TOLERANCE
            && !out.iter().any(|x| (x - edge).abs() <= EDGE_TOLERANCE)
        {
            out.push(edge);
        }
    }
    out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    drop_degenerate(out, MIN_CELL_EXTENT)
}

/// `xs`, with the column boundaries a table rules in only some of its rows.
///
/// A spec table gives most rows one value across several columns and rules
/// the line between two of them only in the rows where they differ (`35 µm`
/// beside `40 µm`, one `>1500` across both). Such a line falls short of the
/// coverage a grid line needs, and the rows that draw it read both values as
/// one. A vertical that stands on the table's row lines at both ends and
/// crosses two rows or more is a cell's edge however few rows draw it; the
/// rows it is missing from span it. Pieces drawn a few points apart in
/// different rows, never two in one row, are one line: a word processor
/// sizes each row's cells on its own.
fn with_partial_columns(xs: Vec<f64>, ys: &[f64], vs: &[&Rule]) -> Vec<f64> {
    if ys.len() < 3 {
        return xs;
    }
    let (lo, hi) = (xs[0], xs[xs.len() - 1]);
    let on_row_line = |y: f64| ys.iter().any(|r| (r - y).abs() <= EDGE_TOLERANCE);
    // Each candidate line: its position and the rows it crosses.
    let mut lines: Vec<(f64, Vec<usize>)> = Vec::new();
    for r in vs {
        let x = r.axis_pos();
        if x <= lo + MIN_CELL_EXTENT
            || x >= hi - MIN_CELL_EXTENT
            || xs.iter().any(|b| (b - x).abs() <= EDGE_TOLERANCE)
            || !on_row_line(r.bbox.y0)
            || !on_row_line(r.bbox.y1)
        {
            continue;
        }
        let rows: Vec<usize> = ys
            .windows(2)
            .enumerate()
            .filter(|(_, w)| {
                let mid = (w[0] + w[1]) / 2.0;
                r.bbox.y0 <= mid && r.bbox.y1 >= mid
            })
            .map(|(k, _)| k)
            .collect();
        match lines
            .iter_mut()
            .find(|(at, _)| (at - x).abs() <= AXIS_TOLERANCE)
        {
            Some((_, have)) => have.extend(rows),
            None => lines.push((x, rows)),
        }
    }
    lines.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // Lines a few points apart that never share a row are one.
    let mut merged: Vec<(f64, Vec<usize>, usize)> = Vec::new();
    for (x, mut rows) in lines {
        rows.sort_unstable();
        rows.dedup();
        match merged.last_mut() {
            Some((at, have, n))
                if x - *at <= 2.0 * EDGE_TOLERANCE && !rows.iter().any(|k| have.contains(k)) =>
            {
                *at = (*at * *n as f64 + x) / (*n as f64 + 1.0);
                *n += 1;
                have.extend(rows);
            }
            _ => merged.push((x, rows, 1)),
        }
    }
    let mut out = xs;
    out.extend(
        merged
            .into_iter()
            .filter(|(_, rows, _)| rows.len() >= 2)
            .map(|(x, _, _)| x),
    );
    out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    drop_degenerate(out, MIN_CELL_EXTENT)
}

/// Is this single row of cells a banner — a letterhead or title block?
///
/// One row is a table only when something encloses it: several columns, and a
/// vertical rule running the height of the band to close the box. Enclosure is
/// what separates a banner from a caption's underlines, which draw a short
/// rule beneath each word. Their endpoints cluster into a dozen column
/// boundaries and their two baselines into a top and a bottom, so on
/// boundaries alone a row of underlined words is indistinguishable from a
/// letterhead — and one such page turned its real table into twelve empty
/// cells. Nothing encloses an underline.
fn is_banner(xs: &[f64], ys: &[f64], rules: &[Rule]) -> bool {
    if ys.len() != 2 || xs.len() < 4 {
        return false;
    }
    // The vertical has to stand inside the box it closes. Accepting one
    // anywhere on the page that merely spans the same heights let a
    // decorative bar in a margin vouch for a row it never touched, which
    // called every entry of a ruled table of contents a banner.
    let mid = (ys[0] + ys[1]) / 2.0;
    let (lo, hi) = (xs[0] - EDGE_TOLERANCE, xs[xs.len() - 1] + EDGE_TOLERANCE);
    rules.iter().any(|r| {
        r.orientation == Orientation::Vertical
            && r.bbox.y0 <= mid
            && r.bbox.y1 >= mid
            && r.bbox.x0 >= lo
            && r.bbox.x1 <= hi
    })
}

/// Vertical gap between consecutive row boundaries, as a multiple of the median
/// row height, above which the rules belong to *different* tables.
///
/// Benchmarked at 2.5, 3.5 and 5.0. 2.5 split too eagerly (TEDS 0.779), 5.0 too
/// rarely (0.800); 3.5 is both the best TEDS and the best overall.
const TABLE_SPLIT_GAP: f64 = 3.5;

/// Split one page's row boundaries into separate tables.
///
/// A page's ruling lines are not one table. One page carries three tables of
/// 8, 4 and 6 rows and returned a single 20-row grid; another carries two and
/// returned one. Consecutive row boundaries separated by much more than the
/// median row height are a gap between tables, not a tall row.
fn split_row_bands(
    xs: Vec<f64>,
    ys: Vec<f64>,
    rules: &[Rule],
    page: usize,
    frames: &[BBox],
    glyphs: &[crate::glyph::Glyph],
) -> Vec<Grid> {
    if ys.len() < 2 {
        return Vec::new();
    }
    let mut heights: Vec<f64> = ys.windows(2).map(|w| w[1] - w[0]).collect();
    let mut sorted = heights.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2].max(1.0);
    heights.push(0.0);

    // Height alone misses the gap between two tables that happen to sit close
    // together: a letterhead banner directly above a data table cleared the
    // rules but not 3.5x the median row, so the two clustered into one grid
    // and the banner's text was cut at the data table's columns. A band that
    // no vertical rule crosses is the whitespace between two ruled boxes, not
    // a tall row — the same evidence the leading/trailing trim already uses.
    //
    // The band must sit *directly* between two crossed bands. Asking only for
    // some crossed band somewhere above and somewhere below is far weaker than
    // it reads: one decorative vertical at the top of the page and another
    // further down leave every band in between looking like a gap, which split
    // a ruled table of contents into one table per entry. Two boxes with
    // whitespace between them touch that whitespace on both sides.
    let vs: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.orientation == Orientation::Vertical)
        .collect();
    let crossed: Vec<bool> = ys
        .windows(2)
        .map(|w| {
            let mid = (w[0] + w[1]) / 2.0;
            vs.iter().any(|r| r.bbox.y0 <= mid && r.bbox.y1 >= mid)
        })
        .collect();
    let is_gap = |i: usize| {
        i > 0 && i + 1 < crossed.len() && !crossed[i] && crossed[i - 1] && crossed[i + 1]
    };
    // A run of bands no vertical crosses, next to bands verticals do cross,
    // is a different structure: a key/value list ruled with horizontals
    // only, sitting above a fully ruled table. The leading and trailing
    // trims already refuse uncrossed bands at a grid's ends on this
    // evidence; a run in the middle was taking its columns from the ruled
    // table below it, which cut every label of the list a few characters
    // in. The run keeps its own rows and derives its own columns — from
    // its horizontals alone, one column — rather than borrowing.
    //
    // Both runs must be at least two rows. A single crossed band is a
    // banner or one boxed entry, and a ruled table of contents alternates
    // those with unruled entries all the way down; cutting at every change
    // made a table of each pair. Two structures each several rows tall are
    // two tables.
    //
    // And the uncrossed run must have no columns of its own. The defect is
    // borrowing: a run ruled with continuous horizontals has nothing to
    // say about columns and takes the neighbour's. A run whose horizontals
    // are segmented states its own columns from their endpoints, borrows
    // nothing, and was never cut wrongly — separating it only manufactures
    // a table where the whole-span reading had found none.
    let run_bounds = |i: usize| {
        let mut lo = i;
        while lo > 0 && crossed[lo - 1] == crossed[i] {
            lo -= 1;
        }
        let mut hi = i;
        while hi + 1 < crossed.len() && crossed[hi + 1] == crossed[i] {
            hi += 1;
        }
        (lo, hi)
    };
    let borrows = |i: usize| {
        let (lo, hi) = run_bounds(i);
        band_columns(&ys[lo..=hi + 1], rules, glyphs).is_none_or(|xs| xs.len() <= 2)
    };
    let changes_after = |i: usize| {
        if i + 1 >= crossed.len() || crossed[i] == crossed[i + 1] {
            return false;
        }
        let (a, b) = (run_bounds(i), run_bounds(i + 1));
        let uncrossed = if crossed[i] { i + 1 } else { i };
        a.1 - a.0 >= 1 && b.1 - b.0 >= 1 && borrows(uncrossed)
    };

    // Two ruled tables set flush, one directly under the other, share a
    // border and leave no band of whitespace between them: a browser
    // prints consecutive tables that way. Read as one grid, the union of
    // their column boundaries is every row's, and where two of them fall
    // close together the narrower cut is dropped as degenerate — the
    // lower table's boundary then stands in for the upper's, and the upper
    // table's first two columns read as one (`Item Qty`, `Fish pie 2`).
    //
    // What tells them apart is the box: every row of one ruled table
    // reaches the same left and right edge. Where the edge changes between
    // two runs of rows, each at least two rows, and each run draws a
    // column boundary inside the other's box that the other does not, they
    // are two tables. Totals rows ruled under the last columns of the
    // table above them change the edge but draw no boundary of their own;
    // they stay part of it, aligned to its columns.
    let crossing = |i: usize| -> Vec<f64> {
        let mid = (ys[i] + ys[i + 1]) / 2.0;
        let mut xs: Vec<f64> = vs
            .iter()
            .filter(|r| r.bbox.y0 <= mid && r.bbox.y1 >= mid)
            .map(|r| r.axis_pos())
            .collect();
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        xs
    };
    let reach: Vec<Option<(f64, f64)>> = (0..crossed.len())
        .map(|i| {
            let xs = crossing(i);
            (xs.len() >= 2).then(|| (xs[0], xs[xs.len() - 1]))
        })
        .collect();
    let same_reach = |a: Option<(f64, f64)>, b: Option<(f64, f64)>| match (a, b) {
        (Some(a), Some(b)) => {
            (a.0 - b.0).abs() <= EDGE_TOLERANCE && (a.1 - b.1).abs() <= EDGE_TOLERANCE
        }
        _ => false,
    };
    let reach_run = |i: usize, step: isize| -> Vec<usize> {
        let mut run = vec![i];
        let mut k = i as isize + step;
        while k >= 0 && (k as usize) < reach.len() && same_reach(reach[k as usize], reach[i]) {
            run.push(k as usize);
            k += step;
        }
        run
    };
    // A boundary of one run strictly inside the other's box, with nothing
    // drawn near it there.
    let foreign = |from: &[usize], into: &[usize]| {
        let (lo, hi) = reach[into[0]].unwrap_or((0.0, 0.0));
        let theirs: Vec<f64> = into.iter().flat_map(|&k| crossing(k)).collect();
        from.iter().flat_map(|&k| crossing(k)).any(|x| {
            x > lo + EDGE_TOLERANCE
                && x < hi - EDGE_TOLERANCE
                && !theirs.iter().any(|t| (t - x).abs() <= EDGE_TOLERANCE)
        })
    };
    let boxes_change_after = |i: usize| {
        if i + 1 >= reach.len()
            || reach[i].is_none()
            || reach[i + 1].is_none()
            || same_reach(reach[i], reach[i + 1])
        {
            return false;
        }
        let (above, below) = (reach_run(i, -1), reach_run(i + 1, 1));
        above.len() >= 2 && below.len() >= 2 && foreign(&above, &below) && foreign(&below, &above)
    };

    // In a table drawn with characters, a band the box's own sides run down
    // through, unbroken from the band beside it, is a tall row and not the
    // space between two tables: a batch record's step whose instructions
    // fill the page. A printer rules every row it means, with a line of its
    // own or a blank one, so a band between its rules is one row. Ruled with
    // lines, the same band is as often a body whose rows were never ruled,
    // which the text reads better than one crammed row would. The sides of
    // two boxes set apart stop where each box ends; both sides, so one long
    // bar down a margin vouches for nothing.
    let walled = |i: usize| {
        let Some((lo, hi)) = reach[i] else {
            return false;
        };
        let mid = (ys[i] + ys[i + 1]) / 2.0;
        if !frames.iter().any(|f| {
            f.y0 <= mid && mid <= f.y1 && f.x0 <= lo + EDGE_TOLERANCE && hi <= f.x1 + EDGE_TOLERANCE
        }) {
            return false;
        }
        let neighbours: Vec<f64> = [i.checked_sub(1), Some(i + 1)]
            .into_iter()
            .flatten()
            .filter(|&k| k + 1 < ys.len())
            .map(|k| (ys[k] + ys[k + 1]) / 2.0)
            .collect();
        let side = |x: f64| {
            vs.iter().any(|r| {
                (r.axis_pos() - x).abs() <= EDGE_TOLERANCE
                    && r.bbox.y0 <= mid
                    && r.bbox.y1 >= mid
                    && neighbours.iter().any(|&m| r.bbox.y0 <= m && r.bbox.y1 >= m)
            })
        };
        side(lo) && side(hi)
    };

    // A tall band the table's structure runs on through is one of its rows,
    // when its text is one row's: a label beside a paragraph of value, a SKU
    // beside a three-line description. Height says only that the band is
    // tall; the gap between two tables is told by the structure stopping.
    //
    // Two of its rules crossing the band and the band next to it, by one
    // stroke or each row's own: the box's sides, or two of its column
    // rules. One long rule is as often the page's divider.
    let rules_run_through = |i: usize| {
        let mid = (ys[i] + ys[i + 1]) / 2.0;
        let neighbours: Vec<f64> = [i.checked_sub(1), Some(i + 1)]
            .into_iter()
            .flatten()
            .filter(|&k| k + 1 < ys.len())
            .map(|k| (ys[k] + ys[k + 1]) / 2.0)
            .collect();
        let at = |x: f64, m: f64| {
            vs.iter().any(|r| {
                (r.axis_pos() - x).abs() <= EDGE_TOLERANCE && r.bbox.y0 <= m && r.bbox.y1 >= m
            })
        };
        crossing(i)
            .into_iter()
            .filter(|&x| at(x, mid) && neighbours.iter().any(|&m| at(x, m)))
            .count()
            >= 2
    };
    // Ruled with horizontals alone: the rules over and under the band have
    // one extent and break at the same columns, as a table's rows do.
    let h_lines = {
        let hs: Vec<&Rule> = rules
            .iter()
            .filter(|r| r.orientation == Orientation::Horizontal)
            .collect();
        cluster(&hs)
    };
    let line_at = |y: f64| {
        h_lines
            .iter()
            .find(|g| (g.axis - y).abs() <= EDGE_TOLERANCE)
    };
    let breaks = |g: &GridLine| {
        coalesce(
            g.ends
                .iter()
                .copied()
                .filter(|e| *e > g.lo + EDGE_TOLERANCE && *e < g.hi - EDGE_TOLERANCE)
                .collect(),
            EDGE_TOLERANCE,
        )
    };
    let ruled_alike = |i: usize| -> Option<Vec<f64>> {
        let (a, b) = (line_at(ys[i])?, line_at(ys[i + 1])?);
        if (a.lo - b.lo).abs() > EDGE_TOLERANCE || (a.hi - b.hi).abs() > EDGE_TOLERANCE {
            return None;
        }
        let (ba, bb) = (breaks(a), breaks(b));
        let alike = !ba.is_empty()
            && ba.len() == bb.len()
            && ba
                .iter()
                .zip(&bb)
                .all(|(p, q)| (p - q).abs() <= 2.0 * EDGE_TOLERANCE);
        alike.then(|| {
            let mut cols = vec![a.lo];
            cols.extend(ba);
            cols.push(a.hi);
            cols
        })
    };
    // The band's text is one row's: the first column holding text sets one
    // line or two (a label, wrapped at most once), and another column holds
    // text beside it. A heading between two tables holds one column; a
    // body whose rows were never ruled names a row on every line. Unless
    // the band's cells may span, no line runs across a column's edge, as a
    // paragraph set between two tables would. Rotated labels say nothing
    // of the rows.
    let one_row = |i: usize, cols: &[f64], spanning: bool| {
        if cols.len() < 3 {
            return false;
        }
        let (top, bottom) = (ys[i], ys[i + 1]);
        let inside: Vec<&crate::glyph::Glyph> = glyphs
            .iter()
            .filter(|g| {
                g.bbox.is_some()
                    && g.is_horizontal()
                    && !g.text.trim().is_empty()
                    && g.origin.1 > top
                    && g.origin.1 < bottom
                    && g.origin.0 >= cols[0]
                    && g.origin.0 <= cols[cols.len() - 1]
            })
            .collect();
        let column = |g: &crate::glyph::Glyph| {
            cols.windows(2)
                .position(|w| g.origin.0 >= w[0] && g.origin.0 < w[1])
        };
        let size = inside
            .iter()
            .map(|g| g.font_size as f64)
            .fold(1.0, f64::max);
        let lines = |c: usize| {
            let mut at: Vec<f64> = inside
                .iter()
                .filter(|g| column(g) == Some(c))
                .map(|g| g.origin.1)
                .collect();
            at.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            coalesce(at, 0.3 * size).len()
        };
        let held: Vec<usize> = (0..cols.len() - 1).filter(|&c| lines(c) > 0).collect();
        let [first, _, ..] = held.as_slice() else {
            return false;
        };
        if lines(*first) > 2 {
            return false;
        }
        spanning
            || !cols[1..cols.len() - 1].iter().any(|&x| {
                inside.iter().any(|a| {
                    let ab = a.bbox.unwrap_or_default();
                    ab.x1 <= x
                        && inside.iter().any(|c| {
                            let cb = c.bbox.unwrap_or_default();
                            cb.x0 >= x
                                && (c.origin.1 - a.origin.1).abs() < 0.3 * a.font_size as f64
                                && cb.x0 - ab.x1 < a.font_size as f64
                        })
                })
            })
    };
    let runs_on = |i: usize| {
        (rules_run_through(i) && one_row(i, &crossing(i), true))
            || (!crossed[i] && ruled_alike(i).is_some_and(|cols| one_row(i, &cols, false)))
    };
    // Two tables boxed in one frame, a blank band between them: the band is
    // crossed by the frame's two sides alone, where the bands either side of
    // it rule two tables, columns set at different places, and holds no
    // text. Each table reads its own columns. A band across the table holding a label (a group's
    // banner) is crossed by the frame alone too, and stays a row.
    let (left, right) = (xs[0], xs[xs.len() - 1]);
    let framed_only = |i: usize| {
        let at = crossing(i);
        !at.is_empty()
            && at
                .iter()
                .all(|&x| (x - left).abs() <= EDGE_TOLERANCE || (x - right).abs() <= EDGE_TOLERANCE)
    };
    let ruled_inside = |i: usize| {
        crossing(i)
            .iter()
            .any(|&x| x > left + EDGE_TOLERANCE && x < right - EDGE_TOLERANCE)
    };
    let blank = |i: usize| {
        !glyphs.iter().any(|g| {
            !g.text.trim().is_empty()
                && g.bbox.is_some_and(|b| {
                    let y = (b.y0 + b.y1) / 2.0;
                    y > ys[i] && y < ys[i + 1] && b.x0 >= left && b.x1 <= right
                })
        })
    };
    // The column rules inside the frame differ either side: two tables,
    // not one with a blank row set between its rows.
    let inner = |i: usize| -> Vec<f64> {
        crossing(i)
            .into_iter()
            .filter(|&x| x > left + EDGE_TOLERANCE && x < right - EDGE_TOLERANCE)
            .collect()
    };
    let ruled_apart = |a: usize, b: usize| {
        let (p, q) = (inner(a), inner(b));
        p.len() != q.len()
            || p.iter()
                .zip(&q)
                .any(|(x, y)| (x - y).abs() > EDGE_TOLERANCE)
    };
    // Each side a table by its own ruling: three columns or more, two rules
    // inside, over three rows or more. A box's one row, a diagram's panel,
    // a chart's axis are no table to part off.
    let a_table = |rows: usize, at: usize| rows >= 3 && inner(at).len() >= 2;
    let boxed_gap = |i: usize| {
        if !(i > 0 && i + 2 < ys.len() && framed_only(i) && blank(i)) {
            return false;
        }
        let above = (0..i).rev().take_while(|&k| ruled_inside(k)).count();
        let below = (i + 1..ys.len() - 1)
            .take_while(|&k| ruled_inside(k))
            .count();
        a_table(above, i - 1) && a_table(below, i + 1) && ruled_apart(i - 1, i + 1)
    };

    let mut out = Vec::new();
    let mut start = 0usize;
    for i in 0..ys.len() - 1 {
        let is_last = i + 2 == ys.len();
        // The band a table opens with, its rules running on through it into
        // the rows below, is its header row however tall: column labels
        // wrap over as many lines as they need. Its column rules among them:
        // a frame's two sides run as well round a chart set over a table.
        let heads = i == start && rules_run_through(i) && crossing(i).len() >= 3;
        let too_tall =
            (ys[i + 1] - ys[i]) > median * TABLE_SPLIT_GAP && !walled(i) && !runs_on(i) && !heads
                || is_gap(i)
                || boxed_gap(i);
        let ends_run = !too_tall && (changes_after(i) || boxes_change_after(i));
        if too_tall || is_last || ends_run {
            // A band that is itself too tall is the gap: end the table before it.
            let end = if too_tall { i } else { i + 1 };
            if end > start {
                let sub: Vec<f64> = ys[start..=end].to_vec();
                // Recompute columns from the rules inside this band only.
                // Tables stacked on one page rarely share a column layout —
                // three stacked tables with 5, 4 and 4 columns saw a
                // page-wide `xs` report 8 for all of them.
                let band_xs = band_columns(&sub, rules, glyphs).unwrap_or_else(|| xs.clone());
                // A single band stands alone only as a banner: several columns
                // across one row. Two columns over one row is a box, and the
                // rules around a paragraph are not a table.
                if sub.len() >= MIN_GRID_LINES || is_banner(&band_xs, &sub, rules) {
                    out.push(Grid {
                        layout: None,
                        page,
                        bbox: BBox {
                            x0: band_xs[0],
                            y0: sub[0],
                            x1: band_xs[band_xs.len() - 1],
                            y1: sub[sub.len() - 1],
                        },
                        xs: band_xs,
                        ys: sub,
                    });
                }
            }
            if too_tall || ends_run {
                start = i + 1;
            }
        }
    }
    // Nothing split out: fall back to the whole span.
    if out.is_empty() && (ys.len() >= MIN_GRID_LINES || is_banner(&xs, &ys, rules)) {
        out.push(Grid {
            layout: None,
            page,
            bbox: BBox {
                x0: xs[0],
                y0: ys[0],
                x1: xs[xs.len() - 1],
                y1: ys[ys.len() - 1],
            },
            xs,
            ys,
        });
    }
    out
}

/// Most lines of text that may stand past a table at the foot of its page,
/// or over its row at the head of the next, for the table to run on across
/// the break: the page's folio or running line.
const BREAK_LINES: usize = 2;

/// Most height a row continued past a page break may have, in the table's
/// median row height: a row of the table's, wrapped onto a line or two.
const CONTINUED_ROW_HEIGHT: f64 = 3.0;

/// The lines of text, counted by baseline, among `glyphs` whose boxes
/// `keep`.
fn lines_where(glyphs: &[crate::glyph::Glyph], keep: impl Fn(&BBox) -> bool) -> usize {
    let mut at: Vec<(f64, f64)> = glyphs
        .iter()
        .filter(|g| !g.text.trim().is_empty() && g.bbox.is_some_and(|b| keep(&b)))
        .map(|g| (g.origin.1, g.font_size.max(1.0) as f64))
        .collect();
    at.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut lines = 0usize;
    let mut last = f64::MIN;
    for (y, fs) in at {
        if y - last > 0.5 * fs {
            lines += 1;
            last = y;
        }
    }
    lines
}

/// Does the ruled table end its page's text: [`BREAK_LINES`] lines or
/// fewer stand under it, a folio or a running line?
pub fn ends_page(g: &Grid, glyphs: &[crate::glyph::Glyph]) -> bool {
    lines_where(glyphs, |b| b.y0 >= g.bbox.y1) <= BREAK_LINES
}

/// A table's last row printed at the head of the next page, inside the
/// table's ruling: `Needle Wash | Methanol` under the page break, its
/// table's other rows on the page before. One ruled row stands alone as no
/// table, and its label and value were read as a heading and a paragraph.
///
/// It is `tail`'s, the ruled table ending the page before ([`ends_page`]),
/// when the page opens on it, [`BREAK_LINES`] lines or fewer over it, and
/// it is ruled as the table is: rules over and under it across the table's
/// width, its sides drawn at the table's, every column rule in it standing
/// at one of the table's columns. A row of a table's height
/// ([`CONTINUED_ROW_HEIGHT`]), with text in two cells or more, and no more
/// rows ruled under it (those are a table of their own). It is read in the
/// table's columns.
pub fn continued_row(
    tail: &Grid,
    rules: &[Rule],
    glyphs: &[crate::glyph::Glyph],
    grids: &[Grid],
    page: usize,
) -> Option<Grid> {
    let (x0, x1) = (*tail.xs.first()?, *tail.xs.last()?);
    let width = x1 - x0;
    let mut heights: Vec<f64> = tail.ys.windows(2).map(|w| w[1] - w[0]).collect();
    heights.sort_by(f64::total_cmp);
    let row = *heights.get(heights.len() / 2)?;
    // The lines ruled across the table's width.
    let mut across: Vec<f64> = Vec::new();
    let hs: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.orientation == Orientation::Horizontal)
        .collect();
    let mut ys: Vec<f64> = hs.iter().map(|r| r.axis_pos()).collect();
    ys.sort_by(f64::total_cmp);
    for y in coalesce(ys, EDGE_TOLERANCE) {
        let spans = hs
            .iter()
            .filter(|r| (r.axis_pos() - y).abs() <= EDGE_TOLERANCE)
            .map(|r| (r.bbox.x0, r.bbox.x1));
        let ends = hs
            .iter()
            .filter(|r| (r.axis_pos() - y).abs() <= EDGE_TOLERANCE);
        let lo = ends.clone().map(|r| r.bbox.x0).fold(f64::MAX, f64::min);
        let hi = ends.map(|r| r.bbox.x1).fold(f64::MIN, f64::max);
        if (lo - x0).abs() <= EDGE_TOLERANCE
            && (hi - x1).abs() <= EDGE_TOLERANCE
            && span_coverage(spans, x0, x1) >= 0.95 * width
        {
            across.push(y);
        }
    }
    let (&top, &bottom) = (across.first()?, across.get(1)?);
    if bottom - top > row * CONTINUED_ROW_HEIGHT
        || across
            .get(2)
            .is_some_and(|&y| y - bottom <= row * CONTINUED_ROW_HEIGHT)
        || lines_where(glyphs, |b| b.y1 <= top) > BREAK_LINES
    {
        return None;
    }
    let band = BBox {
        x0,
        y0: top,
        x1,
        y1: bottom,
    };
    if grids.iter().any(|g| g.bbox.intersects(&band)) {
        return None;
    }
    let mid = (top + bottom) / 2.0;
    let walls: Vec<f64> = rules
        .iter()
        .filter(|r| r.orientation == Orientation::Vertical && r.bbox.y0 <= mid && r.bbox.y1 >= mid)
        .map(|r| r.axis_pos())
        .filter(|&x| x >= x0 - EDGE_TOLERANCE && x <= x1 + EDGE_TOLERANCE)
        .collect();
    let at = |x: f64| walls.iter().any(|w| (w - x).abs() <= EDGE_TOLERANCE);
    let aligned = walls
        .iter()
        .all(|w| tail.xs.iter().any(|x| (x - w).abs() <= EDGE_TOLERANCE));
    if !(at(x0) && at(x1) && aligned) {
        return None;
    }
    let g = Grid {
        layout: None,
        page,
        bbox: band,
        xs: tail.xs.clone(),
        ys: vec![top, bottom],
    };
    let held = g
        .cell_texts(glyphs)
        .iter()
        .filter(|c| !c.trim().is_empty())
        .count();
    (held >= 2).then_some(g)
}
/// A table's head stranded at its page's foot: the break fell under the
/// header's first row, the rest of the table on the next page (the arms,
/// `Drug A n=429 | Drug B n=425`, ruled at one page's foot; `All Grades |
/// Grades 3-4` under them and the body at the next page's head). One ruled
/// row stands alone as no table, and the head was read as paragraphs, the
/// next page's part headed by its own row alone.
///
/// It is one ruled row that ends its page ([`ends_page`]), set as a header
/// is (bold in each cell, or shaded), with labels in two cells or more and
/// no figures, and the next page opens on a table drawn at its columns
/// ([`opens_under`]). A row of a table's body is set as the rows under it
/// are (`• Lab tests | $0 per visit`, `Entrants: | See the day's list`):
/// bold nowhere, or in its label alone. A column's name is a phrase
/// ([`HEAD_LABEL_WORDS`]); captions boxed side by side at the foot, over
/// the next page's figures in boxes at the same places, are sentences.
///
/// Its table's ruling, `drawn_to` before its empty rows were trimmed, ends
/// under the row, or runs on past the page's last line: the break cut it
/// there. A form's labels ruled over an empty box for entries, closed
/// above the page's folio, are a table ending on its page.
pub fn stranded_head(
    g: &Grid,
    drawn_to: f64,
    glyphs: &[crate::glyph::Glyph],
    fills: &[crate::rule::Fill],
) -> bool {
    if g.rows() != 1 || g.cols() < 2 || !ends_page(g, glyphs) {
        return false;
    }
    if drawn_to > g.bbox.y1 + EDGE_TOLERANCE && lines_where(glyphs, |b| b.y0 >= drawn_to) > 0 {
        return false;
    }
    if !g.cells_are_bold(glyphs, 0) && !g.band_has_fill(fills, 0) {
        return false;
    }
    let cells = g.cell_texts(glyphs);
    let labels: Vec<&str> = cells
        .iter()
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .collect();
    labels.len() >= 2
        && !labels
            .iter()
            .any(|c| is_numeric_cell(c) || c.split_whitespace().count() > HEAD_LABEL_WORDS)
}

/// Most words a column's name in a stranded head holds: an arm with its
/// dose and count (`Drug A 200 mg every 3 weeks in combination with Drug B
/// n=403`) runs to a dozen or so; a caption runs to sentences.
const HEAD_LABEL_WORDS: usize = 20;

/// Does the page whose ruled tables are `next` open on a table whose
/// columns are drawn where `head`'s are, to within `tolerance`:
/// [`BREAK_LINES`] lines or fewer over its first table, and as many column
/// edges, each at one of `head`'s?
pub fn opens_under(
    head: &Grid,
    next: &[Grid],
    next_glyphs: &[crate::glyph::Glyph],
    tolerance: f64,
) -> bool {
    let Some(opening) = next.iter().min_by(|a, b| a.bbox.y0.total_cmp(&b.bbox.y0)) else {
        return false;
    };
    lines_where(next_glyphs, |b| b.y1 <= opening.bbox.y0) <= BREAK_LINES
        && opening.xs.len() == head.xs.len()
        && opening
            .xs
            .iter()
            .zip(&head.xs)
            .all(|(a, b)| (a - b).abs() <= tolerance)
}

/// Detect table grids on a page from its ruling lines.
///
/// Try the page-wide hypothesis first for segmented grids, then connected
/// geometry components. The latter prevents an unrelated page rule from
/// inflating the span and making a real table's local dividers look too short.
pub fn detect(rules: &[Rule], page: usize) -> Vec<Grid> {
    detect_framed(rules, &[], &[], page, &[])
}

/// [`detect`], knowing the frames of the tables the page draws with
/// characters ([`crate::textart`]), whose every band between rules is a
/// row however tall.
pub fn detect_framed(
    rules: &[Rule],
    fills: &[crate::rule::Fill],
    glyphs: &[crate::glyph::Glyph],
    page: usize,
    frames: &[BBox],
) -> Vec<Grid> {
    let grids = detect_framed_rules(rules, fills, glyphs, page, frames)
        .into_iter()
        .map(|g| cut_to_box(g, rules, fills, glyphs))
        .map(|g| cut_page_head(g, rules, glyphs))
        .collect();
    with_column_bands(with_header_band_edges(grids, fills, glyphs), rules, glyphs)
}

/// Whether a cell opens on a form's field number and the field's name: one
/// or two digits, a box's letter where the form parts a field (`13a.`),
/// and a stop, then a capital (`1. DATE OF ORDER`, `12. F.O.B. POINT`,
/// `17a. CONTRACTOR/OFFEROR`).
fn numbered_field(text: &str) -> bool {
    let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
    let rest = &text[digits.len()..];
    let rest = match rest.chars().next() {
        Some(l) if l.is_ascii_lowercase() => &rest[1..],
        _ => rest,
    };
    (1..=2).contains(&digits.len())
        && rest.starts_with(". ")
        && rest[2..].chars().next().is_some_and(char::is_uppercase)
}

/// Whether a cell's text is a page counter and nothing else: `PAGE 1 OF
/// 30`, `Page 2 of 7`, a form's `PAGE OF PAGES` box with its two figures.
fn page_counter(text: &str) -> bool {
    let words: Vec<String> = text.split_whitespace().map(|w| w.to_lowercase()).collect();
    words.len() <= 6
        && words.first().is_some_and(|w| w == "page")
        && words.iter().any(|w| w == "of")
        && words.iter().any(|w| w.chars().all(|c| c.is_ascii_digit()))
        && words.iter().all(|w| {
            matches!(w.as_str(), "page" | "of" | "pages") || w.chars().all(|c| c.is_ascii_digit())
        })
}

/// Most rows a form's head band runs to: its title and a notice under it.
const PAGE_HEAD_ROWS: usize = 2;

/// A ruled grid cut under the page's head: its first rows a band holding
/// the page's counter (`PAGE OF PAGES` over `1 30`, read down its column)
/// beside a line or two of words and no figure (the form's title, a
/// notice), down to the first rule across the grid under the counter. A
/// form sets its head inside its frame; the head names nothing of the
/// fields under it, and a row of it is no header. The band's text is read
/// as the page's.
fn cut_page_head(g: Grid, rules: &[Rule], glyphs: &[crate::glyph::Glyph]) -> Grid {
    if g.layout.is_some() || g.rows() < PAGE_HEAD_ROWS + 3 {
        return g;
    }
    let cols = g.cols();
    let texts = g.cell_texts(glyphs);
    let cell = |r: usize, c: usize| texts[r * cols + c].trim();
    // The counter's column, and the rows it is read down.
    let down = |c: usize, rows: usize| {
        (0..rows)
            .map(|r| cell(r, c))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let Some((counter, rows)) = (0..cols).find_map(|c| {
        (1..=PAGE_HEAD_ROWS)
            .find(|&n| page_counter(&down(c, n)))
            .map(|n| (c, n))
    }) else {
        return g;
    };
    let (x0, x1) = (g.xs[0], g.xs[cols]);
    let counter_bottom = glyphs
        .iter()
        .filter(|gl| !gl.text.trim().is_empty())
        .filter_map(|gl| gl.bbox.map(|b| (gl.center(), b)))
        .filter(|((x, y), _)| {
            *x >= g.xs[counter] && *x < g.xs[counter + 1] && *y >= g.ys[0] && *y < g.ys[rows]
        })
        .map(|(_, b)| b.y1)
        .fold(f64::MIN, f64::max);
    // The first boundary under it a rule runs across the grid along.
    let across = |y: f64| {
        rules.iter().any(|r| {
            r.orientation == Orientation::Horizontal
                && (r.axis_pos() - y).abs() <= EDGE_TOLERANCE
                && r.bbox.x0 <= x0 + EDGE_TOLERANCE
                && r.bbox.x1 >= x1 - EDGE_TOLERANCE
        })
    };
    let Some(cut) = (1..=PAGE_HEAD_ROWS).find(|&k| g.ys[k] > counter_bottom && across(g.ys[k]))
    else {
        return g;
    };
    // Beside the counter, each row of the band a line of words: no figure.
    let words_only = (0..cut).all(|r| {
        (0..cols)
            .filter(|&c| c != counter)
            .all(|c| !cell(r, c).chars().any(|ch| ch.is_ascii_digit()))
    });
    if !words_only || g.rows() - cut < 3 {
        return g;
    }
    let ys = g.ys[cut..].to_vec();
    Grid {
        bbox: BBox {
            y0: ys[0],
            ..g.bbox
        },
        ys,
        ..g
    }
}

/// Fewest of a grid's row boundaries a box holds inside it to frame rows of
/// the grid: two, a table's rows ruled apart within it. A row's own
/// shading, or a header's band, holds none.
const BOX_INNER_EDGES: usize = 2;

/// A grid cut to the box its table is set in: a filled box across the
/// grid's width and short of its height, holding rows of the grid ruled
/// apart inside it, one of its edges running through a row of the grid
/// that holds text on both sides of it. That row ran the box's first line
/// together with what stands over it (a receipt's stay details over its
/// `DESCRIPTION | RATE | AMOUNT`), ruled off by the page's rules over the
/// box, those rules running past the box's sides. The table is the box's:
/// the rows outside it are the page's.
///
/// The box opens on the table's header, naming its columns. A header set on
/// the page over a shaded body meets the box at a row's edge, and nothing
/// is cut.
fn cut_to_box(
    g: Grid,
    rules: &[Rule],
    fills: &[crate::rule::Fill],
    glyphs: &[crate::glyph::Glyph],
) -> Grid {
    if g.layout.is_some() || g.rows() < 2 {
        return g;
    }
    let tol = EDGE_TOLERANCE;
    let (gx0, gx1) = (g.xs[0], g.xs[g.cols()]);
    let (gy0, gy1) = (g.ys[0], g.ys[g.rows()]);
    // Glyphs of the grid's width in `y0..y1`.
    let any_in = |y0: f64, y1: f64| {
        glyphs.iter().any(|gl| {
            !gl.text.trim().is_empty()
                && gl.bbox.is_some_and(|b| {
                    let (x, y) = gl.center();
                    x >= gx0 && x <= gx1 && y > y0 && y < y1 && b.y0 >= y0 - tol
                })
        })
    };
    // The row an edge at `y` runs through, text on both sides of it.
    let straddled = |y: f64| {
        g.ys.windows(2)
            .any(|w| w[0] < y - tol && w[1] > y + tol && any_in(w[0], y) && any_in(y, w[1]))
    };
    let Some(b) = fills
        .iter()
        .map(|f| f.bbox)
        .filter(|b| b.x0 <= gx0 + tol && b.x1 >= gx1 - tol)
        .filter(|b| b.y0 > gy0 + tol || b.y1 < gy1 - tol)
        .filter(|b| {
            g.ys.iter()
                .filter(|&&y| y > b.y0 + tol && y < b.y1 - tol)
                .count()
                >= BOX_INNER_EDGES
        })
        .find(|b| {
            // On the side of an edge the grid's row runs through, the grid's
            // rules are the page's, running past both the box's sides: one at
            // least, and none of the box's width.
            let page_ruled = |side: &dyn Fn(f64) -> bool| {
                let ruled: Vec<&Rule> = rules
                    .iter()
                    .filter(|r| r.orientation == Orientation::Horizontal)
                    .filter(|r| side(r.axis_pos()))
                    .filter(|r| g.ys.iter().any(|y| (y - r.axis_pos()).abs() <= tol))
                    .filter(|r| r.bbox.x1 > gx0 && r.bbox.x0 < gx1)
                    .collect();
                !ruled.is_empty()
                    && ruled
                        .iter()
                        .all(|r| r.bbox.x0 < b.x0 - tol && r.bbox.x1 > b.x1 + tol)
            };
            (straddled(b.y0) && page_ruled(&|y| y < b.y0 - tol))
                || (straddled(b.y1) && page_ruled(&|y| y > b.y1 + tol))
        })
    else {
        return g;
    };
    // The box's edges, or a boundary of the grid's at either.
    let at = |edge: f64| {
        g.ys.iter()
            .copied()
            .find(|y| (y - edge).abs() <= tol)
            .unwrap_or(edge)
    };
    let (top, bottom) = (at(b.y0).max(gy0), at(b.y1).min(gy1));
    let mut ys = vec![top];
    ys.extend(
        g.ys.iter()
            .copied()
            .filter(|&y| y > top + tol && y < bottom - tol),
    );
    ys.push(bottom);
    let cut = Grid {
        bbox: BBox {
            y0: top,
            y1: bottom,
            ..g.bbox
        },
        ys,
        ..g.clone()
    };
    // The box opens on its table's header: its first row names two columns
    // or more, each in a few words, no figure or leader among them. A box
    // of fields run on with leaders (`Wages . . . 125,000`) is a form's.
    let cells = cut.cell_texts(glyphs);
    let names: Vec<&str> = cells[..cut.cols()]
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    let named = names.len() >= 2
        && names.iter().all(|t| {
            t.split_whitespace().count() <= BOX_HEAD_WORDS
                && t.chars().any(char::is_alphabetic)
                && !t.chars().any(|c| c.is_ascii_digit())
                && !t.contains("..")
                && !t.contains(". .")
        });
    if named {
        cut
    } else {
        g
    }
}

/// Most words a column's name in a box's first row runs to: a header's
/// label, not a field's line.
const BOX_HEAD_WORDS: usize = 4;

/// [`detect_framed`]'s grids, as the rules alone give them.
fn detect_framed_rules(
    rules: &[Rule],
    fills: &[crate::rule::Fill],
    glyphs: &[crate::glyph::Glyph],
    page: usize,
    frames: &[BBox],
) -> Vec<Grid> {
    let whole = detect_group(rules, fills, glyphs, page, frames);
    if whole.is_empty() {
        // The page-wide reading finds nothing where two tables on the page
        // rule their columns apart at different places: across the page no
        // edge but the outer two has the lines' support, and a one-column
        // grid with verticals inside it is no table. Read in touching
        // components, a table ruled with horizontals only falls apart into
        // lines, none touching the next; its lines ruled alike are read
        // together.
        let mut out = detect_components(rules, fills, glyphs, page, frames);
        for stack in ruled_stacks(rules, fills) {
            for g in detect_group_trimmed(&stack, fills, glyphs, page, frames, false) {
                // Rows and columns: a chart's gridlines, broken alike where
                // its bars stand, leave one column of tick labels.
                if g.rows() < 2 || g.cols() < 2 || !rules_its_rows(&g, glyphs) {
                    continue;
                }
                // A band the components read inside it on its own (a shaded
                // header whose separators touch its rule) is one of its rows.
                let band_within = |o: &Grid| {
                    o.rows() == 1
                        && o.bbox.x0 >= g.bbox.x0 - EDGE_TOLERANCE
                        && o.bbox.x1 <= g.bbox.x1 + EDGE_TOLERANCE
                        && o.bbox.y0 >= g.bbox.y0 - EDGE_TOLERANCE
                        && o.bbox.y1 <= g.bbox.y1 + EDGE_TOLERANCE
                };
                if out
                    .iter()
                    .filter(|o| overlaps(&o.bbox, &g.bbox))
                    .all(|o| band_within(o))
                {
                    out.retain(|o| !band_within(o));
                    out.push(g);
                }
            }
        }
        out.sort_by(|a, b| a.bbox.y0.total_cmp(&b.bbox.y0));
        return out;
    }
    let boxed = detect_boxes(rules, fills, glyphs, page, frames);
    if boxed.grids.is_empty() {
        return whole;
    }
    // The page-wide reading stands where no box is. Where it ran over a
    // box, the box is read on its own, and the rules no box took are read
    // again within that grid's bounds: what the page-wide grid had folded
    // in beside the box, a table ruled with horizontals only.
    // A box where the page-wide reading found no table is one only as a
    // table would be: rows and columns, mostly filled. A form's strip of
    // field boxes, a chart's legend and a schematic's outlines are boxes
    // too.
    //
    // Where the two readings find the same grid, the page-wide one stands.
    let same = |a: &Grid, b: &Grid| {
        a.xs.len() == b.xs.len()
            && a.ys.len() == b.ys.len()
            && a.xs
                .iter()
                .zip(&b.xs)
                .all(|(p, q)| (p - q).abs() <= EDGE_TOLERANCE)
            && a.ys
                .iter()
                .zip(&b.ys)
                .all(|(p, q)| (p - q).abs() <= EDGE_TOLERANCE)
    };
    let mut out: Vec<Grid> = boxed
        .grids
        .into_iter()
        .filter(|b| whole.iter().any(|w| overlaps(&w.bbox, &b.bbox)) || reads_as_table(b, glyphs))
        .map(|b| whole.iter().find(|w| same(w, &b)).cloned().unwrap_or(b))
        .collect();
    for w in whole {
        if !out.iter().any(|b| overlaps(&b.bbox, &w.bbox)) {
            out.push(w);
            continue;
        }
        let within = |r: &Rule| {
            r.bbox.x0 >= w.bbox.x0 - EDGE_TOLERANCE
                && r.bbox.x1 <= w.bbox.x1 + EDGE_TOLERANCE
                && r.bbox.y0 >= w.bbox.y0 - EDGE_TOLERANCE
                && r.bbox.y1 <= w.bbox.y1 + EDGE_TOLERANCE
        };
        let rest: Vec<Rule> = rules
            .iter()
            .zip(&boxed.used)
            .filter(|(r, used)| !**used && within(r))
            .map(|(r, _)| *r)
            .collect();
        for mut g in detect_group(&rest, fills, glyphs, page, frames) {
            // Its rows lying inside a box already read are that box's: a
            // shaded header ruled into cells of its own, the band the rows
            // under it took for their top.
            let in_a_box = |g: &Grid, lo: f64, hi: f64| {
                let band = BBox {
                    x0: g.bbox.x0,
                    y0: lo,
                    x1: g.bbox.x1,
                    y1: hi,
                };
                out.iter().any(|b| overlaps(&b.bbox, &band))
            };
            while g.ys.len() > MIN_GRID_LINES && in_a_box(&g, g.ys[0], g.ys[1]) {
                g.ys.remove(0);
            }
            while g.ys.len() > MIN_GRID_LINES
                && in_a_box(&g, g.ys[g.ys.len() - 2], g.ys[g.ys.len() - 1])
            {
                g.ys.pop();
            }
            g.bbox.y0 = g.ys[0];
            g.bbox.y1 = g.ys[g.ys.len() - 1];
            if !out.iter().any(|b| overlaps(&b.bbox, &g.bbox)) {
                out.push(g);
            }
        }
    }
    out.sort_by(|a, b| {
        a.bbox
            .y0
            .partial_cmp(&b.bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.bbox
                    .x0
                    .partial_cmp(&b.bbox.x0)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    out
}

/// A grid found on its own holds a table: three rows or more, two columns
/// or more, and text in half its cells, counting a merged cell once.
fn reads_as_table(g: &Grid, glyphs: &[crate::glyph::Glyph]) -> bool {
    let mut g = g.clone();
    g.trim_to_content(glyphs);
    if g.rows() < 3 || g.cols() < 2 {
        return false;
    }
    let cells = g.cell_texts(glyphs);
    let held = cells.iter().filter(|c| !c.trim().is_empty()).count();
    held * 2 >= cells.len()
}

/// A grid whose columns are prose set side by side, or a box laid out
/// around paragraphs: not a table.
///
/// Prose runs on from one line into the next, and a table's cells rarely
/// do: two or three columns of body text, cut into one row per line, give
/// cells that end mid-sentence (`built into the`, `un-`) over cells that
/// carry on in lowercase (`AM Select dishwasher`, `restricted openings`).
/// A column runs on where its cells are lines of words and a third of them
/// carry on into the next (a feature list starts as many items as it
/// carries lines on), and a grid is prose where
/// half the columns holding text run on: a table's figures and codes do
/// not, whatever its one description column does. A ruled box whose one
/// row under its labels holds paragraphs side by side is a page laid out
/// in columns.
pub fn reads_as_prose(g: &Grid, glyphs: &[crate::glyph::Glyph]) -> bool {
    let cells = g.cell_texts(glyphs);
    let (rows, cols) = (g.rows(), g.cols());
    if rows == 0 || cols == 0 {
        return false;
    }
    let cell = |r: usize, c: usize| cells[r * cols + c].trim();
    let runs_on = |upper: &str, lower: &str| {
        let ends_open = upper
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_lowercase() || ch == ',')
            || upper
                .strip_suffix('-')
                .and_then(|s| s.chars().next_back())
                .is_some_and(char::is_alphabetic);
        // A lowercase word carries on; a list's `a)` or `ii.` starts an item.
        let word = lower.split_whitespace().next().unwrap_or("");
        let letters = word.trim_end_matches([',', ';', ':', '.']);
        ends_open
            && letters.chars().count() >= 2
            && letters.chars().all(char::is_alphabetic)
            && letters.chars().next().is_some_and(char::is_lowercase)
    };
    // Rows a line each: a ruled table's cells wrap inside themselves, and
    // what one row says of the next is nothing about prose.
    let homes = g.glyph_cells(glyphs);
    let mut bases: Vec<Vec<f64>> = vec![Vec::new(); rows * cols];
    for (gl, home) in glyphs.iter().zip(&homes) {
        if let Some(i) = home {
            if gl.bbox.is_some() && !gl.text.trim().is_empty() {
                bases[*i].push(gl.origin.1);
            }
        }
    }
    let lines_in = |b: &Vec<f64>| {
        let mut b = b.clone();
        b.sort_by(|p, q| p.total_cmp(q));
        coalesce(b, 2.0).len()
    };
    let filled: Vec<usize> = bases
        .iter()
        .filter(|b| !b.is_empty())
        .map(lines_in)
        .collect();
    let one_line = filled.iter().filter(|n| **n == 1).count() * 5 >= filled.len() * 4;
    let mut held = 0;
    let mut prose = 0;
    for c in 0..cols {
        if !(0..rows).any(|r| !cell(r, c).is_empty()) {
            continue;
        }
        held += 1;
        let pairs: Vec<bool> = (0..rows.saturating_sub(1))
            .filter(|&r| !cell(r, c).is_empty() && !cell(r + 1, c).is_empty())
            .map(|r| runs_on(cell(r, c), cell(r + 1, c)))
            .collect();
        // Words to a cell: prose fills its line, a table's values are a
        // word or two.
        let texts: Vec<&str> = (0..rows)
            .map(|r| cell(r, c))
            .filter(|t| !t.is_empty())
            .collect();
        let words = texts
            .iter()
            .map(|t| t.split_whitespace().count())
            .sum::<usize>() as f64
            / texts.len().max(1) as f64;
        if pairs.len() >= 3
            && words >= PROSE_WORDS
            && pairs.iter().filter(|p| **p).count() * 3 >= pairs.len()
        {
            prose += 1;
        }
    }
    if one_line && held > 0 && prose * 2 >= held && prose >= 1 {
        return true;
    }
    // A box laid out around paragraphs: one row under its labels, and in
    // it paragraphs side by side. One long cell beside a label is a table's
    // row whose value runs long (a list of recommendations).
    rows <= 2
        && (0..cols)
            .filter(|&c| cell(rows - 1, c).split_whitespace().count() >= PAGE_LAYOUT_WORDS)
            .count()
            >= 2
}

/// Columns read from alignment that cut words: a boundary between two
/// letters set a hair apart on one line, in two rows or more and a quarter
/// of them. Justified prose lines its words up into columns of its own
/// (`Only h | osted con | ferenc`); a table's columns part values, not
/// words.
pub fn cuts_words(g: &Grid, glyphs: &[crate::glyph::Glyph]) -> bool {
    if g.cols() < 2 || g.rows() == 0 {
        return false;
    }
    let inner = &g.xs[1..g.xs.len() - 1];
    let mut cut_rows = 0;
    for w in g.ys.windows(2) {
        let row: Vec<&crate::glyph::Glyph> = glyphs
            .iter()
            .filter(|gl| {
                gl.bbox.is_some()
                    && gl.is_horizontal()
                    && gl.text.chars().all(char::is_alphanumeric)
                    && !gl.text.is_empty()
                    && gl.origin.1 > w[0]
                    && gl.origin.1 <= w[1]
                    && gl.origin.0 >= g.bbox.x0
                    && gl.origin.0 <= g.bbox.x1
            })
            .collect();
        let cut = inner.iter().any(|&x| {
            row.iter().any(|a| {
                let ab = a.bbox.unwrap_or_default();
                ab.x0 < x
                    && row.iter().any(|b| {
                        let bb = b.bbox.unwrap_or_default();
                        bb.x1 > x
                            && bb.x0 >= ab.x1 - 0.5
                            && bb.x0 - ab.x1 < 0.15 * a.font_size as f64
                            && (b.origin.1 - a.origin.1).abs() < 0.2 * a.font_size as f64
                            && (ab.x1 > x || bb.x0 < x || (ab.x1 <= x && bb.x0 >= x))
                    })
            })
        });
        if cut {
            cut_rows += 1;
        }
    }
    cut_rows >= 2 && cut_rows * 4 >= g.rows()
}

/// Words a cell holds that make it a page's paragraph, not a table's value.
const PAGE_LAYOUT_WORDS: usize = 60;

/// Words a column's cells hold on average when they are lines of prose.
const PROSE_WORDS: f64 = 4.0;

/// Two boxes share some area.
fn overlaps(a: &BBox, b: &BBox) -> bool {
    a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
}

/// The grids a page's boxes draw, and which rules they took.
struct Boxes {
    grids: Vec<Grid>,
    used: Vec<bool>,
}

/// The tables a page draws as boxes: each set of touching rules read on its
/// own, with the loose rules inside it, where it rules a grid by itself.
///
/// A page's rules are not one grid. Read all at once, two tables side by
/// side share their row lines, a chart's axis or the page's column divider
/// that lines up with a table's rows becomes one of its columns, and a small
/// table's rules fall short of the page-wide coverage a grid line needs.
/// A box is its own evidence: the rules that touch one another are one
/// drawing. Its loose rules — interior rows drawn short of the verticals —
/// lie inside it. Inner boxes are read first, so a frame round the page or
/// round a panel takes only the rules no box inside it has.
///
/// Rules no box claims are left to the page-wide reading: a table ruled with
/// horizontals only has no touching rules at all.
fn detect_boxes(
    rules: &[Rule],
    fills: &[crate::rule::Fill],
    glyphs: &[crate::glyph::Glyph],
    page: usize,
    frames: &[BBox],
) -> Boxes {
    let comps = component_indices(rules);
    let mut used = vec![false; rules.len()];
    let mut grids: Vec<Grid> = Vec::new();
    let bbox_of = |idx: &[usize]| {
        idx.iter().fold(
            BBox {
                x0: f64::MAX,
                y0: f64::MAX,
                x1: f64::MIN,
                y1: f64::MIN,
            },
            |b, &i| BBox {
                x0: b.x0.min(rules[i].bbox.x0),
                y0: b.y0.min(rules[i].bbox.y0),
                x1: b.x1.max(rules[i].bbox.x1),
                y1: b.y1.max(rules[i].bbox.y1),
            },
        )
    };
    let loose: Vec<bool> = {
        let mut l = vec![false; rules.len()];
        for c in &comps {
            if c.len() < MIN_GRID_LINES {
                c.iter().for_each(|&i| l[i] = true);
            }
        }
        l
    };
    let comps: Vec<Vec<usize>> = comps
        .into_iter()
        .filter(|c| c.len() >= MIN_GRID_LINES)
        .collect();
    let comps = bound_by_bands(comps, fills, &bbox_of);
    let mut boxes: Vec<(&Vec<usize>, BBox)> = comps.iter().map(|c| (c, bbox_of(c))).collect();
    let area = |b: &BBox| (b.x1 - b.x0) * (b.y1 - b.y0);
    boxes.sort_by(|a, b| {
        area(&a.1)
            .partial_cmp(&area(&b.1))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (comp, bb) in boxes {
        let inside = |r: &Rule| {
            r.bbox.x0 >= bb.x0 - EDGE_TOLERANCE
                && r.bbox.x1 <= bb.x1 + EDGE_TOLERANCE
                && r.bbox.y0 >= bb.y0 - EDGE_TOLERANCE
                && r.bbox.y1 <= bb.y1 + EDGE_TOLERANCE
        };
        // A loose rule is the box's own where it runs between two of the
        // box's lines, stopping short of each by a hair: a row drawn short
        // of the verticals. A rule ending in the open is something else on
        // the box's ground, a link's underline under a cell's words.
        let lines_across = |o: Orientation| -> Vec<f64> {
            let mut at: Vec<f64> = comp
                .iter()
                .map(|&i| &rules[i])
                .filter(|r| r.orientation == o)
                .map(|r| r.axis_pos())
                .collect();
            match o {
                Orientation::Vertical => at.extend([bb.x0, bb.x1]),
                Orientation::Horizontal => at.extend([bb.y0, bb.y1]),
            }
            at
        };
        let (verticals, horizontals) = (
            lines_across(Orientation::Vertical),
            lines_across(Orientation::Horizontal),
        );
        let meets =
            |at: &[f64], end: f64| at.iter().any(|a| (a - end).abs() <= 2.0 * EDGE_TOLERANCE);
        let spans = |r: &Rule| match r.orientation {
            Orientation::Horizontal => meets(&verticals, r.bbox.x0) && meets(&verticals, r.bbox.x1),
            Orientation::Vertical => {
                meets(&horizontals, r.bbox.y0) && meets(&horizontals, r.bbox.y1)
            }
        };
        let mut idx: Vec<usize> = comp.clone();
        idx.extend(
            (0..rules.len())
                .filter(|&i| loose[i] && !used[i] && inside(&rules[i]) && spans(&rules[i])),
        );
        let rs: Vec<Rule> = idx.iter().map(|&i| rules[i]).collect();
        let got = detect_group(&rs, fills, glyphs, page, frames);
        // A box ruling one column is as often a sidebar parted by a rule
        // as a list, and is left to the page-wide reading's tests for one.
        if got.iter().all(|g| g.cols() < 2)
            || got
                .iter()
                .any(|g| grids.iter().any(|t| overlaps(&t.bbox, &g.bbox)))
        {
            continue;
        }
        idx.iter().for_each(|&i| used[i] = true);
        grids.extend(got);
    }
    Boxes { grids, used }
}

/// Boxes set side by side that one shaded band runs across are one table.
///
/// A table can leave a column unruled — a label or a value spanning every
/// row, standing in the open between two ruled blocks — and its rules then
/// touch in two sets with the open column between them. The shading of the
/// header band, run across the whole table, says they are one. Two tables
/// side by side shade their own headers. A band is a row's height, not a
/// panel behind both boxes.
fn bound_by_bands(
    comps: Vec<Vec<usize>>,
    fills: &[crate::rule::Fill],
    bbox_of: &dyn Fn(&[usize]) -> BBox,
) -> Vec<Vec<usize>> {
    let boxes: Vec<BBox> = comps.iter().map(|c| bbox_of(c)).collect();
    let mut owner: Vec<usize> = (0..comps.len()).collect();
    fn root(owner: &mut [usize], mut i: usize) -> usize {
        while owner[i] != i {
            owner[i] = owner[owner[i]];
            i = owner[i];
        }
        i
    }
    for a in 0..boxes.len() {
        for b in 0..boxes.len() {
            let (l, r) = (&boxes[a], &boxes[b]);
            if a == b || l.x1 >= r.x0 {
                continue;
            }
            let height = |x: &BBox| x.y1 - x.y0;
            let shared = l.y1.min(r.y1) - l.y0.max(r.y0);
            if shared < 0.5 * height(l).min(height(r)) {
                continue;
            }
            let banded = fills.iter().any(|f| {
                let fb = &f.bbox;
                fb.y1 - fb.y0 <= 0.5 * height(l).min(height(r))
                    && fb.x0 < l.x1 - EDGE_TOLERANCE
                    && fb.x1 > r.x0 + EDGE_TOLERANCE
                    && fb.y0 < l.y1.min(r.y1)
                    && fb.y1 > l.y0.max(r.y0)
            });
            if banded {
                let (ra, rb) = (root(&mut owner, a), root(&mut owner, b));
                owner[ra.max(rb)] = ra.min(rb);
            }
        }
    }
    let mut joined: Vec<Vec<usize>> = vec![Vec::new(); comps.len()];
    for (i, c) in comps.into_iter().enumerate() {
        let r = root(&mut owner, i);
        joined[r].extend(c);
    }
    joined
        .into_iter()
        .filter(|c| !c.is_empty())
        .map(|mut c| {
            c.sort_unstable();
            c
        })
        .collect()
}

/// Where a set of touching rules is two boxes stacked a hair apart: the
/// middle of the gap between them.
///
/// A spec sheet sets its option tables one under another, each boxed with
/// its own title band and its own columns (`Trims` over two, `Housings`
/// under it over two others), two or three points apart. That is close
/// enough for their rules to touch, and read as one drawing the two boxes'
/// dividers made three columns neither has. The gap says they are two: a
/// line across the whole set on either side of it, less than a row apart,
/// and no vertical crossing between them, where every rule between a
/// box's doubled top line crosses it. Each side is closed at both ends and
/// rules a column the other does not. A header ruled off by a double line
/// with the verticals broken across it rules the same columns as the rows
/// under it, or fewer, and stays one table.
fn stacked_boxes_gap(rs: &[&Rule]) -> Option<f64> {
    let (hs, vs): (Vec<&Rule>, Vec<&Rule>) = rs
        .iter()
        .copied()
        .partition(|r| r.orientation == Orientation::Horizontal);
    if hs.len() < 4 || vs.len() < 4 {
        return None;
    }
    let x0 = rs.iter().map(|r| r.bbox.x0).fold(f64::INFINITY, f64::min);
    let x1 = rs
        .iter()
        .map(|r| r.bbox.x1)
        .fold(f64::NEG_INFINITY, f64::max);
    let width = x1 - x0;
    let mut ys: Vec<f64> = hs.iter().map(|r| r.axis_pos()).collect();
    ys.sort_by(f64::total_cmp);
    ys.dedup_by(|a, b| (*a - *b).abs() <= 0.5);
    // Lines run across the whole set, drawn in pieces or not.
    let across: Vec<f64> = ys
        .into_iter()
        .filter(|&y| {
            let pieces = hs
                .iter()
                .filter(|r| (r.axis_pos() - y).abs() <= 0.5)
                .map(|r| (r.bbox.x0, r.bbox.x1));
            span_coverage(pieces, x0, x1) >= 0.95 * width
        })
        .collect();
    for w in across.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b - a >= MIN_CELL_EXTENT
            || vs
                .iter()
                .any(|v| v.bbox.y0 < b - 1.0 && v.bbox.y1 > a + 1.0)
        {
            continue;
        }
        let mid = (a + b) / 2.0;
        let side = |above: bool| -> Vec<f64> {
            vs.iter()
                .filter(|v| ((v.bbox.y0 + v.bbox.y1) / 2.0 < mid) == above)
                .map(|v| v.axis_pos())
                .collect()
        };
        let (up, down) = (side(true), side(false));
        let closed = |xs: &[f64]| {
            xs.iter().any(|x| (x - x0).abs() <= EDGE_TOLERANCE)
                && xs.iter().any(|x| (x - x1).abs() <= EDGE_TOLERANCE)
        };
        let inner = |xs: &[f64]| -> Vec<f64> {
            xs.iter()
                .copied()
                .filter(|&x| x > x0 + EDGE_TOLERANCE && x < x1 - EDGE_TOLERANCE)
                .collect()
        };
        let own = |p: &[f64], q: &[f64]| {
            p.iter()
                .any(|x| q.iter().all(|y| (x - y).abs() > EDGE_TOLERANCE))
        };
        let (iu, id) = (inner(&up), inner(&down));
        if closed(&up) && closed(&down) && own(&iu, &id) && own(&id, &iu) {
            return Some(mid);
        }
    }
    None
}

/// `comp`, a set of touching rules, parted into the boxes stacked in it
/// (see [`stacked_boxes_gap`]).
fn split_stacked_boxes<T: Copy>(comp: Vec<T>, rule: impl Fn(T) -> Rule) -> Vec<Vec<T>> {
    let mut out = Vec::new();
    let mut todo = vec![comp];
    while let Some(c) = todo.pop() {
        let rs: Vec<Rule> = c.iter().map(|&t| rule(t)).collect();
        let refs: Vec<&Rule> = rs.iter().collect();
        match stacked_boxes_gap(&refs) {
            Some(mid) => {
                let (up, down): (Vec<T>, Vec<T>) = c.into_iter().partition(|&t| {
                    let b = rule(t).bbox;
                    (b.y0 + b.y1) / 2.0 < mid
                });
                todo.push(up);
                todo.push(down);
            }
            None => out.push(c),
        }
    }
    out
}

/// The rules of one component, filed by the cells of a coarse grid their
/// reach covers (each rule's box widened by [`EDGE_TOLERANCE`] and a cell),
/// so that whether any of them touches a rule is asked of the few filed
/// near it, not of all of them. A chart drawn in strokes can put thousands
/// of rules in one component. A component of [`FEW_PIECES`] boxes or fewer
/// is asked box by box, unfiled.
struct TouchGrid {
    /// The component's distinct boxes: whether a rule touches the component
    /// turns on its rules' boxes alone, and a chart's strokes repeat theirs.
    boxes: Vec<BBox>,
    filed: rustc_hash::FxHashSet<[u64; 4]>,
    cells: rustc_hash::FxHashMap<(i64, i64), Vec<BBox>>,
    /// Rules too long to file cell by cell, asked every time.
    wide: Vec<BBox>,
}

fn box_bits(b: &BBox) -> [u64; 4] {
    [
        b.x0.to_bits(),
        b.y0.to_bits(),
        b.x1.to_bits(),
        b.y1.to_bits(),
    ]
}

impl TouchGrid {
    const CELL: f64 = 16.0;
    const MAX_CELLS: i64 = 4096;

    fn span(lo: f64, hi: f64) -> (i64, i64) {
        (
            ((lo - EDGE_TOLERANCE) / Self::CELL).floor() as i64 - 1,
            ((hi + EDGE_TOLERANCE) / Self::CELL).floor() as i64 + 1,
        )
    }

    fn new() -> Self {
        TouchGrid {
            boxes: Vec::new(),
            filed: Default::default(),
            cells: Default::default(),
            wide: Vec::new(),
        }
    }

    fn filing(&self) -> bool {
        self.boxes.len() > FEW_PIECES
    }

    fn insert(&mut self, b: &BBox) {
        if self.filing() {
            if self.filed.insert(box_bits(b)) {
                self.boxes.push(*b);
                self.file(b);
            }
            return;
        }
        if self.boxes.iter().any(|o| box_bits(o) == box_bits(b)) {
            return;
        }
        self.boxes.push(*b);
        if self.filing() {
            for o in self.boxes.clone() {
                self.filed.insert(box_bits(&o));
                self.file(&o);
            }
        }
    }

    fn file(&mut self, b: &BBox) {
        let ((x0, x1), (y0, y1)) = (Self::span(b.x0, b.x1), Self::span(b.y0, b.y1));
        if (x1 - x0 + 1).saturating_mul(y1 - y0 + 1) > Self::MAX_CELLS {
            self.wide.push(*b);
            return;
        }
        for x in x0..=x1 {
            for y in y0..=y1 {
                self.cells.entry((x, y)).or_default().push(*b);
            }
        }
    }

    /// Whether any of the component's rules touches `c`: the test of
    /// [`component_indices`]. Filed, it is asked of the rules filed in the
    /// cells `c` covers: a rule touching `c` reaches a point of `c`'s box,
    /// so it is filed in a cell `c` covers.
    fn touches(&self, c: &BBox) -> bool {
        let touch = |r: &BBox| {
            r.x1 + EDGE_TOLERANCE >= c.x0
                && c.x1 + EDGE_TOLERANCE >= r.x0
                && r.y1 + EDGE_TOLERANCE >= c.y0
                && c.y1 + EDGE_TOLERANCE >= r.y0
        };
        if !self.filing() {
            return self.boxes.iter().any(touch);
        }
        if self.wide.iter().any(touch) {
            return true;
        }
        let ((x0, x1), (y0, y1)) = (Self::span(c.x0, c.x1), Self::span(c.y0, c.y1));
        if (x1 - x0 + 1).saturating_mul(y1 - y0 + 1) > Self::MAX_CELLS {
            return self.boxes.iter().any(touch);
        }
        (x0..=x1).any(|x| {
            (y0..=y1).any(|y| {
                self.cells
                    .get(&(x, y))
                    .is_some_and(|ks| ks.iter().any(touch))
            })
        })
    }
}

/// The page's rules in touching sets, as indices.
fn component_indices(rules: &[Rule]) -> Vec<Vec<usize>> {
    let mut remaining: Vec<usize> = (0..rules.len()).collect();
    let mut components: Vec<Vec<usize>> = Vec::new();
    while let Some(seed) = remaining.pop() {
        let mut component = vec![seed];
        let mut grid = TouchGrid::new();
        grid.insert(&rules[seed].bbox);
        let mut changed = true;
        while changed {
            changed = false;
            let mut i = 0;
            while i < remaining.len() {
                let candidate = rules[remaining[i]];
                if grid.touches(&candidate.bbox) {
                    grid.insert(&candidate.bbox);
                    component.push(remaining[i]);
                    remaining.swap_remove(i);
                    changed = true;
                } else {
                    i += 1;
                }
            }
        }
        component.sort_unstable();
        components.extend(split_stacked_boxes(component, |i| rules[i]));
    }
    components
}

/// A table region whose detected structure should not be trusted.
#[derive(Debug, Clone)]
pub struct SuspectTable {
    pub page: usize,
    /// The region the fragments cover — what a model tier would be handed.
    pub bbox: BBox,
    /// Column count of each fragment, in reading order. Disagreement among
    /// them is the evidence for [`SuspectReason::Fragmented`].
    pub fragment_cols: Vec<usize>,
    pub reason: SuspectReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspectReason {
    /// One contiguous ruled region reported as fragments that disagree on
    /// how many columns it has.
    Fragmented,
    /// A column of single values holds cells with several crammed together —
    /// rows that were merged.
    MergedRows,
}

/// A table's header set as a shaded band at its top: shading one height
/// under a rule across it, divided into cells by verticals that stand on
/// that rule and end at the band's foot. The body under it is often ruled by nothing, and read from
/// its text or its alignment; the band is its header row ([`take_header_bands`]).
#[derive(Debug, Clone)]
pub(crate) struct HeaderBand {
    pub bbox: BBox,
    pub cells: Vec<BBox>,
}

/// The page's header bands. Shading one height, run together end to end,
/// is a band; the verticals standing on its top rule and ending at its foot
/// divide it into its cells, however its shading was drawn (a cell at a
/// time, or two cells under one fill).
pub(crate) fn header_bands(rules: &[Rule], fills: &[crate::rule::Fill]) -> Vec<HeaderBand> {
    let t = EDGE_TOLERANCE;
    // A band hangs under a rule and is divided by verticals: a page with
    // no rule of either kind has none, however much it shades.
    let mut tops: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.orientation == Orientation::Horizontal)
        .collect();
    let verticals: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.orientation == Orientation::Vertical)
        .collect();
    if fills.is_empty() || tops.is_empty() || verticals.is_empty() {
        return Vec::new();
    }
    tops.sort_by(|a, b| a.axis_pos().total_cmp(&b.axis_pos()));
    let mut boxes: Vec<BBox> = fills.iter().map(|f| f.bbox).collect();
    boxes.sort_by(|a, b| a.y0.total_cmp(&b.y0).then(a.x0.total_cmp(&b.x0)));
    let touch = |r: &BBox, b: &BBox| {
        (r.y0 - b.y0).abs() <= t && (r.y1 - b.y1).abs() <= t && b.x0 <= r.x1 + t && b.x1 >= r.x0 - t
    };
    // Runs of shading one height, joined where they meet or overlap. The
    // shading comes in order of its tops, so a run's top is its first
    // piece's and the runs stand in order of their tops: a piece is only
    // looked for among the runs whose tops lie within the tolerance of its
    // own. A chart's page draws thousands of fills.
    let mut runs: Vec<BBox> = Vec::new();
    let mut from = 0;
    for b in boxes {
        while from < runs.len() && runs[from].y0 < b.y0 - t {
            from += 1;
        }
        match runs[from..].iter_mut().find(|r| touch(r, &b)) {
            Some(r) => *r = r.union(&b),
            None => runs.push(b),
        }
    }
    // Runs a later piece of shading joined, merged until none touch, the
    // first touching pair first. Runs still stand in order of their tops,
    // and a merge keeps the earlier run's top, so each is compared only with
    // the runs after it whose tops lie within the tolerance. After a merge
    // only the grown run can touch a run it did not before, one whose top
    // lies within the tolerance of its own: the next pair is looked for from
    // the first of those, not from the page's first run again.
    let mut start = 0;
    loop {
        let mut hit = None;
        'scan: for i in start..runs.len() {
            for j in i + 1..runs.len() {
                if runs[j].y0 > runs[i].y0 + t {
                    break;
                }
                if touch(&runs[i], &runs[j]) {
                    hit = Some((i, j));
                    break 'scan;
                }
            }
        }
        let Some((i, j)) = hit else { break };
        let b = runs.remove(j);
        runs[i] = runs[i].union(&b);
        let y0 = runs[i].y0;
        start = runs.partition_point(|r| r.y0 < y0 - t);
    }
    let mut out = Vec::new();
    for bbox in runs {
        let lo = tops.partition_point(|r| r.axis_pos() < bbox.y0 - t);
        let top = tops[lo..]
            .iter()
            .take_while(|r| r.axis_pos() <= bbox.y0 + t)
            .any(|r| r.bbox.x0 <= bbox.x0 + t && r.bbox.x1 >= bbox.x1 - t);
        if !top {
            continue;
        }
        let mut dividers: Vec<f64> = verticals
            .iter()
            .filter(|r| {
                r.axis_pos() > bbox.x0 + t
                    && r.axis_pos() < bbox.x1 - t
                    && (r.bbox.y0 - bbox.y0).abs() <= t
                    && (r.bbox.y1 - bbox.y1).abs() <= t
            })
            .map(|r| r.axis_pos())
            .collect();
        dividers.sort_by(f64::total_cmp);
        dividers.dedup_by(|a, b| (*a - *b).abs() <= t);
        if dividers.is_empty() {
            continue;
        }
        let mut edges = vec![bbox.x0];
        edges.extend(dividers);
        edges.push(bbox.x1);
        let cells = edges
            .windows(2)
            .map(|w| BBox {
                x0: w[0],
                y0: bbox.y0,
                x1: w[1],
                y1: bbox.y1,
            })
            .collect();
        out.push(HeaderBand { bbox, cells });
    }
    out
}

/// Take each header band over the table read under it as its header row,
/// one cell a column: the band's cells line up with the table's columns
/// one for one, the table's first rows below the band start within the
/// band's own height of its foot, and nothing else of the page's text
/// stands between. Rows the reading made of the band's own lines (its
/// labels' second lines, `(billions of dollars)` over `(percent)`) are the
/// band's; a table read from the band's lines alone, set over nothing, is
/// dropped. Returns which grids to keep.
pub(crate) fn take_header_bands(
    grids: &mut [Grid],
    bands: &[HeaderBand],
    glyphs: &[crate::glyph::Glyph],
) -> Vec<bool> {
    let t = EDGE_TOLERANCE;
    let mut keep = vec![true; grids.len()];
    let centre = |g: &crate::glyph::Glyph| {
        g.bbox
            .filter(|_| g.is_horizontal() && !g.text.trim().is_empty())
            .map(|b| ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0))
    };
    let inside = |b: &BBox, (x, y): (f64, f64)| x >= b.x0 && x <= b.x1 && y >= b.y0 && y <= b.y1;
    for band in bands {
        let Some(gi) = (0..grids.len()).find(|&gi| {
            let g = &grids[gi];
            let cols = g.cols();
            let lined_up = cols == band.cells.len()
                && (0..cols).all(|c| {
                    let mid = (g.xs[c] + g.xs[c + 1]) / 2.0;
                    mid >= band.cells[c].x0 && mid <= band.cells[c].x1
                });
            let under = g.bbox.y0 >= band.bbox.y0 - t
                && g.bbox.y1 > band.bbox.y1 + t
                && g.bbox.y0 <= band.bbox.y1 + band.bbox.height();
            keep[gi] && lined_up && under && g.ys[0] > band.bbox.y0 + t
        }) else {
            continue;
        };
        // Nothing of the page's text between the band and the table, but
        // what the table holds.
        let homes = grids[gi].glyph_cells(glyphs);
        let top_of_body = (0..glyphs.len())
            .filter(|&i| homes[i].is_some())
            .filter_map(|i| centre(&glyphs[i]))
            .filter(|&(_, y)| y > band.bbox.y1)
            .map(|(_, y)| y)
            .fold(f64::MAX, f64::min);
        // Lines between the band and the table's first row are its rows
        // too where they are labels alone, in the band's first cell: a
        // section's label (`Assets`) over a group's (`Total securities`).
        // Anything else between, and the band is not this table's.
        let stray: Vec<usize> = (0..glyphs.len())
            .filter(|&i| {
                homes[i].is_none()
                    && centre(&glyphs[i]).is_some_and(|(x, y)| {
                        x > band.bbox.x0 && x < band.bbox.x1 && y > band.bbox.y1 && y < top_of_body
                    })
            })
            .collect();
        let labels_only = stray
            .iter()
            .all(|&i| centre(&glyphs[i]).is_some_and(|(x, _)| x <= band.cells[0].x1));
        if top_of_body == f64::MAX || !labels_only {
            continue;
        }
        // The label lines, top to bottom: (top, bottom) of each baseline's ink.
        let mut label_lines: Vec<(f64, f64, f64)> = Vec::new();
        for &i in &stray {
            let (Some(b), y) = (glyphs[i].bbox, glyphs[i].origin.1) else {
                continue;
            };
            let fs = (glyphs[i].font_size as f64).max(1.0);
            match label_lines.iter_mut().find(|l| (l.0 - y).abs() < 0.3 * fs) {
                Some(l) => {
                    l.1 = l.1.min(b.y0);
                    l.2 = l.2.max(b.y1);
                }
                None => label_lines.push((y, b.y0, b.y1)),
            }
        }
        label_lines.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Each label line's glyphs, all of them, spaces too.
        let label_glyphs: Vec<Vec<usize>> = label_lines
            .iter()
            .map(|l| {
                (0..glyphs.len())
                    .filter(|&i| {
                        homes[i].is_none()
                            && (glyphs[i].origin.1 - l.0).abs()
                                < 0.3 * (glyphs[i].font_size as f64).max(1.0)
                            && glyphs[i].origin.0 >= band.cells[0].x0
                            && glyphs[i].origin.0 <= band.cells[0].x1
                    })
                    .collect()
            })
            .collect();
        // Each cell's glyphs, its inked ones by their centres and its spaces
        // by where the pen set them: a word space is the cell's own.
        let head: Vec<Vec<usize>> = band
            .cells
            .iter()
            .map(|cell| {
                (0..glyphs.len())
                    .filter(|&i| match centre(&glyphs[i]) {
                        Some(p) => inside(cell, p),
                        None => {
                            glyphs[i].is_horizontal()
                                && !glyphs[i].text.is_empty()
                                && glyphs[i].text.trim().is_empty()
                                && inside(cell, glyphs[i].origin)
                        }
                    })
                    .collect()
            })
            .collect();
        if head.iter().all(Vec::is_empty) {
            continue;
        }
        let g = &mut grids[gi];
        let cols = g.cols();
        if let Some(lay) = g.layout.as_deref() {
            // Rows of the band's own lines, at the reading's top.
            let in_band = |r: usize| {
                let inked: Vec<(f64, f64)> = lay.cells[r * cols..(r + 1) * cols]
                    .iter()
                    .flatten()
                    .filter_map(|&i| centre(&glyphs[i]))
                    .collect();
                !inked.is_empty() && inked.iter().all(|&p| inside(&band.bbox, p))
            };
            let rows = g.rows();
            let k = (0..rows).take_while(|&r| in_band(r)).count();
            if k == rows {
                continue;
            }
            let mut lay = lay.clone();
            let mut cells: Vec<Vec<usize>> = head.clone();
            // The label rows, each a group's label across the table.
            let labels = label_glyphs.len();
            for ids in &label_glyphs {
                cells.push(ids.clone());
                cells.extend(std::iter::repeat_n(Vec::new(), cols - 1));
            }
            cells.extend(lay.cells.drain(k * cols..));
            lay.cells = cells;
            lay.header_rows = 1 + lay.header_rows.saturating_sub(k);
            let shift = |r: usize| r + 1 + labels - k;
            lay.spans = lay
                .spans
                .iter()
                .filter(|s| s.0 >= k)
                .map(|&(r, a, b)| (shift(r), a, b))
                .collect();
            lay.spread = lay
                .spread
                .iter()
                .filter(|s| s.0 >= k)
                .map(|&(r, c)| (shift(r), c))
                .collect();
            lay.downs = lay
                .downs
                .iter()
                .filter(|d| d.0 >= k)
                .map(|&(r0, c, r1)| (shift(r0), c, shift(r1)))
                .collect();
            lay.sub_headers = (1..=labels)
                .chain(
                    lay.sub_headers
                        .iter()
                        .filter(|&&r| r >= k)
                        .map(|&r| shift(r)),
                )
                .collect();
            lay.spans.extend((1..=labels).map(|r| (r, 0, cols)));
            let mut ys = vec![band.bbox.y0, band.bbox.y1];
            for w in label_lines.windows(2) {
                ys.push((w[0].2 + w[1].1) / 2.0);
            }
            if let Some(last) = label_lines.last() {
                ys.push(last.2.max(band.bbox.y1 + 0.1));
            }
            let floor = *ys.last().expect("the band's foot");
            ys.extend(g.ys[k + 1..].iter().map(|&y| y.max(floor + 0.1)));
            g.ys = ys;
            g.layout = Some(std::sync::Arc::new(lay));
            crate::textgrid::claim(g, glyphs);
        } else {
            let mut below: Vec<f64> =
                g.ys.iter()
                    .copied()
                    .filter(|&y| y > band.bbox.y1 + t)
                    .collect();
            // The table's own top edge goes where its label rows above it
            // end, or the band's foot is its first row's top.
            if g.ys[0] > band.bbox.y1 + t && label_lines.is_empty() {
                below.remove(0);
            }
            if below.is_empty() {
                continue;
            }
            let mut ys = vec![band.bbox.y0, band.bbox.y1];
            // A row for each label line, parted midway between their ink.
            for w in label_lines.windows(2) {
                ys.push((w[0].2 + w[1].1) / 2.0);
            }
            ys.extend(below);
            g.ys = ys;
        }
        // The band's cells are the table's columns as drawn, the body's
        // lined up under them: its edges are the table's.
        let mut xs: Vec<f64> = band.cells.iter().map(|c| c.x0).collect();
        xs.push(band.bbox.x1);
        g.xs = xs;
        g.bbox = g.bbox.union(&band.bbox);
        // Readings of the band's lines alone.
        for (oi, o) in grids.iter().enumerate() {
            let (x, y) = ((o.bbox.x0 + o.bbox.x1) / 2.0, (o.bbox.y0 + o.bbox.y1) / 2.0);
            if oi != gi && inside(&band.bbox, (x, y)) && o.bbox.y1 <= band.bbox.y1 + t {
                keep[oi] = false;
            }
        }
    }
    keep
}

/// Fraction of a column's populated cells that must carry a number before it
/// counts as a figures column.
const NUMERIC_COLUMN: f64 = 0.6;

/// Cells in such a column holding this many values or more are merged rows.
const CRAMMED_VALUES: usize = 3;

/// Detect rows the grid merged, by looking for cells that hold several
/// values in a column whose other cells hold one.
///
/// Row boundaries come from ruling and alignment, and where a table rules
/// only some of its rows the rest collapse together — a Federal Reserve
/// statement reported `Interest income Other` against
/// `$ 1,167 38 (218) (25)`, four rows crushed into one cell, while the
/// column counts stayed perfectly consistent. Column fragmentation cannot
/// see this: the grid is the right shape and the wrong height.
///
/// Wrapped prose is not the target and does not qualify — the test is
/// confined to columns whose cells are otherwise single values, which is
/// what a figures column is.
fn is_value_cell(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit()) && !s.chars().any(|c| c.is_alphabetic())
}

fn merged_rows(g: &Grid, cells: &[String]) -> bool {
    let (rows, cols) = (g.rows(), g.cols());
    if rows == 0 || cols == 0 {
        return false;
    }
    let value_count = |s: &str| -> usize {
        s.split_whitespace()
            .filter(|t| t.chars().any(|c| c.is_ascii_digit()))
            .count()
    };
    for c in 0..cols {
        let populated: Vec<&String> = (0..rows)
            .map(|r| &cells[r * cols + c])
            .filter(|s| !s.trim().is_empty())
            .collect();
        if populated.len() < 3 {
            continue;
        }
        // A figures column: most cells are *only* values. Merely containing
        // a number is not enough — a column of prose ("recorded 3 charges of
        // 12.4 and 8.1 in 2024") would otherwise qualify and every wrapped
        // sentence would read as merged rows.
        let numeric = populated.iter().filter(|s| is_value_cell(s)).count();
        if (numeric as f64) < populated.len() as f64 * NUMERIC_COLUMN {
            continue;
        }
        // Mixed arity is the tell: the same column holds rows of one value
        // and rows of several, so the several are rows that were merged.
        let has_single = populated.iter().any(|s| value_count(s) == 1);
        let has_crammed = populated.iter().any(|s| value_count(s) >= CRAMMED_VALUES);
        if has_single && has_crammed {
            return true;
        }
    }
    false
}

/// Rows a one-column grid needs before its text alignment is read for
/// columns; see [`Grid::split_by_alignment`].
const ALIGNED_SPLIT_MIN_ROWS: usize = 3;
/// Narrowest gap, in ems of the grid's text, that separates two columns.
const ALIGNED_SPLIT_GAP_EMS: f64 = 1.5;
/// Narrowest gap in points, whatever the type size.
const ALIGNED_SPLIT_GAP_MIN: f64 = 12.0;
/// Share of text rows whose ink may cross a gap and leave it a gap: a boxed
/// notice or banner spanning the list.
const ALIGNED_SPLIT_CROSSING_PCT: usize = 20;
/// Share of text rows that must set text on both sides of a gap for it to
/// be a column boundary rather than the ragged right of short rows.
const ALIGNED_SPLIT_BOTH_SIDES_PCT: usize = 60;

/// Horizontal overlap, as a fraction of the narrower box, above which two
/// grids sit in the same column band.
const SAME_BAND_OVERLAP: f64 = 0.6;

/// Vertical gap between stacked grids, in multiples of their mean row height,
/// below which they are one contiguous ruled region rather than two tables
/// that merely share a page.
const CONTIGUOUS_GAP: f64 = 2.0;

/// Table regions whose structure is probably wrong, for a caller that can
/// escalate them to a model tier.
///
/// The signal is *fragmentation with disagreeing column counts inside one
/// contiguous ruled region*. A real table does not change from four columns
/// to two and back down its own body; when the detector reports that, it has
/// shredded one table itroduced no vertical rules to bound. On the J&J
/// annual report's regulatory-approval matrix — horizontally ruled, no
/// verticals, four sparse columns of bullet marks — this reports 4x4, 17x2,
/// 10x2 and 5x3 stacked in one band, where the truth is a single 6x12.
///
/// Deliberately *not* "more than one table on a page": pages legitimately
/// carry several, and the same report's income-statement page holds two
/// grids that are each internally consistent at four columns. Only
/// disagreement within one uninterrupted region counts.
pub fn suspect_tables(grids: &[Grid], glyphs: &[crate::glyph::Glyph]) -> Vec<SuspectTable> {
    let mut out = Vec::new();
    let mut used = vec![false; grids.len()];
    for i in 0..grids.len() {
        if used[i] {
            continue;
        }
        let mut region = vec![i];
        used[i] = true;
        // Walk down the page collecting grids that continue this band.
        loop {
            let last = *region.last().unwrap();
            let (lb, lh) = (grids[last].bbox, row_height(&grids[last]));
            let next = (0..grids.len()).find(|&j| {
                if used[j] || grids[j].page != grids[last].page {
                    return false;
                }
                let b = grids[j].bbox;
                let overlap = (lb.x1.min(b.x1) - lb.x0.max(b.x0)).max(0.0);
                let narrower = (lb.x1 - lb.x0).min(b.x1 - b.x0).max(1.0);
                let gap = b.y0 - lb.y1;
                overlap >= narrower * SAME_BAND_OVERLAP
                    && gap >= -lh
                    && gap <= lh.max(row_height(&grids[j])) * CONTIGUOUS_GAP
            });
            match next {
                Some(j) => {
                    used[j] = true;
                    region.push(j);
                }
                None => break,
            }
        }
        if region.len() < 2 {
            continue;
        }
        let cols: Vec<usize> = region.iter().map(|&k| grids[k].cols()).collect();
        if cols.iter().all(|c| *c == cols[0]) {
            continue;
        }
        let bbox = region
            .iter()
            .map(|&k| grids[k].bbox)
            .reduce(|a, b| a.union(&b))
            .unwrap();
        out.push(SuspectTable {
            page: grids[region[0]].page,
            bbox,
            fragment_cols: cols,
            reason: SuspectReason::Fragmented,
        });
    }
    // Merged rows are independent of fragmentation: the grid can be exactly
    // the right shape and still have collapsed several rows into one.
    for (i, g) in grids.iter().enumerate() {
        if used[i] && out.iter().any(|s| s.bbox.intersects(&g.bbox)) {
            continue;
        }
        if merged_rows(g, &g.cell_texts(glyphs)) {
            out.push(SuspectTable {
                page: g.page,
                bbox: g.bbox,
                fragment_cols: vec![g.cols()],
                reason: SuspectReason::MergedRows,
            });
        }
    }
    out
}

fn row_height(g: &Grid) -> f64 {
    let rows = g.rows().max(1) as f64;
    ((g.bbox.y1 - g.bbox.y0) / rows).max(1.0)
}

/// The page-wide hypothesis dilutes evidence when a page carries several
/// *separate* ruled boxes: a boundary must be backed by half the page's wide
/// cross-lines, and with five independent boxes each box's own edges fall
/// under that bar. This is the competing reading — every touching cluster of
/// rules taken as its own table — for a caller that can measure which one
/// actually explains the page (see `document::analyze_with`). Preferring it
/// unconditionally is wrong: on ordinary one-table pages it fragments the
/// grid, measured at TEDS −0.040.
pub fn detect_by_component(rules: &[Rule], page: usize) -> Vec<Grid> {
    detect_components(rules, &[], &[], page, &[])
}

/// [`detect_by_component`], knowing the frames as [`detect_framed`] does.
pub fn detect_by_component_framed(
    rules: &[Rule],
    fills: &[crate::rule::Fill],
    glyphs: &[crate::glyph::Glyph],
    page: usize,
    frames: &[BBox],
) -> Vec<Grid> {
    with_column_bands(
        with_header_band_edges(
            detect_components(rules, fills, glyphs, page, frames),
            fills,
            glyphs,
        ),
        rules,
        glyphs,
    )
}

/// `grids`, each widened to the edge of the header shaded across its first
/// row where the band runs on past its rules over a column of labels (see
/// [`with_header_band_edge`]); not onto another grid.
fn with_header_band_edges(
    mut grids: Vec<Grid>,
    fills: &[crate::rule::Fill],
    glyphs: &[crate::glyph::Glyph],
) -> Vec<Grid> {
    for i in 0..grids.len() {
        let Some(wider) = with_header_band_edge(&grids[i], fills, glyphs) else {
            continue;
        };
        if !grids
            .iter()
            .enumerate()
            .any(|(j, o)| j != i && overlaps(&o.bbox, &wider.bbox))
        {
            grids[i] = wider;
        }
    }
    grids
}

/// `g` widened to the edge of the header shaded across its first row, where
/// the band runs on past its rules over a column of labels.
///
/// A catalogue's product table shades its header row across every column
/// but rules its rows only from the second column on: each product's name
/// spans its group of rows, and only the rules between groups reach the
/// table's left edge, too few for the edge to be a boundary by their ends
/// and none of them its first or last (`FP0R-E8 Expansion Unit` beside six
/// rows). The header band says where the table's box is, where its text
/// stands over the strip it adds (`Product name`) and words stand in the
/// strip in the rows below. A band wider than its table with no name over
/// the strip, or no words under it, says nothing.
fn with_header_band_edge(
    g: &Grid,
    fills: &[crate::rule::Fill],
    glyphs: &[crate::glyph::Glyph],
) -> Option<Grid> {
    if g.ys.len() < 3 || g.xs.len() < 2 {
        return None;
    }
    let (left, right) = (g.xs[0], g.xs[g.xs.len() - 1]);
    let (top, bottom) = (g.ys[0], g.ys[g.ys.len() - 1]);
    let on_line = |y: f64| g.ys.iter().any(|o| (o - y).abs() <= EDGE_TOLERANCE);
    // Some glyph wholly inside the strip between `x0` and the grid's edge,
    // its middle between `y0` and `y1`.
    let ink = |x0: f64, y0: f64, y1: f64| {
        glyphs.iter().any(|gl| {
            !gl.text.trim().is_empty()
                && gl.bbox.is_some_and(|b| {
                    let cy = (b.y0 + b.y1) / 2.0;
                    b.x0 >= x0 && b.x1 <= left && cy > y0 && cy < y1
                })
        })
    };
    // The band may be drawn a cell at a time.
    let band = join_bands(fills).into_iter().map(|f| f.bbox).find(|b| {
        (b.y0 - top).abs() <= EDGE_TOLERANCE
            && on_line(b.y1)
            && b.y1 < bottom - MIN_CELL_EXTENT
            && b.x1 >= right - EDGE_TOLERANCE
            && b.x0 < left - MIN_CELL_EXTENT
    })?;
    if !(ink(band.x0, band.y0, band.y1) && ink(band.x0, band.y1, bottom)) {
        return None;
    }
    let mut wider = g.clone();
    wider.xs.insert(0, band.x0);
    wider.bbox.x0 = band.x0;
    Some(wider)
}

/// `grids`, each raised to the top of the band its ruling opens over a
/// group of its columns (see [`with_column_band`]); not onto another grid.
fn with_column_bands(
    mut grids: Vec<Grid>,
    rules: &[Rule],
    glyphs: &[crate::glyph::Glyph],
) -> Vec<Grid> {
    for i in 0..grids.len() {
        let Some(taller) = with_column_band(&grids[i], rules, glyphs) else {
            continue;
        };
        if !grids
            .iter()
            .enumerate()
            .any(|(j, o)| j != i && overlaps(&o.bbox, &taller.bbox))
        {
            grids[i] = taller;
        }
    }
    grids
}

/// `g` raised by a row where its top rule runs over a group of its columns
/// only and the rule beside the group runs on above it, up to the top of a
/// band naming the group.
///
/// A table left open at the top sets a band over the columns its figures
/// share (`BOLSAS ADICIONALES NECESARIAS POR CADA 100 PIES DE LA LÍNEA DEL
/// TECHO` over three roof pitches) and rules under it across those columns
/// alone; the column of labels beside them (`VALOR-R`) stands open from the
/// band's top down through the header, its rule running on up beside the
/// band. The rules alone start the grid under the band, and the band reads
/// as a heading over the table. Its text must stand over the group alone:
/// a title running on over the labels is the table's, not the group's.
fn with_column_band(g: &Grid, rules: &[Rule], glyphs: &[crate::glyph::Glyph]) -> Option<Grid> {
    let cols = g.cols();
    if g.ys.len() < 3 || cols < 3 || g.xs.len() != cols + 1 {
        return None;
    }
    let top = g.ys[0];
    let at_top: Vec<(f64, f64)> = rules
        .iter()
        .filter(|r| {
            r.orientation == Orientation::Horizontal
                && (r.bbox.y0 - top).abs() <= EDGE_TOLERANCE * 2.0
        })
        .map(|r| (r.bbox.x0, r.bbox.x1))
        .collect();
    let ruled: Vec<bool> = (0..cols)
        .map(|c| {
            let (x0, x1) = (g.xs[c], g.xs[c + 1]);
            span_coverage(at_top.iter().copied(), x0, x1) >= 0.6 * (x1 - x0)
        })
        .collect();
    // The group: one run of ruled columns, two or more, the open ones beside
    // it at the table's edges.
    let a = ruled.iter().position(|r| *r)?;
    let b = a + ruled[a..].iter().take_while(|r| **r).count();
    if b - a < 2 || ruled[b..].iter().any(|r| *r) || (a == 0 && b == cols) {
        return None;
    }
    // The rule at each open side of the group runs on above the top; the
    // band stands as tall as the shorter.
    let mut band_top = f64::NEG_INFINITY;
    for k in [a, b] {
        if k == 0 || k == cols {
            continue;
        }
        let x = g.xs[k];
        let up = rules
            .iter()
            .filter(|r| {
                r.orientation == Orientation::Vertical
                    && (r.axis_pos() - x).abs() <= EDGE_TOLERANCE
                    && r.bbox.y1 >= top - EDGE_TOLERANCE
                    && r.bbox.y0 < top - MIN_CELL_EXTENT
            })
            .map(|r| r.bbox.y0)
            .reduce(f64::min)?;
        band_top = band_top.max(up);
    }
    let (left, right) = (g.xs[0], g.xs[cols]);
    let (gx0, gx1) = (g.xs[a] - EDGE_TOLERANCE, g.xs[b] + EDGE_TOLERANCE);
    let mut named = false;
    for b in glyphs
        .iter()
        .filter(|gl| gl.is_horizontal() && !gl.text.trim().is_empty())
        .filter_map(|gl| gl.bbox)
    {
        let (cx, cy) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
        if cy <= band_top || cy >= top || cx <= left || cx >= right {
            continue;
        }
        if b.x0 < gx0 || b.x1 > gx1 {
            return None;
        }
        named = true;
    }
    if !named {
        return None;
    }
    let mut taller = g.clone();
    taller.ys.insert(0, band_top);
    taller.bbox.y0 = band_top;
    Some(taller)
}

/// A grid read from a stack of row lines is a table ruled row by row: its
/// lines part its text's lines, three to a row at most (a statement ruled
/// only under its header and its totals sets many rows between them), and
/// two of its columns or more hold text in half its rows (a chart's
/// gridlines leave one column of tick labels, a list of boxes one of
/// marks).
fn rules_its_rows(g: &Grid, glyphs: &[crate::glyph::Glyph]) -> bool {
    let inside: Vec<&crate::glyph::Glyph> = glyphs
        .iter()
        .filter(|gl| gl.is_horizontal() && !gl.text.trim().is_empty())
        .filter(|gl| {
            gl.bbox.is_some_and(|b| {
                let (cx, cy) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
                cx > g.bbox.x0 && cx < g.bbox.x1 && cy > g.bbox.y0 && cy < g.bbox.y1
            })
        })
        .collect();
    let mut sizes: Vec<f32> = inside.iter().map(|gl| gl.font_size).collect();
    if sizes.is_empty() {
        return false;
    }
    sizes.sort_by(f32::total_cmp);
    let fs = sizes[sizes.len() / 2].max(1.0) as f64;
    let baselines = coalesce(inside.iter().map(|gl| gl.origin.1).collect(), 0.4 * fs).len();
    if baselines > 3 * g.rows() {
        return false;
    }
    let (rows, cols) = (g.rows(), g.cols());
    let cells = g.cell_texts(glyphs);
    let held = (0..cols)
        .filter(|&c| {
            (0..rows)
                .filter(|&r| !cells[r * cols + c].trim().is_empty())
                .count()
                * 2
                >= rows
        })
        .count();
    held >= 2
}

/// Row lines drawn unbroken, each across the same stretch of the page, four
/// or more one under the next at an even pitch: a table ruled with
/// horizontals only, between every row or most, its columns said by its text
/// alone. Four lines bound three rows, the least a grid is taken for a table
/// with ([`reads_as_table`]). Two such tables set side by side in a page's two
/// columns interleave their lines down the page, and the page-wide reading
/// finds neither. Each stack's extent, from its first line to its last, and
/// its lines' heights.
pub(crate) fn unbroken_stacks(rules: &[Rule]) -> Vec<(BBox, Vec<f64>)> {
    let mut hs: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.orientation == Orientation::Horizontal)
        .collect();
    hs.sort_by(|a, b| a.axis_pos().total_cmp(&b.axis_pos()));
    // The pieces drawn at each height, grouped as [`cluster`] groups them.
    let mut levels: Vec<(f64, Vec<(f64, f64)>)> = Vec::new();
    for r in hs {
        match levels.last_mut() {
            Some((axis, pieces)) if (*axis - r.axis_pos()).abs() <= AXIS_TOLERANCE => {
                pieces.push((r.bbox.x0, r.bbox.x1))
            }
            _ => levels.push((r.axis_pos(), vec![(r.bbox.x0, r.bbox.x1)])),
        }
    }
    // Each level's stretches: pieces that touch or overlap, a stretch apart
    // from the next where a gap parts them. Two tables side by side whose
    // row lines fall level draw two lines there, one across each. A line is
    // unbroken when its pieces end only at its two ends.
    let mut whole: Vec<(f64, f64, f64)> = Vec::new();
    for (axis, mut pieces) in levels {
        pieces.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut runs: Vec<(f64, f64, Vec<(f64, f64)>)> = Vec::new();
        for p in pieces {
            match runs.last_mut() {
                Some((_, hi, run)) if p.0 <= *hi + EDGE_TOLERANCE => {
                    *hi = hi.max(p.1);
                    run.push(p);
                }
                _ => runs.push((p.0, p.1, vec![p])),
            }
        }
        for (lo, hi, run) in runs {
            let at_ends =
                |e: f64| (e - lo).abs() <= EDGE_TOLERANCE || (e - hi).abs() <= EDGE_TOLERANCE;
            if run.iter().all(|&(a, b)| at_ends(a) && at_ends(b)) {
                whole.push((axis, lo, hi));
            }
        }
    }
    let mut out = Vec::new();
    let mut used = vec![false; whole.len()];
    for i in 0..whole.len() {
        if used[i] {
            continue;
        }
        let (_, lo, hi) = whole[i];
        let same: Vec<usize> = (i..whole.len())
            .filter(|&j| {
                !used[j]
                    && (whole[j].1 - lo).abs() <= EDGE_TOLERANCE
                    && (whole[j].2 - hi).abs() <= EDGE_TOLERANCE
            })
            .collect();
        for &j in &same {
            used[j] = true;
        }
        let mut ys: Vec<f64> = same.iter().map(|&j| whole[j].0).collect();
        ys.sort_by(f64::total_cmp);
        if ys.len() < 4 {
            continue;
        }
        let mut gaps: Vec<f64> = ys.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_by(f64::total_cmp);
        let pitch = gaps[gaps.len() / 2];
        // Runs at the stack's pitch: a gap past three rows parts two tables
        // of one width.
        let mut start = 0;
        for k in 1..=ys.len() {
            if k == ys.len() || ys[k] - ys[k - 1] > pitch * TABLE_SPLIT_GAP {
                if k - start >= 4 {
                    out.push((
                        BBox {
                            x0: lo,
                            y0: ys[start],
                            x1: hi,
                            y1: ys[k - 1],
                        },
                        ys[start..k].to_vec(),
                    ));
                }
                start = k;
            }
        }
    }
    out
}

/// Each stack of row lines ruled alike ([`ruled_stacks`]): its extent, and
/// the places its lines break, the columns' edges as drawn.
pub(crate) fn stack_columns(rules: &[Rule], fills: &[crate::rule::Fill]) -> Vec<(BBox, Vec<f64>)> {
    ruled_stacks(rules, fills)
        .into_iter()
        .filter_map(|stack| {
            let hs: Vec<&Rule> = stack
                .iter()
                .filter(|r| r.orientation == Orientation::Horizontal)
                .collect();
            let lo = hs.iter().map(|r| r.bbox.x0).fold(f64::MAX, f64::min);
            let hi = hs.iter().map(|r| r.bbox.x1).fold(f64::MIN, f64::max);
            let top = hs.iter().map(|r| r.bbox.y0).fold(f64::MAX, f64::min);
            let bottom = hs.iter().map(|r| r.bbox.y1).fold(f64::MIN, f64::max);
            if hs.is_empty() || hi <= lo {
                return None;
            }
            let ends: Vec<f64> = hs
                .iter()
                .flat_map(|r| [r.bbox.x0, r.bbox.x1])
                .filter(|&e| e > lo + EDGE_TOLERANCE && e < hi - EDGE_TOLERANCE)
                .collect();
            let bbox = BBox {
                x0: lo,
                y0: top,
                x1: hi,
                y1: bottom,
            };
            Some((bbox, coalesce(ends, EDGE_TOLERANCE)))
        })
        .collect()
}

/// Stacks of a page's row lines ruled alike: three lines or more, one under
/// the next, each with the same extent and broken at the same places, a
/// table ruled with horizontals only that draws each line a column at a
/// time. Its lines need not touch. With each stack, the verticals standing
/// within it or a row above it (a shaded header's separators), one of them
/// at least on a break.
fn ruled_stacks(rules: &[Rule], fills: &[crate::rule::Fill]) -> Vec<Vec<Rule>> {
    let hs: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.orientation == Orientation::Horizontal)
        .collect();
    let lines = cluster(&hs);
    let breaks = |g: &GridLine| {
        coalesce(
            g.ends
                .iter()
                .copied()
                .filter(|e| *e > g.lo + EDGE_TOLERANCE && *e < g.hi - EDGE_TOLERANCE)
                .collect(),
            EDGE_TOLERANCE,
        )
    };
    let alike = |a: &GridLine, b: &GridLine| {
        let (ba, bb) = (breaks(a), breaks(b));
        (a.lo - b.lo).abs() <= EDGE_TOLERANCE
            && (a.hi - b.hi).abs() <= EDGE_TOLERANCE
            && !ba.is_empty()
            && ba.len() == bb.len()
            && ba
                .iter()
                .zip(&bb)
                .all(|(p, q)| (p - q).abs() <= 2.0 * EDGE_TOLERANCE)
    };
    let mut stacks: Vec<Vec<&GridLine>> = Vec::new();
    for line in &lines {
        match stacks.last_mut() {
            Some(stack) if alike(stack[stack.len() - 1], line) => stack.push(line),
            _ => stacks.push(vec![line]),
        }
    }
    stacks
        .into_iter()
        .filter(|stack| stack.len() >= 3)
        .filter_map(|stack| {
            let (top, bottom) = (stack[0].axis, stack[stack.len() - 1].axis);
            let (lo, hi) = (stack[0].lo, stack[0].hi);
            let mut gaps: Vec<f64> = stack.windows(2).map(|w| w[1].axis - w[0].axis).collect();
            gaps.sort_by(f64::total_cmp);
            let row = gaps[gaps.len() / 2];
            let picked: Vec<Rule> = rules
                .iter()
                .filter(|r| match r.orientation {
                    Orientation::Horizontal => stack.iter().any(|g| {
                        (r.axis_pos() - g.axis).abs() <= AXIS_TOLERANCE
                            && r.bbox.x0 >= lo - EDGE_TOLERANCE
                            && r.bbox.x1 <= hi + EDGE_TOLERANCE
                    }),
                    Orientation::Vertical => {
                        r.axis_pos() >= lo - EDGE_TOLERANCE
                            && r.axis_pos() <= hi + EDGE_TOLERANCE
                            && r.bbox.y0 >= top - row - EDGE_TOLERANCE
                            && r.bbox.y1 <= bottom + row + EDGE_TOLERANCE
                    }
                })
                .copied()
                .collect();
            // Its breaks are columns' edges where a vertical stands on one
            // (a shaded header's separators): a statement's figures
            // underlined a column at a time break alike too, between columns
            // that rows of text no rule parts carry on above and below.
            let edges = breaks(stack[0]);
            let near = |x: f64, e: f64| (x - e).abs() <= 2.0 * EDGE_TOLERANCE;
            let separated = picked.iter().any(|r| {
                r.orientation == Orientation::Vertical
                    && edges.iter().any(|&e| near(r.axis_pos(), e))
            });
            // Or where a shaded band over the rows is drawn a column at a
            // time: fills of one height, meeting end to end across the
            // lines' extent, an edge on every break. A chart's bars stand
            // apart, each its own height.
            let banded = || {
                let within: Vec<&crate::rule::Fill> = fills
                    .iter()
                    .filter(|f| {
                        f.bbox.x0 >= lo - EDGE_TOLERANCE
                            && f.bbox.x1 <= hi + EDGE_TOLERANCE
                            && f.bbox.y0 >= top - row - EDGE_TOLERANCE
                            && f.bbox.y1 <= bottom + EDGE_TOLERANCE
                    })
                    .collect();
                within.iter().any(|seed| {
                    let mut band: Vec<&&crate::rule::Fill> = within
                        .iter()
                        .filter(|f| {
                            (f.bbox.y0 - seed.bbox.y0).abs() <= EDGE_TOLERANCE
                                && (f.bbox.y1 - seed.bbox.y1).abs() <= EDGE_TOLERANCE
                        })
                        .collect();
                    band.sort_by(|a, b| a.bbox.x0.total_cmp(&b.bbox.x0));
                    band.len() > edges.len()
                        && near(band[0].bbox.x0, lo)
                        && near(band[band.len() - 1].bbox.x1, hi)
                        && band.windows(2).all(|w| near(w[0].bbox.x1, w[1].bbox.x0))
                        && edges
                            .iter()
                            .all(|&e| band.iter().any(|f| near(f.bbox.x1, e)))
                })
            };
            (separated || banded()).then_some(picked)
        })
        .collect()
}

/// Group the rules into touching components and detect a grid in each.
fn detect_components(
    rules: &[Rule],
    fills: &[crate::rule::Fill],
    glyphs: &[crate::glyph::Glyph],
    page: usize,
    frames: &[BBox],
) -> Vec<Grid> {
    let mut remaining: Vec<usize> = (0..rules.len()).collect();
    let mut components: Vec<Vec<Rule>> = Vec::new();
    while let Some(seed) = remaining.pop() {
        let mut component = vec![rules[seed]];
        let mut changed = true;
        while changed {
            changed = false;
            let mut i = 0;
            while i < remaining.len() {
                let candidate = rules[remaining[i]];
                let connected = component.iter().any(|r| {
                    r.bbox.x1 + EDGE_TOLERANCE >= candidate.bbox.x0
                        && candidate.bbox.x1 + EDGE_TOLERANCE >= r.bbox.x0
                        && r.bbox.y1 + EDGE_TOLERANCE >= candidate.bbox.y0
                        && candidate.bbox.y1 + EDGE_TOLERANCE >= r.bbox.y0
                });
                if connected {
                    component.push(candidate);
                    remaining.swap_remove(i);
                    changed = true;
                } else {
                    i += 1;
                }
            }
        }
        components.extend(split_stacked_boxes(component, |r| r));
    }

    let mut out: Vec<Grid> = components
        .iter()
        .filter(|component| component.len() >= MIN_GRID_LINES)
        .flat_map(|component| detect_group(component, fills, glyphs, page, frames))
        .collect();
    out.sort_by(|a, b| {
        a.bbox
            .y0
            .partial_cmp(&b.bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// Row boundaries with the edges of the fills laid across the table.
///
/// A shaded header band, or a shaded row, can be the only thing parting a
/// row from the next: no rule is drawn under it, and the shading's edge is
/// the line. In a grid of two rows and two columns or more, a band across
/// nine tenths of the table's width, from one of its edges to the other or
/// hanging from one of its lines, whose edge falls between two boundaries,
/// a cell's height from each and through no line of text, is a row boundary
/// too. A table striped row by row is left to its rules.
fn with_fill_edges(
    mut ys: Vec<f64>,
    xs: &[f64],
    fills: &[crate::rule::Fill],
    glyphs: &[crate::glyph::Glyph],
    open_sides: bool,
    verticals: &[BBox],
) -> Vec<f64> {
    // A table's rows and columns, already: a box the rules draw as one cell
    // (a chart's frame, its title band shaded) is not parted into rows by
    // its shading.
    if xs.len() < 3 || ys.len() < 3 {
        return ys;
    }
    let (Some(&x0), Some(&x1)) = (xs.first(), xs.last()) else {
        return ys;
    };
    let (Some(&top), Some(&bottom)) = (ys.first(), ys.last()) else {
        return ys;
    };
    let straddled = |y: f64| {
        glyphs.iter().any(|g| {
            g.bbox.is_some_and(|b| {
                b.x1 > x0 && b.x0 < x1 && b.y0 < y - EDGE_TOLERANCE && b.y1 > y + EDGE_TOLERANCE
            }) && !g.text.trim().is_empty()
        })
    };
    let lines = ys.clone();
    let on_line = |y: f64| lines.iter().any(|&o| (o - y).abs() <= EDGE_TOLERANCE);
    // Bands down a table, alternate rows striped, are no header's, and
    // their edges are no rules.
    let bands = shaded_bands(fills, xs, &ys);
    if striped(&bands) {
        return ys;
    }
    // A band from the table's one edge to the other set just over its first
    // rule, a row at most between them, naming two of its columns or more,
    // is the table's top: its header shaded, its rules drawn only under the
    // rows below (`Model Number | WD261KRYZ | …` over `Formatted capacity`).
    // A title band over a table sets one label, and stays its title. Only
    // a table whose sides are open, ruled with horizontals alone: a box its
    // verticals draw is bounded by them, and a band outside it is not its
    // own. Nor a header ruled into cells of its own, its verticals parting
    // its names: that is a box of its own, read as one.
    let row = ys
        .windows(2)
        .map(|w| w[1] - w[0])
        .fold(f64::MAX, f64::min)
        .max(MIN_CELL_EXTENT);
    // The columns a band names: its runs of text, set apart by more than a
    // word space, each inside one column. A header names two or more; a
    // title band over the table is one run, however many columns its words
    // fall across (`Technical data`).
    let named_columns = |b: &crate::geom::BBox| {
        let mut inside: Vec<&crate::glyph::Glyph> = glyphs
            .iter()
            .filter(|g| !g.text.trim().is_empty())
            .filter(|g| {
                g.bbox.is_some_and(|gb| {
                    let (cx, cy) = ((gb.x0 + gb.x1) / 2.0, (gb.y0 + gb.y1) / 2.0);
                    cy > b.y0 && cy < b.y1 && cx > b.x0 && cx < b.x1
                })
            })
            .collect();
        inside.sort_by(|a, b| a.bbox.unwrap().x0.total_cmp(&b.bbox.unwrap().x0));
        let mut runs: Vec<(f64, f64)> = Vec::new();
        for g in inside {
            let gb = g.bbox.unwrap();
            let space = g.font_size.max(1.0) as f64 * 0.8;
            match runs.last_mut() {
                Some(r) if gb.x0 - r.1 <= space => r.1 = r.1.max(gb.x1),
                _ => runs.push((gb.x0, gb.x1)),
            }
        }
        let interior = &xs[1..xs.len() - 1];
        if runs
            .iter()
            .any(|(a, b)| interior.iter().any(|&e| *a < e - 1.0 && *b > e + 1.0))
        {
            return 0;
        }
        let cols: std::collections::HashSet<usize> = runs
            .iter()
            .filter_map(|(a, b)| {
                let cx = (a + b) / 2.0;
                xs.windows(2).position(|w| cx >= w[0] && cx < w[1])
            })
            .collect();
        cols.len()
    };
    if let Some(head) = join_bands(fills)
        .into_iter()
        .filter(|_| open_sides)
        .find(|f| {
            let b = f.bbox;
            (b.x0 - x0).abs() <= EDGE_TOLERANCE * 2.0
                && (b.x1 - x1).abs() <= EDGE_TOLERANCE * 2.0
                && b.y1 <= top + EDGE_TOLERANCE
                && top - b.y1 <= row * 1.5
                && b.height() >= MIN_CELL_EXTENT
                && b.height() <= row * 3.0
                && named_columns(&b) >= 2
                && !verticals.iter().any(|v| {
                    v.y0 < b.y1 - EDGE_TOLERANCE
                        && v.y1 > b.y0 + EDGE_TOLERANCE
                        && v.x0 > x0 + EDGE_TOLERANCE
                        && v.x1 < x1 - EDGE_TOLERANCE
                })
        })
    {
        let b = head.bbox;
        if top - b.y1 >= MIN_CELL_EXTENT && !straddled(b.y1) {
            ys.insert(0, b.y1);
        }
        ys.insert(0, b.y0);
    }
    let top = ys[0];
    // A band from the table's one edge to the other is the table's own: a
    // section's band drawn a cell at a time, with no rule over or under it.
    let edge_to_edge = |b: &crate::geom::BBox| {
        (b.x0 - x0).abs() <= EDGE_TOLERANCE * 2.0 && (b.x1 - x1).abs() <= EDGE_TOLERANCE * 2.0
    };
    // Text between `y` and the next line on the far side from `other`: a
    // row the band's edge parts from it.
    let text_beyond = |ys: &[f64], y: f64, other: f64| {
        let (lo, hi) = if other > y {
            (ys.iter().copied().filter(|&o| o < y).fold(top, f64::max), y)
        } else {
            (
                y,
                ys.iter().copied().filter(|&o| o > y).fold(bottom, f64::min),
            )
        };
        glyphs.iter().any(|g| {
            !g.text.trim().is_empty()
                && g.bbox.is_some_and(|gb| {
                    let (cx, cy) = ((gb.x0 + gb.x1) / 2.0, (gb.y0 + gb.y1) / 2.0);
                    cx > x0 && cx < x1 && cy > lo && cy < hi
                })
        })
    };
    for f in &bands {
        let b = f.bbox;
        // Else a table's shading hangs from one of its lines: a header band
        // from the top rule, a shaded row from the rule above it. Its other
        // edge parts the row it falls in. A box laid over a chart does
        // neither. A band from edge to edge, hanging from no line, parts a
        // row from it only where the row holds text: one set in the blank
        // between two tables (a heading's band between two field lists)
        // parts nothing, and the blank stays the gap between them.
        for (y, other) in [(b.y0, b.y1), (b.y1, b.y0)] {
            if !(on_line(other) || edge_to_edge(&b) && text_beyond(&ys, y, other))
                || !shading_ends(&bands, f, y)
                || y - top < MIN_CELL_EXTENT
                || bottom - y < MIN_CELL_EXTENT
                || ys.iter().any(|&o| (o - y).abs() < MIN_CELL_EXTENT)
                || straddled(y)
            {
                continue;
            }
            ys.push(y);
        }
    }
    ys.sort_by(f64::total_cmp);
    ys
}

/// Whether `f`'s shading stops at its edge on `y`: no other fill butts on
/// to it there over the same columns. A column shaded as one block is often
/// drawn as a fill per row, edge to edge; the seams are not seen, and part
/// nothing. A background laid under the whole table is no such fill: it
/// starts well before the edge, and the band on it still shows.
fn shading_ends(fills: &[crate::rule::Fill], f: &crate::rule::Fill, y: f64) -> bool {
    let b = f.bbox;
    let below = (b.y1 - y).abs() <= (b.y0 - y).abs();
    !fills.iter().any(|g| {
        let o = g.bbox;
        let butts = if below {
            (o.y0 - y).abs() <= EDGE_TOLERANCE && o.y1 > y + EDGE_TOLERANCE
        } else {
            (o.y1 - y).abs() <= EDGE_TOLERANCE && o.y0 < y - EDGE_TOLERANCE
        };
        butts && o.x1.min(b.x1) - o.x0.max(b.x0) >= (b.x1 - b.x0) * FILL_ROW_COVERAGE
    })
}

/// The fills laid across a table as bands: nine tenths of its width, under
/// half its height (a background under the whole table is no band), their
/// middles inside it. A band drawn a cell at a time, one fill a column on
/// one line, is one band.
fn shaded_bands(fills: &[crate::rule::Fill], xs: &[f64], ys: &[f64]) -> Vec<crate::rule::Fill> {
    let (Some(&x0), Some(&x1), Some(&top), Some(&bottom)) =
        (xs.first(), xs.last(), ys.first(), ys.last())
    else {
        return Vec::new();
    };
    let (w, h) = (x1 - x0, bottom - top);
    join_bands(fills)
        .into_iter()
        .filter(|f| {
            let mid = (f.bbox.y0 + f.bbox.y1) / 2.0;
            mid > top
                && mid < bottom
                && f.bbox.height() < h / 2.0
                && f.bbox.x1.min(x1) - f.bbox.x0.max(x0) >= w * FILL_ROW_COVERAGE
        })
        .collect()
}

/// Fills on one line side by side, joined: a band drawn a cell at a time.
fn join_bands(fills: &[crate::rule::Fill]) -> Vec<crate::rule::Fill> {
    let mut sorted: Vec<&crate::rule::Fill> = fills.iter().collect();
    sorted.sort_by(|a, b| {
        a.bbox
            .y0
            .total_cmp(&b.bbox.y0)
            .then(a.bbox.y1.total_cmp(&b.bbox.y1))
            .then(a.bbox.x0.total_cmp(&b.bbox.x0))
    });
    let mut joined: Vec<crate::rule::Fill> = Vec::new();
    for f in sorted {
        match joined.last_mut() {
            Some(j)
                if (j.bbox.y0 - f.bbox.y0).abs() <= 1.0
                    && (j.bbox.y1 - f.bbox.y1).abs() <= 1.0
                    && f.bbox.x0 <= j.bbox.x1 + 2.0
                    && f.bbox.x1 >= j.bbox.x0 - 2.0 =>
            {
                j.bbox = j.bbox.union(&f.bbox);
            }
            _ => joined.push(*f),
        }
    }
    joined
}

/// The bands shaded across a table, edge to edge, that are lines of its
/// own: none where they stripe it row by row.
pub(crate) fn section_bands(
    fills: &[crate::rule::Fill],
    xs: &[f64],
    ys: &[f64],
) -> Vec<crate::rule::Fill> {
    let bands = shaded_bands(fills, xs, ys);
    if striped(&bands) {
        Vec::new()
    } else {
        bands
    }
}

/// Bands striping a table row by row: two or more, each a row's height or
/// so from the next. Their edges are no rules. Section bands set rows apart
/// are not stripes.
fn striped(bands: &[crate::rule::Fill]) -> bool {
    let mut ys: Vec<(f64, f64)> = bands.iter().map(|f| (f.bbox.y0, f.bbox.y1)).collect();
    ys.sort_by(|a, b| a.0.total_cmp(&b.0));
    bands.len() >= 2
        && ys
            .windows(2)
            .any(|w| w[1].0 - w[0].1 <= 1.5 * (w[0].1 - w[0].0).max(w[1].1 - w[1].0))
}

/// Share of a table's width a fill must cover for its edge to part rows.
const FILL_ROW_COVERAGE: f64 = 0.9;

/// Boundaries along one axis.
///
/// `axis_lines` are the grid lines perpendicular to the axis (their positions
/// are boundaries directly); `cross_lines` run along it, and the *endpoints* of
/// their segments mark boundaries too — the only source when a grid is drawn
/// one segment per cell.
fn boundaries(
    axis_lines: &[GridLine],
    cross_lines: &[GridLine],
    axis_span: f64,
    cross_span: f64,
) -> Vec<f64> {
    drop_degenerate(
        boundary_candidates(axis_lines, cross_lines, axis_span, cross_span),
        MIN_CELL_EXTENT,
    )
}

/// Boundary positions along one axis, coalesced, before [`boundaries`]
/// drops the degenerate ones.
fn boundary_candidates(
    axis_lines: &[GridLine],
    cross_lines: &[GridLine],
    axis_span: f64,
    cross_span: f64,
) -> Vec<f64> {
    let mut cands: Vec<f64> = axis_lines
        .iter()
        .filter(|g| (g.hi - g.lo) >= axis_span * MIN_LINE_COVERAGE)
        .map(|g| g.axis)
        .collect();

    let wide: Vec<&GridLine> = cross_lines
        .iter()
        .filter(|g| (g.hi - g.lo) >= cross_span * MIN_LINE_COVERAGE)
        .collect();
    if !wide.is_empty() {
        let all = coalesce(
            wide.iter().flat_map(|g| g.ends.iter().copied()).collect(),
            EDGE_TOLERANCE,
        );
        let needed = ((wide.len() as f64) * MIN_BOUNDARY_SUPPORT).ceil() as usize;
        for cand in all {
            let support = wide
                .iter()
                .filter(|g| g.ends.iter().any(|e| (e - cand).abs() <= EDGE_TOLERANCE))
                .count();
            if support >= needed {
                cands.push(cand);
            }
        }
        // A gutter the rows' rules part at again and again is a column's
        // edge, however few of the table's rules it is on: a statement
        // ruled with underlines under its figures, a piece under each
        // column, between rules across the whole table that part nothing.
        // Only where no edge stands already, which it would move: the run's
        // two ends are often the table's own.
        let parts = |g: &GridLine| {
            g.gutters
                .iter()
                .chain(&g.gutter_ends)
                .copied()
                .collect::<Vec<_>>()
        };
        let found = cands.clone();
        let gutters = coalesce(wide.iter().flat_map(|g| parts(g)).collect(), EDGE_TOLERANCE);
        for cand in gutters {
            let support = wide
                .iter()
                .filter(|g| parts(g).iter().any(|e| (e - cand).abs() <= EDGE_TOLERANCE))
                .count();
            let new = !found
                .iter()
                .any(|e| (e - cand).abs() <= 2.0 * EDGE_TOLERANCE);
            if support >= MIN_GUTTER_LINES && new {
                cands.push(cand);
            }
        }
    }
    coalesce(cands, EDGE_TOLERANCE)
}

/// True when drawn geometry supports a candidate aligned table.
///
/// Alignment alone is indistinguishable from prose that happens to break into
/// columns, and acting on it alone was net-negative: it cost NID 0.868 -> 0.839.
/// Every zero-TEDS document in the corpus has a rule or a fill spanning its
/// table; ordinary prose has neither.
fn has_spanning_rule(g: &Grid, rules: &[Rule]) -> bool {
    let w = g.bbox.width();
    let h = g.bbox.height().max(1.0);
    if w <= 0.0 {
        return false;
    }
    let margin = h * CORROBORATION_MARGIN;
    let (lo, hi) = (g.bbox.y0 - margin, g.bbox.y1 + margin);

    rules.iter().any(|r| {
        r.orientation == Orientation::Horizontal
            && r.axis_pos() >= lo
            && r.axis_pos() <= hi
            && r.bbox.x1.min(g.bbox.x1) - r.bbox.x0.max(g.bbox.x0) >= w * CORROBORATION_COVERAGE
    })
}

fn has_nearby_top_rule(g: &Grid, rules: &[Rule]) -> bool {
    let w = g.bbox.width();
    let h = g.bbox.height().max(1.0);
    rules.iter().any(|r| {
        r.orientation == Orientation::Horizontal
            && r.axis_pos() < g.bbox.y0
            && g.bbox.y0 - r.axis_pos() <= h
            && r.bbox.x1.min(g.bbox.x1) - r.bbox.x0.max(g.bbox.x0) >= w * CORROBORATION_COVERAGE
    })
}

fn has_header_fill_row(g: &Grid, fills: &[crate::rule::Fill]) -> bool {
    let w = g.bbox.width();
    if w <= 0.0 {
        return false;
    }
    for seed in fills {
        if seed.bbox.y1 < g.bbox.y0 - seed.bbox.height() || seed.bbox.y0 > g.bbox.y1 {
            continue;
        }
        let row: Vec<&crate::rule::Fill> = fills
            .iter()
            .filter(|f| {
                f.bbox.y1 >= seed.bbox.y0
                    && f.bbox.y0 <= seed.bbox.y1
                    && f.bbox.x1 >= g.bbox.x0
                    && f.bbox.x0 <= g.bbox.x1
            })
            .collect();
        if row.len() < g.cols() {
            continue;
        }
        let covered: f64 = row
            .iter()
            .map(|f| f.bbox.x1.min(g.bbox.x1) - f.bbox.x0.max(g.bbox.x0))
            .filter(|span| *span > 0.0)
            .sum();
        if covered >= w * 0.80 {
            return true;
        }
    }
    false
}

pub fn is_corroborated(g: &Grid, rules: &[Rule], fills: &[crate::rule::Fill]) -> bool {
    if has_spanning_rule(g, rules) {
        return true;
    }

    let w = g.bbox.width();
    let h = g.bbox.height().max(1.0);
    if w <= 0.0 {
        return false;
    }
    let margin = h * CORROBORATION_MARGIN;
    let (lo, hi) = (g.bbox.y0 - margin, g.bbox.y1 + margin);
    fills.iter().any(|f| {
        f.bbox.y1 >= lo
            && f.bbox.y0 <= hi
            && f.bbox.x1.min(g.bbox.x1) - f.bbox.x0.max(g.bbox.x0) >= w * CORROBORATION_COVERAGE
    })
}

/// Short aligned runs are common in ordinary display layouts. Accept a
/// three-row candidate only with at least three columns and strong geometry.
pub fn accepts_aligned_candidate(g: &Grid, rules: &[Rule], fills: &[crate::rule::Fill]) -> bool {
    let cell_fill_support = fills
        .iter()
        .filter(|f| {
            let x = (f.bbox.x0 + f.bbox.x1) * 0.5;
            let y = (f.bbox.y0 + f.bbox.y1) * 0.5;
            x >= g.bbox.x0 && x <= g.bbox.x1 && y >= g.bbox.y0 && y <= g.bbox.y1
        })
        .count()
        >= g.rows() * g.cols();
    let corroborated = is_corroborated(g, rules, fills);
    let dense_fill_support = corroborated && fills.len() >= g.rows() * g.cols() * 2;
    let header_fill_support = g.cols() >= 3 && has_header_fill_row(g, fills);
    let nearby_top_rule = g.cols() >= 3 && has_nearby_top_rule(g, rules);
    (corroborated || cell_fill_support || header_fill_support || nearby_top_rule)
        && (g.rows() > MIN_ALIGNED_ROWS
            || (g.cols() >= 3
                && (has_spanning_rule(g, rules)
                    || nearby_top_rule
                    || cell_fill_support
                    || header_fill_support
                    || dense_fill_support)))
}

/// Remove boundaries that would create a cell too thin to hold anything.
fn drop_degenerate(vals: Vec<f64>, min: f64) -> Vec<f64> {
    let mut out: Vec<f64> = Vec::with_capacity(vals.len());
    for v in vals {
        match out.last() {
            Some(prev) if v - prev < min => {}
            _ => out.push(v),
        }
    }
    out
}

fn supported_clusters(mut vals: Vec<f64>, tolerance: f64, minimum: usize) -> Vec<f64> {
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = Vec::new();
    let mut group: Vec<f64> = Vec::new();
    for value in vals {
        if group.last().is_none_or(|last| value - last <= tolerance) {
            group.push(value);
        } else {
            if group.len() >= minimum {
                out.push(group.iter().sum::<f64>() / group.len() as f64);
            }
            group = vec![value];
        }
    }
    if group.len() >= minimum {
        out.push(group.iter().sum::<f64>() / group.len() as f64);
    }
    out
}

/// Recover tables whose filled rectangles repeat in stable text/cell bands.
///
/// Some exports draw a large table background plus one rectangle around each
/// populated cell fragment. Repeated X starts reveal columns; clustered Y
/// starts merge wrapped fragments into logical rows.
pub fn detect_fill_bands(fills: &[crate::rule::Fill], page: usize) -> Vec<Grid> {
    let usable: Vec<&crate::rule::Fill> = fills
        .iter()
        .filter(|fill| {
            let w = fill.bbox.width();
            let h = fill.bbox.height();
            (20.0..=300.0).contains(&w) && (4.0..=100.0).contains(&h)
        })
        .collect();
    if usable.len() < 12 {
        return Vec::new();
    }

    let xs = supported_clusters(usable.iter().map(|fill| fill.bbox.x0).collect(), 6.0, 3);
    if xs.len() < 3 {
        return Vec::new();
    }
    let supported: Vec<&crate::rule::Fill> = usable
        .into_iter()
        .filter(|fill| xs.iter().any(|x| (fill.bbox.x0 - x).abs() <= 6.0))
        .collect();
    let ys = supported_clusters(
        supported.iter().map(|fill| fill.bbox.y0).collect(),
        8.0,
        xs.len().div_ceil(2),
    );
    if ys.len() < 3 {
        return Vec::new();
    }

    let mut runs: Vec<Vec<f64>> = Vec::new();
    for y in ys {
        match runs.last_mut() {
            Some(run) if y - run[run.len() - 1] <= 50.0 => run.push(y),
            _ => runs.push(vec![y]),
        }
    }

    runs.into_iter()
        .filter(|run| run.len() >= 3)
        .filter_map(|run| {
            let y0 = run[0] - 1.0;
            let y_last = *run.last()?;
            let band_fills: Vec<&&crate::rule::Fill> = supported
                .iter()
                .filter(|fill| fill.bbox.y0 >= y0 && fill.bbox.y0 <= y_last + 8.0)
                .collect();
            let x_hi = band_fills
                .iter()
                .map(|fill| fill.bbox.x1)
                .fold(f64::MIN, f64::max);
            let y_hi = band_fills
                .iter()
                .map(|fill| fill.bbox.y1)
                .fold(f64::MIN, f64::max)
                + 1.0;
            let mut boundaries = xs.clone();
            boundaries.push(x_hi + 1.0);
            let grid = Grid {
                layout: None,
                page,
                bbox: BBox {
                    x0: boundaries[0],
                    y0,
                    x1: x_hi + 1.0,
                    y1: y_hi,
                },
                xs: drop_degenerate(boundaries, MIN_CELL_EXTENT),
                ys: run
                    .into_iter()
                    .map(|y| y - 1.0)
                    .chain(std::iter::once(y_hi))
                    .collect(),
            };
            (grid.bbox.width() >= 300.0
                && grid.cols() >= 3
                && grid.rows() >= 3
                && is_corroborated(&grid, &[], fills))
            .then_some(grid)
        })
        .collect()
}

/// Recover sparse tables bounded by horizontal rules.
///
/// Forms often leave most response cells blank, so repeated-column support
/// cannot work. A dense header row supplies the columns, while populated
/// labels in the first column supply logical row starts.
pub fn detect_horizontal_bands(
    glyphs: &[crate::glyph::Glyph],
    rules: &[Rule],
    page: usize,
) -> Vec<Grid> {
    let mut groups: Vec<Vec<&Rule>> = Vec::new();
    for rule in rules
        .iter()
        .filter(|rule| rule.orientation == Orientation::Horizontal && rule.length() >= 80.0)
    {
        if let Some(group) = groups.iter_mut().find(|group| {
            (group[0].bbox.x0 - rule.bbox.x0).abs() <= 10.0
                && (group[0].bbox.x1 - rule.bbox.x1).abs() <= 10.0
        }) {
            group.push(rule);
        } else {
            groups.push(vec![rule]);
        }
    }

    let mut out = Vec::new();
    for group in groups {
        let mut rule_ys = coalesce(
            group.iter().map(|rule| rule.axis_pos()).collect(),
            AXIS_TOLERANCE,
        );
        if rule_ys.len() < 2 {
            continue;
        }
        rule_ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let x0 = group
            .iter()
            .map(|rule| rule.bbox.x0)
            .fold(f64::MAX, f64::min);
        let x1 = group
            .iter()
            .map(|rule| rule.bbox.x1)
            .fold(f64::MIN, f64::max);
        let y0 = rule_ys[0];
        let y1 = *rule_ys.last().unwrap();

        let mut band: Vec<&crate::glyph::Glyph> = glyphs
            .iter()
            .filter(|glyph| {
                glyph.bbox.is_some_and(|bbox| {
                    let x = (bbox.x0 + bbox.x1) * 0.5;
                    let y = (bbox.y0 + bbox.y1) * 0.5;
                    x >= x0 && x <= x1 && y >= y0 && y <= y1
                })
            })
            .collect();
        band.sort_by(|a, b| {
            a.origin
                .1
                .partial_cmp(&b.origin.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(
                    a.origin
                        .0
                        .partial_cmp(&b.origin.0)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });
        let mut rows: Vec<Vec<&crate::glyph::Glyph>> = Vec::new();
        for glyph in band {
            let tolerance = glyph.font_size.max(1.0) as f64 * ROW_BASELINE_TOLERANCE;
            match rows.last_mut() {
                Some(row) if (row[0].origin.1 - glyph.origin.1).abs() <= tolerance => {
                    row.push(glyph)
                }
                _ => rows.push(vec![glyph]),
            }
        }
        let Some(densest) = rows.iter().max_by_key(|row| cell_starts(row).len()) else {
            continue;
        };
        let starts = cell_starts(densest);
        if starts.len() < 2 {
            continue;
        }
        let first_cut = (starts[0] + starts[1]) * 0.5;
        let label_rows: Vec<&Vec<&crate::glyph::Glyph>> = rows
            .iter()
            .filter(|row| {
                row.iter().any(|glyph| {
                    glyph.bbox.is_some_and(|bbox| {
                        let x = (bbox.x0 + bbox.x1) * 0.5;
                        x >= x0 && x < first_cut
                    })
                })
            })
            .collect();
        if label_rows.len() < 2 {
            continue;
        }

        let mut xs = vec![x0];
        xs.extend(starts.into_iter().skip(1));
        xs.push(x1);
        let xs = drop_degenerate(coalesce(xs, EDGE_TOLERANCE), MIN_CELL_EXTENT);
        if xs.len() < 3 {
            continue;
        }
        let mut ys = vec![y0];
        ys.extend(label_rows.iter().skip(1).map(|row| {
            row.iter()
                .filter_map(|glyph| glyph.bbox)
                .map(|bbox| bbox.y0)
                .fold(f64::MAX, f64::min)
                - 1.0
        }));
        ys.push(y1);
        let ys = drop_degenerate(coalesce(ys, EDGE_TOLERANCE), MIN_CELL_EXTENT);
        if ys.len() < 3 {
            continue;
        }
        out.push(Grid {
            layout: None,
            page,
            bbox: BBox {
                x0: xs[0],
                y0: ys[0],
                x1: xs[xs.len() - 1],
                y1: ys[ys.len() - 1],
            },
            xs,
            ys,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Alignment-based detection, for tables drawn without a full grid.
// ---------------------------------------------------------------------------

/// Consecutive rows needed before aligned text counts as a table.
const MIN_ALIGNED_ROWS: usize = 3;

/// Baseline agreement for two lines to be in the same row, as a fraction of
/// font size.
const ROW_BASELINE_TOLERANCE: f64 = 0.5;

/// Column positions within this fraction of font size are the same column.
const COLUMN_ALIGN_TOLERANCE: f64 = 1.5;

/// Gap, as a fraction of font size, that separates table cells within a row.
///
/// Well above the measured word-space mode (0.35) but well below the
/// line-splitting threshold (1.5), because table columns are routinely closer
/// together than a column gutter. Precision comes from requiring the resulting
/// positions to repeat across rows, not from this threshold.
const TABLE_CELL_GAP: f64 = 0.9;

/// Widest gap, in font sizes, between two letters of one word: where the
/// pen leaves off and the next letter's ink begins, a kern apart at most.
const WORD_TOUCH: f64 = 0.05;

/// Fraction of a run's rows that must use a column for it to be real.
const MIN_COLUMN_SUPPORT: f64 = 0.6;

/// Fraction of a candidate table's width a rule or fill must span to
/// corroborate it.
const CORROBORATION_COVERAGE: f64 = 0.5;

/// How far outside a candidate's y-range corroborating geometry may sit, as a
/// fraction of the candidate's height. A table's top rule often sits just above
/// its first row of text.
const CORROBORATION_MARGIN: f64 = 0.15;

/// Detect tables from text alignment, working from glyphs.
///
/// This cannot work from assembled lines. The line pass splits at gaps wider
/// than 1.5x font size, but table columns are routinely closer than that: a
/// header row like `Saccharometer DI Water Glucose Solution Yeast
/// Suspension` is four cells that arrive as a single line. Working from lines
/// found zero candidates on every document that needed this strategy.
///
/// So rows are built from glyph baselines, split at a *table-scale* gap, and
/// the precision signal is that the split positions repeat across rows.
/// Ordinary prose has gaps too, but they land in different places on every
/// line; a table's do not.
pub fn detect_aligned(
    glyphs: &[crate::glyph::Glyph],
    rules: &[Rule],
    fills: &[crate::rule::Fill],
    page: usize,
) -> Vec<Grid> {
    detect_aligned_with_refused(glyphs, rules, fills, page).0
}

/// [`detect_aligned`]'s grids, and the runs of aligned rows it refused for
/// want of anything to corroborate them: text that lines up into columns,
/// which is not a table. Bulleted lists set side by side are such a run
/// (`document::list_boxes`).
pub fn detect_aligned_with_refused(
    glyphs: &[crate::glyph::Glyph],
    rules: &[Rule],
    fills: &[crate::rule::Fill],
    page: usize,
) -> (Vec<Grid>, Vec<Grid>) {
    // The page's lines, read only for a candidate its geometry does not
    // already accept and big enough for a caption to name.
    let captions = std::cell::OnceCell::new();
    let captions = || -> &Vec<BBox> {
        captions.get_or_init(|| {
            crate::line::assemble(glyphs)
                .into_iter()
                .filter(|line| numbered_table_caption(&line.text))
                .map(|line| line.bbox)
                .collect()
        })
    };
    let (rows, cells) = aligned_rows(glyphs);
    let (mut out, refused): (Vec<Grid>, Vec<Grid>) = aligned_runs(&rows, &cells, page)
        .into_iter()
        .partition(|g| {
            let captioned = || {
                g.cols() >= 3
                    && g.rows() >= 3
                    && captions().iter().any(|caption| {
                        caption.y1 <= g.bbox.y0
                            && g.bbox.y0 - caption.y1 <= g.bbox.height()
                            && caption.x1 >= g.bbox.x0
                            && caption.x0 <= g.bbox.x1
                    })
            };
            accepts_aligned_candidate(g, rules, fills)
                || captioned()
                || has_typeset_header(g, glyphs)
                || has_two_axis_header(g, glyphs)
        });
    // Two unruled tables set close together — a key/value block of two
    // columns just above a line table of five — align as one run, and the
    // run is neither: its first row is no header and its rows disagree on
    // their cells. Each stretch of rows that agrees on how many cells it
    // has is tried on its own, by type alone, where no accepted grid
    // already covers it.
    for part in aligned_parts(&rows, &cells, page) {
        let taken = out
            .iter()
            .any(|g| g.bbox.y0 < part.bbox.y1 && part.bbox.y0 < g.bbox.y1);
        if !taken && (has_typeset_header(&part, glyphs) || has_two_axis_header(&part, glyphs)) {
            out.push(part);
        }
    }
    for list in colon_field_lists(glyphs, &rows, page) {
        let taken = out
            .iter()
            .any(|g| g.bbox.y0 < list.bbox.y1 && list.bbox.y0 < g.bbox.y1);
        if !taken {
            out.push(list);
        }
    }
    for g in out.iter_mut() {
        extend_by_banner(g, &rows);
        drop_field_line(g, glyphs, rules);
        fit_right_aligned_heads(g, glyphs);
        fit_heads_to_figures(g, glyphs);
    }
    (out, refused)
}

/// Least share of an aligned grid's width a rule spans to rule its first
/// row off from the rest: the grid's own width, its cells' ends allowed for.
const FIELD_LINE_RULE_SPAN: f64 = 0.9;

/// Drop an aligned grid's first row where it is a line of fields, each a
/// label set inline with its value (`Guest Name: Sanjay | Date:
/// 31/12/2017`), or a line holding a figure over a row that names the
/// columns in words (`Date limite de paiement … | 29.99` over `Montant EUR
/// HT | Montant TVA | …`), ruled off from the rows under it by a rule
/// across the grid: the page's facts over the table, not the names of its
/// columns. A header names its columns; a field states its own value, and
/// the table's head is its top. The grid starts at the rule.
fn drop_field_line(g: &mut Grid, glyphs: &[crate::glyph::Glyph], rules: &[Rule]) {
    if g.layout.is_some() || g.rows() < 3 {
        return;
    }
    let cols = g.cols();
    let cells = g.cell_texts(glyphs);
    let first: Vec<&str> = cells[..cols]
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    let fields = first.len() >= 2 && first.iter().all(|t| label_colon(t) == Some(false));
    // A line holding an amount of its own (`29.99`) over heads named in
    // words, figures under them: the heads are the table's top.
    let figures_under = (2..g.rows()).any(|r| {
        cells[r * cols..(r + 1) * cols]
            .iter()
            .any(|t| is_numeric_cell(t.trim()))
    });
    let over_names = first.iter().any(|t| is_numeric_cell(t))
        && !head_runs(g, glyphs, 1).is_empty()
        && figures_under;
    if !fields && !over_names {
        return;
    }
    let (x0, x1) = (g.xs[0], g.xs[cols]);
    // The ink of a row: its glyphs' tops and bottoms.
    let ink = |r: usize| {
        glyphs
            .iter()
            .filter(|gl| !gl.text.trim().is_empty())
            .filter_map(|gl| gl.bbox.map(|b| (gl.center(), b)))
            .filter(|((x, y), _)| *x >= x0 && *x <= x1 && *y >= g.ys[r] && *y < g.ys[r + 1])
            .fold((f64::MAX, f64::MIN), |(lo, hi), (_, b)| {
                (lo.min(b.y0), hi.max(b.y1))
            })
    };
    let (above, below) = (ink(0).1, ink(1).0);
    let ruled = rules.iter().find(|r| {
        r.orientation == Orientation::Horizontal
            && r.axis_pos() > above
            && r.axis_pos() < below
            && r.bbox.x1.min(x1) - r.bbox.x0.max(x0) >= (x1 - x0) * FIELD_LINE_RULE_SPAN
    });
    let Some(rule) = ruled else {
        return;
    };
    let top = rule.axis_pos();
    g.ys.remove(0);
    g.ys[0] = top;
    g.bbox.y0 = top;
}

/// The runs of words in row `r` of a grid where the row names its columns
/// in words: two runs or more, parted by a cell's gap, letters in them and
/// no figure. Empty where the row is no such row. Read from the row's
/// glyphs, not its cells' text: edges cutting through the names regroup
/// their words.
fn head_runs(g: &Grid, glyphs: &[crate::glyph::Glyph], r: usize) -> Vec<(f64, f64)> {
    let mut inked: Vec<(f64, f64, f64)> = Vec::new();
    let mut letters = false;
    // The row's glyphs as its cells take them, whole words by their
    // centres of mass ([`Grid::row_glyphs`]).
    for gl in g
        .row_glyphs(glyphs, r)
        .iter()
        .filter(|gl| gl.is_horizontal())
    {
        let Some(b) = gl.bbox else { continue };
        if gl.text.chars().any(|c| c.is_ascii_digit()) {
            return Vec::new();
        }
        letters |= gl.text.chars().any(char::is_alphabetic);
        inked.push((b.x0, b.x1, f64::from(gl.font_size).max(1.0)));
    }
    inked.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut runs: Vec<(f64, f64)> = Vec::new();
    for (a, b, fs) in inked {
        match runs.last_mut() {
            Some(run) if a - run.1 <= fs * TABLE_CELL_GAP => run.1 = run.1.max(b),
            _ => runs.push((a, b)),
        }
    }
    if letters && runs.len() >= 2 {
        runs
    } else {
        Vec::new()
    }
}

/// The median size of the glyphs between `y0` and `y1`: a row's type.
fn head_size(glyphs: &[crate::glyph::Glyph], y0: f64, y1: f64) -> f64 {
    let mut sizes: Vec<f64> = glyphs
        .iter()
        .filter(|gl| !gl.text.trim().is_empty())
        .filter(|gl| {
            let (_, y) = gl.center();
            y > y0 && y < y1
        })
        .map(|gl| f64::from(gl.font_size))
        .collect();
    sizes.sort_by(f64::total_cmp);
    sizes.get(sizes.len() / 2).copied().unwrap_or(1.0).max(1.0)
}

/// Move an aligned grid's column edges into the gutters between its
/// columns, each column its head and the figures under it together, where
/// the heads stand over their figures but not within the edges set where
/// the figures start: a chart's table sets each head centred over its bar
/// and figure (`Low-income countries` over a bar and `–4.3`), the bar left
/// of the figure, so each head falls in the column before its own. Cut so,
/// the heads' row is no header and is left out, its lines read as the
/// page's, the heads' words interleaved (`Lower-Upper-Lowmiddlemiddle-…`).
///
/// Every column past the first holds figures: most of its words (the
/// body's, by their centres) are numbers. Text set in panels under their
/// titles lines up word for word as well, and is no such table. Each head
/// (a run of the header row's words) stands over the figures of exactly
/// one column, taken at their median extent, and each column's figures
/// under one head at most. Each edge moves to the middle of the gutter
/// between the column before it, head and figures, and its own, where that
/// gutter is as wide as the columns are parted by and the edge cuts through
/// no run of the body's words: a chart sets each figure at its bar's end,
/// and a negative bar's figure stands out in the gutter, a run of its own.
/// A head set just past the grid's first edge over the first column's
/// figures (`World`) moves that edge out to it.
fn fit_heads_to_figures(g: &mut Grid, glyphs: &[crate::glyph::Glyph]) {
    let cols = g.cols();
    if g.layout.is_some() || g.rows() < 2 || cols < 2 {
        return;
    }
    let fs = head_size(glyphs, g.ys[0], g.ys[1]);
    let (top, bottom) = (g.ys[0], g.ys[g.rows()]);
    // The page's words, each glyph joined to the one before it on its line
    // where they touch (a sign to its digits): a word is placed whole, by
    // its centre, as the grid's cells take it.
    let mut placed: Vec<(f64, f64, f64, f64, &str)> = glyphs
        .iter()
        .filter(|gl| !gl.text.trim().is_empty() && gl.is_horizontal())
        .filter_map(|gl| {
            let b = gl.bbox?;
            Some((
                b.x0,
                b.x1,
                gl.origin.1,
                f64::from(gl.font_size).max(1.0),
                gl.text.as_str(),
            ))
        })
        .collect();
    placed.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.0.total_cmp(&b.0)));
    let mut inked: Vec<(f64, f64, f64)> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    let mut last: Option<(f64, f64)> = None;
    for (a, b, y, size, t) in placed {
        match (inked.last_mut(), last) {
            (Some(w), Some((ly, _))) if (y - ly).abs() <= size * 0.3 && a - w.1 <= size * 0.25 => {
                w.1 = w.1.max(b);
                if let Some(s) = texts.last_mut() {
                    s.push_str(t);
                }
            }
            _ => {
                inked.push((a, b, y));
                texts.push(t.to_string());
            }
        }
        last = Some((y, size));
    }
    // The figures of each column: the body's words by their centres.
    let body: Vec<(f64, f64, &str)> = inked
        .iter()
        .zip(&texts)
        .filter(|(&(_, _, y), _)| y >= g.ys[1] && y < bottom)
        .map(|(&(a, b, _), t)| (a, b, t.as_str()))
        .collect();
    // The body's runs: its words on one line a cell's gap apart joined.
    let runs: Vec<(f64, f64, f64)> = {
        let mut lines: Vec<(f64, f64, f64)> = inked
            .iter()
            .filter(|&&(_, _, y)| y >= g.ys[1] && y < bottom)
            .copied()
            .collect();
        lines.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.0.total_cmp(&b.0)));
        let mut runs: Vec<(f64, f64, f64)> = Vec::new();
        for (a, b, y) in lines {
            match runs.last_mut() {
                Some(r) if (y - r.2).abs() <= fs * 0.3 && a - r.1 <= fs * TABLE_CELL_GAP => {
                    r.1 = r.1.max(b)
                }
                _ => runs.push((a, b, y)),
            }
        }
        runs
    };
    // Taken at their median start and end: a figure the edges set in the
    // wrong column stretches no column.
    let mut words: Vec<(Vec<f64>, Vec<f64>)> = vec![(Vec::new(), Vec::new()); cols];
    let mut numeric = vec![0usize; cols];
    for &(a, b, t) in &body {
        let m = (a + b) / 2.0;
        if let Some(c) = g.xs.windows(2).position(|w| m >= w[0] && m < w[1]) {
            words[c].0.push(a);
            words[c].1.push(b);
            let t: String = t.chars().map(|c| if c == '–' { '-' } else { c }).collect();
            numeric[c] += usize::from(is_numeric_cell(&t) || is_amount_unit(&t));
        }
    }
    // Columns of figures past the first, which may be a stub of labels: most
    // of each one's words are figures, or the units a statement sets over
    // them (`£m`). Text set in panels under their titles lines up as well,
    // word for word, and is no table of heads over figures.
    if (1..cols).any(|c| numeric[c] * 2 <= words[c].0.len()) {
        return;
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let mut figures: Vec<(f64, f64)> = Vec::with_capacity(cols);
    for (mut starts, mut ends) in words {
        if starts.is_empty() {
            return;
        }
        figures.push((median(&mut starts), median(&mut ends)));
    }
    // The header row's runs, and any just past the first edge.
    let mut heads: Vec<(f64, f64)> = Vec::new();
    {
        let mut row: Vec<(f64, f64)> = inked
            .iter()
            .filter(|&&(a, b, y)| {
                y > top - fs * 0.5 && y < g.ys[1] && b > g.xs[0] - fs * 4.0 && a < g.xs[cols]
            })
            .map(|&(a, b, _)| (a, b))
            .collect();
        if inked
            .iter()
            .any(|&(a, b, y)| y > top && y < g.ys[1] && a < g.xs[cols] && b > g.xs[0] - fs * 4.0)
            && head_runs(g, glyphs, 0).is_empty()
        {
            return;
        }
        row.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (a, b) in row {
            match heads.last_mut() {
                Some(h) if a - h.1 <= fs * TABLE_CELL_GAP => h.1 = h.1.max(b),
                _ => heads.push((a, b)),
            }
        }
    }
    if heads.len() < 2 {
        return;
    }
    // Each head over one column's figures, each column under one head.
    let over = |h: (f64, f64), f: (f64, f64)| h.0 < f.1 && f.0 < h.1;
    let mut owner: Vec<Option<(f64, f64)>> = vec![None; cols];
    for &h in &heads {
        let under: Vec<usize> = (0..cols).filter(|&c| over(h, figures[c])).collect();
        let [c] = under[..] else {
            return;
        };
        if owner[c].is_some() {
            return;
        }
        owner[c] = Some(h);
    }
    // Some head must stand outside its column's edges, or nothing is cut.
    let cut =
        (0..cols).any(|c| owner[c].is_some_and(|h| h.0 < g.xs[c] - 0.5 || h.1 > g.xs[c + 1] + 0.5));
    if !cut {
        return;
    }
    let block = |c: usize| match owner[c] {
        Some(h) => (h.0.min(figures[c].0), h.1.max(figures[c].1)),
        None => figures[c],
    };
    let mut xs = g.xs.clone();
    for k in 1..cols {
        let (left, right) = (block(k - 1).1, block(k).0);
        let edge = (left + right) / 2.0;
        // No run of the body's words cut through, words a cell's gap apart
        // one run: a chart sets a figure at its bar's end, and a negative
        // bar's figure stands out in the gutter on one side of the edge or
        // the other; a line of text run past the edge is a cell's.
        let clear = !runs.iter().any(|&(a, b, _)| a < edge && b > edge);
        if right - left < fs * ALIGNED_SPLIT_GAP_EMS || !clear {
            return;
        }
        xs[k] = edge;
    }
    xs[0] = xs[0].min(block(0).0 - 0.5);
    xs[cols] = xs[cols].max(block(cols - 1).1 + 0.5);
    // One figure to a column on each line, by the fitted edges: a chart's
    // axis sets its ticks along one line (`5 4 3 2 1 0`), runs apart in one
    // column, where a table sets one figure to a cell. (By the old edges, a
    // negative bar's figure falls in the column before, beside that
    // column's own.)
    let column = |a: f64, b: f64| {
        let m = (a + b) / 2.0;
        xs.windows(2).position(|w| m >= w[0] && m < w[1])
    };
    let mut prev: Option<(usize, f64)> = None;
    for &(a, b, y) in &runs {
        let Some(c) = column(a, b) else {
            continue;
        };
        if prev.is_some_and(|(pc, py)| pc == c && (y - py).abs() <= fs * 0.3) {
            return;
        }
        prev = Some((c, y));
    }
    g.bbox.x0 = xs[0];
    g.bbox.x1 = xs[cols];
    g.xs = xs;
}

/// The unit a statement sets over a column of amounts: a currency with its
/// scale (`£m`, `$000`, `(€bn)`), or a percentage sign.
fn is_amount_unit(t: &str) -> bool {
    let t = t.trim().trim_start_matches('(').trim_end_matches(')');
    if t == "%" {
        return true;
    }
    t.strip_prefix(['£', '$', '€', '¥'])
        .is_some_and(|scale| matches!(scale, "m" | "mn" | "bn" | "k" | "000" | "'000" | "’000"))
}

/// Move an aligned grid's column edges out from under its heads where a
/// head is set right-aligned over its figures and wider than them (`Montant
/// EUR HT` over `24.99`): the column starts where its figures do, and the
/// head reaches back over the gutter, cut through by the edge. Where the
/// rows are labelled in a stub, the head ends where the column's figures
/// end, it reaches back a letter's width or more, and nothing of the column
/// before stands between its start and the edge, the edge moves into the
/// gutter before the head, midway to the text before it.
fn fit_right_aligned_heads(g: &mut Grid, glyphs: &[crate::glyph::Glyph]) {
    if g.layout.is_some() || g.rows() < 2 || g.cols() < 2 {
        return;
    }
    let cols = g.cols();
    let runs = head_runs(g, glyphs, 0);
    if runs.is_empty() {
        return;
    }
    // Amounts beside a stub of labels, as an invoice or a statement sets
    // them: most body rows name themselves in words in the first column.
    let cells = g.cell_texts(glyphs);
    let named = (1..g.rows())
        .filter(|&r| {
            cells[r * cols]
                .trim_start()
                .starts_with(char::is_alphabetic)
        })
        .count();
    if named < 2 || named * 2 < g.rows() - 1 {
        return;
    }
    let (y1, bottom) = (g.ys[1], g.ys[g.rows()]);
    let body: Vec<(f64, f64)> = glyphs
        .iter()
        .filter(|gl| !gl.text.trim().is_empty() && gl.is_horizontal())
        .filter_map(|gl| {
            let b = gl.bbox?;
            let (x, y) = gl.center();
            (x >= g.xs[0] && x <= g.xs[cols] && y >= y1 && y < bottom).then_some((b.x0, b.x1))
        })
        .collect();
    for k in 1..cols {
        let edge = g.xs[k];
        let Some(&(a, b)) = runs.iter().find(|&&(a, b)| a < edge && b > edge) else {
            continue;
        };
        if b > g.xs[k + 1] {
            continue;
        }
        // Set right-aligned over the column's figures: the head ends where
        // they end, within a letter's width. A head centred over them is
        // another layout's.
        let fs = head_size(glyphs, g.ys[0], g.ys[1]);
        // Reaching back over the gutter, not set at the edge a hair before it.
        if edge - a < fs {
            continue;
        }
        let right = body
            .iter()
            .filter(|&&(s, e)| (s + e) / 2.0 >= edge && (s + e) / 2.0 < g.xs[k + 1])
            .map(|&(_, e)| e)
            .fold(f64::MIN, f64::max);
        if right == f64::MIN || (b - right).abs() > fs {
            continue;
        }
        // The text before the head: the body's and the head's own.
        let before = body
            .iter()
            .chain(runs.iter())
            .filter(|&&(_, e)| e < a)
            .map(|&(_, e)| e)
            .fold(g.xs[k - 1], f64::max);
        // Nothing of the column before reaches under the head; the
        // column's own figures may start a hair before its edge.
        let clear = !body.iter().any(|&(s, e)| (s + e) / 2.0 < edge && e > a);
        // Into a gutter, as wide as one the columns are parted by: letters
        // set apart in a word (`D ECIMAL`) leave a narrower gap.
        if clear && a - before >= fs * ALIGNED_SPLIT_GAP_EMS {
            g.xs[k] = (before + a) / 2.0;
        }
    }
}

/// Take into an accepted grid the column banner set just above it.
///
/// An unruled table's stacked header (`EUR` over `Unit` and `Total`) puts
/// the banner on a line of its own, one cell wide, and an aligned run is
/// rows of two cells or more, so the banner was left out as a paragraph and
/// the columns it names lost it. The line above is a banner when it is
/// bold, sits within half a line of the table, stays inside its width, and
/// each label on it starts on an inner column boundary and spans two
/// columns or more. A heading wrapped onto two lines over one column
/// (`Contour` over `Farming`) spans one and stays a paragraph.
fn extend_by_banner(g: &mut Grid, rows: &[Vec<&crate::glyph::Glyph>]) {
    if g.xs.len() < 4 {
        return;
    }
    let top = g.ys[0];
    let Some(row) = rows.iter().rev().find(|r| {
        let y1 = r
            .iter()
            .map(|x| x.bbox.unwrap().y1)
            .fold(f64::MIN, f64::max);
        y1 <= top + 1.5
    }) else {
        return;
    };
    let fs = row[0].font_size.max(1.0) as f64;
    let y0 = row
        .iter()
        .map(|x| x.bbox.unwrap().y0)
        .fold(f64::MAX, f64::min);
    let y1 = row
        .iter()
        .map(|x| x.bbox.unwrap().y1)
        .fold(f64::MIN, f64::max);
    let x0 = row
        .iter()
        .map(|x| x.bbox.unwrap().x0)
        .fold(f64::MAX, f64::min);
    let x1 = row
        .iter()
        .map(|x| x.bbox.unwrap().x1)
        .fold(f64::MIN, f64::max);
    let close = top - y1 <= 0.5 * fs;
    let bold = row.iter().all(|x| x.weight.unwrap_or(400) >= 600);
    let inside = x1 <= g.bbox.x1 + 1.0;
    let starts = cell_starts(row);
    let aligned = starts.iter().all(|s| {
        g.xs[1..g.xs.len() - 1]
            .iter()
            .any(|b| (b - s).abs() <= fs * COLUMN_ALIGN_TOLERANCE)
    });
    // Each label spans two columns or more before the next one starts.
    let ncols = g.xs.len() - 1;
    let cols_at: Vec<usize> = starts
        .iter()
        .map(|s| {
            g.xs.windows(2)
                .position(|w| {
                    *s >= w[0] - fs * COLUMN_ALIGN_TOLERANCE
                        && *s < w[1] - fs * COLUMN_ALIGN_TOLERANCE
                })
                .unwrap_or(0)
        })
        .collect();
    let spans = cols_at.iter().enumerate().all(|(i, k)| {
        let next = cols_at.get(i + 1).copied().unwrap_or(ncols);
        next >= k + 2
    });
    if close && bold && inside && aligned && spans && x0 > g.xs[1] - fs * COLUMN_ALIGN_TOLERANCE {
        g.ys.insert(0, y0 - 1.0);
        g.bbox.y0 = y0 - 1.0;
    }
}

/// Stretches of consecutive rows that break into the same number of
/// cells, as grids: at least [`MIN_ALIGNED_ROWS`] of them, or two rows of
/// two cells, which are a field list already (`No. | WB-66086` over `Date |
/// 11 August 2026`).
fn aligned_parts(rows: &[Vec<&crate::glyph::Glyph>], cells: &[Vec<f64>], page: usize) -> Vec<Grid> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < rows.len() {
        let n = cells[i].len();
        let mut j = i;
        while j + 1 < rows.len() && cells[j + 1].len() == n {
            j += 1;
        }
        let min_rows = if n == 2 { 2 } else { MIN_ALIGNED_ROWS };
        if n >= 2 && j - i + 1 >= min_rows {
            // A header row over a column of row labels leaves its corner
            // empty, so it breaks into one cell fewer than the rows under
            // it and belongs to them.
            let start = if n >= 3 && i > 0 && cells[i - 1].len() + 1 == n {
                i - 1
            } else {
                i
            };
            let (rs, cs) = (&rows[start..=j], &cells[start..=j]);
            // Starts find no column whose cells begin in different places
            // (text left-set, figures right-set); rank still does.
            out.extend(grid_from_cells(rs, cs, page).or_else(|| grid_by_rank(rs, cs, page)));
        }
        i = j + 1;
    }
    out
}

/// A grid whose columns are placed by rank alone, for rows that all break
/// into the same number of cells but whose cells start nowhere in common.
/// Only [`aligned_parts`] asks for one, and only a typeset table accepts it.
fn grid_by_rank(
    rows: &[Vec<&crate::glyph::Glyph>],
    cells: &[Vec<f64>],
    page: usize,
) -> Option<Grid> {
    let xs = columns_by_rank(rows, cells)?;
    if xs.len() < 3 {
        return None;
    }
    let mut ys: Vec<f64> = rows
        .iter()
        .map(|r| {
            r.iter()
                .map(|g| g.bbox.unwrap().y0)
                .fold(f64::MAX, f64::min)
                - 1.0
        })
        .collect();
    let y_hi = rows
        .last()?
        .iter()
        .map(|g| g.bbox.unwrap().y1)
        .fold(f64::MIN, f64::max);
    ys.push(y_hi + 1.0);
    let ys = join_wrapped_rows(ys);
    Some(Grid {
        layout: None,
        page,
        bbox: BBox {
            x0: xs[0],
            y0: ys[0],
            x1: xs[xs.len() - 1],
            y1: ys[ys.len() - 1],
        },
        xs,
        ys,
    })
}

/// Longest label, in words, of a field in an unruled field list.
///
/// Receipts' labels run to three (`Price each`, `VAT 20%`, `Amount due`). A
/// bold caption wrapped down a column beside prose has the same weights as
/// a field list, label bold and value plain on every line; its lines run
/// on, five words and more.
const MAX_FIELD_LABEL_WORDS: usize = 4;

/// An aligned run that is set as a table: a bold header row naming every
/// column over plain rows, with at least one column of numbers — or, in two
/// columns, a short bold label beside a plain value on every row, under a
/// bold header row or none (`Number | WB-47843`, `Quantity | 2`), or a
/// label of any weight that a colon ends ([`colon_fields`]).
///
/// A table printed with no rules at all (`border: none`, which is how many
/// receipts and invoices are styled) leaves alignment as its only drawn
/// trace, and alignment alone is what prose broken into columns also has —
/// accepting it ungated cost reading order across the corpus. Type is the
/// evidence that remains: a row of bold labels across three or more
/// columns, each label different, the rows under it plain, and figures
/// under at least one label. Prose set in columns has no such row. A field
/// list's values are as often text as figures (a number, a date, an item,
/// a code), so in two columns the labels carry the evidence alone.
/// An aligned run whose type says what it is — a transposed table's two
/// axes, or a field list's bold labels — which a reading from alignment
/// alone would take for an ordinary table with a header.
pub fn typed_layout(g: &Grid, glyphs: &[crate::glyph::Glyph]) -> bool {
    has_two_axis_header(g, glyphs) || (g.cols() == 2 && has_typeset_header(g, glyphs))
}

fn has_typeset_header(g: &Grid, glyphs: &[crate::glyph::Glyph]) -> bool {
    let (rows, cols) = (g.rows(), g.cols());
    if cols == 2 && rows >= 2 {
        let bold = |(weighted, heavy): (usize, usize)| weighted > 0 && heavy * 2 > weighted;
        let w0 = g.cell_weights(glyphs, 0);
        let first = usize::from(bold(w0[0]) && bold(w0[1]));
        let cells = g.cell_texts(glyphs);
        let labelled = (first..rows).all(|r| {
            let w = g.cell_weights(glyphs, r);
            bold(w[0])
                && w[1].0 > 0
                && !bold(w[1])
                && cells[r * 2].split_whitespace().count() <= MAX_FIELD_LABEL_WORDS
        });
        return (rows - first >= 2 && labelled)
            || (colon_fields(&cells) && !wraps_under_labels(g, glyphs));
    }
    if cols < 3 || rows < 3 {
        return false;
    }
    if !g.cells_are_bold(glyphs, 0) || (1..rows).any(|r| g.row_is_bold(glyphs, r)) {
        return false;
    }
    let cells = g.cell_texts(glyphs);
    let at = |r: usize, c: usize| cells[r * cols + c].trim();
    // Each label names its own column; a chart's axis titles repeat.
    let labels = (0..cols).all(|c| {
        !at(0, c).is_empty() && !is_numeric_cell(at(0, c)) && (0..c).all(|k| at(0, k) != at(0, c))
    });
    let figures = (0..cols).any(|c| {
        let body: Vec<&str> = (1..rows)
            .map(|r| at(r, c))
            .filter(|t| !t.is_empty())
            .collect();
        !body.is_empty() && body.iter().all(|t| is_numeric_cell(t))
    });
    labels && figures
}

/// Longest label, in words, of a field whose label a colon ends. A colon
/// says where the label stops, so it may run longer than a bold one
/// (`Supplied with cap fitted:`); a lead-in sentence ending in one runs
/// longer still.
const MAX_COLON_LABEL_WORDS: usize = 6;

/// Two columns of fields whose labels a colon ends, `Weight: | 24 g` down a
/// spec sheet, set plain as often as bold.
///
/// Weight is no evidence there, and alignment alone is what prose set in two
/// columns has too. The colon is: prose does not end a line with one beside
/// another column row after row. A label's colon follows a word (`Weight:`,
/// `No.:`, `Liabilities ($):`), not a figure, so a schedule's `10:30` is no
/// label. Each row has a value beside its label, and each label is short and
/// holds its colon. One that ran on to the next label's first word
/// (`Closure Type: Screw/` over `Snap Fit:`) does not end with it,
/// but most do: `Patient: A. Sample | DOB: 1987` is two fields to a row,
/// not a label beside its value. More than one word after the colon is a
/// value wrapped back under its label, and the row is no field.
///
/// A value is a word, a figure or a phrase. A note's is a sentence:
/// `NOTE: | Output is measured in finished units. All series are…` over `SOURCE: | …`
/// under a chart is its caption, set with a hanging indent.
fn colon_fields(cells: &[String]) -> bool {
    let rows = cells.len() / 2;
    if rows < 2 {
        return false;
    }
    let (mut ending, mut sentences) = (0, 0);
    for r in 0..rows {
        let (label, value) = (cells[r * 2].trim(), cells[r * 2 + 1].trim());
        if value.is_empty() || label.split_whitespace().count() > MAX_COLON_LABEL_WORDS {
            return false;
        }
        match label_colon(label) {
            None => return false,
            Some(last) => ending += usize::from(last),
        }
        sentences += usize::from(
            value.ends_with('.') && value.split_whitespace().count() > MAX_COLON_LABEL_WORDS,
        );
    }
    ending * 2 > rows && sentences * 2 <= rows
}

/// Where a field label's colon is: `None` when it has none after a word, or
/// more than one word follows it; else whether it ends the label.
fn label_colon(label: &str) -> Option<bool> {
    let label = label.trim_end();
    let cs: Vec<char> = label.chars().collect();
    let at = (1..cs.len()).rfind(|&i| {
        cs[i] == ':' && (cs[i - 1].is_alphabetic() || matches!(cs[i - 1], ')' | '.'))
    })?;
    let after: String = cs[at + 1..].iter().collect();
    match after.split_whitespace().count() {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

/// Runs of [`colon_fields`] found row by row, each row's glyphs in x order.
///
/// [`aligned_rows`] keeps a row's glyphs in the order of their baselines,
/// so a value set a hundredth of a point above its label comes first
/// (`38 mm` before `Neck Diameter:`), the label seems to start before the value
/// ends, and the row reads as one cell. A spec sheet that sets each value
/// on its own baseline breaks its field list into pieces too short to be
/// found, and its labels and values are read as two columns of text.
///
/// Only field lists are read from the sorted rows. Over every aligned run
/// the same order turns more multi-column prose into tables — two columns
/// of a report, three panels of a slide — than the gates of
/// [`detect_aligned`] can turn away.
fn colon_field_lists(
    glyphs: &[crate::glyph::Glyph],
    rows: &[Vec<&crate::glyph::Glyph>],
    page: usize,
) -> Vec<Grid> {
    let mut rows = rows.to_vec();
    for r in rows.iter_mut() {
        r.sort_by(|a, b| a.bbox.unwrap().x0.total_cmp(&b.bbox.unwrap().x0));
    }
    let cells: Vec<Vec<f64>> = rows.iter().map(|r| cell_starts(r)).collect();
    // A row is a field when it breaks into a label and a value, and the
    // label holds a colon after a word.
    let field = |i: usize| {
        cells[i].len() == 2 && {
            let split = cells[i][1] - 0.01;
            let label: String = rows[i]
                .iter()
                .filter(|g| g.bbox.unwrap().x0 < split)
                .map(|g| g.text.as_str())
                .collect();
            label_colon(&label).is_some()
        }
    };
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < rows.len() {
        if !field(i) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j + 1 < rows.len() && field(j + 1) {
            j += 1;
        }
        if j > i {
            if let Some(g) = grid_from_cells(&rows[i..=j], &cells[i..=j], page) {
                if g.cols() == 2
                    && colon_fields(&g.cell_texts(glyphs))
                    && !wraps_under_labels(&g, glyphs)
                {
                    out.push(g);
                }
            }
        }
        i = j + 1;
    }
    out
}

/// Fewest fields a list whose labels end in no colon is read from. Two rows
/// that happen to break at one place are common; three are a list.
const MIN_PLAIN_FIELDS: usize = 3;

/// Most words a field's value holds: a word, a figure or a phrase.
const MAX_FIELD_VALUE_WORDS: usize = 12;

/// Farthest apart, in font sizes, two fields of one list set their
/// baselines. A list's rows sit a line apart; a blank line between two
/// rows parts two lists, or a list from what follows it.
const MAX_FIELD_PITCH: f64 = 2.2;

/// How far, in font sizes, a field's label or value may start (or, set
/// right, end) from where the list's others do.
const FIELD_ALIGN_TOLERANCE: f64 = 0.6;

/// Largest share of the list's size a mark set on its own baseline above a
/// field may have: a raised `®` set smaller, beside the label below it.
const RAISED_MARK_SIZE: f64 = 0.8;

/// A run of glyphs on one baseline that the gap parting cells leaves
/// whole: a cell's text on its row.
struct Seg {
    x0: f64,
    x1: f64,
    ids: Vec<usize>,
}

/// The page's glyphs as rows by baseline, by their indices in x order, and
/// each row parted into cells at gaps wider than [`TABLE_CELL_GAP`]. Empty
/// where the page has too little text to hold a list.
fn segment_rows(glyphs: &[crate::glyph::Glyph]) -> (Vec<Vec<usize>>, Vec<Vec<Seg>>) {
    let bb = |i: usize| glyphs[i].bbox.unwrap();
    let mut ids: Vec<usize> = (0..glyphs.len())
        .filter(|&i| {
            let g = &glyphs[i];
            g.bbox.is_some() && g.is_horizontal() && !g.text.trim().is_empty()
        })
        .collect();
    if ids.len() < MIN_PLAIN_FIELDS * 2 {
        return (Vec::new(), Vec::new());
    }
    ids.sort_by(|&a, &b| {
        glyphs[a]
            .origin
            .1
            .total_cmp(&glyphs[b].origin.1)
            .then(glyphs[a].origin.0.total_cmp(&glyphs[b].origin.0))
    });
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for i in ids {
        let g = &glyphs[i];
        let fs = g.font_size.max(1.0) as f64;
        match rows.last_mut() {
            Some(r) if (glyphs[r[0]].origin.1 - g.origin.1).abs() < fs * ROW_BASELINE_TOLERANCE => {
                r.push(i)
            }
            _ => rows.push(vec![i]),
        }
    }
    let segs: Vec<Vec<Seg>> = rows
        .iter_mut()
        .map(|r| {
            r.sort_by(|&a, &b| bb(a).x0.total_cmp(&bb(b).x0));
            let mut out: Vec<Seg> = Vec::new();
            // Text after a run of dot leaders starts a cell, as in
            // [`cell_starts`].
            let row: Vec<&crate::glyph::Glyph> = r.iter().map(|&i| &glyphs[i]).collect();
            let contents = crate::textgrid::contents_entry(&row);
            let mut run = crate::textgrid::LeaderRun::default();
            for &i in r.iter() {
                let fs = glyphs[i].font_size.max(1.0) as f64;
                let after_leaders = run.step(&glyphs[i]) >= crate::textgrid::LEADER_DOTS
                    && crate::textgrid::leader_dots(glyphs[i].text.trim()) == 0
                    && !contents;
                match out.last_mut() {
                    Some(s) if bb(i).x0 - s.x1 <= fs * TABLE_CELL_GAP && !after_leaders => {
                        s.x1 = s.x1.max(bb(i).x1);
                        s.ids.push(i);
                    }
                    _ => out.push(Seg {
                        x0: bb(i).x0,
                        x1: bb(i).x1,
                        ids: vec![i],
                    }),
                }
            }
            out
        })
        .collect();
    (rows, segs)
}

/// A page's text read once for [`field_lists_in`] and [`column_tables_in`]:
/// its rows by baseline, each parted into cells, and its spaces.
pub struct PageRows {
    rows: Vec<Vec<usize>>,
    segs: Vec<Vec<Seg>>,
    spaces: Spaces,
}

impl PageRows {
    pub fn new(glyphs: &[crate::glyph::Glyph]) -> Self {
        let (rows, segs) = segment_rows(glyphs);
        PageRows {
            rows,
            segs,
            spaces: Spaces::new(glyphs),
        }
    }
}

/// A field of a list found by [`field_lists`]: its row, then the glyphs of
/// its label and of its value.
type Field = (usize, Vec<usize>, Vec<usize>);

/// Lists of fields set without rules, `Label | value` row after row, found
/// within the page's columns.
///
/// [`colon_field_lists`] reads rows across the whole page, so a list set
/// in one of three page columns (`OTC | Yes` beside a column of prose on
/// either side) never makes a row of two cells, and a list that sets no
/// colon after its labels (`CP | Polished Chrome`) has nothing there to
/// show it is one. Here a list is looked for between the page's gutters
/// (`columns`, as [`crate::column::detect`] finds them on `glyphs`): it starts at a row whose label opens its
/// column, the value beside it reaching to the next gutter, and goes on
/// down every row that puts a label where it did and a value where its
/// first value starts or ends (values set right, `Yes` and `#142, 143`).
/// A row with nothing between those gutters is another column's, and is
/// passed over. A value set closer to a long label than the gap that parts
/// cells is parted from it where the list's values start.
///
/// A list whose labels end in colons is held to [`colon_fields`]. One
/// without them is held to [`plain_fields`]: three rows at least, short
/// labels that are not prose's lines.
///
/// The table comes out read: each cell holds its own glyphs (indices into
/// `glyphs`) and it has no header, since its first row is a field like the
/// others.
pub fn field_lists(
    glyphs: &[crate::glyph::Glyph],
    page: usize,
    columns: &[crate::column::Column],
) -> Vec<Grid> {
    field_lists_in(glyphs, page, columns, &PageRows::new(glyphs))
}

/// As [`field_lists`], from the page's rows read already.
pub fn field_lists_in(
    glyphs: &[crate::glyph::Glyph],
    page: usize,
    columns: &[crate::column::Column],
    text: &PageRows,
) -> Vec<Grid> {
    let bb = |i: usize| glyphs[i].bbox.unwrap();
    let (rows, segs, spaces) = (&text.rows, &text.segs, &text.spaces);
    if rows.is_empty() {
        return Vec::new();
    }
    let baseline = |r: usize| glyphs[rows[r][0]].origin.1;
    let cuts: Vec<f64> = columns.iter().skip(1).map(|c| c.x0).collect();
    // Runs that share rows share their cells: each cell's text is read once.
    let mut read: rustc_hash::FxHashMap<Vec<usize>, String> = Default::default();
    let mut used: Vec<Vec<bool>> = segs.iter().map(|r| vec![false; r.len()]).collect();
    let mut out = Vec::new();
    for i in 0..rows.len() {
        for k in 0..segs[i].len().saturating_sub(1) {
            if used[i][k] {
                continue;
            }
            let (label, value) = (&segs[i][k], &segs[i][k + 1]);
            // The label opens its column, and the value is the last cell
            // before the next gutter.
            let lo = cuts
                .iter()
                .rev()
                .find(|&&c| c <= label.x0 + 0.5)
                .copied()
                .unwrap_or(f64::MIN);
            let hi = cuts
                .iter()
                .find(|&&c| c > value.x0)
                .copied()
                .unwrap_or(f64::MAX);
            if (k > 0 && segs[i][k - 1].x1 > lo) || segs[i].get(k + 2).is_some_and(|n| n.x0 < hi) {
                continue;
            }
            let fs = glyphs[label.ids[0]].font_size.max(1.0) as f64;
            let tol = fs * FIELD_ALIGN_TOLERANCE;
            let (lx, vx0, vx1) = (label.x0, value.x0, value.x1);
            let mut fields: Vec<Field> = vec![(i, label.ids.clone(), value.ids.clone())];
            let mut taken: Vec<(usize, Vec<usize>)> = vec![(i, vec![k, k + 1])];
            let mut marks: Vec<usize> = Vec::new();
            let mut last = baseline(i);
            for (j, row) in segs.iter().enumerate().skip(i + 1) {
                if baseline(j) - last > fs * MAX_FIELD_PITCH {
                    break;
                }
                let inside: Vec<usize> = (0..row.len())
                    .filter(|&s| row[s].x0 < hi && row[s].x1 > lo)
                    .collect();
                if inside.is_empty() {
                    continue;
                }
                let small = |s: usize| {
                    row[s]
                        .ids
                        .iter()
                        .all(|&g| (glyphs[g].font_size as f64) < fs * RAISED_MARK_SIZE)
                };
                if inside.iter().all(|&s| small(s)) {
                    marks.extend(inside.iter().flat_map(|&s| row[s].ids.iter().copied()));
                    continue;
                }
                let at_label = |s: &Seg| (s.x0 - lx).abs() <= tol;
                let found = match inside[..] {
                    [a, b]
                        if at_label(&row[a])
                            && ((row[b].x0 - vx0).abs() <= tol
                                || (row[b].x1 - vx1).abs() <= tol) =>
                    {
                        Some((row[a].ids.clone(), row[b].ids.clone()))
                    }
                    // A value set closer to its label than cells are parted:
                    // part them where the list's values start.
                    [a] if at_label(&row[a]) => {
                        let g = &row[a].ids;
                        (1..g.len())
                            .find(|&m| {
                                (bb(g[m]).x0 - vx0).abs() <= tol
                                    && bb(g[m]).x0 - bb(g[m - 1]).x1 >= fs * 0.15
                            })
                            .map(|m| (g[..m].to_vec(), g[m..].to_vec()))
                    }
                    _ => None,
                };
                let Some((mut l, mut v)) = found else {
                    break;
                };
                // A raised mark is its own line's, set above its baseline.
                let split = (bb(*l.last().unwrap()).x1 + bb(v[0]).x0) / 2.0;
                for m in marks.drain(..) {
                    if bb(m).x0 < split {
                        l.push(m);
                    } else {
                        v.push(m);
                    }
                }
                fields.push((j, l, v));
                taken.push((j, inside.clone()));
                last = baseline(j);
            }
            let n = fields.len();
            // Under three rows a list is read on its colons alone, and
            // every label must hold one.
            if n < MIN_PLAIN_FIELDS
                && fields
                    .iter()
                    .any(|f| !f.1.iter().any(|&i| glyphs[i].text.contains(':')))
            {
                continue;
            }
            if let Some(g) = field_grid(glyphs, &fields, page, spaces) {
                let texts: Vec<String> = g
                    .layout
                    .as_ref()
                    .unwrap()
                    .cells
                    .iter()
                    .map(|ids| {
                        read.entry(ids.clone())
                            .or_insert_with(|| cell_text(glyphs, ids))
                            .clone()
                    })
                    .collect();
                // Labels on both sides (`Conf: | Conf:`) are two blocks of
                // fields side by side, not one.
                let paired = texts
                    .chunks(2)
                    .filter(|c| label_colon(c[1].trim()) == Some(true))
                    .count()
                    * 2
                    >= n;
                let colon = n >= 2 && !paired && colon_fields(&texts);
                let plain =
                    n >= MIN_PLAIN_FIELDS && !paired && plain_fields(glyphs, &fields, &texts);
                if (colon || plain) && !wraps_under_labels(&g, glyphs) {
                    let mut g = g;
                    if plain && !colon && names_its_columns(&texts) {
                        if let Some(lay) = g.layout.as_mut() {
                            std::sync::Arc::make_mut(lay).header_rows = 1;
                        }
                    }
                    for (j, ss) in taken {
                        for s in ss {
                            used[j][s] = true;
                        }
                    }
                    out.push(g);
                }
            }
        }
    }
    out
}

/// Widest share of its page column a line across a gutter from a table may
/// take and still be one of the table's cells, not the next column's text.
const CELL_ACROSS_GUTTER: f64 = 0.35;

/// Fewest columns a table read by [`column_tables`] has; two are a field
/// list's, which [`field_lists`] reads.
const MIN_COLUMN_TABLE_COLUMNS: usize = 3;

/// Tables of three columns or more set without rules in one column of a page
/// of several: a paint data sheet's `Base | oz. per gallon | Strength` over
/// its bases, in the first of three page columns.
///
/// Across the page each of their rows shares its baseline with the other
/// columns' lines, so the aligned reader never finds them a run. Here, as in
/// [`field_lists`], rows are read between the page's gutters. A table starts
/// at a row whose first cell opens its page column, three cells or more
/// before the next gutter, and runs on down every row with as many cells,
/// each set where the first row's is: starting, ending or centred with it,
/// since a header is often centred over figures set right
/// (`oz. per gallon:` over `0-7` and `10-20`). A row with nothing in that
/// column is another column's, and passed over.
///
/// A table wider than one column has gutters of its own, wide gaps between
/// its label column and its figures, and they read as the page's. So a run
/// that has short cells just across a gutter on half its rows or more runs
/// on past it, and is not read here.
///
/// It is held to what a table shows and prose in a column does not: at
/// least three rows of short cells, few of them sentences, and a header row
/// (labels each ending in a colon, or bold over rows that are not, or
/// naming columns of quantities) or a column of quantities.
///
/// Only on pages of several columns: on a page of one, the aligned reader
/// already reads each row whole.
pub fn column_tables(
    glyphs: &[crate::glyph::Glyph],
    page: usize,
    columns: &[crate::column::Column],
) -> Vec<Grid> {
    column_tables_in(glyphs, page, columns, &PageRows::new(glyphs))
}

/// As [`column_tables`], from the page's rows read already.
pub fn column_tables_in(
    glyphs: &[crate::glyph::Glyph],
    page: usize,
    columns: &[crate::column::Column],
    text: &PageRows,
) -> Vec<Grid> {
    if columns.len() < 2 {
        return Vec::new();
    }
    let (rows, segs, spaces) = (&text.rows, &text.segs, &text.spaces);
    if rows.is_empty() {
        return Vec::new();
    }
    let baseline = |r: usize| glyphs[rows[r][0]].origin.1;
    let cuts: Vec<f64> = columns.iter().skip(1).map(|c| c.x0).collect();
    let mut used: Vec<Vec<bool>> = segs.iter().map(|r| vec![false; r.len()]).collect();
    let mut out = Vec::new();
    for i in 0..rows.len() {
        let mut k = 0;
        while k < segs[i].len() {
            if used[i][k] {
                k += 1;
                continue;
            }
            let first = &segs[i][k];
            let lo = cuts
                .iter()
                .rev()
                .find(|&&c| c <= first.x0 + 0.5)
                .copied()
                .unwrap_or(f64::MIN);
            let hi = cuts
                .iter()
                .find(|&&c| c > first.x0)
                .copied()
                .unwrap_or(f64::MAX);
            let w = segs[i][k..].iter().take_while(|s| s.x0 < hi).count();
            if (k > 0 && segs[i][k - 1].x1 > lo) || w < MIN_COLUMN_TABLE_COLUMNS {
                k += 1;
                continue;
            }
            let fs = glyphs[first.ids[0]].font_size.max(1.0) as f64;
            let tol = fs * FIELD_ALIGN_TOLERANCE;
            let refs: Vec<(f64, f64)> = segs[i][k..k + w].iter().map(|s| (s.x0, s.x1)).collect();
            let aligned = |s: &Seg, m: usize| {
                let (a, b) = refs[m];
                (s.x0 - a).abs() <= tol
                    || (s.x1 - b).abs() <= tol
                    || ((s.x0 + s.x1) - (a + b)).abs() / 2.0 <= tol
            };
            let mut table: Vec<Vec<Vec<usize>>> =
                vec![segs[i][k..k + w].iter().map(|s| s.ids.clone()).collect()];
            let mut taken: Vec<(usize, Vec<usize>)> = vec![(i, (k..k + w).collect())];
            let mut last = baseline(i);
            for (j, row) in segs.iter().enumerate().skip(i + 1) {
                // A header is often set a line further from the rows under it.
                let pitch = MAX_FIELD_PITCH + if table.len() == 1 { 1.0 } else { 0.0 };
                if baseline(j) - last > fs * pitch {
                    break;
                }
                let inside: Vec<usize> = (0..row.len())
                    .filter(|&s| row[s].x0 < hi && row[s].x1 > lo)
                    .collect();
                if inside.is_empty() {
                    continue;
                }
                if inside.len() != w
                    || !inside.iter().enumerate().all(|(m, &s)| aligned(&row[s], m))
                {
                    break;
                }
                table.push(inside.iter().map(|&s| row[s].ids.clone()).collect());
                taken.push((j, inside));
                last = baseline(j);
            }
            // Short cells set across a gutter on the same rows: the table runs
            // on past it, and its gutter is the table's own.
            let width_at = |x: f64| {
                columns
                    .iter()
                    .find(|c| x >= c.x0 && x < c.x1)
                    .map_or(f64::MAX, |c| c.x1 - c.x0)
            };
            let short = |s: &Seg| s.x1 - s.x0 < width_at(s.x0) * CELL_ACROSS_GUTTER;
            let runs_on = taken
                .iter()
                .filter(|(j, _)| {
                    let row = &segs[*j];
                    let right = row.iter().find(|s| s.x0 >= hi);
                    let left = row.iter().rev().find(|s| s.x1 <= lo);
                    right.is_some_and(short) || left.is_some_and(short)
                })
                .count();
            let accepted = (table.len() >= MIN_PLAIN_FIELDS && runs_on * 2 < table.len())
                .then(|| column_table_header(glyphs, &table))
                .flatten()
                .and_then(|header| layout_grid(glyphs, &table, page, spaces, header));
            match accepted {
                Some(g) => {
                    for (j, ss) in taken {
                        for s in ss {
                            used[j][s] = true;
                        }
                    }
                    out.push(g);
                    k += w;
                }
                _ => k += 1,
            }
        }
    }
    out
}

/// Whether a run of rows read by [`column_tables`] is a table, and if so
/// how many header rows it has: `None` where it is not one.
///
/// Its cells are short, a word, a figure or a phrase, and few are
/// sentences. Its first row is a header when each of its cells is a label
/// ending in a colon, when it is bold over a second row that is not, or when
/// it names columns whose cells are quantities. Without a header it must
/// hold a column of quantities.
fn column_table_header(glyphs: &[crate::glyph::Glyph], table: &[Vec<Vec<usize>>]) -> Option<usize> {
    let texts: Vec<Vec<String>> = table
        .iter()
        .map(|r| {
            r.iter()
                .map(|ids| cell_text(glyphs, ids).trim().to_string())
                .collect()
        })
        .collect();
    let cells = texts.iter().flatten().count();
    let sentences = texts
        .iter()
        .flatten()
        .filter(|t| t.ends_with('.') && t.split_whitespace().count() > MAX_COLON_LABEL_WORDS)
        .count();
    if texts
        .iter()
        .flatten()
        .any(|t| t.split_whitespace().count() > MAX_FIELD_VALUE_WORDS)
        || sentences * 4 > cells
    {
        return None;
    }
    let w = texts[0].len();
    let heavy = |ids: &[usize]| {
        let weighted: Vec<u32> = ids.iter().filter_map(|&i| glyphs[i].weight).collect();
        !weighted.is_empty() && weighted.iter().filter(|&&x| x >= 600).count() * 2 > weighted.len()
    };
    // Columns, below the first row, whose cells are mostly quantities.
    let quantity_columns: Vec<usize> = (0..w)
        .filter(|&c| {
            let below: Vec<&String> = texts[1..].iter().map(|r| &r[c]).collect();
            below.iter().filter(|t| reads_as_quantity(t)).count() * 2 > below.len()
        })
        .collect();
    let colons = texts[0].iter().all(|t| t.ends_with(':'))
        && texts[1..].iter().flatten().all(|t| !t.ends_with(':'));
    let bold = table[0].iter().all(|ids| heavy(ids)) && !table[1].iter().all(|ids| heavy(ids));
    let names = !quantity_columns.is_empty()
        && quantity_columns
            .iter()
            .all(|&c| !texts[0][c].is_empty() && !reads_as_quantity(&texts[0][c]));
    if colons || bold || names {
        Some(1)
    } else if !quantity_columns.is_empty() {
        Some(0)
    } else {
        None
    }
}

/// The grid of a table read cell by cell from its glyphs, row by row: its
/// columns parted halfway between one column's widest cell and the next
/// one's leftmost, its cells holding their glyphs and the spaces between.
fn layout_grid(
    glyphs: &[crate::glyph::Glyph],
    table: &[Vec<Vec<usize>>],
    page: usize,
    spaces: &Spaces,
    header_rows: usize,
) -> Option<Grid> {
    let bb = |i: usize| glyphs[i].bbox.unwrap();
    let span = |ids: &[usize]| {
        ids.iter().fold((f64::MAX, f64::MIN), |(a, b), &i| {
            (a.min(bb(i).x0), b.max(bb(i).x1))
        })
    };
    let w = table.first()?.len();
    let column = |c: usize| -> (f64, f64) {
        table
            .iter()
            .map(|r| span(&r[c]))
            .fold((f64::MAX, f64::MIN), |(a, b), (x0, x1)| {
                (a.min(x0), b.max(x1))
            })
    };
    let mut xs = vec![column(0).0 - 0.5];
    for c in 1..w {
        let cut = (column(c - 1).1 + column(c).0) / 2.0;
        xs.push(cut.max(xs[xs.len() - 1] + 0.1));
    }
    let right = (column(w - 1).1 + 0.5).max(xs[xs.len() - 1] + 0.1);
    xs.push(right);
    let mut ys: Vec<f64> = Vec::with_capacity(table.len() + 1);
    for r in table {
        let top = r
            .iter()
            .flatten()
            .map(|&i| bb(i).y0)
            .fold(f64::MAX, f64::min)
            - 1.0;
        let floor = ys.last().map_or(f64::MIN, |y| y + 0.1);
        ys.push(top.max(floor));
    }
    let bottom = table
        .last()?
        .iter()
        .flatten()
        .map(|&i| bb(i).y1)
        .fold(f64::MIN, f64::max)
        + 1.0;
    ys.push(bottom.max(ys[ys.len() - 1] + 0.1));
    let cells: Vec<Vec<usize>> = table
        .iter()
        .flatten()
        .map(|ids| {
            let mut out = ids.clone();
            out.extend(spaces.within(glyphs, ids[0], span(ids)));
            out
        })
        .collect();
    let mut claimed: Vec<usize> = cells
        .iter()
        .flatten()
        .map(|&i| glyphs[i].draw_index)
        .collect();
    claimed.sort_unstable();
    claimed.dedup();
    Some(Grid {
        page,
        bbox: BBox {
            x0: xs[0],
            y0: ys[0],
            x1: xs[w],
            y1: ys[ys.len() - 1],
        },
        xs,
        ys,
        layout: Some(std::sync::Arc::new(TableLayout {
            header_rows,
            spans: Vec::new(),
            spread: Vec::new(),
            downs: Vec::new(),
            sub_headers: Vec::new(),
            restated_headers: Vec::new(),
            cells,
            claimed,
        })),
    })
}

/// The grid of a field list found by [`field_lists`]: two columns parted
/// between the labels' right edge and the values' left, a row per field,
/// and the cells read from the glyphs each field holds.
fn field_grid(
    glyphs: &[crate::glyph::Glyph],
    fields: &[Field],
    page: usize,
    spaces: &Spaces,
) -> Option<Grid> {
    let bb = |i: usize| glyphs[i].bbox.unwrap();
    let span = |ids: &[usize]| {
        ids.iter().fold((f64::MAX, f64::MIN), |(a, b), &i| {
            (a.min(bb(i).x0), b.max(bb(i).x1))
        })
    };
    let labels: Vec<(f64, f64)> = fields.iter().map(|f| span(&f.1)).collect();
    let values: Vec<(f64, f64)> = fields.iter().map(|f| span(&f.2)).collect();
    let label_x1 = labels.iter().map(|s| s.1).fold(f64::MIN, f64::max);
    let value_x0 = values.iter().map(|s| s.0).fold(f64::MAX, f64::min);
    if fields.len() < 2 || label_x1 >= value_x0 {
        return None;
    }
    let xs = vec![
        labels.iter().map(|s| s.0).fold(f64::MAX, f64::min) - 0.5,
        (label_x1 + value_x0) / 2.0,
        values.iter().map(|s| s.1).fold(f64::MIN, f64::max) + 0.5,
    ];
    let mut ys: Vec<f64> = Vec::with_capacity(fields.len() + 1);
    for f in fields {
        let top =
            f.1.iter()
                .chain(&f.2)
                .map(|&i| bb(i).y0)
                .fold(f64::MAX, f64::min)
                - 1.0;
        let floor = ys.last().map_or(f64::MIN, |y| y + 0.1);
        ys.push(top.max(floor));
    }
    let bottom = fields
        .last()?
        .1
        .iter()
        .chain(&fields.last()?.2)
        .map(|&i| bb(i).y1)
        .fold(f64::MIN, f64::max)
        + 1.0;
    ys.push(bottom.max(ys[ys.len() - 1] + 0.1));
    // The spaces between a cell's words are its own: on its baseline,
    // within its ink.
    let with_spaces = |ids: &Vec<usize>| {
        let mut out = ids.clone();
        out.extend(spaces.within(glyphs, ids[0], span(ids)));
        out
    };
    let cells: Vec<Vec<usize>> = fields
        .iter()
        .flat_map(|f| [with_spaces(&f.1), with_spaces(&f.2)])
        .collect();
    let mut claimed: Vec<usize> = cells
        .iter()
        .flatten()
        .map(|&i| glyphs[i].draw_index)
        .collect();
    claimed.sort_unstable();
    claimed.dedup();
    Some(Grid {
        page,
        bbox: BBox {
            x0: xs[0],
            y0: ys[0],
            x1: xs[2],
            y1: ys[ys.len() - 1],
        },
        xs,
        ys,
        layout: Some(std::sync::Arc::new(TableLayout {
            header_rows: 0,
            spans: Vec::new(),
            spread: Vec::new(),
            downs: Vec::new(),
            sub_headers: Vec::new(),
            restated_headers: Vec::new(),
            cells,
            claimed,
        })),
    })
}

/// A page's space glyphs by baseline, for [`field_grid`] to give each cell
/// its own.
struct Spaces {
    /// `(baseline, glyph)`, in baseline order.
    at: Vec<(f64, usize)>,
}

impl Spaces {
    fn new(glyphs: &[crate::glyph::Glyph]) -> Self {
        let mut at: Vec<(f64, usize)> = (0..glyphs.len())
            .filter(|&i| glyphs[i].text.trim().is_empty())
            .map(|i| (glyphs[i].origin.1, i))
            .collect();
        at.sort_by(|a, b| a.0.total_cmp(&b.0));
        Spaces { at }
    }

    /// The spaces on `first`'s baseline whose centre is inside `(x0, x1)`,
    /// in page order. A space is often drawn with no box, where its pen
    /// position says where it is.
    fn within(
        &self,
        glyphs: &[crate::glyph::Glyph],
        first: usize,
        (x0, x1): (f64, f64),
    ) -> Vec<usize> {
        let g0 = &glyphs[first];
        let tol = g0.font_size.max(1.0) as f64 * ROW_BASELINE_TOLERANCE;
        let centre = |i: usize| match glyphs[i].bbox {
            Some(b) => (b.x0 + b.x1) / 2.0,
            None => glyphs[i].origin.0 + glyphs[i].advance.unwrap_or(0.0) / 2.0,
        };
        let from = self.at.partition_point(|s| s.0 <= g0.origin.1 - tol);
        let mut out: Vec<usize> = self.at[from..]
            .iter()
            .take_while(|s| s.0 < g0.origin.1 + tol)
            .map(|s| s.1)
            .filter(|&i| {
                let c = centre(i);
                (glyphs[i].origin.1 - g0.origin.1).abs() < tol && c > x0 && c < x1
            })
            .collect();
        out.sort_unstable();
        out
    }
}

/// A cell's text from its glyphs, as [`TableLayout::texts`] reads it.
fn cell_text(glyphs: &[crate::glyph::Glyph], ids: &[usize]) -> String {
    let gs: Vec<crate::glyph::Glyph> = ids.iter().map(|&i| glyphs[i].clone()).collect();
    join_cell_lines(crate::line::assemble(&gs).iter().map(|l| l.text.as_str()))
}

/// A list of fields whose labels end in no colon: `CP | Polished Chrome`,
/// `Maximum user weight size A | 300 lb/136 kg`.
///
/// Alignment is all it has, and two columns of prose align too, as do two
/// lists or two addresses set side by side. So each label is short (at most
/// [`MAX_COLON_LABEL_WORDS`] words), holds a letter and opens on no bullet.
/// Each value is a word, a figure or a phrase (at most
/// [`MAX_FIELD_VALUE_WORDS`] words), and few are sentences. Labels open as
/// names do: at most a quarter on a lowercase letter, where prose's lines
/// run on from the line above, and fewer than half are a list's
/// enumerators (`(a)`, `iv)`, `10. FIFA`), whose items are no fields. The
/// labels or the values end where they will. Prose fills its columns, so
/// its lines' ends crowd at the right edge; a field list's labels end where
/// each label does, its values where each value does. Values that crowd
/// there and run on in lowercase are a column of prose beside a list. And
/// values that are all page numbers, never going back, are a contents
/// page's.
///
/// And the two columns are of two kinds, as a label and its value are and
/// two lists side by side are not. At least one of these holds:
///
/// - the labels are set bold and the values are not;
/// - most values are quantities (`300 lb/136 kg`, `1st–99th percentile`);
/// - every label is a code, one short word (`CP`, `2MB`, `CIO`);
/// - the values come from a short list, half of them repeats of another
///   (`Yes`, `No`; `Optional`).
fn plain_fields(glyphs: &[crate::glyph::Glyph], fields: &[Field], texts: &[String]) -> bool {
    let n = fields.len();
    if texts.len() != n * 2 {
        return false;
    }
    let (mut lower, mut sentences) = (0usize, 0usize);
    for r in 0..n {
        let (label, value) = (texts[r * 2].trim(), texts[r * 2 + 1].trim());
        let words = value.split_whitespace().count();
        let Some(first) = label.chars().next() else {
            return false;
        };
        if value.is_empty()
            || words > MAX_FIELD_VALUE_WORDS
            || label.split_whitespace().count() > MAX_COLON_LABEL_WORDS
            || !label.chars().any(char::is_alphabetic)
            || matches!(
                first,
                '•' | '●'
                    | '○'
                    | '■'
                    | '□'
                    | '▪'
                    | '◦'
                    | '·'
                    | '-'
                    | '–'
                    | '—'
                    | '*'
                    | '>'
            )
        {
            return false;
        }
        lower += usize::from(first.is_lowercase());
        sentences += usize::from(value.ends_with('.') && words > MAX_COLON_LABEL_WORDS);
    }
    let bb = |i: usize| glyphs[i].bbox.unwrap();
    let fs = glyphs[fields[0].1[0]].font_size.max(1.0) as f64;
    let crowded = |ends: Vec<f64>| {
        let right = ends.iter().copied().fold(f64::MIN, f64::max);
        ends.iter().filter(|&&e| e >= right - fs * 2.0).count() * 5 >= n * 3
    };
    let ends = |pick: fn(&Field) -> &Vec<usize>| {
        fields
            .iter()
            .map(|f| pick(f).iter().map(|&i| bb(i).x1).fold(f64::MIN, f64::max))
            .collect::<Vec<f64>>()
    };
    let labels: Vec<&str> = (0..n).map(|r| texts[r * 2].trim()).collect();
    let values: Vec<&str> = (0..n).map(|r| texts[r * 2 + 1].trim()).collect();
    let values_run_on = values
        .iter()
        .filter(|v| v.starts_with(|c: char| c.is_lowercase()))
        .count()
        * 2
        > n;
    let value_ends_crowd = crowded(ends(|f| &f.2));
    let prose = value_ends_crowd && (values_run_on || crowded(ends(|f| &f.1)));
    let enumerators = labels
        .iter()
        .filter(|l| l.split_whitespace().next().is_some_and(enumerator))
        .count();
    // Page numbers that never go back: a contents page's entries.
    let pages: Vec<u32> = values
        .iter()
        .filter(|v| v.len() <= 4 && !v.starts_with('0'))
        .filter_map(|v| v.parse::<u32>().ok())
        .collect();
    let contents = n >= 3 && pages.len() == n && pages.windows(2).all(|w| w[0] <= w[1]);
    if lower * 4 > n || sentences * 2 > n || prose || enumerators * 2 >= n || contents {
        return false;
    }
    let heavy = |ids: &[usize]| {
        let weighted: Vec<u32> = ids.iter().filter_map(|&i| glyphs[i].weight).collect();
        !weighted.is_empty() && weighted.iter().filter(|&&w| w >= 600).count() * 2 > weighted.len()
    };
    let bold = fields.iter().all(|f| heavy(&f.1) && !heavy(&f.2));
    let quantities = values.iter().filter(|v| reads_as_quantity(v)).count() * 2 > n;
    let codes = labels.iter().all(|l| {
        !l.contains(char::is_whitespace)
            && l.chars().count() <= 5
            && l.chars().any(|c| c.is_uppercase() || c.is_ascii_digit())
    });
    let mut distinct: Vec<&str> = values.clone();
    distinct.sort_unstable();
    distinct.dedup();
    let closed = distinct.len() * 2 <= n;
    bold || quantities || codes || closed
}

/// A list of two columns whose first row names them: a word over values
/// that are quantities (`Description | Units` over `Chafing dish | 12`).
/// A field's value is a quantity like the rest, or the values are words
/// throughout, and the first row is one more field.
fn names_its_columns(texts: &[String]) -> bool {
    let n = texts.len() / 2;
    if n < MIN_PLAIN_FIELDS {
        return false;
    }
    let value = |r: usize| texts[r * 2 + 1].trim();
    let label = |r: usize| texts[r * 2].trim();
    let quantities = (1..n).filter(|&r| reads_as_quantity(value(r))).count();
    !reads_as_quantity(value(0))
        && !reads_as_quantity(label(0))
        && value(0).chars().any(char::is_alphabetic)
        && quantities * 4 >= (n - 1) * 3
}

/// A list's enumerator standing for a label: `(a)`, `b.`, `iv)`, `26a.`,
/// a number or one letter, or a roman numeral, in brackets or ended by a
/// stop. `No.` and `Ref.` are words.
pub(crate) fn enumerator(label: &str) -> bool {
    let inner = label.trim_start_matches('(').trim_end_matches(['.', ')']);
    if inner.is_empty() || inner.len() == label.len() {
        return false;
    }
    let digits = inner.trim_end_matches(|c: char| c.is_ascii_lowercase());
    let numbered = !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && inner.len() - digits.len() <= 1;
    let lettered = inner.chars().count() == 1 && inner.chars().all(char::is_alphabetic);
    let roman = inner.len() <= 5
        && (inner.chars().all(|c| "ivxlcdm".contains(c))
            || inner.chars().all(|c| "IVXLCDM".contains(c)));
    numbered || lettered || roman
}

/// Whether a field list's last value wraps back under its labels: the line
/// set just under it starts where the labels do and runs on across the
/// values, and is no label itself. Those are paragraphs that open on a
/// label (`Change Note: CN 4431 - Add the drying time to the second wash and`
/// over `the operator's check after it.`), whose words a wide space after
/// the colon happened to part. A field list wraps a value under the value.
fn wraps_under_labels(g: &Grid, glyphs: &[crate::glyph::Glyph]) -> bool {
    let rows = g.rows();
    if rows == 0 || g.xs.len() < 3 {
        return false;
    }
    let pitch = (g.ys[rows] - g.ys[0]) / rows as f64;
    let mut below: Vec<&crate::glyph::Glyph> = glyphs
        .iter()
        .filter(|x| x.is_horizontal() && !x.text.trim().is_empty())
        .filter(|x| {
            x.bbox
                .is_some_and(|b| b.y0 > g.bbox.y1 - 1.0 && b.y0 < g.bbox.y1 + 0.75 * pitch)
        })
        .collect();
    if below.is_empty() {
        return false;
    }
    below.sort_by(|a, b| a.bbox.unwrap().x0.total_cmp(&b.bbox.unwrap().x0));
    let (x0, x1) = (
        below[0].bbox.unwrap().x0,
        below
            .iter()
            .map(|x| x.bbox.unwrap().x1)
            .fold(f64::MIN, f64::max),
    );
    let fs = below[0].font_size.max(1.0) as f64;
    let text: String = below.iter().map(|x| x.text.as_str()).collect();
    (x0 - g.xs[0]).abs() <= fs && x1 > g.xs[1] && label_colon(&text).is_none()
}

fn numbered_table_caption(text: &str) -> bool {
    text.trim()
        .to_ascii_lowercase()
        .strip_prefix("table ")
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_ascii_digit())
}

/// Generate text-aligned table candidates before applying the geometry gate.
///
/// Exposed for diagnostics: passing empty geometry to [`detect_aligned`] does
/// not disable corroboration, it rejects every candidate.
pub fn detect_aligned_candidates(glyphs: &[crate::glyph::Glyph], page: usize) -> Vec<Grid> {
    let (rows, cells) = aligned_rows(glyphs);
    aligned_runs(&rows, &cells, page)
}

/// A page's text as rows by baseline, with where each row breaks into cells.
#[allow(clippy::type_complexity)]
fn aligned_rows(glyphs: &[crate::glyph::Glyph]) -> (Vec<Vec<&crate::glyph::Glyph>>, Vec<Vec<f64>>) {
    let mut gs: Vec<&crate::glyph::Glyph> = glyphs
        .iter()
        .filter(|g| g.bbox.is_some() && g.is_horizontal() && !g.text.trim().is_empty())
        .collect();
    // A valid short table can contain one glyph run in each of 3×2 cells.
    // Precision is enforced later by repeated columns plus drawn geometry.
    if gs.len() < MIN_ALIGNED_ROWS * 2 {
        return (Vec::new(), Vec::new());
    }
    gs.sort_by(|a, b| {
        a.origin
            .1
            .partial_cmp(&b.origin.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.origin
                    .0
                    .partial_cmp(&b.origin.0)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    // Rows by baseline.
    let mut rows: Vec<Vec<&crate::glyph::Glyph>> = Vec::new();
    for g in gs {
        let fs = g.font_size.max(1.0) as f64;
        match rows.last_mut() {
            Some(r) if (r[0].origin.1 - g.origin.1).abs() < fs * ROW_BASELINE_TOLERANCE => {
                r.push(g)
            }
            _ => rows.push(vec![g]),
        }
    }

    // A row holding dot leaders, left to right: leaders stand only between
    // a label and its value, and part them ([`cell_starts`]), but glyphs a
    // hair off the row's baseline were sorted by height first and come out
    // of turn. Other rows keep their order (see [`colon_field_lists`]).
    for r in rows.iter_mut() {
        let mut run = crate::textgrid::LeaderRun::default();
        let leads = r.iter().any(|g| {
            run.step(g);
            run.leads()
        });
        if leads {
            let mut sorted = r.clone();
            sorted.sort_by(|a, b| a.bbox.unwrap().x0.total_cmp(&b.bbox.unwrap().x0));
            if !crate::textgrid::contents_entry(&sorted) {
                *r = sorted;
            }
        }
    }
    // Cell starts within each row.
    let cells: Vec<Vec<f64>> = rows.iter().map(|r| cell_starts(r)).collect();
    (rows, cells)
}

/// Runs of consecutive rows of two or more cells, as candidate grids.
fn aligned_runs(rows: &[Vec<&crate::glyph::Glyph>], cells: &[Vec<f64>], page: usize) -> Vec<Grid> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < rows.len() {
        if cells[i].len() < 2 {
            i += 1;
            continue;
        }
        // The run's rows, past a field's value wrapped onto a line of its
        // own.
        let mut run_rows: Vec<Vec<&crate::glyph::Glyph>> = vec![rows[i].clone()];
        let mut run_cells: Vec<Vec<f64>> = vec![cells[i].clone()];
        // The foot of a last value wrapped under the last row.
        let mut wrapped_foot: Option<f64> = None;
        let mut j = i;
        while j + 1 < rows.len() {
            if cells[j + 1].len() >= 2 {
                j += 1;
                run_rows.push(rows[j].clone());
                run_cells.push(cells[j].clone());
            } else if j + 2 < rows.len()
                && cells[j + 2].len() >= 2
                && run_rows
                    .iter()
                    .zip(&run_cells)
                    .all(|(r, c)| field_row(r, c))
                && field_row(&rows[j + 2], &cells[j + 2])
                && wraps_a_value(
                    &rows[j + 1],
                    &run_rows[run_rows.len() - 1],
                    &run_cells[run_cells.len() - 1],
                )
            {
                // Left out of the rows the columns and rows are found from:
                // the row's band runs down to the next row's top, so the
                // wrapped line is read into the row above, and the rows'
                // pitch stays the rows' own.
                j += 1;
            } else if j + 1 < rows.len()
                && run_rows.len() >= 2
                && run_rows
                    .iter()
                    .zip(&run_cells)
                    .all(|(r, c)| field_row(r, c))
                && wraps_a_value(
                    &rows[j + 1],
                    &run_rows[run_rows.len() - 1],
                    &run_cells[run_cells.len() - 1],
                )
                && within_pitch(&run_rows, &rows[j + 1])
            {
                // The grid's last value wrapped, under its last row: the
                // grid's foot is moved down under the line, so it is read
                // into the row. The grid is found from the rows alone: a
                // last band two lines deep, counted as a pitch, made the
                // rows' pitch two lines, and rows a line apart read as one
                // row's wrapped lines (`By: Name: Title:`).
                wrapped_foot = rows[j + 1]
                    .iter()
                    .filter_map(|g| g.bbox.map(|b| b.y1))
                    .reduce(f64::max);
                j += 1;
                break;
            } else {
                break;
            }
        }
        if run_rows.len() >= MIN_ALIGNED_ROWS {
            if let Some(mut g) = grid_from_cells(&run_rows, &run_cells, page) {
                if let (Some(y1), Some(foot)) = (wrapped_foot, g.ys.last_mut()) {
                    *foot = foot.max(y1 + 1.0);
                    g.bbox.y1 = *foot;
                }
                out.push(g);
            }
        }
        i = j + 1;
    }
    out
}

/// Whether `line` stands under the run's last row no farther than the run's
/// rows stand from each other: a wrapped value's next line, not the next
/// block's first.
fn within_pitch(rows: &[Vec<&crate::glyph::Glyph>], line: &[&crate::glyph::Glyph]) -> bool {
    let base = |r: &[&crate::glyph::Glyph]| r.first().map(|g| g.origin.1);
    let mut pitches: Vec<f64> = rows
        .windows(2)
        .filter_map(|w| Some(base(&w[1])? - base(&w[0])?))
        .filter(|p| *p > 0.0)
        .collect();
    if pitches.is_empty() {
        return false;
    }
    pitches.sort_by(f64::total_cmp);
    let pitch = pitches[pitches.len() / 2];
    match (base(line), rows.last().and_then(|r| base(r))) {
        (Some(y), Some(last)) => y > last && y - last <= pitch * 1.1,
        _ => false,
    }
}

/// Whether a row is a field grid's: label and value pairs across it, each
/// label ending in a colon (`Ordering account: | A10… | Ordering address: |
/// 12 OAK ST`). Only such a grid's run goes on past a wrapped value: a
/// run of other rows joined over the line read their pitch from rows that
/// are not the grid's.
fn field_row(row: &[&crate::glyph::Glyph], starts: &[f64]) -> bool {
    if starts.len() < 2 || starts.len() % 2 != 0 {
        return false;
    }
    (0..starts.len()).step_by(2).all(|k| {
        let end = starts.get(k + 1).copied().unwrap_or(f64::MAX);
        let label: String = row
            .iter()
            .filter(|g| {
                g.bbox
                    .is_some_and(|b| b.x0 >= starts[k] - 0.01 && b.x0 < end - 0.01)
            })
            .map(|g| g.text.as_str())
            .collect();
        label_colon(&label) == Some(true)
    })
}

/// Whether a line of one cell is a field's value wrapped from the row
/// above: set where one of that row's values starts, past its first
/// column, beside a label that ends in a colon (`Ordering address:` over
/// `12 OAK ST`, then `SPRINGFIELD, ST 00000` under the street), within
/// that value's column, and no field of its own. A line that starts a
/// column of figures, a row's label of its own, or a field, is a row.
fn wraps_a_value(
    line: &[&crate::glyph::Glyph],
    above: &[&crate::glyph::Glyph],
    starts: &[f64],
) -> bool {
    let Some(x) = line
        .iter()
        .filter_map(|g| g.bbox)
        .map(|b| b.x0)
        .reduce(f64::min)
    else {
        return false;
    };
    let fs = line.first().map_or(1.0, |g| g.font_size.max(1.0) as f64);
    let Some(k) = starts
        .iter()
        .position(|s| (s - x).abs() <= fs * FIELD_ALIGN_TOLERANCE)
    else {
        return false;
    };
    if k == 0 {
        return false;
    }
    // A value's later line: no field of its own (`ACRN: AC`), and within
    // its column, short of the next one's start.
    let text: String = line.iter().map(|g| g.text.as_str()).collect();
    if label_colon(&text).is_some() {
        return false;
    }
    let right = line
        .iter()
        .filter_map(|g| g.bbox)
        .map(|b| b.x1)
        .fold(f64::MIN, f64::max);
    if starts.get(k + 1).is_some_and(|&next| right > next - 0.01) {
        return false;
    }
    let label: String = above
        .iter()
        .filter(|g| {
            g.bbox
                .is_some_and(|b| b.x0 >= starts[k - 1] - 0.01 && b.x0 < starts[k] - 0.01)
        })
        .map(|g| g.text.as_str())
        .collect();
    label_colon(&label) == Some(true)
}

/// Column boundaries for rows that all break into the same number of
/// cells, taking each row's k-th cell as column k.
///
/// Columns found by where cells *start* lose a column whose cells start in
/// different places: right-aligned figures under a left-aligned heading
/// (`Quantity` over `3`, `4`, `2`) start nowhere in common, the heading's
/// start is supported by one row in four, and the heading falls into the
/// column beside it. When every row has the same number of cells, rank is
/// the column, and the columns are sound if the k-th cells of all rows end
/// before any row's next cell begins. Each boundary is then the leftmost
/// start of its column.
fn columns_by_rank(rows: &[Vec<&crate::glyph::Glyph>], cells: &[Vec<f64>]) -> Option<Vec<f64>> {
    // A header row over a column of row labels has an empty corner, one
    // cell fewer than the rows under it; its k-th cell is column k + 1.
    let corner = cells.len() >= 2 && cells[0].len() + 1 == cells[1].len();
    let n = if corner {
        cells[1].len()
    } else {
        cells.first()?.len()
    };
    if n < 2
        || cells
            .iter()
            .enumerate()
            .any(|(i, r)| r.len() != n && !(corner && i == 0))
    {
        return None;
    }
    let mut lo = vec![f64::MAX; n];
    let mut hi = vec![f64::MIN; n];
    for (i, (row, starts)) in rows.iter().zip(cells).enumerate() {
        let off = usize::from(corner && i == 0);
        for g in row {
            let b = g.bbox?;
            let k = starts.iter().rposition(|s| *s <= b.x0 + 0.01)? + off;
            lo[k] = lo[k].min(b.x0);
            hi[k] = hi[k].max(b.x1);
        }
    }
    if (1..n).any(|k| hi[k - 1] >= lo[k]) {
        return None;
    }
    let mut xs = lo;
    xs.push(hi[n - 1] + 1.0);
    Some(drop_degenerate(xs, MIN_CELL_EXTENT))
}

/// Rows typeset on two axes: the first row bold throughout, and every later
/// row a bold label in its first cell beside plain cells.
fn two_axis_rows(rows: &[Vec<&crate::glyph::Glyph>], cells: &[Vec<f64>]) -> bool {
    if rows.len() < 3 {
        return false;
    }
    let heavy = |g: &&crate::glyph::Glyph| g.weight.unwrap_or(400) >= 600;
    let known = |g: &&&crate::glyph::Glyph| g.weight.is_some();
    let head: Vec<_> = rows[0].iter().filter(known).collect();
    if head.is_empty() || head.iter().filter(|g| heavy(g)).count() * 2 <= head.len() {
        return false;
    }
    rows[1..].iter().zip(&cells[1..]).all(|(row, starts)| {
        if starts.len() < 3 {
            return false;
        }
        let second = starts[1];
        let (mut lb, mut lw, mut rb, mut rw) = (0, 0, 0, 0);
        for g in row.iter().filter(|g| g.weight.is_some()) {
            let x = g.bbox.map_or(g.origin.0, |b| b.x0);
            if x < second - 0.01 {
                lw += 1;
                lb += usize::from(heavy(g));
            } else {
                rw += 1;
                rb += usize::from(heavy(g));
            }
        }
        lw > 0 && lb * 2 > lw && rw > 0 && rb * 2 < rw
    })
}

/// An aligned run that is set as a transposed table: items across a bold
/// header row whose corner may be empty, each named once; a bold property
/// label down the first column beside plain cells; and one row, not one
/// column, of figures, since in a transposed table the figures run along
/// the rows (`Qty`, `Each`) and a column holds an item's code beside them.
/// [`has_typeset_header`] asks for the other orientation and refuses it.
fn has_two_axis_header(g: &Grid, glyphs: &[crate::glyph::Glyph]) -> bool {
    let (rows, cols) = (g.rows(), g.cols());
    if cols < 3 || rows < 3 {
        return false;
    }
    let cells = g.cell_texts(glyphs);
    let at = |r: usize, c: usize| cells[r * cols + c].trim().to_string();
    let w0 = g.cell_weights(glyphs, 0);
    let bold = |(w, h): (usize, usize)| w > 0 && h * 2 > w;
    if !(1..cols).all(|c| bold(w0[c]))
        || !(1..cols).all(|c| !at(0, c).is_empty() && !is_numeric_cell(&at(0, c)))
    {
        return false;
    }
    if !(1..cols).all(|c| (1..c).all(|k| at(0, k) != at(0, c))) {
        return false;
    }
    let labelled = (1..rows).all(|r| {
        let w = g.cell_weights(glyphs, r);
        bold(w[0]) && (1..cols).all(|c| w[c].0 > 0 && !bold(w[c]))
    });
    let figures = (1..rows).any(|r| (1..cols).all(|c| is_numeric_cell(&at(r, c))));
    labelled && figures
}

/// x positions where a row breaks into cells.
fn cell_starts(row: &[&crate::glyph::Glyph]) -> Vec<f64> {
    let mut out = Vec::new();
    let mut prev_end: Option<f64> = None;
    // Dot leaders run a label up to the next column's text, a word space or
    // less short of it: the text after a run of them starts a cell. Not in
    // a contents entry's line, whose leaders run to its page.
    let contents = crate::textgrid::contents_entry(row);
    let mut run = crate::textgrid::LeaderRun::default();
    for g in row {
        let b = g.bbox.unwrap();
        let fs = g.font_size.max(1.0) as f64;
        let after_leaders = run.step(g) >= crate::textgrid::LEADER_DOTS
            && crate::textgrid::leader_dots(g.text.trim()) == 0
            && !contents;
        match prev_end {
            None => out.push(b.x0),
            Some(pe) if b.x0 - pe > fs * TABLE_CELL_GAP => out.push(b.x0),
            Some(_) if after_leaders => out.push(b.x0),
            _ => {}
        }
        prev_end = Some(b.x1);
    }
    out
}

/// Largest share of a table's typical row pitch that still counts as a line
/// wrapping inside a cell rather than a new row.
///
/// A cell whose text wraps puts its continuation on its own baseline, and a
/// row-per-baseline banding then reports it as a row of its own. A rule-less
/// table whose column headings run to two lines comes out with two rows per
/// heading and its halves in different cells, which is worse than either the
/// column count or the text being wrong: the shape is confidently reported
/// and confidently wrong.
///
/// The discriminator is pitch, not content: leading inside a cell is set
/// tighter than the space between rows, whatever the cell holds. Measured
/// against the row pitch of the same table rather than an absolute, since a
/// dense table's rows can sit closer than a loose table's wrapped lines.
const WRAPPED_LINE_PITCH: f64 = 0.75;

/// Collapse row boundaries that sit closer together than this table's own
/// rows do — the second line of a wrapped cell, not a row.
fn join_wrapped_rows(ys: Vec<f64>) -> Vec<f64> {
    if ys.len() < 4 {
        // Two bands or fewer: no pitch to compare against.
        return ys;
    }
    let mut pitches: Vec<f64> = ys.windows(2).map(|w| w[1] - w[0]).collect();
    pitches.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = pitches[pitches.len() / 2];
    if median <= 0.0 {
        return ys;
    }
    let mut out = vec![ys[0]];
    for w in ys.windows(2) {
        // The last boundary is the table's foot and always stands.
        if w[1] - w[0] >= median * WRAPPED_LINE_PITCH || w[1] == ys[ys.len() - 1] {
            out.push(w[1]);
        }
    }
    out
}

fn grid_from_cells(
    rows: &[Vec<&crate::glyph::Glyph>],
    cells: &[Vec<f64>],
    page: usize,
) -> Option<Grid> {
    let fs = rows[0][0].font_size.max(1.0) as f64;
    let full_rows = rows;

    // A transposed table, items across its header and properties down its
    // first column, is typeset on two axes; its item columns set a name
    // left over figures set right, so their cells start nowhere in common.
    let two_axis = two_axis_rows(rows, cells);

    // Totals rows beneath a line table (a label and an amount, two cells)
    // join its aligned run, and counted with it they dilute the support
    // each column needs: a column of figures on three rows of six goes
    // unfound, and its heading falls into the column beside it. The rows
    // with the most cells are the table's core; they alone find the
    // columns, and the short rows are placed into them.
    let widest = cells.iter().map(Vec::len).max().unwrap_or(0);
    let core: Vec<usize> = (0..cells.len())
        .filter(|&i| cells[i].len() == widest)
        .collect();
    let use_core = !two_axis
        && widest >= 3
        && core.len() >= MIN_ALIGNED_ROWS
        && core.len() * 2 >= cells.len()
        && core.len() < cells.len();
    let core_rows: Vec<Vec<&crate::glyph::Glyph>> = core.iter().map(|&i| rows[i].clone()).collect();
    let core_cells: Vec<Vec<f64>> = core.iter().map(|&i| cells[i].clone()).collect();
    let (rows, cells): (&[Vec<&crate::glyph::Glyph>], &[Vec<f64>]) = if use_core {
        (&core_rows, &core_cells)
    } else {
        (rows, cells)
    };

    let all: Vec<f64> = cells.iter().flatten().copied().collect();
    let cols = coalesce(all, fs * COLUMN_ALIGN_TOLERANCE);
    if cols.len() < 2 {
        return None;
    }
    // The precision signal: each column position must recur across most rows.
    let needed = (rows.len() as f64 * MIN_COLUMN_SUPPORT).ceil() as usize;
    let supported: Vec<f64> = cols
        .into_iter()
        .filter(|c| {
            cells
                .iter()
                .filter(|r| {
                    r.iter()
                        .any(|x| (x - c).abs() < fs * COLUMN_ALIGN_TOLERANCE)
                })
                .count()
                >= needed
        })
        .collect();
    let supported = if supported.len() < 2 && two_axis {
        let mut by_rank = columns_by_rank(rows, cells)?;
        by_rank.pop();
        by_rank
    } else {
        supported
    };
    if supported.len() < 2 {
        return None;
    }

    let x_hi = full_rows
        .iter()
        .flat_map(|r| r.iter().map(|g| g.bbox.unwrap().x1))
        .fold(f64::MIN, f64::max);
    let mut xs = supported;
    xs.push(x_hi + 1.0);
    let mut xs = drop_degenerate(xs, MIN_CELL_EXTENT);
    // Where a row's own cells land two to a column, the boundaries are in
    // the wrong place for it; rank places them when every row agrees on
    // how many cells there are. Only placed, never added: a column the
    // starts did not find stays unfound (a chart's tick labels rank into
    // columns as readily as a table's figures). A table typeset on two
    // axes is the exception: its type is the evidence the starts lacked.
    let column_of = |xs: &[f64], x: f64| xs.iter().rposition(|b| *b <= x + 0.01).unwrap_or(0);
    let crowded = cells.iter().any(|starts| {
        let mut at: Vec<usize> = starts.iter().map(|x| column_of(&xs, *x)).collect();
        at.dedup();
        at.len() < starts.len()
    });
    if crowded || two_axis {
        if let Some(by_rank) = columns_by_rank(rows, cells) {
            if by_rank.len() == xs.len() || (two_axis && by_rank.len() > xs.len()) {
                xs = by_rank;
            }
        }
    }
    // The short rows the core's columns were found without may reach past
    // its edges.
    if use_core && xs.len() >= 2 {
        let x_lo = full_rows
            .iter()
            .flat_map(|r| r.iter().map(|g| g.bbox.unwrap().x0))
            .fold(f64::MAX, f64::min);
        let n = xs.len();
        xs[0] = xs[0].min(x_lo);
        xs[n - 1] = xs[n - 1].max(x_hi + 1.0);
    }
    if xs.len() < 3 {
        return None;
    }

    let mut ys: Vec<f64> = full_rows
        .iter()
        .map(|r| {
            r.iter()
                .map(|g| g.bbox.unwrap().y0)
                .fold(f64::MAX, f64::min)
                - 1.0
        })
        .collect();
    let y_hi = full_rows
        .last()?
        .iter()
        .map(|g| g.bbox.unwrap().y1)
        .fold(f64::MIN, f64::max);
    ys.push(y_hi + 1.0);
    let ys = join_wrapped_rows(ys);

    Some(Grid {
        layout: None,
        page,
        bbox: BBox {
            x0: xs[0],
            y0: ys[0],
            x1: xs[xs.len() - 1],
            y1: ys[ys.len() - 1],
        },
        xs,
        ys,
    })
}

#[cfg(test)]
mod header_tests {
    use super::*;

    fn rows(data: &[&[&str]]) -> Vec<Vec<String>> {
        data.iter()
            .map(|r| r.iter().map(|c| c.to_string()).collect())
            .collect()
    }

    #[test]
    fn text_header_over_numeric_columns_is_kept() {
        let t = rows(&[
            &["Item", "2023 total", "Change"],
            &["Revenue", "10,250", "5%"],
            &["Costs", "8,100", "-2%"],
        ]);
        assert_eq!(decide_header_rows(&t, false), 1);
    }

    #[test]
    fn numeric_first_row_is_demoted() {
        // A financial statement's first data row: amounts under amounts.
        let t = rows(&[
            &["", "6,785", "7,004"],
            &["Gold certificates", "11,037", "11,037"],
            &["Coin", "1,332", "1,304"],
        ]);
        assert_eq!(decide_header_rows(&t, false), 0);
    }

    #[test]
    fn style_evidence_vetoes_the_demotion() {
        let t = rows(&[&["", "6,785"], &["Gold", "11,037"]]);
        assert_eq!(decide_header_rows(&t, true), 1);
    }

    #[test]
    fn year_row_over_money_columns_is_a_header() {
        let t = rows(&[
            &["", "2023", "2024"],
            &["Revenue", "10,250", "11,900"],
            &["Costs", "8,100", "8,400"],
        ]);
        assert_eq!(decide_header_rows(&t, false), 1);
    }

    #[test]
    fn all_text_table_keeps_the_presumed_header() {
        let t = rows(&[&["Technology", "Trigger"], &["Graph databases", "Rising"]]);
        assert_eq!(decide_header_rows(&t, false), 1);
    }

    fn h(x0: f64, x1: f64, y: f64) -> Rule {
        Rule {
            bbox: BBox {
                x0,
                y0: y,
                x1,
                y1: y + 0.6,
            },
            orientation: Orientation::Horizontal,
            page: 0,
        }
    }
    fn v(y0: f64, y1: f64, x: f64) -> Rule {
        Rule {
            bbox: BBox {
                x0: x,
                y0,
                x1: x + 0.6,
                y1,
            },
            orientation: Orientation::Vertical,
            page: 0,
        }
    }

    fn grid_with_bands() -> Grid {
        // 4 columns x 5 rows, rows 20pt tall from y=100.
        Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 100.0, 200.0, 300.0, 400.0],
            ys: vec![100.0, 120.0, 140.0, 160.0, 180.0, 200.0],
            bbox: BBox {
                x0: 0.0,
                y0: 100.0,
                x1: 400.0,
                y1: 200.0,
            },
        }
    }

    fn fill(x0: f64, y0: f64, x1: f64, y1: f64) -> crate::rule::Fill {
        crate::rule::Fill {
            bbox: BBox { x0, y0, x1, y1 },
            page: 0,
            paint: None,
            stroked: false,
        }
    }

    #[test]
    fn stacked_banner_header_reads_from_the_fill_run() {
        let g = grid_with_bands();
        // Banner band, banner band, per-cell shaded column-name row; data unshaded.
        let fills = vec![
            fill(0.0, 100.0, 400.0, 120.0),
            fill(0.0, 120.0, 400.0, 140.0),
            fill(0.0, 140.0, 95.0, 160.0),
            fill(100.0, 140.0, 195.0, 160.0),
            fill(200.0, 140.0, 295.0, 160.0),
            fill(300.0, 140.0, 400.0, 160.0),
        ];
        let rows: Vec<Vec<String>> = (0..5)
            .map(|r| (0..4).map(|c| format!("r{r}c{c}")).collect())
            .collect();
        assert_eq!(g.header_rows(&rows, &[], &[], &fills), 3);
    }

    #[test]
    fn a_shaded_first_band_is_no_header_row() {
        // A funding table: its header shaded, its first period band shaded
        // under it, and the next period's band shaded further down. The
        // shaded run is the header and the first band, not a two-row header.
        let g = grid_with_bands();
        let row = |r: usize| fill(0.0, 100.0 + 20.0 * r as f64, 400.0, 120.0 + 20.0 * r as f64);
        let rows: Vec<Vec<String>> = [
            ["CLIN", "Funding Amount", "Estimated Date", "Note"],
            ["Months 1 – 12", "", "", ""],
            ["0001 – Storage", "TBD", "TBD", "a"],
            ["Months 13-24", "", "", ""],
            ["0006 – Storage", "$2,947,920.24", "December 3, 2021", "b"],
        ]
        .iter()
        .map(|r| r.iter().map(|t| t.to_string()).collect())
        .collect();
        assert_eq!(g.header_rows(&rows, &[], &[], &[row(0), row(1), row(3)]), 1);
        // With no band of the kind further down, a shaded second row under
        // the header is the header's own.
        assert_eq!(g.header_rows(&rows, &[], &[], &[row(0), row(1)]), 2);
        // A one-column listing holds such rows alone: its shading, broken
        // once, is no band's. Its run is deeper than a header is, and it
        // keeps the single header row it falls back to; shortened by one
        // as though it ended on a band, it read as four header rows.
        let one = Grid {
            xs: vec![0.0, 400.0],
            ys: (0..10).map(|r| 100.0 + 20.0 * r as f64).collect(),
            ..grid_with_bands()
        };
        let lines: Vec<Vec<String>> = (0..9).map(|r| vec![format!("line {r}")]).collect();
        let shaded: Vec<_> = (0..9).filter(|&r| r != 5).map(row).collect();
        assert_eq!(one.header_rows(&lines, &[], &[], &shaded), 1);
    }

    #[test]
    fn full_shading_gives_no_contrast_and_falls_back() {
        let g = grid_with_bands();
        let fills: Vec<_> = (0..5)
            .map(|r| fill(0.0, 100.0 + 20.0 * r as f64, 400.0, 120.0 + 20.0 * r as f64))
            .collect();
        let rows: Vec<Vec<String>> = (0..5)
            .map(|r| (0..4).map(|c| format!("r{r}c{c}")).collect())
            .collect();
        // Run covers the whole grid -> falls back to the single-row logic;
        // all-text rows keep the presumed single header.
        assert_eq!(g.header_rows(&rows, &[], &[], &fills), 1);
    }

    /// A letterhead: one row of fields ruled side by side, closed top and
    /// bottom, with verticals shutting the box. Rejecting one-row grids left
    /// these rules to cluster into the table below, whose columns then cut the
    /// letterhead's text mid-word.
    #[test]
    fn a_closed_one_row_banner_is_a_table() {
        let rules = vec![
            h(36.0, 98.0, 54.0),
            h(99.0, 326.0, 54.0),
            h(326.0, 457.0, 54.0),
            h(458.0, 506.0, 54.0),
            h(507.0, 555.0, 54.0),
            h(36.0, 98.0, 100.0),
            h(99.0, 326.0, 100.0),
            h(326.0, 457.0, 100.0),
            h(458.0, 506.0, 100.0),
            h(507.0, 555.0, 100.0),
            v(54.0, 100.0, 35.0),
            v(54.0, 100.0, 98.0),
        ];
        let gs = detect(&rules, 0);
        assert_eq!(gs.len(), 1, "the banner is one grid");
        assert_eq!(gs[0].rows(), 1);
        assert_eq!(gs[0].cols(), 5, "five fields across");
    }

    /// The same two baselines and the same scatter of column edges, drawn as
    /// underlines beneath separate words. Nothing encloses them, so they are
    /// not a banner — on one page this shape displaced the page's real table.
    #[test]
    fn unenclosed_underlines_are_not_a_banner() {
        let rules = vec![
            h(210.0, 248.0, 631.0),
            h(263.0, 331.0, 631.0),
            h(346.0, 401.0, 631.0),
            h(416.0, 459.0, 631.0),
            h(156.0, 195.0, 693.0),
            h(217.0, 295.0, 693.0),
            h(316.0, 379.0, 693.0),
            h(402.0, 456.0, 693.0),
        ];
        assert!(
            detect(&rules, 0).iter().all(|g| g.rows() > 1),
            "underlines with no verticals are not a one-row table"
        );
    }

    /// A banner sitting directly above a data table, closer than the
    /// height-based split gap. The band between them is crossed by no vertical
    /// — that, not the distance, is what says they are two tables.
    #[test]
    fn a_banner_splits_from_the_table_beneath_it() {
        let mut rules = vec![
            // Banner: 3 fields, closed box, y 54-100.
            h(36.0, 200.0, 54.0),
            h(200.0, 400.0, 54.0),
            h(400.0, 555.0, 54.0),
            h(36.0, 200.0, 100.0),
            h(200.0, 400.0, 100.0),
            h(400.0, 555.0, 100.0),
            v(54.0, 100.0, 36.0),
            v(54.0, 100.0, 200.0),
        ];
        // Data table below, rows 16pt tall — the 46pt banner is under the
        // 3.5x median gap, so height alone would keep them as one grid.
        for i in 0..5 {
            let y = 139.0 + 16.0 * i as f64;
            rules.push(h(100.0, 400.0, y));
        }
        rules.push(v(139.0, 203.0, 100.0));
        rules.push(v(139.0, 203.0, 250.0));
        rules.push(v(139.0, 203.0, 400.0));
        let gs = detect(&rules, 0);
        assert_eq!(gs.len(), 2, "banner and table are separate grids");
        assert_eq!(gs[0].rows(), 1, "the banner keeps its own single row");
        assert_eq!(gs[0].cols(), 3, "and its own columns, not the table's");
        assert!(gs[1].rows() > 1, "the data table keeps its rows");
    }

    /// Every band of a table ruled with horizontals alone is uncrossed. Only a
    /// band with ruled bands on *both* sides is the whitespace between two
    /// tables; asking merely for some vertical somewhere above and below split
    /// a ruled table of contents into one table per entry.
    #[test]
    fn a_horizontally_ruled_table_is_not_split_into_rows() {
        let mut rules = vec![v(50.0, 108.0, 54.0)]; // decoration near the top
        for i in 0..6 {
            let y = 126.0 + 24.0 * i as f64;
            rules.push(h(68.0, 127.0, y));
            rules.push(h(127.0, 341.0, y));
            rules.push(h(341.0, 373.0, y));
        }
        let gs = detect(&rules, 0);
        assert_eq!(gs.len(), 1, "one table, not one grid per entry");
        assert!(gs[0].rows() > 1, "its entries stay in one grid");
    }

    #[test]
    fn merged_cells_fill_down_and_banners_coalesce() {
        // 3 columns x 4 rows. Row 0 is a banner (no verticals cross it).
        // In rows 1-3, column 0 is one merged cell: the interior horizontal
        // rules are segmented and do not run across it.
        let mut rules = vec![
            h(0.0, 300.0, 100.0),   // table top
            h(0.0, 300.0, 120.0),   // below banner — full width
            h(100.0, 300.0, 140.0), // row split, columns 1-2 only
            h(100.0, 300.0, 160.0), // row split, columns 1-2 only
            h(0.0, 300.0, 180.0),   // table bottom
            v(120.0, 180.0, 0.0),
            v(120.0, 180.0, 100.0),
            v(120.0, 180.0, 200.0),
            v(120.0, 180.0, 300.0),
        ];
        rules.sort_by(|a, b| a.bbox.y0.partial_cmp(&b.bbox.y0).unwrap());
        let g = Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 100.0, 200.0, 300.0],
            ys: vec![100.0, 120.0, 140.0, 160.0, 180.0],
            bbox: BBox {
                x0: 0.0,
                y0: 100.0,
                x1: 300.0,
                y1: 180.0,
            },
        };
        // The banner band is shaded, as a drawn banner is.
        let fills = vec![fill(0.0, 100.0, 300.0, 120.0)];
        let m = g.merges(&rules, &fills);
        assert!(m.full_width_row[0], "banner row spans every column");
        assert!(!m.full_width_row[1]);
        // Column 0 continues through rows 2 and 3; columns 1-2 do not.
        assert!(m.continues_above[2 * 3], "col 0 row 2 continues");
        assert!(m.continues_above[3 * 3], "col 0 row 3 continues");
        assert!(!m.continues_above[2 * 3 + 1], "col 1 row 2 is its own cell");

        let mut rows = vec![
            vec!["Program".into(), "Requirements".into(), "".into()],
            vec!["Single family".into(), "97%".into(), "620".into()],
            vec!["(detached)".into(), "95%".into(), "680".into()],
            vec!["".into(), "90%".into(), "720".into()],
        ];
        // Denormalising gathers the wrapped merged text and repeats it, and
        // coalesces the banner. `Element::cells` keeps the raw grid; this is
        // what a self-containment consumer applies on top.
        denormalize(&mut rows, &m);
        assert_eq!(rows[0], vec!["Program Requirements", "", ""]);
        for row in rows.iter().take(4).skip(1) {
            assert_eq!(row[0], "Single family (detached)");
        }
        assert_eq!(rows[3][1], "90%", "unmerged columns are untouched");
    }

    /// A glyph at `x` with width `w` on baseline `y`.
    fn gly(x: f64, y: f64, w: f64, text: &str) -> crate::glyph::Glyph {
        crate::glyph::Glyph {
            text: text.into(),
            bbox: Some(BBox {
                x0: x,
                y0: y - 8.0,
                x1: x + w,
                y1: y,
            }),
            page: 0,
            origin: (x, y),
            rotation_deg: 0.0,
            font_size: 9.0,
            weight: None,
            italic: false,
            advance: Some(w),
            draw_index: 0,
            ocr: None,
        }
    }

    #[test]
    fn a_run_spanning_three_columns_is_split_per_glyph() {
        // 4 columns, 100pt each. One pen-contiguous run of three values that
        // the gap test never broke — a row-spanning label sharing its line
        // with the figures beside it.
        let g = Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 100.0, 200.0, 300.0, 400.0],
            ys: vec![0.0, 20.0],
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 400.0,
                y1: 20.0,
            },
        };
        let mut glyphs = Vec::new();
        for (i, (x, t)) in [(120.0, "L"), (128.0, "b"), (220.0, "6"), (320.0, "9")]
            .into_iter()
            .enumerate()
        {
            let mut gl = gly(x, 10.0, 8.0, t);
            gl.draw_index = i;
            glyphs.push(gl);
        }
        let cells = g.cell_texts(&glyphs);
        assert_eq!(cells[1], "Lb", "label stays in its own column");
        assert_eq!(cells[2], "6");
        assert_eq!(cells[3], "9");
    }

    #[test]
    fn a_run_straddling_one_boundary_stays_whole() {
        // A right-aligned money value with its symbol pinned at the column's
        // left edge legally crosses its own boundary; splitting it is the
        // `$ 7,8 | 35,559` defect.
        let g = Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 100.0, 200.0],
            ys: vec![0.0, 20.0],
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 200.0,
                y1: 20.0,
            },
        };
        let mut glyphs = Vec::new();
        for (i, x) in [90.0, 98.0, 106.0].into_iter().enumerate() {
            let mut gl = gly(x, 10.0, 8.0, "7");
            gl.draw_index = i;
            glyphs.push(gl);
        }
        let cells = g.cell_texts(&glyphs);
        assert!(
            cells[0] == "777" || cells[1] == "777",
            "the run lands whole in one cell, got {cells:?}"
        );
    }

    #[test]
    fn a_nested_tables_columns_do_not_slice_the_outer_rows() {
        // Outer table: label column + content column. Rows 1-2 hold a nested
        // table that rules one interior column of its own; rows 0 and 3 are
        // outer prose spanning the whole content column.
        let mut rules = vec![
            h(0.0, 300.0, 0.0),
            h(0.0, 300.0, 20.0),
            h(0.0, 300.0, 40.0),
            h(0.0, 300.0, 60.0),
            h(0.0, 300.0, 80.0),
            v(0.0, 80.0, 0.0),    // left edge
            v(0.0, 80.0, 100.0),  // label | content, drawn throughout
            v(20.0, 60.0, 200.0), // nested column, rows 1-2 only
            v(0.0, 80.0, 300.0),  // right edge
        ];
        rules.sort_by(|a, b| a.bbox.y0.partial_cmp(&b.bbox.y0).unwrap());
        let g = Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 100.0, 200.0, 300.0],
            ys: vec![0.0, 20.0, 40.0, 60.0, 80.0],
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 300.0,
                y1: 80.0,
            },
        };
        let m = g.merges(&rules, &[]);
        // Rows 1-2 keep the nested boundary; rows 0 and 3 do not.
        assert!(!m.continues_left[5], "nested row keeps its column");
        assert!(!m.continues_left[2 * 3 + 2]);
        assert!(m.continues_left[2], "outer prose row spans the boundary");
        assert!(m.continues_left[3 * 3 + 2]);
        // The label boundary is drawn on every row, so column 1 never merges.
        assert!((0..4).all(|r| !m.continues_left[r * 3 + 1]));

        let mut rows = vec![
            vec![
                "Label".into(),
                "prose that runs".into(),
                "the full width".into(),
            ],
            vec!["".into(), "Occupancy".into(), "Amount".into()],
            vec!["".into(), "Primary".into(), "3%".into()],
            vec!["Next".into(), "more prose".into(), "continuing".into()],
        ];
        denormalize(&mut rows, &m);
        assert_eq!(rows[0][1], "prose that runs the full width");
        assert_eq!(rows[0][2], "");
        assert_eq!(rows[1][1], "Occupancy", "nested row is untouched");
        assert_eq!(rows[1][2], "Amount");
        assert_eq!(rows[3][1], "more prose continuing");
    }

    fn grid_at(y0: f64, y1: f64, cols: usize, rows: usize) -> Grid {
        let xs: Vec<f64> = (0..=cols).map(|c| c as f64 * 100.0).collect();
        let ys: Vec<f64> = (0..=rows)
            .map(|r| y0 + (y1 - y0) * r as f64 / rows as f64)
            .collect();
        Grid {
            layout: None,
            page: 0,
            bbox: BBox {
                x0: 0.0,
                y0,
                x1: cols as f64 * 100.0,
                y1,
            },
            xs,
            ys,
        }
    }

    #[test]
    fn fragments_disagreeing_on_columns_are_suspect() {
        // One ruled region reported as 4 columns then 2 -- a table the
        // detector shredded because nothing bounded its columns.
        let grids = vec![grid_at(100.0, 144.0, 4, 4), grid_at(165.0, 418.0, 2, 17)];
        let s = suspect_tables(&grids, &[]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].fragment_cols, vec![4, 2]);
        assert_eq!(s[0].bbox.y0, 100.0);
        assert_eq!(s[0].bbox.y1, 418.0);
    }

    #[test]
    fn a_figures_column_of_mixed_arity_is_suspect() {
        // Rows merged into one cell: the grid is the right shape and the
        // wrong height, so column fragmentation cannot see it.
        let g = grid_at(0.0, 100.0, 2, 4);
        let cells: Vec<String> = [
            "Interest income Other",
            "$ 1,167 38 (218) (25)",
            "Total other items",
            "(205) 48",
            "Net income",
            "$ 914",
            "Allocated",
            "$ 870",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(merged_rows(&g, &cells));

        // The same shape with one value per row is fine.
        let ok: Vec<String> = [
            "Interest income",
            "$ 1,167",
            "Fees",
            "38",
            "Net income",
            "$ 914",
            "Allocated",
            "$ 870",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(!merged_rows(&g, &ok));
    }

    #[test]
    fn wrapped_prose_is_not_merged_rows() {
        // A text column of sentences carrying numbers is not a figures
        // column and must not qualify.
        let g = grid_at(0.0, 100.0, 2, 3);
        let cells: Vec<String> = [
            "Note 1",
            "The Company recorded 3 charges of 12.4 and 8.1 in 2024",
            "Note 2",
            "See section 7.10.3 for details",
            "Note 3",
            "Refer to page 42",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(!merged_rows(&g, &cells));
    }

    #[test]
    fn consistent_fragments_and_separated_tables_are_not_suspect() {
        // Same band, same column count: one table split across a page break
        // in the ruling, not a structure failure.
        let same = vec![grid_at(100.0, 200.0, 4, 8), grid_at(210.0, 300.0, 4, 8)];
        assert!(suspect_tables(&same, &[]).is_empty());

        // Different column counts but a wide gap between them: two unrelated
        // tables that merely share a page.
        let apart = vec![grid_at(100.0, 200.0, 4, 8), grid_at(600.0, 700.0, 2, 8)];
        assert!(suspect_tables(&apart, &[]).is_empty());
    }

    #[test]
    fn separate_boxes_are_found_per_component() {
        // Two labelled boxes stacked with a gap, as on a printed form. The
        // page-wide reading dilutes each box's evidence; per-component finds
        // both. (Which one the pipeline keeps is decided after trimming —
        // see `document::analyze_with`.)
        let mut rules = Vec::new();
        for (top, bot) in [(0.0_f64, 40.0_f64), (100.0, 140.0)] {
            rules.extend([
                h(0.0, 300.0, top),
                h(0.0, 300.0, (top + bot) / 2.0),
                h(0.0, 300.0, bot),
                v(top, bot, 0.0),
                v(top, bot, 150.0),
                v(top, bot, 300.0),
            ]);
        }
        let grids = detect_by_component(&rules, 0);
        assert_eq!(grids.len(), 2, "one grid per box, got {grids:?}");
        assert!(grids.iter().all(|g| g.cols() == 2 && g.rows() == 2));
    }

    /// A ruled table: verticals at `xs` for every row band of `ys`, and a
    /// full-width horizontal at each `ys`.
    fn ruled(xs: &[f64], ys: &[f64]) -> Vec<Rule> {
        let mut rules = Vec::new();
        for &y in ys {
            rules.push(h(xs[0], xs[xs.len() - 1], y));
        }
        for w in ys.windows(2) {
            for &x in xs {
                rules.push(v(w[0], w[1], x));
            }
        }
        rules
    }

    #[test]
    fn boxes_side_by_side_are_two_tables() {
        // Two tables set side by side share their row lines, and read all
        // at once they were one grid of both their columns.
        let mut rules = ruled(&[0.0, 40.0, 80.0, 120.0], &[0.0, 20.0, 40.0, 60.0]);
        rules.extend(ruled(&[160.0, 200.0, 240.0], &[0.0, 20.0, 40.0, 60.0]));
        let grids = detect(&rules, 0);
        assert_eq!(grids.len(), 2, "got {grids:?}");
        assert_eq!(grids[0].cols(), 3);
        assert_eq!(grids[1].cols(), 2);
    }

    #[test]
    fn a_small_box_keeps_the_rows_its_column_rule_closes() {
        // A field list ruled between its rows, with no rule over the first
        // or under the last: the column rule's ends close them. Measured
        // against the page's ruling, the column rule fell short of a grid
        // line and the first and last rows were lost.
        let mut rules: Vec<Rule> = [20.0, 40.0, 60.0]
            .iter()
            .map(|&y| h(0.0, 200.0, y))
            .collect();
        rules.push(v(0.0, 80.0, 100.0));
        // Elsewhere on the page: a long divider and a rule across it.
        rules.push(v(100.0, 700.0, 400.0));
        rules.push(h(300.0, 600.0, 500.0));
        let grids = detect(&rules, 0);
        let g = grids
            .iter()
            .find(|g| g.bbox.x1 <= 210.0)
            .expect("the field list");
        assert_eq!(g.rows(), 4, "got {:?}", g.ys);
        assert_eq!(g.cols(), 2);
    }

    #[test]
    fn boxes_one_band_runs_across_are_one_table() {
        // A table leaves a column unruled (60-90), so its rules touch in two
        // sets; its header band is shaded across the whole table.
        let mut rules = ruled(&[0.0, 60.0], &[0.0, 20.0, 40.0, 60.0]);
        rules.extend(ruled(&[90.0, 130.0, 170.0], &[0.0, 20.0, 40.0, 60.0]));
        let band = crate::rule::Fill {
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 170.0,
                y1: 20.0,
            },
            page: 0,
            paint: None,
            stroked: false,
        };
        let grids = detect_framed(&rules, &[band], &[], 0, &[]);
        assert_eq!(grids.len(), 1, "got {grids:?}");
        assert_eq!(grids[0].xs.len(), 5, "got {:?}", grids[0].xs);
        // Each set shades its own header: two tables.
        let bands = [
            crate::rule::Fill {
                bbox: BBox {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 60.0,
                    y1: 20.0,
                },
                page: 0,
                paint: None,
                stroked: false,
            },
            crate::rule::Fill {
                bbox: BBox {
                    x0: 90.0,
                    y0: 0.0,
                    x1: 170.0,
                    y1: 20.0,
                },
                page: 0,
                paint: None,
                stroked: false,
            },
        ];
        let grids = detect_framed(&rules, &bands, &[], 0, &[]);
        assert!(
            grids.iter().all(|g| g.bbox.x1 <= 61.0 || g.bbox.x0 >= 89.0),
            "got {grids:?}"
        );
    }

    #[test]
    fn a_link_underlined_in_a_cell_is_not_a_row() {
        // The underline lies loose in the box, ending in the open.
        let mut rules = ruled(&[0.0, 100.0, 200.0, 400.0], &[0.0, 20.0, 50.0]);
        rules.push(h(205.0, 330.0, 40.0));
        let grids = detect(&rules, 0);
        assert_eq!(grids.len(), 1, "got {grids:?}");
        assert_eq!(grids[0].rows(), 2, "got {:?}", grids[0].ys);
    }

    #[test]
    fn a_label_column_spanning_rows_is_inside_the_table() {
        // No left border, and each label spans three rows: only the rules
        // round each group reach the table's left edge.
        let mut rules = Vec::new();
        for (y, full) in [
            (0.0, true),
            (20.0, true),
            (40.0, false),
            (60.0, false),
            (80.0, false),
            (100.0, true),
            (120.0, false),
            (140.0, false),
            (160.0, false),
            (180.0, true),
        ] {
            rules.push(h(if full { 0.0 } else { 50.0 }, 300.0, y));
        }
        for x in [50.0, 150.0, 300.0] {
            rules.push(v(0.0, 180.0, x));
        }
        let grids = detect(&rules, 0);
        assert_eq!(grids.len(), 1, "got {grids:?}");
        assert!((grids[0].xs[0] - 0.0).abs() < 1.0, "got {:?}", grids[0].xs);
        assert_eq!(grids[0].cols(), 3);
    }

    #[test]
    fn row_lines_ruled_alike_under_a_header_stack_into_one_table() {
        // Five row lines, each drawn a column at a time, broken at 100 and
        // 200; a shaded header's separators stand on the breaks above them.
        let mut rules = Vec::new();
        for y in [20.0, 33.0, 46.0, 59.0, 72.0] {
            for (a, b) in [(0.0, 100.0), (100.0, 200.0), (200.0, 300.0)] {
                rules.push(h(a, b, y));
            }
        }
        for x in [100.0, 200.0] {
            rules.push(v(7.0, 20.0, x));
        }
        let stacks = ruled_stacks(&rules, &[]);
        assert_eq!(stacks.len(), 1, "got {stacks:?}");
        assert_eq!(stacks[0].len(), rules.len());
        // A statement's figures underlined a column at a time break alike
        // too, with no vertical on a break: no table of their own.
        let underlines: Vec<Rule> = rules
            .iter()
            .filter(|r| r.orientation == Orientation::Horizontal)
            .copied()
            .collect();
        assert!(ruled_stacks(&underlines, &[]).is_empty());
        // Nor are unbroken lines, a chart's gridlines: no columns.
        let gridlines: Vec<Rule> = [20.0, 33.0, 46.0, 59.0]
            .iter()
            .map(|&y| h(0.0, 300.0, y))
            .chain([v(7.0, 20.0, 100.0)])
            .collect();
        assert!(ruled_stacks(&gridlines, &[]).is_empty());
        // A shaded header drawn a column at a time, fills meeting end to end
        // on the breaks between its first two lines, states the columns as
        // separators would.
        let band = [
            fill(0.0, 20.0, 100.0, 33.0),
            fill(100.0, 20.0, 200.0, 33.0),
            fill(200.0, 20.0, 300.0, 33.0),
        ];
        let stacks = ruled_stacks(&underlines, &band);
        assert_eq!(stacks.len(), 1, "got {stacks:?}");
        assert_eq!(stacks[0].len(), underlines.len());
        // A chart's bars between broken gridlines stand apart, each its own
        // height: no band.
        let bars = [fill(20.0, 40.0, 80.0, 72.0), fill(120.0, 50.0, 180.0, 72.0)];
        assert!(ruled_stacks(&underlines, &bars).is_empty());
    }

    #[test]
    fn row_lines_drawn_whole_down_two_columns_are_two_stacks() {
        // Two tables side by side, each ruled under every row with lines
        // drawn whole across its own width, at pitches of their own: their
        // lines interleave down the page, and two fall level.
        let left: Vec<f64> = (0..6).map(|i| 100.0 + 14.0 * i as f64).collect();
        let right: Vec<f64> = (0..5).map(|i| 107.0 + 17.0 * i as f64).collect();
        assert!(left
            .iter()
            .any(|a| right.iter().any(|b| (a - b).abs() < 2.0)));
        let mut rules: Vec<Rule> = left.iter().map(|&y| h(0.0, 250.0, y)).collect();
        rules.extend(right.iter().map(|&y| h(300.0, 550.0, y)));
        let stacks = unbroken_stacks(&rules);
        assert_eq!(stacks.len(), 2, "got {stacks:?}");
        let near = |a: f64, b: f64| (a - b).abs() < 1.0;
        assert!(stacks.iter().any(|(s, ys)| near(s.x0, 0.0)
            && near(s.x1, 250.0)
            && near(s.y0, 100.0)
            && near(s.y1, 170.0)
            && ys.len() == 6));
        assert!(stacks.iter().any(|(s, ys)| near(s.x0, 300.0)
            && near(s.x1, 550.0)
            && near(s.y0, 107.0)
            && near(s.y1, 175.0)
            && ys.len() == 5));
        // Three lines are no stack.
        assert!(unbroken_stacks(&rules[..3]).is_empty());
        // Lines drawn a column at a time, their pieces meeting end to end,
        // say columns: those are ruled stacks', not these.
        let broken: Vec<Rule> = left
            .iter()
            .flat_map(|&y| [h(0.0, 100.0, y), h(100.0, 250.0, y)])
            .collect();
        assert!(unbroken_stacks(&broken).is_empty());
        // A gap of many rows parts two tables of one width.
        let apart: Vec<Rule> = [100.0, 114.0, 128.0, 142.0, 300.0, 314.0, 328.0, 342.0]
            .iter()
            .map(|&y| h(0.0, 250.0, y))
            .collect();
        assert_eq!(unbroken_stacks(&apart).len(), 2);
    }

    #[test]
    fn a_header_band_over_a_label_column_is_the_tables_edge() {
        // A product table shaded across its header row, its rows ruled
        // from the second column on: only the rule between the two groups
        // reaches the label column, too few for its edge.
        let mut rules: Vec<Rule> = [10.0, 20.0, 40.0, 50.0, 60.0]
            .iter()
            .map(|&y| h(100.0, 300.0, y))
            .collect();
        rules.push(h(0.0, 300.0, 30.0));
        for x in [100.0, 200.0, 300.0] {
            rules.push(v(0.0, 60.0, x));
        }
        let band = [fill(0.0, 0.0, 300.0, 10.0)];
        let header = gly(20.0, 8.0, 50.0, "Product");
        let labels = [
            gly(5.0, 18.0, 60.0, "Unit A"),
            gly(5.0, 38.0, 60.0, "Unit B"),
        ];
        let text: Vec<_> = std::iter::once(header.clone())
            .chain(labels.iter().cloned())
            .chain([gly(120.0, 18.0, 40.0, "Input")])
            .collect();
        let grids = detect_framed(&rules, &band, &text, 0, &[]);
        assert_eq!(grids.len(), 1, "got {grids:?}");
        assert!(grids[0].xs[0].abs() < 1.0, "got {:?}", grids[0].xs);
        assert_eq!(grids[0].cols(), 3);
        // No name over the strip: the band is only wider than its table.
        let unnamed: Vec<_> = labels.iter().cloned().collect();
        let grids = detect_framed(&rules, &band, &unnamed, 0, &[]);
        assert!(
            (grids[0].xs[0] - 100.0).abs() < 1.0,
            "got {:?}",
            grids[0].xs
        );
        // No labels under it.
        let grids = detect_framed(&rules, &band, std::slice::from_ref(&header), 0, &[]);
        assert!(
            (grids[0].xs[0] - 100.0).abs() < 1.0,
            "got {:?}",
            grids[0].xs
        );
        // A band short of the table's far edge is no header across it.
        let short = [fill(0.0, 0.0, 250.0, 10.0)];
        let grids = detect_framed(&rules, &short, &text, 0, &[]);
        assert!(
            (grids[0].xs[0] - 100.0).abs() < 1.0,
            "got {:?}",
            grids[0].xs
        );
    }

    #[test]
    fn a_table_ruling_its_rows_is_not_parted_where_its_verticals_break() {
        // Rows ruled with horizontals, the verticals drawn in pieces that
        // break inside the two data rows.
        let xs = [0.0, 100.0, 200.0, 300.0];
        let mut rules: Vec<Rule> = [0.0, 20.0, 60.0, 100.0]
            .iter()
            .map(|&y| h(0.0, 300.0, y))
            .collect();
        for x in xs {
            rules.push(v(0.0, 20.0, x));
            for (a, b) in [(20.0, 40.0), (40.0, 60.0), (60.0, 80.0), (80.0, 100.0)] {
                rules.push(v(a, b, x));
            }
        }
        // The first row names its model once, over a second line of values
        // a line's leading under the first (`54 1/8` over `1375`); the
        // second row's break has a new row named under it.
        let glyphs = vec![
            gly(10.0, 38.0, 30.0, "T-49"),
            gly(110.0, 38.0, 30.0, "54"),
            gly(110.0, 48.0, 30.0, "1375"),
            gly(10.0, 77.0, 30.0, "R1"),
            gly(10.0, 92.0, 30.0, "R2"),
        ];
        let grids = detect_framed(&rules, &[], &glyphs, 0, &[]);
        assert_eq!(grids.len(), 1, "got {grids:?}");
        assert_eq!(grids[0].ys.len(), 5, "got {:?}", grids[0].ys);
        assert!(
            !grids[0].ys.iter().any(|y| (y - 40.0).abs() < 1.0),
            "got {:?}",
            grids[0].ys
        );
        // Where a shaded band ends at the break, the band parts the rows.
        let band = crate::rule::Fill {
            bbox: BBox {
                x0: 0.0,
                y0: 20.0,
                x1: 300.0,
                y1: 40.0,
            },
            page: 0,
            paint: None,
            stroked: false,
        };
        let grids = detect_framed(&rules, &[band], &glyphs, 0, &[]);
        assert_eq!(grids[0].ys.len(), 6, "got {:?}", grids[0].ys);
    }

    /// A label/value table: four 15pt rows, a 70pt row, two more 15pt rows.
    const TALL_YS: [f64; 8] = [0.0, 15.0, 30.0, 45.0, 60.0, 130.0, 145.0, 160.0];

    /// Labels and values on each short row's line.
    fn short_rows(value_x: f64) -> Vec<crate::glyph::Glyph> {
        let mut g = Vec::new();
        for (i, w) in TALL_YS.windows(2).enumerate() {
            if w[1] - w[0] < 20.0 {
                g.push(gly(10.0, w[0] + 11.0, 30.0, &format!("L{i}")));
                g.push(gly(value_x, w[0] + 11.0, 30.0, &format!("V{i}")));
            }
        }
        g
    }

    #[test]
    fn a_tall_row_its_rules_run_through_is_a_row() {
        let mut rules: Vec<Rule> = TALL_YS.iter().map(|&y| h(0.0, 300.0, y)).collect();
        for x in [0.0, 100.0, 300.0] {
            rules.push(v(0.0, 160.0, x));
        }
        // A label beside a paragraph of value: one row.
        let mut glyphs = short_rows(110.0);
        glyphs.push(gly(10.0, 71.0, 30.0, "Intended"));
        for k in 0..4 {
            glyphs.push(gly(110.0, 71.0 + 12.0 * k as f64, 150.0, "value"));
        }
        let grids = detect_framed(&rules, &[], &glyphs, 0, &[]);
        assert_eq!(grids.len(), 1, "got {grids:?}");
        assert_eq!(grids[0].rows(), 7);
        // A label on every line: rows never ruled, and the band is the gap
        // it always was.
        let mut glyphs = short_rows(110.0);
        for k in 0..4 {
            glyphs.push(gly(10.0, 71.0 + 12.0 * k as f64, 30.0, "row"));
            glyphs.push(gly(110.0, 71.0 + 12.0 * k as f64, 30.0, "value"));
        }
        let grids = detect_framed(&rules, &[], &glyphs, 0, &[]);
        assert!(grids.iter().all(|g| g.rows() < 7), "got {grids:?}");
    }

    #[test]
    fn a_tall_row_ruled_with_horizontals_alone_is_a_row() {
        // Each rule broken at the column, over and under the tall band alike.
        let rules: Vec<Rule> = TALL_YS
            .iter()
            .flat_map(|&y| [h(0.0, 97.0, y), h(103.0, 300.0, y)])
            .collect();
        let mut glyphs = short_rows(110.0);
        glyphs.push(gly(10.0, 71.0, 30.0, "Intended"));
        for k in 0..4 {
            glyphs.push(gly(110.0, 71.0 + 12.0 * k as f64, 150.0, "value"));
        }
        let grids = detect_framed(&rules, &[], &glyphs, 0, &[]);
        assert_eq!(grids.len(), 1, "got {grids:?}");
        assert_eq!(grids[0].rows(), 7);
        // A heading set between two tables holds one column: two tables.
        let mut glyphs = short_rows(110.0);
        glyphs.push(gly(10.0, 120.0, 60.0, "Dimensions"));
        let grids = detect_framed(&rules, &[], &glyphs, 0, &[]);
        assert!(grids.iter().all(|g| g.rows() < 7), "got {grids:?}");
    }

    #[test]
    fn a_tall_header_its_rules_run_through_is_the_tables_top_row() {
        // Column labels wrapped over four lines, over 15pt rows.
        let ys = [0.0, 70.0, 85.0, 100.0, 115.0, 130.0];
        let mut rules: Vec<Rule> = ys.iter().map(|&y| h(0.0, 300.0, y)).collect();
        for x in [0.0, 100.0, 200.0, 300.0] {
            rules.push(v(0.0, 130.0, x));
        }
        let grids = detect_framed(&rules, &[], &[], 0, &[]);
        assert_eq!(grids.len(), 1, "got {grids:?}");
        assert_eq!(grids[0].rows(), 5, "got {:?}", grids[0].ys);
    }

    #[test]
    fn a_label_turned_up_its_header_cell_is_read_into_it() {
        // A header row 60pt tall, its second column's name set on its side.
        let g = Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 100.0, 200.0],
            ys: vec![0.0, 60.0, 75.0],
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 200.0,
                y1: 75.0,
            },
        };
        let mut gl = vec![gly(10.0, 30.0, 30.0, "SKU")];
        let turned = |y: f64, t: &str| {
            let mut c = gly(150.0, y, 6.0, t);
            c.rotation_deg = 90.0;
            c.bbox = Some(BBox {
                x0: 144.0,
                y0: y - 6.0,
                x1: 150.0,
                y1: y,
            });
            c.origin = (150.0, y);
            c
        };
        // Reading up the page: `I`, `O`, `P`, `s`.
        for (k, t) in ["I", "O", "P", "s"].into_iter().enumerate() {
            gl.push(turned(50.0 - 7.0 * k as f64, t));
        }
        gl.push(gly(10.0, 70.0, 30.0, "A-1"));
        gl.push(gly(110.0, 70.0, 20.0, "12"));
        let cells = g.cell_texts(&gl);
        assert_eq!(cells[1], "IOPs", "got {cells:?}");
        // A sidebar running down through both rows is no cell's.
        let mut side = gl.clone();
        side.retain(|c| c.is_horizontal());
        for k in 0..10 {
            let mut c = turned(70.0 - 7.0 * k as f64, "x");
            c.bbox = Some(BBox {
                x0: 194.0,
                y0: 70.0 - 7.0 * k as f64 - 6.0,
                x1: 199.0,
                y1: 70.0 - 7.0 * k as f64,
            });
            c.origin = (199.0, 70.0 - 7.0 * k as f64);
            side.push(c);
        }
        let cells = g.cell_texts(&side);
        assert!(!cells.iter().any(|c| c.contains('x')), "got {cells:?}");
    }

    #[test]
    fn a_table_set_on_its_side_is_not_read_into_its_cells() {
        // A landscape table on a portrait page: every cell's text turned, its
        // rows running down the page. Read across the page's grid it would
        // come out transposed; its text is left to the prose.
        let g = Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 20.0, 40.0, 60.0],
            ys: vec![0.0, 60.0, 120.0],
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 60.0,
                y1: 120.0,
            },
        };
        let mut gl = Vec::new();
        for (c, x) in [12.0, 32.0, 52.0].into_iter().enumerate() {
            for (r, y) in [50.0, 110.0].into_iter().enumerate() {
                let t = ["P", "0", "1", "N", "2", "3"][c * 2 + r];
                let mut glyph = gly(x, y, 6.0, t);
                glyph.rotation_deg = 90.0;
                glyph.bbox = Some(BBox {
                    x0: x - 6.0,
                    y0: y - 6.0,
                    x1: x,
                    y1: y,
                });
                glyph.origin = (x, y);
                gl.push(glyph);
            }
        }
        let cells = g.cell_texts(&gl);
        assert!(cells.iter().all(|c| c.is_empty()), "got {cells:?}");
    }

    fn plain_grid(xs: Vec<f64>, ys: Vec<f64>) -> Grid {
        let bbox = BBox {
            x0: xs[0],
            y0: ys[0],
            x1: *xs.last().unwrap(),
            y1: *ys.last().unwrap(),
        };
        Grid {
            layout: None,
            page: 0,
            xs,
            ys,
            bbox,
        }
    }

    #[test]
    fn heads_set_over_a_tables_top_rule_are_its_top_row() {
        let grid = || {
            plain_grid(
                vec![0.0, 100.0, 200.0, 300.0],
                vec![100.0, 120.0, 140.0, 160.0],
            )
        };
        let rules = |from: f64| vec![h(from, 300.0, 100.0)];
        // Heads on two lines, each in its column, over the rule.
        let heads = vec![
            gly(5.0, 96.0, 40.0, "Substance"),
            gly(110.0, 96.0, 30.0, "TWA"),
            gly(210.0, 96.0, 30.0, "STEL"),
            gly(110.0, 86.0, 30.0, "OSHA"),
            gly(210.0, 86.0, 30.0, "NIOSH"),
        ];
        // The table's own text, at the heads' size.
        let cells = [
            gly(5.0, 115.0, 30.0, "ethanol"),
            gly(110.0, 115.0, 20.0, "1000"),
            gly(210.0, 115.0, 10.0, "-"),
        ];
        let heads = [heads, cells.to_vec()].concat();
        let mut g = grid();
        g.take_column_heads(&rules(0.0), &heads);
        assert_eq!(g.rows(), 4);
        assert_eq!(g.ys[0], 77.0);
        // A title over them stays out.
        let mut titled = [&heads[..3], &cells[..]].concat();
        titled.push(gly(5.0, 86.0, 250.0, "TABLE 3. EXPOSURE LIMITS"));
        let mut g = grid();
        g.take_column_heads(&rules(0.0), &titled);
        assert_eq!(g.ys[0], 87.0);
        // A line across the columns' edges is no row of heads.
        let mut g = grid();
        let mut across = cells.to_vec();
        across.push(gly(5.0, 96.0, 250.0, "TABLE 3. LIMITS"));
        g.take_column_heads(&rules(0.0), &across);
        assert_eq!(g.rows(), 3);
        // Nor is one head alone.
        let mut g = grid();
        g.take_column_heads(&rules(0.0), &[&heads[..1], &cells[..]].concat());
        assert_eq!(g.rows(), 3);
        // Nor a row of data over the rule: a label and its figures.
        let mut g = grid();
        let mut data = cells.to_vec();
        data.extend([
            gly(5.0, 96.0, 40.0, "South East"),
            gly(110.0, 96.0, 30.0, "57,384"),
        ]);
        g.take_column_heads(&rules(0.0), &data);
        assert_eq!(g.rows(), 3);
        // Nor two heads in one column: the grid is not theirs.
        let mut g = grid();
        let mut two = cells.to_vec();
        two.extend([
            gly(5.0, 96.0, 40.0, "Item"),
            gly(110.0, 96.0, 20.0, "2014"),
            gly(160.0, 96.0, 20.0, "2013"),
        ]);
        g.take_column_heads(&rules(0.0), &two);
        assert_eq!(g.rows(), 3);
        // A top ruled over part of the table is no top rule.
        let mut g = grid();
        g.take_column_heads(&rules(150.0), &heads);
        assert_eq!(g.rows(), 3);
    }

    #[test]
    fn a_shaded_first_row_of_figures_starting_stripes_is_data() {
        let rows: Vec<Vec<String>> = [
            ["Substance", "TWA", "STEL"],
            ["ethanol", "1000", "-"],
            ["2-propanol", "400", "500"],
            ["acetone", "1000", "-"],
            ["butanedione", "0.005", "0.025"],
        ]
        .iter()
        .map(|r| r.iter().map(|t| t.to_string()).collect())
        .collect();
        assert!(figures_row(&rows[1]));
        assert!(!figures_row(&rows[0]));
        assert!(figures_row(&[
            "ethanol".to_string(),
            "0.25 – 21.4".to_string(),
            "±17.0".to_string()
        ]));
        let g = plain_grid(
            vec![0.0, 100.0, 200.0, 300.0],
            (0..6).map(|k| k as f64 * 20.0).collect(),
        );
        // Heads shaded, then every other row from the first.
        let shaded = [
            fill(0.0, 0.0, 300.0, 20.0),
            fill(0.0, 20.0, 300.0, 40.0),
            fill(0.0, 60.0, 300.0, 80.0),
        ];
        assert_eq!(g.leading_fill_run(&shaded), 2);
        assert!(g.stripes_from(&shaded, 1));
        assert_eq!(g.header_rows(&rows, &[], &[], &shaded), 1);
    }

    #[test]
    fn a_first_stripe_painted_as_the_body_is_data() {
        let rows: Vec<Vec<String>> = [
            ["CLIN", "Title", "Type", "Estimated Cost"],
            ["00033-a", "Phase In", "CPAF", "$612,211,866"],
            ["00033-b", "Site Usage Fees", "CR", "$2,473,358"],
            [
                "00033-c",
                "Pension and Reservation Management",
                "CR",
                "$36,048,417",
            ],
            ["000341a", "Core Functions", "CPFF", "$251,387,408"],
            ["000341b", "Site Usage Fees", "CR", "$49,940,727"],
        ]
        .iter()
        .map(|r| r.iter().map(|t| t.to_string()).collect())
        .collect();
        assert!(!figures_row(&rows[1]));
        let g = plain_grid(
            vec![0.0, 100.0, 200.0, 300.0, 400.0],
            (0..7).map(|k| k as f64 * 20.0).collect(),
        );
        let paint = |rgb: [u8; 3], y0: f64| crate::rule::Fill {
            paint: Some(crate::rule::Paint::Solid([rgb[0], rgb[1], rgb[2], 255])),
            ..fill(0.0, y0, 400.0, y0 + 20.0)
        };
        let (dark, pale) = ([20, 96, 130], [193, 228, 245]);
        // A dark header, then the body striped pale from its first row.
        let shaded = [
            paint(dark, 0.0),
            paint(pale, 20.0),
            paint(pale, 60.0),
            paint(pale, 100.0),
        ];
        assert_eq!(g.leading_fill_run(&shaded), 2);
        assert_eq!(g.header_rows(&rows, &[], &[], &shaded), 1);
        // The header painted as the stripes are: the run is two header rows,
        // as it was.
        let alike = [
            paint(pale, 0.0),
            paint(pale, 20.0),
            paint(pale, 60.0),
            paint(pale, 100.0),
        ];
        assert_eq!(g.header_rows(&rows, &[], &[], &alike), 2);
    }

    #[test]
    fn a_band_over_a_group_of_columns_is_the_tables_top_row() {
        // A label column and three columns under a band; the rule under
        // the band runs over the three alone, and the label column's rule
        // runs on up beside the band.
        let g = plain_grid(
            vec![0.0, 40.0, 120.0, 200.0, 280.0],
            vec![114.0, 136.0, 150.0, 164.0, 178.0],
        );
        let rules = |stub: bool, top_from: f64| {
            let mut rs = vec![h(top_from, 280.0, 114.0)];
            rs.extend([136.0, 150.0, 164.0, 178.0].map(|y| h(0.0, 280.0, y)));
            rs.push(v(if stub { 100.0 } else { 114.0 }, 178.0, 40.0));
            rs.extend([120.0, 200.0].map(|x| v(114.0, 178.0, x)));
            rs
        };
        let band = [gly(45.0, 110.0, 150.0, "BAGS PER 100 FT OF RIDGE")];
        let taller = with_column_band(&g, &rules(true, 40.0), &band).expect("raised");
        assert_eq!(taller.ys[0], 100.0);
        assert_eq!(taller.rows(), g.rows() + 1);
        // Text running on over the label column is the table's title.
        let title = [gly(5.0, 110.0, 190.0, "ADDITIONAL BAGS PER 100 FT")];
        assert!(with_column_band(&g, &rules(true, 40.0), &title).is_none());
        // No rule running on above the top: the grid starts where it did.
        assert!(with_column_band(&g, &rules(false, 40.0), &band).is_none());
        // A top rule across every column closes the table; nothing opens.
        assert!(with_column_band(&g, &rules(true, 0.0), &band).is_none());
        // Nothing named in the band.
        assert!(with_column_band(&g, &rules(true, 40.0), &[]).is_none());
    }

    #[test]
    fn boxes_stacked_a_hair_apart_are_two_drawings() {
        // Two boxes 2.5 pt apart, each closed, parting their columns at
        // different places.
        let boxes = |second_divider: f64, joined: bool| {
            let mut rs = vec![
                h(0.0, 300.0, 0.0),
                h(0.0, 300.0, 40.0),
                v(0.0, 40.0, 100.0),
                h(0.0, 300.0, 42.5),
                h(0.0, 300.0, 80.0),
                v(42.5, 80.0, second_divider),
            ];
            if joined {
                rs.extend([v(0.0, 80.0, 0.0), v(0.0, 80.0, 300.0)]);
            } else {
                rs.extend([
                    v(0.0, 40.0, 0.0),
                    v(0.0, 40.0, 300.0),
                    v(42.5, 80.0, 0.0),
                    v(42.5, 80.0, 300.0),
                ]);
            }
            rs
        };
        let rs = boxes(200.0, false);
        let refs: Vec<&Rule> = rs.iter().collect();
        let gap = stacked_boxes_gap(&refs).expect("two boxes");
        assert!(gap > 40.0 && gap < 42.5);
        let comps = component_indices(&rs);
        assert_eq!(comps.len(), 2);
        // A header ruled off by a double line, the verticals broken across
        // it, parts its columns where the rows under it do.
        let rs = boxes(100.0, false);
        let refs: Vec<&Rule> = rs.iter().collect();
        assert!(stacked_boxes_gap(&refs).is_none());
        // Sides running on across the gap make one box.
        let rs = boxes(200.0, true);
        let refs: Vec<&Rule> = rs.iter().collect();
        assert!(stacked_boxes_gap(&refs).is_none());
        assert_eq!(component_indices(&rs).len(), 1);
    }

    #[test]
    fn section_dividers_down_two_columns_are_no_table() {
        // Rules over each section of a page set in two columns: the first
        // across both, then by turns across the right column and the left.
        // A short stroke of a drawn box among them crosses none of the
        // bands, and the trim runs out of lines first; of the rules left,
        // none is drawn over the same stretch as the next.
        let mut rules = vec![h(0.0, 140.0, 100.0), h(160.0, 300.0, 100.0)];
        rules.push(h(160.0, 300.0, 200.0));
        rules.push(h(0.0, 140.0, 300.0));
        rules.push(h(160.0, 300.0, 400.0));
        rules.push(v(335.0, 345.0, 50.0));
        // Each section holds its lines of text: a list of four.
        let text: Vec<crate::glyph::Glyph> = [
            130.0, 145.0, 160.0, 175.0, 230.0, 245.0, 260.0, 275.0, 330.0, 345.0, 360.0, 375.0,
        ]
        .iter()
        .flat_map(|&y| {
            [
                gly(10.0, y, 100.0, "Section text"),
                gly(170.0, y, 100.0, "Section text"),
            ]
        })
        .collect();
        assert!(detect_group(&rules, &[], &text, 0, &[]).is_empty());
        // Without text between them, or with a last row wrapped to three
        // lines, rules apart are two tables side by side ending apart, and
        // the grid stands as before.
        assert!(!detect_group(&rules, &[], &[], 0, &[]).is_empty());
        let wrapped: Vec<crate::glyph::Glyph> = [0.0, 100.0, 200.0]
            .iter()
            .flat_map(|&top| {
                [140.0, 155.0, 170.0].map(move |y| gly(10.0, top + y, 100.0, "Last row"))
            })
            .collect();
        assert!(!detect_group(&rules, &[], &wrapped, 0, &[]).is_empty());

        // A table ruled with horizontals only under a stroke of decoration
        // is still one (`a_horizontally_ruled_table_is_not_split_into_rows`):
        // its rules share their stretch of the page.

        // A table whose lower rows its frame crosses keeps them, under a
        // heading's underline the trim takes off.
        let mut table: Vec<Rule> = [60.0, 100.0, 120.0, 140.0, 160.0]
            .iter()
            .map(|&y| h(0.0, 300.0, y))
            .collect();
        table.extend([
            v(100.0, 160.0, 0.0),
            v(100.0, 160.0, 150.0),
            v(100.0, 160.0, 300.0),
        ]);
        let grids = detect_group(&table, &[], &[], 0, &[]);
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].rows(), 3);
    }

    #[test]
    fn a_stub_beside_header_and_body_leaves_the_header_closed() {
        // A stub labelling every row, a size label over two header rows, a
        // group label over its two leaves; the rule under the header runs
        // under every column but the stub's.
        let g = plain_grid(
            vec![0.0, 100.0, 200.0, 300.0, 400.0],
            vec![0.0, 20.0, 40.0, 60.0, 80.0, 100.0],
        );
        let rules = vec![
            h(0.0, 400.0, 0.0),
            h(200.0, 400.0, 20.0),
            h(100.0, 400.0, 40.0),
            h(100.0, 400.0, 60.0),
            h(100.0, 400.0, 80.0),
            h(0.0, 400.0, 100.0),
            v(0.0, 100.0, 0.0),
            v(0.0, 100.0, 100.0),
            v(0.0, 100.0, 200.0),
            v(20.0, 100.0, 300.0),
            v(0.0, 100.0, 400.0),
        ];
        let merges = g.merges(&rules, &[]);
        let t = rows(&[
            &["Tolerance", "Size", "Length", ""],
            &["", "", "Short", "Long"],
            &["", "M6", "1.0", "2.0"],
            &["", "M8", "1.5", "2.5"],
            &["", "M10", "2.0", "3.0"],
        ]);
        assert_eq!(g.stacked_header_rows(1, &t, &merges, &rules), 2);
    }

    /// A ruled box, columns at `xs`, one row from y=100 to y=140, each
    /// line `(x, baseline, text)` set as glyphs: the grid, its glyphs and
    /// its frame's rules.
    fn stacked_box(
        xs: &[f64],
        lines: &[(f64, f64, &str)],
    ) -> (Grid, Vec<crate::glyph::Glyph>, Vec<Rule>) {
        let g = plain_grid(xs.to_vec(), vec![100.0, 140.0]);
        let mut glyphs = Vec::new();
        for &(x, y, t) in lines {
            glyphs.extend(
                t.chars()
                    .enumerate()
                    .map(|(i, c)| gly(x + 5.0 * i as f64, y, 5.0, &c.to_string())),
            );
        }
        let mut rules = vec![
            h(xs[0], xs[xs.len() - 1], 100.0),
            h(xs[0], xs[xs.len() - 1], 140.0),
        ];
        rules.extend(xs.iter().map(|&x| v(100.0, 140.0, x)));
        (g, glyphs, rules)
    }

    #[test]
    fn values_stacked_one_a_line_part_their_row() {
        // Totals' labels over each other beside their amounts, a block of
        // text beside them on lines of its own.
        let (mut g, glyphs, rules) = stacked_box(
            &[0.0, 150.0, 300.0, 400.0],
            &[
                (5.0, 112.0, "Ordered on behalf"),
                (5.0, 124.0, "of the office"),
                (5.0, 136.0, "Buyer"),
                (155.0, 115.0, "TOTAL"),
                (155.0, 127.0, "TOTAL VAT"),
                (305.0, 115.0, "17,937.50"),
                (305.0, 127.0, "3,587.50"),
            ],
        );
        assert_eq!(g.split_stacked_rows(&glyphs), vec![(0, 1)]);
        assert_eq!(g.rows(), 2);
        let texts = g.cell_texts(&glyphs);
        assert_eq!(
            (texts[1].as_str(), texts[2].as_str()),
            ("TOTAL", "17,937.50")
        );
        assert_eq!(
            (texts[4].as_str(), texts[5].as_str()),
            ("TOTAL VAT", "3,587.50")
        );
        // The block beside them spans both rows, read whole.
        let mut m = g.merges(&rules, &[]);
        assert_eq!(
            g.stacked_merges(&glyphs, &rules, &[(100.0, 140.0)], &mut m),
            vec![(0..2, 0)]
        );
        assert!(m.continues_above[3]);
        assert!(!m.continues_above[4] && !m.continues_above[5]);
        // A note mark beside the second amount alone is that row's: its
        // column stacks with the amounts, its first cell empty.
        let (mut g, glyphs, rules) = stacked_box(
            &[0.0, 150.0, 300.0, 400.0, 450.0],
            &[
                (5.0, 112.0, "Ordered on behalf"),
                (5.0, 124.0, "of the office"),
                (5.0, 136.0, "Buyer"),
                (155.0, 115.0, "TOTAL"),
                (155.0, 127.0, "TOTAL VAT"),
                (305.0, 115.0, "17,937.50"),
                (305.0, 127.0, "3,587.50"),
                (405.0, 127.0, "a"),
            ],
        );
        assert_eq!(g.split_stacked_rows(&glyphs), vec![(0, 1)]);
        let mut m = g.merges(&rules, &[]);
        assert_eq!(
            g.stacked_merges(&glyphs, &rules, &[(100.0, 140.0)], &mut m),
            vec![(0..2, 0)]
        );
        assert!(!m.continues_above[4 + 3]);
        let texts = g.cell_texts(&glyphs);
        assert_eq!((texts[3].as_str(), texts[7].as_str()), ("", "a"));
        // A label of one line level with the first values spans their rows
        // (`Al` beside `INAA` over `WDXRF`); rows the table set apart
        // itself, not parted here, are left as they are.
        let (mut g, glyphs, rules) = stacked_box(
            &[0.0, 150.0, 300.0, 400.0],
            &[
                (5.0, 115.0, "Al"),
                (155.0, 115.0, "INAA"),
                (155.0, 127.0, "WDXRF"),
                (305.0, 115.0, "1.15"),
                (305.0, 127.0, "0.13"),
            ],
        );
        assert_eq!(g.split_stacked_rows(&glyphs), vec![(0, 1)]);
        let mut m = g.merges(&rules, &[]);
        assert!(g.stacked_merges(&glyphs, &rules, &[], &mut m).is_empty());
        assert_eq!(
            g.stacked_merges(&glyphs, &rules, &[(100.0, 140.0)], &mut m),
            vec![(0..2, 0)]
        );
    }

    #[test]
    fn wrapped_text_or_one_column_of_figures_parts_no_row() {
        // Wrapped text beside wrapped text lines up, and holds no figures.
        let (mut g, glyphs, _) = stacked_box(
            &[0.0, 150.0, 300.0],
            &[
                (5.0, 115.0, "The quick brown"),
                (5.0, 127.0, "fox jumps"),
                (155.0, 115.0, "Second column"),
                (155.0, 127.0, "wraps here"),
            ],
        );
        assert!(g.split_stacked_rows(&glyphs).is_empty());
        // Figures stacked beside one label are that label's values.
        let (mut g, glyphs, _) = stacked_box(
            &[0.0, 150.0, 300.0],
            &[
                (5.0, 115.0, "Dimensions"),
                (155.0, 115.0, "120"),
                (155.0, 127.0, "80"),
            ],
        );
        assert!(g.split_stacked_rows(&glyphs).is_empty());
        // Nor a figure wrapped at its thousands space, a count over its
        // share in brackets, or a range run on in words.
        for (a, b) in [("453", "500,00"), ("5", "(3.2%)"), ("36,0 à", "38,0")] {
            let (mut g, glyphs, _) = stacked_box(
                &[0.0, 150.0, 300.0],
                &[
                    (5.0, 115.0, "Atelier"),
                    (5.0, 127.0, "Saulnier"),
                    (155.0, 115.0, a),
                    (155.0, 127.0, b),
                ],
            );
            assert!(g.split_stacked_rows(&glyphs).is_empty(), "{a} / {b}");
        }
        // A figure's sign past the ASCII range (`50−60`) is read by its
        // characters, not cut through.
        let (mut g, glyphs, _) = stacked_box(
            &[0.0, 150.0, 300.0],
            &[
                (5.0, 115.0, "Atelier"),
                (5.0, 127.0, "Saulnier"),
                (155.0, 115.0, "4"),
                (155.0, 127.0, "50\u{2212}60"),
            ],
        );
        let _ = g.split_stacked_rows(&glyphs);
        // Nor a label's second line run on in lower case.
        let (mut g, glyphs, _) = stacked_box(
            &[0.0, 150.0, 300.0],
            &[
                (5.0, 115.0, "Total amount"),
                (5.0, 127.0, "excluding tax"),
                (155.0, 115.0, "120"),
                (155.0, 127.0, "80"),
            ],
        );
        assert!(g.split_stacked_rows(&glyphs).is_empty());
    }

    #[test]
    fn two_tables_boxed_in_one_frame_part_at_the_blank_band() {
        // One frame round two tables, its sides running on down through the
        // blank band between them, each ruled at columns of its own.
        let mut rules: Vec<Rule> = [0.0, 20.0, 40.0, 60.0, 80.0, 100.0, 120.0, 140.0, 160.0]
            .iter()
            .map(|&y| h(0.0, 300.0, y))
            .collect();
        rules.extend([v(0.0, 160.0, 0.0), v(0.0, 160.0, 300.0)]);
        rules.extend([
            v(0.0, 80.0, 100.0),
            v(0.0, 80.0, 200.0),
            v(100.0, 160.0, 120.0),
            v(100.0, 160.0, 220.0),
        ]);
        let grids = detect_group(&rules, &[], &[], 0, &[]);
        assert_eq!(grids.len(), 2, "{grids:?}");
        assert_eq!(grids[0].cols(), 3);
        assert_eq!(grids[1].cols(), 3);
        // A box of one row under a table, its one rule inside set apart from
        // the table's: no second table to part off.
        let mut boxed: Vec<Rule> = [0.0, 20.0, 40.0, 60.0, 80.0, 100.0, 120.0]
            .iter()
            .map(|&y| h(0.0, 300.0, y))
            .collect();
        boxed.extend([v(0.0, 120.0, 0.0), v(0.0, 120.0, 300.0)]);
        boxed.extend([
            v(0.0, 80.0, 100.0),
            v(0.0, 80.0, 200.0),
            v(100.0, 120.0, 150.0),
        ]);
        let grids = detect_group(&boxed, &[], &[], 0, &[]);
        assert_eq!(grids.len(), 1, "{grids:?}");
        // A label set across the band, a group's banner: one table.
        let banner = [gly(120.0, 94.0, 60.0, "Group")];
        let grids = detect_group(&rules, &[], &banner, 0, &[]);
        assert_eq!(grids.len(), 1, "{grids:?}");
    }

    #[test]
    fn a_rule_drawn_in_overlapping_pieces_has_one_column_edge_per_overlap() {
        let ends = |pieces: &[(f64, f64)]| {
            let rules: Vec<Rule> = pieces.iter().map(|&(a, b)| h(a, b, 100.0)).collect();
            let refs: Vec<&Rule> = rules.iter().collect();
            let mut e = cluster(&refs)[0].ends.clone();
            e.sort_by(f64::total_cmp);
            e.dedup();
            e
        };
        // Each cell's piece run on into the next: one edge, mid-overlap,
        // whether they overlap by a few points or by a column's padding.
        assert_eq!(
            ends(&[(0.0, 104.0), (100.0, 204.0), (200.0, 300.0)]),
            vec![0.0, 102.0, 202.0, 300.0]
        );
        assert_eq!(
            ends(&[(0.0, 200.0), (150.0, 300.0)]),
            vec![0.0, 175.0, 300.0]
        );
        // Drawn a cell at a time, pieces meeting end to end: columns.
        assert_eq!(
            ends(&[(0.0, 100.0), (100.0, 200.0)]),
            vec![0.0, 100.0, 200.0]
        );
        // Group underlines under a rule drawn across all of them keep theirs.
        assert_eq!(
            ends(&[(0.0, 300.0), (10.0, 140.0), (160.0, 290.0)]),
            vec![0.0, 10.0, 140.0, 160.0, 290.0, 300.0]
        );
    }

    #[test]
    fn a_vertical_elsewhere_on_the_page_trims_no_rows() {
        // A table ruled with horizontals alone, each drawn a column at a
        // time, and a logo's strokes at the page foot: the page-wide reading
        // takes in every rule, and the logo's verticals cross none of the
        // table's rows.
        let mut rules = Vec::new();
        for k in 0..7 {
            let y = 100.0 + 20.0 * k as f64;
            for x in [0.0, 100.0, 200.0] {
                rules.push(h(x, x + 100.0, y));
            }
        }
        for x in [10.0, 14.0, 18.0] {
            rules.push(v(700.0, 715.0, x));
        }
        let grids = detect_group(&rules, &[], &[], 0, &[]);
        assert!(
            grids.iter().any(|g| g.rows() == 6 && g.cols() == 3),
            "got {:?}",
            grids
                .iter()
                .map(|g| (g.rows(), g.cols()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn section_bands_part_rows_but_stripes_do_not() {
        let xs = [0.0, 100.0, 200.0, 300.0];
        let ys = vec![0.0, 30.0, 90.0, 120.0, 180.0];
        // Two bands rows apart, each across the table, drawn a cell at a
        // time with no rule over or under them.
        let mut cells = Vec::new();
        for (y0, y1) in [(50.0, 64.0), (140.0, 154.0)] {
            for x in [0.0, 100.0, 200.0] {
                cells.push(fill(x, y0, x + 100.0, y1));
            }
        }
        // A row over each band and a row under it.
        let rows: Vec<_> = [45.0, 80.0, 135.0, 170.0]
            .into_iter()
            .map(|y| gly(5.0, y, 40.0, "Weight"))
            .collect();
        let parted = with_fill_edges(ys.clone(), &xs, &cells, &rows, true, &[]);
        for y in [50.0, 64.0, 140.0, 154.0] {
            assert!(parted.contains(&y), "{y} in {parted:?}");
        }
        // A band set in the blank between two field lists, a heading's,
        // parts no rows: the blank stays the gap between them.
        let lists = vec![0.0, 12.0, 24.0, 36.0, 84.0, 96.0, 108.0];
        let heading = [fill(0.0, 49.0, 300.0, 71.0)];
        let text: Vec<_> = [10.0, 22.0, 34.0, 94.0, 106.0]
            .into_iter()
            .map(|y| gly(5.0, y, 40.0, "Length"))
            .chain([gly(5.0, 63.0, 60.0, "Dimensions")])
            .collect();
        assert_eq!(
            with_fill_edges(lists.clone(), &xs, &heading, &text, true, &[]),
            lists
        );
        // Stripes a row apart are no rules.
        let stripes = [
            fill(0.0, 34.0, 300.0, 46.0),
            fill(0.0, 58.0, 300.0, 70.0),
            fill(0.0, 82.0, 300.0, 89.0),
        ];
        assert_eq!(
            with_fill_edges(ys.clone(), &xs, &stripes, &[], true, &[]),
            ys
        );
    }

    #[test]
    fn a_band_over_the_first_rule_is_the_tables_top() {
        // The header shaded across the table a row over its first rule,
        // its rules drawn only under the rows below.
        let xs = [0.0, 100.0, 200.0, 300.0];
        let ys = vec![100.0, 120.0, 140.0, 160.0];
        let band = [fill(0.0, 72.0, 300.0, 86.0)];
        let names = [
            gly(5.0, 82.0, 40.0, "Model"),
            gly(105.0, 82.0, 40.0, "WD261"),
            gly(205.0, 82.0, 40.0, "WD242"),
        ];
        let topped = with_fill_edges(ys.clone(), &xs, &band, &names, true, &[]);
        assert_eq!(&topped[..3], &[72.0, 86.0, 100.0]);
        // A title band over the table, one label, stays its title.
        let title = [gly(105.0, 82.0, 60.0, "Specifications")];
        assert_eq!(
            with_fill_edges(ys.clone(), &xs, &band, &title, true, &[]),
            ys
        );
        // Nor a header ruled into cells of its own, verticals between its
        // names: a box of its own, read as one.
        let ruled = [v(72.0, 86.0, 100.0).bbox, v(72.0, 86.0, 200.0).bbox];
        assert_eq!(
            with_fill_edges(ys.clone(), &xs, &band, &names, true, &ruled),
            ys
        );
        // Not a box set well above it.
        let apart = [fill(0.0, 20.0, 300.0, 34.0)];
        assert_eq!(
            with_fill_edges(ys.clone(), &xs, &apart, &names, true, &[]),
            ys
        );
    }

    #[test]
    fn a_shaded_header_band_parts_its_first_row() {
        // A header shaded down to y 10 over a first row with no rule between.
        let xs = [0.0, 100.0, 200.0];
        let ys = vec![0.0, 20.0, 40.0];
        let band = fill(0.0, 0.0, 200.0, 10.0);
        let parted = with_fill_edges(ys.clone(), &xs, std::slice::from_ref(&band), &[], true, &[]);
        assert_eq!(parted, vec![0.0, 10.0, 20.0, 40.0]);
        // Not through a line of text set across the edge.
        let across = gly(20.0, 14.0, 30.0, "Name");
        let kept = with_fill_edges(
            ys.clone(),
            &xs,
            std::slice::from_ref(&band),
            &[across],
            true,
            &[],
        );
        assert_eq!(kept, ys);
        // Nor at the seam of a column shaded as one block, a fill a row.
        let next = fill(0.0, 10.0, 200.0, 20.0);
        let seam = with_fill_edges(ys.clone(), &xs, &[band, next], &[], true, &[]);
        assert_eq!(seam, ys);
    }

    #[test]
    fn a_shaded_row_closes_the_columns_its_rule_leaves_open() {
        // A rule under the first column only, at y 20; the second column's
        // shading ends on the same line.
        let g = plain_grid(vec![0.0, 100.0, 200.0], vec![0.0, 10.0, 20.0, 30.0]);
        let rules = vec![h(0.0, 100.0, 20.0)];
        let open = g.merges(&rules, &[]);
        assert!(
            open.continues_above[2 * 2 + 1],
            "no shading: the column is open"
        );
        let shaded = [fill(100.0, 10.0, 200.0, 20.0)];
        let closed = g.merges(&rules, &shaded);
        assert!(
            !closed.continues_above[2 * 2 + 1],
            "the shading's edge parts it"
        );
        // A shading that carries on in a fill butted on below parts nothing.
        let block = [
            fill(100.0, 10.0, 200.0, 20.0),
            fill(100.0, 20.0, 200.0, 30.0),
        ];
        assert!(g.merges(&rules, &block).continues_above[2 * 2 + 1]);
    }

    #[test]
    fn a_single_bold_unit_over_its_column_restates_the_header() {
        // A measurements table: `mm` over its column, then, partway down,
        // `in` restated over the one column its row fills.
        let g = plain_grid(
            vec![0.0, 100.0, 200.0, 300.0],
            (0..6).map(|k| k as f64 * 20.0).collect(),
        );
        let rows = |unit_col: usize| {
            let mut r = rows(&[
                &["", "mm", "kg"],
                &["Length", "120", "4"],
                &["", "", ""],
                &["Height", "4.7", ""],
                &["Depth", "3.1", ""],
            ]);
            r[2][unit_col] = "in".into();
            r
        };
        let set = |rows: &Vec<Vec<String>>, bold_row: usize| -> Vec<crate::glyph::Glyph> {
            let mut out = Vec::new();
            for (r, row) in rows.iter().enumerate() {
                for (c, t) in row.iter().enumerate().filter(|(_, t)| !t.is_empty()) {
                    let mut x = gly(c as f64 * 100.0 + 5.0, r as f64 * 20.0 + 14.0, 30.0, t);
                    x.weight = Some(if r == 0 || r == bold_row { 700 } else { 400 });
                    out.push(x);
                }
            }
            out
        };
        let unit = rows(1);
        assert_eq!(
            g.restated_header_rows(&unit, 1, &[], &set(&unit, 2), &[]),
            vec![2]
        );
        // Straight under the header, it is the header's label wrapped.
        let mut wrapped = rows(1);
        wrapped.swap(1, 2);
        assert!(g
            .restated_header_rows(&wrapped, 1, &[], &set(&wrapped, 1), &[])
            .is_empty());
        // Set plain, it is one more value.
        assert!(g
            .restated_header_rows(&unit, 1, &[], &set(&unit, 9), &[])
            .is_empty());
        // Alone in the first column, a name labels the rows' group.
        let group = rows(0);
        assert!(g
            .restated_header_rows(&group, 1, &[], &set(&group, 2), &[])
            .is_empty());
    }

    #[test]
    fn a_bold_row_of_labels_partway_down_restates_the_header() {
        let g = plain_grid(
            vec![0.0, 100.0, 200.0, 300.0],
            (0..6).map(|k| k as f64 * 20.0).collect(),
        );
        let rows = rows(&[
            &["", "LB/Gal", "g/L"],
            &["Coating Density", "11.95", "1432"],
            &["VOC Content", "By wt", "By vol"],
            &["Total Volatiles", "39.0%", "55.7%"],
            &["Water", "39.0%", "55.6%"],
        ]);
        let set = |r: usize, bold: &[bool]| -> Vec<crate::glyph::Glyph> {
            rows[r]
                .iter()
                .enumerate()
                .filter(|(_, t)| !t.is_empty())
                .map(|(c, t)| {
                    let mut x = gly(c as f64 * 100.0 + 5.0, r as f64 * 20.0 + 14.0, 30.0, t);
                    x.weight = Some(if bold[c] { 700 } else { 400 });
                    x
                })
                .collect()
        };
        let mut glyphs = Vec::new();
        for r in 0..5 {
            // The labels in the first column keep their own weight.
            glyphs.extend(set(r, &[false, r == 2 || r == 0, r == 2 || r == 0]));
        }
        assert_eq!(g.restated_header_rows(&rows, 1, &[], &glyphs, &[]), vec![2]);
        // A row of figures set bold is a total, not a header.
        let mut totals = rows.clone();
        totals[2] = vec!["Total".into(), "12".into(), "14".into()];
        assert!(g
            .restated_header_rows(&totals, 1, &[], &glyphs, &[])
            .is_empty());
        // Nor is a band across the row.
        assert!(g
            .restated_header_rows(&rows, 1, &[2], &glyphs, &[])
            .is_empty());
        // One bold word beside a label is emphasis, not a header.
        let mut one = Vec::new();
        for r in 0..5 {
            one.extend(set(r, &[false, r == 2 || r == 0, r == 0]));
        }
        assert!(g.restated_header_rows(&rows, 1, &[], &one, &[]).is_empty());
        // A bold row of names under another is the table's top block
        // continuing, not a header restated.
        let top: Vec<Vec<String>> = [
            ["Class", "Mission Critical", "Business Critical"],
            ["Workload", "High", "Medium"],
            ["Usage", "Email, planning", "Backup, archive"],
            ["Interface", "SAS 12 Gb", "SATA 6 Gb"],
            ["RPM", "10000", "7200"],
        ]
        .iter()
        .map(|r| r.iter().map(|t| t.to_string()).collect())
        .collect();
        let set_top = |r: usize| -> Vec<crate::glyph::Glyph> {
            top[r]
                .iter()
                .enumerate()
                .map(|(c, t)| {
                    let mut x = gly(c as f64 * 100.0 + 5.0, r as f64 * 20.0 + 14.0, 30.0, t);
                    x.weight = Some(if r < 2 { 700 } else { 400 });
                    x
                })
                .collect()
        };
        let glyphs: Vec<_> = (0..5).flat_map(set_top).collect();
        assert!(g
            .restated_header_rows(&top, 0, &[], &glyphs, &[])
            .is_empty());
    }

    #[test]
    fn a_row_shaded_like_the_header_restates_it_but_a_watermark_does_not() {
        let g = plain_grid(
            vec![0.0, 100.0, 200.0, 300.0],
            (0..6).map(|k| k as f64 * 20.0).collect(),
        );
        let rows = rows(&[
            &["", "Alpha", "Beta"],
            &["Size", "12", "14"],
            &["Mode", "Fast", "Slow"],
            &["Rate", "3", "4"],
            &["Load", "5", "6"],
        ]);
        // The header and the row restating it, each shaded on its own.
        let bands = [fill(0.0, 0.0, 300.0, 20.0), fill(0.0, 40.0, 300.0, 60.0)];
        assert_eq!(g.restated_header_rows(&rows, 1, &[], &[], &bands), vec![2]);
        // A page's watermark drawn under the top three rows, ending above
        // the rest, shades none of them.
        let watermark = [fill(0.0, -200.0, 300.0, 60.0)];
        assert!(g
            .restated_header_rows(&rows, 1, &[], &[], &watermark)
            .is_empty());
        // A table laid on a background is shaded throughout: no row of it
        // stands apart by its shading.
        let mut laid = bands.to_vec();
        laid.push(fill(-10.0, -10.0, 310.0, 110.0));
        assert!(g.restated_header_rows(&rows, 1, &[], &[], &laid).is_empty());
    }

    #[test]
    fn a_row_of_names_on_their_side_restates_the_header() {
        // Field rows, then a row whose columns are named on their side over
        // rows of codes and figures; no weights, no shading.
        let g = plain_grid(
            vec![0.0, 60.0, 120.0, 180.0],
            vec![0.0, 20.0, 40.0, 100.0, 120.0, 140.0],
        );
        let rows = rows(&[
            &["Form", "SFF", ""],
            &["Speed", "10000 rpm", ""],
            &["SKU", "Throughput (MiB/s)", "Random Rd (IOPs)"],
            &["P40430-B21", "195", "170"],
            &["P53561-B21", "200", "165"],
        ]);
        let mut glyphs = Vec::new();
        for (x, t) in [(90.0, "T"), (150.0, "R")] {
            let mut c = gly(x, 90.0, 6.0, t);
            c.rotation_deg = 90.0;
            c.bbox = Some(BBox {
                x0: x - 6.0,
                y0: 84.0,
                x1: x,
                y1: 90.0,
            });
            c.origin = (x, 90.0);
            glyphs.push(c);
        }
        assert_eq!(g.restated_header_rows(&rows, 0, &[], &glyphs, &[]), vec![2]);
    }

    #[test]
    fn a_column_rule_drawn_in_some_rows_is_a_column() {
        let ys: Vec<f64> = (0..=10).map(|k| k as f64 * 20.0).collect();
        let mut rules = ruled(&[0.0, 100.0, 200.0, 300.0], &ys);
        // Ruled apart in two rows of ten; in a third a few points over, as a
        // word processor sizes each row's cells on its own.
        rules.push(v(20.0, 40.0, 150.0));
        rules.push(v(40.0, 60.0, 150.0));
        rules.push(v(80.0, 100.0, 154.0));
        // In one row only: no column.
        rules.push(v(60.0, 80.0, 250.0));
        let grids = detect(&rules, 0);
        assert_eq!(grids.len(), 1, "got {grids:?}");
        let xs = &grids[0].xs;
        assert_eq!(xs.len(), 5, "got {xs:?}");
        assert!(xs.iter().any(|x| (x - 151.0).abs() < 2.0), "got {xs:?}");
        assert!(!xs.iter().any(|x| (x - 250.0).abs() < 2.0), "got {xs:?}");
    }

    #[test]
    fn tables_set_flush_one_under_the_other_are_two_grids() {
        // A browser prints consecutive tables with no gap: a 4-column line
        // table and, sharing its bottom border, a 2-column totals table
        // whose one boundary (90) falls near the line table's first (98).
        let mut rules = ruled(
            &[34.0, 98.0, 129.0, 169.0, 209.0],
            &[113.0, 132.0, 151.0, 170.0],
        );
        rules.extend(ruled(&[34.0, 90.0, 130.0], &[170.0, 189.0, 208.0, 227.0]));
        let grids = detect(&rules, 0);
        assert_eq!(grids.len(), 2, "got {grids:?}");
        assert_eq!(grids[0].xs.len(), 5, "the line table keeps its own columns");
        assert_eq!(grids[1].xs.len(), 3);

        // Totals ruled under the last two columns draw no boundary of their
        // own: they stay rows of the table, on its columns.
        let mut rules = ruled(
            &[34.0, 98.0, 129.0, 169.0, 209.0],
            &[113.0, 132.0, 151.0, 170.0],
        );
        rules.extend(ruled(&[129.0, 169.0, 209.0], &[170.0, 189.0, 208.0]));
        let grids = detect(&rules, 0);
        assert_eq!(grids.len(), 1, "got {grids:?}");
        assert_eq!(grids[0].rows(), 5);
    }

    #[test]
    fn a_bold_label_column_does_not_make_its_first_row_a_header() {
        // Subtotal | 38.60 over Tax | 3.86: the labels are bold on every
        // row, the amounts on none.
        let g = Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 100.0, 200.0],
            ys: vec![100.0, 120.0, 140.0, 160.0],
            bbox: BBox {
                x0: 0.0,
                y0: 100.0,
                x1: 200.0,
                y1: 160.0,
            },
        };
        let run = |x: f64, y: f64, text: &str, bold: bool| -> Vec<crate::glyph::Glyph> {
            text.chars()
                .enumerate()
                .map(|(i, c)| {
                    let mut gl = gly(x + 6.0 * i as f64, y, 5.0, &c.to_string());
                    gl.weight = Some(if bold { 700 } else { 400 });
                    gl
                })
                .collect()
        };
        let mut glyphs = Vec::new();
        for (y, label, amount) in [
            (115.0, "Subtotal", "38.60"),
            (135.0, "Tax", "3.86"),
            (155.0, "Total", "42.46"),
        ] {
            glyphs.extend(run(5.0, y, label, true));
            glyphs.extend(run(105.0, y, amount, false));
        }
        let t = rows(&[
            &["Subtotal", "38.60"],
            &["Tax", "3.86"],
            &["Total", "42.46"],
        ]);
        assert_eq!(g.header_rows(&t, &glyphs, &[], &[]), 0);
        // A header bold in every cell over plain rows still is one, numbers
        // and all.
        let mut glyphs = Vec::new();
        glyphs.extend(run(5.0, 115.0, "2023", true));
        glyphs.extend(run(105.0, 115.0, "2024", true));
        glyphs.extend(run(5.0, 135.0, "10", false));
        glyphs.extend(run(105.0, 135.0, "12", false));
        let t = rows(&[&["1", "2"], &["10", "12"], &["", ""]]);
        assert_eq!(g.header_rows(&t, &glyphs, &[], &[]), 1);
    }

    #[test]
    fn two_bold_label_columns_beside_plain_values_make_no_header() {
        // A field's name and the size each value is for, both bold on every
        // row, the values plain: a list of fields, the first row one of them.
        let g = plain_grid(
            vec![0.0, 100.0, 150.0, 300.0],
            vec![100.0, 120.0, 140.0, 160.0],
        );
        let run = |x: f64, y: f64, text: &str, bold: bool| -> Vec<crate::glyph::Glyph> {
            text.chars()
                .enumerate()
                .map(|(i, c)| {
                    let mut gl = gly(x + 6.0 * i as f64, y, 5.0, &c.to_string());
                    gl.weight = Some(if bold { 700 } else { 400 });
                    gl
                })
                .collect()
        };
        let mut glyphs = Vec::new();
        for (y, label, size, value) in [
            (115.0, "Code", "4 ft", "7712"),
            (135.0, "Count", "4 ft", "60"),
            (155.0, "Weight", "4 ft", "12 kg"),
        ] {
            glyphs.extend(run(5.0, y, label, true));
            glyphs.extend(run(105.0, y, size, true));
            glyphs.extend(run(155.0, y, value, false));
        }
        let t = rows(&[
            &["Code", "4 ft", "7712"],
            &["Count", "4 ft", "60"],
            &["Weight", "4 ft", "12 kg"],
        ]);
        assert_eq!(g.header_rows(&t, &glyphs, &[], &[]), 0);
        // A header bold across every column, the values' too, still is one.
        let mut glyphs = Vec::new();
        glyphs.extend(run(5.0, 115.0, "Item", true));
        glyphs.extend(run(105.0, 115.0, "Size", true));
        glyphs.extend(run(155.0, 115.0, "Price", true));
        for (y, a, b, c) in [(135.0, "Bolt", "M6", "0.40"), (155.0, "Nut", "M6", "0.10")] {
            glyphs.extend(run(5.0, y, a, true));
            glyphs.extend(run(105.0, y, b, true));
            glyphs.extend(run(155.0, y, c, false));
        }
        let t = rows(&[
            &["Item", "Size", "Price"],
            &["Bolt", "M6", "0.40"],
            &["Nut", "M6", "0.10"],
        ]);
        assert_eq!(g.header_rows(&t, &glyphs, &[], &[]), 1);
    }

    /// Two columns of `fields`, one 20pt row each, label and value set in
    /// `weight` (the first row in `head`), with a rule under every row.
    fn field_list(
        fields: &[(&str, &str)],
        head: Option<u32>,
        weight: Option<u32>,
    ) -> (Grid, Vec<Vec<String>>, Vec<crate::glyph::Glyph>, Vec<Rule>) {
        let ys: Vec<f64> = (0..=fields.len())
            .map(|i| 100.0 + 20.0 * i as f64)
            .collect();
        let g = Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 100.0, 400.0],
            ys: ys.clone(),
            bbox: BBox {
                x0: 0.0,
                y0: ys[0],
                x1: 400.0,
                y1: ys[fields.len()],
            },
        };
        let mut glyphs = Vec::new();
        for (r, (label, value)) in fields.iter().enumerate() {
            for (x, text) in [(5.0, label), (105.0, value)] {
                glyphs.extend(text.chars().enumerate().map(|(i, c)| {
                    let mut gl = gly(x + 5.0 * i as f64, ys[r] + 15.0, 5.0, &c.to_string());
                    gl.weight = if r == 0 { head } else { weight };
                    gl
                }));
            }
        }
        let t = fields
            .iter()
            .map(|(a, b)| vec![a.to_string(), b.to_string()])
            .collect();
        let rules = ys.iter().map(|&y| h(0.0, 400.0, y)).collect();
        (g, t, glyphs, rules)
    }

    const FIELDS: &[(&str, &str)] = &[
        ("Pipetting type", "Positive displacement dispenser"),
        ("Channel options", "1"),
        ("Volume range", "1 µL – 50,000 µL"),
        ("Operation", "Electronic"),
    ];

    #[test]
    fn a_ruled_list_of_fields_has_no_header_row() {
        let (g, t, glyphs, rules) = field_list(FIELDS, None, None);
        assert_eq!(g.header_rows(&t, &glyphs, &rules, &[]), 0);
        // Set the same, weights declared or not.
        let (g, t, glyphs, rules) = field_list(FIELDS, Some(400), Some(400));
        assert_eq!(g.header_rows(&t, &glyphs, &rules, &[]), 0);
    }

    #[test]
    fn a_first_row_set_apart_is_a_header_over_fields() {
        // Bold over plain.
        let (g, t, glyphs, rules) = field_list(FIELDS, Some(700), Some(400));
        assert_eq!(g.header_rows(&t, &glyphs, &rules, &[]), 1);
        // Ruled off more heavily than the rows under it.
        let (g, t, glyphs, mut rules) = field_list(FIELDS, None, None);
        rules[1].bbox.y1 = rules[1].bbox.y0 + 1.5;
        assert_eq!(g.header_rows(&t, &glyphs, &rules, &[]), 1);
        // The only row ruled off from the next.
        let (g, t, glyphs, mut rules) = field_list(FIELDS, None, None);
        rules.drain(2..4);
        assert_eq!(g.header_rows(&t, &glyphs, &rules, &[]), 1);
        // Shaded over bare rows.
        let (g, t, glyphs, rules) = field_list(FIELDS, None, None);
        let band = [fill(0.0, 100.0, 400.0, 120.0)];
        assert_eq!(g.header_rows(&t, &glyphs, &rules, &band), 1);
    }

    #[test]
    fn a_plain_header_names_its_column_in_fewer_words_than_its_values() {
        let (g, t, glyphs, rules) = field_list(
            &[
                ("Settings", "Recommendations"),
                ("General", "Change the pump frit as specified"),
                ("Mobile phase", "Use a clean solvent reservoir"),
            ],
            None,
            None,
        );
        assert_eq!(g.header_rows(&t, &glyphs, &rules, &[]), 1);
    }

    /// A grid of `rows`, 20pt each, columns at `xs`: each cell's text set
    /// in its weight, a rule under every row across the grid, and the
    /// vertical `rules` given.
    fn weighted_grid(
        xs: &[f64],
        rows: &[&[(&str, Option<u32>)]],
        verticals: Vec<Rule>,
    ) -> (Grid, Vec<Vec<String>>, Vec<crate::glyph::Glyph>, Vec<Rule>) {
        let ys: Vec<f64> = (0..=rows.len()).map(|i| 100.0 + 20.0 * i as f64).collect();
        let (left, right) = (xs[0], xs[xs.len() - 1]);
        let g = Grid {
            layout: None,
            page: 0,
            xs: xs.to_vec(),
            ys: ys.clone(),
            bbox: BBox {
                x0: left,
                y0: ys[0],
                x1: right,
                y1: ys[rows.len()],
            },
        };
        let mut glyphs = Vec::new();
        for (r, cells) in rows.iter().enumerate() {
            for (c, (text, weight)) in cells.iter().enumerate() {
                glyphs.extend(text.chars().enumerate().map(|(i, ch)| {
                    let mut gl = gly(
                        xs[c] + 5.0 + 5.0 * i as f64,
                        ys[r] + 15.0,
                        5.0,
                        &ch.to_string(),
                    );
                    gl.weight = *weight;
                    gl
                }));
            }
        }
        let t = rows
            .iter()
            .map(|cells| cells.iter().map(|(t, _)| t.to_string()).collect())
            .collect();
        let mut rules: Vec<Rule> = ys.iter().map(|&y| h(left, right, y)).collect();
        rules.extend(verticals);
        (g, t, glyphs, rules)
    }

    const BOLD: Option<u32> = Some(700);
    const PLAIN: Option<u32> = Some(400);

    #[test]
    fn a_first_row_set_as_the_fields_under_it_is_no_header() {
        // A parameter list: the first fields' labels bold, their values
        // plain, the rest plain throughout.
        let (g, t, glyphs, rules) = weighted_grid(
            &[0.0, 150.0, 400.0],
            &[
                &[("Instrument", BOLD), ("GC 2010 Plus", PLAIN)],
                &[("Detector", BOLD), ("MS TQ8040", PLAIN)],
                &[("Carrier Gas", PLAIN), ("Helium", PLAIN)],
                &[("Flow Rate", PLAIN), ("1.0 mL/min", PLAIN)],
            ],
            Vec::new(),
        );
        assert_eq!(g.header_rows(&t, &glyphs, &rules, &[]), 0);
        // Alone in that setting, the first row is set apart: a header.
        let (g, t, glyphs, rules) = weighted_grid(
            &[0.0, 150.0, 400.0],
            &[
                &[("Parameter", BOLD), ("Setting", PLAIN)],
                &[("Carrier Gas", PLAIN), ("Helium", PLAIN)],
                &[("Flow Rate", PLAIN), ("1.0 mL/min", PLAIN)],
            ],
            Vec::new(),
        );
        assert_eq!(g.header_rows(&t, &glyphs, &rules, &[]), 1);
        // Bold across, over bold labels beside plain values: a header.
        let (g, t, glyphs, rules) = weighted_grid(
            &[0.0, 150.0, 400.0],
            &[
                &[("Parameter", BOLD), ("Setting", BOLD)],
                &[("Detector", BOLD), ("MS TQ8040", PLAIN)],
                &[("Carrier Gas", PLAIN), ("Helium", PLAIN)],
            ],
            Vec::new(),
        );
        assert!(!g.opens_as_field(&t, &weights_of(&g, &glyphs), &rules, &[]));
    }

    fn weights_of(g: &Grid, glyphs: &[crate::glyph::Glyph]) -> Vec<Vec<(usize, usize)>> {
        (0..g.rows()).map(|r| g.cell_weights(glyphs, r)).collect()
    }

    #[test]
    fn a_bold_label_alone_over_plain_fields_opens_a_section() {
        let fields: &[&[(&str, Option<u32>)]] = &[
            &[("Instrument", BOLD), ("GC 2010 Plus", PLAIN)],
            &[("Conditions", BOLD), ("", None)],
            &[("Carrier Gas", PLAIN), ("Helium", PLAIN)],
            &[("Flow Rate", PLAIN), ("1.0 mL/min", PLAIN)],
        ];
        let (g, t, glyphs, _) = weighted_grid(&[0.0, 150.0, 400.0], fields, Vec::new());
        assert_eq!(g.bold_section_rows(&t, &glyphs, 0), vec![1]);
        // A bold label beside a value is a field: `Instrument` opens none.
        // Set plain, a label with no value is a field left blank.
        let plain: &[&[(&str, Option<u32>)]] = &[
            &[("Conditions", PLAIN), ("", None)],
            &[("Carrier Gas", PLAIN), ("Helium", PLAIN)],
            &[("Flow Rate", PLAIN), ("1.0 mL/min", PLAIN)],
        ];
        let (g, t, glyphs, _) = weighted_grid(&[0.0, 150.0, 400.0], plain, Vec::new());
        assert!(g.bold_section_rows(&t, &glyphs, 0).is_empty());
        // In a list whose labels are all bold, a blank value opens nothing.
        let all_bold: &[&[(&str, Option<u32>)]] = &[
            &[("Conditions", BOLD), ("", None)],
            &[("Carrier Gas", BOLD), ("Helium", PLAIN)],
            &[("Flow Rate", BOLD), ("1.0 mL/min", PLAIN)],
        ];
        let (g, t, glyphs, _) = weighted_grid(&[0.0, 150.0, 400.0], all_bold, Vec::new());
        assert!(g.bold_section_rows(&t, &glyphs, 0).is_empty());
        // A note set bold is a sentence, and a line run on across the cells
        // runs long: no heading.
        for note in [
            "Steps 7 to 9 are only needed for a coil.",
            "Accuracy technique atomic absorption flame analyte lead ashing acid",
        ] {
            let rows: &[&[(&str, Option<u32>)]] = &[
                &[(note, BOLD), ("", None)],
                &[("Carrier Gas", PLAIN), ("Helium", PLAIN)],
            ];
            let (g, t, glyphs, _) = weighted_grid(&[0.0, 150.0, 400.0], rows, Vec::new());
            assert!(g.bold_section_rows(&t, &glyphs, 0).is_empty(), "{note}");
        }
        // Under a header that leaves its stub empty, the label is its head.
        let headed: &[&[(&str, Option<u32>)]] = &[
            &[("", None), ("Organizer", BOLD)],
            &[("Conference", BOLD), ("", None)],
            &[("Annual Meeting", PLAIN), ("A society", PLAIN)],
        ];
        let (g, t, glyphs, _) = weighted_grid(&[0.0, 150.0, 400.0], headed, Vec::new());
        assert!(g.bold_section_rows(&t, &glyphs, 1).is_empty());
        assert_eq!(g.bold_section_rows(&t, &glyphs, 0), vec![1]);
    }

    #[test]
    fn a_first_field_s_value_spans_the_columns_a_table_nested_under_it_rules() {
        // The value column parted into three under `Gradient` alone: the
        // first row's value runs across all three.
        let xs = [0.0, 100.0, 200.0, 300.0, 400.0];
        let rows: &[&[(&str, Option<u32>)]] = &[
            &[
                ("Column", BOLD),
                ("C18 2.7", PLAIN),
                ("µm 50 x", PLAIN),
                ("3.0 mm", PLAIN),
            ],
            &[
                ("Flow Rate", BOLD),
                ("0.5 mL/min", PLAIN),
                ("", None),
                ("", None),
            ],
            &[
                ("Gradient", BOLD),
                ("0", PLAIN),
                ("95", PLAIN),
                ("5", PLAIN),
            ],
            &[("", None), ("8.0", PLAIN), ("10", PLAIN), ("90", PLAIN)],
        ];
        let frame = vec![
            v(100.0, 180.0, 0.0),
            v(100.0, 180.0, 100.0),
            v(100.0, 180.0, 400.0),
        ];
        let nested = vec![v(140.0, 180.0, 200.0), v(140.0, 180.0, 300.0)];
        let (g, t, glyphs, rules) =
            weighted_grid(&xs, rows, frame.iter().chain(&nested).copied().collect());
        assert!(g.opens_as_field(&t, &weights_of(&g, &glyphs), &rules, &[]));
        assert_eq!(g.header_rows(&t, &glyphs, &rules, &[]), 0);
        // Parted through the first row too, its cells are values apart.
        let through = vec![v(100.0, 180.0, 200.0), v(100.0, 180.0, 300.0)];
        let (g, t, glyphs, rules) =
            weighted_grid(&xs, rows, frame.iter().chain(&through).copied().collect());
        assert!(!g.opens_as_field(&t, &weights_of(&g, &glyphs), &rules, &[]));
        // Parted from the second row on, the first is a banner over the
        // columns the second names.
        let banner: &[&[(&str, Option<u32>)]] = &[
            &[
                ("Section A", BOLD),
                ("Payment Due", PLAIN),
                ("Dates", PLAIN),
                ("", None),
            ],
            &[
                ("", None),
                ("(a) 4/15", PLAIN),
                ("(b) 6/15", PLAIN),
                ("(c) 9/15", PLAIN),
            ],
            &[
                ("Line 16", BOLD),
                ("100", PLAIN),
                ("100", PLAIN),
                ("100", PLAIN),
            ],
            &[
                ("Line 17", BOLD),
                ("25", PLAIN),
                ("25", PLAIN),
                ("25", PLAIN),
            ],
        ];
        let under = vec![v(120.0, 180.0, 200.0), v(120.0, 180.0, 300.0)];
        let (g, t, glyphs, rules) =
            weighted_grid(&xs, banner, frame.iter().chain(&under).copied().collect());
        assert!(!g.opens_as_field(&t, &weights_of(&g, &glyphs), &rules, &[]));
    }

    #[test]
    fn a_first_row_over_figures_names_them() {
        // A bold label beside a plain value, as a bold total is set: the
        // words over the figures are their header.
        let (g, t, glyphs, rules) = weighted_grid(
            &[0.0, 150.0, 400.0],
            &[
                &[("Item", BOLD), ("Amount", PLAIN)],
                &[("Bolt", PLAIN), ("0.40", PLAIN)],
                &[("Nut", PLAIN), ("0.10", PLAIN)],
                &[("Total", BOLD), ("0.50", PLAIN)],
            ],
            Vec::new(),
        );
        assert_eq!(g.header_rows(&t, &glyphs, &rules, &[]), 1);
        // Set like the rows under it, ruled like them, the first row of a
        // label column over figures still names them.
        let (g, t, glyphs, rules) = field_list(
            &[
                ("Test series", "Peak area ratio %RSD (n = 6)"),
                ("Run 1", "0.94"),
                ("Run 2", "1.35"),
                ("Run 3", "0.86"),
                ("Criteria", "≤ 10"),
            ],
            None,
            None,
        );
        assert_eq!(g.header_rows(&t, &glyphs, &rules, &[]), 1);
        // A first value that is a value like the figures, a quantity or a
        // single word, leaves the list a list.
        for first in [
            ("Ion Source", "APCI"),
            ("Mean ± SD", "38.7 ± 9.89"),
            ("Time to Exit", "5 Years"),
        ] {
            let (g, t, glyphs, rules) = field_list(
                &[
                    first,
                    ("Gas Temp. (°C)", "325"),
                    ("Vaporizer (°C)", "350"),
                    ("Gas flow (L/min)", "5"),
                ],
                None,
                None,
            );
            assert_eq!(g.header_rows(&t, &glyphs, &rules, &[]), 0, "{first:?}");
        }
    }

    #[test]
    fn unruled_interiors_claim_no_merges() {
        // A bare bordered box: no interior horizontal rules, so "merged with
        // the row above" is unsupported by evidence and must not be claimed.
        let g = Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 150.0, 300.0],
            ys: vec![100.0, 130.0, 160.0],
            bbox: BBox {
                x0: 0.0,
                y0: 100.0,
                x1: 300.0,
                y1: 160.0,
            },
        };
        let rules = vec![
            h(0.0, 300.0, 100.0),
            h(0.0, 300.0, 160.0),
            v(100.0, 160.0, 0.0),
            v(100.0, 160.0, 150.0),
            v(100.0, 160.0, 300.0),
        ];
        let m = g.merges(&rules, &[]);
        assert!(m.continues_above.iter().all(|x| !x));
    }

    #[test]
    fn decoration_rules_above_the_box_are_trimmed() {
        // A bordered 2x3 grid from y=100..160, plus a page-decoration rule at
        // y=60 and a heading underline at y=80 — full width, no verticals up
        // there. The grid must start at 100, not 60.
        let mut rules = vec![
            h(0.0, 300.0, 60.0),
            h(0.0, 300.0, 80.0),
            h(0.0, 300.0, 100.0),
            h(0.0, 300.0, 130.0),
            h(0.0, 300.0, 160.0),
            v(100.0, 160.0, 0.0),
            v(100.0, 160.0, 150.0),
            v(100.0, 160.0, 300.0),
        ];
        rules.sort_by(|a, b| a.bbox.y0.partial_cmp(&b.bbox.y0).unwrap());
        let grids = detect(&rules, 0);
        assert_eq!(grids.len(), 1);
        assert!(
            (grids[0].ys[0] - 100.0).abs() < 1.0,
            "grid should start at the box top, got ys={:?}",
            grids[0].ys
        );
    }

    #[test]
    fn numeric_cell_classifier() {
        for n in ["6,785", "$ 7,835,559", "(1,304)", "5%", "-2.4", "10 250"] {
            assert!(is_numeric_cell(n), "{n} should be numeric");
        }
        for t in ["2023", "1999", "Q1 2024", "Revenue", "", "Note 3"] {
            assert!(!is_numeric_cell(t), "{t} should not be numeric");
        }
    }

    #[test]
    fn quantity_cells_open_on_a_number() {
        for n in [
            "17,000 xg",
            "0-30",
            "4' on Center",
            "10000 rpm",
            "$40",
            "≤6 in.",
            "6,785",
        ] {
            assert!(reads_as_quantity(n), "{n} should read as a quantity");
        }
        for t in [
            "2023",
            "M17 Series",
            "SFF (2.5”)",
            "- 4\" (102mm) Trim",
            "Zone",
            "",
        ] {
            assert!(!reads_as_quantity(t), "{t} should not read as a quantity");
        }
    }

    #[test]
    fn a_grid_of_numbered_boxes_is_a_form_without_a_header() {
        // A form's first box holds its title and box 1 beside its page box;
        // the boxes under it are numbered across the rows and columns.
        let g = plain_grid(vec![0.0, 100.0, 200.0, 300.0], vec![0.0, 20.0, 40.0, 60.0]);
        let rows = |r: &[[&str; 3]]| -> Vec<Vec<String>> {
            r.iter()
                .map(|r| r.iter().map(|t| t.to_string()).collect())
                .collect()
        };
        let form = rows(&[
            [
                "ORDER FOR SUPPLIES 1. REQUISITION NUMBER",
                "",
                "PAGE OF 1 34",
            ],
            [
                "2. CONTRACT NO. 47Q",
                "3. AWARD DATE",
                "4. ORDER NUMBER 8930",
            ],
            [
                "13a. THIS CONTRACT IS",
                "b. TELEPHONE NUMBER",
                "14. METHOD RFQ",
            ],
        ]);
        assert_eq!(g.header_rows(&form, &[], &[], &[]), 0);
        // A list numbered down its first column numbers its entries: its
        // first row still heads it.
        let list = rows(&[
            ["Section", "Pages", "Owner"],
            ["1. Introduction", "3", "Office A"],
            ["2. Scope", "4", "Office B"],
        ]);
        assert_eq!(g.header_rows(&list, &[], &[], &[]), 1);
        // Plain names over entries numbered across the columns head them.
        let principles = rows(&[
            ["Institutional", "Processes", "Output"],
            ["1. Independence", "7. Sound methods", "11. Relevance"],
            ["2. Mandate", "8. Procedures", "12. Accuracy"],
        ]);
        assert_eq!(g.header_rows(&principles, &[], &[], &[]), 1);
    }

    #[test]
    fn a_page_counter_and_a_numbered_field_are_read_as_such() {
        assert!(page_counter("PAGE OF PAGES 1 30"));
        assert!(page_counter("Page 2 of 7"));
        assert!(!page_counter("Page Size of the tray"));
        assert!(!page_counter("Copybooks, 88 page, pack of 10"));
        assert!(numbered_field("1. DATE OF ORDER"));
        assert!(numbered_field("12. F.O.B. POINT"));
        assert!(!numbered_field("1.5 mm"));
        assert!(!numbered_field("2. results were"));
        assert!(numbered_field("17a. CONTRACTOR/OFFEROR"));
        assert!(numbered_field("13b. RATING"));
        assert!(!numbered_field("3ab. Notes"));
    }

    /// A form's frame: a head band of its title over a notice beside a page
    /// box (`PAGE OF PAGES` over `1 30`), a rule across under them, then
    /// numbered fields ruled row by row. `counter` sets the page box.
    fn form(counter: bool) -> (Grid, Vec<crate::glyph::Glyph>, Vec<Rule>) {
        let g = plain_grid(
            vec![20.0, 300.0, 500.0, 596.0],
            vec![13.0, 28.0, 40.0, 60.0, 80.0, 100.0],
        );
        let mut glyphs = vec![
            gly(160.0, 26.0, 120.0, "ORDER FOR SUPPLIES OR SERVICES"),
            gly(22.0, 38.0, 200.0, "IMPORTANT: Mark all packages"),
            gly(22.0, 55.0, 60.0, "1. DATE OF ORDER"),
            gly(302.0, 55.0, 60.0, "2. CONTRACT NO."),
            gly(22.0, 75.0, 60.0, "3. ORDER NO."),
            gly(22.0, 95.0, 60.0, "5. ISSUING OFFICE"),
        ];
        if counter {
            glyphs.push(gly(505.0, 24.0, 60.0, "PAGE OF PAGES"));
            glyphs.push(gly(505.0, 37.0, 20.0, "1 30"));
        }
        let rules = vec![
            h(19.0, 597.0, 13.0),
            h(19.0, 499.0, 28.0),
            h(19.0, 597.0, 40.0),
        ];
        (g, glyphs, rules)
    }

    #[test]
    fn a_form_s_page_head_is_cut_from_its_grid() {
        let (g, glyphs, rules) = form(true);
        let g = cut_page_head(g, &rules, &glyphs);
        assert_eq!(g.ys, vec![40.0, 60.0, 80.0, 100.0]);
        // Its fields are numbered: no header.
        let rows: Vec<Vec<String>> = g
            .cell_texts(&glyphs)
            .chunks(g.cols())
            .map(|r| r.to_vec())
            .collect();
        assert_eq!(g.header_rows(&rows, &glyphs, &rules, &[]), 0);
        // With no page counter in it, the band is the grid's.
        let (g, glyphs, rules) = form(false);
        let ys = g.ys.clone();
        assert_eq!(cut_page_head(g, &rules, &glyphs).ys, ys);
    }

    /// A receipt's grid as the page's rules read it: two rows over a filled
    /// box (the page's fields, then stay details), the box's header in the
    /// second row with them, its rows ruled inside it, and a note under it.
    /// `over` sets the stay details over the box. The page's rules at the
    /// grid's top two edges run from `x0` to `x1`.
    fn receipt(
        over: bool,
        x0: f64,
        x1: f64,
    ) -> (
        Grid,
        Vec<crate::glyph::Glyph>,
        Vec<crate::rule::Fill>,
        Vec<Rule>,
    ) {
        let g = plain_grid(
            vec![42.0, 297.0, 470.0, 552.0],
            vec![68.0, 96.0, 216.0, 253.0, 290.0, 327.0, 367.0, 476.0],
        );
        let mut glyphs = vec![
            gly(50.0, 88.0, 60.0, "Guest"),
            gly(50.0, 200.0, 60.0, "DESCRIPTION"),
            gly(400.0, 200.0, 30.0, "RATE"),
            gly(480.0, 200.0, 40.0, "AMOUNT"),
            gly(50.0, 240.0, 60.0, "Room"),
            gly(50.0, 278.0, 60.0, "Grand"),
            gly(50.0, 315.0, 60.0, "Payment"),
            gly(50.0, 352.0, 60.0, "Balance"),
            gly(50.0, 420.0, 60.0, "Note"),
        ];
        if over {
            glyphs.push(gly(50.0, 120.0, 60.0, "Hotel"));
        }
        let rules = vec![h(x0, x1, 67.7), h(x0, x1, 95.7)];
        (g, glyphs, vec![fill(41.0, 179.0, 553.0, 368.0)], rules)
    }

    #[test]
    fn a_box_of_leadered_fields_is_not_cut_to() {
        // The box opens on a field run on with leaders, not on names.
        let (g, mut glyphs, fills, rules) = receipt(true, 28.0, 566.0);
        glyphs.retain(|gl| !matches!(gl.text.as_str(), "DESCRIPTION" | "RATE" | "AMOUNT"));
        glyphs.push(gly(50.0, 200.0, 200.0, "Wages . . . . . . . ."));
        glyphs.push(gly(480.0, 200.0, 40.0, "125,000"));
        let ys = g.ys.clone();
        assert_eq!(cut_to_box(g, &rules, &fills, &glyphs).ys, ys);
    }

    #[test]
    fn a_grid_run_over_a_boxed_table_is_cut_to_the_box() {
        let (g, glyphs, fills, rules) = receipt(true, 28.0, 566.0);
        let g = cut_to_box(g, &rules, &fills, &glyphs);
        assert_eq!(g.ys, vec![179.0, 216.0, 253.0, 290.0, 327.0, 367.0]);
        assert_eq!(g.cell_texts(&glyphs)[0], "DESCRIPTION");
        // Where nothing stands over the box in the row its edge runs
        // through, that row is the box's header set on the page: no cut.
        let (g, glyphs, fills, rules) = receipt(false, 28.0, 566.0);
        let ys = g.ys.clone();
        assert_eq!(cut_to_box(g, &rules, &fills, &glyphs).ys, ys);
        // Nor where the rules over the box stop within its width: they are
        // the table's own, a form's boxes ruled as its rows are.
        let (g, glyphs, fills, rules) = receipt(true, 42.0, 552.0);
        let ys = g.ys.clone();
        assert_eq!(cut_to_box(g, &rules, &fills, &glyphs).ys, ys);
    }

    #[test]
    fn a_line_of_fields_ruled_off_over_an_aligned_table_is_no_header() {
        let mut g = plain_grid(
            vec![40.0, 300.0, 400.0, 560.0],
            vec![70.0, 95.0, 115.0, 135.0],
        );
        let mut glyphs = vec![
            gly(42.0, 88.0, 70.0, "Guest Name: Avery"),
            gly(402.0, 88.0, 70.0, "Date: 31/12/2017"),
        ];
        for (k, t) in ["Hotel Details", "Check In", "Rooms"].iter().enumerate() {
            glyphs.push(gly([42.0, 302.0, 402.0][k], 110.0, 40.0, t));
        }
        for (k, t) in ["Resort", "31/12/2017", "1"].iter().enumerate() {
            glyphs.push(gly([42.0, 302.0, 402.0][k], 130.0, 40.0, t));
        }
        let rules = vec![h(30.0, 570.0, 96.0)];
        drop_field_line(&mut g, &glyphs, &rules);
        assert_eq!(g.rows(), 2);
        assert_eq!(g.cell_texts(&glyphs)[0], "Hotel Details");
        // Column names over the rule stay the header.
        let mut g = plain_grid(
            vec![40.0, 300.0, 400.0, 560.0],
            vec![70.0, 95.0, 115.0, 135.0],
        );
        glyphs[0] = gly(42.0, 88.0, 70.0, "Guest Name");
        glyphs[1] = gly(402.0, 88.0, 70.0, "Date");
        drop_field_line(&mut g, &glyphs, &rules);
        assert_eq!(g.rows(), 3);
    }

    /// An invoice's amounts under right-aligned heads wider than their
    /// figures, the grid's edges where the figures start; `over` sets a due
    /// date and its amount on a line over the heads, ruled off from them.
    fn amounts(over: bool) -> (Grid, Vec<crate::glyph::Glyph>, Vec<Rule>) {
        let mut ys = vec![370.0, 390.0, 410.0, 430.0];
        let mut glyphs = vec![
            gly(297.0, 384.0, 74.0, "Montant EUR HT"),
            gly(402.0, 384.0, 58.0, "Montant TVA"),
            gly(487.0, 384.0, 79.0, "Montant EUR TTC"),
            gly(40.0, 402.0, 160.0, "Abonnements"),
            gly(335.0, 402.0, 30.0, "24.99"),
            gly(431.0, 402.0, 22.0, "5.00"),
            gly(530.0, 402.0, 29.0, "29.99"),
            gly(40.0, 422.0, 84.0, "Communications"),
            gly(341.0, 422.0, 22.0, "0.00"),
            gly(431.0, 422.0, 22.0, "0.00"),
            gly(536.0, 422.0, 22.0, "0.00"),
        ];
        if over {
            ys.insert(0, 340.0);
            glyphs.push(gly(40.0, 348.0, 220.0, "Date limite de paiement"));
            glyphs.push(gly(488.0, 348.0, 37.0, "29.99"));
        }
        let g = plain_grid(vec![40.0, 336.0, 431.0, 531.0, 566.0], ys);
        (g, glyphs, vec![h(40.0, 555.0, 367.0)])
    }

    #[test]
    fn right_aligned_heads_keep_their_columns() {
        let (mut g, glyphs, _) = amounts(false);
        fit_right_aligned_heads(&mut g, &glyphs);
        let texts = g.cell_texts(&glyphs);
        assert_eq!(
            &texts[..4],
            &["", "Montant EUR HT", "Montant TVA", "Montant EUR TTC"],
            "{:?}",
            g.xs
        );
        assert_eq!(texts[5], "24.99");
    }

    #[test]
    fn a_line_with_a_figure_ruled_off_over_named_columns_leaves_the_grid() {
        let (mut g, glyphs, rules) = amounts(true);
        drop_field_line(&mut g, &glyphs, &rules);
        assert_eq!(g.rows(), 3);
        assert_eq!(g.ys[0], 367.3);
        // Not where no rule parts it from the heads.
        let (mut g, glyphs, _) = amounts(true);
        drop_field_line(&mut g, &glyphs, &[]);
        assert_eq!(g.rows(), 4);
    }

    /// A chart's table under `heads` (`(x, width, text)`), its figures right
    /// of where their bars would stand, the edges where the figures start.
    fn chart_table(heads: &[(f64, f64, &str)]) -> (Grid, Vec<crate::glyph::Glyph>) {
        let g = plain_grid(
            vec![202.0, 266.0, 331.0, 398.0, 465.0, 484.0],
            vec![174.0, 215.0, 233.0, 250.0],
        );
        let mut glyphs: Vec<crate::glyph::Glyph> =
            heads.iter().map(|&(x, w, t)| gly(x, 206.0, w, t)).collect();
        for (r, y) in [(0, 226.0), (1, 243.0)] {
            for (k, x) in [200.0, 265.0, 330.0, 397.0, 462.0].iter().enumerate() {
                glyphs.push(gly(
                    *x,
                    y,
                    13.0,
                    ["-0.8", "-4.3", "1.0", "-2.5", "0.9"][(k + r) % 5],
                ));
            }
        }
        (g, glyphs)
    }

    #[test]
    fn amount_units_stand_with_figures() {
        assert!(is_amount_unit("£m") && is_amount_unit("($000)") && is_amount_unit("€bn"));
        assert!(is_amount_unit("%"));
        assert!(!is_amount_unit("£") && !is_amount_unit("Sales") && !is_amount_unit("$m2"));
    }

    #[test]
    fn heads_over_their_figures_set_the_columns_edges() {
        // Each head centred over its bar and figure, so left of its column's
        // edge, set where the figures start.
        let (mut g, glyphs) = chart_table(&[
            (187.0, 21.0, "World"),
            (244.0, 33.0, "Low"),
            (311.0, 33.0, "Lower"),
            (378.0, 33.0, "Upper"),
            (445.0, 33.0, "High"),
        ]);
        fit_heads_to_figures(&mut g, &glyphs);
        let texts = g.cell_texts(&glyphs);
        assert_eq!(
            &texts[..5],
            &["World", "Low", "Lower", "Upper", "High"],
            "{:?}",
            g.xs
        );
        assert_eq!(&texts[5..10], &["-0.8", "-4.3", "1.0", "-2.5", "0.9"]);
        // Heads set within their columns' edges: nothing moves.
        let (mut g, glyphs) = chart_table(&[
            (203.0, 18.0, "World"),
            (267.0, 18.0, "Low"),
            (332.0, 18.0, "Lower"),
            (399.0, 18.0, "Upper"),
            (466.0, 17.0, "High"),
        ]);
        let xs = g.xs.clone();
        fit_heads_to_figures(&mut g, &glyphs);
        assert_eq!(g.xs, xs);
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_grid_is_not_read_across_a_drawings_box() {
        use crate::geom::BBox;
        use crate::rule::Fill;
        let bb = |x0: f64, y0: f64, x1: f64, y1: f64| BBox { x0, y0, x1, y1 };
        // A wiring diagram's box holding its switches, and a table's shaded
        // label column holding only text.
        let mut fills = vec![Fill {
            bbox: bb(46.0, 95.0, 421.0, 563.0),
            page: 0,
            paint: None,
            stroked: false,
        }];
        for k in 0..3 {
            let x = 115.0 + 100.0 * k as f64;
            fills.push(Fill {
                bbox: bb(x, 135.0, x + 29.0, 180.0),
                page: 0,
                paint: None,
                stroked: false,
            });
        }
        fills.push(Fill {
            bbox: bb(50.0, 600.0, 150.0, 700.0),
            page: 0,
            paint: None,
            stroked: false,
        });
        let walls = walls(&fills);
        assert_eq!(walls.len(), 2, "{walls:?}");
        // Wire labels inside the box and the note beside it, in one grid.
        assert!(walled_in(&bb(260.0, 140.0, 590.0, 160.0), &walls));
        // A grid inside the box, or one with the shaded column, is a grid.
        assert!(!walled_in(&bb(120.0, 140.0, 400.0, 160.0), &walls));
        assert!(!walled_in(&bb(50.0, 610.0, 400.0, 690.0), &walls));
    }
    use super::*;

    #[test]
    fn a_one_column_list_with_aligned_values_splits_into_two() {
        use crate::glyph::Glyph;
        // Labels at x=46, values at x=227, six rows 15pt apart, plus a
        // notice line spanning the width in row 0.
        let mut glyphs = Vec::new();
        let mut put = |text: &str, x: f64, y: f64| {
            let mut cx = x;
            for (i, ch) in text.chars().enumerate() {
                glyphs.push(Glyph {
                    text: ch.to_string(),
                    bbox: Some(BBox {
                        x0: cx,
                        y0: y - 7.0,
                        x1: cx + 4.0,
                        y1: y + 2.0,
                    }),
                    page: 0,
                    origin: (cx, y),
                    rotation_deg: 0.0,
                    font_size: 8.4,
                    weight: None,
                    italic: false,
                    advance: Some(4.5),
                    draw_index: i,
                    ocr: None,
                });
                cx += 4.5;
            }
        };
        put(
            "SYNTHETIC DATA — every figure on this page is fabricated for testing purposes only",
            50.0,
            230.0,
        );
        for (i, (k, v)) in [
            ("MS-DRG", "207 — Respiratory System"),
            ("MDC", "04 — Respiratory"),
            ("DRG Weight", "9.1742"),
            ("GMLOS", "18.5 days"),
            ("AMLOS", "23.7 days"),
            ("SOI", "4 (Extreme)"),
        ]
        .iter()
        .enumerate()
        {
            let y = 259.0 + 15.0 * i as f64;
            put(k, 46.0, y);
            put(v, 227.0, y);
        }
        let mut g = Grid {
            layout: None,
            page: 0,
            xs: vec![40.0, 572.0],
            ys: (0..8)
                .map(|i| 222.0 + 15.0 * i as f64 + if i > 0 { 33.0 } else { 0.0 })
                .collect(),
            bbox: BBox {
                x0: 40.0,
                y0: 222.0,
                x1: 572.0,
                y1: 360.0,
            },
        };
        // Row 0 is the tall notice band 222..255; rows 1.. are the list.
        g.ys = vec![222.0, 255.0, 270.0, 285.0, 300.0, 315.0, 330.0, 345.0];
        let spanning = g.split_by_alignment(&glyphs);
        assert_eq!(g.cols(), 2, "xs = {:?}", g.xs);
        assert!(g.xs[1] > 120.0 && g.xs[1] < 227.0, "cut at {}", g.xs[1]);
        assert_eq!(spanning, vec![0], "the notice row spans the cut");
        let cells = g.cell_texts(&glyphs);
        assert_eq!(cells[2], "MS-DRG");
        assert_eq!(cells[3], "207 — Respiratory System");
    }

    #[test]
    fn a_ragged_right_is_not_a_column() {
        use crate::glyph::Glyph;
        // Short labels only: the empty space right of them is a margin.
        let mut glyphs = Vec::new();
        for (i, k) in ["Alpha", "Beta", "Gamma", "Delta"].iter().enumerate() {
            let y = 259.0 + 15.0 * i as f64;
            let mut cx = 46.0;
            for (j, ch) in k.chars().enumerate() {
                glyphs.push(Glyph {
                    text: ch.to_string(),
                    bbox: Some(BBox {
                        x0: cx,
                        y0: y - 7.0,
                        x1: cx + 4.0,
                        y1: y + 2.0,
                    }),
                    page: 0,
                    origin: (cx, y),
                    rotation_deg: 0.0,
                    font_size: 8.4,
                    weight: None,
                    italic: false,
                    advance: Some(4.5),
                    draw_index: j,
                    ocr: None,
                });
                cx += 4.5;
            }
        }
        let mut g = Grid {
            layout: None,
            page: 0,
            xs: vec![40.0, 572.0],
            ys: vec![252.0, 267.0, 282.0, 297.0, 312.0],
            bbox: BBox {
                x0: 40.0,
                y0: 252.0,
                x1: 572.0,
                y1: 312.0,
            },
        };
        assert!(g.split_by_alignment(&glyphs).is_empty());
        assert_eq!(g.cols(), 1);
    }

    /// `text` at 10pt from `x` on baseline `base`, one glyph a word.
    fn set_at(x: f64, base: f64, text: &str) -> crate::glyph::Glyph {
        let w = 5.0 * text.chars().count() as f64;
        crate::glyph::Glyph {
            text: text.into(),
            bbox: Some(BBox {
                x0: x,
                y0: base - 8.0,
                x1: x + w,
                y1: base + 2.0,
            }),
            page: 0,
            origin: (x, base),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            italic: false,
            advance: Some(w),
            draw_index: 0,
            ocr: None,
        }
    }

    /// A statement's rules: across the table over and under its header and
    /// under its last row, and between them two rows of underlines, a
    /// piece under each of four columns of figures with 8pt between them.
    fn underlined_statement() -> Vec<Rule> {
        let mut rules = vec![
            h(50.0, 386.0, 40.0),
            h(50.0, 386.0, 60.0),
            h(50.0, 386.0, 140.0),
        ];
        for y in [103.0, 123.0] {
            for (a, b) in [
                (150.0, 204.0),
                (212.0, 266.0),
                (274.0, 328.0),
                (336.0, 386.0),
            ] {
                rules.push(h(a, b, y));
            }
        }
        rules
    }

    #[test]
    fn a_shaded_header_band_heads_the_table_read_under_it() {
        // A band of four shaded cells under a rule across them, divided by
        // verticals standing on that rule and ending at the band's foot,
        // its labels wrapped over two lines; under it a table read from its
        // alignment, ruled by nothing.
        let fill = |x0: f64, x1: f64| {
            crate::rule::Fill::new(
                BBox {
                    x0,
                    y0: 100.0,
                    x1,
                    y1: 130.0,
                },
                0,
            )
        };
        let fills = vec![
            fill(50.0, 200.0),
            fill(200.0, 290.0),
            fill(290.0, 380.0),
            fill(380.0, 470.0),
        ];
        let mut rules = vec![h(50.0, 470.0, 100.0)];
        for x in [200.0, 290.0, 380.0] {
            rules.push(v(100.0, 130.0, x));
        }
        let bands = header_bands(&rules, &fills);
        assert_eq!(bands.len(), 1, "{bands:?}");
        assert_eq!(bands[0].cells.len(), 4);
        // Shading with no divider standing on its rule, or dividers with no
        // rule across the top, is no band.
        assert!(header_bands(&rules[..1], &fills).is_empty());
        assert!(header_bands(&rules[1..], &fills).is_empty());

        let mut glyphs = vec![
            set_at(55.0, 112.0, "Item"),
            set_at(210.0, 112.0, "Outstanding"),
            set_at(210.0, 124.0, "(billions)"),
            set_at(300.0, 112.0, "Growth"),
            set_at(300.0, 124.0, "(percent)"),
            set_at(390.0, 112.0, "Average"),
            set_at(390.0, 124.0, "(percent)"),
        ];
        for (i, label) in ["Equities", "Farmland", "Bonds"].into_iter().enumerate() {
            let base = 145.0 + 12.0 * i as f64;
            glyphs.push(set_at(55.0, base, label));
            for x in [230.0, 320.0, 410.0] {
                glyphs.push(set_at(x, base, "1.5"));
            }
        }
        // The alignment's reading: its own column edges, its first row
        // under the band.
        let mut grids = vec![Grid {
            layout: None,
            page: 0,
            xs: vec![55.0, 210.0, 305.0, 395.0, 440.0],
            ys: vec![135.0, 147.5, 159.5, 171.5],
            bbox: BBox {
                x0: 55.0,
                y0: 135.0,
                x1: 440.0,
                y1: 171.5,
            },
        }];
        let keep = take_header_bands(&mut grids, &bands, &glyphs);
        assert_eq!(keep, vec![true]);
        let g = &grids[0];
        assert_eq!(g.rows(), 4, "{:?}", g.ys);
        // The band's divisions are the table's columns.
        for (x, want) in g.xs.iter().zip([50.0, 200.0, 290.0, 380.0, 470.0]) {
            assert!((x - want).abs() < 0.5, "{:?}", g.xs);
        }
        let texts = g.cell_texts(&glyphs);
        assert_eq!(
            &texts[..4],
            &[
                "Item",
                "Outstanding (billions)",
                "Growth (percent)",
                "Average (percent)"
            ]
        );
        assert_eq!(&texts[4..8], &["Equities", "1.5", "1.5", "1.5"]);
    }

    #[test]
    fn rules_of_two_tables_side_by_side_part_at_no_gutters_together() {
        // Two tables either side of the page's gutter, each row ruled a cell
        // at a time: two gutters in each table's rule, a gap wider than a
        // gutter between the tables. No row is underlined in four columns.
        let pieces = [
            (50.0, 193.0),
            (198.0, 241.0),
            (246.0, 289.0),
            (306.0, 473.0),
            (478.0, 509.0),
            (514.0, 544.0),
        ];
        let rules: Vec<Rule> = pieces.iter().map(|&(a, b)| h(a, b, 377.7)).collect();
        let refs: Vec<&Rule> = rules.iter().collect();
        let lines = cluster(&refs);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].gutters.is_empty(), "{:?}", lines[0].gutters);
        // One table's underlines, four pieces in one chain: three gutters.
        let one: Vec<Rule> = [
            (170.0, 225.0),
            (230.0, 285.0),
            (290.0, 345.0),
            (350.0, 405.0),
        ]
        .iter()
        .map(|&(a, b)| h(a, b, 80.0))
        .collect();
        let refs: Vec<&Rule> = one.iter().collect();
        assert_eq!(cluster(&refs)[0].gutters.len(), 3);
    }

    #[test]
    fn underlines_under_figures_mark_their_columns() {
        // Two rows of underlines among five rules across the table: their
        // pieces' ends have too little of the table's rules behind them to
        // be columns, but the gutters they part at recur, row after row.
        let rules = underlined_statement();
        let mut glyphs = vec![
            set_at(180.0, 55.0, "2023"),
            set_at(242.0, 55.0, "2022"),
            set_at(304.0, 55.0, "2021"),
            set_at(362.0, 55.0, "2020"),
        ];
        for (base, label) in [
            (75.0, "Cash"),
            (87.0, "Receivables"),
            (99.0, "Inventory"),
            (119.0, "Total"),
            (135.0, "Other"),
        ] {
            glyphs.push(set_at(55.0, base, label));
            for x in [175.0, 237.0, 299.0, 357.0] {
                glyphs.push(set_at(x, base, "1,234"));
            }
        }
        let grids = detect_framed(&rules, &[], &glyphs, 0, &[]);
        assert_eq!(grids.len(), 1, "{grids:?}");
        let xs = &grids[0].xs;
        assert_eq!(xs.len(), 6, "a labels' column and four of figures: {xs:?}");
        for (got, want) in xs.iter().zip([50.0, 150.0, 208.0, 270.0, 332.0, 386.0]) {
            assert!((got - want).abs() <= 1.0, "{xs:?}");
        }
    }

    #[test]
    fn a_rules_gutters_under_running_text_are_no_columns() {
        // The same rules, a line of prose set on the first row of
        // underlines: its words run on across every place they part.
        let rules = underlined_statement();
        let mut glyphs = Vec::new();
        let mut x = 52.0;
        for w in "the gains and losses on these holdings are reported in the notes to the statements that follow".split(' ') {
            glyphs.push(set_at(x, 100.0, w));
            x += 5.0 * w.chars().count() as f64 + 3.0;
        }
        assert!(x > 386.0, "the line runs across the table");
        let grids = detect_framed(&rules, &[], &glyphs, 0, &[]);
        assert!(
            grids.iter().all(|g| !g
                .xs
                .iter()
                .any(|&x| (x - 208.0).abs() <= 3.0 || (x - 270.0).abs() <= 3.0)),
            "{grids:?}"
        );
    }

    fn h(x0: f64, x1: f64, y: f64) -> Rule {
        Rule {
            bbox: BBox {
                x0,
                y0: y,
                x1,
                y1: y + 0.6,
            },
            orientation: Orientation::Horizontal,
            page: 0,
        }
    }
    fn v(y0: f64, y1: f64, x: f64) -> Rule {
        Rule {
            bbox: BBox {
                x0: x,
                y0,
                x1: x + 0.6,
                y1,
            },
            orientation: Orientation::Vertical,
            page: 0,
        }
    }

    /// A table grouped by rules, its rows drawn but not ruled: per group,
    /// a sample on each row, and a product and its formulation centred on
    /// the group, the formulation wrapped across the rows' cuts. `drawn`
    /// says whether the border is stroked row by row (breaking at each row)
    /// or in one piece per group.
    fn grouped_rows(drawn: bool) -> (Grid, Vec<crate::glyph::Glyph>, Vec<Rule>) {
        let xs = vec![72.0, 191.0, 340.0, 504.0];
        let ys = vec![162.0, 179.0, 196.0, 213.0, 230.0, 247.0, 264.0, 281.0];
        let mut glyphs = Vec::new();
        let mut put = |text: &str, x: f64, baseline: f64| {
            let mut cx = x;
            for ch in text.chars() {
                glyphs.push(crate::glyph::Glyph {
                    text: ch.to_string(),
                    bbox: Some(BBox {
                        x0: cx,
                        y0: baseline - 9.0,
                        x1: cx + 5.5,
                        y1: baseline + 3.0,
                    }),
                    page: 0,
                    origin: (cx, baseline),
                    rotation_deg: 0.0,
                    font_size: 12.0,
                    weight: None,
                    italic: false,
                    advance: Some(6.0),
                    draw_index: 0,
                    ocr: None,
                });
                cx += 6.0;
            }
        };
        put("Product", 100.0, 174.0);
        put("Formulation", 230.0, 174.0);
        put("Sample", 400.0, 174.0);
        // Group one: a single line centred on the group, and two lines,
        // the second set across the cut between the second and third rows.
        put("Alpha", 100.0, 207.5);
        put("10 mM acetate,", 200.0, 204.0);
        put("pH 5.0", 240.0, 218.0);
        put("Unstressed", 360.0, 192.0);
        put("Stirred", 360.0, 209.0);
        put("Lit", 360.0, 226.0);
        // Group two: the formulation's first two lines sit on the samples'
        // baselines, a row apart (1.42 of its size), the second carrying
        // on the first; its last is set a paragraph's leading below.
        put("Beta", 100.0, 258.5);
        put("12.5 mM histidine,", 200.0, 243.0);
        put("mMol histidine HCl,", 200.0, 260.0);
        put("pH 6.0", 240.0, 273.0);
        put("Unstressed", 360.0, 243.0);
        put("Stirred", 360.0, 260.0);
        put("Lit", 360.0, 277.0);
        let mut rules: Vec<Rule> = [162.0, 179.0, 230.0, 281.0]
            .iter()
            .map(|&y| h(72.0, 504.0, y))
            .collect();
        let spans: Vec<(f64, f64)> = if drawn {
            ys.windows(2).map(|w| (w[0], w[1])).collect()
        } else {
            vec![(162.0, 179.0), (179.0, 230.0), (230.0, 281.0)]
        };
        for (y0, y1) in spans {
            rules.push(v(y0, y1, 72.0));
            rules.push(v(y0, y1, 504.0));
        }
        let grid = Grid {
            page: 0,
            bbox: BBox {
                x0: 72.0,
                y0: 162.0,
                x1: 504.0,
                y1: 281.0,
            },
            xs,
            ys,
            layout: None,
        };
        (grid, glyphs, rules)
    }

    #[test]
    fn values_set_across_a_group_of_drawn_rows_span_it() {
        let (g, glyphs, rules) = grouped_rows(true);
        let mut m = g.merges(&rules, &[]);
        assert!(m.continues_above.iter().all(|x| !x), "nothing ruled open");
        g.text_merges(&glyphs, &rules, &mut m);
        let spans = |r: usize, c: usize| m.continues_above[r * 3 + c];
        for r in [2, 3, 5, 6] {
            assert!(
                spans(r, 0) && spans(r, 1),
                "row {r}: product and formulation span"
            );
            assert!(!spans(r, 2), "row {r}: each sample is its own");
        }
        for r in [1, 4] {
            assert!((0..3).all(|c| !spans(r, c)), "row {r} opens its group");
        }
        // The formulation reads whole, its carried-on line included.
        assert_eq!(
            g.region_text(&glyphs, &m, 4..7, 1..2).as_deref(),
            Some("12.5 mM histidine, mMol histidine HCl, pH 6.0")
        );
    }

    #[test]
    fn rows_the_border_runs_through_are_not_a_group() {
        // The same text, but the border stroked once per group: the grid's
        // cuts inside a group are not rows anyone drew, and the samples'
        // lines are one cell's wrapped text for all the page says.
        let (g, glyphs, rules) = grouped_rows(false);
        let mut m = g.merges(&rules, &[]);
        g.text_merges(&glyphs, &rules, &mut m);
        assert!(m.continues_above.iter().all(|x| !x));
    }

    #[test]
    fn a_boxed_cell_over_rows_ruled_in_pieces_spans_them() {
        // A boxed table: a label and its condition wrapped over a group of
        // two rows whose values are ruled apart a cell at a time, the
        // label's cells left open. The table's verticals box its cells, so
        // each of those is one cell spanning both rows.
        let xs = vec![72.0, 200.0, 300.0, 360.0, 420.0, 480.0, 540.0];
        let ys = vec![100.0, 120.0, 140.0, 160.0, 180.0];
        let mut glyphs = Vec::new();
        let mut put = |text: &str, x: f64, baseline: f64| {
            let mut cx = x;
            for ch in text.chars() {
                glyphs.push(crate::glyph::Glyph {
                    text: ch.to_string(),
                    bbox: (ch != ' ').then_some(BBox {
                        x0: cx,
                        y0: baseline - 7.0,
                        x1: cx + 4.5,
                        y1: baseline + 2.0,
                    }),
                    page: 0,
                    origin: (cx, baseline),
                    rotation_deg: 0.0,
                    font_size: 9.0,
                    weight: None,
                    italic: false,
                    advance: Some(5.0),
                    draw_index: 0,
                    ocr: None,
                });
                cx += 5.0;
            }
        };
        for (t, x) in [
            ("Parameter", 80.0),
            ("Condition", 205.0),
            ("Min", 305.0),
            ("Typ", 365.0),
            ("Max", 425.0),
            ("Unit", 485.0),
        ] {
            put(t, x, 114.0);
        }
        put("Operating", 80.0, 133.0);
        put("range", 80.0, 147.0);
        put("For temperatures", 205.0, 133.0);
        put("below zero", 205.0, 147.0);
        for (t, x) in [("-40", 305.0), ("25", 365.0), ("85", 425.0), ("C", 485.0)] {
            put(t, x, 133.0);
        }
        for (t, x) in [("0", 305.0), ("100", 425.0), ("RH", 485.0)] {
            put(t, x, 154.0);
        }
        put("Supply", 80.0, 174.0);
        put("1.8", 365.0, 174.0);
        put("uA", 485.0, 174.0);
        let mut rules: Vec<Rule> = [100.0, 120.0, 160.0, 180.0]
            .iter()
            .map(|&y| h(72.0, 540.0, y))
            .collect();
        for (x0, x1) in [
            (301.0, 359.0),
            (361.0, 419.0),
            (421.0, 479.0),
            (481.0, 539.0),
        ] {
            rules.push(h(x0, x1, 140.0));
        }
        for &x in &xs {
            rules.push(v(100.0, 180.0, x));
        }
        let g = Grid {
            page: 0,
            bbox: BBox {
                x0: 72.0,
                y0: 100.0,
                x1: 540.0,
                y1: 180.0,
            },
            xs,
            ys,
            layout: None,
        };
        let mut m = g.merges(&rules, &[]);
        g.text_merges(&glyphs, &rules, &mut m);
        let cols = 6;
        assert!(m.continues_above[2 * cols], "the label spans its rows");
        assert!(
            m.continues_above[2 * cols + 1],
            "the condition spans its rows"
        );
    }

    /// The segmented-rule shape: horizontal rules drawn as one segment per
    /// cell, no full-height verticals.
    fn segmented_table() -> Vec<Rule> {
        let xs = [68.0, 183.0, 297.0, 412.0, 528.0];
        let ys = [77.0, 203.0, 284.0, 366.0, 448.0, 530.0];
        let mut r = Vec::new();
        for y in ys {
            for w in xs.windows(2) {
                r.push(h(w[0], w[1], y));
            }
        }
        for w in ys.windows(2) {
            r.push(v(w[0], w[1], 68.5));
        }
        r
    }

    #[test]
    fn recovers_columns_from_segment_endpoints() {
        let g = detect(&segmented_table(), 0);
        assert_eq!(g.len(), 1);
        assert_eq!(
            g[0].cols(),
            4,
            "four columns from segment endpoints: {:?}",
            g[0].xs
        );
        assert_eq!(g[0].rows(), 5);
    }

    #[test]
    fn unrelated_page_rule_does_not_shrink_local_table_coverage() {
        let mut rules = vec![h(0.0, 600.0, 20.0)];
        for y in [200.0, 230.0, 260.0] {
            rules.push(h(50.0, 550.0, y));
        }
        for x in [50.0, 150.0, 300.0, 450.0, 550.0] {
            rules.push(v(200.0, 260.0, x));
        }
        let grids = detect(&rules, 0);
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].cols(), 4);
        assert_eq!(grids[0].rows(), 2);
    }

    #[test]
    fn sustained_ruled_stack_can_be_one_column() {
        let mut rules = Vec::new();
        for y in [100.0, 125.0, 150.0, 175.0, 200.0] {
            rules.push(h(50.0, 300.0, y));
        }
        rules.push(v(100.0, 200.0, 50.0));
        rules.push(v(100.0, 200.0, 300.0));
        let grids = detect(&rules, 0);
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].cols(), 1);
        assert_eq!(grids[0].rows(), 4);
    }

    #[test]
    fn numbered_table_captions_are_strict() {
        assert!(numbered_table_caption("Table 2. Contents"));
        assert!(numbered_table_caption(" table 12: Results"));
        assert!(!numbered_table_caption("Table of Contents"));
        assert!(!numbered_table_caption("Suitable for table use"));
    }

    #[test]
    fn repeated_fill_bands_form_logical_rows_and_columns() {
        let mut fills = vec![crate::rule::Fill {
            bbox: BBox {
                x0: 40.0,
                y0: 90.0,
                x1: 440.0,
                y1: 210.0,
            },
            page: 0,
            paint: None,
            stroked: false,
        }];
        for row in 0..5 {
            for col in 0..4 {
                fills.push(crate::rule::Fill {
                    bbox: BBox {
                        x0: 50.0 + col as f64 * 100.0,
                        y0: 100.0 + row as f64 * 20.0,
                        x1: 130.0 + col as f64 * 100.0,
                        y1: 110.0 + row as f64 * 20.0,
                    },
                    page: 0,
                    paint: None,
                    stroked: false,
                });
            }
        }
        let grids = detect_fill_bands(&fills, 0);
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].cols(), 4);
        assert_eq!(grids[0].rows(), 5);
    }

    #[test]
    fn cell_lookup_maps_a_point_to_row_and_column() {
        let g = &detect(&segmented_table(), 0)[0];
        assert_eq!(g.cell_at(100.0, 100.0), Some((0, 0)));
        assert_eq!(g.cell_at(450.0, 300.0), Some((2, 3)));
        assert_eq!(g.cell_at(10.0, 10.0), None, "outside the grid");
    }

    #[test]
    fn trims_rows_that_reach_past_the_table() {
        use crate::geom::BBox as B;
        use crate::glyph::Glyph;
        let mut r = segmented_table();
        // A footer separator far below the table.
        r.push(h(68.0, 528.0, 700.0));
        let mut g = detect(&r, 0).remove(0);
        let rows_before = g.rows();
        // Content only in the real table area.
        let glyphs: Vec<Glyph> = (0..5)
            .map(|i| Glyph {
                text: "x".into(),
                bbox: Some(B {
                    x0: 100.0,
                    y0: 100.0 + i as f64 * 80.0,
                    x1: 110.0,
                    y1: 110.0 + i as f64 * 80.0,
                }),
                page: 0,
                origin: (100.0, 110.0),
                rotation_deg: 0.0,
                font_size: 10.0,
                weight: None,
                italic: false,
                advance: None,
                draw_index: 0,
                ocr: None,
            })
            .collect();
        g.trim_to_content(&glyphs);
        assert!(
            g.rows() < rows_before,
            "empty trailing rows must be trimmed"
        );
    }

    #[test]
    fn a_tables_last_row_over_the_page_break_is_its_row() {
        let ruled = |ys: &[f64]| {
            let mut r: Vec<Rule> = ys.iter().map(|&y| h(68.0, 297.0, y)).collect();
            for w in ys.windows(2) {
                for x in [68.0, 183.0, 297.0] {
                    r.push(v(w[0], w[1], x));
                }
            }
            r
        };
        let tail = detect(&ruled(&[600.0, 620.0, 640.0, 660.0]), 0).remove(0);
        let mk = |x: f64, y: f64| crate::glyph::Glyph {
            text: "x".into(),
            bbox: Some(BBox {
                x0: x,
                y0: y,
                x1: x + 6.0,
                y1: y + 10.0,
            }),
            page: 1,
            origin: (x, y + 10.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            italic: false,
            advance: None,
            draw_index: 0,
            ocr: None,
        };
        // Only a folio under the table on its page.
        assert!(ends_page(&tail, &[mk(150.0, 760.0)]));
        assert!(!ends_page(
            &tail,
            &[mk(80.0, 680.0), mk(80.0, 700.0), mk(80.0, 720.0)]
        ));
        let row = vec![mk(80.0, 52.0), mk(200.0, 52.0)];
        let g = continued_row(&tail, &ruled(&[50.0, 66.0]), &row, &[], 1).expect("the table's row");
        assert_eq!(g.xs, tail.xs);
        assert_eq!(g.rows(), 1);
        // Two rows ruled are a table of their own.
        assert!(continued_row(&tail, &ruled(&[50.0, 66.0, 82.0]), &row, &[], 1).is_none());
        // A row whose column rule stands where the table has none is
        // another table's.
        let mut other = ruled(&[50.0, 66.0]);
        other.push(v(50.0, 66.0, 240.0));
        assert!(continued_row(&tail, &other, &row, &[], 1).is_none());
        // Text over the row: the page does not open on it.
        let mut over = row.clone();
        over.extend([mk(80.0, 10.0), mk(80.0, 22.0), mk(80.0, 34.0)]);
        assert!(continued_row(&tail, &ruled(&[50.0, 66.0]), &over, &[], 1).is_none());
    }

    #[test]
    fn a_tables_head_stranded_at_the_page_foot_heads_the_next_pages_table() {
        let glyph = |text: &str, x: f64, y: f64, weight: Option<u32>| crate::glyph::Glyph {
            text: text.into(),
            bbox: Some(BBox {
                x0: x,
                y0: y,
                x1: x + (6.0 * text.len() as f64).min(100.0),
                y1: y + 10.0,
            }),
            page: 0,
            origin: (x, y + 10.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight,
            italic: false,
            advance: None,
            draw_index: 0,
            ocr: None,
        };
        let mk = |text: &str, x: f64, y: f64| glyph(text, x, y, None);
        let bold = |text: &str, x: f64, y: f64| glyph(text, x, y, Some(700));
        let xs = vec![68.0, 183.0, 297.0, 411.0];
        let grid = |xs: &[f64], ys: &[f64]| Grid {
            page: 0,
            xs: xs.to_vec(),
            ys: ys.to_vec(),
            bbox: BBox {
                x0: xs[0],
                y0: ys[0],
                x1: xs[xs.len() - 1],
                y1: ys[ys.len() - 1],
            },
            layout: None,
        };
        // The arms ruled at the page's foot, a folio under them.
        let head = grid(&xs, &[720.0, 750.0]);
        let page = vec![
            bold("Reaction", 72.0, 728.0),
            bold("ArmA", 190.0, 728.0),
            bold("ArmB", 300.0, 728.0),
            mk("12", 300.0, 770.0),
        ];
        assert!(stranded_head(&head, 750.0, &page, &[]));
        // A row set as a body's rows are is no head.
        let plain = vec![
            mk("Reaction", 72.0, 728.0),
            mk("ArmA", 190.0, 728.0),
            mk("ArmB", 300.0, 728.0),
        ];
        assert!(!stranded_head(&head, 750.0, &plain, &[]));
        // A row of figures is a table's last row, not its head.
        let figures = vec![
            bold("Total", 72.0, 728.0),
            bold("56", 190.0, 728.0),
            bold("45", 300.0, 728.0),
        ];
        assert!(!stranded_head(&head, 750.0, &figures, &[]));
        // Captions boxed side by side are sentences, no column's names.
        let caption = "Figure 3 The artery lies in front of the vein. The image was taken a little below the ligament, and the artery has divided.";
        let captions = vec![bold(caption, 72.0, 728.0), bold(caption, 190.0, 728.0)];
        assert!(!stranded_head(&head, 750.0, &captions, &[]));
        // Ruled on under the row to an empty box above the folio: the
        // table ends on its page. Ruled on past the page's last line: the
        // break cut it.
        assert!(!stranded_head(&head, 765.0, &page, &[]));
        assert!(stranded_head(&head, 792.0, &page, &[]));
        // Prose under the row: the page goes on, and the row is no head.
        let mut prose = page.clone();
        prose.extend([
            mk("text", 72.0, 760.0),
            mk("text", 72.0, 774.0),
            mk("text", 72.0, 788.0),
        ]);
        assert!(!stranded_head(&head, 750.0, &prose, &[]));
        // The next page opens on the table, its columns drawn at the head's.
        let body = grid(&xs, &[50.0, 66.0, 82.0, 98.0]);
        let next = vec![mk("Nausea", 72.0, 52.0), mk("28", 190.0, 52.0)];
        assert!(opens_under(&head, &[body.clone()], &next, 3.0));
        // A table at other columns is another table.
        let other = grid(&[68.0, 240.0, 411.0], &[50.0, 66.0, 82.0]);
        assert!(!opens_under(&head, &[other], &next, 3.0));
        // Prose over the table: the next page does not open on it.
        let mut over = next.clone();
        over.extend([
            mk("text", 72.0, 10.0),
            mk("text", 72.0, 22.0),
            mk("text", 72.0, 34.0),
        ]);
        assert!(!opens_under(&head, &[body], &over, 3.0));
    }

    #[test]
    fn content_below_an_empty_row_is_not_part_of_the_table() {
        use crate::geom::BBox as B;
        use crate::glyph::Glyph;
        let mut r = segmented_table();
        r.push(h(68.0, 528.0, 700.0));
        let mut g = detect(&r, 0).remove(0);
        let mk = |y: f64| Glyph {
            text: "x".into(),
            bbox: Some(B {
                x0: 100.0,
                y0: y,
                x1: 110.0,
                y1: y + 10.0,
            }),
            page: 0,
            origin: (100.0, y + 10.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            italic: false,
            advance: None,
            draw_index: 0,
            ocr: None,
        };
        // Table content, then a footer well below the empty gap row.
        let mut glyphs: Vec<Glyph> = (0..5).map(|i| mk(100.0 + i as f64 * 80.0)).collect();
        glyphs.push(mk(650.0));
        g.trim_to_content(&glyphs);
        let cells = g.cell_texts(&glyphs);
        assert!(
            g.rows() <= 5,
            "footer must not become a table row, got {} rows",
            g.rows()
        );
        assert!(cells.iter().all(|c| !c.contains("footer")));
    }

    /// A two-column grid of five 20 pt rows from y = 100, text in rows 0, 1
    /// and 3, its column edges drawn down every row but the blank ones
    /// named in `bare`.
    fn spacer_table(bare: &[usize]) -> (Grid, Vec<Rule>, Vec<crate::glyph::Glyph>) {
        use crate::geom::BBox as B;
        let xs = [68.0, 183.0, 297.0];
        let ys = [100.0, 120.0, 140.0, 160.0, 180.0, 200.0];
        let mut r = Vec::new();
        for y in ys {
            r.push(h(68.0, 297.0, y));
        }
        for (i, w) in ys.windows(2).enumerate() {
            for (k, x) in xs.iter().enumerate() {
                let side = k == 0 || k + 1 == xs.len();
                if !bare.contains(&i) || side {
                    r.push(v(w[0], w[1], *x));
                }
            }
        }
        let g = detect(&r, 0).remove(0);
        let mk = |x: f64, y: f64| crate::glyph::Glyph {
            text: "x".into(),
            bbox: Some(B {
                x0: x,
                y0: y,
                x1: x + 6.0,
                y1: y + 10.0,
            }),
            page: 0,
            origin: (x, y + 10.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            italic: false,
            advance: None,
            draw_index: 0,
            ocr: None,
        };
        let glyphs = [0usize, 1, 3]
            .iter()
            .flat_map(|&row| {
                let y = ys[row] + 5.0;
                [mk(80.0, y), mk(200.0, y)]
            })
            .collect();
        (g, r, glyphs)
    }

    #[test]
    fn a_blank_row_ruled_as_the_tables_own_does_not_end_it() {
        let (mut g, rules, glyphs) = spacer_table(&[]);
        assert_eq!(g.rows(), 5);
        g.trim_to_content_ruled(&glyphs, &rules);
        assert_eq!(g.rows(), 4, "the row under the blank one is the table's");
        // The trailing blank row ends it: nothing with text follows.
        assert!((g.bbox.y1 - 180.0).abs() < 1.0, "ends at {}", g.bbox.y1);
    }

    #[test]
    fn a_blank_row_only_the_frame_runs_through_ends_the_table() {
        let (mut g, rules, glyphs) = spacer_table(&[2]);
        g.trim_to_content_ruled(&glyphs, &rules);
        assert_eq!(g.rows(), 2, "the column rule stops at the blank row");
        // Read without the page's rules, the table ends at its first blank
        // row, as it always has.
        let (mut g, _, glyphs) = spacer_table(&[]);
        g.trim_to_content(&glyphs);
        assert_eq!(g.rows(), 2);
    }

    #[test]
    fn a_legend_under_a_charts_gridlines_is_no_row_of_it() {
        // The gridlines run on through the blank band and the legend under
        // it, whose words run across a gridline.
        let (mut g, rules, mut glyphs) = spacer_table(&[]);
        glyphs.retain(|gl| gl.origin.1 < 160.0);
        for x0 in [176.0, 182.5, 189.0] {
            let mut gl = glyphs[0].clone();
            gl.bbox = Some(BBox {
                x0,
                y0: 165.0,
                x1: x0 + 6.0,
                y1: 175.0,
            });
            gl.origin = (x0, 175.0);
            glyphs.push(gl);
        }
        g.trim_to_content_ruled(&glyphs, &rules);
        assert_eq!(g.rows(), 2, "the legend is no row of the grid");
    }

    #[test]
    fn a_tables_bands_are_set_alike_and_its_rows_apart() {
        // Two columns, five 20 pt rows from y = 100, every edge drawn.
        let ys = [100.0, 120.0, 140.0, 160.0, 180.0, 200.0];
        let mut rules: Vec<Rule> = ys.iter().map(|&y| h(68.0, 297.0, y)).collect();
        for w in ys.windows(2) {
            for x in [68.0, 183.0, 297.0] {
                rules.push(v(w[0], w[1], x));
            }
        }
        let g = detect(&rules, 0).remove(0);
        assert_eq!(g.rows(), 5);
        let mk = |x: f64, y: f64, size: f32, weight: u32| crate::glyph::Glyph {
            text: "x".into(),
            bbox: Some(BBox {
                x0: x,
                y0: y,
                x1: x + 6.0,
                y1: y + 10.0,
            }),
            page: 0,
            origin: (x, y + 10.0),
            rotation_deg: 0.0,
            font_size: size,
            weight: Some(weight),
            italic: false,
            advance: None,
            draw_index: 0,
            ocr: None,
        };
        // Bands at rows 0 and 3, bold; a field at row 1, plain; a title in
        // a larger size at row 4.
        let glyphs = vec![
            mk(80.0, 105.0, 10.0, 700),
            mk(80.0, 125.0, 10.0, 400),
            mk(200.0, 125.0, 10.0, 400),
            mk(80.0, 165.0, 10.0, 700),
            mk(80.0, 185.0, 14.0, 700),
        ];
        assert!(g.rows_set_alike(&glyphs, &rules, &[], 0, 3));
        assert!(!g.rows_set_alike(&glyphs, &rules, &[], 0, 1), "weight");
        assert!(!g.rows_set_alike(&glyphs, &rules, &[], 3, 4), "size");
        // A row with no text is set no way.
        assert!(!g.rows_set_alike(&glyphs, &rules, &[], 0, 2));
    }

    #[test]
    fn a_border_rule_beside_a_cell_edge_makes_no_empty_column() {
        let mut r = segmented_table();
        // A left border 0.5pt inside the first column edge.
        for w in [77.0, 203.0, 284.0, 366.0, 448.0, 530.0].windows(2) {
            r.push(v(w[0], w[1], 68.0));
        }
        let g = detect(&r, 0);
        assert_eq!(g[0].cols(), 4, "no spurious empty column: {:?}", g[0].xs);
    }

    #[test]
    fn aligned_run_without_drawn_geometry_is_rejected() {
        use crate::geom::BBox as B;
        use crate::glyph::Glyph;
        let mk = |x: f64, y: f64| Glyph {
            text: "w".into(),
            bbox: Some(B {
                x0: x,
                y0: y,
                x1: x + 8.0,
                y1: y + 10.0,
            }),
            page: 0,
            origin: (x, y + 10.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            italic: false,
            advance: None,
            draw_index: 0,
            ocr: None,
        };
        // Five rows, four columns, gaps well above TABLE_CELL_GAP.
        let mut gl = Vec::new();
        for r in 0..5 {
            let y = r as f64 * 20.0;
            for c in 0..4 {
                let x = 50.0 + c as f64 * 60.0;
                gl.push(mk(x, y));
                gl.push(mk(x + 9.0, y));
            }
        }
        assert_eq!(detect_aligned_candidates(&gl, 0).len(), 1);
        assert!(
            detect_aligned(&gl, &[], &[], 0).is_empty(),
            "alignment alone is not a table"
        );
        let r = vec![h(50.0, 290.0, -5.0)];
        assert_eq!(
            detect_aligned(&gl, &r, &[], 0).len(),
            1,
            "a spanning rule corroborates it"
        );
    }

    #[test]
    fn a_header_is_set_apart_from_its_rows_past_a_band() {
        // A labour table's labels, bold, over a contract year's band, bold
        // too and its words in two cells, over plain rows of figures: the
        // labels head the rows, set apart from the first row of figures.
        let g = grid(
            &[0.0, 100.0, 200.0, 300.0, 400.0],
            &[0.0, 20.0, 40.0, 60.0, 80.0],
        );
        let rows: Vec<Vec<String>> = [
            ["Labor Category", "Hours", "Rate", "Amount"],
            ["MAS Contract Year 4", "(Base period)", "", ""],
            ["Expert 8", "0", "$407.51", "$0.00"],
            ["Expert 7", "14", "$378.34", "$5,297.00"],
        ]
        .iter()
        .map(|r| r.iter().map(|t| t.to_string()).collect())
        .collect();
        let mut gl = Vec::new();
        for (r, row) in rows.iter().enumerate() {
            let weight = Some(if r < 2 { 700 } else { 400 });
            for (c, t) in row.iter().enumerate() {
                if !t.is_empty() {
                    gl.push(word(c as f64 * 100.0 + 5.0, r, t, weight));
                }
            }
        }
        assert!(g.heads_its_rows(1, &rows, &gl, &[], &[]));
        // A first row set as the rows under it are, words over words in
        // one weight, earns nothing past the band either: a list's first
        // entry is no header.
        let names: Vec<Vec<String>> = [
            ["Alder", "Birch", "Cedar", "Dogwood"],
            ["Group one", "(native)", "", ""],
            ["Elm", "Fir", "Ginkgo", "Hazel"],
            ["Ironwood", "Juniper", "Larch", "Maple"],
        ]
        .iter()
        .map(|r| r.iter().map(|t| t.to_string()).collect())
        .collect();
        assert!(!g.heads_its_rows(1, &names, &[], &[], &[]));
    }

    #[test]
    fn a_field_grids_last_value_wrapped_under_it_is_its_own() {
        // The grid's last row's value wraps onto a line under it; the next
        // block starts a blank line further down.
        let pairs = |r: usize, cells: [&str; 4], gl: &mut Vec<crate::glyph::Glyph>| {
            for (k, t) in cells.iter().enumerate() {
                gl.push(word([0.0, 120.0, 300.0, 420.0][k], r, t, None));
            }
        };
        let mut gl = Vec::new();
        pairs(
            0,
            ["Record name:", "Oak Trust", "Record ID:", "R1001"],
            &mut gl,
        );
        pairs(
            1,
            ["Tax ID:", "XX-0001", "Source system:", "Brokerage"],
            &mut gl,
        );
        pairs(2, ["Address:", "40 PINE RD", "Account:", "A2002"], &mut gl);
        gl.push(word(120.0, 3, "RIVERTON, ST", None));
        let (rows, cells) = aligned_rows(&gl);
        let grids = aligned_runs(&rows, &cells, 0);
        assert_eq!(grids.len(), 1);
        let texts = grids[0].cell_texts(&gl);
        assert_eq!(texts[2 * 4 + 1], "40 PINE RD RIVERTON, ST");
        // A line a row's pitch and more under the grid is the next block's.
        let mut gl = Vec::new();
        pairs(
            0,
            ["Record name:", "Oak Trust", "Record ID:", "R1001"],
            &mut gl,
        );
        pairs(
            1,
            ["Tax ID:", "XX-0001", "Source system:", "Brokerage"],
            &mut gl,
        );
        pairs(2, ["Address:", "40 PINE RD", "Account:", "A2002"], &mut gl);
        gl.push(word(120.0, 5, "RIVERTON, ST", None));
        let (rows, cells) = aligned_rows(&gl);
        let texts = aligned_runs(&rows, &cells, 0)[0].cell_texts(&gl);
        assert_eq!(texts[2 * 4 + 1], "40 PINE RD");
        // A signature block: three fields a line apart, two more a blank
        // line apart, the last one's value wrapped. The wrapped line deepens
        // the last row only; the rows a line apart stay rows of their own.
        let mut gl = Vec::new();
        for (r, label, value) in [
            (0, "By:", "____________"),
            (1, "Name:", "Jordan Lee"),
            (2, "Title:", "Member"),
            (4, "Email:", "jl@example.com"),
            (6, "Address:", "12 Harbor Way"),
        ] {
            gl.push(word(0.0, r, label, None));
            gl.push(word(60.0, r, value, None));
        }
        gl.push(word(60.0, 7, "Portland ME 04101", None));
        let (rows, cells) = aligned_rows(&gl);
        let grids = aligned_runs(&rows, &cells, 0);
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].ys.len(), 6);
        let texts = grids[0].cell_texts(&gl);
        assert_eq!(texts[1], "____________");
        assert_eq!(texts[2 * 2], "Title:");
        assert_eq!(texts[4 * 2 + 1], "12 Harbor Way Portland ME 04101");
    }

    #[test]
    fn a_field_grid_goes_on_past_a_wrapped_value() {
        // Two label:value pairs a row; one value wraps onto a line of its
        // own under its first line; the grid goes on under it.
        let mut gl = Vec::new();
        for (r, cells) in [
            (0, ["Message ref:", "REF01", "Message type:", "Outgoing"]),
            (1, ["Amount:", "USD 10.00", "Value date:", "01/02/2030"]),
            (
                2,
                [
                    "Ordering account:",
                    "A1001",
                    "Ordering address:",
                    "12 OAK ST",
                ],
            ),
            (
                4,
                ["Beneficiary:", "J. DOE", "Beneficiary bank:", "FIRST BANK"],
            ),
        ] {
            for (k, t) in cells.iter().enumerate() {
                gl.push(word([0.0, 120.0, 300.0, 420.0][k], r, t, None));
            }
        }
        gl.push(word(420.0, 3, "SPRINGFIELD, ST", None));
        let (rows, cells) = aligned_rows(&gl);
        let grids = aligned_runs(&rows, &cells, 0);
        assert_eq!(grids.len(), 1, "one grid, not two around the wrap");
        let g = &grids[0];
        assert_eq!((g.rows(), g.cols()), (4, 4));
        let texts = g.cell_texts(&gl);
        assert_eq!(texts[2 * 4 + 3], "12 OAK ST SPRINGFIELD, ST");
        assert_eq!(texts[3 * 4 + 3], "FIRST BANK");
        // A line set where a value starts, under a row that is no field
        // grid's (a schedule's figures), ends the run as it did.
        let mut gl = Vec::new();
        for (r, cells) in [
            (0, ["0002", "1", "LN0001", "01 Feb 2023"]),
            (1, ["0003", "1", "LN0001", "01 Feb 2023"]),
            (3, ["0004", "1", "LN0001", "01 Feb 2023"]),
        ] {
            for (k, t) in cells.iter().enumerate() {
                gl.push(word([0.0, 120.0, 300.0, 420.0][k], r, t, None));
            }
        }
        gl.push(word(300.0, 2, "EXTENSION", None));
        let (rows, cells) = aligned_rows(&gl);
        assert!(aligned_runs(&rows, &cells, 0).iter().all(|g| g.rows() < 3));
    }

    #[test]
    fn a_cells_text_leaves_out_blank_glyphs_under_it() {
        // A date in a cell, glyph by glyph, and three spaces of the
        // description line under it the cell took in: read with them, the
        // spaces' gaps parted the date (`26.0 7.2012`).
        let ch = |x: f64, y: f64, t: &str, inked: bool| crate::glyph::Glyph {
            text: t.into(),
            bbox: inked.then_some(crate::geom::BBox {
                x0: x,
                y0: y - 8.6,
                x1: x + 6.0,
                y1: y,
            }),
            page: 0,
            origin: (x, y),
            rotation_deg: 0.0,
            font_size: if inked { 12.0 } else { 8.0 },
            weight: Some(400),
            italic: false,
            advance: Some(if t == "." { 3.33 } else { 6.67 }),
            draw_index: 0,
            ocr: None,
        };
        let mut glyphs = Vec::new();
        let mut x = 126.75;
        for c in "26.07.2012".chars() {
            let t = c.to_string();
            glyphs.push(ch(x, 579.5, &t, true));
            x += if c == '.' { 3.33 } else { 6.67 };
        }
        for bx in [140.14, 145.06, 168.17] {
            glyphs.push(ch(bx, 587.7, " ", false));
        }
        let layout = TableLayout {
            cells: vec![(0..glyphs.len()).collect()],
            ..TableLayout::default()
        };
        assert_eq!(layout.texts(&glyphs, 1), ["26.07.2012"]);
    }

    /// A word as one glyph run at `x`, 5pt a character, on row `r`.
    fn word(x: f64, r: usize, text: &str, weight: Option<u32>) -> crate::glyph::Glyph {
        let y = r as f64 * 20.0;
        let w = 5.0 * text.chars().count() as f64;
        crate::glyph::Glyph {
            text: text.into(),
            bbox: Some(crate::geom::BBox {
                x0: x,
                y0: y,
                x1: x + w,
                y1: y + 10.0,
            }),
            page: 0,
            origin: (x, y + 10.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight,
            italic: false,
            advance: Some(w),
            draw_index: 0,
            ocr: None,
        }
    }

    #[test]
    fn an_unruled_table_is_read_from_its_typeset_header() {
        // `border: none`: a bold header, left-aligned, over rows whose
        // figures are right-aligned — `Quantity` starts where no `3` does.
        let lines = |bold: Option<u32>, plain: Option<u32>| {
            let mut gl = vec![
                word(50.0, 0, "PLU", bold),
                word(110.0, 0, "Item", bold),
                word(200.0, 0, "Quantity", bold),
                word(265.0, 0, "Each", bold),
                word(315.0, 0, "Amount", bold),
            ];
            for (r, (code, item, qty, each, amount)) in [
                ("BK-210", "Almond croissant", "3", "3.35", "10.05"),
                ("KT-301", "Fish pie", "4", "12.60", "50.40"),
                ("KT-320", "Club sandwich", "2", "7.40", "14.80"),
            ]
            .into_iter()
            .enumerate()
            {
                let r = r + 1;
                gl.push(word(50.0, r, code, plain));
                gl.push(word(110.0, r, item, plain));
                gl.push(word(245.0 - 5.0 * qty.len() as f64, r, qty, plain));
                gl.push(word(300.0 - 5.0 * each.len() as f64, r, each, plain));
                gl.push(word(350.0 - 5.0 * amount.len() as f64, r, amount, plain));
            }
            gl
        };
        let gl = lines(Some(700), Some(400));
        let grids = detect_aligned(&gl, &[], &[], 0);
        assert_eq!(
            grids.len(),
            1,
            "a typeset header corroborates the alignment"
        );
        let g = &grids[0];
        assert_eq!(
            g.cols(),
            5,
            "the right-aligned column keeps its own heading"
        );
        let cells = g.cell_texts(&gl);
        assert_eq!(&cells[..5], ["PLU", "Item", "Quantity", "Each", "Amount"]);
        assert_eq!(cells[5 + 2], "3");
        // The same text with no weights is alignment alone.
        assert!(detect_aligned(&lines(None, None), &[], &[], 0).is_empty());
        // Bold all the way down is not a header over data.
        assert!(detect_aligned(&lines(Some(700), Some(700)), &[], &[], 0).is_empty());
        // Set just under a key/value block, the two align as one run that
        // is neither; each is found on its own, the two rows of bold label
        // and plain value a field list already.
        let mut gl: Vec<crate::glyph::Glyph> = lines(Some(700), Some(400))
            .into_iter()
            .map(|mut g| {
                if let Some(b) = g.bbox.as_mut() {
                    b.y0 += 40.0;
                    b.y1 += 40.0;
                }
                g.origin.1 += 40.0;
                g
            })
            .collect();
        for (r, (label, value)) in [("No.", "WB-66086"), ("Date", "11 August 2026")]
            .into_iter()
            .enumerate()
        {
            gl.push(word(50.0, r, label, Some(700)));
            gl.push(word(110.0, r, value, Some(400)));
        }
        let mut grids = detect_aligned(&gl, &[], &[], 0);
        grids.sort_by(|a, b| a.bbox.y0.total_cmp(&b.bbox.y0));
        assert_eq!(grids.len(), 2, "got {grids:?}");
        assert_eq!((grids[0].rows(), grids[0].cols()), (2, 2));
        assert_eq!(
            grids[0].cell_texts(&gl),
            ["No.", "WB-66086", "Date", "11 August 2026"]
        );
        assert_eq!((grids[1].rows(), grids[1].cols()), (4, 5));
    }

    #[test]
    fn an_unruled_field_list_is_read_from_its_bold_labels() {
        let mut gl = Vec::new();
        for (r, (label, value)) in [
            ("Purchased", "Cinnamon bun"),
            ("Quantity", "2"),
            ("Unit price", "2.35"),
            ("Grand total", "5.79"),
        ]
        .into_iter()
        .enumerate()
        {
            gl.push(word(50.0, r, label, Some(700)));
            gl.push(word(260.0 - 5.0 * value.len() as f64, r, value, Some(400)));
        }
        let grids = detect_aligned(&gl, &[], &[], 0);
        assert_eq!(grids.len(), 1);
        let rows: Vec<Vec<String>> = {
            let cells = grids[0].cell_texts(&gl);
            cells.chunks(2).map(|c| c.to_vec()).collect()
        };
        assert_eq!(
            grids[0].header_rows(&rows, &gl, &[], &[]),
            0,
            "a field is not a header"
        );
    }

    #[test]
    fn a_word_across_two_columns_is_not_cut_between_them() {
        // `Dimensions`, its letters touching, set across two of a grid's
        // column boundaries: dealt glyph by glyph it read `Di | mensio | ns`.
        let word_at = |x: f64, text: &str| -> Vec<crate::glyph::Glyph> {
            text.chars()
                .enumerate()
                .map(|(k, c)| word(x + 5.0 * k as f64, 0, &c.to_string(), None))
                .collect()
        };
        let grid = Grid {
            layout: None,
            page: 0,
            bbox: crate::geom::BBox {
                x0: 0.0,
                y0: -1.0,
                x1: 60.0,
                y1: 11.0,
            },
            xs: vec![0.0, 20.0, 40.0, 60.0],
            ys: vec![-1.0, 11.0],
        };
        let gl = word_at(5.0, "Dimensions");
        assert_eq!(grid.cell_texts(&gl), ["", "Dimensions", ""]);
    }

    #[test]
    fn a_rule_drawn_down_through_a_cell_parts_its_text() {
        // Two boxes side by side in one cell of a form, a rule between them
        // for their height, a line under both.
        let mut gl = vec![
            word(5.0, 0, "Ordered", None),
            word(45.0, 0, "on", None),
            word(60.0, 0, "behalf", None),
            word(160.0, 0, "LONDON", None),
            word(195.0, 0, "102", None),
            word(215.0, 0, "PETTY", None),
            word(5.0, 1, "State", None),
            word(35.0, 1, "for", None),
            word(55.0, 1, "Justice:", None),
            word(160.0, 1, "Petty", None),
            word(190.0, 1, "France", None),
            word(5.0, 2, "Tel:", None),
            word(30.0, 2, "Fax:", None),
            word(160.0, 2, "Email", None),
        ];
        for (i, g) in gl.iter_mut().enumerate() {
            g.draw_index = i;
        }
        let grid = Grid {
            layout: None,
            page: 0,
            bbox: crate::geom::BBox {
                x0: 0.0,
                y0: -1.0,
                x1: 300.0,
                y1: 55.0,
            },
            xs: vec![0.0, 300.0],
            ys: vec![-1.0, 55.0],
        };
        let rule = |x: f64, y0: f64, y1: f64| Rule {
            bbox: crate::geom::BBox {
                x0: x,
                y0,
                x1: x + 0.5,
                y1,
            },
            orientation: Orientation::Vertical,
            page: 0,
        };
        // Read as one, the boxes' lines take turns.
        let whole = grid.cell_texts(&gl)[0].clone();
        assert!(
            whole.starts_with("Ordered on behalf LONDON 102 PETTY"),
            "{whole}"
        );
        // Parted: each box whole, the left first, then the line under them.
        assert_eq!(
            grid.cell_texts_parted(&gl, &[rule(150.0, -1.0, 32.0)])[0],
            "Ordered on behalf State for Justice: LONDON 102 PETTY Petty France Tel: Fax: Email"
        );
        // Nor does a stroke drawn through a word: `behalf` runs 60 to 90.
        assert_eq!(
            grid.cell_texts_parted(&gl, &[rule(75.0, -1.0, 32.0)])[0],
            whole
        );
        // A small table set in the cell, its columns of values parted by a
        // rule, reads across its rows.
        let mut small = vec![
            word(5.0, 0, "DI1", None),
            word(160.0, 0, "Stop", None),
            word(5.0, 1, "0", None),
            word(160.0, 1, "Start", None),
        ];
        for (i, g) in small.iter_mut().enumerate() {
            g.draw_index = i;
        }
        assert_eq!(
            grid.cell_texts_parted(&small, &[rule(150.0, -1.0, 32.0)]),
            grid.cell_texts(&small)
        );
        // A rule on the cell's edge is its column's, and one with text on
        // one side of it only parts nothing.
        assert_eq!(
            grid.cell_texts_parted(&gl, &[rule(1.0, -1.0, 55.0)])[0],
            whole
        );
        assert_eq!(
            grid.cell_texts_parted(&gl, &[rule(250.0, -1.0, 55.0)])[0],
            whole
        );
    }

    #[test]
    fn a_word_across_the_grids_edge_is_in_the_cell_or_the_prose() {
        // Letters 5pt apart, one pen run: a word.
        let letters = |x: f64, text: &str| -> Vec<crate::glyph::Glyph> {
            text.chars()
                .enumerate()
                .map(|(k, c)| {
                    let mut g = word(x + 5.0 * k as f64, 0, &c.to_string(), None);
                    g.bbox.as_mut().unwrap().y0 = 100.0;
                    g.bbox.as_mut().unwrap().y1 = 110.0;
                    g.origin.1 = 110.0;
                    g
                })
                .collect()
        };
        let grid = Grid {
            layout: None,
            page: 0,
            bbox: crate::geom::BBox {
                x0: 100.0,
                y0: 90.0,
                x1: 300.0,
                y1: 140.0,
            },
            xs: vec![100.0, 200.0, 300.0],
            ys: vec![90.0, 140.0],
        };
        // `See` starts left of the edge: its centre of mass is outside, so
        // its last letter, centred inside, stays with the rest in the prose.
        let see = letters(88.0, "See");
        assert_eq!(grid.takes(&see), [false, false, false]);
        // `warranty` has its centre of mass inside: the cell reads it whole,
        // its first letters, centred outside, too.
        let w = letters(92.0, "warranty");
        assert!(grid.takes(&w).iter().all(|&t| t));
        assert_eq!(grid.cell_texts(&w)[0], "warranty");
        // Read across the row, as a title or a spanning row is, the word is
        // whole too: its first letters, taken, are read.
        assert_eq!(grid.row_text(&w, 0), "warranty");
        assert_eq!(grid.row_glyphs(&w, 0).len(), w.len());
        assert_eq!(grid.row_text(&see, 0), "");
    }

    /// A spec sheet's fields, labels and values in one face, every other
    /// value set `lift` points above its label.
    fn spec_fields(labels: &[&str], lift: f64) -> Vec<crate::glyph::Glyph> {
        let values = [
            "Polypropylene",
            "500 ml",
            "38 mm",
            "24 g",
            "24 per case",
            "Yes",
        ];
        let mut gl = Vec::new();
        for (r, label) in labels.iter().enumerate() {
            gl.push(word(50.0, r, label, Some(400)));
            let mut v = word(200.0, r, values[r % values.len()], Some(400));
            if r % 2 == 1 {
                v.origin.1 -= lift;
                let b = v.bbox.as_mut().unwrap();
                b.y0 -= lift;
                b.y1 -= lift;
            }
            gl.push(v);
        }
        gl
    }

    #[test]
    fn a_field_list_whose_labels_end_in_colons_is_read_in_any_weight() {
        let labels = [
            "Material:",
            "Capacity:",
            "Neck Diameter:",
            "Weight:",
            "Pack Size:",
            "Supplied with cap fitted:",
        ];
        // A value a hundredth of a point above its label sorts ahead of it
        // by baseline; read in that order the row was one cell.
        let gl = spec_fields(&labels, 0.02);
        let grids = detect_aligned(&gl, &[], &[], 0);
        assert_eq!(grids.len(), 1, "got {grids:?}");
        assert_eq!((grids[0].rows(), grids[0].cols()), (6, 2));
        let cells = grids[0].cell_texts(&gl);
        assert_eq!(&cells[4..6], ["Neck Diameter:", "38 mm"]);
        assert_eq!(&cells[10..12], ["Supplied with cap fitted:", "Yes"]);
        // Without their colons the same rows are alignment alone.
        let bare: Vec<&str> = labels.iter().map(|l| l.trim_end_matches(':')).collect();
        assert!(detect_aligned(&spec_fields(&bare, 0.02), &[], &[], 0).is_empty());
    }

    fn field_texts(g: &Grid, gl: &[crate::glyph::Glyph]) -> Vec<String> {
        g.layout
            .as_ref()
            .expect("a field list is read")
            .texts(gl, g.cols())
    }

    #[test]
    fn a_field_list_without_colons_is_read_with_its_values_set_right() {
        let mut gl = Vec::new();
        for (r, (label, value)) in [
            ("OTC", "Yes"),
            ("OTC Phase II", "Yes"),
            ("CARB SCM 2007", "Yes"),
            ("MIR-Manufacturer Inventory", "No"),
            ("MPI", "#142, 143"),
        ]
        .into_iter()
        .enumerate()
        {
            gl.push(word(50.0, r, label, Some(700)));
            gl.push(word(
                300.0 - 5.0 * value.chars().count() as f64,
                r,
                value,
                Some(400),
            ));
        }
        let lists = field_lists(&gl, 0, &crate::column::detect(&gl));
        assert_eq!(lists.len(), 1, "got {lists:?}");
        assert_eq!((lists[0].rows(), lists[0].cols()), (5, 2));
        assert_eq!(lists[0].layout.as_ref().unwrap().header_rows, 0);
        let cells = field_texts(&lists[0], &gl);
        assert_eq!(
            &cells[6..10],
            ["MIR-Manufacturer Inventory", "No", "MPI", "#142, 143"]
        );
    }

    #[test]
    fn a_field_list_is_read_beside_a_column_of_prose() {
        // Rows of the list share their baselines with a column of prose on
        // their left: across the page each row is three cells.
        let mut gl = Vec::new();
        for r in 0..10 {
            gl.push(word(50.0, r, "Prose set in its own column here", None));
        }
        // The list's own column runs on above it and below it.
        for r in [0, 1, 2, 7, 8, 9] {
            gl.push(word(260.0, r, "Finishes shown are samples", None));
        }
        for (r, (label, value)) in [
            ("CP", "Polished Chrome"),
            ("VS", "Vibrant Stainless"),
            ("BL", "Matte Black"),
            ("2MB", "Vibrant Brushed Brass"),
        ]
        .into_iter()
        .enumerate()
        {
            gl.push(word(260.0, r + 3, label, None));
            gl.push(word(290.0, r + 3, value, None));
        }
        let lists = field_lists(&gl, 0, &crate::column::detect(&gl));
        assert_eq!(lists.len(), 1, "got {lists:?}");
        let cells = field_texts(&lists[0], &gl);
        assert_eq!(
            &cells[..4],
            ["CP", "Polished Chrome", "VS", "Vibrant Stainless"]
        );
        assert_eq!(&cells[6..8], ["2MB", "Vibrant Brushed Brass"]);
    }

    #[test]
    fn a_table_in_one_column_of_several_is_read_whole() {
        // A paint sheet's tinting grid in the first of two page columns; each
        // of its rows shares a baseline with the second column's prose.
        let mut gl = Vec::new();
        let rows = [
            ("Base:", "oz. per gallon:", "Strength:"),
            ("Ultrawhite", "0-7", "SherColor"),
            ("Extra White", "0-7", "SherColor"),
            ("Deep Base", "4-14", "SherColor"),
            ("Ultradeep Base", "10-20", "SherColor"),
        ];
        for (r, (base, oz, strength)) in rows.into_iter().enumerate() {
            gl.push(word(40.0, r, base, None));
            // Centred over its column.
            gl.push(word(140.0 - 2.5 * oz.chars().count() as f64, r, oz, None));
            gl.push(word(190.0, r, strength, None));
        }
        // The grid's own column runs on under it.
        for r in 5..10 {
            gl.push(word(
                40.0,
                r,
                "Text under the grid, filling its column",
                None,
            ));
        }
        for r in 0..10 {
            gl.push(word(260.0, r, "Prose set in the next column", None));
        }
        let lists = column_tables(&gl, 0, &crate::column::detect(&gl));
        assert_eq!(lists.len(), 1, "got {lists:?}");
        assert_eq!((lists[0].rows(), lists[0].cols()), (5, 3));
        let lay = lists[0].layout.as_ref().unwrap();
        assert_eq!(lay.header_rows, 1);
        let cells = lay.texts(&gl, 3);
        assert_eq!(&cells[..3], ["Base:", "oz. per gallon:", "Strength:"]);
        assert_eq!(&cells[12..], ["Ultradeep Base", "10-20", "SherColor"]);
    }

    #[test]
    fn two_columns_of_short_lines_are_not_a_field_list() {
        // Both columns filled to their right edge, as prose fills them.
        let mut gl = Vec::new();
        for (r, (left, right)) in [
            ("Results came back early", "Then the team went on"),
            ("And were better than hoped", "To plan the second round"),
            ("For every one of the sites", "With the same partners as"),
            ("Measured over the season", "Before and in the same way"),
        ]
        .into_iter()
        .enumerate()
        {
            let at = |x1: f64, t: &str| word(x1 - 5.0 * t.chars().count() as f64, r, t, None);
            gl.push(at(190.0, left));
            gl.push(at(360.0, right));
        }
        // Set flush left as well: one start for every line of each column.
        for g in gl.iter_mut() {
            let b = g.bbox.as_mut().unwrap();
            let x0 = if b.x0 < 200.0 { 50.0 } else { 220.0 };
            let w = b.x1 - b.x0;
            b.x0 = x0;
            b.x1 = x0 + w;
            g.origin.0 = x0;
        }
        assert!(field_lists(&gl, 0, &crate::column::detect(&gl)).is_empty());
    }

    #[test]
    fn a_value_wrapped_under_its_label_is_a_paragraph() {
        let mut gl = Vec::new();
        for (r, value) in [
            "CN 4425 - Correct the units in the mixing table.",
            "CN 4431 - Add the drying time to the second wash and",
        ]
        .into_iter()
        .enumerate()
        {
            gl.push(word(50.0, r, "Change", Some(400)));
            gl.push(word(85.0, r, "Note:", Some(400)));
            gl.push(word(130.0, r, value, Some(400)));
        }
        assert_eq!(detect_aligned(&gl, &[], &[], 0).len(), 1);
        gl.push(word(
            50.0,
            2,
            "the operator's check after it, and renumber the steps.",
            Some(400),
        ));
        assert!(detect_aligned(&gl, &[], &[], 0).is_empty());
    }

    #[test]
    fn colon_fields_are_labels_beside_values() {
        let s = |v: &[&str]| v.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        assert!(colon_fields(&s(&[
            "Report no.:",
            "FR-0042",
            "Issued:",
            "2026-01-05"
        ])));
        assert!(colon_fields(&s(&[
            "Liabilities ($):",
            "1,204",
            "Assets ($):",
            "3,410"
        ])));
        // A label run on to the next one's first word, among labels that
        // end in their colon.
        assert!(colon_fields(&s(&[
            "Pack Size:",
            "12 per pack",
            "Closure Type: Screw/",
            "N/A",
            "Snap Fit:",
            "Yes",
        ])));
        // Two fields to a row.
        assert!(!colon_fields(&s(&[
            "Patient: A. Sample",
            "DOB: 1987-03-14",
            "Ward: West wing",
            "Bed: 12",
        ])));
        // A schedule's times are no labels.
        assert!(!colon_fields(&s(&[
            "09:00",
            "Registration",
            "10:30",
            "Keynote"
        ])));
        // A chart's note and source are sentences.
        assert!(!colon_fields(&s(&[
            "NOTE:",
            "Output is measured in finished units. All series are adjusted.",
            "SOURCE:",
            "Plant records and the office's own calculations from them.",
        ])));
        // A field needs its value.
        assert!(!colon_fields(&s(&["Weight:", "24 g", "Comments:", ""])));
    }

    #[test]
    fn three_aligned_rows_need_drawn_geometry() {
        use crate::geom::BBox as B;
        use crate::glyph::Glyph;
        let mut gl = Vec::new();
        for r in 0..3 {
            for c in 0..4 {
                let x = 50.0 + c as f64 * 60.0;
                let y = r as f64 * 20.0;
                gl.push(Glyph {
                    text: "cell".into(),
                    bbox: Some(B {
                        x0: x,
                        y0: y,
                        x1: x + 20.0,
                        y1: y + 10.0,
                    }),
                    page: 0,
                    origin: (x, y + 10.0),
                    rotation_deg: 0.0,
                    font_size: 10.0,
                    weight: None,
                    italic: false,
                    advance: None,
                    draw_index: 0,
                    ocr: None,
                });
            }
        }
        assert_eq!(detect_aligned_candidates(&gl, 0).len(), 1);
        assert!(detect_aligned(&gl, &[], &[], 0).is_empty());
        let fill = crate::rule::Fill {
            bbox: B {
                x0: 50.0,
                y0: -5.0,
                x1: 290.0,
                y1: 15.0,
            },
            page: 0,
            paint: None,
            stroked: false,
        };
        assert!(
            detect_aligned(&gl, &[], &[fill], 0).is_empty(),
            "three-row candidates need a rule, not only a broad fill"
        );
        let cell_fills: Vec<crate::rule::Fill> = (0..3)
            .flat_map(|r| {
                (0..4).map(move |c| crate::rule::Fill {
                    bbox: B {
                        x0: 50.0 + c as f64 * 60.0,
                        y0: r as f64 * 20.0,
                        x1: 70.0 + c as f64 * 60.0,
                        y1: 10.0 + r as f64 * 20.0,
                    },
                    page: 0,
                    paint: None,
                    stroked: false,
                })
            })
            .collect();
        assert_eq!(
            detect_aligned(&gl, &[], &cell_fills, 0).len(),
            1,
            "one fill per cell is strong short-table evidence"
        );
        assert_eq!(
            detect_aligned(&gl, &[h(50.0, 290.0, -5.0)], &[], 0).len(),
            1
        );
    }

    #[test]
    fn recovers_rows_from_vertical_segment_endpoints() {
        // The mirror shape: verticals segmented per row, with only a top and
        // bottom horizontal.
        let xs = [84.0, 200.0, 320.0, 440.0];
        let ys = [155.0, 181.0, 208.0, 234.0, 260.0, 287.0];
        let mut r = Vec::new();
        for x in xs {
            for w in ys.windows(2) {
                r.push(v(w[0], w[1], x));
            }
        }
        r.push(h(84.0, 440.0, 155.0));
        r.push(h(84.0, 440.0, 287.0));
        let g = detect(&r, 0);
        assert_eq!(g.len(), 1, "segmented verticals must form a grid");
        assert_eq!(g[0].cols(), 3);
        assert_eq!(
            g[0].rows(),
            5,
            "rows from vertical endpoints: {:?}",
            g[0].ys
        );
    }

    #[test]
    fn chart_gridlines_do_not_form_a_table() {
        // Several long horizontal gridlines, but their endpoints are only the
        // plot edges — no interior x is supported, so there is no grid.
        let mut r = Vec::new();
        for y in [100.0, 150.0, 200.0, 250.0, 300.0, 350.0] {
            r.push(h(60.0, 540.0, y));
        }
        assert!(detect(&r, 0).is_empty(), "a chart is not a table");
    }

    #[test]
    fn a_single_box_is_not_a_table() {
        let r = vec![
            h(0.0, 100.0, 0.0),
            h(0.0, 100.0, 50.0),
            v(0.0, 50.0, 0.0),
            v(0.0, 50.0, 100.0),
        ];
        assert!(detect(&r, 0).is_empty(), "two rules each way is a box");
    }

    #[test]
    fn short_underlines_do_not_form_a_grid() {
        // Rules under headings: each covers a small fraction of the width.
        let r = vec![
            h(0.0, 30.0, 0.0),
            h(0.0, 30.0, 100.0),
            h(0.0, 30.0, 200.0),
            h(400.0, 800.0, 300.0),
        ];
        assert!(detect(&r, 0).is_empty());
    }

    #[test]
    fn coalesce_merges_near_equal_positions() {
        let out = coalesce(vec![68.0, 68.4, 183.0, 183.2, 297.0], 3.0);
        assert_eq!(out.len(), 3);
        assert!((out[0] - 68.2).abs() < 0.01);
    }

    /// `text` set from `x` on baseline `y`: one glyph a character at 10pt,
    /// 5.5pt apart, spaces as outline-less glyphs the way a PDF draws them.
    fn set(text: &str, x: f64, y: f64) -> Vec<crate::glyph::Glyph> {
        text.chars()
            .enumerate()
            .map(|(i, ch)| {
                let cx = x + 5.5 * i as f64;
                crate::glyph::Glyph {
                    text: ch.to_string(),
                    bbox: (ch != ' ').then_some(BBox {
                        x0: cx,
                        y0: y - 8.0,
                        x1: cx + 5.0,
                        y1: y + 2.0,
                    }),
                    page: 0,
                    origin: (cx, y),
                    rotation_deg: 0.0,
                    font_size: 10.0,
                    weight: None,
                    italic: false,
                    advance: Some(5.5),
                    draw_index: 0,
                    ocr: None,
                }
            })
            .collect()
    }

    #[test]
    fn text_after_dot_leaders_starts_a_cell() {
        let inked = |gs: &[crate::glyph::Glyph]| -> Vec<crate::glyph::Glyph> {
            gs.iter().filter(|g| g.bbox.is_some()).cloned().collect()
        };
        // A label's leaders run up to the next column's text, a word space
        // short of it: the leaders are the gutter.
        let row = inked(&set("Kolkata, India ........ Manufacturing", 40.0, 100.0));
        let refs: Vec<&crate::glyph::Glyph> = row.iter().collect();
        assert_eq!(cell_starts(&refs).len(), 2);
        // An ellipsis in a sentence parts nothing.
        let row = inked(&set("and so on... more", 40.0, 100.0));
        let refs: Vec<&crate::glyph::Glyph> = row.iter().collect();
        assert_eq!(cell_starts(&refs).len(), 1);
        // A glyph a hair off the row's baseline comes first by height; a row
        // holding leaders is still read left to right.
        let mut glyphs = set(
            "Kolkata, India ........ Manufacturing  Freight",
            40.0,
            100.0,
        );
        let n = glyphs.len();
        for g in &mut glyphs[n - 7..] {
            g.origin.1 -= 0.01;
        }
        glyphs.extend(set("Belo Horizonte ........ Office  Freight", 40.0, 112.0));
        let (_, cells) = aligned_rows(&glyphs);
        assert_eq!(cells[0].len(), 3);
        assert_eq!(cells[1].len(), 3);
    }

    #[test]
    fn a_sign_set_after_its_figure_is_read_into_its_cell() {
        // `4,85 €` and `19,40 €` in a drawn grid: the first sign sits well
        // inside its column, and was read with the figure after it.
        let g = grid(&[40.0, 150.0, 230.0, 320.0, 400.0], &[100.0, 120.0]);
        let mut gl = set("Seau", 42.0, 115.0);
        gl.extend(set("4", 160.0, 115.0));
        gl.extend(set("4,85", 240.0, 115.0));
        gl.extend(set("€", 265.0, 115.0));
        gl.extend(set("19,40", 330.0, 115.0));
        gl.extend(set("€", 360.5, 115.0));
        let cells = g.cell_texts(&gl);
        assert_eq!(cells[2], "4,85 €");
        assert_eq!(cells[3], "19,40 €");
    }

    #[test]
    fn a_pinned_sign_still_prices_the_figure_after_it() {
        // `$` a word space after the figure in the column before, its own
        // figure set right: the line's figures are bare, so the sign is a
        // prefix and goes with the figure after it.
        let g = grid(&[40.0, 150.0, 285.0, 400.0], &[100.0, 120.0]);
        let mut gl = set("Revenue", 42.0, 115.0);
        gl.extend(set("1,234", 250.0, 115.0));
        gl.extend(set("$", 281.0, 115.0));
        gl.extend(set("5,678", 340.0, 115.0));
        let cells = g.cell_texts(&gl);
        assert_eq!(cells[1], "1,234");
        assert_eq!(cells[2], "$ 5,678");
    }

    #[test]
    fn a_sign_past_a_cut_is_not_the_figure_before_it() {
        // `$` a word space after `1,234` but across the cut, pinned at its
        // own column's edge, its figure further off than a prefix reaches:
        // it stays in its column, whatever the line's other signs.
        let g = grid(&[40.0, 150.0, 279.0, 400.0, 480.0], &[100.0, 120.0]);
        let mut gl = set("Revenue", 42.0, 115.0);
        gl.extend(set("1,234", 250.0, 115.0));
        gl.extend(set("$", 281.0, 115.0));
        gl.extend(set("5,678", 370.0, 115.0));
        gl.extend(set("9,10", 410.0, 115.0));
        gl.extend(set("€", 435.0, 115.0));
        let cells = g.cell_texts(&gl);
        assert_eq!(cells[1], "1,234");
        assert_eq!(cells[3], "9,10 €");
    }

    fn grid(xs: &[f64], ys: &[f64]) -> Grid {
        Grid {
            layout: None,
            page: 0,
            xs: xs.to_vec(),
            ys: ys.to_vec(),
            bbox: BBox {
                x0: xs[0],
                y0: ys[0],
                x1: *xs.last().unwrap(),
                y1: *ys.last().unwrap(),
            },
        }
    }

    fn merged(g: &Grid, above: &[(usize, usize)], left: &[(usize, usize)]) -> Merges {
        let n = g.rows() * g.cols();
        let mut m = Merges {
            continues_above: vec![false; n],
            continues_left: vec![false; n],
            full_width_row: vec![false; g.rows()],
        };
        for &(r, c) in above {
            m.continues_above[r * g.cols() + c] = true;
        }
        for &(r, c) in left {
            m.continues_left[r * g.cols() + c] = true;
        }
        m
    }

    #[test]
    fn columns_of_prose_are_not_a_table() {
        // Two columns of body text cut into one row a line.
        let g = grid(&[0.0, 300.0, 600.0], &[0.0, 14.0, 28.0, 42.0, 56.0]);
        let mut gl = Vec::new();
        for (r, (a, b)) in [
            (
                "The control system is built into the",
                "Revolving wash arms with un-",
            ),
            (
                "dishwasher and is available in standard",
                "restricted openings above and below",
            ),
            (
                "electrical specifications for every site,",
                "provide thorough distribution of water",
            ),
            (
                "and it monitors every wash cycle.",
                "jets to all of the dishware surfaces.",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let y = 10.0 + 14.0 * r as f64;
            gl.extend(set(a, 5.0, y));
            gl.extend(set(b, 305.0, y));
        }
        assert!(reads_as_prose(&g, &gl));
        // A table whose one description column wraps: its codes and figures
        // do not run on.
        let g = grid(&[0.0, 100.0, 400.0, 500.0], &[0.0, 14.0, 28.0, 42.0, 56.0]);
        let mut gl = Vec::new();
        for (r, (code, text, qty)) in [
            ("A-100", "Stainless steel wash arm with an", "12"),
            ("", "extended reach for tall racks", ""),
            ("A-200", "Rinse arm for the upper rack and", "4"),
            ("", "the lower rack of the machine", ""),
        ]
        .into_iter()
        .enumerate()
        {
            let y = 10.0 + 14.0 * r as f64;
            gl.extend(set(code, 5.0, y));
            gl.extend(set(text, 105.0, y));
            gl.extend(set(qty, 405.0, y));
        }
        assert!(!reads_as_prose(&g, &gl));
    }

    #[test]
    fn columns_that_cut_words_are_not_a_table() {
        // Justified prose: its columns fall inside words.
        let g = grid(&[0.0, 22.0, 60.0, 200.0], &[0.0, 14.0, 28.0, 42.0]);
        let mut gl = Vec::new();
        for (r, line) in [
            "Only hosted conference calls",
            "for outgoing faxes only",
            "Discounted rate on research",
        ]
        .into_iter()
        .enumerate()
        {
            gl.extend(set(line, 2.0, 10.0 + 14.0 * r as f64));
        }
        assert!(cuts_words(&g, &gl));
        // Values in their columns: nothing is cut.
        let g = grid(&[0.0, 40.0, 100.0, 200.0], &[0.0, 14.0, 28.0, 42.0]);
        let mut gl = Vec::new();
        for (r, (a, b)) in [("Fax", "1.00"), ("Scan", "0.20"), ("Copy", "0.10")]
            .into_iter()
            .enumerate()
        {
            let y = 10.0 + 14.0 * r as f64;
            gl.extend(set(a, 2.0, y));
            gl.extend(set(b, 65.0, y));
        }
        assert!(!cuts_words(&g, &gl));
    }

    #[test]
    fn a_box_round_paragraphs_is_not_a_table() {
        let g = grid(&[0.0, 300.0, 600.0], &[0.0, 14.0, 300.0]);
        let mut gl = set("CHARACTERISTICS", 5.0, 10.0);
        gl.extend(set("COMPLIANCE", 305.0, 10.0));
        let para = "word ".repeat(12);
        for k in 0..6 {
            gl.extend(set(&para, 5.0, 30.0 + 14.0 * k as f64));
        }
        gl.extend(set("Yes", 305.0, 30.0));
        // One paragraph beside a short value is a table's long row.
        assert!(!reads_as_prose(&g, &gl));
        for k in 0..6 {
            gl.extend(set(&para, 305.0, 44.0 + 14.0 * k as f64));
        }
        assert!(reads_as_prose(&g, &gl));
    }

    #[test]
    fn a_value_wrapped_at_its_hyphen_keeps_it() {
        assert_eq!(join_cell_lines(["FR-", "15T"].into_iter()), "FR-15T");
        assert_eq!(
            join_cell_lines(["Dov Ben-", "David"].into_iter()),
            "Dov Ben-David"
        );
        assert_eq!(
            join_cell_lines(["Nominal", "flow"].into_iter()),
            "Nominal flow"
        );
        // A dash set apart is a range or a blank, not a wrapped word.
        assert_eq!(join_cell_lines(["10 -", "20"].into_iter()), "10 - 20");
        // A break that may be a typesetter's is left for the document.
        assert_eq!(
            join_cell_lines(["Electronic Fluo-", "rescent"].into_iter()),
            format!("Electronic Fluo{CELL_BREAK}rescent")
        );
        let g = grid(&[0.0, 60.0], &[0.0, 30.0]);
        let mut gl = set("FR-", 5.0, 10.0);
        gl.extend(set("15T", 5.0, 22.0));
        assert_eq!(g.cell_texts(&gl), ["FR-15T"]);
    }

    #[test]
    fn a_banner_higher_than_its_row_does_not_take_the_label_beside_it() {
        // `Contact` is centred over its columns, two points above `Payroll`:
        // sorted by baseline alone it came first, and `Payroll` then started
        // behind its pen and was read as the same word.
        let g = grid(&[0.0, 100.0, 200.0], &[90.0, 125.0]);
        let mut gl = set("Payroll", 10.0, 105.0);
        gl.extend(set("Contact", 115.0, 102.75));
        assert_eq!(g.cell_texts(&gl), ["Payroll", "Contact"]);
    }

    #[test]
    fn a_banner_across_two_columns_is_read_whole() {
        let g = grid(&[0.0, 60.0, 120.0], &[0.0, 20.0, 40.0]);
        let mut gl = set("Weight (kg)", 35.0, 14.0);
        gl.extend(set("Net", 10.0, 34.0));
        gl.extend(set("Shipping", 70.0, 34.0));
        let m = merged(&g, &[], &[(0, 1)]);
        assert_eq!(
            g.region_text(&gl, &m, 0..1, 0..2).as_deref(),
            Some("Weight (kg)")
        );
    }

    #[test]
    fn a_space_the_grid_cuts_after_stays_in_the_merged_cell() {
        // `le T2 2026`, its year set a narrow space after the quarter and
        // cut from it by the grid: the page's space glyph is the only sign
        // of the word break, narrower than a gap between digits reads as.
        let g = grid(&[0.0, 59.0, 120.0], &[0.0, 20.0]);
        let mut gl = set("le T2", 30.0, 14.0);
        let mut sp = set(" ", 57.5, 14.0);
        sp[0].advance = Some(2.6);
        gl.extend(sp);
        gl.extend(set("2026", 60.1, 14.0));
        let m = merged(&g, &[], &[(0, 1)]);
        assert_eq!(
            g.region_text(&gl, &m, 0..1, 0..2).as_deref(),
            Some("le T2 2026")
        );
        // Each cell on its own trims the space away.
        assert_eq!(g.cell_texts(&gl), ["le T2", "2026"]);
    }

    #[test]
    fn columns_never_ruled_apart_keep_their_own_values() {
        let g = grid(&[0.0, 60.0, 120.0], &[0.0, 20.0]);
        let mut gl = set("Foreign", 10.0, 14.0);
        gl.extend(set("Domestic", 70.0, 14.0));
        let m = merged(&g, &[], &[(0, 1)]);
        assert_eq!(g.region_text(&gl, &m, 0..1, 0..2), None);
    }

    #[test]
    fn a_header_centred_across_two_rows_is_read_whole() {
        // One line on the cut between the rows it spans: `Staff` and
        // `member` were dealt to the rows either side of it.
        let g = grid(&[0.0, 100.0, 200.0], &[0.0, 20.0, 40.0]);
        let mut gl = set("Staff member", 10.0, 24.0);
        gl.extend(set("Contact", 110.0, 14.0));
        gl.extend(set("Phone", 110.0, 34.0));
        let m = merged(&g, &[(1, 0)], &[]);
        assert_eq!(
            g.region_text(&gl, &m, 0..2, 0..1).as_deref(),
            Some("Staff member")
        );
    }

    #[test]
    fn labels_on_their_values_baselines_are_separate_rows() {
        // Tight rows with no rule between the labels: a paragraph's leading
        // apart, but each label sits on the baseline of its own value.
        let rows = |upper: &str, lower: &str| {
            let mut gl = set(upper, 5.0, 10.0);
            gl.extend(set(lower, 5.0, 22.0));
            gl.extend(set("L", 160.0, 10.0));
            gl.extend(set("06/14/2026", 160.0, 22.0));
            gl
        };
        let g = grid(&[0.0, 150.0, 220.0], &[0.0, 13.0, 26.0]);
        let m = merged(&g, &[(1, 0)], &[]);
        let gl = rows("Revision Level", "Revision Date");
        assert_eq!(g.region_text(&gl, &m, 0..2, 0..1), None);
        // The same geometry, but the lower line plainly carries on the upper.
        let gl = rows("KEYTRUDA in combination", "with axitinib");
        assert_eq!(
            g.region_text(&gl, &m, 0..2, 0..1).as_deref(),
            Some("KEYTRUDA in combination with axitinib")
        );
    }

    /// A table that groups its columns, ruled with horizontals only: a row
    /// label column, then two groups of three, each group's label set over
    /// a rule drawn under that group alone, the rules parted at the cut
    /// between the groups. `top` is what is ruled where the group rules
    /// are; `beside` is set on the labels' line over the row labels.
    fn grouped_columns(
        top: &[Rule],
        beside: Option<&str>,
    ) -> (Grid, Vec<Rule>, Vec<crate::glyph::Glyph>) {
        let xs = [40.0, 140.0, 190.0, 240.0, 290.0, 340.0, 390.0, 440.0];
        let mut rules = top.to_vec();
        for y in [120.0, 135.0, 150.0] {
            rules.push(h(40.0, 440.0, y));
        }
        let mut gl = set("Table 1. Prices", 40.0, 70.0);
        gl.extend(set("2024", 200.0, 96.0));
        gl.extend(set("Change", 345.0, 96.0));
        if let Some(t) = beside {
            gl.extend(set(t, 42.0, 96.0));
        }
        gl.extend(set("State", 42.0, 116.0));
        for (i, name) in ["Under", "Surf", "Total", "Under", "Surf", "Total"]
            .iter()
            .enumerate()
        {
            gl.extend(set(name, xs[i + 1] + 2.0, 116.0));
        }
        for (y, label) in [(131.0, "Alabama"), (146.0, "Alaska")] {
            gl.extend(set(label, 42.0, y));
            for x in &xs[1..7] {
                gl.extend(set("12.5", x + 10.0, y));
            }
        }
        (grid(&xs, &[100.0, 120.0, 135.0, 150.0]), rules, gl)
    }

    fn group_rules() -> Vec<Rule> {
        vec![h(140.0, 288.0, 100.0), h(292.0, 440.0, 100.0)]
    }

    #[test]
    fn labels_over_the_rules_that_group_the_columns_are_the_top_row() {
        let (mut g, rules, gl) = grouped_columns(&group_rules(), None);
        g.take_group_labels(&rules, &gl);
        assert_eq!(g.rows(), 4, "the labels' line is taken in");
        assert!(
            g.ys[0] < 90.0 && g.ys[0] > 80.0,
            "and nothing above it: {:?}",
            g.ys
        );
        let cells = g.cell_texts(&gl);
        assert_eq!(cells[2], "2024");
        let mut m = g.merges(&rules, &[]);
        g.group_label_merges(&gl, &rules, &mut m);
        assert_eq!(
            &m.continues_left[..7],
            &[false, false, true, true, false, true, true],
            "each label spans the columns its rule runs under"
        );
    }

    #[test]
    fn a_rule_across_the_whole_table_groups_nothing() {
        // The table's top edge, and over it the table's caption.
        let (mut g, rules, gl) = grouped_columns(&[h(40.0, 440.0, 100.0)], None);
        g.take_group_labels(&rules, &gl);
        assert_eq!(g.rows(), 3);
        // Ruled column by column: each column's own underline.
        let cut: Vec<Rule> = [40.0, 140.0, 190.0, 240.0, 290.0, 340.0, 390.0]
            .iter()
            .map(|&x| h(x + 2.0, x + 48.0, 100.0))
            .collect();
        let (mut g, rules, gl) = grouped_columns(&cut, None);
        g.take_group_labels(&rules, &gl);
        assert_eq!(g.rows(), 3);
    }

    #[test]
    fn a_line_over_the_unruled_columns_is_no_row_of_group_labels() {
        // A label over the row labels set with them: the header above is
        // more than one line of group labels, and none of it is taken.
        let (mut g, rules, gl) = grouped_columns(&group_rules(), Some("Region"));
        g.take_group_labels(&rules, &gl);
        assert_eq!(g.rows(), 3);
    }

    #[test]
    fn values_over_a_rule_that_leaves_a_column_out_keep_their_columns() {
        // A total's figures, the rule under them leaving the label column
        // out: three values, the cuts falling cleanly between them.
        let xs = [40.0, 140.0, 190.0, 240.0];
        let g = grid(&xs, &[100.0, 120.0, 135.0, 150.0]);
        let mut rules = vec![h(140.0, 240.0, 120.0)];
        rules.extend([135.0, 150.0].map(|y| h(40.0, 240.0, y)));
        let mut gl = set("Total", 42.0, 116.0);
        for x in &xs[1..4] {
            gl.extend(set("12.5", x + 10.0, 116.0));
        }
        for y in [131.0, 146.0] {
            gl.extend(set("Item", 42.0, y));
            for x in &xs[1..4] {
                gl.extend(set("1.0", x + 10.0, y));
            }
        }
        let mut m = g.merges(&rules, &[]);
        g.group_label_merges(&gl, &rules, &mut m);
        assert!(!m.continues_left.iter().any(|x| *x));
    }
}

/// An upright edge of a box drawn on the page, from its top to its foot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wall {
    pub x: f64,
    pub y0: f64,
    pub y1: f64,
}

/// Fewest shapes a box holds for it to be a drawing's frame, whose edges
/// part the text either side of them.
const FRAMED_SHAPES: usize = 3;

/// The upright edges of the boxes drawn round drawings: a fill holding
/// other shapes, a wiring diagram's box. A table's shaded column holds
/// text, not shapes, and its edge parts no row.
pub fn walls(fills: &[crate::rule::Fill]) -> Vec<Wall> {
    fills
        .iter()
        .filter(|f| {
            let b = f.bbox;
            let area = b.width() * b.height();
            fills
                .iter()
                .filter(|o| {
                    let c = o.bbox;
                    let (cx, cy) = ((c.x0 + c.x1) * 0.5, (c.y0 + c.y1) * 0.5);
                    b.contains(cx, cy) && c.width() * c.height() < area * 0.5
                })
                .count()
                >= FRAMED_SHAPES
        })
        .flat_map(|f| {
            let b = f.bbox;
            [
                Wall {
                    x: b.x0,
                    y0: b.y0,
                    y1: b.y1,
                },
                Wall {
                    x: b.x1,
                    y0: b.y0,
                    y1: b.y1,
                },
            ]
        })
        .collect()
}

/// Does one of `walls` stand inside `b`, between its left and right
/// edges, from its top to its foot? A grid read across it puts a drawing's
/// labels and the note beside its box in one row.
pub fn walled_in(b: &crate::geom::BBox, walls: &[Wall]) -> bool {
    walls
        .iter()
        .any(|w| w.x > b.x0 + 1.0 && w.x < b.x1 - 1.0 && w.y0 <= b.y0 && w.y1 >= b.y1)
}

#[cfg(test)]
mod cross_gutter_tests {
    use super::*;

    /// `text` set at `x` on the baseline `y`, 9 pt, a glyph a character
    /// 4.5 pt wide, rising 8.5 pt over its baseline and dropping 2 under it; a space is a gap of the
    /// same width.
    fn say(text: &str, x: f64, y: f64) -> Vec<crate::glyph::Glyph> {
        let mut out = Vec::new();
        for (i, c) in text.chars().enumerate() {
            if c == ' ' {
                continue;
            }
            let x0 = x + i as f64 * 4.5;
            out.push(crate::glyph::Glyph {
                text: c.to_string(),
                bbox: Some(BBox {
                    x0,
                    y0: y - 8.5,
                    x1: x0 + 4.2,
                    y1: y + 2.0,
                }),
                page: 0,
                origin: (x0, y),
                rotation_deg: 0.0,
                font_size: 9.0,
                advance: Some(4.5),
                weight: None,
                italic: false,
                draw_index: 0,
                ocr: None,
            });
        }
        out
    }

    const PROSE: &str = "the members agreed that the outlook for the economy";

    /// A paragraph of `n` lines from baseline `y`, 11 pt apart, at `x`.
    fn paragraph(x: f64, y: f64, n: usize) -> Vec<crate::glyph::Glyph> {
        (0..n)
            .flat_map(|i| say(PROSE, x, y + 11.0 * i as f64))
            .collect()
    }

    fn grid(xs: &[f64], ys: &[f64]) -> Grid {
        Grid {
            layout: None,
            page: 0,
            xs: xs.to_vec(),
            ys: ys.to_vec(),
            bbox: BBox {
                x0: xs[0],
                y0: ys[0],
                x1: *xs.last().unwrap(),
                y1: *ys.last().unwrap(),
            },
        }
    }

    fn crosses(glyphs: &[crate::glyph::Glyph], g: &Grid) -> bool {
        let runs = crate::textgrid::runs(&crate::textgrid::lines(glyphs));
        let rows = reading_rows(g, glyphs);
        channels_in(&rows)
            .into_iter()
            .any(|span| crosses_gutter(g, &rows, || &runs, span, 600.0).is_some())
    }

    #[test]
    fn a_table_read_with_the_prose_beside_it_crosses_the_gutter() {
        // The left column's paragraph runs from the page's top to its foot;
        // the right column sets a paragraph, then a table of labels and
        // figures. A reading of the table took the paragraph's lines beside
        // it as its first column.
        let mut gl = paragraph(40.0, 100.0, 26);
        gl.extend(paragraph(310.0, 100.0, 6));
        let rows = [
            "Inventories",
            "Product liabilities",
            "Accrued expenses",
            "State taxes",
        ];
        for (i, label) in rows.iter().enumerate() {
            let y = 200.0 + 14.0 * i as f64;
            gl.extend(say(label, 310.0, y));
            gl.extend(say("1,513", 520.0, y));
        }
        let g = grid(
            &[40.0, 300.0, 500.0, 560.0],
            &[190.0, 203.0, 217.0, 231.0, 245.0],
        );
        assert!(crosses(&gl, &g));
    }

    #[test]
    fn a_table_wrapped_into_two_halves_does_not_cross_the_gutter() {
        // Prose in both columns over and under one table set in two halves
        // side by side, every row on one baseline across both.
        let mut gl = paragraph(40.0, 100.0, 4);
        gl.extend(paragraph(310.0, 100.0, 4));
        for i in 0..6 {
            let y = 180.0 + 14.0 * i as f64;
            gl.extend(say(&format!("C{}—C{}", i + 1, i + 2), 40.0, y));
            gl.extend(say("1.392 (3)", 200.0, y));
            gl.extend(say(&format!("N{}—C{}", i + 1, i + 3), 310.0, y));
            gl.extend(say("1.401 (2)", 470.0, y));
        }
        gl.extend(paragraph(40.0, 300.0, 4));
        gl.extend(paragraph(310.0, 300.0, 4));
        let ys: Vec<f64> = (0..7).map(|i| 170.0 + 14.0 * i as f64).collect();
        let g = grid(&[40.0, 190.0, 300.0, 460.0, 560.0], &ys);
        assert!(!crosses(&gl, &g));
    }

    #[test]
    fn a_gap_between_a_tables_own_columns_is_no_gutter() {
        // One column of prose across the page over a table whose columns of
        // text leave a gap where a gutter would be: the prose runs across
        // it, and nothing beside the table runs on as a paragraph.
        let mut gl: Vec<crate::glyph::Glyph> = (0..4)
            .flat_map(|i| say(&format!("{PROSE} {PROSE}"), 40.0, 100.0 + 12.0 * i as f64))
            .collect();
        let rows = [
            ("Spray drying", "Budesonide"),
            ("Freeze drying", "Fenofibrate"),
        ];
        for (i, (a, b)) in rows.iter().enumerate() {
            let y = 180.0 + 14.0 * i as f64;
            gl.extend(say(a, 40.0, y));
            gl.extend(say(b, 310.0, y));
        }
        let g = grid(&[40.0, 300.0, 560.0], &[170.0, 183.0, 197.0]);
        assert!(!crosses(&gl, &g));
    }
}

#[cfg(test)]
mod piece_tests {
    use super::*;

    /// A small deterministic generator: positions on a coarse lattice, so
    /// pieces repeat each other and share ends, as a chart's strokes do.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }
        fn at(&mut self, steps: u64, step: f64) -> f64 {
            (self.next() % steps) as f64 * step
        }
    }

    /// The pairwise readings the sweeps replace, kept as they were.
    fn middles_pairwise(parts: &[(f64, f64)], t: f64) -> Vec<f64> {
        let mut middles = Vec::new();
        for (i, &(a, b)) in parts.iter().enumerate() {
            for &(c, d) in &parts[i + 1..] {
                let nested = (c <= a + t && d >= b - t) || (a <= c + t && b >= d - t);
                let (lo, hi) = (a.max(c), b.min(d));
                if hi - lo > t && !nested {
                    middles.push((lo + hi) / 2.0);
                }
            }
        }
        middles
    }

    fn ends_pairwise(parts: &[(f64, f64)], t: f64) -> Vec<f64> {
        let mut ends: Vec<f64> = parts.iter().flat_map(|&(a, b)| [a, b]).collect();
        ends.retain(|&e| {
            let own: Vec<&(f64, f64)> = parts
                .iter()
                .filter(|(a, b)| (a - e).abs() <= 0.01 || (b - e).abs() <= 0.01)
                .collect();
            !own.iter().any(|&&(a, b)| {
                parts.iter().any(|&(c, d)| {
                    let contains = c <= a + t && d >= b - t;
                    let overlaps = c < b - t && d > a + t;
                    overlaps && !contains && c < e - t && d > e + t
                })
            })
        });
        ends
    }

    fn components_pairwise(rules: &[Rule]) -> Vec<Vec<usize>> {
        let mut remaining: Vec<usize> = (0..rules.len()).collect();
        let mut components: Vec<Vec<usize>> = Vec::new();
        while let Some(seed) = remaining.pop() {
            let mut component = vec![seed];
            let mut changed = true;
            while changed {
                changed = false;
                let mut i = 0;
                while i < remaining.len() {
                    let candidate = rules[remaining[i]];
                    let connected = component.iter().any(|&k| {
                        let r = rules[k];
                        r.bbox.x1 + EDGE_TOLERANCE >= candidate.bbox.x0
                            && candidate.bbox.x1 + EDGE_TOLERANCE >= r.bbox.x0
                            && r.bbox.y1 + EDGE_TOLERANCE >= candidate.bbox.y0
                            && candidate.bbox.y1 + EDGE_TOLERANCE >= r.bbox.y0
                    });
                    if connected {
                        component.push(remaining[i]);
                        remaining.swap_remove(i);
                        changed = true;
                    } else {
                        i += 1;
                    }
                }
            }
            component.sort_unstable();
            components.extend(split_stacked_boxes(component, |i| rules[i]));
        }
        components
    }

    fn bits(v: &[f64]) -> Vec<u64> {
        v.iter().map(|x| x.to_bits()).collect()
    }

    #[test]
    fn a_rules_pieces_read_by_sweep_as_they_were_pair_by_pair() {
        let t = EDGE_TOLERANCE;
        let (mut middles, mut dropped) = (0, 0);
        for seed in 0..200u64 {
            let mut rng = Lcg(seed);
            let n = 1 + (rng.next() % 60) as usize;
            // Some runs on a fine lattice (overlaps of every size), some on
            // a coarse one (stacked repeats sharing their ends).
            let step = if seed.is_multiple_of(2) { 0.5 } else { 7.5 };
            let parts: Vec<(f64, f64)> = (0..n)
                .map(|_| {
                    let a = rng.at(80, step);
                    (a, a + rng.at(40, step))
                })
                .collect();
            assert_eq!(
                bits(&overlap_middles(&parts, t)),
                bits(&middles_pairwise(&parts, t)),
                "middles, seed {seed}"
            );
            let mut ends: Vec<f64> = parts.iter().flat_map(|&(a, b)| [a, b]).collect();
            ends.retain(end_kept(&parts, t));
            assert_eq!(
                bits(&ends),
                bits(&ends_pairwise(&parts, t)),
                "ends, seed {seed}"
            );
            middles += middles_pairwise(&parts, t).len();
            dropped += 2 * parts.len() - ends.len();
        }
        // The runs hold both kinds of overlap the readings act on.
        assert!(
            middles > 0 && dropped > 0,
            "{middles} middles, {dropped} ends dropped"
        );
    }

    #[test]
    fn touching_rules_are_grouped_as_they_were_rule_by_rule() {
        let mut joined = 0;
        for seed in 0..200u64 {
            let mut rng = Lcg(seed ^ 0x9e37);
            let n = 1 + (rng.next() % 80) as usize;
            let rules: Vec<Rule> = (0..n)
                .map(|_| {
                    let (x, y) = (rng.at(60, 5.0), rng.at(60, 5.0));
                    let len = rng.at(30, 5.0);
                    let orientation = if rng.next().is_multiple_of(2) {
                        Orientation::Horizontal
                    } else {
                        Orientation::Vertical
                    };
                    let bbox = match orientation {
                        Orientation::Horizontal => BBox {
                            x0: x,
                            y0: y,
                            x1: x + len,
                            y1: y + 0.6,
                        },
                        Orientation::Vertical => BBox {
                            x0: x,
                            y0: y,
                            x1: x + 0.6,
                            y1: y + len,
                        },
                    };
                    Rule {
                        bbox,
                        orientation,
                        page: 0,
                    }
                })
                .collect();
            let comps = component_indices(&rules);
            assert_eq!(comps, components_pairwise(&rules), "seed {seed}");
            joined += comps.iter().filter(|c| c.len() > 1).count();
        }
        assert!(joined > 0);
    }
}
