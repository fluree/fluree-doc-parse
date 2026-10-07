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

/// Baseline agreement, in font sizes, for two glyphs to be on one line.
const BASELINE_TOLERANCE: f64 = 0.5;

/// Baseline distance, in font sizes, up to which a line continues the one
/// above it: a wrapped label, set at its paragraph's leading. A table's own
/// rows are set further apart, or the row beside it carries values.
const WRAP_LEADING: f64 = 1.35;

/// Vertical gap between consecutive lines, in font sizes, that ends a table.
const MAX_ROW_GAP: f64 = 2.6;

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

fn is_leader(text: &str) -> bool {
    matches!(text, "." | "·" | "…" | "․" | "‥")
}

/// The page's lines, each cut into chunks, with dot leaders dropped and a
/// currency sign set apart from its figure taken back in to it.
pub fn lines(glyphs: &[Glyph]) -> Vec<Line> {
    let index = |g: &Glyph| {
        (g as *const Glyph as usize - glyphs.as_ptr() as usize) / std::mem::size_of::<Glyph>()
    };
    let mut gs: Vec<&Glyph> = glyphs
        .iter()
        .filter(|g| g.bbox.is_some() && g.is_horizontal() && !g.text.trim().is_empty())
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
                words,
            });
        }
        // `$` set apart from its figure is the figure's: `$ 10,441`.
        let mut merged: Vec<Chunk> = Vec::new();
        for c in chunks {
            if let Some(prev) = merged.last_mut() {
                let joins =
                    (is_currency(&prev.text) && is_figure(&c.text) && c.x0 - prev.x1 <= fs * 6.0)
                        || (c.text.trim() == "%"
                            && is_figure(&prev.text)
                            && c.x0 - prev.x1 <= fs * 2.0);
                if joins {
                    prev.text = format!("{} {}", prev.text, c.text);
                    prev.glyphs.extend(c.glyphs);
                    prev.words.extend(c.words);
                    prev.x1 = c.x1;
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
/// line is cut off: a banner's words across the gutters it spans stay one
/// label. A cut is kept only where every piece then sits in one column.
fn split_at_gutters(cols: &[(f64, f64)], l: &Line) -> Line {
    let mut chunks: Vec<Chunk> = Vec::new();
    for (i, c) in l.chunks.iter().enumerate() {
        // Only the line's last chunk: its figure is then the row's value. A
        // label with its row's value set after it keeps a figure of its own
        // (`Antihistamine tablets (30) | 30.80`), however wide a monospaced
        // word space.
        // A run of prose ending in a figure (a footnote under the table) is
        // no label and its value; cut, it would fit the columns as a row.
        if covered(cols, c).len() <= 1
            || c.words.len() < 2
            || c.words.len() > MAX_CUT_WORDS
            || i + 1 < l.chunks.len()
        {
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
                words: ws.into_iter().cloned().collect(),
            })
            .collect();
        if cut.len() > 1 && cut.iter().all(|p| covered(cols, p).len() <= 1) {
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
    let mut lefts: Vec<f64> = all
        .iter()
        .filter(|l| in_band(l))
        .filter_map(|l| {
            l.chunks
                .iter()
                .filter(|c| c.x1 <= region.x0 + fs && c.x0 < region.x0 - fs)
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
            let mut more: Vec<f64> = lines_in
                .iter()
                .filter_map(|l| {
                    l.chunks
                        .iter()
                        .filter(|c| c.x1 <= x0 + 0.5 && x0 - c.x1 < fs * 8.0)
                        .max_by(|a, b| a.x1.total_cmp(&b.x1))
                        .map(|c| c.x0)
                })
                .collect();
            if more.len() * 2 < lines_in.len() || more.len() < 2 {
                break;
            }
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
    let clipped: Vec<Line> = clipped
        .iter()
        .map(|l| split_at_gutters(&cols0, l))
        .collect();
    let value_row = |l: &Line| fits(&cols0, l) && l.chunks.len() >= 2 && has_figures(&cols0, l);
    let label_row = |l: &Line| {
        l.chunks.len() == 1
            && column_of(&cols0, &l.chunks[0]) == Some(0)
            && covered(&cols0, &l.chunks[0]).len() <= 1
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
            if l.y0 - clipped[band[j - 1]].y1 > fs * MAX_ROW_GAP {
                break;
            }
            if value_row(l) {
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
            if next.y0 - clipped[last].y1 > fs * MAX_ROW_GAP {
                break;
            }
            if value_row(next) {
                last += 1;
                continue;
            }
            // Up to three label lines, then a row of values.
            let mut j = last + 1;
            while j < clipped.len() && j <= last + 3 && label_row(&clipped[j]) {
                j += 1;
            }
            let resumes = j > last + 1
                && clipped
                    .get(j)
                    .is_some_and(|n| n.y0 - clipped[j - 1].y1 <= fs * MAX_ROW_GAP && value_row(n))
                && (last + 1..j).all(|i| clipped[i].y0 - clipped[i - 1].y1 <= fs * MAX_ROW_GAP);
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
                .map(|r| r.axis_pos())
                .collect();
            ys.sort_by(f64::total_cmp);
            ys.dedup_by(|a, b| (*a - *b).abs() <= 3.0);
            ys
        };
        let mut crossed = 0usize;
        let mut first = start;
        let mut header_seen = 0usize;
        let mut header_named = false;
        while first > floor && header_seen < MAX_HEADER_LINES {
            let above = &clipped[first - 1];
            let cur = &clipped[first];
            if cur.y0 - above.y1 > fs * MAX_ROW_GAP {
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
            if crossed + between >= 2 {
                break;
            }
            let prose = above
                .chunks
                .iter()
                .any(|c| c.text.split_whitespace().count() >= 9)
                || above.chunks.first().is_some_and(|c| is_caption(&c.text));
            if prose {
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
            // across some of them, or with its own rule beneath) or a label
            // wrapped tight above one of them belongs to it; a title across
            // the table, or a line of text across its gutters, does not.
            let ncols0 = cols0.len();
            let names_columns =
                |l: &Line| fits(&cols0, l) && l.chunks.len() * 2 >= ncols0 && l.chunks.len() >= 2;
            let banner_chunk = |c: &Chunk| {
                let ks = covered(&cols0, c);
                ((2..ncols0).contains(&ks.len()) && ks[0] > 0)
                    || rules.iter().any(|r| {
                        r.orientation == Orientation::Horizontal
                            && r.bbox.y0 >= above.base - fs * 0.2
                            && r.bbox.y0 <= above.base + fs * 0.8
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

/// Whether a label line runs on into the next line, rather than naming the
/// rows under it: the next line starts mid-phrase, or this one ends on a
/// word that wants one; and a bold line over a plain one is a heading.
fn continues(l: &Line, n: &Line) -> bool {
    let (Some(a), Some(b)) = (l.chunks.first(), n.chunks.first()) else {
        return false;
    };
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

/// Rows, header and bands from the table's lines.
fn build(lines: Vec<&Line>, rules: &[Rule], x0: f64, x1: f64, page: usize) -> Option<Grid> {
    let fs = lines.iter().map(|l| l.fs).fold(0.0, f64::max).max(1.0);
    // The header ends above the first line of figures past the label column.
    let first_body = {
        let cols = columns(&lines, x0, x1);
        lines.iter().position(|l| has_figures(&cols, l))?
    };
    let (head, body) = lines.split_at(first_body);
    // Body section labels set above the first figures belong to the body:
    // a label-only line in the label column, the header above it naming
    // columns past the first.
    let mut head: Vec<&Line> = head.to_vec();
    let mut body: Vec<&Line> = body.to_vec();
    let cols = columns(&body, x0, x1);
    if cols.len() < 2 {
        return None;
    }
    while let Some(l) = head.last() {
        let label_only = l.chunks.iter().all(|c| column_of(&cols, c) == Some(0))
            && l.chunks.iter().all(|c| covered(&cols, c).len() <= 1);
        let wrapped = head.len() > 1 && {
            let p = head[head.len() - 2];
            l.base - p.base <= l.fs * WRAP_LEADING
                && p.chunks.iter().any(|c| column_of(&cols, c) == Some(0))
        };
        if label_only && head.len() > 1 && !wrapped {
            body.insert(0, head.pop().unwrap());
        } else {
            break;
        }
    }
    // A header line that is a column's label wrapped onto a second line joins
    // it; a banner, or a line with a rule under it, stays a row of its own.
    let rule_under = |l: &Line, c: &Chunk| -> Option<(f64, f64)> {
        rules
            .iter()
            .filter(|r| r.orientation == Orientation::Horizontal)
            .filter(|r| r.bbox.y0 >= l.base - fs * 0.2 && r.bbox.y0 <= l.base + fs * 0.8)
            .filter(|r| r.bbox.x0 <= (c.x0 + c.x1) / 2.0 && r.bbox.x1 >= (c.x0 + c.x1) / 2.0)
            .map(|r| (r.bbox.x0, r.bbox.x1))
            .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
            // A banner's rule is under it alone; one under several labels
            // is the rule between the header and the body.
            .filter(|&(a, b)| {
                l.chunks
                    .iter()
                    .filter(|d| (d.x0 + d.x1) / 2.0 >= a && (d.x0 + d.x1) / 2.0 <= b)
                    .count()
                    == 1
            })
    };
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
            // A rule under a column's own label, as wide as its column,
            // underlines it; one reaching across columns makes a banner.
            let ruled_across = rule_under(l, c).is_some_and(|(a, b)| {
                covered(
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
            // over `Unit` and `Total`).
            let tight_below = below.is_some_and(|(n, _)| n.base - l.base <= n.fs * WRAP_LEADING);
            let label_of = |n: &Line, k: usize| {
                n.chunks.len() >= 2
                    && n.chunks
                        .iter()
                        .any(|d| column_of(&cols, d) == Some(k) && covered(&cols, d).len() <= 1)
            };
            let over_next = k > 0
                && !tight_below
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
            let under = leaf_bases
                .iter()
                .filter(|(k, _)| names.contains(k) || names.is_empty())
                .map(|(_, b)| *b)
                .fold(f64::MAX, f64::min);
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
    let mut body_rows: Vec<(Vec<&Line>, bool)> = Vec::new(); // (lines, section)
    let mut i = 0;
    while i < body.len() {
        let l = body[i];
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
    let mut spans: Vec<(usize, usize, usize)> = Vec::new();
    for (r, ls) in head_rows.iter().enumerate() {
        for l in ls {
            for c in &l.chunks {
                let mut ks = covered(&cols, c);
                if let Some((a, b)) = rule_under(l, c) {
                    let wide = Chunk {
                        x0: a,
                        x1: b,
                        ..c.clone()
                    };
                    let under = covered(&cols, &wide);
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
            let ruled = rule_under(l, c).is_some_and(|(a, b)| {
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
            let next = at.get(i + 1).map_or(ncols, |&(k2, _)| k2);
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
            cells,
            claimed: Vec::new(),
        })),
    })
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

/// The regions of grids ruled with horizontals only: their bands are not
/// rows and their segments' ends are not columns.
pub fn unruled_grids(grids: &[Grid], rules: &[Rule], lines: &[Line]) -> Vec<usize> {
    (0..grids.len())
        .filter(|&i| {
            !has_vertical(rules, &grids[i].bbox)
                && !(ruled_row_by_row(&grids[i], rules)
                    && !lumped(&grids[i], lines)
                    && !labels_outside(&grids[i], lines))
        })
        .collect()
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
/// under a description, tax lines indented under an item) sit in one place.
fn lumped(g: &Grid, lines: &[Line]) -> bool {
    g.ys.windows(2).any(|w| {
        lines
            .iter()
            .filter(|l| l.base > w[0] && l.base <= w[1])
            .filter(|l| {
                l.chunks
                    .iter()
                    .filter(|c| {
                        (c.x0 + c.x1) / 2.0 >= g.bbox.x0 && (c.x0 + c.x1) / 2.0 <= g.bbox.x1
                    })
                    .count()
                    >= 3
            })
            .count()
            >= 2
    })
}

/// A grid with a rule across it at every boundary between its bands: each
/// band is one row, however many lines it holds (a description, its service
/// dates, the tax lines indented under it). Three rules around a journal
/// table's header and body, or the short rules under a statement's figures,
/// are not that.
fn ruled_row_by_row(g: &Grid, rules: &[Rule]) -> bool {
    let width = g.bbox.x1 - g.bbox.x0;
    g.rows() >= 4
        && g.ys[1..g.ys.len() - 1].iter().all(|&y| {
            let spans: Vec<(f64, f64)> = rules
                .iter()
                .filter(|r| {
                    r.orientation == Orientation::Horizontal && (r.axis_pos() - y).abs() <= 3.0
                })
                .map(|r| (r.bbox.x0.max(g.bbox.x0), r.bbox.x1.min(g.bbox.x1)))
                .filter(|(a, b)| b > a)
                .collect();
            spans.iter().map(|(a, b)| b - a).sum::<f64>() >= 0.9 * width
        })
}

/// Runs of lines carrying figures in two places or more that no grid holds:
/// a statement's rows, ruled by nothing but underlines.
pub fn figure_runs(lines: &[Line], taken: &[BBox]) -> Vec<BBox> {
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
            let gap = lines[k].y0 - lines[k - 1].y1;
            if gap > lines[k].fs * MAX_ROW_GAP || !free(&lines[k]) {
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
    if dots >= 3 && end > 0 {
        t[..end].trim_end().to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn leaders_are_dropped_from_a_label() {
        assert_eq!(strip_leaders("Basic . . . . . . . ."), "Basic");
        assert_eq!(strip_leaders("Diluted ........"), "Diluted");
        assert_eq!(strip_leaders("U.S."), "U.S.");
        assert_eq!(strip_leaders("..."), "...");
    }
}
