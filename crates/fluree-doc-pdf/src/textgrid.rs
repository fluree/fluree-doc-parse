//! Tables without vertical rules, read from their text.
//!
//! Most tables in annual reports and journals draw no column rule at all.
//! A financial statement underlines its figures — a short rule under each
//! column of a sum, a double rule under each total — and a journal table is
//! set between three full-width rules. Read as ruling, both mislead: the
//! underlines' endpoints become column boundaries that leave out the label
//! column, and the bands between rules become rows holding a dozen lines
//! each (`426 4 | milli 438 5`). Measured on a sample of FinTabNet pages,
//! seven tables in ten were not found as tables at all; on PubTables-1M, a
//! third.
//!
//! Such a table states its structure in its text instead. Its rows are its
//! baselines, a label wrapped onto a second line excepted. Its columns are
//! the stripes of the page that its rows leave empty: figures set right and
//! labels set left start nowhere in common, but they share their gutters.
//! A label with no figures beside it names the rows under it. The header is
//! what sits above the first row of figures, and a header label that runs
//! across a gutter, or has a rule beneath it reaching across one, names
//! every column it covers.
//!
//! [`read`] reads one table from a region of the page that something else
//! has found: a grid ruled with horizontals only ([`unruled_grids`]), or a
//! run of figures nothing has claimed ([`figure_runs`]).

use crate::geom::BBox;
use crate::glyph::Glyph;
use crate::rule::{Orientation, Rule};
use crate::table::{Grid, TableLayout};

/// Gap, in font sizes, that parts two cells of a row (as in
/// `table::TABLE_CELL_GAP`); a word space is a third of this.
const CELL_GAP: f64 = 0.9;

/// Gap, in font sizes, above which two glyphs are separate words.
const WORD_GAP: f64 = 0.2;

/// Widest gap, in font sizes, between a figure and a currency sign set
/// after it as its own: a word space, breaking or not.
const SIGN_SPACE: f64 = 0.6;

/// Baseline agreement, in font sizes, for two glyphs to be on one line.
const BASELINE_TOLERANCE: f64 = 0.5;

/// Baseline distance, in font sizes, up to which a line continues the one
/// above it: a wrapped label, set at its paragraph's leading. A table's own
/// rows are set further apart, or the row beside it carries values.
const WRAP_LEADING: f64 = 1.35;

/// Baseline distance, in font sizes, up to which a line is the next one
/// under another: set at a body's leading, 1.2 to 1.5 of its size. A line
/// with a blank line between stands twice that below.
const NEXT_LINE: f64 = 1.6;

/// Vertical gap between consecutive lines, in font sizes, that ends a table.
const MAX_ROW_GAP: f64 = 2.6;

/// Share of the table's width a rule must run across to set off a band of
/// its rows: a section's rule, not a column group's.
const BAND_RULE_SPAN: f64 = 0.6;

/// Most words in a section's label: a band's name (`Adverse events`), not a
/// title or a note.
const SECTION_WORDS: usize = 6;

/// A section's label, by its text: a few words, no caption (`Table 5.` over
/// the next table sets off no band of this one).
fn section_label(text: &str) -> bool {
    text.split_whitespace().count() <= SECTION_WORDS && !is_caption(text)
}

/// Is the gap between two lines, `above` ending at `y0` and the next
/// starting at `y1`, one a table's rows run on across? A row gap; or, where
/// the next line is a section's label (`band` true), twice one with a rule
/// across the table, `x0..x1`, standing in it: a section band is set off by
/// a rule with a row's space on each side of it (`Withdrawal` over its rows,
/// under the rows before). Past a rule with no such label under it, the
/// table has ended: a page's foot under its last row.
fn row_gap(rules: &[Rule], x0: f64, x1: f64, y0: f64, y1: f64, fs: f64, band: bool) -> bool {
    let gap = y1 - y0;
    if gap <= fs * MAX_ROW_GAP {
        return true;
    }
    band && gap <= fs * MAX_ROW_GAP * 2.0
        && rules.iter().any(|r| {
            r.orientation == Orientation::Horizontal
                && r.axis_pos() > y0
                && r.axis_pos() < y1
                && r.bbox.x1.min(x1) - r.bbox.x0.max(x0) >= BAND_RULE_SPAN * (x1 - x0)
        })
}

/// Deepest header read.
const MAX_HEADER_LINES: usize = 5;

/// A run of glyphs on one line, parted from its neighbours by a cell gap.
#[derive(Debug, Clone)]
struct Chunk {
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
    text: String,
    bold: bool,
    /// Indices into the page's glyphs.
    glyphs: Vec<usize>,
    /// The page's space glyphs between this chunk and the next on its line.
    trailing: Vec<usize>,
    /// Where its dot leaders, dropped from its text, ran to: the next
    /// column's edge. Its ink's end where it has none.
    reach: f64,
    /// The chunk's words, each a chunk of its own with no words, so a chunk
    /// read across a gutter can be cut where its words part.
    words: Vec<Chunk>,
}

impl Chunk {
    fn width(&self) -> f64 {
        self.x1 - self.x0
    }
}

/// One baseline's worth of chunks, left to right.
#[derive(Debug, Clone)]
pub struct Line {
    base: f64,
    fs: f64,
    y0: f64,
    y1: f64,
    chunks: Vec<Chunk>,
}

/// The space between two glyphs set one after the other: from where the
/// pen left the first to where it set the second, or between their ink,
/// whichever is less. A narrow digit's ink starts well inside its advance,
/// and a gap read from ink alone parts `0.` from `17`.
fn gap(prev: &Glyph, next: &Glyph) -> f64 {
    let (p, n) = (prev.bbox.unwrap(), next.bbox.unwrap());
    let ink = n.x0 - p.x1;
    match prev.advance {
        Some(a) if a > 0.0 => ink.min(next.origin.0 - (prev.origin.0 + a)),
        _ => ink,
    }
}

/// A figure as tables print one: digits dressed in currency, grouping,
/// signs, parentheses and percent; a dash or `n/a` standing for none.
fn is_figure(text: &str) -> bool {
    // An amount with its ISO currency code: `GBP 3.29`, `3.29 EUR`.
    let t = text.trim();
    let code = |w: &str| w.len() == 3 && w.chars().all(|c| c.is_ascii_uppercase());
    if let Some((a, b)) = t.split_once(' ') {
        if (code(a) && !b.contains(' ') && is_figure(b))
            || (code(b) && !a.contains(' ') && is_figure(a))
        {
            return true;
        }
    }
    let s: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if s.is_empty() {
        return false;
    }
    if s.chars()
        .all(|c| matches!(c, '—' | '–' | '-' | '‒' | '―' | '−' | '%' | '$'))
    {
        return true;
    }
    let lower = s.to_lowercase();
    if matches!(
        lower.as_str(),
        "n/a" | "na" | "nm" | "n.m." | "nd" | "nr" | "ns" | "nil" | "*" | "**" | "***" | "-/-"
    ) {
        return true;
    }
    if !s.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    // Mostly digits and the signs that dress them; no word longer than a
    // unit's two letters (`5.3 mm`, `rs8008858`, `1.2e-5`).
    let numeric = |c: char| {
        c.is_ascii_digit()
            || matches!(
                c,
                '$' | '€'
                    | '£'
                    | '¥'
                    | '%'
                    | ','
                    | '.'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '-'
                    | '+'
                    | '−'
                    | '–'
                    | '—'
                    | '/'
                    | ':'
                    | '±'
                    | '<'
                    | '>'
                    | '≤'
                    | '≥'
                    | '~'
                    | '='
                    | '×'
                    | '*'
                    | '·'
                    | '\''
                    | '’'
            )
    };
    let mut run = 0usize;
    for c in s.chars() {
        if c.is_alphabetic() {
            run += 1;
            if run > 2 {
                return false;
            }
        } else {
            run = 0;
        }
    }
    let n = s.chars().count();
    s.chars().filter(|c| numeric(*c)).count() * 10 >= n * 6
}

fn is_currency(text: &str) -> bool {
    matches!(
        text.trim(),
        "$" | "€" | "£" | "¥" | "US$" | "C$" | "A$" | "Ps." | "R$"
    )
}

pub(crate) fn is_leader(text: &str) -> bool {
    matches!(text, "." | "·" | "…" | "․" | "‥")
}

/// Fewest dots in a run read as leaders inside a line: more than an
/// ellipsis's three.
pub(crate) const LEADER_DOTS: usize = 4;

/// The dots a glyph adds to a run of leaders: none where it is no leader,
/// and the run ends.
pub(crate) fn leader_dots(text: &str) -> usize {
    match text {
        "…" => 3,
        t if is_leader(t) => 1,
        _ => 0,
    }
}

/// A run of dot leaders read along a row, glyph by glyph. Leaders are set
/// close: a dot more than a size past the run's last starts a run of its
/// own, and two columns' `..` (a table's empty values) a gutter apart are
/// no leader.
#[derive(Default)]
pub(crate) struct LeaderRun {
    dots: usize,
    end: f64,
}

impl LeaderRun {
    /// Count `g` into the run, and say how many dots ran up to it.
    pub(crate) fn step(&mut self, g: &Glyph) -> usize {
        let before = self.dots;
        match leader_dots(g.text.trim()) {
            0 => self.dots = 0,
            n => {
                let fs = g.font_size.max(1.0) as f64;
                let (x0, x1) = g.bbox.map_or((g.origin.0, g.origin.0), |b| (b.x0, b.x1));
                self.dots = if self.dots > 0 && x0 - self.end > fs {
                    n
                } else {
                    self.dots + n
                };
                self.end = x1;
            }
        }
        before
    }

    /// Whether the run holds leaders now.
    pub(crate) fn leads(&self) -> bool {
        self.dots >= LEADER_DOTS
    }
}

/// A row, left to right, that is a contents entry: its last run of dot
/// leaders runs to a page number alone (`Scientific Method Lab ....... 6`).
/// A contents list is no table, and its leaders part nothing.
pub(crate) fn contents_entry(row: &[&Glyph]) -> bool {
    let mut run = LeaderRun::default();
    let mut after: Option<usize> = None;
    for (i, g) in row.iter().enumerate() {
        if run.step(g) >= LEADER_DOTS && leader_dots(g.text.trim()) == 0 {
            after = Some(i);
        }
    }
    let Some(start) = after else {
        return false;
    };
    let page: String = row[start..]
        .iter()
        .map(|g| g.text.trim())
        .collect::<String>();
    let n = page.chars().count();
    (1..=4).contains(&n) && page.chars().all(|c| c.is_ascii_digit())
        || (1..=6).contains(&n) && page.chars().all(|c| "ivxlc".contains(c))
}

/// The runs of text `lines` set, each as its box, its words and its line's
/// size: what is set together on a line, apart from what stands a gap away
/// (the gutter between a page's columns among them).
pub(crate) fn runs(lines: &[Line]) -> Vec<crate::table::TextRun> {
    lines
        .iter()
        .flat_map(|l| {
            l.chunks.iter().map(move |c| crate::table::TextRun {
                bbox: BBox {
                    x0: c.x0,
                    y0: c.y0,
                    x1: c.x1,
                    y1: c.y1,
                },
                text: c.text.clone(),
                size: l.fs,
            })
        })
        .collect()
}

/// The page's lines, each cut into chunks, with dot leaders dropped and a
/// currency sign set apart from its figure taken back in to it.
pub fn lines(glyphs: &[Glyph]) -> Vec<Line> {
    lines_where(glyphs, |_| true)
}

/// [`lines`] of the text set between `x0` and `x1` alone: one of the page's
/// columns, read as if the next column's text beside it were not there.
/// Chunks still name glyphs by their index in the page's glyphs.
pub fn lines_within(glyphs: &[Glyph], x0: f64, x1: f64) -> Vec<Line> {
    lines_where(glyphs, |g| {
        g.bbox
            .is_some_and(|b| (b.x0 + b.x1) / 2.0 >= x0 && (b.x0 + b.x1) / 2.0 < x1)
    })
}

fn lines_where(glyphs: &[Glyph], keep: impl Fn(&Glyph) -> bool) -> Vec<Line> {
    let index = |g: &Glyph| {
        (g as *const Glyph as usize - glyphs.as_ptr() as usize) / std::mem::size_of::<Glyph>()
    };
    let mut gs: Vec<&Glyph> = glyphs
        .iter()
        .filter(|g| g.bbox.is_some() && g.is_horizontal() && !g.text.trim().is_empty())
        .filter(|g| keep(g))
        .collect();
    gs.sort_by(|a, b| {
        a.origin
            .1
            .total_cmp(&b.origin.1)
            .then(a.origin.0.total_cmp(&b.origin.0))
    });
    let mut rows: Vec<Vec<&Glyph>> = Vec::new();
    for g in gs {
        let fs = g.font_size.max(1.0) as f64;
        match rows.last_mut() {
            Some(r) if (r[0].origin.1 - g.origin.1).abs() < fs * BASELINE_TOLERANCE => r.push(g),
            _ => rows.push(vec![g]),
        }
    }
    let spaces: Vec<usize> = (0..glyphs.len())
        .filter(|&i| {
            let g = &glyphs[i];
            !g.text.is_empty() && g.text.trim().is_empty() && g.is_horizontal()
        })
        .collect();
    let mut out = Vec::new();
    for mut row in rows {
        row.sort_by(|a, b| a.bbox.unwrap().x0.total_cmp(&b.bbox.unwrap().x0));
        let mut sizes: Vec<f64> = row.iter().map(|g| g.font_size as f64).collect();
        sizes.sort_by(f64::total_cmp);
        let fs = sizes[sizes.len() / 2].max(1.0);
        let mut groups: Vec<Vec<&Glyph>> = Vec::new();
        for g in row {
            match groups.last_mut() {
                Some(cur) if gap(cur.last().unwrap(), g) <= fs * CELL_GAP => cur.push(g),
                _ => groups.push(vec![g]),
            }
        }
        // Two figures a word space apart are two values: no figure holds a
        // space, save a currency sign's.
        let groups: Vec<Vec<&Glyph>> = groups
            .into_iter()
            .flat_map(|g| split_figures(g, fs))
            .collect();
        let mut chunks: Vec<Chunk> = Vec::new();
        for mut group in groups {
            // Dot leaders, trailing or alone: a run of three dots or more.
            let mut dots = 0usize;
            let mut cut = group.len();
            let reach = group.last().map_or(f64::MIN, |g| g.bbox.unwrap().x1);
            for (i, g) in group.iter().enumerate().rev() {
                if is_leader(g.text.trim()) {
                    dots += if g.text.trim() == "…" { 3 } else { 1 };
                    cut = i;
                } else {
                    break;
                }
            }
            if dots >= 3 {
                group.truncate(cut);
            }
            if group.is_empty() {
                continue;
            }
            let heavy = group
                .iter()
                .filter(|g| g.weight.unwrap_or(400) >= 600)
                .count();
            let bold = heavy * 2 > group.len();
            let mut text = String::new();
            let mut prev: Option<&Glyph> = None;
            let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
            let mut words: Vec<Chunk> = Vec::new();
            for g in &group {
                let b = g.bbox.unwrap();
                let parted = prev.is_some_and(|p| gap(p, g) > fs * WORD_GAP);
                if parted {
                    text.push(' ');
                }
                text.push_str(g.text.trim());
                prev = Some(g);
                x0 = x0.min(b.x0);
                x1 = x1.max(b.x1);
                y0 = y0.min(b.y0);
                y1 = y1.max(b.y1);
                match words.last_mut() {
                    Some(w) if !parted => {
                        w.text.push_str(g.text.trim());
                        w.glyphs.push(index(g));
                        w.x1 = w.x1.max(b.x1);
                        w.y0 = w.y0.min(b.y0);
                        w.y1 = w.y1.max(b.y1);
                    }
                    _ => words.push(Chunk {
                        x0: b.x0,
                        x1: b.x1,
                        y0: b.y0,
                        y1: b.y1,
                        text: g.text.trim().to_string(),
                        bold,
                        glyphs: vec![index(g)],
                        trailing: Vec::new(),
                        reach: b.x1,
                        words: Vec::new(),
                    }),
                }
            }
            chunks.push(Chunk {
                x0,
                x1,
                y0,
                y1,
                text,
                bold,
                glyphs: group.iter().map(|g| index(g)).collect(),
                trailing: Vec::new(),
                reach: reach.max(x1),
                words,
            });
        }
        // A sign a word space after a figure is that figure's where the row
        // sets its signs after its figures (`4,85 €`), as a sign that
        // prices no figure after it says. Elsewhere it is the next figure's.
        let after_figure = |i: usize| {
            i > 0
                && is_currency(&chunks[i].text)
                && is_figure(&chunks[i - 1].text)
                && chunks[i].x0 - chunks[i - 1].x1 <= fs * SIGN_SPACE
        };
        let prices_next = |i: usize| {
            chunks
                .get(i + 1)
                .is_some_and(|n| is_figure(&n.text) && n.x0 - chunks[i].x1 <= fs * 6.0)
        };
        let suffixed = (0..chunks.len()).any(|i| after_figure(i) && !prices_next(i));
        let suffix: Vec<bool> = (0..chunks.len())
            .map(|i| suffixed && after_figure(i))
            .collect();
        // `$` set apart from its figure is the figure's: `$ 10,441`.
        let mut merged: Vec<Chunk> = Vec::new();
        for (i, c) in chunks.into_iter().enumerate() {
            if let Some(prev) = merged.last_mut() {
                let joins = suffix[i]
                    || (is_currency(&prev.text)
                        && is_figure(&c.text)
                        && c.x0 - prev.x1 <= fs * 6.0)
                    || (c.text.trim() == "%"
                        && is_figure(&prev.text)
                        && c.x0 - prev.x1 <= fs * 2.0);
                if joins {
                    prev.text = format!("{} {}", prev.text, c.text);
                    prev.glyphs.extend(c.glyphs);
                    prev.words.extend(c.words);
                    prev.x1 = c.x1;
                    prev.reach = c.reach;
                    prev.y0 = prev.y0.min(c.y0);
                    prev.y1 = prev.y1.max(c.y1);
                    continue;
                }
            }
            merged.push(c);
        }
        if merged.is_empty() {
            continue;
        }
        // The page's own space glyphs between a chunk's ink are its words'
        // spaces: `SPF50 200ml`, not `SPF50200ml`.
        for c in merged.iter_mut() {
            let base = c.y1;
            for &i in &spaces {
                let g = &glyphs[i];
                let x = g.origin.0;
                if x > c.x0
                    && x < c.x1
                    && (g.origin.1 - base).abs() < fs * 0.6
                    && !c.glyphs.contains(&i)
                {
                    c.glyphs.push(i);
                    // A space inside a word's ink is that word's; one between
                    // words belongs to the word before it.
                    if let Some(w) = c.words.iter_mut().rev().find(|w| w.x0 <= x) {
                        w.glyphs.push(i);
                    }
                }
            }
        }
        for i in 1..merged.len() {
            let (a, b) = (merged[i - 1].x1, merged[i].x0);
            let base = merged[i].y1;
            let between: Vec<usize> = spaces
                .iter()
                .copied()
                .filter(|&k| {
                    let g = &glyphs[k];
                    g.origin.0 >= a - 0.5
                        && g.origin.0 <= b + 0.5
                        && (g.origin.1 - base).abs() < fs * 0.6
                })
                .collect();
            merged[i - 1].trailing = between;
        }
        let base = row_base(&merged);
        out.push(Line {
            base,
            fs,
            y0: merged.iter().map(|c| c.y0).fold(f64::MAX, f64::min),
            y1: merged.iter().map(|c| c.y1).fold(f64::MIN, f64::max),
            chunks: merged,
        });
    }
    out
}

/// A run of glyphs cut at each word gap that parts two figures, or a figure
/// from the currency sign of the next.
fn split_figures(group: Vec<&Glyph>, fs: f64) -> Vec<Vec<&Glyph>> {
    let mut words: Vec<Vec<&Glyph>> = Vec::new();
    for g in group {
        match words.last_mut() {
            Some(w) if gap(w.last().unwrap(), g) <= fs * WORD_GAP => w.push(g),
            _ => words.push(vec![g]),
        }
    }
    let text = |w: &Vec<&Glyph>| w.iter().map(|g| g.text.trim()).collect::<String>();
    let mut out: Vec<Vec<&Glyph>> = Vec::new();
    let mut prev: Option<String> = None;
    for w in words {
        let t = text(&w);
        let cut = prev.as_deref().is_some_and(|p| {
            is_figure(p)
                && !is_currency(p)
                && !p.chars().any(char::is_alphabetic)
                && p.chars()
                    .any(|c| c.is_ascii_digit() || c == '—' || c == '–')
                && (is_currency(&t) || (is_figure(&t) && t != "%" && !t.starts_with(['.', ','])))
                // A day and its year: `Oct 1 2026`, `1, 2026`.
                && !(is_year(&t)
                    && p.trim_end_matches(',').parse::<u32>().is_ok_and(|d| (1..=31).contains(&d)))
        });
        match out.last_mut() {
            Some(cur) if !cut => cur.extend(w),
            _ => out.push(w),
        }
        prev = Some(t);
    }
    out
}

fn row_base(chunks: &[Chunk]) -> f64 {
    chunks.iter().map(|c| c.y1).fold(f64::MIN, f64::max)
}

/// The part of a line inside `[x0, x1]`, by chunk centre.
fn clip(line: &Line, x0: f64, x1: f64) -> Option<Line> {
    let chunks: Vec<Chunk> = line
        .chunks
        .iter()
        .filter(|c| {
            let m = (c.x0 + c.x1) / 2.0;
            m >= x0 && m <= x1
        })
        .cloned()
        .collect();
    if chunks.is_empty() {
        return None;
    }
    Some(Line {
        base: line.base,
        fs: line.fs,
        y0: chunks.iter().map(|c| c.y0).fold(f64::MAX, f64::min),
        y1: chunks.iter().map(|c| c.y1).fold(f64::MIN, f64::max),
        chunks,
    })
}

/// Whether a vertical rule stands inside the box: then the table is ruled,
/// and its columns are the ruling's.
pub fn has_vertical(rules: &[Rule], b: &BBox) -> bool {
    rules.iter().any(|r| {
        r.orientation == Orientation::Vertical
            && r.bbox.x0 > b.x0 + 2.0
            && r.bbox.x1 < b.x1 - 2.0
            && r.bbox.y1 > b.y0 + 2.0
            && r.bbox.y0 < b.y1 - 2.0
            && r.length() > 6.0
    })
}

