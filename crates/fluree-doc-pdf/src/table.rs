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
const EDGE_TOLERANCE: f64 = 3.0;

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
    pub fn texts(&self, glyphs: &[crate::glyph::Glyph], cols: usize) -> Vec<String> {
        let mut out: Vec<String> = self
            .cells
            .iter()
            .map(|ids| {
                let gs: Vec<crate::glyph::Glyph> = ids.iter().map(|&i| glyphs[i].clone()).collect();
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
    /// Header labels set across the header's rows, read into the top one:
    /// `(first row, column, end row)`.
    pub downs: Vec<(usize, usize, usize)>,
    /// Body rows that label the rows beneath them.
    pub sub_headers: Vec<usize>,
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
    fn glyph_cells(&self, glyphs: &[crate::glyph::Glyph]) -> Vec<Option<usize>> {
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
        let suffixed: std::collections::HashSet<usize> = (0..runs.len())
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
                for &i in run.iter() {
                    let b = glyphs[i].bbox.unwrap();
                    if let Some((r, c)) = self.cell_at((b.x0 + b.x1) * 0.5, cy) {
                        home[i] = Some(r * cols + c);
                    }
                }
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
            let occupied: std::collections::HashSet<usize> =
                home.iter().flatten().copied().collect();
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
        let inside: Vec<crate::glyph::Glyph> = glyphs
            .iter()
            .filter(|g| {
                g.bbox.is_some() && !g.text.trim().is_empty() && g.is_horizontal() && {
                    let (x, y) = g.center();
                    self.cell_at(x, y).is_some_and(|(r, _)| r == row)
                }
            })
            .cloned()
            .collect();
        crate::line::assemble(&inside)
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
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
            let at_y: Vec<(f64, f64)> = hs
                .iter()
                .filter(|rule| (rule.bbox.y0 - y).abs() <= EDGE_TOLERANCE * 2.0)
                .map(|rule| (rule.bbox.x0, rule.bbox.x1))
                .collect();
            if at_y.is_empty() {
                continue;
            }
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
        let at = |k: usize| {
            rules
                .iter()
                .filter(|r| {
                    r.orientation == Orientation::Horizontal
                        && (r.bbox.y0 - self.ys[k]).abs() <= EDGE_TOLERANCE * 2.0
                        && r.bbox.x1 > left
                        && r.bbox.x0 < right
                })
                .map(|r| (r.bbox.x0, r.bbox.x1))
                .collect::<Vec<_>>()
        };
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
        let run = self.leading_fill_run(fills);
        if run >= 2 && run < rows.len() && run <= MAX_HEADER_ROWS {
            return run;
        }
        // A label column — bold on every row, nothing else bold — makes a
        // list of fields, whose first row is a field like the others.
        let bold = |(weighted, heavy): (usize, usize)| weighted > 0 && heavy * 2 > weighted;
        let weights: Vec<Vec<(usize, usize)>> = (0..self.rows())
            .map(|r| self.cell_weights(glyphs, r))
            .collect();
        let label_column = self.cols() >= 2
            && run == 0
            && weights.iter().all(|w| bold(w[0]))
            && weights.iter().all(|w| w[1..].iter().all(|c| !bold(*c)));
        // So does one whose labels a colon ends, in any weight.
        let colon_labels =
            self.cols() == 2 && rows.iter().all(|r| r.len() == 2) && colon_fields(&rows.concat());
        if label_column || colon_labels || self.lists_fields(rows, glyphs, rules, fills) {
            return 0;
        }
        // run == 1 is itself the exclusive-stripe evidence: band 0 filled,
        // band 1 not (where the run stopped).
        let styled = self.cells_are_bold(glyphs, 0) && !self.row_is_bold(glyphs, 1) || run == 1;
        decide_header_rows(rows, styled)
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
        if !labelled || names {
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

    /// The header block grown down through a stacked header's rows.
    ///
    /// A header cell ruled open into the row beneath it — `Item` spanning
    /// two rows beside `Price` over `Unit` and `Line` — makes that row part
    /// of the header. Open cells alone do not say so: a stub column ruled
    /// nowhere is open on every row, and following it made a 30-row table
    /// all header. So the rows taken in must read as labels (no numbers),
    /// the block stays within [`MAX_HEADER_ROWS`], and a rule drawn across
    /// the whole table must close it, the way a header is ruled off from
    /// its data. Otherwise the measured count stands.
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
        let y = self.ys[n];
        let closing = span_coverage(
            rules
                .iter()
                .filter(|r| {
                    r.orientation == Orientation::Horizontal
                        && (r.bbox.y0 - y).abs() <= EDGE_TOLERANCE * 2.0
                })
                .map(|r| (r.bbox.x0, r.bbox.x1)),
            self.bbox.x0,
            self.bbox.x1,
        );
        if closing >= 0.9 * (self.bbox.x1 - self.bbox.x0) {
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
        let (Some(&y0), Some(&y1)) = (self.ys.get(row), self.ys.get(row + 1)) else {
            return false;
        };
        let mid = (y0 + y1) / 2.0;
        let mut spans: Vec<(f64, f64)> = fills
            .iter()
            .filter(|f| f.bbox.y0 <= mid && f.bbox.y1 >= mid)
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

    /// Length of the leading run of filled row bands, stopping at the first
    /// unfilled band. Zebra striping self-limits: its bands alternate, so the
    /// run never exceeds 1.
    fn leading_fill_run(&self, fills: &[crate::rule::Fill]) -> usize {
        (0..self.rows())
            .take_while(|r| self.band_has_fill(fills, *r))
            .count()
    }
    /// Cell index containing a point, as (row, col).
    pub fn cell_at(&self, x: f64, y: f64) -> Option<(usize, usize)> {
        let c = self.xs.windows(2).position(|w| x >= w[0] && x < w[1])?;
        let r = self.ys.windows(2).position(|w| y >= w[0] && y < w[1])?;
        Some((r, c))
    }
}

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

/// A cell that reads as a quantity: digits with currency/percent/grouping
/// dressing and nothing alphabetic. Bare 4-digit years are *not* numeric for
/// header purposes — `2023 | 2024` over money columns is a header row.
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
            }
            _ => out.push(GridLine {
                axis: r.axis_pos(),
                lo,
                hi,
                ends: vec![lo, hi],
            }),
        }
    }
    out
}

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
        // row would pull the page footer in as a final table row.
        let r1 = (r0..rows).take_while(|&r| row_used[r]).last().unwrap_or(r0);
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

    let h_lines = cluster(&hs);
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
    // verticals at all — tables ruled purely with horizontals carry no such
    // evidence in either direction and are left whole.
    let mut ys = ys;
    if !vs.is_empty() {
        let crossed = |lo: f64, hi: f64| {
            let mid = (lo + hi) / 2.0;
            vs.iter().any(|r| r.bbox.y0 <= mid && r.bbox.y1 >= mid)
        };
        while ys.len() > MIN_GRID_LINES && !crossed(ys[0], ys[1]) {
            ys.remove(0);
        }
        while ys.len() > MIN_GRID_LINES && !crossed(ys[ys.len() - 2], ys[ys.len() - 1]) {
            ys.pop();
        }
    }

    // Boundaries closer together than this hold no content — an artefact of a
    // border rule sitting alongside a cell edge, which produced a spurious
    // empty first column on one such table.
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
fn band_columns(ys: &[f64], rules: &[Rule]) -> Option<Vec<f64>> {
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
    let h_lines = cluster(&hs);
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
        band_columns(&ys[lo..=hi + 1], rules).is_none_or(|xs| xs.len() <= 2)
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
                || is_gap(i);
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
                let band_xs = band_columns(&sub, rules).unwrap_or_else(|| xs.clone());
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
    let whole = detect_group(rules, fills, glyphs, page, frames);
    if whole.is_empty() {
        return detect_components(rules, fills, glyphs, page, frames);
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
        for g in detect_group(&rest, fills, glyphs, page, frames) {
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

/// The page's rules in touching sets, as indices.
fn component_indices(rules: &[Rule]) -> Vec<Vec<usize>> {
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
        components.push(component);
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
    detect_components(rules, fills, glyphs, page, frames)
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
        components.push(component);
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
    let captions: Vec<BBox> = crate::line::assemble(glyphs)
        .into_iter()
        .filter(|line| numbered_table_caption(&line.text))
        .map(|line| line.bbox)
        .collect();
    let (rows, cells) = aligned_rows(glyphs);
    let mut out: Vec<Grid> = aligned_runs(&rows, &cells, page)
        .into_iter()
        .filter(|g| {
            let captioned = g.cols() >= 3
                && g.rows() >= 3
                && captions.iter().any(|caption| {
                    caption.y1 <= g.bbox.y0
                        && g.bbox.y0 - caption.y1 <= g.bbox.height()
                        && caption.x1 >= g.bbox.x0
                        && caption.x0 <= g.bbox.x1
                });
            accepts_aligned_candidate(g, rules, fills)
                || captioned
                || has_typeset_header(g, glyphs)
                || has_two_axis_header(g, glyphs)
        })
        .collect();
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
    for list in colon_field_lists(glyphs, page) {
        let taken = out
            .iter()
            .any(|g| g.bbox.y0 < list.bbox.y1 && list.bbox.y0 < g.bbox.y1);
        if !taken {
            out.push(list);
        }
    }
    for g in out.iter_mut() {
        extend_by_banner(g, &rows);
    }
    out
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
fn colon_field_lists(glyphs: &[crate::glyph::Glyph], page: usize) -> Vec<Grid> {
    let (mut rows, _) = aligned_rows(glyphs);
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
        let mut j = i;
        while j + 1 < rows.len() && cells[j + 1].len() >= 2 {
            j += 1;
        }
        if j - i + 1 >= MIN_ALIGNED_ROWS {
            if let Some(g) = grid_from_cells(&rows[i..=j], &cells[i..=j], page) {
                out.push(g);
            }
        }
        i = j + 1;
    }
    out
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
    for g in row {
        let b = g.bbox.unwrap();
        let fs = g.font_size.max(1.0) as f64;
        match prev_end {
            None => out.push(b.x0),
            Some(pe) if b.x0 - pe > fs * TABLE_CELL_GAP => out.push(b.x0),
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
            advance: Some(w),
            draw_index: 0,
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
            },
            crate::rule::Fill {
                bbox: BBox {
                    x0: 90.0,
                    y0: 0.0,
                    x1: 170.0,
                    y1: 20.0,
                },
                page: 0,
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
}

#[cfg(test)]
mod tests {
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
                    advance: Some(4.5),
                    draw_index: i,
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
                    advance: Some(4.5),
                    draw_index: j,
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
                    advance: Some(6.0),
                    draw_index: 0,
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
                advance: None,
                draw_index: 0,
            })
            .collect();
        g.trim_to_content(&glyphs);
        assert!(
            g.rows() < rows_before,
            "empty trailing rows must be trimmed"
        );
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
            advance: None,
            draw_index: 0,
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
            advance: None,
            draw_index: 0,
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
            advance: Some(w),
            draw_index: 0,
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
                    advance: None,
                    draw_index: 0,
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
                    advance: Some(5.5),
                    draw_index: 0,
                }
            })
            .collect()
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
