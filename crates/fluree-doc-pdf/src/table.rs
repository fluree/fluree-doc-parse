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
        let (rows, cols) = (self.rows(), self.cols());
        let mut buckets: Vec<Vec<crate::glyph::Glyph>> = vec![Vec::new(); rows * cols];

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
        idx.sort_by(|&a, &b| {
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
                if !same_line || start - pen_end > fs * 0.2 {
                    runs.push(std::mem::take(&mut run));
                }
            }
            run.push(i);
        }
        if !run.is_empty() {
            runs.push(run);
        }
        // A lone currency symbol belongs to the number it prices: merge it
        // into the following numeric run so `$` and `7,835,559` land in the
        // same cell. Financial statements pin the symbol at the column's
        // left edge with the value right-aligned, so the gap between them
        // can be wide — several ems — while still being one cell.
        let mut m = 0;
        while m + 1 < runs.len() {
            let is_currency = runs[m].len() == 1
                && matches!(glyphs[runs[m][0]].text.trim(), "$" | "€" | "£" | "¥");
            let next_numeric = runs[m + 1].iter().all(|&i| {
                glyphs[i]
                    .text
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '(' | ')' | '%' | '-'))
            });
            let close = {
                let a = &glyphs[*runs[m].last().unwrap()];
                let b = &glyphs[runs[m + 1][0]];
                let fs = a.font_size.max(1.0) as f64;
                (b.origin.1 - a.origin.1).abs() < fs * 0.3
                    && b.bbox.unwrap().x0 - a.bbox.unwrap().x1 < fs * 8.0
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
                        buckets[r * cols + c].push(glyphs[i].clone());
                    }
                }
                continue;
            }
            if let Some((r, c)) = self.cell_at(cx, cy) {
                for &i in run.iter() {
                    buckets[r * cols + c].push(glyphs[i].clone());
                }
            }
        }
        buckets
            .iter()
            .map(|gs| {
                crate::line::assemble(gs)
                    .iter()
                    .map(|l| l.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
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
                for r in 0..rows {
                    // A banner is the whole-row case, and must also be drawn
                    // as one: un-ruled rows equally occur where two separate
                    // tables were welded into a single grid component, and
                    // those rows are not banners. Shading is what makes a
                    // band read as a label rather than a row.
                    m.full_width_row[r] = (1..cols).all(|c| m.continues_left[r * cols + c])
                        && self.band_has_fill(fills, r);
                }
            }
        }
        m
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
        if label_column {
            return 0;
        }
        // run == 1 is itself the exclusive-stripe evidence: band 0 filled,
        // band 1 not (where the run stopped).
        let styled = self.cells_are_bold(glyphs, 0) && !self.row_is_bold(glyphs, 1) || run == 1;
        decide_header_rows(rows, styled)
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
fn is_numeric_cell(s: &str) -> bool {
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
}

/// Detect one table grid from a related set of ruling lines.
fn detect_group(rules: &[Rule], page: usize) -> Vec<Grid> {
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
    let ys = boundaries(&h_lines, &v_lines, span_x, span_y);
    let xs = boundaries(&v_lines, &h_lines, span_y, span_x);
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
    split_row_bands(xs, ys, rules, page)
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
    let xs = boundaries(&v_lines, &h_lines, span_y, span_x);
    (xs.len() >= 2).then_some(xs)
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
fn split_row_bands(xs: Vec<f64>, ys: Vec<f64>, rules: &[Rule], page: usize) -> Vec<Grid> {
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

    let mut out = Vec::new();
    let mut start = 0usize;
    for i in 0..ys.len() - 1 {
        let is_last = i + 2 == ys.len();
        let too_tall = (ys[i + 1] - ys[i]) > median * TABLE_SPLIT_GAP || is_gap(i);
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
    let whole = detect_group(rules, page);
    if !whole.is_empty() {
        return whole;
    }
    detect_components(rules, page)
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
    detect_components(rules, page)
}

/// Group the rules into touching components and detect a grid in each.
fn detect_components(rules: &[Rule], page: usize) -> Vec<Grid> {
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
        .flat_map(|component| detect_group(component, page))
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
    drop_degenerate(coalesce(cands, EDGE_TOLERANCE), MIN_CELL_EXTENT)
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
            accepts_aligned_candidate(g, rules, fills) || captioned || has_typeset_header(g, glyphs)
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
        if !taken && has_typeset_header(&part, glyphs) {
            out.push(part);
        }
    }
    out
}

/// Stretches of at least [`MIN_ALIGNED_ROWS`] consecutive rows that break
/// into the same number of cells, as grids.
fn aligned_parts(rows: &[Vec<&crate::glyph::Glyph>], cells: &[Vec<f64>], page: usize) -> Vec<Grid> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < rows.len() {
        let n = cells[i].len();
        let mut j = i;
        while j + 1 < rows.len() && cells[j + 1].len() == n {
            j += 1;
        }
        if n >= 2 && j - i + 1 >= MIN_ALIGNED_ROWS {
            out.extend(grid_from_cells(&rows[i..=j], &cells[i..=j], page));
        }
        i = j + 1;
    }
    out
}

/// An aligned run that is set as a table: a bold header row naming every
/// column over plain rows, with at least one column of numbers — or a bold
/// label beside a plain value on every row of two columns, the values
/// mostly figures (`Quantity | 2`, `Net | 4.70`).
///
/// A table printed with no rules at all (`border: none`, which is how many
/// receipts and invoices are styled) leaves alignment as its only drawn
/// trace, and alignment alone is what prose broken into columns also has —
/// accepting it ungated cost reading order across the corpus. Type is the
/// evidence that remains: a row of bold labels across three or more
/// columns, each label different, the rows under it plain, and figures
/// under at least one label. Prose set in columns has no such row.
fn has_typeset_header(g: &Grid, glyphs: &[crate::glyph::Glyph]) -> bool {
    let (rows, cols) = (g.rows(), g.cols());
    if cols == 2 && rows >= 3 {
        let bold = |(weighted, heavy): (usize, usize)| weighted > 0 && heavy * 2 > weighted;
        let labelled = (0..rows).all(|r| {
            let w = g.cell_weights(glyphs, r);
            bold(w[0]) && w[1].0 > 0 && !bold(w[1])
        });
        let cells = g.cell_texts(glyphs);
        let figures = (0..rows)
            .filter(|r| is_numeric_cell(cells[r * 2 + 1].trim()))
            .count();
        return labelled && figures * 2 >= rows;
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
    let n = cells.first()?.len();
    if n < 2 || cells.iter().any(|r| r.len() != n) {
        return None;
    }
    let mut lo = vec![f64::MAX; n];
    let mut hi = vec![f64::MIN; n];
    for (row, starts) in rows.iter().zip(cells) {
        for g in row {
            let b = g.bbox?;
            let k = starts.iter().rposition(|s| *s <= b.x0 + 0.01)?;
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
    if supported.len() < 2 {
        return None;
    }

    let x_hi = rows
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
    // columns as readily as a table's figures).
    let column_of = |xs: &[f64], x: f64| xs.iter().rposition(|b| *b <= x + 0.01).unwrap_or(0);
    let crowded = cells.iter().any(|starts| {
        let mut at: Vec<usize> = starts.iter().map(|x| column_of(&xs, *x)).collect();
        at.dedup();
        at.len() < starts.len()
    });
    if crowded {
        if let Some(by_rank) = columns_by_rank(rows, cells) {
            if by_rank.len() == xs.len() {
                xs = by_rank;
            }
        }
    }
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
        assert_eq!(g.header_rows(&rows, &[], &fills), 3);
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
        assert_eq!(g.header_rows(&rows, &[], &fills), 1);
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
        assert_eq!(g.header_rows(&t, &glyphs, &[]), 0);
        // A header bold in every cell over plain rows still is one, numbers
        // and all.
        let mut glyphs = Vec::new();
        glyphs.extend(run(5.0, 115.0, "2023", true));
        glyphs.extend(run(105.0, 115.0, "2024", true));
        glyphs.extend(run(5.0, 135.0, "10", false));
        glyphs.extend(run(105.0, 135.0, "12", false));
        let t = rows(&[&["1", "2"], &["10", "12"], &["", ""]]);
        assert_eq!(g.header_rows(&t, &glyphs, &[]), 1);
    }

    #[test]
    fn unruled_interiors_claim_no_merges() {
        // A bare bordered box: no interior horizontal rules, so "merged with
        // the row above" is unsupported by evidence and must not be claimed.
        let g = Grid {
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
        // is neither; the line table is still found on its own.
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
        let grids = detect_aligned(&gl, &[], &[], 0);
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].cols(), 5);
        assert_eq!(grids[0].rows(), 4);
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
            grids[0].header_rows(&rows, &gl, &[]),
            0,
            "a field is not a header"
        );
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
}