/// Columns as the stripes the rows leave empty: each column's ink extent,
/// left to right. A stripe a few rows cross (a spanning label) is still a
/// gutter; a column of currency signs belongs to the figures beside it.
fn columns(lines: &[&Line], x0: f64, x1: f64) -> Vec<(f64, f64)> {
    let used: Vec<&&Line> = lines.iter().filter(|l| l.chunks.len() >= 2).collect();
    let used: Vec<&&Line> = if used.is_empty() {
        lines.iter().collect()
    } else {
        used
    };
    if used.is_empty() || x1 <= x0 {
        return Vec::new();
    }
    let step = 0.5;
    let n = ((x1 - x0) / step).ceil() as usize + 1;
    let mut count = vec![0usize; n];
    for l in &used {
        let mut mark = vec![false; n];
        for c in &l.chunks {
            let a = (((c.x0 - x0) / step).floor().max(0.0) as usize).min(n - 1);
            let b = (((c.x1 - x0) / step).ceil().max(0.0) as usize).min(n - 1);
            for m in mark.iter_mut().take(b + 1).skip(a) {
                *m = true;
            }
        }
        for (k, m) in mark.iter().enumerate() {
            count[k] += usize::from(*m);
        }
    }
    let threshold = used.len() / 8;
    let fs = used.iter().map(|l| l.fs).fold(0.0, f64::max).max(1.0);
    let min_gutter = ((fs * 0.3) / step).ceil() as usize;
    let mut cols: Vec<(f64, f64, usize)> = Vec::new();
    let mut k = 0;
    while k < n {
        if count[k] <= threshold {
            k += 1;
            continue;
        }
        let start = k;
        let mut peak = 0;
        let mut end = k;
        // Walk to the next gutter at least `min_gutter` wide.
        while k < n {
            if count[k] > threshold {
                peak = peak.max(count[k]);
                end = k;
                k += 1;
                continue;
            }
            let mut g = k;
            while g < n && count[g] <= threshold {
                g += 1;
            }
            if g - k >= min_gutter || g >= n {
                break;
            }
            k = g;
        }
        cols.push((x0 + start as f64 * step, x0 + end as f64 * step, peak));
        k = end + 1;
    }
    // A stripe only one row inks is no column.
    cols.retain(|c| c.2 >= 2 || used.len() < 3);
    // Currency signs set at the column's left edge, apart from figures of
    // every width, are the next column's.
    let only_currency = |a: f64, b: f64| {
        let inside: Vec<&Chunk> = used
            .iter()
            .flat_map(|l| l.chunks.iter())
            .filter(|c| c.x0 >= a - 0.5 && c.x1 <= b + 0.5)
            .collect();
        !inside.is_empty() && inside.iter().all(|c| is_currency(&c.text))
    };
    let mut out: Vec<(f64, f64)> = Vec::new();
    let mut pending: Option<f64> = None;
    for (a, b, _) in cols {
        if only_currency(a, b) {
            pending = Some(pending.unwrap_or(a));
            continue;
        }
        out.push((pending.take().unwrap_or(a), b));
    }
    out
}

fn column_of(cols: &[(f64, f64)], c: &Chunk) -> Option<usize> {
    let m = (c.x0 + c.x1) / 2.0;
    // The column whose extent holds the centre, else the nearest.
    if let Some(k) = cols.iter().position(|&(a, b)| m >= a && m <= b) {
        return Some(k);
    }
    cols.iter()
        .enumerate()
        .min_by(|x, y| {
            let d = |&(a, b): &(f64, f64)| if m < a { a - m } else { m - b };
            d(x.1).total_cmp(&d(y.1))
        })
        .map(|(k, _)| k)
}

/// Columns a chunk covers: those whose extent it overlaps by a third of the
/// narrower of the two.
fn covered(cols: &[(f64, f64)], c: &Chunk) -> Vec<usize> {
    cols.iter()
        .enumerate()
        .filter(|(_, &(a, b))| {
            let o = c.x1.min(b) - c.x0.max(a);
            o > 0.0 && o >= ((b - a).min(c.width())) / 3.0
        })
        .map(|(k, _)| k)
        .collect()
}

/// A line that sits in the table's columns: each chunk within one column.
fn fits(cols: &[(f64, f64)], l: &Line) -> bool {
    !cols.is_empty() && l.chunks.iter().all(|c| covered(cols, c).len() <= 1)
}

/// A line with each chunk that runs across a gutter cut where a figure
/// starts inside it. A label long enough to bring its figure within a cell
/// gap reads as one chunk with it (`montmorillonite/smectite 100`), which no
/// column holds, and the table's rows broke at it. Only a figure ending the
/// line is cut off, or one filling a column its line leaves empty: a
/// banner's words across the gutters it spans stay one label. A cut is kept
/// only where every piece then sits in one column. `held` counts the
/// region's lines with a chunk in each column.
fn split_at_gutters(cols: &[(f64, f64)], held: &[usize], l: &Line) -> Line {
    let mut chunks: Vec<Chunk> = Vec::new();
    for (i, c) in l.chunks.iter().enumerate() {
        // The line's last chunk: its figure is then the row's value. A run of
        // prose ending in a figure (a footnote under the table) is no label
        // and its value; cut, it would fit the columns as a row.
        // Or a chunk further in whose cut piece would fill a column the rest
        // of its line leaves empty: a label set close to its first figure
        // (`LOC440055 0.721`), the row's other values in the columns after.
        // A label with its row's value set after it keeps a figure of its own
        // (`Antihistamine tablets (30) | 30.80`): the value fills the column.
        // Only a column the table's rows fill, three lines at least and half
        // as many as its fullest column (wrapped lines in other columns
        // aside): a receipt's amounts, set where each line's text ends, make
        // columns of a line or two, and a label's own figure (`Antihistamine
        // tablets (30)`) falls in one of them.
        let fullest = held.iter().copied().max().unwrap_or(0);
        let fills_empty = |piece: &Chunk| {
            column_of(cols, piece).is_some_and(|k| {
                let filled = held.get(k).copied().unwrap_or(0);
                filled >= 3
                    && filled * 2 >= fullest
                    && !l
                        .chunks
                        .iter()
                        .enumerate()
                        .any(|(j, d)| j != i && covered(cols, d).contains(&k))
            })
        };
        let last = i + 1 == l.chunks.len();
        if covered(cols, c).len() <= 1 || c.words.len() < 2 || c.words.len() > MAX_CUT_WORDS {
            chunks.push(c.clone());
            continue;
        }
        let mut pieces: Vec<Vec<&Chunk>> = vec![vec![&c.words[0]]];
        for w in &c.words[1..] {
            let before = *pieces.last().unwrap().last().unwrap();
            let cut = cols.windows(2).any(|p| {
                let (gutter_x0, gutter_x1) = (p[0].1, p[1].0);
                before.x1 <= gutter_x1 + 0.5
                    && w.x0 >= gutter_x0 - 0.5
                    && column_of(cols, before) != column_of(cols, w)
                    && (is_figure(&w.text) || is_currency(&w.text))
                    && !date_part(before, w)
            });
            if cut {
                pieces.push(vec![w]);
            } else {
                pieces.last_mut().unwrap().push(w);
            }
        }
        let n = pieces.len();
        let cut: Vec<Chunk> = pieces
            .into_iter()
            .enumerate()
            .map(|(i, ws)| Chunk {
                x0: ws.iter().map(|w| w.x0).fold(f64::MAX, f64::min),
                x1: ws.iter().map(|w| w.x1).fold(f64::MIN, f64::max),
                y0: ws.iter().map(|w| w.y0).fold(f64::MAX, f64::min),
                y1: ws.iter().map(|w| w.y1).fold(f64::MIN, f64::max),
                text: ws
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                bold: c.bold,
                glyphs: ws.iter().flat_map(|w| w.glyphs.iter().copied()).collect(),
                trailing: if i + 1 == n {
                    c.trailing.clone()
                } else {
                    Vec::new()
                },
                reach: if i + 1 == n {
                    c.reach
                } else {
                    ws.iter().map(|w| w.x1).fold(f64::MIN, f64::max)
                },
                words: ws.into_iter().cloned().collect(),
            })
            .collect();
        if cut.len() > 1
            && cut.iter().all(|p| covered(cols, p).len() <= 1)
            && (last || cut[1..].iter().all(&fills_empty))
        {
            chunks.extend(cut);
        } else {
            chunks.push(c.clone());
        }
    }
    Line {
        chunks,
        ..l.clone()
    }
}

/// Most words in a chunk cut at a gutter: a label and its figure, not a
/// sentence (`montmorillonite/smectite 100`, not a footnote's line).
const MAX_CUT_WORDS: usize = 8;

/// Most words a stub's head may hold over a table read from its text: it
/// names what the rows are, and runs to a title's length at most.
const MAX_STUB_HEAD_WORDS: usize = 12;

/// Whether text is set as a title: two in three of its words of four
/// letters or more capitalised, and no stop, semicolon, colon or bracketed
/// citation in it. Sentence case and a sentence's stops are prose's.
fn set_as_title(text: &str) -> bool {
    let long: Vec<&str> = text
        .split_whitespace()
        .filter(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 4)
        .collect();
    let capitalised = long
        .iter()
        .filter(|w| {
            w.chars()
                .find(|c| c.is_alphabetic())
                .is_some_and(char::is_uppercase)
        })
        .count();
    !long.is_empty() && capitalised * 3 >= long.len() * 2 && !text.contains(['.', ';', ':', '['])
}

/// A word that is part of a date with the word before it: a year, or a day
/// after a month's name (`December 31, 2016`), which a banner ends on.
fn date_part(before: &Chunk, w: &Chunk) -> bool {
    let t = w.text.trim();
    if is_year(t) {
        return true;
    }
    // The word before ends on a month's name, full or short, a space lost
    // before it or not (`As ofDecember 31, 2015`).
    let month = before.text.trim().trim_end_matches('.').to_lowercase();
    let months = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
        "jan",
        "feb",
        "mar",
        "apr",
        "jun",
        "jul",
        "aug",
        "sep",
        "sept",
        "oct",
        "nov",
        "dec",
    ];
    t.trim_end_matches(',')
        .parse::<u32>()
        .is_ok_and(|d| (1..=31).contains(&d))
        && months.iter().any(|m| month.ends_with(m))
}

fn has_figures(cols: &[(f64, f64)], l: &Line) -> bool {
    l.chunks.iter().any(|c| {
        is_figure(&c.text) && !is_year(&c.text) && column_of(cols, c).is_some_and(|k| k > 0)
    })
}

/// The tables in the lines of `region`: each run of rows carrying values
/// in the region's columns, with the header over it.
pub fn read(all: &[Line], rules: &[Rule], region: BBox, page: usize) -> Vec<Grid> {
    read_beside(all, rules, region, page, false)
}

/// Whether the bands between a stack's row lines (at `ys`, down the page)
/// hold text, three in four of them or more: a table ruled under its rows
/// sets a row in every band, a blank row or two aside, where a chart's
/// gridlines stand over its bars and a drawing's lines over nothing.
pub fn bands_hold_text(all: &[Line], region: BBox, ys: &[f64]) -> bool {
    let bands = ys.len().saturating_sub(1);
    let held = ys
        .windows(2)
        .filter(|w| {
            all.iter().any(|l| {
                l.base > w[0]
                    && l.base < w[1]
                    && l.chunks
                        .iter()
                        .any(|c| c.x0 >= region.x0 - 1.0 && c.x1 <= region.x1 + 1.0)
            })
        })
        .count();
    bands > 0 && held * 4 >= bands * 3
}

/// [`read`], for a region a stack of unbroken row lines rules: its labels
/// are never text that stands under another table's rules, rules that cover
/// it and stop short of the region. A table set beside this one in the
/// page's next column draws its row lines across its own width, and its last
/// column is no label column of this one.
pub fn read_ruled_stack(all: &[Line], rules: &[Rule], region: BBox, page: usize) -> Vec<Grid> {
    read_beside(all, rules, region, page, true)
}

fn read_beside(
    all: &[Line],
    rules: &[Rule],
    region: BBox,
    page: usize,
    own_rules: bool,
) -> Vec<Grid> {
    let fs = all
        .iter()
        .filter(|l| l.base >= region.y0 && l.y0 <= region.y1)
        .map(|l| l.fs)
        .fold(0.0, f64::max);
    if fs <= 0.0 {
        return Vec::new();
    }
    // The region's lines, the label column taken in on the left: the chunk
    // nearest the region on each of its lines, where those agree.
    let in_band = |l: &Line| l.base >= region.y0 - fs * 0.3 && l.y0 <= region.y1 + fs * 0.3;
    // A rule standing between a chunk and the figures, running far past
    // them both ways, divides them: the page's column divider. What lies
    // across it is the next column's text, not these figures' labels. A
    // table's own column rules stand between its labels and its figures
    // too, but run no further than the table.
    let divided = |l: &Line, c: &Chunk, right: f64| {
        rules.iter().any(|r| {
            r.orientation == crate::rule::Orientation::Vertical
                && r.axis_pos() > c.x1
                && r.axis_pos() < right
                && r.bbox.y0 <= l.y0
                && r.bbox.y1 >= l.base
                && r.bbox.y1 - r.bbox.y0 >= 2.0 * (region.y1 - region.y0)
        })
    };
    // In a stack's region, text under another table's rules is that
    // table's ([`read_ruled_stack`]).
    let ruled_elsewhere = |c: &Chunk| {
        own_rules
            && rules.iter().any(|r| {
                r.orientation == crate::rule::Orientation::Horizontal
                    && r.bbox.x0 <= c.x0
                    && r.bbox.x1 >= c.x1
                    && r.bbox.x1 < region.x0 - fs * 0.5
                    && r.bbox.y0 >= region.y0 - fs
                    && r.bbox.y0 <= region.y1 + fs
            })
    };
    let mut lefts: Vec<f64> = all
        .iter()
        .filter(|l| in_band(l))
        .filter_map(|l| {
            l.chunks
                .iter()
                .filter(|c| c.x1 <= region.x0 + fs && c.x0 < region.x0 - fs)
                .filter(|c| !divided(l, c, region.x0))
                .filter(|c| !ruled_elsewhere(c))
                .max_by(|a, b| a.x1.total_cmp(&b.x1))
                // A statement sets its labels at the margin and its figures
                // at the far side of the page, with or without leaders.
                .filter(|c| region.x0 - c.x1 < (region.x1 - region.x0).max(fs * 40.0))
                .map(|c| c.x0)
        })
        .collect();
    lefts.sort_by(f64::total_cmp);
    // Only a region of figures alone lacks its labels: where the region's
    // rows start with a label, its rules already cover the label column.
    let starts_with_figures = {
        let firsts: Vec<bool> = all
            .iter()
            .filter(|l| in_band(l))
            .filter_map(|l| {
                l.chunks
                    .iter()
                    .find(|c| (c.x0 + c.x1) / 2.0 >= region.x0 && (c.x0 + c.x1) / 2.0 <= region.x1)
                    .map(|c| is_figure(&c.text) || is_currency(&c.text))
            })
            .collect();
        firsts.iter().filter(|f| **f).count() * 2 > firsts.len()
    };
    let mut x0 = region.x0;
    if starts_with_figures && lefts.len() >= 2 {
        let median = lefts[lefts.len() / 2];
        if let Some(&l) = lefts.iter().find(|&&l| l >= median - fs * 6.0) {
            x0 = x0.min(l - 0.5);
        }
        // Further columns of labels set close to the left (a code beside a
        // description), where half the region's lines have one.
        let lines_in: Vec<&Line> = all.iter().filter(|l| in_band(l)).collect();
        for _ in 0..4 {
            let picked: Vec<(&Line, &Chunk)> = lines_in
                .iter()
                .filter_map(|l| {
                    // A label's leaders run up to the column beside it, and
                    // it reaches as far as they do.
                    l.chunks
                        .iter()
                        .filter(|c| c.x1 <= x0 + 0.5 && x0 - c.reach.min(x0) < fs * 8.0)
                        .filter(|c| !divided(l, c, region.x0))
                        .filter(|c| !ruled_elsewhere(c))
                        .max_by(|a, b| a.x1.total_cmp(&b.x1))
                        .map(|c| (*l, c))
                })
                .collect();
            if picked.len() * 2 < lines_in.len() || picked.len() < 2 {
                break;
            }
            // Prose set in the page's next column over runs on from line to
            // line, across a gutter narrower than the reach here; a column of
            // codes beside descriptions starts each line afresh.
            let ran = picked
                .windows(2)
                .filter(|w| {
                    let ((a, ca), (b, cb)) = (w[0], w[1]);
                    b.base - a.base <= a.fs.max(b.fs) * NEXT_LINE && runs_on(ca, cb)
                })
                .count();
            if ran >= 2 && ran * 2 >= picked.len() - 1 {
                break;
            }
            let mut more: Vec<f64> = picked.iter().map(|(_, c)| c.x0).collect();
            more.sort_by(f64::total_cmp);
            let median = more[more.len() / 2];
            match more.iter().find(|&&l| l >= median - fs * 6.0) {
                Some(&l) if l < x0 - 1.0 => x0 = l - 0.5,
                _ => break,
            }
        }
    }
    let mut x1 = region.x1;
    // A header label wider than its figures hangs past the last column.
    {
        for l in all
            .iter()
            .filter(|l| l.base < region.y0 && l.base >= region.y0 - fs * 4.0)
        {
            for c in &l.chunks {
                if c.x0 < x1 && c.x1 > x1 && (x1 - c.x0) >= 0.3 * c.width() && c.x1 - x1 < fs * 2.5
                {
                    x1 = c.x1 + 0.5;
                }
            }
        }
    }
    // A frame of full-width rules over and under the region is the table's
    // width: a journal table's top and bottom rules.
    let frame: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.orientation == Orientation::Horizontal)
        .filter(|r| r.bbox.x0 <= region.x0 + fs && r.bbox.x1 >= region.x1 - fs)
        .filter(|r| r.bbox.y0 >= region.y0 - fs * 6.0 && r.bbox.y0 <= region.y1 + fs * 3.0)
        .collect();
    if frame.iter().any(|r| r.bbox.y0 <= region.y0 + fs)
        && frame.iter().any(|r| r.bbox.y0 >= region.y1 - fs)
    {
        x0 = x0.min(frame.iter().map(|r| r.bbox.x0).fold(f64::MAX, f64::min) - 0.5);
        x1 = x1.max(frame.iter().map(|r| r.bbox.x1).fold(f64::MIN, f64::max) + 0.5);
    }
    let clipped: Vec<Line> = all.iter().filter_map(|l| clip(l, x0, x1)).collect();
    let band: Vec<usize> = (0..clipped.len())
        .filter(|&i| in_band(&clipped[i]))
        .collect();
    if band.len() < 2 {
        return Vec::new();
    }
    // Columns from the rows that end in figures, else from every row of two
    // cells or more.
    let valued: Vec<&Line> = band
        .iter()
        .map(|&i| &clipped[i])
        .filter(|l| {
            l.chunks.len() >= 2
                && l.chunks[1..]
                    .iter()
                    .any(|c| is_figure(&c.text) && !is_year(&c.text))
        })
        .collect();
    let cols0 = if valued.len() >= 2 {
        columns(&valued, x0, x1)
    } else {
        let ls: Vec<&Line> = band.iter().map(|&i| &clipped[i]).collect();
        columns(&ls, x0, x1)
    };
    if cols0.len() < 2 {
        return Vec::new();
    }
    // How many of the region's lines set a chunk in each column: a column
    // the table's rows fill, or one a line or two happen to.
    let held: Vec<usize> = (0..cols0.len())
        .map(|k| {
            band.iter()
                .filter(|&&i| clipped[i].chunks.iter().any(|c| covered(&cols0, c) == [k]))
                .count()
        })
        .collect();
    let clipped: Vec<Line> = clipped
        .iter()
        .map(|l| split_at_gutters(&cols0, &held, l))
        .collect();
    let value_row = |l: &Line| fits(&cols0, l) && l.chunks.len() >= 2 && has_figures(&cols0, l);
    // A line ruled off above and below, as the rows around it are, holding
    // its label and a value in the columns: a row whatever its value says
    // (`Packaging (W x H x D) | 31.4 cm x 10.9 cm x 15.7 cm` among lengths
    // and weights). Rules across most of the region, a line's height or so
    // from it each side.
    let row_rules: Vec<f64> = rules
        .iter()
        .filter(|r| {
            r.orientation == Orientation::Horizontal
                && r.bbox.x1.min(x1) - r.bbox.x0.max(x0) >= 0.3 * (x1 - x0)
        })
        .map(|r| r.axis_pos())
        .collect();
    let boxed = |l: &Line| {
        row_rules
            .iter()
            .any(|&y| y <= l.y0 + 1.0 && l.y0 - y <= fs * 1.5)
            && row_rules
                .iter()
                .any(|&y| y >= l.y1 - 1.0 && y - l.y1 <= fs * 1.5)
    };
    // Only after a row ruled off in the same way: a table ruled row by row.
    // A header ruled above and below, under a table ruled only round its
    // header, is another table's. Its values carry a measure, not only names
    // or years: a header ruled off as its rows are (`(millions) | 2017 |
    // 2016`) starts the next table.
    let ruled_row = |l: &Line, prev: &Line| {
        let values: Vec<&Chunk> = l
            .chunks
            .iter()
            .filter(|c| column_of(&cols0, c).is_some_and(|k| k > 0))
            .collect();
        boxed(prev)
            && boxed(l)
            && fits(&cols0, l)
            && l.chunks.len() >= 2
            && l.chunks.iter().any(|c| column_of(&cols0, c) == Some(0))
            && values
                .iter()
                .any(|c| c.text.chars().any(|ch| ch.is_ascii_digit()))
            && !values.iter().all(|c| is_year(&c.text))
    };
    let label_row = |l: &Line| {
        l.chunks.len() == 1
            && column_of(&cols0, &l.chunks[0]) == Some(0)
            && covered(&cols0, &l.chunks[0]).len() <= 1
    };
    // A section's label in the label column, as [`section_label`] reads one.
    let section = |l: &Line| label_row(l) && section_label(&l.chunks[0].text);
    let running = {
        let ls: Vec<&Line> = band.iter().map(|&i| &clipped[i]).collect();
        running_columns(&cols0, &ls)
    };
    // Runs of value rows, up to three label lines between them.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut k = 0;
    while k < band.len() {
        let i = band[k];
        if !value_row(&clipped[i]) {
            k += 1;
            continue;
        }
        let (start, mut end) = (i, i);
        let mut j = k + 1;
        let mut labels = 0;
        while j < band.len() {
            let l = &clipped[band[j]];
            if !row_gap(
                rules,
                x0,
                x1,
                clipped[band[j - 1]].y1,
                l.y0,
                fs,
                label_row(l),
            ) {
                break;
            }
            if value_row(l)
                || ruled_row(l, &clipped[band[j - 1]])
                || carries_on(
                    &cols0,
                    &running,
                    &clipped[band[j - 1]],
                    l,
                    band.get(j + 1).map(|&n| &clipped[n]),
                )
            {
                end = band[j];
                labels = 0;
            } else if label_row(l) && labels < 3 {
                labels += 1;
            } else {
                break;
            }
            j += 1;
        }
        runs.push((start, end));
        k = band.iter().position(|&b| b == end).unwrap() + 1;
    }
    // A table set in blocks, each under its own title and opening on the
    // first block's line of column names again (`<16 | 16/17 | 18/19 | <20`
    // under `Bolivia 1994 | Bolivia 2008`, and again under `Colombia 1990 |
    // Colombia 2010`): one run, its blocks read in [`build`]. The title is a
    // header's depth of lines at most, none of them a row
    // ([`titles_a_block`]), set apart as rows are.
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    for (start, end) in runs {
        if let Some(prev) = blocks.last_mut() {
            let titles = prev.1 + 1..start;
            if (1..=MAX_HEADER_LINES).contains(&titles.len())
                && titles.clone().all(|i| {
                    titles_a_block(&cols0, &clipped[i])
                        && clipped[i]
                            .chunks
                            .iter()
                            .any(|c| c.text.chars().filter(|ch| ch.is_alphabetic()).count() >= 2)
                })
                && (prev.1 + 1..=start)
                    .all(|i| row_gap(rules, x0, x1, clipped[i - 1].y1, clipped[i].y0, fs, false))
                && same_cells(&cols0, &clipped[prev.0], &clipped[start])
            {
                prev.1 = end;
                continue;
            }
        }
        blocks.push((start, end));
    }
    let runs = blocks;
    let mut out = Vec::new();
    let mut floor = 0usize;
    if std::env::var_os("FDOC_TEXTGRID_DEBUG").is_some() {
        eprintln!(
            "TEXTGRID read x {x0:.0}..{x1:.0} cols0 {cols0:?} runs {runs:?} band {}",
            band.len()
        );
    }
    for (start, end) in runs {
        // Down past the region: rows that keep to the columns.
        let mut last = end;
        while last + 1 < clipped.len() {
            let next = &clipped[last + 1];
            if !row_gap(
                rules,
                x0,
                x1,
                clipped[last].y1,
                next.y0,
                fs,
                label_row(next),
            ) {
                break;
            }
            if value_row(next)
                || ruled_row(next, &clipped[last])
                || carries_on(
                    &cols0,
                    &running,
                    &clipped[last],
                    next,
                    clipped.get(last + 2),
                )
            {
                last += 1;
                continue;
            }
            // Up to three label lines, then a row of values.
            let mut j = last + 1;
            while j < clipped.len() && j <= last + 3 && label_row(&clipped[j]) {
                j += 1;
            }
            let resumes = j > last + 1
                && clipped.get(j).is_some_and(|n| {
                    row_gap(rules, x0, x1, clipped[j - 1].y1, n.y0, fs, false) && value_row(n)
                })
                && (last + 1..j).all(|i| {
                    let l = &clipped[i];
                    row_gap(rules, x0, x1, clipped[i - 1].y1, l.y0, fs, section(l))
                });
            if !resumes {
                break;
            }
            last = j;
        }
        // Up: label lines and header lines over the columns, as far as the
        // second rule across the table (the first is the header's own), a
        // caption, or prose.
        let width = x1 - x0;
        let across: Vec<f64> = {
            let mut ys: Vec<f64> = rules
                .iter()
                .filter(|r| r.orientation == Orientation::Horizontal && r.length() >= 0.6 * width)
                .filter(|r| r.bbox.x0 <= x0 + width * 0.4 && r.bbox.x1 >= x1 - width * 0.4)
                // A rule under a header's groups, over the columns past the
                // stub and short of it, is the groups' underline, not one of
                // the table's own rules, which run across its stub too.
                .filter(|r| cols0.first().is_none_or(|c| r.bbox.x0 < c.1))
                .map(|r| r.axis_pos())
                .collect();
            ys.sort_by(f64::total_cmp);
            ys.dedup_by(|a, b| (*a - *b).abs() <= 3.0);
            ys
        };
        // A header framed by rules across the table: one over its first row
        // (label lines aside), one over the header, and between them only
        // header lines, no row of figures and no caption. Inside the frame
        // the walk goes on across a rule (`Pathology | Radiology` ruled off
        // under `Preparation (h)`), across a wide gap (a drug's name and
        // `Number of` set well above `Trials | Patients`), and past its depth;
        // what it takes and where else it stops is the walk's, as before.
        let framed: Option<usize> = {
            let headerish = |l: &Line| {
                let text: String = l
                    .chunks
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                // Labels are a few words, as a section's are; a line ending
                // on a colon introduces what follows, as prose does.
                !is_caption(&text)
                    && l.chunks.iter().all(|c| {
                        c.text.split_whitespace().count() <= SECTION_WORDS
                            && !c.text.trim_end().ends_with(':')
                    })
                    && (!value_row(l)
                        || l.chunks.iter().all(|c| {
                            !is_figure(&c.text)
                                || is_year(&c.text)
                                || is_unit_head(&c.text)
                                || is_sample_size(&c.text)
                        }))
            };
            let ruled =
                |y0: f64, y1: f64| across.iter().copied().filter(move |&r| r > y0 && r < y1);
            // Under the last header line's baseline: brackets descend past a
            // rule drawn close under them (`(95% CI)`).
            let under = |k: usize| ruled(clipped[k - 1].base, clipped[k].y0 + 1.0).next_back();
            // The rule over the first row, label lines between them: the
            // titles of the body's first sections (`Operating activities`), as
            // short as a header's lines. A longer title there (`Selected
            // information for plans with liability for service`) is a
            // block's own, and the walk reads it and what is over it.
            let mut lo = start;
            while lo > floor && label_row(&clipped[lo - 1]) {
                lo -= 1;
            }
            let h = (lo > floor && (lo..start).all(|k| headerish(&clipped[k])))
                .then(|| under(lo))
                .flatten();
            h.and_then(|h| {
                // Up from rule to rule while each band between holds header
                // lines only. The stub holds the header's own head once: a
                // second band with words in the stub is a row ruled off as
                // the rows below it are (`Gender | Male | Female` over `Age
                // at diagnosis | 22 months | 4 years`). A head wrapped across
                // a rule goes on in lower case (`Registration` over `period`)
                // and is still the one head.
                let mut top = None;
                let mut bottom = h;
                let mut i = lo;
                let mut stubbed = 0usize;
                while let Some(r) = across.iter().copied().rfind(|&r| r < bottom - 1.0) {
                    let mut j = i;
                    while j > floor && clipped[j - 1].y0 > r - 1.0 {
                        j -= 1;
                    }
                    if j == i || !(j..i).all(|k| headerish(&clipped[k])) {
                        break;
                    }
                    if (j..i).any(|k| {
                        clipped[k].chunks.iter().any(|c| {
                            column_of(&cols0, c) == Some(0)
                                && !c.text.trim_start().starts_with(char::is_lowercase)
                        })
                    }) {
                        stubbed += 1;
                        if stubbed > 1 {
                            break;
                        }
                    }
                    top = Some(j);
                    bottom = r;
                    i = j;
                }
                top
            })
        };
        let mut crossed = 0usize;
        // Inside the frame, the line `k` is the header's whatever the
        // walk's limits say: no rule across, gap or depth ends it there.
        let framed_at = |k: usize| framed.is_some_and(|t| k >= t);
        let mut first = start;
        let mut header_seen = 0usize;
        let mut header_named = false;
        // Inside the frame the header's depth counts its levels, a label
        // wrapped over several lines one level (`NNT/` over `NNTp/`, `NNH`
        // and `(95% CI)`): rows of words with no figures stop it as before.
        let mut levels = 0usize;
        let wraps_onto = |above: &Line, cur: &Line| {
            cur.base - above.base <= cur.fs.max(above.fs) * WRAP_LEADING * 1.2
                && cur.chunks.iter().all(|c| {
                    above
                        .chunks
                        .iter()
                        .any(|a| column_of(&cols0, a) == column_of(&cols0, c))
                })
        };
        while first > floor
            && if framed_at(first - 1) {
                levels < MAX_HEADER_LINES
            } else {
                header_seen < MAX_HEADER_LINES
            }
        {
            let above = &clipped[first - 1];
            let cur = &clipped[first];
            if !row_gap(rules, x0, x1, above.y1, cur.y0, fs, section(cur)) && !framed_at(first - 1)
            {
                break;
            }
            if std::env::var_os("FDOC_TEXTGRID_DEBUG").is_some() {
                eprintln!(
                    "  walk above {:?}",
                    above
                        .chunks
                        .iter()
                        .map(|c| (c.text.as_str(), c.x0 as i32, c.x1 as i32))
                        .collect::<Vec<_>>()
                );
            }
            let between = across
                .iter()
                .filter(|&&y| y > above.base && y < cur.y0 + 1.0)
                .count();
            if crossed + between >= 2 && !framed_at(first - 1) {
                break;
            }
            // A stub's head beside the columns' own heads may run to a
            // title's length (`Sales Volumes per Day by Products from Ongoing
            // Operations | 2017 | 2016`): set as a title, its longer words
            // capitalised and no sentence's stop in it, beside two heads or
            // more. A line of prose beside a table runs in sentence case.
            let heads_beside = above
                .chunks
                .iter()
                .filter(|d| column_of(&cols0, d).is_some_and(|k| k > 0))
                .count();
            let prose = above.chunks.iter().any(|c| {
                let words = c.text.split_whitespace().count();
                words >= 9
                    && !(heads_beside >= 2
                        && words <= MAX_STUB_HEAD_WORDS
                        && column_of(&cols0, c) == Some(0)
                        && set_as_title(&c.text))
            }) || above.chunks.first().is_some_and(|c| is_caption(&c.text));
            // A caption wrapped over the table: its lines after the first,
            // each the next line under the one before and running on from
            // it, are the caption's.
            let caption_tail = {
                let mut k = first - 1;
                let mut tail = false;
                while k > 0 && first - 1 - k < 3 {
                    let (up, l) = (&clipped[k - 1], &clipped[k]);
                    let runs = l.chunks.len() == 1
                        && l.base - up.base <= l.fs.max(up.fs) * NEXT_LINE
                        && up.chunks.last().is_some_and(|a| runs_on(a, &l.chunks[0]));
                    if !runs {
                        break;
                    }
                    if up.chunks.first().is_some_and(|c| is_caption(&c.text)) {
                        tail = true;
                        break;
                    }
                    k -= 1;
                }
                tail
            };
            if prose || caption_tail {
                break;
            }
            crossed += between;
            let header = above
                .chunks
                .iter()
                .all(|c| !is_figure(&c.text) || is_year(&c.text))
                && above
                    .chunks
                    .iter()
                    .any(|c| column_of(&cols0, c).is_some_and(|k| k > 0))
                && above
                    .chunks
                    .iter()
                    .all(|c| c.x0 >= x0 - fs && c.x1 <= x1 + fs);
            // A label line under the header, or a stub wrapped above it.
            let stub = label_row(above)
                && (header_seen == 0
                    || (cur.base - above.base <= cur.fs * WRAP_LEADING
                        && cur.chunks.iter().any(|c| column_of(&cols0, c) == Some(0))));
            // Over a header that names the columns, only a banner (a label
            // across some of them, or with its own rule beneath, however
            // near the header under it the rule is drawn) or a label wrapped
            // tight above one of them belongs to it; a title across the
            // table, or a line of text across its gutters, does not.
            let ncols0 = cols0.len();
            let names_columns =
                |l: &Line| fits(&cols0, l) && l.chunks.len() * 2 >= ncols0 && l.chunks.len() >= 2;
            let banner_chunk = |c: &Chunk| {
                let ks = covered(&cols0, c);
                ((2..ncols0).contains(&ks.len()) && ks[0] > 0)
                    || rules.iter().any(|r| {
                        r.orientation == Orientation::Horizontal
                            && r.bbox.y0 >= above.base - fs * 0.2
                            && r.bbox.y0 < cur.y0
                            && r.bbox.x0 <= (c.x0 + c.x1) / 2.0
                            && r.bbox.x1 >= (c.x0 + c.x1) / 2.0
                            && r.length() >= c.width() * 1.5
                            && covered(
                                &cols0,
                                &Chunk {
                                    x0: r.bbox.x0,
                                    x1: r.bbox.x1,
                                    ..c.clone()
                                },
                            )
                            .len()
                                >= 2
                    })
            };
            let wrapped = fits(&cols0, above) && cur.base - above.base <= cur.fs * WRAP_LEADING;
            // Labels in the columns the header below leaves empty (a stub
            // centred across its lines), or wrapped onto a label below them.
            let taken_cols: Vec<usize> = (first..start)
                .flat_map(|i| {
                    clipped[i]
                        .chunks
                        .iter()
                        .filter_map(|c| column_of(&cols0, c))
                })
                .collect();
            let fills_gap = |c: &Chunk| {
                covered(&cols0, c).len() <= 1 && {
                    let k = column_of(&cols0, c);
                    k.is_some_and(|k| !taken_cols.contains(&k))
                        || (first..start).any(|i| {
                            clipped[i].base - above.base <= clipped[i].fs * WRAP_LEADING * 1.2
                                && clipped[i].chunks.iter().any(|d| column_of(&cols0, d) == k)
                        })
                }
            };
            let banner = above.chunks.iter().all(&banner_chunk);
            let fills_gaps = above.chunks.iter().all(|c| banner_chunk(c) || fills_gap(c));
            if header_named && !(banner || wrapped || fills_gaps) {
                break;
            }
            if value_row(above) {
                header_seen = 0;
                header_named = false;
            } else if header {
                header_seen += 1;
                if !wraps_onto(above, cur) {
                    levels += 1;
                }
                header_named |= names_columns(above);
            } else if stub {
                if header_seen > 0 {
                    header_seen += 1;
                }
            } else {
                break;
            }
            first -= 1;
        }
        // Label lines above with no header over them are not the table's.
        while first < start
            && label_row(&clipped[first])
            && !(first..start).any(|i| {
                clipped[i]
                    .chunks
                    .iter()
                    .any(|c| column_of(&cols0, c).is_some_and(|k| k > 0))
            })
        {
            first += 1;
        }
        floor = last + 1;
        let lines: Vec<&Line> = (first..=last).map(|i| &clipped[i]).collect();
        if let Some(g) = build(lines, rules, x0, x1, page) {
            out.push(g);
        }
    }
    out
}

/// Regions of horizontal rules inside `within`, joined where they sit side
/// by side or one over another: one table's underlines, apart from the
/// next table's across a page column's gutter.
pub fn rule_regions(rules: &[Rule], within: &BBox) -> Vec<BBox> {
    let hs: Vec<BBox> = rules
        .iter()
        .filter(|r| r.orientation == Orientation::Horizontal)
        .map(|r| r.bbox)
        .filter(|b| {
            b.x1 >= within.x0 - 1.0
                && b.x0 <= within.x1 + 1.0
                && b.y1 >= within.y0 - 1.0
                && b.y0 <= within.y1 + 1.0
        })
        .collect();
    let mut groups: Vec<BBox> = Vec::new();
    for b in hs {
        groups.push(b);
        // Merge to a fixed point.
        loop {
            let mut merged = false;
            'outer: for i in 0..groups.len() {
                for j in (i + 1)..groups.len() {
                    let (a, c) = (groups[i], groups[j]);
                    let xgap = (a.x0.max(c.x0) - a.x1.min(c.x1)).max(0.0);
                    if xgap <= RULE_JOIN {
                        groups[i] = BBox {
                            x0: a.x0.min(c.x0),
                            y0: a.y0.min(c.y0),
                            x1: a.x1.max(c.x1),
                            y1: a.y1.max(c.y1),
                        };
                        groups.remove(j);
                        merged = true;
                        break 'outer;
                    }
                }
            }
            if !merged {
                break;
            }
        }
    }
    groups
}

/// Widest gap between two rules of one table, in points: the space between
/// a statement's figure columns, short of a page column's gutter.
const RULE_JOIN: f64 = 24.0;

/// How much further the next row stands under a wrapped cell's line than
/// that line under its cell: a row pitch with space between rows, a quarter
/// more than the leading at least, beyond the point or so that the tops of
/// words move with their letters.
const TIGHT_WRAP: f64 = 1.25;

/// Whether a label line runs on into the next line, rather than naming the
/// rows under it: the next line starts mid-phrase, or this one ends on a
/// word that wants one; and a bold line over a plain one is a heading.
fn continues(l: &Line, n: &Line) -> bool {
    let (Some(a), Some(b)) = (l.chunks.first(), n.chunks.first()) else {
        return false;
    };
    runs_on(a, b)
}

/// A line that carries on the cells of the line above it, in columns that
/// are cells and not prose ([`running_columns`]): the next line under it
/// ([`NEXT_LINE`]), nothing in the stub column and no figure,
/// each of its words in a column the line above fills, running on from what
/// stands there ([`runs_on`]), or qualifying a cell of words
/// ([`qualifies`]; a number's line under it, `3` over `(mg/m )`, is a unit's
/// broken superscript, not a wrapped name). A name or a description wrapped
/// onto a second line, its row's other cells done on the first.
///
/// Or, under a cell of words, alone on its line between two rows of figures
/// and within the extent of the cell's first line (the line a wrap fills, to
/// half the type's size),
/// set tighter under that cell than the next row's cell in its column is
/// under it ([`TIGHT_WRAP`]): a cell's own leading, inside its row, where the
/// row pitch parts rows. So a wrapped cell's second line that starts afresh
/// carries on its cell (`Gallinarum Pullorum` over `CDC1983 67`), where a
/// label naming the rows under it sits as near to them.
fn carries_on(
    cols: &[(f64, f64)],
    running: &[bool],
    above: &Line,
    l: &Line,
    below: Option<&Line>,
) -> bool {
    let fs = l.fs.max(above.fs);
    let d = l.base - above.base;
    // One cell's line between two rows of figures: the table's own rows on
    // both sides, not labels about a drawing.
    let figured = |ln: &Line| ln.chunks.iter().any(|c| is_figure(&c.text));
    let between_rows = l.chunks.len() == 1 && figured(above) && below.is_some_and(figured);
    d > 0.0
        && d <= fs * NEXT_LINE
        && !l.chunks.is_empty()
        && fits(cols, l)
        && l.chunks.iter().all(|c| {
            let Some(k) = column_of(cols, c).filter(|&k| k > 0) else {
                return false;
            };
            !running.get(k).copied().unwrap_or(false)
                && !is_figure(&c.text)
                && cell_in(cols, above, k).is_some_and(|a| {
                    // Within the cell's first line, which the wrap filled.
                    let under = c.x0 >= a.x0 - fs * 0.5 && c.x1 <= a.x1 + fs * 0.5;
                    let tight = between_rows
                        && under
                        && below.and_then(|n| cell_in(cols, n, k)).is_some_and(|n| {
                            let up = c.y0 - a.y0;
                            up > 0.0 && up * TIGHT_WRAP < n.y0 - c.y0
                        });
                    // A wrapped name: the cell above holds words.
                    let named = !is_figure(&a.text) && a.text.chars().any(char::is_alphabetic);
                    runs_on(a, c) || (named && (tight || qualifies(a, c)))
                })
        })
}

/// Whether a cell's line `b` is a qualifier of the cell above, `a`: a
/// bracket opening on a lowercase word (`Uracil` over `(extracellular)`), as
/// a line opening mid-phrase runs on ([`runs_on`]). A list's enumerator in
/// brackets (`(a)`, `(iv)`) starts an item; a bold line over a plain one is a
/// heading.
fn qualifies(a: &Chunk, b: &Chunk) -> bool {
    !(a.bold && !b.bold)
        && b.text
            .strip_prefix('(')
            .is_some_and(|r| r.starts_with(|c: char| c.is_lowercase()))
        && !crate::table::enumerator(b.text.split_whitespace().next().unwrap_or(""))
}

/// A line's last chunk in column `k`.
fn cell_in<'a>(cols: &[(f64, f64)], l: &'a Line, k: usize) -> Option<&'a Chunk> {
    l.chunks.iter().rfind(|a| column_of(cols, a) == Some(k))
}

/// Whether a line is a block's title, not a row: figures (years aside) in
/// fewer than half the columns (`Kenya | 1993 | 2008/9 | Average annual %
/// rate of change` over thirteen columns).
fn titles_a_block(cols: &[(f64, f64)], l: &Line) -> bool {
    let figures = l
        .chunks
        .iter()
        .filter(|c| is_figure(&c.text) && !is_year(&c.text))
        .count();
    figures * 2 < cols.len()
}

/// Whether a line can name a table's columns: nothing in the stub (a row of
/// data names itself there), names over half the columns or more, and no
/// plain number among them. Bounds and ranges name columns (`<16 | 16/17 |
/// <20`); a chart's ticks (`3 | 3`) and a formula's subscripts (`0 | 0 | 0`)
/// do not.
fn names_columns(cols: &[(f64, f64)], l: &Line) -> bool {
    let plain = |t: &str| t.replace([',', ' '], "").parse::<f64>().is_ok();
    !l.chunks.iter().any(|c| column_of(cols, c) == Some(0))
        && l.chunks.len() * 2 >= cols.len()
        && !l.chunks.iter().any(|c| plain(&c.text))
}

/// Whether two lines set the same words in the same columns, nothing in the
/// stub: a line of column names restated (`< 20` and `<20` alike). A row of
/// data names itself in the stub, however often it recurs (`Number of
/// patients | 909 | 462` under each characteristic).
fn same_cells(cols: &[(f64, f64)], a: &Line, b: &Line) -> bool {
    let words = |c: &Chunk| c.text.split_whitespace().collect::<String>();
    names_columns(cols, a)
        && a.chunks.len() >= 2
        && a.chunks.len() == b.chunks.len()
        && a.chunks.iter().zip(&b.chunks).all(|(x, y)| {
            column_of(cols, x).is_some()
                && column_of(cols, x) == column_of(cols, y)
                && words(x) == words(y)
        })
}

/// The columns whose text runs on across the table's rows: on the lines
/// that open a row, a label in the stub column, the column's text runs on
/// from the line above in half of them, two at least. That is a page's prose
/// read in beside the table, its sentences carried down through the rows; a
/// table's cells start afresh on each row.
fn running_columns(cols: &[(f64, f64)], lines: &[&Line]) -> Vec<bool> {
    let mut opened = vec![0usize; cols.len()];
    let mut ran = vec![0usize; cols.len()];
    for w in lines.windows(2) {
        let (up, l) = (w[0], w[1]);
        if !l.chunks.iter().any(|c| column_of(cols, c) == Some(0)) {
            continue;
        }
        let near = l.base - up.base <= l.fs.max(up.fs) * NEXT_LINE;
        for c in &l.chunks {
            let Some(k) = column_of(cols, c).filter(|&k| k > 0) else {
                continue;
            };
            opened[k] += 1;
            if near
                && up
                    .chunks
                    .iter()
                    .filter(|a| column_of(cols, a) == Some(k))
                    .next_back()
                    .is_some_and(|a| runs_on(a, c))
            {
                ran[k] += 1;
            }
        }
    }
    (0..cols.len())
        .map(|k| ran[k] >= 2 && ran[k] * 2 >= opened[k])
        .collect()
}

/// Whether text `b`, on the next line, runs on from `a`: it starts
/// mid-phrase, or `a` ends on a word or mark that wants more; not after a
/// colon, and not from a bold line into a plain one (a heading).
fn runs_on(a: &Chunk, b: &Chunk) -> bool {
    let t = a.text.trim_end();
    if t.ends_with(':') || (a.bold && !b.bold) {
        return false;
    }
    let last = t.rsplit(' ').next().unwrap_or("").to_lowercase();
    b.text.starts_with(|c: char| c.is_lowercase())
        || t.ends_with([',', '-', '–', '(', '&', '/'])
        || matches!(
            last.as_str(),
            "and"
                | "or"
                | "of"
                | "from"
                | "for"
                | "to"
                | "in"
                | "on"
                | "by"
                | "with"
                | "per"
                | "the"
                | "a"
                | "an"
                | "at"
                | "before"
                | "after"
                | "less"
                | "net"
                | "into"
                | "under"
                | "over"
                | "than"
                | "through"
                | "between"
                | "within"
                | "without"
                | "during"
                | "across"
                | "via"
        )
}

/// `Table 2: …`, `TABLE 4.` — a caption, set over the table, not in it.
fn is_caption(t: &str) -> bool {
    let mut w = t.split_whitespace();
    let first = w.next().unwrap_or("").to_lowercase();
    matches!(
        first.as_str(),
        "table" | "tab." | "exhibit" | "schedule" | "figure" | "fig." | "chart" | "graph"
    ) && w.next().is_some_and(|n| {
        n.chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c.is_uppercase())
    })
}

/// A year, or a span of years (`2019-2020`, `2014–19`): a header's label,
/// not a figure.
fn is_year(t: &str) -> bool {
    let t = t.trim();
    // A year with its note's mark (`2007(1)`, `2004(b)`, `2008*`) is a year.
    let t = strip_note_mark(t);
    let year = |s: &str| s.len() == 4 && s.parse::<u32>().is_ok_and(|y| (1900..2100).contains(&y));
    if year(t) {
        return true;
    }
    match t.split_once(['-', '–', '/']) {
        Some((a, b)) => {
            year(a) && (year(b) || (b.len() == 2 && b.chars().all(|c| c.is_ascii_digit())))
        }
        None => false,
    }
}

/// `t` without a note's mark after it: a letter or one or two digits in
/// brackets (`(1)`, `(b)`), or a run of `*`, `†`, `‡`, `§`.
fn strip_note_mark(t: &str) -> &str {
    let t = t.trim_end_matches(['*', '†', '‡', '§']).trim_end();
    if let Some(open) = t.rfind('(') {
        let inner = &t[open + 1..];
        if let Some(mark) = inner.strip_suffix(')') {
            if !mark.is_empty()
                && mark.chars().count() <= 2
                && mark.chars().all(|c| c.is_alphanumeric())
            {
                return t[..open].trim_end();
            }
        }
    }
    t
}

/// A sample's size set over its column (`n = 59`, `(n=735)`, `N=104`):
/// the column's population, a header's line, not a value.
fn is_sample_size(t: &str) -> bool {
    let t = t
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
    let Some(rest) = t.strip_prefix(['n', 'N']) else {
        return false;
    };
    let Some(count) = rest.trim_start().strip_prefix('=') else {
        return false;
    };
    let count = count.trim();
    !count.is_empty()
        && count
            .chars()
            .all(|c| c.is_ascii_digit() || c == ',' || c == ' ')
}

/// Whether a column's head is a unit of what its cells count: a percentage,
/// a count (`n`, `N`, `No.`), both (`n (%)`), or a confidence interval
/// (`95% CI`).
fn is_unit_head(t: &str) -> bool {
    let t: String = t.split_whitespace().collect();
    let t = t.trim_start_matches('(').trim_end_matches(')');
    matches!(
        t,
        "%" | "n" | "N" | "No." | "no." | "n(%)" | "N(%)" | "No.(%)" | "CI"
    ) || t
        .strip_suffix("%CI")
        .is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Rows, header and bands from the table's lines.
fn build(lines: Vec<&Line>, rules: &[Rule], x0: f64, x1: f64, page: usize) -> Option<Grid> {
    let fs = lines.iter().map(|l| l.fs).fold(0.0, f64::max).max(1.0);
    // The header ends above the first line of figures past the label column.
    let first_body = {
        let cols = columns(&lines, x0, x1);
        let first = lines.iter().position(|l| has_figures(&cols, l))?;
        // Lines of the columns' sample sizes (`(n=735) | (n=640)`) under
        // their heads, nothing in the stub, are the header's: a row of data
        // names itself in the stub, and a group's own size sits beside its
        // label (`Skin/Soft Tissue | n= 175`).
        // So are lines of the columns' units (`% | N | % | N`, `No. | %`,
        // `n | % | 95% CI`) under their groups: a row of data gives values,
        // not what they count. The stub may hold its own head there (`BMI
        // Class`), a label with no figure in it.
        let units = |l: &Line| {
            let past: Vec<&Chunk> = l
                .chunks
                .iter()
                .filter(|c| column_of(&cols, c) != Some(0))
                .collect();
            past.len() >= 2
                && past.iter().all(|c| is_unit_head(&c.text))
                && l.chunks
                    .iter()
                    .filter(|c| column_of(&cols, c) == Some(0))
                    .all(|c| !c.text.chars().any(|ch| ch.is_ascii_digit()))
        };
        let sizes = |l: &Line| {
            units(l)
                || !l.chunks.iter().any(|c| column_of(&cols, c) == Some(0))
                    && l.chunks.iter().any(|c| is_sample_size(&c.text))
                    && l.chunks
                        .iter()
                        .all(|c| is_sample_size(&c.text) || !is_figure(&c.text) || is_year(&c.text))
        };
        let mut k = first;
        while k < lines.len() && sizes(lines[k]) {
            k += 1;
        }
        if !lines[k..].iter().any(|l| has_figures(&cols, l)) {
            k = first;
        }
        lines[k..]
            .iter()
            .position(|l| has_figures(&cols, l))
            .map(|p| k + p)?
    };
    let (head, body) = lines.split_at(first_body);
    // Body section labels set above the first figures belong to the body:
    // a label-only line in the label column, the header above it naming
    // columns past the first.
    let mut head: Vec<&Line> = head.to_vec();
    let mut body: Vec<&Line> = body.to_vec();
    // A table set in blocks ([`read_beside`]): its first line of figures
    // opens each block again, under the block's title. That line names the
    // columns of its block's rows, each time it stands (a header restated);
    // each block's title is a section over its rows, the first block's too
    // (the header's lines set close over the first column line). The first
    // title stands over the first column line, so neither is the table's
    // header: no block is named by another's title.
    let mut titles: Vec<&Line> = Vec::new();
    let mut restated: Vec<&Line> = Vec::new();
    if let Some(&names) = body.first() {
        let all = columns(&lines, x0, x1);
        let again: Vec<usize> = (1..body.len())
            .filter(|&i| same_cells(&all, names, body[i]) && titles_a_block(&all, body[i - 1]))
            .collect();
        if !again.is_empty() {
            let mut t = head.len();
            while t > 0
                && (t == head.len()
                    || head[t].base - head[t - 1].base
                        <= head[t].fs.max(head[t - 1].fs) * WRAP_LEADING)
            {
                t -= 1;
            }
            titles = head.split_off(t);
            restated.push(names);
            restated.extend(again.iter().map(|&r| body[r]));
            for &r in &again {
                let mut j = r;
                while j > 1
                    && r - j < MAX_HEADER_LINES
                    && titles_a_block(&all, body[j - 1])
                    && body[j - 1]
                        .chunks
                        .iter()
                        .any(|c| column_of(&all, c).is_some_and(|k| k > 0))
                {
                    j -= 1;
                    titles.push(body[j]);
                }
            }
            let first: Vec<&Line> = titles
                .iter()
                .take_while(|l| l.base <= names.base)
                .copied()
                .collect();
            body = first.into_iter().chain(body.iter().copied()).collect();
        }
    }
    let is_title = |l: &Line| titles.iter().any(|t| std::ptr::eq(*t, l));
    let is_restated = |l: &Line| restated.iter().any(|t| std::ptr::eq(*t, l));
    let cols = {
        let rows: Vec<&Line> = body.iter().filter(|l| !is_title(l)).copied().collect();
        columns(&rows, x0, x1)
    };
    if cols.len() < 2 {
        return None;
    }
    while let Some(l) = head.last() {
        // The header's own rule, drawn under the line above (an underline
        // under each column's head), parts this line from it: the header
        // ends there, and what is under it is the body's.
        let ruled_apart = head.len() > 1 && {
            let p = head[head.len() - 2];
            rules.iter().any(|r| {
                r.orientation == Orientation::Horizontal
                    && r.axis_pos() > p.base
                    && r.axis_pos() < l.base - l.fs * 0.5
                    && r.bbox.x0 < cols[0].1
                    && r.bbox.x1 > cols[0].0
            })
        };
        // In the label column; or, under the header's rule, hung out to
        // the left of it as a group's label often is (`United States` over
        // its indented rows), reaching into it and short of the second
        // column. A speck with no letter or figure in it says nothing.
        let said: Vec<&Chunk> = l
            .chunks
            .iter()
            .filter(|c| c.text.chars().any(char::is_alphanumeric))
            .collect();
        let hung = |c: &Chunk| ruled_apart && c.x1 > cols[0].0 && c.x1 < cols[1].0;
        let label_only = !said.is_empty()
            && said
                .iter()
                .all(|c| column_of(&cols, c) == Some(0) || hung(c))
            && said.iter().all(|c| covered(&cols, c).len() <= 1);
        // Wrapped from the line above, unless that rule parts them
        // (`United States` under `Sales Volumes per Day by Products | 2017
        // | 2016`).
        let wrapped = head.len() > 1 && {
            let p = head[head.len() - 2];
            l.base - p.base <= l.fs * WRAP_LEADING
                && p.chunks.iter().any(|c| column_of(&cols, c) == Some(0))
                && !ruled_apart
        };
        if label_only && head.len() > 1 && !wrapped {
            body.insert(0, head.pop().unwrap());
        } else {
            break;
        }
    }
    // A header line that is a column's label wrapped onto a second line joins
    // it; a banner, or a line with a rule under it, stays a row of its own.
    // The rule under a label: the pieces under its middle, or with `joined`
    // the run of pieces that meet end to end through it. A group's
    // underline drawn a column at a time, its joins bridged, is one rule
    // under the group's label, however few columns the label's own words
    // cover (`Net sales (a)` over three years); it says which columns the
    // label spans, not where the header's rows part.
    let rule_at = |l: &Line, c: &Chunk, joined: bool| -> Option<(f64, f64)> {
        let mid = (c.x0 + c.x1) / 2.0;
        let mut pieces: Vec<(f64, f64)> = rules
            .iter()
            .filter(|r| r.orientation == Orientation::Horizontal)
            .filter(|r| r.bbox.y0 >= l.base - fs * 0.2 && r.bbox.y0 <= l.base + fs * 0.8)
            .map(|r| (r.bbox.x0, r.bbox.x1))
            .collect();
        let piece = pieces
            .iter()
            .copied()
            .filter(|&(a, b)| a <= mid && b >= mid)
            .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)));
        let found = if joined {
            pieces.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut runs: Vec<(f64, f64)> = Vec::new();
            for &(a, b) in &pieces {
                match runs.last_mut() {
                    Some(r) if a <= r.1 + 0.5 => r.1 = r.1.max(b),
                    _ => runs.push((a, b)),
                }
            }
            // Group underlines, one under each group and parted between
            // them; a rule otherwise is read by the piece under the label.
            if runs.len() >= 2 {
                runs.into_iter().find(|&(a, b)| a <= mid && b >= mid)
            } else {
                None
            }
        } else {
            piece
        };
        // A banner's rule is under it alone; one under several labels is
        // the rule between the header and the body.
        found.filter(|&(a, b)| {
            l.chunks
                .iter()
                .filter(|d| (d.x0 + d.x1) / 2.0 >= a && (d.x0 + d.x1) / 2.0 <= b)
                .count()
                == 1
        })
    };
    let rule_under = |l: &Line, c: &Chunk| rule_at(l, c, false);
    // The header is banner rows and one row of column labels. A banner
    // label runs across columns, has its own rule beneath it, or stands
    // over another column's label without running on into it; it keeps a
    // row of its own, above or below the column labels as it is set. Every
    // other label is its column's, however many lines it takes: `December
    // 28,` over `2014`, `Remittance` over `inflows in 2020`, and a stub
    // centred across the header's lines (`AMS`).
    let synth = |chunks: Vec<Chunk>, base: f64| -> Line {
        Line {
            base,
            fs,
            y0: chunks.iter().map(|c| c.y0).fold(f64::MAX, f64::min),
            y1: chunks.iter().map(|c| c.y1).fold(f64::MIN, f64::max),
            chunks,
        }
    };
    // A label alone on its header line, centred over the columns of
    // figures, names them all: `(in millions)` under the years.
    let lone_centred = |l: &Line, c: &Chunk| {
        let n = cols.len();
        l.chunks.len() == 1 && n >= 3 && column_of(&cols, c).is_some_and(|k| k > 0) && {
            let (a, b) = (cols[1].0, cols[n - 1].1);
            let mid = (c.x0 + c.x1) / 2.0;
            (mid - (a + b) / 2.0).abs() <= (b - a) * 0.15 && c.width() > cols[1].1 - cols[1].0
        }
    };
    let mut leaf: Vec<Chunk> = Vec::new();
    // (base, chunks, above the column labels, columns named)
    let mut banners: Vec<(f64, Vec<Chunk>, bool, Vec<usize>)> = Vec::new();
    let mut leaf_bases: Vec<(usize, f64)> = Vec::new();
    let mut kinds: Vec<Vec<bool>> = Vec::new();
    for (i, l) in head.iter().enumerate() {
        let mut ks = Vec::new();
        for c in &l.chunks {
            let k = column_of(&cols, c).unwrap_or(0);
            // The label under this one in its column: a column's label, not
            // a banner alone on its line.
            let below = head[i + 1..].iter().find_map(|n| {
                n.chunks
                    .iter()
                    .find(|d| {
                        column_of(&cols, d) == Some(k)
                            && !lone_centred(n, d)
                            && covered(&cols, d).len() <= 1
                    })
                    .map(|d| (n, d))
            });
            let runs_on = below.is_some_and(|(n, _)| n.base - l.base <= n.fs * WRAP_LEADING * 1.2);
            // A column label's own second line, set close under its first
            // on its axis (`Age` over `(Years)`), goes on naming the label's
            // column. Under a banner's first line it is judged as any line.
            let wraps_from_above = head[..i]
                .iter()
                .enumerate()
                .rev()
                .find_map(|(pi, p)| {
                    p.chunks
                        .iter()
                        .enumerate()
                        .find(|(_, d)| column_of(&cols, d) == Some(k))
                        .map(|(di, d)| (pi, di, p, d))
                })
                .is_some_and(|(pi, di, p, d)| {
                    let tol = l.fs * 0.5;
                    !kinds[pi][di]
                        && l.base - p.base <= l.fs * WRAP_LEADING * 1.2
                        && ((d.x0 - c.x0).abs() <= tol
                            || ((d.x0 + d.x1) / 2.0 - (c.x0 + c.x1) / 2.0).abs() <= tol)
                });
            // A rule under a column's own label, as wide as its column,
            // underlines it; one reaching across columns makes a banner.
            // Under a label's second line, one across the stub and every
            // column is the table's own, ruling the header's levels apart
            // (`Age` over `(Years)` over the rule, `n | Mean | SD` under).
            let ruled_across = rule_under(l, c).is_some_and(|(a, b)| {
                !(wraps_from_above && a <= cols[0].1 && b >= cols[cols.len() - 1].0)
                    && covered(
                        &cols,
                        &Chunk {
                            x0: a,
                            x1: b,
                            ..c.clone()
                        },
                    )
                    .len()
                        >= 2
            });
            // A label over the column beside it too: that column is named
            // below this line and nowhere level with it or above (`EUR`
            // over `Unit` and `Total`). A label's second line names its own.
            let tight_below = below.is_some_and(|(n, _)| n.base - l.base <= n.fs * WRAP_LEADING);
            let label_of = |n: &Line, k: usize| {
                n.chunks.len() >= 2
                    && n.chunks
                        .iter()
                        .any(|d| column_of(&cols, d) == Some(k) && covered(&cols, d).len() <= 1)
            };
            let over_next = k > 0
                && !tight_below
                && !wraps_from_above
                && k + 1 < cols.len()
                && head[i + 1..].iter().any(|n| label_of(n, k + 1))
                && !head[..=i]
                    .iter()
                    .any(|n| n.chunks.iter().any(|d| column_of(&cols, d) == Some(k + 1)));
            let banner = covered(&cols, c).len() >= 2
                || ruled_across
                || over_next
                || lone_centred(l, c)
                || (k > 0 && below.is_some() && !runs_on);
            ks.push(banner);
            if !banner {
                leaf_bases.push((k, l.base));
            }
        }
        kinds.push(ks);
    }
    // A label wrapped onto the next line is of a kind with that line: a
    // banner's first line is the banner's (`Proportion of Hospital` over
    // `Discharges with SDV`, over the column's unit `%`).
    for i in (0..head.len()).rev() {
        for (ci, c) in head[i].chunks.iter().enumerate() {
            if kinds[i][ci] {
                continue;
            }
            let k = column_of(&cols, c).unwrap_or(0);
            let Some(j) = head
                .iter()
                .enumerate()
                .skip(i + 1)
                .find(|(_, n)| n.chunks.iter().any(|d| column_of(&cols, d) == Some(k)))
                .map(|(j, _)| j)
            else {
                continue;
            };
            let n = head[j];
            let wraps = n.base - head[i].base <= n.fs * WRAP_LEADING * 1.2;
            // Set on the label's own axis, as a label's lines are: starting
            // where it starts, or centred where it is (a unit centred
            // between two years is no year's second line).
            let aligned = |d: &Chunk| {
                let tol = n.fs * 0.5;
                (c.x0 - d.x0).abs() <= tol
                    || ((c.x0 + c.x1) / 2.0 - (d.x0 + d.x1) / 2.0).abs() <= tol
            };
            // And the column's unit under that line, its own label: the
            // label wraps over a group level (`%` under `Discharges with
            // SDV`), as a line of units read into the header makes one. A
            // label wrapped over its column's own words stays one label.
            let labelled_below = head.iter().enumerate().skip(j + 1).any(|(m, l2)| {
                l2.chunks.iter().zip(&kinds[m]).any(|(e, &banner)| {
                    !banner && column_of(&cols, e) == Some(k) && is_unit_head(&e.text)
                })
            });
            if k > 0
                && wraps
                && labelled_below
                && n.chunks.iter().zip(&kinds[j]).any(|(d, &banner)| {
                    banner
                        && column_of(&cols, d) == Some(k)
                        && covered(&cols, d).len() <= 1
                        && aligned(d)
                })
            {
                kinds[i][ci] = true;
                leaf_bases.retain(|&(kk, b)| !(kk == k && b == head[i].base));
            }
        }
    }
    // A header ending on a line of the columns' units ([`is_unit_head`]).
    let over_units = head.iter().any(|l| {
        let past: Vec<&Chunk> = l
            .chunks
            .iter()
            .filter(|c| column_of(&cols, c) != Some(0))
            .collect();
        past.len() >= 2 && past.iter().all(|c| is_unit_head(&c.text))
    });
    for (i, l) in head.iter().enumerate() {
        let mut row: Vec<Chunk> = Vec::new();
        for (c, &banner) in l.chunks.iter().zip(&kinds[i]) {
            if banner {
                row.push(c.clone());
            } else {
                leaf.push(c.clone());
            }
        }
        if !row.is_empty() {
            // Above the labels of the columns it names, or below them.
            let names: Vec<usize> = row
                .iter()
                .flat_map(|c| {
                    if lone_centred(l, c) {
                        (1..cols.len()).collect()
                    } else {
                        covered(&cols, c)
                    }
                })
                .collect();
            // Naming no column outright (a label set in the gap over its
            // pair, `Poor (1)` over `n | %`), over a line of units, it is
            // measured against the columns' last line of labels: a label
            // level with it (the stub's head, a wider group's own) is no
            // measure.
            let under = if names.is_empty() && over_units {
                let last = leaf_bases
                    .iter()
                    .filter(|(k, _)| *k > 0)
                    .map(|(_, b)| *b)
                    .fold(f64::MIN, f64::max);
                if last == f64::MIN {
                    f64::MAX
                } else {
                    last
                }
            } else {
                leaf_bases
                    .iter()
                    .filter(|(k, _)| names.contains(k) || names.is_empty())
                    .map(|(_, b)| *b)
                    .fold(f64::MAX, f64::min)
            };
            let mut named = names.clone();
            for c in &row {
                if let Some((a, b)) = rule_under(l, c) {
                    named.extend(covered(
                        &cols,
                        &Chunk {
                            x0: a,
                            x1: b,
                            ..c.clone()
                        },
                    ));
                }
            }
            banners.push((l.base, row, l.base < under || under == f64::MAX, named));
        }
    }
    // A column label with no banner over it, set higher than the header's
    // last line, spans the header's rows and is read into the top one: a
    // stub centred across them (`AMS`), a label wrapped down beside a
    // banner and the labels under it (`Remittance` over `inflows in 2020`),
    // a label level with the banner over the next column (`Mineral or
    // colloid type` beside `CEC of pure colloid` over `cmolc/kg`). Left in
    // the last line, the banner beside it took its column too.
    let above_names: Vec<usize> = banners
        .iter()
        .filter(|b| b.2)
        .flat_map(|b| b.3.iter().copied())
        .collect();
    let mut lifted: Vec<Chunk> = Vec::new();
    let mut lifted_cols: Vec<usize> = Vec::new();
    if banners.iter().any(|b| b.2) && !leaf.is_empty() {
        // The line most columns' labels end on: a label wrapped further
        // down is no measure of it.
        let mut ends: Vec<f64> = (0..cols.len())
            .filter_map(|k| {
                leaf.iter()
                    .filter(|c| column_of(&cols, c) == Some(k))
                    .map(|c| c.y1)
                    .reduce(f64::max)
            })
            .collect();
        ends.sort_by(f64::total_cmp);
        let bottom = ends[ends.len() / 2];
        for k in 0..cols.len() {
            let in_k: Vec<&Chunk> = leaf
                .iter()
                .filter(|c| column_of(&cols, c) == Some(k) && covered(&cols, c).len() <= 1)
                .collect();
            let top = in_k.iter().map(|c| c.y1).fold(f64::MAX, f64::min);
            if !above_names.contains(&k) && !in_k.is_empty() && top < bottom - fs * 0.5 {
                lifted_cols.push(k);
            }
        }
        let (up, stay): (Vec<Chunk>, Vec<Chunk>) = leaf.into_iter().partition(|c| {
            covered(&cols, c).len() <= 1
                && column_of(&cols, c).is_some_and(|k| lifted_cols.contains(&k))
        });
        if stay.is_empty() {
            leaf = up;
            lifted_cols.clear();
        } else {
            leaf = stay;
            lifted = up;
        }
    }
    let mut head_lines: Vec<Line> = Vec::new();
    for (base, row, above, _) in &banners {
        if *above {
            let mut row = row.clone();
            if head_lines.is_empty() {
                row.append(&mut lifted);
                row.sort_by(|a, b| a.x0.total_cmp(&b.x0));
            }
            head_lines.push(synth(row, *base));
        }
    }
    let leaf_row = head_lines.len();
    if !leaf.is_empty() {
        let base = leaf.iter().map(|c| c.y1).fold(f64::MIN, f64::max);
        head_lines.push(synth(leaf, base));
    }
    for (base, row, above, _) in &banners {
        if !*above {
            head_lines.push(synth(row.clone(), *base));
        }
    }
    let downs: Vec<(usize, usize, usize)> =
        lifted_cols.iter().map(|&k| (0, k, leaf_row + 1)).collect();
    let head_rows: Vec<Vec<&Line>> = head_lines.iter().map(|l| vec![l]).collect();
    // Body rows: one per line, a wrapped label joined to its row and a
    // label alone over rows read as their section.
    let full = |l: &Line| {
        l.chunks.len() >= 2
            || l.chunks
                .iter()
                .any(|c| column_of(&cols, c).is_some_and(|k| k > 0))
    };
    let label_only = |l: &Line| !full(l);
    let mut pitches: Vec<f64> = body
        .windows(2)
        .filter(|w| full(w[0]) && full(w[1]))
        .map(|w| w[1].base - w[0].base)
        .collect();
    pitches.sort_by(f64::total_cmp);
    let pitch = pitches.get(pitches.len() / 2).copied().unwrap_or(fs * 2.0);
    let tight = |a: &Line, b: &Line| {
        let d = b.base - a.base;
        d <= b.fs * WRAP_LEADING && d < pitch * 0.85
    };
    let running = running_columns(&cols, &body);
    let mut body_rows: Vec<(Vec<&Line>, bool)> = Vec::new(); // (lines, section)
    let mut restated_rows: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let l = body[i];
        // A block's line of column names: a row of its own, naming the
        // columns of the rows under it.
        if is_restated(l) {
            restated_rows.push(body_rows.len());
            body_rows.push((vec![l], false));
            i += 1;
            continue;
        }
        // A block's title, its lines together: a section.
        if is_title(l) {
            let mut ls = vec![l];
            i += 1;
            while i < body.len() && is_title(body[i]) {
                ls.push(body[i]);
                i += 1;
            }
            body_rows.push((ls, true));
            continue;
        }
        if label_only(l) {
            // Wrapped onto the next line, which carries the row's values.
            let next = body.get(i + 1).copied();
            let forward = next.is_some_and(|n| tight(l, n) && continues(l, n));
            // Or the tail of the row above.
            let back = body_rows.last().is_some_and(|(prev, section)| {
                !section && tight(prev.last().unwrap(), l) && !forward
            });
            if back {
                body_rows.last_mut().unwrap().0.push(l);
                i += 1;
                continue;
            }
            if forward {
                let n = next.unwrap();
                if label_only(n) {
                    // A section label on two lines.
                    let mut ls = vec![l, n];
                    i += 2;
                    while i < body.len() && label_only(body[i]) && tight(ls[ls.len() - 1], body[i])
                    {
                        ls.push(body[i]);
                        i += 1;
                    }
                    let section = i < body.len();
                    body_rows.push((ls, section));
                } else {
                    body_rows.push((vec![l, n], false));
                    i += 2;
                }
                continue;
            }
            body_rows.push((vec![l], i + 1 < body.len()));
            i += 1;
            continue;
        }
        // A cell wrapped onto this line: the row above's.
        if body_rows.last().is_some_and(|(prev, section)| {
            !section
                && carries_on(
                    &cols,
                    &running,
                    prev.last().unwrap(),
                    l,
                    body.get(i + 1).copied(),
                )
        }) {
            body_rows.last_mut().unwrap().0.push(l);
            i += 1;
            continue;
        }
        body_rows.push((vec![l], false));
        i += 1;
    }
    if std::env::var_os("FDOC_TEXTGRID_DEBUG").is_some() {
        eprintln!("TEXTGRID build pitch {pitch:.1} fs {fs:.1} cols {cols:?}");
        for ls in &head_rows {
            eprintln!(
                "  head {:?}",
                ls.iter()
                    .map(|l| (
                        l.base,
                        l.chunks
                            .iter()
                            .map(|c| (c.text.as_str(), c.x0 as i32, c.x1 as i32))
                            .collect::<Vec<_>>()
                    ))
                    .collect::<Vec<_>>()
            );
        }
        for (ls, sec) in &body_rows {
            eprintln!(
                "  row sec={sec} {:?}",
                ls.iter()
                    .map(|l| (
                        l.base,
                        l.chunks.iter().map(|c| c.text.as_str()).collect::<Vec<_>>()
                    ))
                    .collect::<Vec<_>>()
            );
        }
    }
    // A label alone at the foot is not the table's.
    while body_rows
        .last()
        .is_some_and(|(ls, _)| ls.iter().all(|l| label_only(l)))
    {
        body_rows.pop();
    }
    let figure_rows = body_rows
        .iter()
        .filter(|(ls, s)| !s && ls.iter().any(|l| full(l)))
        .count();
    if figure_rows < 2 {
        return None;
    }
    // Most rows put a value past the label column.
    let valued = body_rows
        .iter()
        .filter(|(ls, s)| {
            !s && ls.iter().any(|l| {
                l.chunks
                    .iter()
                    .any(|c| column_of(&cols, c).is_some_and(|k| k > 0))
            })
        })
        .count();
    if valued * 2 < figure_rows {
        return None;
    }

    // A table labels its rows, or names its columns: the tick labels of a
    // chart's axes do neither, and line up as well as any figures.
    let value_rows: Vec<&Vec<&Line>> = body_rows
        .iter()
        .filter(|(_, s)| !s)
        .map(|(ls, _)| ls)
        .collect();
    let labelled = value_rows
        .iter()
        .filter(|ls| {
            ls.iter().any(|l| {
                l.chunks.iter().any(|c| {
                    column_of(&cols, c) == Some(0)
                        && !is_figure(&c.text)
                        && c.text.chars().any(char::is_alphabetic)
                })
            })
        })
        .count();
    let named = head_rows.iter().any(|ls| {
        ls.iter()
            .flat_map(|l| l.chunks.iter())
            .filter(|c| !is_figure(&c.text) && c.text.chars().any(char::is_alphabetic))
            .count()
            >= 2
    });
    if labelled * 2 < value_rows.len() && !named {
        return None;
    }
    // An axis: a short table with a row of three figures or more in even
    // steps (`65 70 75 80`, `0.03 0.06 0.09`) is a chart's scale.
    let axis = value_rows.len() <= 3
        && value_rows.iter().any(|ls| {
            let xs: Vec<f64> = ls
                .iter()
                .flat_map(|l| l.chunks.iter())
                .filter_map(|c| c.text.trim().replace(',', "").parse::<f64>().ok())
                .collect();
            xs.len() >= 3
                && !xs
                    .iter()
                    .all(|x| x.fract() == 0.0 && (1900.0..2100.0).contains(x))
                && {
                    let d = xs[1] - xs[0];
                    d != 0.0
                        && xs
                            .windows(2)
                            .all(|w| ((w[1] - w[0]) - d).abs() <= d.abs() * 0.01)
                }
        });
    if axis {
        return None;
    }
    // Display equations and their numbers: `(3.15a)` beside each.
    if cols.len() == 2 {
        let numbers: Vec<&str> = value_rows
            .iter()
            .flat_map(|ls| ls.iter().flat_map(|l| l.chunks.iter()))
            .filter(|c| column_of(&cols, c) == Some(1))
            .map(|c| c.text.trim())
            .collect();
        let numbered = |t: &str| {
            t.len() >= 3
                && t.starts_with('(')
                && t.ends_with(')')
                && t[1..t.len() - 1]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit())
                && t[1..t.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.' || c.is_ascii_lowercase())
        };
        if !numbers.is_empty() && numbers.iter().all(|t| numbered(t)) {
            return None;
        }
    }
    // A contents page: entries over a rising column of page numbers, under
    // no header or one that says so. A header naming both columns makes it
    // a table of rising values (`Mineral or colloid type` and `CEC of pure
    // colloid`: 10, 30, 100, 150, 200); a label over one is a chart's.
    let head_label = |k: usize| {
        head_rows
            .iter()
            .flat_map(|ls| ls.iter().flat_map(|l| l.chunks.iter()))
            .filter(|c| column_of(&cols, c) == Some(k) && c.text.chars().any(char::is_alphabetic))
            .any(|c| !c.text.trim().to_lowercase().starts_with("page"))
    };
    let figures_named = head_label(0) && head_label(1);
    if cols.len() == 2 && !figures_named {
        let pages: Vec<u32> = value_rows
            .iter()
            .flat_map(|ls| ls.iter().flat_map(|l| l.chunks.iter()))
            .filter(|c| column_of(&cols, c) == Some(1))
            .filter_map(|c| c.text.trim().parse::<u32>().ok())
            .collect();
        if pages.len() >= 3
            && pages.len() * 10 >= value_rows.len() * 8
            && pages.windows(2).all(|w| w[0] <= w[1])
        {
            return None;
        }
    }
    // Boundaries.
    let all_rows: Vec<Vec<&Line>> = head_rows
        .iter()
        .cloned()
        .chain(body_rows.iter().map(|(ls, _)| ls.clone()))
        .collect();
    let top = |ls: &Vec<&Line>| ls.iter().map(|l| l.y0).fold(f64::MAX, f64::min);
    let bottom = |ls: &Vec<&Line>| ls.iter().map(|l| l.y1).fold(f64::MIN, f64::max);
    let mut ys = vec![top(&all_rows[0]) - 0.5];
    for w in all_rows.windows(2) {
        let (a, b) = (bottom(&w[0]), top(&w[1]));
        let y = if b > a {
            (a + b) / 2.0
        } else {
            let (pa, pb) = (w[0].last().unwrap().base, w[1][0].base);
            (pa + pb) / 2.0 - fs * 0.3
        };
        ys.push(y.max(*ys.last().unwrap() + 0.1));
    }
    ys.push(bottom(all_rows.last().unwrap()).max(*ys.last().unwrap()) + 0.5);
    let left = all_rows
        .iter()
        .flatten()
        .flat_map(|l| l.chunks.iter().map(|c| c.x0))
        .fold(f64::MAX, f64::min)
        .min(cols[0].0);
    let right = all_rows
        .iter()
        .flatten()
        .flat_map(|l| l.chunks.iter().map(|c| c.x1))
        .fold(f64::MIN, f64::max)
        .max(cols[cols.len() - 1].1);
    let mut xs = vec![left - 0.5];
    for w in cols.windows(2) {
        // Just left of the next column's ink: a label's leaders run up to it.
        xs.push(((w[0].1 + w[1].0) / 2.0).max(w[1].0 - fs * 0.5));
    }
    xs.push(right + 0.5);

    // Header spans: a label across a gutter, or a rule under it that reaches
    // across one, names every column it covers.
    let ncols = cols.len();
    // A label over the figures reads its rule's whole run of group
    // underlines where it is set as a group's label is: centred over the
    // run, as the banner passes below take a centred label, and the only
    // label of its header row over it. A column's label set to one side of
    // the cells' borders run on under its neighbour (`OP` over `OP | LOS`)
    // is not. The labels' own head reads the piece under it, not a rule
    // across the header.
    let group_run = |row: &[&Line], l: &Line, c: &Chunk| -> Option<(f64, f64)> {
        if !column_of(&cols, c).is_some_and(|k| k > 0) {
            return None;
        }
        rule_at(l, c, true).filter(|&(a, b)| {
            let centred = ((c.x0 + c.x1) / 2.0 - (a + b) / 2.0).abs() <= (b - a) * 0.15;
            centred
                && !row.iter().flat_map(|o| o.chunks.iter()).any(|d| {
                    let m = (d.x0 + d.x1) / 2.0;
                    !std::ptr::eq(d, c) && m >= a && m <= b
                })
        })
    };
    let mut spans: Vec<(usize, usize, usize)> = Vec::new();
    // The header rows with a label underlined by a rule or run of its own.
    let mut underlined: Vec<usize> = Vec::new();
    for (r, ls) in head_rows.iter().enumerate() {
        for l in ls {
            for c in &l.chunks {
                let mut ks = covered(&cols, c);
                let run = group_run(ls, l, c);
                // A rule under several labels of the row is none of theirs
                // alone: the groups it underlines are cut between them below.
                let shared = |&(a, b): &(f64, f64)| {
                    ls.iter().flat_map(|o| o.chunks.iter()).any(|d| {
                        let m = (d.x0 + d.x1) / 2.0;
                        !std::ptr::eq(d, c) && m >= a && m <= b
                    })
                };
                if let Some((a, b)) = run.or_else(|| rule_under(l, c).filter(|r| !shared(r))) {
                    let wide = Chunk {
                        x0: a,
                        x1: b,
                        ..c.clone()
                    };
                    let under = covered(&cols, &wide);
                    if under.len() >= 2 {
                        underlined.push(r);
                    }
                    if under.len() > ks.len() {
                        ks = under;
                    }
                }
                // A label alone on its header line, centred over the
                // columns of figures, names them all: `(in millions)`.
                let lone =
                    l.chunks.len() == 1 && ncols >= 3 && column_of(&cols, c).is_some_and(|k| k > 0);
                if lone && ks.len() < ncols - 1 {
                    let (a, b) = (cols[1].0, cols[ncols - 1].1);
                    let mid = (c.x0 + c.x1) / 2.0;
                    if (mid - (a + b) / 2.0).abs() <= (b - a) * 0.15
                        && c.width() > cols[1].1 - cols[1].0
                    {
                        ks = (1..ncols).collect();
                    }
                }
                if ks.len() >= 2 {
                    spans.push((r, ks[0], ks[ks.len() - 1] + 1));
                }
            }
        }
    }
    // Labels each set over the middle of a group of columns, two columns'
    // width or less: groups' names centred over the runs they head (`Sex` over
    // `Markers | Position(cM) | P-value`, `Sex + Smoking` over the next
    // three). The groups tile the columns a rule under them all runs across,
    // or else every column past the stub; each label is centred over its own
    // run, within 15% of its width, as the banner passes above take a centred
    // label. Of the cuts that centre them all, the one centring them best
    // stands.
    for r in 0..head_rows.len().saturating_sub(1) {
        let mut labels: Vec<(usize, f64)> = Vec::new();
        // The spans the labels' own words make: no more than where they
        // stand, and given up for the groups found here.
        let mut own: Vec<(usize, usize, usize)> = Vec::new();
        let mut wide = false;
        for c in head_rows[r].iter().flat_map(|l| l.chunks.iter()) {
            let ks = covered(&cols, c);
            if ks.len() > 2 {
                wide = true;
            }
            if ks.len() == 2 {
                own.push((r, ks[0], ks[1] + 1));
            }
            if let Some(k) = column_of(&cols, c) {
                labels.push((k, (c.x0 + c.x1) / 2.0));
            }
        }
        labels.sort_by(|a, b| a.1.total_cmp(&b.1));
        let n = labels.len();
        // A label with an underline of its own has said what it spans, even
        // where that is just the two columns its words reach: `September 30,
        // 2016` underlined across `Carrying Amount | Fair Value`, with a
        // `Level` column before it that is neither group's.
        if wide
            || underlined.contains(&r)
            || n < 2
            || n * 2 > ncols
            || labels.iter().any(|&(k, ..)| k == 0)
            || labels.windows(2).any(|w| w[0].0 == w[1].0)
            || labels.iter().any(|&(k, ..)| {
                spans
                    .iter()
                    .any(|sp| sp.0 == r && sp.1 <= k && k < sp.2 && !own.contains(sp))
            })
        {
            continue;
        }
        let named_below: Vec<usize> = head_rows[r + 1]
            .iter()
            .flat_map(|l| l.chunks.iter())
            .filter_map(|c| column_of(&cols, c))
            .collect();
        // The stretch the groups tile: under a rule drawn below every label,
        // its columns; else every column past the stub.
        // Below the labels' middle: an underline can sit on their baseline.
        let base = head_rows[r]
            .iter()
            .map(|l| (l.y0 + l.base) / 2.0)
            .fold(f64::MIN, f64::max);
        let next_top = head_rows[r + 1]
            .iter()
            .map(|l| l.y0)
            .fold(f64::MAX, f64::min);
        let ruled = rules
            .iter()
            .filter(|ru| {
                ru.orientation == Orientation::Horizontal
                    && ru.axis_pos() > base
                    && ru.axis_pos() < next_top
                    && labels
                        .iter()
                        .all(|&(_, m)| ru.bbox.x0 <= m && ru.bbox.x1 >= m)
            })
            .map(|ru| {
                let ks: Vec<usize> = (0..ncols)
                    .filter(|&j| {
                        let (a, b) = cols[j];
                        ru.bbox.x1.min(b) - ru.bbox.x0.max(a) > (b - a) * 0.5
                    })
                    .collect();
                ks
            })
            .find(|ks| !ks.is_empty());
        let (lo, hi) = match ruled {
            Some(ks) => (ks[0], ks[ks.len() - 1] + 1),
            None => (1, ncols),
        };
        if labels.iter().any(|&(k, ..)| k < lo || k >= hi) {
            continue;
        }
        // Each cut: the first column of every group after the first.
        let mut best: Option<(f64, Vec<usize>)> = None;
        let mut cut: Vec<usize> = labels[1..].iter().map(|_| 0).collect();
        fn search(
            i: usize,
            labels: &[(usize, f64)],
            cut: &mut Vec<usize>,
            (lo, hi): (usize, usize),
            cols: &[(f64, f64)],
            named_below: &[usize],
            best: &mut Option<(f64, Vec<usize>)>,
        ) {
            if i == cut.len() {
                let mut bounds = vec![lo];
                bounds.extend(cut.iter().copied());
                bounds.push(hi);
                let mut err = 0.0;
                for (g, w) in bounds.windows(2).enumerate() {
                    let (a, b) = (cols[w[0]].0, cols[w[1] - 1].1);
                    let off = ((a + b) / 2.0 - labels[g].1).abs();
                    if off > (b - a) * 0.15 || !(w[0]..w[1]).all(|j| named_below.contains(&j)) {
                        return;
                    }
                    err += off;
                }
                if best.as_ref().is_none_or(|(e, _)| err < *e) {
                    *best = Some((err, cut.clone()));
                }
                return;
            }
            let from = if i == 0 { lo } else { cut[i - 1] } + 1;
            let from = from.max(labels[i].0 + 1);
            for b in from..=labels[i + 1].0 {
                cut[i] = b;
                search(i + 1, labels, cut, (lo, hi), cols, named_below, best);
            }
        }
        search(
            0,
            &labels,
            &mut cut,
            (lo, hi),
            &cols,
            &named_below,
            &mut best,
        );
        if let Some((_, cut)) = best {
            spans.retain(|sp| !own.contains(sp));
            let mut bounds = vec![lo];
            bounds.extend(cut);
            bounds.push(hi);
            for w in bounds.windows(2) {
                spans.push((r, w[0], w[1]));
            }
        }
    }
    // A banner centred over the columns between its neighbours that the row
    // under it names is theirs, however few of them its words cover:
    // `Average Annual Growth` over five year ranges between `AMS` and
    // `Remittance`. A rule under it as wide as its columns says what it
    // spans, and stands.
    for r in 0..head_rows.len().saturating_sub(1) {
        let labels: Vec<(usize, &Line, &Chunk)> = head_rows[r]
            .iter()
            .flat_map(|l| l.chunks.iter().map(move |c| (*l, c)))
            .filter_map(|(l, c)| column_of(&cols, c).map(|k| (k, l, c)))
            .collect();
        let named_below: Vec<usize> = head_rows[r + 1]
            .iter()
            .flat_map(|l| l.chunks.iter())
            .filter_map(|c| column_of(&cols, c))
            .collect();
        for &(k, l, c) in &labels {
            let ks = covered(&cols, c);
            // A rule narrower than the label's own columns says nothing of
            // what it spans.
            let ruled = group_run(&head_rows[r], l, c)
                .or_else(|| rule_under(l, c))
                .is_some_and(|(a, b)| {
                    covered(
                        &cols,
                        &Chunk {
                            x0: a,
                            x1: b,
                            ..c.clone()
                        },
                    )
                    .len()
                        >= ks.len()
                });
            if ks.len() < 2 || ruled {
                continue;
            }
            // Its neighbours by the columns they cover, not their centres:
            // a banner over two columns holds both.
            let spread = |d: &Chunk, j: usize| {
                let ds = covered(&cols, d);
                if ds.is_empty() {
                    (j, j)
                } else {
                    (ds[0], ds[ds.len() - 1])
                }
            };
            // Nor into a span a banner to its left was just given.
            let lo = labels
                .iter()
                .filter(|(j, _, d)| !std::ptr::eq(*d, c) && spread(d, *j).1 < ks[0])
                .map(|(j, _, d)| spread(d, *j).1 + 1)
                .chain(
                    spans
                        .iter()
                        .filter(|&&(sr, _, c1)| sr == r && c1 <= ks[ks.len() - 1])
                        .map(|&(_, _, c1)| c1),
                )
                .max()
                .unwrap_or(1);
            let hi = labels
                .iter()
                .filter(|(j, _, d)| !std::ptr::eq(*d, c) && spread(d, *j).0 > ks[ks.len() - 1])
                .map(|(j, _, d)| spread(d, *j).0)
                .min()
                .unwrap_or(ncols);
            if lo >= ks[0] && hi <= ks[ks.len() - 1] + 1 {
                continue;
            }
            let (a, b) = (cols[lo].0, cols[hi - 1].1);
            let centred = ((c.x0 + c.x1) / 2.0 - (a + b) / 2.0).abs() <= (b - a) * 0.15;
            if centred && lo <= k && (lo..hi).all(|j| named_below.contains(&j)) {
                spans.retain(|&(sr, c0, c1)| !(sr == r && c0 <= k && k < c1));
                spans.push((r, lo, hi));
            }
        }
    }
    // A banner row's label names the columns to its right as far as the
    // next label, where the row under it names them: `EUR` over `Unit` and
    // `Total`.
    for r in 0..head_rows.len().saturating_sub(1) {
        let mut at: Vec<(usize, usize)> = head_rows[r]
            .iter()
            .flat_map(|l| l.chunks.iter())
            .filter_map(|c| column_of(&cols, c).map(|k| (k, covered(&cols, c).len())))
            .collect();
        at.sort();
        let named_below: Vec<usize> = head_rows[r + 1]
            .iter()
            .flat_map(|l| l.chunks.iter())
            .filter_map(|c| column_of(&cols, c))
            .collect();
        // Labels read up from the rows below span down, not across.
        at.retain(|&(k, _)| !(r == 0 && lifted_cols.contains(&k)));
        if at.is_empty() || at.len() * 2 > ncols || at.iter().any(|&(k, _)| k == 0) {
            continue;
        }
        for (i, &(k, n)) in at.iter().enumerate() {
            if n >= 2
                || spans
                    .iter()
                    .any(|&(sr, c0, c1)| sr == r && c0 <= k && k < c1)
            {
                continue;
            }
            // As far as the next label, or a span given to one already.
            let next = at.get(i + 1).map_or(ncols, |&(k2, _)| k2).min(
                spans
                    .iter()
                    .filter(|&&(sr, c0, _)| sr == r && c0 > k)
                    .map(|&(_, c0, _)| c0)
                    .min()
                    .unwrap_or(ncols),
            );
            let end = (k + 1..next).take_while(|c| named_below.contains(c)).last();
            if let Some(e) = end {
                spans.push((r, k, e + 1));
            }
        }
    }
    let header_rows = head_rows.len();
    let sub_headers: Vec<usize> = body_rows
        .iter()
        .enumerate()
        .filter(|(_, (_, s))| *s)
        .map(|(i, _)| header_rows + i)
        .collect();
    for &r in &sub_headers {
        spans.push((r, 0, ncols));
    }
    let restated_headers: Vec<usize> = restated_rows.iter().map(|&i| header_rows + i).collect();
    // Each chunk's glyphs into its cell: a header label across columns into
    // the first it names, a section label into the first column.
    let mut cells: Vec<Vec<usize>> = vec![Vec::new(); all_rows.len() * ncols];
    for (r, ls) in all_rows.iter().enumerate() {
        let section = sub_headers.contains(&r);
        for l in ls {
            let mut prev: Option<(usize, &Chunk)> = None;
            for c in &l.chunks {
                let k = if section {
                    0
                } else if r < header_rows {
                    let ks = covered(&cols, c);
                    if ks.len() >= 2 {
                        ks[0]
                    } else {
                        column_of(&cols, c).unwrap_or(0)
                    }
                } else {
                    column_of(&cols, c).unwrap_or(0)
                };
                if let Some((pk, p)) = prev {
                    if pk == k {
                        cells[r * ncols + k].extend(&p.trailing);
                    }
                }
                cells[r * ncols + k].extend(&c.glyphs);
                prev = Some((k, c));
            }
        }
    }
    let downs = [
        downs,
        body_downs(&all_rows, &cells, rules, &xs, header_rows, &sub_headers),
    ]
    .concat();
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
        layout: Some(std::sync::Arc::new(TableLayout {
            header_rows,
            spans,
            downs,
            sub_headers,
            restated_headers,
            cells,
            claimed: Vec::new(),
        })),
    })
}

/// Body cells that span rows, read from the groups the ruling draws:
/// `(first row, column, end row)`.
///
/// A table read here takes each line as a row. One that rules its rows off
/// in groups, a rule across the whole table under each, sets some columns
/// once per group, on its first line, and others line by line: a tip size
/// and its colour over the four volumes tested with it, each volume with
/// its own errors. A column with a value on a group's first line and none
/// on its others, where another column fills every line, is one cell over
/// the group, as [`Grid::text_merges`] reads it in a ruled grid.
///
/// Claimed only for groups closed by rules across the table above and
/// below, and only where two groups or more show it. A table ruled under
/// its header and at its foot is one group, and a sparse column in it is
/// values missing from some rows, not a span.
fn body_downs(
    rows: &[Vec<&Line>],
    cells: &[Vec<usize>],
    rules: &[Rule],
    xs: &[f64],
    header_rows: usize,
    sections: &[usize],
) -> Vec<(usize, usize, usize)> {
    let ncols = xs.len().saturating_sub(1);
    let n = rows.len();
    if ncols < 2 || n < header_rows + 4 {
        return Vec::new();
    }
    let (left, right) = (xs[0], xs[ncols]);
    let flat: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.orientation == Orientation::Horizontal)
        .collect();
    // A rule across the whole table between row `r - 1` and row `r`, or
    // under the last row where `r` is `n`.
    let across = |r: usize| -> bool {
        let Some(upper) = rows.get(r.wrapping_sub(1)) else {
            return false;
        };
        let base = upper.iter().map(|l| l.base).fold(f64::MIN, f64::max);
        let fs = upper.iter().map(|l| l.fs).fold(1.0, f64::max);
        let hi = match rows
            .get(r)
            .and_then(|ls| ls.iter().min_by(|a, b| a.base.total_cmp(&b.base)))
        {
            Some(lower) => lower.base - lower.fs * 0.5,
            None => base + fs * 1.5,
        };
        let cover: f64 = flat
            .iter()
            .filter(|h| h.bbox.y0 >= base - fs * 0.2 && h.bbox.y0 <= hi)
            .map(|h| (h.bbox.x1.min(right) - h.bbox.x0.max(left)).max(0.0))
            .sum();
        cover >= 0.9 * (right - left)
    };
    let cuts: Vec<usize> = (header_rows..=n)
        .filter(|&r| sections.contains(&r) || across(r))
        .collect();
    let empty = |r: usize, k: usize| cells[r * ncols + k].is_empty();
    let mut downs = Vec::new();
    let mut groups = 0;
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b - a < 2 || (a..b).any(|r| sections.contains(&r)) || !across(a) || !across(b) {
            continue;
        }
        let full = (0..ncols).any(|k| (a..b).all(|r| !empty(r, k)));
        let once: Vec<usize> = (0..ncols)
            .filter(|&k| !empty(a, k) && (a + 1..b).all(|r| empty(r, k)))
            .collect();
        if full && !once.is_empty() {
            groups += 1;
            downs.extend(once.into_iter().map(|k| (a, k, b)));
        }
    }
    if groups < 2 {
        return Vec::new();
    }
    downs
}

/// Record on a table read here the glyphs its cells take from the page.
pub fn claim(g: &mut Grid, glyphs: &[Glyph]) {
    if let Some(lay) = g.layout.as_mut() {
        let lay = std::sync::Arc::make_mut(lay);
        let mut ids: Vec<usize> = lay
            .cells
            .iter()
            .flatten()
            .map(|&i| glyphs[i].draw_index)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        lay.claimed = ids;
    }
}

/// Cut each band of a grid ruled by its figures' underlines
/// ([`crate::table::ruled_by_underlines`]) that holds two lines of figures
/// or more into a row per line. Such rules close a group of rows, a rule
/// under the figures a subtotal sums and a double rule under a total, and
/// the rows between them are ruled by nothing: each line of figures is a
/// row, and so is each line between them (a section's label over its
/// rows). A band with fewer lines of figures is left whole: a header's
/// labels wrapped over its lines, or a figure with a note under it.
pub fn split_underlined_bands(g: &mut Grid, lines: &[Line]) {
    let cols = g.cols();
    if cols < 3 || g.layout.is_some() {
        return;
    }
    // A line's chunks inside the grid, each with the column its middle is in.
    let xs = g.xs.clone();
    fn inside<'a>(l: &'a Line, xs: &[f64]) -> Vec<(usize, &'a Chunk)> {
        let (x0, x1) = (xs[0], xs[xs.len() - 1]);
        l.chunks
            .iter()
            .filter_map(|c| {
                let m = (c.x0 + c.x1) / 2.0;
                (m > x0 && m < x1).then(|| {
                    (
                        xs.windows(2)
                            .position(|w| m >= w[0] && m < w[1])
                            .unwrap_or(0),
                        c,
                    )
                })
            })
            .collect()
    }
    // Figures in two of the columns after the first, the labels' column.
    let figured = |l: &Line| {
        let mut with: Vec<usize> = inside(l, &xs)
            .into_iter()
            .filter(|(k, c)| *k > 0 && is_figure(&c.text) && !is_year(&c.text))
            .map(|(k, _)| k)
            .collect();
        with.dedup();
        with.len() >= 2
    };
    let mut cuts = Vec::new();
    for w in g.ys.windows(2) {
        let band: Vec<(f64, f64)> = lines
            .iter()
            .filter(|l| l.base > w[0] && l.base <= w[1])
            .filter_map(|l| {
                let held = inside(l, &xs);
                let top = held.iter().map(|(_, c)| c.y0).fold(f64::MAX, f64::min);
                let bottom = held.iter().map(|(_, c)| c.y1).fold(f64::MIN, f64::max);
                (!held.is_empty()).then_some((top, bottom))
            })
            .collect();
        let rows_of_figures = lines
            .iter()
            .filter(|l| l.base > w[0] && l.base <= w[1] && figured(l))
            .count();
        if rows_of_figures < 2 {
            continue;
        }
        for p in band.windows(2) {
            // Midway between one line's ink and the next one's; lines whose
            // ink overlaps are not cut.
            if p[1].0 > p[0].1 {
                cuts.push((p[0].1 + p[1].0) / 2.0);
            }
        }
    }
    if cuts.is_empty() {
        return;
    }
    g.ys.extend(cuts);
    g.ys.sort_by(f64::total_cmp);
}

/// The regions of grids ruled with horizontals only: their bands are not
/// rows and their segments' ends are not columns.
pub fn unruled_grids(
    grids: &[Grid],
    rules: &[Rule],
    fills: &[crate::rule::Fill],
    lines: &[Line],
) -> Vec<usize> {
    (0..grids.len())
        .filter(|&i| {
            !has_vertical(rules, &grids[i].bbox)
                && !(ruled_row_by_row(&grids[i], rules, fills)
                    && !lumped(&grids[i], lines)
                    && !labels_outside(&grids[i], lines))
        })
        .collect()
}

/// A reading that puts in one cell figures the seed's ruling sets in
/// different columns: figures on both sides of one of the ruled grid's
/// column edges, no glyph across it (`$ 4,137 $ 3,610 $ 3,547` under one
/// year, where the rules are drawn a column at a time). Its text read so is
/// no table; left as text, it says nothing wrong. A figure's glyphs set
/// across an edge say the edge is no column's.
pub fn merges_drawn_columns(g: &Grid, edges: &[f64], within: &BBox, glyphs: &[Glyph]) -> bool {
    let Some(lay) = g.layout.as_deref() else {
        return false;
    };
    lay.cells.iter().any(|cell| {
        // The cell's glyphs among the stack's rows: its edges say nothing of
        // a table above or below it read with it.
        let inked: Vec<(&Glyph, BBox)> = cell
            .iter()
            .filter_map(|&i| glyphs.get(i))
            .filter_map(|gl| gl.bbox.map(|b| (gl, b)))
            .filter(|(_, b)| {
                let y = (b.y0 + b.y1) / 2.0;
                y > within.y0 && y < within.y1
            })
            .collect();
        edges.iter().any(|&e| {
            let figure = |gl: &Glyph| gl.text.chars().any(|c| c.is_ascii_digit());
            let fs = inked
                .iter()
                .map(|(gl, _)| gl.font_size as f64)
                .fold(0.0, f64::max)
                .max(1.0);
            // A quarter of the type's size clear of the edge, each side;
            // a closing bracket hung past a column's edge, as statements set
            // them, is the column's own and crosses nothing.
            let clear = fs * 0.25;
            let hung = |gl: &Glyph| matches!(gl.text.as_str(), ")" | "]");
            !inked
                .iter()
                .any(|(gl, b)| b.x0 < e && b.x1 > e && !hung(gl))
                && inked
                    .iter()
                    .any(|(gl, b)| figure(gl) && (b.x0 + b.x1) / 2.0 < e - clear)
                && inked
                    .iter()
                    .any(|(gl, b)| figure(gl) && (b.x0 + b.x1) / 2.0 > e + clear)
        })
    })
}

/// A grid with no vertical, ruled under its rows as a table is and not
/// lumped, with lines of the page's text standing to its left on half its
/// rows: a table set beside a column of the page's prose, in a column of its
/// own. All of it is the grid's geometry and the lines around it.
pub fn ruled_beside_prose(
    g: &Grid,
    rules: &[Rule],
    fills: &[crate::rule::Fill],
    lines: &[Line],
) -> bool {
    !has_vertical(rules, &g.bbox)
        && ruled_row_by_row(g, rules, fills)
        && !lumped(g, lines)
        && labels_outside(g, lines)
}

/// Its rows' labels stand outside it, to its left: the grid is a
/// statement's columns of figures, ruled by their underlines alone.
fn labels_outside(g: &Grid, lines: &[Line]) -> bool {
    let rows: Vec<&Line> = lines
        .iter()
        .filter(|l| l.base > g.bbox.y0 && l.base <= g.bbox.y1)
        .collect();
    let labelled = rows
        .iter()
        .filter(|l| {
            l.chunks
                .iter()
                .any(|c| c.x1 <= g.bbox.x0 + 1.0 && c.text.chars().any(char::is_alphabetic))
        })
        .count();
    labelled * 2 >= rows.len().max(1)
}

/// Some band of the grid holds two lines or more that each spread across
/// it: rows its ruling lumped together. A tall row's further lines (dates
/// under a description, tax lines indented under an item) sit in one place,
/// and values wrapped in every column under a labelled line (`1.47 lb (.67
/// kg)` over `±10%`), the first column left empty, carry that row on.
fn lumped(g: &Grid, lines: &[Line]) -> bool {
    let first = (g.xs[0], g.xs.get(1).copied().unwrap_or(g.bbox.x1));
    g.ys.windows(2).any(|w| {
        let mut labelled_above = false;
        let mut count = 0;
        for l in lines.iter().filter(|l| l.base > w[0] && l.base <= w[1]) {
            let inside: Vec<f64> = l
                .chunks
                .iter()
                .map(|c| (c.x0 + c.x1) / 2.0)
                .filter(|&m| m >= g.bbox.x0 && m <= g.bbox.x1)
                .collect();
            let labelled = inside.iter().any(|&m| m >= first.0 && m < first.1);
            // A line of values carrying on under a labelled line: each value
            // wrapped under itself. Lines with no label under none (a
            // statement's header, years over units) are lines of their own.
            if inside.len() >= 3 && (labelled || !labelled_above) {
                count += 1;
            }
            labelled_above = labelled;
        }
        count >= 2
    })
}

/// A grid with a rule across it at every boundary between its bands: each
/// band is one row, however many lines it holds (a description, its service
/// dates, the tax lines indented under it). Three rules around a journal
/// table's header and body, or the short rules under a statement's figures,
/// are not that. The edge of a band shaded across the table is a line as
/// plain as a rule: a section's band drawn with no rule over or under it.
/// Not a table striped row by row: its stripes shade the rows its rules make.
fn ruled_row_by_row(g: &Grid, rules: &[Rule], fills: &[crate::rule::Fill]) -> bool {
    let width = g.bbox.x1 - g.bbox.x0;
    // Bands across the table, edge to edge: a section's band drawn a cell
    // at a time. Not stripes, which shade the rows the rules make, nor a
    // row of cells a group's cell beside it leaves short.
    let bands = crate::table::section_bands(fills, &g.xs, &g.ys);
    g.rows() >= 4
        && g.ys[1..g.ys.len() - 1].iter().all(|&y| {
            let spans: Vec<(f64, f64)> = rules
                .iter()
                .filter(|r| {
                    r.orientation == Orientation::Horizontal && (r.axis_pos() - y).abs() <= 3.0
                })
                .map(|r| (r.bbox.x0, r.bbox.x1))
                .chain(
                    bands
                        .iter()
                        .filter(|f| (f.bbox.y0 - y).abs() <= 3.0 || (f.bbox.y1 - y).abs() <= 3.0)
                        .map(|f| (f.bbox.x0, f.bbox.x1)),
                )
                .map(|(a, b)| (a.max(g.bbox.x0), b.min(g.bbox.x1)))
                .filter(|(a, b)| b > a)
                .collect();
            spans.iter().map(|(a, b)| b - a).sum::<f64>() >= 0.9 * width
        })
}

/// Runs of lines carrying figures in two places or more that no grid holds:
/// a statement's rows, ruled by nothing but underlines.
pub fn figure_runs(lines: &[Line], rules: &[Rule], taken: &[BBox]) -> Vec<BBox> {
    // What no grid holds of each line.
    let lines: Vec<Line> = lines
        .iter()
        .map(|l| Line {
            chunks: l
                .chunks
                .iter()
                .filter(|c| {
                    let (x, y) = ((c.x0 + c.x1) / 2.0, (c.y0 + c.y1) / 2.0);
                    !taken
                        .iter()
                        .any(|b| x >= b.x0 && x <= b.x1 && y >= b.y0 && y <= b.y1)
                })
                .cloned()
                .collect(),
            ..l.clone()
        })
        .filter(|l| !l.chunks.is_empty())
        .collect();
    let lines = &lines[..];
    let free = |_: &Line| true;
    let figured = |l: &Line| {
        let figs = l
            .chunks
            .iter()
            .filter(|c| is_figure(&c.text) && !is_year(&c.text))
            .count();
        figs >= 2
            || (figs == 1 && l.chunks.len() >= 2 && is_figure(&l.chunks[l.chunks.len() - 1].text))
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !(figured(&lines[i]) && free(&lines[i])) {
            i += 1;
            continue;
        }
        let mut j = i;
        let mut k = i + 1;
        // Up to two lines of labels between rows of figures.
        while k < lines.len() && k <= j + 3 {
            let (above, next) = (&lines[k - 1], &lines[k]);
            let reach = |l: &Line| {
                l.chunks
                    .iter()
                    .fold((f64::MAX, f64::MIN), |(a, b), c| (a.min(c.x0), b.max(c.x1)))
            };
            let (a0, a1) = reach(above);
            let (n0, n1) = reach(next);
            // A section's label: one short chunk, no figures in it, set at the
            // label column of the row above.
            let label = next.chunks.len() == 1
                && !figured(next)
                && section_label(&next.chunks[0].text)
                && above
                    .chunks
                    .first()
                    .is_some_and(|c| (next.chunks[0].x0 - c.x0).abs() <= next.fs);
            if !row_gap(
                rules,
                a0.min(n0),
                a1.max(n1),
                above.y1,
                next.y0,
                next.fs,
                label,
            ) || !free(next)
            {
                break;
            }
            if figured(&lines[k]) {
                j = k;
            }
            k += 1;
        }
        if j > i {
            let figs: Vec<&Chunk> = lines[i..=j]
                .iter()
                .filter(|l| figured(l))
                .flat_map(|l| l.chunks.iter().filter(|c| is_figure(&c.text)))
                .collect();
            let x0 = figs.iter().map(|c| c.x0).fold(f64::MAX, f64::min);
            let x1 = figs.iter().map(|c| c.x1).fold(f64::MIN, f64::max);
            out.push(BBox {
                x0,
                y0: lines[i].y0,
                x1,
                y1: lines[j].y1,
            });
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// A cell's dot leaders, dropped: `Basic . . . . .` is `Basic`.
pub fn strip_leaders(s: &str) -> String {
    strip_leaders_of(s, 3)
}

/// [`strip_leaders`], where a run of `min` dots or more is a leader: a
/// table read by other means than its text's columns holds sentences too,
/// and an ellipsis closing one (`If you pay...`) is no leader.
pub fn strip_leaders_of(s: &str, min: usize) -> String {
    let t = s.trim_end();
    let mut end = t.len();
    let mut dots = 0;
    for (i, ch) in t.char_indices().rev() {
        match ch {
            '.' | '·' | '․' => dots += 1,
            '…' => dots += 3,
            ' ' => {}
            _ => break,
        }
        end = i;
    }
    if dots >= min && end > 0 {
        t[..end].trim_end().to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_band_runs_on_across_its_rule() {
        // Rows at 9 pt, a section's rule across the table at 340 with a
        // row's space each side: the band runs on under its label.
        let rule = |x0: f64, x1: f64, y: f64| Rule {
            bbox: BBox {
                x0,
                y0: y - 0.25,
                x1,
                y1: y + 0.25,
            },
            orientation: Orientation::Horizontal,
            page: 0,
        };
        let across = [rule(60.0, 540.0, 340.0)];
        // A row's gap runs on, ruled or not.
        assert!(row_gap(&[], 60.0, 540.0, 300.0, 318.0, 9.0, false));
        // Twice a row's gap with a rule across it, over a section's label.
        assert!(row_gap(&across, 60.0, 540.0, 328.0, 356.0, 9.0, true));
        // Not over a line that is no label: the table has ended.
        assert!(!row_gap(&across, 60.0, 540.0, 328.0, 356.0, 9.0, false));
        // Nor where the rule under it groups a few columns only.
        let group = [rule(300.0, 420.0, 340.0)];
        assert!(!row_gap(&group, 60.0, 540.0, 328.0, 356.0, 9.0, true));
        // Nor past twice a row's gap.
        assert!(!row_gap(&across, 60.0, 540.0, 300.0, 356.0, 9.0, true));
    }

    #[test]
    fn a_section_label_is_a_few_words_and_no_caption() {
        assert!(section_label("Adverse events"));
        assert!(section_label("Secondary outcomes"));
        assert!(!section_label("Table 5. Ratings by observer group"));
        assert!(!section_label(
            "Values are means and standard deviations of the ratings"
        ));
    }

    #[test]
    fn a_stub_head_is_set_as_a_title_and_prose_is_not() {
        assert!(set_as_title(
            "Sales Volumes per Day by Products from Ongoing Operations"
        ));
        assert!(set_as_title(
            "(Amounts in Millions Except Treasury Stock Shares in Thousands)"
        ));
        // Sentence case, a sentence's stops, a citation: prose.
        assert!(!set_as_title(
            "Then a novel feature selection technique is introduced and performed"
        ));
        assert!(!set_as_title(
            "LVEF measurement; as the predictive ability of LVEF was"
        ));
        assert!(!set_as_title(
            "health care expenditures [20]. In addition, we searched"
        ));
    }
    use crate::rule::Rule;

    #[test]
    fn blocks_restating_their_column_names_read_as_one_table() {
        // Three blocks, each under its own title and opening on the same line
        // of column names, then three rows, set as far apart as rows are.
        let xs = [20.0, 120.0, 180.0, 240.0];
        let mut gl = Vec::new();
        let mut y = 40.0;
        for (title, rows) in [
            (
                "Bolivia 1994",
                [
                    ("Urban", "4.3", "10.7", "1.2"),
                    ("Rural", "7.1", "18.5", "-0.9"),
                    ("Ratio", "1.7", "1.7", ""),
                ],
            ),
            (
                "Colombia 1990",
                [
                    ("Urban", "3.0", "8.5", "1.0"),
                    ("Rural", "5.8", "14.3", "1.3"),
                    ("Ratio", "1.9", "1.7", ""),
                ],
            ),
            (
                "Haiti 1994",
                [
                    ("Urban", "4.9", "10.2", "-1.0"),
                    ("Rural", "4.4", "10.6", "-0.5"),
                    ("Ratio", "0.9", "1.0", ""),
                ],
            ),
        ] {
            gl.extend(set(title, xs[1], y));
            gl.extend(set("Change", xs[3], y));
            y += 18.0;
            for (k, name) in ["<16", "16/17", "<20"].iter().enumerate() {
                gl.extend(set(name, xs[k + 1], y));
            }
            y += 18.0;
            for row in rows {
                for (k, t) in [row.0, row.1, row.2, row.3].iter().enumerate() {
                    gl.extend(set(t, xs[k], y));
                }
                y += 18.0;
            }
        }
        let region = BBox {
            x0: 10.0,
            y0: 47.0,
            x1: 300.0,
            y1: y,
        };
        let grids = read(&lines(&gl), &[], region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        // No header: each block's title is a section and each block's line of
        // column names restates the columns for its own rows.
        let lay = grids[0].layout.as_deref().expect("read from its text");
        assert_eq!(lay.header_rows, 0, "{lay:?}");
        assert_eq!(lay.sub_headers, [0, 5, 10], "{lay:?}");
        assert_eq!(lay.restated_headers, [1, 6, 11], "{lay:?}");
        let texts = grids[0].cell_texts(&gl);
        assert_eq!(
            texts.iter().filter(|t| *t == "16/17").count(),
            3,
            "{texts:?}"
        );
        // The second block's title, in its section's row.
        let title: String = texts[5 * 4..6 * 4].join(" ");
        assert!(
            title.contains("Colombia") && title.contains("Change"),
            "{texts:?}"
        );
        assert_eq!(
            texts.iter().filter(|t| *t == "Urban").count(),
            3,
            "{texts:?}"
        );
    }

    #[test]
    fn a_data_row_repeated_under_each_section_restates_nothing() {
        // `Number of patients` with its counts opens each section again: a
        // row of data, named in the stub, not a line of column names.
        let xs = [20.0, 160.0, 240.0];
        let mut gl = Vec::new();
        gl.extend(set("Characteristics", xs[0], 40.0));
        gl.extend(set("Dose 23", xs[1], 40.0));
        gl.extend(set("Dose 10", xs[2], 40.0));
        let mut y = 58.0;
        for (section, label, a, b) in [
            ("Age", "Mean", "73.8", "73.9"),
            ("Gender", "Males", "335", "175"),
            ("Score", "Mean", "13.1", "13.2"),
        ] {
            gl.extend(set(section, xs[0], y));
            y += 18.0;
            for (t, u, v) in [("Number of patients", "909", "462"), (label, a, b)] {
                gl.extend(set(t, xs[0], y));
                gl.extend(set(u, xs[1], y));
                gl.extend(set(v, xs[2], y));
                y += 18.0;
            }
        }
        let region = BBox {
            x0: 10.0,
            y0: 47.0,
            x1: 300.0,
            y1: y,
        };
        let grids = read(&lines(&gl), &[], region, 0);
        assert!(!grids.is_empty(), "{grids:?}");
        for g in &grids {
            let lay = g.layout.as_deref().expect("read from its text");
            assert!(lay.restated_headers.is_empty(), "{lay:?}");
        }
    }

    #[test]
    fn group_labels_centred_over_their_columns_span_them() {
        // Two groups of three columns past the stub, each named by a label
        // centred over the middle of its three, with no rule under it.
        let mut gl = Vec::new();
        let xs = [20.0, 120.0, 170.0, 220.0, 290.0, 340.0, 390.0];
        gl.extend(set("Sex", 170.0, 40.0));
        gl.extend(set("Sex + Smoking", 340.0 - 5.5 * 4.5, 40.0));
        for (k, t) in ["Chr", "Marker", "Pos", "P", "Marker", "Pos", "P"]
            .iter()
            .enumerate()
        {
            gl.extend(set(t, xs[k], 54.0));
        }
        for i in 0..4 {
            let y = 68.0 + 12.0 * i as f64;
            gl.extend(set(&format!("{}", 2 + i), xs[0], y));
            for (k, &x) in xs.iter().enumerate().skip(1) {
                let v = if k % 3 == 1 {
                    format!("D{}S{}", i, k)
                } else {
                    format!("{}.{}", k, i)
                };
                gl.extend(set(&v, x, y));
            }
        }
        let region = BBox {
            x0: 10.0,
            y0: 60.0,
            x1: 430.0,
            y1: 110.0,
        };
        let grids = read(&lines(&gl), &[], region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        let spans = &grids[0].layout.as_ref().unwrap().spans;
        assert!(spans.contains(&(0, 1, 4)), "{spans:?}");
        assert!(spans.contains(&(0, 4, 7)), "{spans:?}");
    }

    #[test]
    fn underlined_group_labels_leave_a_shared_column_out() {
        // A narrow `Level` column past the stub, then two groups of two
        // columns, each named by a label whose words reach across both,
        // centred over them and underlined across them. Tiling every column
        // past the stub would hand `Level` to the first group.
        let under = |x0: f64, x1: f64| Rule {
            bbox: BBox {
                x0,
                y0: 43.0,
                x1,
                y1: 43.5,
            },
            orientation: crate::rule::Orientation::Horizontal,
            page: 0,
        };
        let rules = vec![under(210.0, 334.5), under(350.0, 474.5)];
        let mut gl = Vec::new();
        let xs = [20.0, 170.0, 210.0, 280.0, 350.0, 420.0];
        gl.extend(set("September 30, 2016", 223.0, 40.0));
        gl.extend(set("September 30, 2015", 363.0, 40.0));
        for (k, t) in ["Level", "Carrying", "Fair", "Carrying", "Fair"]
            .iter()
            .enumerate()
        {
            gl.extend(set(t, xs[k + 1], 54.0));
        }
        for (i, stub) in ["Cash", "Swaps", "Notes", "Loans"].iter().enumerate() {
            let y = 68.0 + 12.0 * i as f64;
            gl.extend(set(stub, xs[0], y));
            gl.extend(set(&format!("{}", 1 + i % 2), xs[1], y));
            for (k, &x) in xs.iter().enumerate().skip(2) {
                gl.extend(set(&format!("${},{}86,99{}", k, i, k), x, y));
            }
        }
        let region = BBox {
            x0: 10.0,
            y0: 60.0,
            x1: 490.0,
            y1: 110.0,
        };
        let grids = read(&lines(&gl), &rules, region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        let spans = &grids[0].layout.as_ref().unwrap().spans;
        assert!(spans.contains(&(0, 2, 4)), "{spans:?}");
        assert!(spans.contains(&(0, 4, 6)), "{spans:?}");
    }

    #[test]
    fn a_header_framed_by_rules_is_read_across_a_rule_and_a_gap_inside_it() {
        // Rules across the table over its header, inside it and under it.
        // Over the inner rule: the columns' names, and a gap above them a
        // group's name across two of them; under it, a group's two columns.
        let across = |y: f64| Rule {
            bbox: BBox {
                x0: 10.0,
                y0: y,
                x1: 330.0,
                y1: y + 0.5,
            },
            orientation: crate::rule::Orientation::Horizontal,
            page: 0,
        };
        let rules = vec![across(20.0), across(66.0), across(90.0)];
        let xs = [20.0, 120.0, 190.0, 260.0];
        let mut gl = Vec::new();
        gl.extend(set("Survey of all patients", 182.0, 32.0));
        for (k, t) in ["Day", "Hours", "Cases", "Total"].iter().enumerate() {
            gl.extend(set(t, xs[k], 58.0));
        }
        gl.extend(set("Mean", 190.0, 82.0));
        gl.extend(set("Max", 260.0, 82.0));
        for (i, row) in [
            ["Mon", "800", "6", "22"],
            ["Tues", "830", "3", "14"],
            ["Wed", "900", "2", "15"],
        ]
        .iter()
        .enumerate()
        {
            let y = 104.0 + 14.0 * i as f64;
            for (k, t) in row.iter().enumerate() {
                gl.extend(set(t, xs[k], y));
            }
        }
        let region = BBox {
            x0: 10.0,
            y0: 92.0,
            x1: 330.0,
            y1: 140.0,
        };
        let grids = read(&lines(&gl), &rules, region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        let lay = grids[0].layout.as_deref().expect("read from its text");
        assert!(lay.header_rows >= 3, "{lay:?}");
        let texts = grids[0].cell_texts(&gl);
        assert!(texts.iter().any(|t| t == "Day"), "{texts:?}");
        // The group's words, over the inner rule and a gap, in the header.
        let head = texts[..lay.header_rows * grids[0].cols()].join(" ");
        assert!(head.contains("Survey of all"), "{texts:?}");
    }

    /// A rule across the table at `y`, from `x0` to `x1`.
    fn rule_across(x0: f64, x1: f64, y: f64) -> Rule {
        Rule {
            bbox: BBox {
                x0,
                y0: y,
                x1,
                y1: y + 0.5,
            },
            orientation: crate::rule::Orientation::Horizontal,
            page: 0,
        }
    }

    #[test]
    fn rows_of_words_ruled_off_as_the_rows_below_are_no_framed_header() {
        // Cases as columns, a rule under every row: the first rows hold
        // words (`Gender | Male | Female`), each labelled in the stub, the
        // later ones figures. The rules part rows; they frame no header.
        let xs = [20.0, 170.0, 240.0, 310.0];
        let mut gl = Vec::new();
        let mut rules = vec![rule_across(10.0, 350.0, 20.0)];
        for (k, t) in ["Case 1", "Case 2", "Case 3"].iter().enumerate() {
            gl.extend(set(t, xs[k + 1], 32.0));
        }
        let rows: [[&str; 4]; 6] = [
            ["Gender", "Male", "Female", "Male"],
            ["Age at onset", "22 months", "4 years", "16 years"],
            ["Blood group", "A pos", "B pos", "O neg"],
            ["Criteria met", "4", "5", "4"],
            ["Hemoglobin", "94", "117", "114"],
            ["Platelets", "310", "254", "402"],
        ];
        for (i, row) in rows.iter().enumerate() {
            let y = 50.0 + 16.0 * i as f64;
            rules.push(rule_across(10.0, 350.0, y - 11.0));
            for (k, t) in row.iter().enumerate() {
                gl.extend(set(t, xs[k], y));
            }
        }
        let region = BBox {
            x0: 10.0,
            y0: 96.0,
            x1: 350.0,
            y1: 140.0,
        };
        let grids = read(&lines(&gl), &rules, region, 0);
        assert!(!grids.is_empty(), "{grids:?}");
        let g = &grids[0];
        let lay = g.layout.as_deref().expect("read from its text");
        let texts = g.cell_texts(&gl);
        let head = texts[..lay.header_rows * g.cols()].join(" ");
        assert!(!head.contains("Gender"), "{texts:?}");
        assert!(!head.contains("Age at onset"), "{texts:?}");
    }

    #[test]
    fn a_labels_second_line_over_the_tables_rule_names_its_own_column() {
        // A label wrapped in the first column past the stub (`Age` over
        // `(Years)`) beside two groups, a rule across the whole table under
        // its second line, and the groups' columns under that. The rule is
        // the header's own: `(Years)` goes on naming `Age`'s column, not
        // every column, nor the column beside it.
        let xs = [20.0, 130.0, 180.0, 220.0, 260.0, 300.0];
        let mut gl = Vec::new();
        let rules = vec![
            rule_across(10.0, 330.0, 20.0),
            rule_across(10.0, 330.0, 50.0),
            rule_across(10.0, 330.0, 66.0),
        ];
        gl.extend(set("Age", 130.0, 34.0));
        gl.extend(set("Boys", 190.0, 34.0));
        gl.extend(set("Girls", 270.0, 34.0));
        gl.extend(set("(Years)", 124.0, 44.0));
        for (k, t) in ["n", "SD", "n", "SD"].iter().enumerate() {
            gl.extend(set(t, xs[k + 2], 60.0));
        }
        for (i, (label, age, vals)) in [
            ("Height", "< 5", ["33", "5.02", "28", "5.11"]),
            ("", "5 to 6", ["214", "4.70", "235", "4.80"]),
            ("Weight", "< 5", ["33", "2.79", "28", "3.31"]),
            ("", "5 to 6", ["214", "2.84", "235", "3.03"]),
        ]
        .iter()
        .enumerate()
        {
            let y = 80.0 + 12.0 * i as f64;
            if !label.is_empty() {
                gl.extend(set(label, xs[0], y));
            }
            gl.extend(set(age, xs[1], y));
            for (k, v) in vals.iter().enumerate() {
                gl.extend(set(v, xs[k + 2], y));
            }
        }
        let region = BBox {
            x0: 10.0,
            y0: 72.0,
            x1: 330.0,
            y1: 120.0,
        };
        let grids = read(&lines(&gl), &rules, region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        let g = &grids[0];
        let lay = g.layout.as_deref().expect("read from its text");
        let texts = g.cell_texts(&gl);
        // `(Years)` is `Age`'s second line, its column's; no header row
        // of its own runs across every column.
        assert!(texts.iter().any(|t| t == "Age (Years)"), "{texts:?}");
        assert_eq!(lay.header_rows, 2, "{texts:?} {lay:?}");
        assert!(
            !lay.spans.iter().any(|&(_, c0, c1)| c1 - c0 >= g.cols()),
            "{lay:?}"
        );
    }

    #[test]
    fn a_line_of_units_under_the_groups_is_the_headers() {
        // Two groups, each over a count and a percentage: the line of units
        // under them, nothing in the stub, names the columns; the rows give
        // their values under it.
        let xs = [20.0, 140.0, 190.0, 250.0, 300.0];
        let mut gl = Vec::new();
        gl.extend(set("Overall", 150.0, 40.0));
        gl.extend(set("Clinic", 262.0, 40.0));
        for (k, u) in ["n", "%", "n", "%"].iter().enumerate() {
            gl.extend(set(u, xs[k + 1], 56.0));
        }
        for (i, (label, vals)) in [
            ("Urinary", ["128", "17.4", "6", "9.8"]),
            ("Respiratory", ["195", "26.5", "7", "11.5"]),
            ("Skin", ["98", "13.3", "4", "6.6"]),
        ]
        .iter()
        .enumerate()
        {
            let y = 74.0 + 16.0 * i as f64;
            gl.extend(set(label, xs[0], y));
            for (k, v) in vals.iter().enumerate() {
                gl.extend(set(v, xs[k + 1], y));
            }
        }
        let region = BBox {
            x0: 10.0,
            y0: 64.0,
            x1: 340.0,
            y1: 120.0,
        };
        let grids = read(&lines(&gl), &[], region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        let lay = grids[0].layout.as_deref().expect("read from its text");
        assert_eq!(lay.header_rows, 2, "{lay:?}");
        let texts = grids[0].cell_texts(&gl);
        let cols = grids[0].cols();
        assert_eq!(texts[2 * cols], "Urinary", "{texts:?}");
        assert!(is_unit_head("95% CI") && is_unit_head("(%)") && is_unit_head("No."));
        assert!(!is_unit_head("95") && !is_unit_head("Total") && !is_unit_head("n=12"));
    }

    #[test]
    fn a_years_note_mark_and_a_samples_size() {
        for (t, bare) in [
            ("2007(1)", "2007"),
            ("2004(b)", "2004"),
            ("2008*", "2008"),
            ("2010 †", "2010"),
            ("2009", "2009"),
            // A count in brackets is no note's mark.
            ("2009(123)", "2009(123)"),
        ] {
            assert_eq!(strip_note_mark(t), bare, "{t}");
        }
        assert!(is_year("2007(1)") && is_year("2004(b)") && is_year("2008*"));
        assert!(!is_year("2009(123)"));
        for t in ["n = 59", "(n=735)", "N=104", "(n= 61)", "n = 1,204"] {
            assert!(is_sample_size(t), "{t}");
        }
        for t in ["59", "n", "n/a", "N (%)", "n = ", "Mean (n)"] {
            assert!(!is_sample_size(t), "{t}");
        }
    }

    #[test]
    fn figures_are_dressed_digits_or_a_dash() {
        for f in [
            "$ 10,441", "(1,552)", "12.5%", "—", "-", "n/a", "1.5x", "(0.75)",
        ] {
            assert!(is_figure(f), "{f}");
        }
        for t in ["Domestic", "Note 13", "2018 Total", ""] {
            assert!(!is_figure(t), "{t}");
        }
    }

    #[test]
    fn a_label_reaches_as_far_as_its_leaders() {
        // A label's leaders run up to the next column, and are dropped from
        // its text; the label reaches as far as they do.
        let glyphs = set("Kolkata, India ........      Office", 40.0, 100.0);
        let l = &lines(&glyphs)[0];
        let texts: Vec<&str> = l.chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["Kolkata, India", "Office"]);
        assert!(l.chunks[0].reach > l.chunks[0].x1 + 30.0);
        assert_eq!(l.chunks[1].reach, l.chunks[1].x1);
        // Two columns' empty values (`..`) a gutter apart are no leaders.
        let empties = set("0·99    ..    ..", 40.0, 100.0);
        let mut run = LeaderRun::default();
        for g in empties.iter().filter(|g| g.bbox.is_some()) {
            run.step(g);
            assert!(!run.leads());
        }
        // A contents entry's leaders run to its page alone.
        let entry = set("Scientific Method Lab ........ 6", 40.0, 100.0);
        let inked: Vec<&Glyph> = entry.iter().filter(|g| g.bbox.is_some()).collect();
        assert!(contents_entry(&inked));
        let preface = set("Foreword ........ xii", 40.0, 100.0);
        let inked: Vec<&Glyph> = preface.iter().filter(|g| g.bbox.is_some()).collect();
        assert!(contents_entry(&inked));
        // A figure after the leaders is a value, and a row of them a table's.
        for row in [
            "Net sales ........ 1,234",
            "Kolkata ........ Office  Lease  800",
        ] {
            let g = set(row, 40.0, 100.0);
            let inked: Vec<&Glyph> = g.iter().filter(|g| g.bbox.is_some()).collect();
            assert!(!contents_entry(&inked), "{row}");
        }
    }

    #[test]
    fn leaders_are_dropped_from_a_label() {
        assert_eq!(strip_leaders("Basic . . . . . . . ."), "Basic");
        assert_eq!(strip_leaders("Diluted ........"), "Diluted");
        assert_eq!(strip_leaders("U.S."), "U.S.");
        assert_eq!(strip_leaders("..."), "...");
        assert_eq!(
            strip_leaders_of("If you pay...", LEADER_DOTS),
            "If you pay..."
        );
        assert_eq!(
            strip_leaders_of("Revenues ........", LEADER_DOTS),
            "Revenues"
        );
    }

    /// `text` set from `x` on baseline `y` at 10pt, 5.5pt a character,
    /// spaces as outline-less glyphs.
    fn set(text: &str, x: f64, y: f64) -> Vec<Glyph> {
        text.chars()
            .enumerate()
            .map(|(i, ch)| {
                let cx = x + 5.5 * i as f64;
                Glyph {
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

    fn chunks(glyphs: &[Glyph]) -> Vec<String> {
        lines(glyphs)[0]
            .chunks
            .iter()
            .map(|c| c.text.clone())
            .collect()
    }

    #[test]
    fn a_band_under_figures_underlines_holds_a_row_per_line() {
        // A statement's grid as its underlines rule it: a header band, a
        // band of three lines of figures over a subtotal's underlines, a
        // total's band and a band holding one figure with a note under it.
        let mut g = crate::table::Grid {
            layout: None,
            page: 0,
            xs: vec![50.0, 150.0, 208.0, 270.0, 332.0, 386.0],
            ys: vec![30.0, 60.0, 103.0, 123.0, 150.0],
            bbox: BBox {
                x0: 50.0,
                y0: 30.0,
                x1: 386.0,
                y1: 150.0,
            },
        };
        let mut glyphs = Vec::new();
        // The header's labels wrapped over two lines.
        for (x, top, under) in [
            (160.0, "Total", "cost"),
            (222.0, "Par", "value"),
            (284.0, "Fair", "value"),
            (344.0, "Net", "gain"),
        ] {
            glyphs.extend(set(top, x, 44.0));
            glyphs.extend(set(under, x, 55.0));
        }
        for (base, label) in [
            (75.0, "Cash"),
            (87.0, "Receivables"),
            (99.0, "Inventory"),
            (119.0, "Total"),
        ] {
            glyphs.extend(set(label, 55.0, base));
            for x in [175.0, 237.0, 299.0, 357.0] {
                glyphs.extend(set("1,234", x, base));
            }
        }
        glyphs.extend(set("Other", 55.0, 135.0));
        glyphs.extend(set("1,234", 175.0, 135.0));
        glyphs.extend(set("see note", 160.0, 146.0));
        split_underlined_bands(&mut g, &lines(&glyphs));
        assert_eq!(g.ys.len(), 7, "{:?}", g.ys);
        // The cuts fall between one line's ink and the next one's.
        for (cut, (above, below)) in [g.ys[2], g.ys[3]].iter().zip([(75.0, 87.0), (87.0, 99.0)]) {
            assert!(*cut > above + 2.0 && *cut < below - 8.0, "{:?}", g.ys);
        }
        assert_eq!(&g.ys[4..], &[103.0, 123.0, 150.0]);
    }

    #[test]
    fn a_table_ruled_row_by_row_and_by_its_bands_is_read_from_its_rules() {
        // Four columns, five rows: rules under three rows, a band's edge
        // under the fourth, and a first row whose values each wrap under
        // themselves, the first column left empty.
        let g = crate::table::Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 100.0, 200.0, 300.0, 400.0],
            ys: vec![0.0, 30.0, 50.0, 70.0, 90.0, 110.0],
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 400.0,
                y1: 110.0,
            },
        };
        let rule = |y: f64| crate::rule::Rule {
            bbox: BBox {
                x0: 0.0,
                y0: y,
                x1: 400.0,
                y1: y + 0.5,
            },
            orientation: Orientation::Horizontal,
            page: 0,
        };
        let rules = vec![rule(30.0), rule(50.0), rule(90.0)];
        let band = crate::rule::Fill {
            bbox: BBox {
                x0: 0.0,
                y0: 70.0,
                x1: 400.0,
                y1: 82.0,
            },
            page: 0,
            paint: None,
            stroked: false,
        };
        let mut glyphs = Vec::new();
        for (x, t) in [
            (5.0, "Weight"),
            (105.0, "1.47 lb"),
            (205.0, "1.47 lb"),
            (305.0, "1.52 lb"),
        ] {
            glyphs.extend(set(t, x, 12.0));
        }
        for x in [105.0, 205.0, 305.0] {
            glyphs.extend(set("10%", x, 24.0));
        }
        for (r, y) in [(1, 44.0), (2, 64.0), (3, 84.0), (4, 104.0)] {
            glyphs.extend(set(&format!("Row {r}"), 5.0, y));
            for x in [105.0, 205.0, 305.0] {
                glyphs.extend(set("5 W", x, y));
            }
        }
        let lines = lines(&glyphs);
        let grids = vec![g.clone()];
        assert!(
            unruled_grids(&grids, &rules, std::slice::from_ref(&band), &lines).is_empty(),
            "its rules and band read its rows"
        );
        // Without the band its fourth line is ruled by nothing.
        assert_eq!(unruled_grids(&grids, &rules, &[], &lines), vec![0]);
        // Two rows of their own, each labelled, under one rule: lumped.
        let mut lumped = glyphs.clone();
        lumped.extend(set("Other", 5.0, 24.0));
        let lines = super::lines(&lumped);
        assert_eq!(
            unruled_grids(&grids, &rules, std::slice::from_ref(&band), &lines),
            vec![0]
        );
    }

    #[test]
    fn labels_across_a_divider_are_not_the_figures_labels() {
        // A run of figures right of a page's column divider, and the left
        // column's prose on the same lines.
        let mut gl = Vec::new();
        for (i, y) in [50.0, 64.0, 78.0, 92.0, 106.0].into_iter().enumerate() {
            gl.extend(set(&format!("prose line {i}"), 20.0, y));
            gl.extend(set(&format!("{}", 10 + i), 200.0, y));
            gl.extend(set(&format!("{}", 50 + i), 250.0, y));
        }
        let ls = lines(&gl);
        let region = BBox {
            x0: 195.0,
            y0: 40.0,
            x1: 265.0,
            y1: 110.0,
        };
        let divider = Rule {
            bbox: BBox {
                x0: 150.0,
                y0: 0.0,
                x1: 150.6,
                y1: 200.0,
            },
            orientation: crate::rule::Orientation::Vertical,
            page: 0,
        };
        let open = read(&ls, &[], region, 0);
        let parted = read(&ls, &[divider], region, 0);
        assert!(
            open.iter().any(|g| g.bbox.x0 < 100.0),
            "without the rule the labels are taken in: {open:?}"
        );
        assert!(parted.iter().all(|g| g.bbox.x0 > 150.0), "got {parted:?}");
    }

    #[test]
    fn prose_beside_a_table_is_not_carried_into_its_cells() {
        // Labels and counts, and beside them a column of prose whose
        // sentences run on down the lines, through the rows as well.
        let prose = [
            "the survey asked each respondent",
            "whether they knew the causative agent",
            "of malaria and most of them named",
            "mosquitoes as the cause while a third",
            "did not know its brand or how it",
            "spreads between people in the villages",
            "and towns where they live today",
        ];
        let mut gl = Vec::new();
        for (i, line) in prose.iter().enumerate() {
            let y = 50.0 + 12.0 * i as f64;
            if i % 2 == 0 {
                let (label, n) = [
                    ("Alpha", "12"),
                    ("Beta", "14"),
                    ("Gamma", "9"),
                    ("Delta", "11"),
                ][i / 2];
                gl.extend(set(label, 20.0, y));
                gl.extend(set(n, 100.0, y));
            }
            gl.extend(set(line, 160.0, y));
        }
        let region = BBox {
            x0: 10.0,
            y0: 36.0,
            x1: 400.0,
            y1: 130.0,
        };
        let grids = read(&lines(&gl), &[], region, 0);
        for g in &grids {
            let texts = g.cell_texts(&gl);
            assert!(
                !texts.iter().any(|t| t.contains("respondent whether")),
                "{texts:?}"
            );
        }
    }

    #[test]
    fn a_captions_second_line_is_no_header() {
        // A caption set in from the table and wrapped onto a second line,
        // 15pt apart, its second line reaching over two columns; then the
        // header 20pt under it, its heads coded with digits as enzymes are,
        // and three rows of figures 15pt apart.
        let mut gl = set(
            "Table 3. Potency of the compound on the enzymes in rat and",
            30.0,
            40.0,
        );
        gl.extend(set("human liver samples.", 30.0, 55.0));
        for (k, t) in ["IC50", "1A2", "2C9", "3A4"].iter().enumerate() {
            gl.extend(set(t, 20.0 + 70.0 * k as f64, 75.0));
        }
        for (i, row) in [
            ["Rat", "12.5", "3.1", "40.2"],
            ["Human", "11.0", "2.9", "38.7"],
            ["Mouse", "13.4", "3.6", "41.9"],
        ]
        .iter()
        .enumerate()
        {
            for (k, t) in row.iter().enumerate() {
                gl.extend(set(t, 20.0 + 70.0 * k as f64, 95.0 + 15.0 * i as f64));
            }
        }
        let region = BBox {
            x0: 10.0,
            y0: 68.0,
            x1: 320.0,
            y1: 130.0,
        };
        let grids = read(&lines(&gl), &[], region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        let texts = grids[0].cell_texts(&gl);
        assert!(texts.iter().any(|t| t == "IC50"), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains("liver")), "{texts:?}");
    }

    #[test]
    fn a_cell_wrapped_onto_the_next_line_stays_in_its_row() {
        // Four rows, a symbol, a figure and a name; one name runs onto a
        // second line. Rows and the wrapped line are set 12pt apart alike.
        let mut gl = Vec::new();
        let rows = [
            ("ALPHA", "0.65", "Coronin, actin binding"),
            ("BETA", "0.61", "Splicing factor, small nuclear"),
            ("", "", "ribonucleoprotein associated"),
            ("GAMMA", "0.67", "Lyn substrate 1"),
            ("DELTA", "-0.60", "Plastin 3"),
        ];
        for (i, (sym, fig, name)) in rows.iter().enumerate() {
            let y = 50.0 + 12.0 * i as f64;
            gl.extend(set(sym, 20.0, y));
            gl.extend(set(fig, 100.0, y));
            gl.extend(set(name, 160.0, y));
        }
        let region = BBox {
            x0: 10.0,
            y0: 36.0,
            x1: 350.0,
            y1: 115.0,
        };
        let grids = read(&lines(&gl), &[], region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        assert_eq!(grids[0].rows(), 4, "{grids:?}");
        let texts = grids[0].cell_texts(&gl);
        assert!(
            texts
                .iter()
                .any(|t| t == "Splicing factor, small nuclear ribonucleoprotein associated"),
            "{texts:?}"
        );
        // A row with nothing in its first column, its name a new one: a row
        // of its own, not the tail of the one above.
        let mut list = Vec::new();
        for (i, (kind, n, name)) in [
            ("Fruit", "12", "Apple"),
            ("", "14", "Banana"),
            ("Root", "9", "Carrot"),
            ("", "11", "Turnip"),
        ]
        .iter()
        .enumerate()
        {
            let y = 50.0 + 12.0 * i as f64;
            list.extend(set(kind, 20.0, y));
            list.extend(set(n, 100.0, y));
            list.extend(set(name, 160.0, y));
        }
        let grids = read(&lines(&list), &[], region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        assert_eq!(grids[0].rows(), 4, "{grids:?}");
    }

    #[test]
    fn a_name_wrapped_past_the_stub_stays_in_its_row() {
        // A code, a name and two figures a row. A name wrapped onto a line
        // that starts afresh, a strain's code, set closer under it than the rows are
        // apart; then a name with a bracketed qualifier on its next line,
        // every line as far apart as the rows.
        let table = |rows: &[(&str, &str, &str, &str)], wrap: (usize, &str, f64), pitch: f64| {
            let mut gl = Vec::new();
            let mut y = 50.0;
            for (i, (n, name, a, b)) in rows.iter().enumerate() {
                gl.extend(set(n, 20.0, y));
                gl.extend(set(name, 60.0, y));
                gl.extend(set(a, 200.0, y));
                gl.extend(set(b, 260.0, y));
                if i == wrap.0 {
                    gl.extend(set(wrap.1, 60.0, y + wrap.2));
                    y += wrap.2;
                }
                y += pitch;
            }
            gl
        };
        let region = BBox {
            x0: 10.0,
            y0: 36.0,
            x1: 300.0,
            y1: 160.0,
        };
        let coded = table(
            &[
                ("sea", "Agona", "118", "32"),
                ("seg", "Gallinarum Pullorum", "117", "32"),
                ("seh", "Heidelberg", "116", "31"),
                ("see", "Newport", "118", "32"),
            ],
            (1, "RKS5078", 8.0),
            12.0,
        );
        let grids = read(&lines(&coded), &[], region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        assert_eq!(grids[0].rows(), 4, "{grids:?}");
        let texts = grids[0].cell_texts(&coded);
        assert!(
            texts.iter().any(|t| t == "Gallinarum Pullorum RKS5078"),
            "{texts:?}"
        );
        let qualified = table(
            &[
                ("A", "Carbamate", "2", "2.772"),
                ("Co", "Uracil", "1", "2.210"),
                ("N", "Orotate", "2", "2.182"),
                ("C", "Melibiose", "1", "2.092"),
            ],
            (1, "(extracellular)", 12.0),
            12.0,
        );
        let grids = read(&lines(&qualified), &[], region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        assert_eq!(grids[0].rows(), 4, "{grids:?}");
        let texts = grids[0].cell_texts(&qualified);
        assert!(
            texts.iter().any(|t| t == "Uracil (extracellular)"),
            "{texts:?}"
        );
        // A list's enumerator in brackets starts an item of its own.
        let chunk = |t: &str| lines(&set(t, 60.0, 50.0))[0].chunks[0].clone();
        assert!(qualifies(&chunk("Uracil"), &chunk("(extracellular)")));
        for item in ["(a) Glycerol", "(iv) Glycerol", "(S)-Dihydroorotate"] {
            assert!(!qualifies(&chunk("Uracil"), &chunk(item)), "{item}");
        }
    }

    #[test]
    fn a_tables_row_lines_hold_its_rows_and_gridlines_do_not() {
        // Five lines 15pt apart: a row of text between each pair.
        let ys = [40.0, 55.0, 70.0, 85.0, 100.0];
        let region = BBox {
            x0: 15.0,
            y0: 40.0,
            x1: 260.0,
            y1: 100.0,
        };
        let mut gl = Vec::new();
        for (i, w) in ys.windows(2).enumerate() {
            gl.extend(set(&format!("Row {i}"), 20.0, w[1] - 4.0));
            gl.extend(set(&format!("{}.5", 10 + i), 200.0, w[1] - 4.0));
        }
        assert!(bands_hold_text(&lines(&gl), region, &ys));
        // One blank band in four still holds the table's rows.
        let three: Vec<Glyph> = gl.iter().filter(|g| g.origin.1 < 90.0).cloned().collect();
        assert!(bands_hold_text(&lines(&three), region, &ys));
        // A chart's gridlines with a legend set in one band, and its axis
        // labels at the lines' left, outside them.
        let mut chart = set("Projections 18", 120.0, 51.0);
        for &y in &ys {
            chart.extend(set(&format!("{}", y as i32), 0.0, y + 3.0));
        }
        assert!(!bands_hold_text(&lines(&chart), region, &ys));
    }

    #[test]
    fn a_stacks_labels_are_not_under_the_next_tables_rules() {
        // Two tables side by side, each ruled under its rows across its own
        // width: codes and their names on the left, figures on the right.
        let rule = |x0: f64, x1: f64, y: f64| Rule {
            bbox: BBox {
                x0,
                y0: y,
                x1,
                y1: y + 0.5,
            },
            orientation: crate::rule::Orientation::Horizontal,
            page: 0,
        };
        let mut gl = Vec::new();
        let mut left_rules = Vec::new();
        let mut right_rules = Vec::new();
        for (i, y) in [50.0, 64.0, 78.0, 92.0, 106.0].into_iter().enumerate() {
            gl.extend(set(&format!("{}", 10 + i), 20.0, y));
            gl.extend(set(&format!("Item {i}"), 190.0, y));
            gl.extend(set(&format!("{}.5", 30 + i), 310.0, y));
            gl.extend(set(&format!("{}.0", 60 + i), 380.0, y));
            left_rules.push(rule(15.0, 250.0, y + 3.0));
            right_rules.push(rule(295.0, 455.0, y + 3.0));
        }
        let ls = lines(&gl);
        let region = BBox {
            x0: 295.0,
            y0: 40.0,
            x1: 455.0,
            y1: 110.0,
        };
        let both: Vec<Rule> = left_rules.iter().chain(&right_rules).copied().collect();
        // Read as any region is, the left table's names are taken in as
        // the labels of the right one's figures.
        let any = read(&ls, &both, region, 0);
        assert!(any.iter().any(|g| g.bbox.x0 < 250.0), "got {any:?}");
        // Read as a stack, they stand under the left table's own rules.
        let stack = read_ruled_stack(&ls, &both, region, 0);
        assert!(stack.iter().all(|g| g.bbox.x0 > 290.0), "got {stack:?}");
        // Labels under no other table's rules are still taken in.
        let open = read_ruled_stack(&ls, &right_rules, region, 0);
        assert!(open.iter().any(|g| g.bbox.x0 < 250.0), "got {open:?}");
    }

    #[test]
    fn prose_across_the_gutter_is_no_label_column() {
        // Figures with their labels in the page's right column, the left
        // column's prose running on beside them a gutter away (40pt, inside
        // the reach for a further column of labels), no rule between.
        let prose = [
            "credit risk is the risk that a party",
            "would incur a loss as a result of",
            "nonperformance by counterparties of",
            "their contractual obligations and the",
            "continued volatility in the markets",
        ];
        let labels = [
            "Revenues",
            "Operations",
            "Depreciation",
            "Income taxes",
            "Other",
        ];
        let row = |gl: &mut Vec<Glyph>, i: usize, l: &str| {
            let y = 50.0 + 12.0 * i as f64;
            gl.extend(set(l, 250.0, y));
            gl.extend(set(&format!("{}", 30 + i), 340.0, y));
            gl.extend(set(&format!("({})", 5 + i), 380.0, y));
        };
        let mut gl = Vec::new();
        for (i, (p, l)) in prose.iter().zip(labels).enumerate() {
            gl.extend(set(p, 20.0, 50.0 + 12.0 * i as f64));
            row(&mut gl, i, l);
        }
        let region = BBox {
            x0: 335.0,
            y0: 40.0,
            x1: 410.0,
            y1: 110.0,
        };
        let grids = read(&lines(&gl), &[], region, 0);
        assert!(!grids.is_empty());
        assert!(grids.iter().all(|g| g.bbox.x0 > 200.0), "{grids:?}");
        // A column of codes beside their descriptions, each line its own,
        // is still taken in.
        let mut coded = Vec::new();
        for (i, (code, l)) in ["REV", "OPS", "DEP", "TAX", "OTH"]
            .iter()
            .zip(labels)
            .enumerate()
        {
            coded.extend(set(code, 200.0, 50.0 + 12.0 * i as f64));
            row(&mut coded, i, l);
        }
        let grids = read(&lines(&coded), &[], region, 0);
        assert!(grids.iter().any(|g| g.bbox.x0 < 210.0), "{grids:?}");
    }

    #[test]
    fn a_label_set_close_to_its_first_figure_keeps_its_row() {
        // Labels at the margin, three columns of figures. One label, a long
        // code, runs to a word space from its first figure, and the two read
        // as one chunk across the gutter; its other figures stand in the
        // columns after.
        let mut gl = Vec::new();
        let rows = [
            "Alpha", "Beta", "Delta", "Epsilon", "Zeta", "", "Eta", "Theta", "Iota", "Kappa",
            "Lambda",
        ];
        for (i, label) in rows.iter().enumerate() {
            let y = 50.0 + 12.0 * i as f64;
            if label.is_empty() {
                // `15` starts at 100, the first figure column's edge.
                gl.extend(set("GAMMA4400551 15", 100.0 - 5.5 * 13.0, y));
            } else {
                gl.extend(set(label, 12.0, y));
                gl.extend(set(&format!("{}", 10 + i), 100.0, y));
            }
            gl.extend(set(&format!("{}", 30 + i), 140.0, y));
            gl.extend(set(&format!("{}", 50 + i), 180.0, y));
        }
        let region = BBox {
            x0: 95.0,
            y0: 40.0,
            x1: 200.0,
            y1: 185.0,
        };
        let grids = read(&lines(&gl), &[], region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        assert_eq!(grids[0].rows(), 11, "{grids:?}");
        let texts = grids[0].cell_texts(&gl);
        assert!(texts.iter().any(|t| t == "GAMMA4400551"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "15"), "{texts:?}");
    }

    /// A table of three groups, a size and its colour set once over three
    /// volumes, each with its error; `ruled` rules each group off, and
    /// otherwise only the header and the foot are ruled.
    fn grouped(ruled: bool) -> Vec<Grid> {
        let mut gl = Vec::new();
        for (t, x) in [
            ("Size", 20.0),
            ("Colour", 80.0),
            ("Volume", 160.0),
            ("Error", 220.0),
        ] {
            gl.extend(set(t, x, 50.0));
        }
        let rule = |y: f64| Rule {
            bbox: BBox {
                x0: 15.0,
                y0: y,
                x1: 260.0,
                y1: y + 0.5,
            },
            orientation: crate::rule::Orientation::Horizontal,
            page: 0,
        };
        let mut rules = vec![rule(55.0)];
        for (g, (size, colour)) in [("Small", "white"), ("Medium", "blue"), ("Large", "red")]
            .into_iter()
            .enumerate()
        {
            let top = 66.0 + 40.0 * g as f64;
            gl.extend(set(size, 20.0, top));
            gl.extend(set(colour, 80.0, top));
            for (i, y) in [top, top + 12.0, top + 24.0].into_iter().enumerate() {
                gl.extend(set(&format!("{}", (g + 1) * 10 + i), 160.0, y));
                gl.extend(set(&format!("{}.5", 9 - i), 220.0, y));
            }
            if ruled || g == 2 {
                rules.push(rule(top + 27.0));
            }
        }
        let region = BBox {
            x0: 10.0,
            y0: 30.0,
            x1: 270.0,
            y1: 190.0,
        };
        read(&lines(&gl), &rules, region, 0)
    }

    #[test]
    fn group_labels_span_the_columns_their_underlines_run_under() {
        // Two groups of three years, each label narrower than one column
        // and set over its group's middle, each underlined a column at a
        // time, the pieces meeting end to end and the groups' runs parted.
        let piece = |x0: f64, x1: f64| Rule {
            bbox: BBox {
                x0,
                y0: 53.0,
                x1,
                y1: 53.5,
            },
            orientation: crate::rule::Orientation::Horizontal,
            page: 0,
        };
        let rules = vec![
            piece(115.0, 147.0),
            piece(147.0, 197.0),
            piece(197.0, 247.0),
            piece(275.0, 307.0),
            piece(307.0, 357.0),
            piece(357.0, 407.0),
        ];
        let years = [120.0, 170.0, 220.0, 280.0, 330.0, 380.0];
        let mut gl = set("Sales", 168.0, 50.0);
        gl.extend(set("Assets", 326.0, 50.0));
        for (x, y) in years
            .iter()
            .zip(["2016", "2015", "2014", "2016", "2015", "2014"])
        {
            gl.extend(set(y, *x, 64.0));
        }
        for (i, label) in ["North", "South", "Total"].into_iter().enumerate() {
            let base = 80.0 + 12.0 * i as f64;
            gl.extend(set(label, 20.0, base));
            for x in years {
                gl.extend(set("1,234", x - 3.0, base));
            }
        }
        let region = BBox {
            x0: 10.0,
            y0: 38.0,
            x1: 420.0,
            y1: 110.0,
        };
        let grids = read(&lines(&gl), &rules, region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        let lay = grids[0].layout.as_deref().expect("read from its text");
        assert!(lay.spans.contains(&(0, 1, 4)), "{:?}", lay.spans);
        assert!(lay.spans.contains(&(0, 4, 7)), "{:?}", lay.spans);
    }

    #[test]
    fn a_row_ruled_off_as_its_neighbours_are_stays_whatever_its_value_says() {
        // Dimensions ruled under every row: lengths and weights, and a
        // package size that is no figure.
        let rule = |y: f64| Rule {
            bbox: BBox {
                x0: 15.0,
                y0: y,
                x1: 260.0,
                y1: y + 0.5,
            },
            orientation: crate::rule::Orientation::Horizontal,
            page: 0,
        };
        let mut gl = Vec::new();
        let mut rules = vec![rule(38.0)];
        for (i, (label, value)) in [
            ("Length", "221 mm"),
            ("Weight", "190 g"),
            ("Packaging", "31 cm x 10 cm x 15 cm"),
            ("Shipping", "388 g"),
        ]
        .into_iter()
        .enumerate()
        {
            let base = 50.0 + 13.0 * i as f64;
            gl.extend(set(label, 20.0, base));
            gl.extend(set(value, 120.0, base));
            rules.push(rule(base + 3.0));
        }
        let region = BBox {
            x0: 10.0,
            y0: 36.0,
            x1: 270.0,
            y1: 95.0,
        };
        let grids = read(&lines(&gl), &rules, region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        assert_eq!(grids[0].rows(), 4, "{grids:?}");
    }

    #[test]
    fn a_reading_that_merges_drawn_columns_is_told() {
        // Three years' figures a word space apart: read from the text alone,
        // one column.
        let rows = [
            ("Reported", ["4,137", "3,610", "3,547"]),
            ("Related", ["(951)", "(281)", "(751)"]),
            ("Charges", ["(110)", "(125)", "(116)"]),
            ("Adjusted", ["3,944", "3,510", "3,356"]),
        ];
        let mut gl = Vec::new();
        for (i, (label, values)) in rows.iter().enumerate() {
            let base = 50.0 + 13.0 * i as f64;
            gl.extend(set(label, 20.0, base));
            for (k, v) in values.iter().enumerate() {
                gl.extend(set(v, 120.0 + 30.0 * k as f64, base));
            }
        }
        let region = BBox {
            x0: 10.0,
            y0: 36.0,
            x1: 230.0,
            y1: 95.0,
        };
        let grids = read(&lines(&gl), &[], region, 0);
        assert_eq!(grids.len(), 1, "{grids:?}");
        let everywhere = BBox {
            x0: 0.0,
            y0: 0.0,
            x1: 300.0,
            y1: 200.0,
        };
        // Edges drawn between the years: the reading merges them.
        assert!(merges_drawn_columns(
            &grids[0],
            &[148.5, 178.5],
            &everywhere,
            &gl
        ));
        // An edge between the labels and the figures parts no cell.
        assert!(!merges_drawn_columns(&grids[0], &[100.0], &everywhere, &gl));
        // An edge through the figures' glyphs is no column's.
        assert!(!merges_drawn_columns(&grids[0], &[132.0], &everywhere, &gl));
        // A stack's edges say nothing of rows outside it.
        let elsewhere = BBox {
            x0: 0.0,
            y0: 100.0,
            x1: 300.0,
            y1: 200.0,
        };
        assert!(!merges_drawn_columns(
            &grids[0],
            &[148.5, 178.5],
            &elsewhere,
            &gl
        ));
    }

    #[test]
    fn a_value_set_once_over_a_ruled_group_spans_it() {
        let grids = grouped(true);
        assert_eq!(grids.len(), 1, "{grids:?}");
        let lay = grids[0].layout.as_deref().unwrap();
        let mut downs = lay.downs.clone();
        downs.sort();
        assert_eq!(
            downs,
            [
                (1, 0, 4),
                (1, 1, 4),
                (4, 0, 7),
                (4, 1, 7),
                (7, 0, 10),
                (7, 1, 10)
            ]
        );
        // Ruled only under the header and at the foot, it is one group, and
        // its empty cells are values missing, not spans.
        let grids = grouped(false);
        assert_eq!(grids.len(), 1, "{grids:?}");
        assert!(grids[0].layout.as_deref().unwrap().downs.is_empty());
    }

    #[test]
    fn a_sign_set_after_its_figure_stays_with_it() {
        // `4,85 €` then `19,40 €`: the last sign prices nothing after it,
        // so the line sets its signs after its figures.
        let mut gl = set("4", 100.0, 50.0);
        gl.extend(set("4,85", 160.0, 50.0));
        gl.extend(set("€", 185.0, 50.0));
        gl.extend(set("19,40", 240.0, 50.0));
        gl.extend(set("€", 270.5, 50.0));
        assert_eq!(chunks(&gl), ["4", "4,85 €", "19,40 €"]);
        // Before a bare quantity as well.
        let mut gl = set("4,85", 100.0, 50.0);
        gl.extend(set("€", 125.0, 50.0));
        gl.extend(set("4", 180.0, 50.0));
        gl.extend(set("19,40", 240.0, 50.0));
        gl.extend(set("€", 270.5, 50.0));
        assert_eq!(chunks(&gl), ["4,85 €", "4", "19,40 €"]);
    }

    #[test]
    fn a_sign_before_its_figure_stays_with_that_one() {
        // A statement's `$` pinned at the column's left edge, a word space
        // after the figure before it: every figure on the line is bare, so
        // each sign is the next figure's.
        let mut gl = set("1,234", 100.0, 50.0);
        gl.extend(set("$", 131.0, 50.0));
        gl.extend(set("5,678", 190.0, 50.0));
        assert_eq!(chunks(&gl), ["1,234", "$ 5,678"]);
    }
}
