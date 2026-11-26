//! Tables drawn with characters.
//!
//! A line printer has no ruling, so the systems that print like one draw a
//! table with the characters they have: `|` (or `¦`) down each column's
//! edge, a line of `_` or `-` between rows, and an underscore struck over a
//! row's text to close it beneath. Batch records and reports from such
//! systems are still produced this way and saved as PDF: Courier on a fixed
//! grid, every bar a glyph.
//!
//! To the layout pass those bars are text. A table's rows run together into
//! one line per page (`|A2. |Rinse the vessel | | | |`), and a header
//! repeated atop every page reads as a running header and is dropped.
//!
//! So the drawing is read back as the drawing it is. A bar set at the same
//! position on line after line is a vertical rule; a line of nothing but
//! rule characters, or an underscore run struck over a line and across its
//! bars, is a horizontal one. Those become ordinary [`Rule`]s, their glyphs
//! leave the text, and the ruled-table reader takes it from there, as it
//! would for a table drawn with lines.
//!
//! What is not drawing stays text: a bar alone, a `____` blank left to be
//! filled in, a dashed line no bar meets.

use crate::geom::BBox;
use crate::glyph::Glyph;
use crate::rule::{Orientation, Rule};

/// A bar must sit at its position on at least this many lines in a row to
/// be a column's edge: two could be any two lines that happen to hold one.
const MIN_RUN_LINES: usize = 3;

/// Edges a frame needs: three, so the frame holds at least two columns. A
/// box around a single note is a box, not a table.
const MIN_FRAME_EDGES: usize = 3;

/// Lines a column's edge may skip and still run on, where the text of a
/// cell too long for its column was printed over the bar.
const MAX_OVERWRITTEN_LINES: usize = 2;

/// How far, in character advances, a rule may fall short of an edge and
/// still be read as meeting it: a printer's top border often starts a
/// character or two inside the bar below it.
const REACH_ADVANCES: f64 = 3.0;

/// Share of a frame's width a line of rule characters must cover to be one
/// of its rules: a short run is a blank left to be filled in.
const MIN_RULE_COVERAGE: f64 = 0.5;

fn is_vertical(t: &str) -> bool {
    matches!(t, "|" | "¦" | "│" | "┃" | "║")
}

fn is_horizontal(t: &str) -> bool {
    matches!(t, "_" | "-" | "─" | "━" | "═")
}

/// A box-drawing corner or crossing: an edge either way.
fn is_junction(t: &str) -> bool {
    t == "+"
        || t.chars().count() == 1
            && t.chars().next().is_some_and(|c| {
                ('\u{250C}'..='\u{254B}').contains(&c) || ('\u{2552}'..='\u{256C}').contains(&c)
            })
}

fn is_rule_char(t: &str) -> bool {
    is_vertical(t) || is_horizontal(t) || is_junction(t)
}

/// One printed line: its glyphs left to right.
struct TextLine {
    baseline: f64,
    glyphs: Vec<usize>,
}

/// A column's edge: a bar at one position on consecutive lines.
#[derive(Debug, Clone)]
struct Edge {
    x: f64,
    /// First and last line it is drawn on.
    first: usize,
    last: usize,
    /// The bar glyphs.
    glyphs: Vec<usize>,
    top: f64,
    bottom: f64,
}

/// A horizontal rule found in the text, before it is joined to the edges.
#[derive(Debug, Clone)]
struct Across {
    y: f64,
    x0: f64,
    x1: f64,
    glyphs: Vec<usize>,
}

/// Read the tables a page draws with characters into rules, and take their
/// drawing glyphs out of the page's text. Returns each table's frame: the
/// box its rules draw.
pub fn draw_rules(glyphs: &mut Vec<Glyph>, rules: &mut Vec<Rule>, page: usize) -> Vec<BBox> {
    let lines = text_lines(glyphs);
    if lines.len() < MIN_RUN_LINES {
        return Vec::new();
    }
    let pitch = line_pitch(&lines);
    let edges = edges(glyphs, &lines, pitch);
    if edges.len() < MIN_FRAME_EDGES {
        return Vec::new();
    }
    let frames = frames(&edges);
    if frames.is_empty() {
        return Vec::new();
    }
    let mut boxes: Vec<BBox> = Vec::new();
    let mut taken = vec![false; glyphs.len()];
    let mut added: Vec<Rule> = Vec::new();
    for frame in frames {
        let mut edges: Vec<Edge> = frame.iter().map(|&i| edges[i].clone()).collect();
        edges.sort_by(|a, b| a.x.total_cmp(&b.x));
        let mut acrosses = acrosses(glyphs, &lines, &edges, pitch);
        // An edge runs to the rule that closes it, above and below: the
        // bar's own glyph stops a little short of the line drawn beside it.
        let advance = advance_of(glyphs, &edges);
        for e in edges.iter_mut() {
            let meets = |a: &Across| {
                a.x0 - REACH_ADVANCES * advance <= e.x && e.x <= a.x1 + REACH_ADVANCES * advance
            };
            // Its own first and last lines may be rule lines it crosses;
            // else the nearest rule just beyond its ends, a border the bar
            // falls short of. Not the next box's border a line further on.
            let own_top = lines[e.first].baseline - 0.25 * pitch;
            let own_bottom = lines[e.last].baseline - 0.25 * pitch;
            let at = |y: f64| acrosses.iter().any(|a| meets(a) && (a.y - y).abs() < 0.5);
            e.top = if at(own_top) {
                own_top
            } else {
                acrosses
                    .iter()
                    .filter(|a| meets(a) && a.y >= e.top - pitch && a.y <= e.top + 0.5)
                    .map(|a| a.y)
                    .fold(
                        e.top,
                        |best, y| if best == e.top || y > best { y } else { best },
                    )
            };
            e.bottom = if at(own_bottom) {
                own_bottom
            } else {
                acrosses
                    .iter()
                    .filter(|a| meets(a) && a.y >= e.bottom - 0.5 && a.y <= e.bottom + pitch)
                    .map(|a| a.y)
                    .fold(e.bottom, |best, y| {
                        if best == e.bottom || y < best {
                            y
                        } else {
                            best
                        }
                    })
            };
        }
        boxes.push(BBox {
            x0: edges.iter().map(|e| e.x).fold(f64::INFINITY, f64::min),
            y0: edges.iter().map(|e| e.top).fold(f64::INFINITY, f64::min),
            x1: edges.iter().map(|e| e.x).fold(f64::NEG_INFINITY, f64::max),
            y1: edges
                .iter()
                .map(|e| e.bottom)
                .fold(f64::NEG_INFINITY, f64::max),
        });
        acrosses.extend(spanned_heads(&edges, &acrosses, pitch));
        acrosses.extend(sub_rows(glyphs, &lines, &edges, &acrosses, pitch));
        for e in &edges {
            for &g in &e.glyphs {
                taken[g] = true;
            }
            added.push(Rule {
                bbox: BBox {
                    x0: e.x - 0.25,
                    y0: e.top,
                    x1: e.x + 0.25,
                    y1: e.bottom,
                },
                orientation: Orientation::Vertical,
                page,
            });
        }
        for a in &acrosses {
            for &g in &a.glyphs {
                taken[g] = true;
            }
            // A rule reaches the edge it stops just short of.
            let (mut x0, mut x1) = (a.x0, a.x1);
            let near = |x: f64| {
                edges
                    .iter()
                    .filter(|e| e.top - 0.5 <= a.y && a.y <= e.bottom + 0.5)
                    .map(|e| e.x)
                    .filter(|ex| (ex - x).abs() <= REACH_ADVANCES * advance)
                    .min_by(|p, q| (p - x).abs().total_cmp(&(q - x).abs()))
            };
            if let Some(ex) = near(x0) {
                x0 = x0.min(ex);
            }
            if let Some(ex) = near(x1) {
                x1 = x1.max(ex);
            }
            added.push(Rule {
                bbox: BBox {
                    x0,
                    y0: a.y - 0.25,
                    x1,
                    y1: a.y + 0.25,
                },
                orientation: Orientation::Horizontal,
                page,
            });
        }
    }
    rules.extend(added);
    let mut it = taken.iter();
    glyphs.retain(|_| !*it.next().unwrap());
    boxes
}

/// The page's horizontal glyphs grouped into printed lines by baseline,
/// top to bottom.
fn text_lines(glyphs: &[Glyph]) -> Vec<TextLine> {
    let mut idx: Vec<usize> = (0..glyphs.len())
        .filter(|&i| glyphs[i].is_horizontal() && !glyphs[i].text.is_empty())
        .collect();
    idx.sort_by(|&a, &b| glyphs[a].origin.1.total_cmp(&glyphs[b].origin.1));
    let mut lines: Vec<TextLine> = Vec::new();
    for i in idx {
        let g = &glyphs[i];
        let tol = (g.font_size.max(1.0) as f64) * 0.3;
        match lines.last_mut() {
            Some(l) if (g.origin.1 - l.baseline).abs() < tol => l.glyphs.push(i),
            _ => lines.push(TextLine {
                baseline: g.origin.1,
                glyphs: vec![i],
            }),
        }
    }
    for l in lines.iter_mut() {
        l.glyphs
            .sort_by(|&a, &b| glyphs[a].origin.0.total_cmp(&glyphs[b].origin.0));
    }
    lines
}

/// The usual distance between one line's baseline and the next.
fn line_pitch(lines: &[TextLine]) -> f64 {
    let mut gaps: Vec<f64> = lines
        .windows(2)
        .map(|w| w[1].baseline - w[0].baseline)
        .filter(|g| *g > 1.0)
        .collect();
    if gaps.is_empty() {
        return 12.0;
    }
    gaps.sort_by(f64::total_cmp);
    gaps[gaps.len() / 2]
}

/// The usual pen advance of the frame's bars: one printed character.
fn advance_of(glyphs: &[Glyph], edges: &[Edge]) -> f64 {
    let mut a: Vec<f64> = edges
        .iter()
        .flat_map(|e| e.glyphs.iter())
        .filter_map(|&g| glyphs[g].advance)
        .filter(|a| *a > 0.0)
        .collect();
    if a.is_empty() {
        return 6.0;
    }
    a.sort_by(f64::total_cmp);
    a[a.len() / 2]
}

/// The horizontal middle of a glyph's mark.
fn mid_x(g: &Glyph) -> f64 {
    match g.bbox {
        Some(b) => (b.x0 + b.x1) * 0.5,
        None => g.origin.0 + g.advance.unwrap_or(0.0) * 0.5,
    }
}

/// The x range a glyph's character cell takes on the line.
fn cell_x(g: &Glyph) -> (f64, f64) {
    match (g.advance.filter(|a| *a > 0.0), g.bbox) {
        (Some(a), _) => (g.origin.0, g.origin.0 + a),
        (None, Some(b)) => (b.x0, b.x1),
        (None, None) => (g.origin.0, g.origin.0),
    }
}

/// Column edges: bars at one position, line after line.
fn edges(glyphs: &[Glyph], lines: &[TextLine], pitch: f64) -> Vec<Edge> {
    // Every bar, by line.
    let mut bars: Vec<(f64, usize, usize)> = Vec::new(); // (x, line, glyph)
    for (li, l) in lines.iter().enumerate() {
        for &g in &l.glyphs {
            let t = glyphs[g].text.as_str();
            if (is_vertical(t) || is_junction(t)) && glyphs[g].bbox.is_some() {
                bars.push((mid_x(&glyphs[g]), li, g));
            }
        }
    }
    if bars.len() < MIN_RUN_LINES * MIN_FRAME_EDGES {
        return Vec::new();
    }
    bars.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    // Positions: bars within a fraction of a character of each other.
    let tol = 1.0;
    let mut columns: Vec<Vec<(f64, usize, usize)>> = Vec::new();
    for b in bars {
        match columns.last_mut() {
            Some(c) if b.0 - c[0].0 <= tol => c.push(b),
            _ => columns.push(vec![b]),
        }
    }
    // Text printed where the bar would be. A rule printed there is not: a
    // border passing under the position is where one box ends and the
    // next begins.
    let covered = |li: usize, x: f64| {
        lines[li].glyphs.iter().any(|&g| {
            let (x0, x1) = cell_x(&glyphs[g]);
            let t = glyphs[g].text.as_str();
            !t.trim().is_empty() && !is_rule_char(t) && x0 <= x && x <= x1
        })
    };
    let mut out = Vec::new();
    for mut col in columns {
        col.sort_by_key(|b| b.1);
        let x = col.iter().map(|b| b.0).sum::<f64>() / col.len() as f64;
        let mut run: Vec<(f64, usize, usize)> = Vec::new();
        let mut flush = |run: &mut Vec<(f64, usize, usize)>| {
            if run.len() >= MIN_RUN_LINES {
                let first = run[0].1;
                let last = run[run.len() - 1].1;
                out.push(Edge {
                    x,
                    first,
                    last,
                    glyphs: run.iter().map(|b| b.2).collect(),
                    top: lines[first].baseline - 0.75 * pitch,
                    bottom: lines[last].baseline + 0.25 * pitch,
                });
            }
            run.clear();
        };
        for b in col {
            if let Some(prev) = run.last() {
                if b.1 == prev.1 {
                    continue;
                }
                let skipped = prev.1 + 1..b.1;
                // The next line, or a line or two on where the bars between
                // were printed over by a cell's text.
                let adjacent = lines[b.1].baseline - lines[prev.1].baseline <= 1.5 * pitch;
                let overwritten = skipped.len() <= MAX_OVERWRITTEN_LINES
                    && lines[b.1].baseline - lines[prev.1].baseline
                        <= (skipped.len() as f64 + 1.5) * pitch
                    && skipped.clone().all(|li| covered(li, x));
                if !(adjacent && skipped.is_empty() || overwritten) {
                    flush(&mut run);
                }
            }
            run.push(b);
        }
        flush(&mut run);
    }
    out
}

/// Edges that stand together as one table: at least [`MIN_FRAME_EDGES`]
/// sharing lines. Returned as groups of indices into `edges`.
fn frames(edges: &[Edge]) -> Vec<Vec<usize>> {
    let shares = |a: &Edge, b: &Edge| {
        let lo = a.first.max(b.first);
        let hi = a.last.min(b.last);
        hi >= lo && hi - lo + 1 >= MIN_RUN_LINES
    };
    // Connected groups under "shares lines with".
    let mut group: Vec<usize> = (0..edges.len()).collect();
    fn root(group: &mut [usize], mut i: usize) -> usize {
        while group[i] != i {
            group[i] = group[group[i]];
            i = group[i];
        }
        i
    }
    for i in 0..edges.len() {
        for j in (i + 1)..edges.len() {
            if shares(&edges[i], &edges[j]) {
                let (a, b) = (root(&mut group, i), root(&mut group, j));
                group[a] = b;
            }
        }
    }
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut roots: Vec<usize> = Vec::new();
    for i in 0..edges.len() {
        let r = root(&mut group, i);
        match roots.iter().position(|&x| x == r) {
            Some(k) => out[k].push(i),
            None => {
                roots.push(r);
                out.push(vec![i]);
            }
        }
    }
    out.retain(|g| g.len() >= MIN_FRAME_EDGES);
    out
}

/// The frame's horizontal rules: lines of nothing but rule characters
/// across most of it, and underscore runs struck over a line's text and
/// across its edges.
fn acrosses(glyphs: &[Glyph], lines: &[TextLine], edges: &[Edge], pitch: f64) -> Vec<Across> {
    let left = edges.iter().map(|e| e.x).fold(f64::INFINITY, f64::min);
    let right = edges.iter().map(|e| e.x).fold(f64::NEG_INFINITY, f64::max);
    let first = edges.iter().map(|e| e.first).min().unwrap_or(0);
    let last = edges.iter().map(|e| e.last).max().unwrap_or(0);
    let advance = advance_of(glyphs, edges);
    let width = (right - left).max(1.0);
    let reach = REACH_ADVANCES * advance;
    let mut out = Vec::new();
    // A line just above the frame may be its top border, and one just
    // below its bottom.
    let lo = first.saturating_sub(1);
    let hi = (last + 1).min(lines.len() - 1);
    for li in lo..=hi {
        let l = &lines[li];
        let inside: Vec<usize> = l
            .glyphs
            .iter()
            .copied()
            .filter(|&g| {
                let x = mid_x(&glyphs[g]);
                x >= left - reach && x <= right + reach
            })
            .collect();
        let marks: Vec<usize> = inside
            .iter()
            .copied()
            .filter(|&g| !glyphs[g].text.trim().is_empty())
            .collect();
        if marks.is_empty() {
            continue;
        }
        let all_rule = marks.iter().all(|&g| is_rule_char(&glyphs[g].text));
        if all_rule {
            // Runs of rule characters, one character after another.
            let runs = runs(glyphs, &marks, is_rule_char);
            let covered: f64 = runs
                .iter()
                .filter(|r| r.iter().any(|&g| is_horizontal(&glyphs[g].text)))
                .map(|r| span(glyphs, r).1 - span(glyphs, r).0)
                .sum();
            if covered < MIN_RULE_COVERAGE * width {
                continue;
            }
            // Outside the frame's lines it must be the frame's own border:
            // a ruled line the bars meet, not a separator set apart from it.
            if li < first || li > last {
                let next = if li < first { first } else { last };
                if (lines[next].baseline - l.baseline).abs() > 1.5 * pitch {
                    continue;
                }
            }
            for r in runs {
                if !r.iter().any(|&g| is_horizontal(&glyphs[g].text)) {
                    continue;
                }
                let (x0, x1) = span(glyphs, &r);
                out.push(Across {
                    y: l.baseline - 0.25 * pitch,
                    x0,
                    x1,
                    glyphs: r,
                });
            }
            continue;
        }
        if li < first || li > last {
            continue;
        }
        // An underscore run struck over the line's text and across a bar:
        // the row's lower edge, drawn on the row's last line.
        let unders: Vec<usize> = marks
            .iter()
            .copied()
            .filter(|&g| glyphs[g].text == "_")
            .collect();
        for r in runs(glyphs, &unders, |t| t == "_") {
            if r.len() < 3 {
                continue;
            }
            let (x0, x1) = span(glyphs, &r);
            let struck = marks.iter().any(|&g| {
                glyphs[g].text != "_" && {
                    let (a, b) = cell_x(&glyphs[g]);
                    let m = (a + b) * 0.5;
                    x0 < m && m < x1
                }
            });
            let crosses = edges
                .iter()
                .any(|e| e.first <= li && li <= e.last && x0 + 0.5 < e.x && e.x < x1 - 0.5);
            if struck && crosses {
                out.push(Across {
                    y: l.baseline + 0.25 * pitch,
                    x0,
                    x1,
                    glyphs: r,
                });
            }
        }
    }
    out
}

/// Rules under the labels a header sets across several columns.
///
/// A header names two columns at once by leaving out the bar between them
/// on its first line, `Weight` over `Low | High`; the
/// bar starts on the line below. Drawn with lines, the label would have a
/// rule under it, and the bar would start from that rule. So an edge that
/// starts partway down the frame's first row starts a rule there, across
/// to the edges either side of it that run on up.
fn spanned_heads(edges: &[Edge], acrosses: &[Across], pitch: f64) -> Vec<Across> {
    let left = edges.iter().map(|e| e.x).fold(f64::INFINITY, f64::min);
    let right = edges.iter().map(|e| e.x).fold(f64::NEG_INFINITY, f64::max);
    let width = right - left;
    let top = edges.iter().map(|e| e.top).fold(f64::INFINITY, f64::min);
    // The first row ends at the first rule across most of the frame.
    let Some(first_row_end) = acrosses
        .iter()
        .filter(|a| a.x1 - a.x0 >= MIN_RULE_COVERAGE * width && a.y > top + 0.5 * pitch)
        .map(|a| a.y)
        .min_by(f64::total_cmp)
    else {
        return Vec::new();
    };
    let mut out: Vec<Across> = Vec::new();
    for e in edges {
        if e.top <= top + 0.5 * pitch || e.top >= first_row_end - 0.5 {
            continue;
        }
        // The edges beside it that run on above where it starts.
        let above = |o: &&Edge| o.top < e.top - 0.5 * pitch && o.bottom > e.top;
        let l = edges
            .iter()
            .filter(|o| o.x < e.x)
            .filter(above)
            .map(|o| o.x)
            .fold(f64::NEG_INFINITY, f64::max);
        let r = edges
            .iter()
            .filter(|o| o.x > e.x)
            .filter(above)
            .map(|o| o.x)
            .fold(f64::INFINITY, f64::min);
        if !l.is_finite() || !r.is_finite() {
            continue;
        }
        if !out
            .iter()
            .any(|a| (a.y - e.top).abs() < 0.5 && a.x0 == l && a.x1 == r)
        {
            out.push(Across {
                y: e.top,
                x0: l,
                x1: r,
                glyphs: Vec::new(),
            });
        }
    }
    out
}

/// Rules between the parts of a row that a blank printed line sets apart.
///
/// A line printer cannot draw a rule through a row without a line of its
/// own, so a batch record parts a step's instructions with a blank line
/// instead: `Start time:` beside its blank, a blank line, `Weight:`
/// beside its blank and its limits. Read as one row, every result
/// lands in one cell, apart from the instruction it answers. So a blank
/// line with text above and below it in the same row is a rule across the
/// row. The first column is parted too only where the part below names its
/// own row there; else its label (`A2.`) spans the parts it heads.
fn sub_rows(
    glyphs: &[Glyph],
    lines: &[TextLine],
    edges: &[Edge],
    acrosses: &[Across],
    pitch: f64,
) -> Vec<Across> {
    if edges.len() < MIN_FRAME_EDGES {
        return Vec::new();
    }
    let (left, label_end, right) = (edges[0].x, edges[1].x, edges[edges.len() - 1].x);
    let width = right - left;
    let drawn: std::collections::HashSet<usize> = edges
        .iter()
        .flat_map(|e| e.glyphs.iter())
        .chain(acrosses.iter().flat_map(|a| a.glyphs.iter()))
        .copied()
        .collect();
    // What a line holds: text in the first column, text after it.
    let holds = |li: usize| -> (bool, bool) {
        let mut label = false;
        let mut rest = false;
        for &g in &lines[li].glyphs {
            if drawn.contains(&g) || glyphs[g].text.trim().is_empty() {
                continue;
            }
            let x = mid_x(&glyphs[g]);
            if x > left && x < label_end {
                label = true;
            } else if x > label_end && x < right {
                rest = true;
            }
        }
        (label, rest)
    };
    // The frame's rows: between rules that cross most of it.
    let mut bounds: Vec<f64> = acrosses
        .iter()
        .filter(|a| a.x1 - a.x0 >= MIN_RULE_COVERAGE * width)
        .map(|a| a.y)
        .collect();
    bounds.sort_by(f64::total_cmp);
    let mut out = Vec::new();
    for w in bounds.windows(2) {
        let row: Vec<usize> = (0..lines.len())
            .filter(|&li| {
                lines[li].baseline - 0.25 * pitch > w[0] + 0.5
                    && lines[li].baseline - 0.25 * pitch < w[1] - 0.5
            })
            .collect();
        let held: Vec<(bool, bool)> = row.iter().map(|&li| holds(li)).collect();
        let blank = |k: usize| !held[k].0 && !held[k].1;
        let mut k = 0;
        while k < row.len() {
            if !blank(k) {
                k += 1;
                continue;
            }
            // A run of blank lines.
            let start = k;
            while k < row.len() && blank(k) {
                k += 1;
            }
            let (before, after) = (start > 0, k < row.len());
            if !before || !after {
                continue;
            }
            // The part below, to the next blank line, labels itself or not.
            let end = (k..row.len()).find(|&j| blank(j)).unwrap_or(row.len());
            let new_label = held[k..end].iter().any(|h| h.0);
            let mid = row[(start + k - 1) / 2];
            out.push(Across {
                y: lines[mid].baseline - 0.25 * pitch,
                x0: if new_label { left } else { label_end },
                x1: right,
                glyphs: Vec::new(),
            });
        }
    }
    out
}

/// Maximal runs of glyphs that pass `keep`, each following the last by
/// one character's advance or less.
fn runs(glyphs: &[Glyph], idx: &[usize], keep: impl Fn(&str) -> bool) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    let mut sorted: Vec<usize> = idx.to_vec();
    sorted.sort_by(|&a, &b| glyphs[a].origin.0.total_cmp(&glyphs[b].origin.0));
    for g in sorted {
        if !keep(&glyphs[g].text) {
            if !run.is_empty() {
                out.push(std::mem::take(&mut run));
            }
            continue;
        }
        if let Some(&p) = run.last() {
            let (_, pend) = cell_x(&glyphs[p]);
            let (start, _) = cell_x(&glyphs[g]);
            let step = glyphs[p].advance.unwrap_or(6.0).max(1.0);
            if start - pend > 0.5 * step {
                out.push(std::mem::take(&mut run));
            }
        }
        run.push(g);
    }
    if !run.is_empty() {
        out.push(run);
    }
    out
}

/// The x extent of a run: from a bar's middle where it starts or ends on
/// one, else from its character cells' outer edges.
fn span(glyphs: &[Glyph], run: &[usize]) -> (f64, f64) {
    let first = &glyphs[run[0]];
    let last = &glyphs[run[run.len() - 1]];
    let x0 = if is_vertical(&first.text) || is_junction(&first.text) {
        mid_x(first)
    } else {
        cell_x(first).0
    };
    let x1 = if is_vertical(&last.text) || is_junction(&last.text) {
        mid_x(last)
    } else {
        cell_x(last).1
    };
    (x0, x1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADV: f64 = 7.2;
    const PITCH: f64 = 12.0;

    /// A page printed in a 12pt monospace face from these lines, one
    /// character per 7.2pt and one line per 12pt.
    fn page(lines: &[&str]) -> Vec<Glyph> {
        let mut out = Vec::new();
        for (li, line) in lines.iter().enumerate() {
            let base = 100.0 + li as f64 * PITCH;
            for (ci, ch) in line.chars().enumerate() {
                out.push(glyph(ch, 50.0 + ci as f64 * ADV, base, out.len()));
            }
        }
        out
    }

    fn glyph(ch: char, x: f64, base: f64, idx: usize) -> Glyph {
        let bbox = match ch {
            ' ' => None,
            '|' | '¦' => Some(BBox {
                x0: x + 3.3,
                y0: base - 9.0,
                x1: x + 3.9,
                y1: base + 3.0,
            }),
            '_' => Some(BBox {
                x0: x,
                y0: base + 0.9,
                x1: x + ADV,
                y1: base + 1.5,
            }),
            _ => Some(BBox {
                x0: x + 0.5,
                y0: base - 7.0,
                x1: x + 6.5,
                y1: base,
            }),
        };
        Glyph {
            text: ch.to_string(),
            bbox,
            page: 0,
            origin: (x, base),
            rotation_deg: 0.0,
            font_size: 12.0,
            weight: None,
            italic: false,
            advance: Some(ADV),
            draw_index: idx,
        }
    }

    fn text(glyphs: &[Glyph]) -> String {
        glyphs.iter().map(|g| g.text.as_str()).collect()
    }

    fn count(rules: &[Rule], o: Orientation) -> usize {
        rules.iter().filter(|r| r.orientation == o).count()
    }

    #[test]
    fn a_table_drawn_in_bars_and_underscores_becomes_rules() {
        let mut gs = page(&[
            "  ______________________",
            "|STEP|INSTRUCTIONS|DONE|",
            "|____|____________|____|",
            "|A1. |Weigh it    |    |",
            "|    |and seal it.|____|",
            "|____|____________|____|",
        ]);
        let mut rules = Vec::new();
        draw_rules(&mut gs, &mut rules, 0);
        assert_eq!(count(&rules, Orientation::Vertical), 4);
        assert_eq!(count(&rules, Orientation::Horizontal), 3);
        // The bars and the rule lines leave the text; the words and the
        // blank to sign on stay.
        let t = text(&gs);
        assert!(!t.contains('|'));
        assert!(t.contains("STEPINSTRUCTIONSDONE"));
        assert!(t.contains("and seal it.____"));
        // The top border reaches the left edge it starts short of, and the
        // edges reach up to it.
        let top = rules
            .iter()
            .filter(|r| r.orientation == Orientation::Horizontal)
            .min_by(|a, b| a.bbox.y0.total_cmp(&b.bbox.y0))
            .unwrap();
        let left = rules
            .iter()
            .filter(|r| r.orientation == Orientation::Vertical)
            .min_by(|a, b| a.bbox.x0.total_cmp(&b.bbox.x0))
            .unwrap();
        assert!(top.bbox.x0 <= left.bbox.x1);
        assert!((left.bbox.y0 - top.axis_pos()).abs() < 0.5);
    }

    #[test]
    fn a_bar_printed_over_by_a_long_cell_still_runs_on() {
        let mut gs = page(&[
            "|____|______|____|",
            "|A1. |short |    |",
            "|    |overflowed |",
            "|    |short |    |",
            "|____|______|____|",
        ]);
        let mut rules = Vec::new();
        draw_rules(&mut gs, &mut rules, 0);
        // Four edges, the third unbroken across the line printed over it.
        assert_eq!(count(&rules, Orientation::Vertical), 4);
    }

    #[test]
    fn an_underscore_struck_across_bars_closes_a_row() {
        let mut gs = page(&[
            "¦-----¦-----¦-----¦",
            "¦ S1  ¦  A  ¦     ¦",
            "¦ S1  ¦  B  ¦     ¦",
            "¦-----¦-----¦-----¦",
        ]);
        // Strike `_` over the `A` line, from the second column on.
        let base = 100.0 + PITCH;
        for ci in 6..18 {
            let n = gs.len();
            gs.push(glyph('_', 50.0 + ci as f64 * ADV, base, n));
        }
        let mut rules = Vec::new();
        draw_rules(&mut gs, &mut rules, 0);
        assert_eq!(count(&rules, Orientation::Horizontal), 3);
        assert!(!text(&gs).contains('_'));
    }

    #[test]
    fn a_blank_line_parts_a_step_and_its_label_spans_the_parts() {
        let mut gs = page(&[
            "|____|________|____|",
            "|A1. |Time:   |___ |",
            "|    |        |    |",
            "|    |Mass:   |___ |",
            "|    |        |    |",
            "|A2. |Weight: |___ |",
            "|____|________|____|",
        ]);
        let mut rules = Vec::new();
        draw_rules(&mut gs, &mut rules, 0);
        let mut hs: Vec<(f64, f64)> = rules
            .iter()
            .filter(|r| r.orientation == Orientation::Horizontal)
            .map(|r| (r.axis_pos(), r.bbox.x0))
            .collect();
        hs.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(hs.len(), 4);
        // Between `Time:` and `Mass:` the step's label runs on: the rule
        // starts at the second column. Before `A2.` it crosses the first.
        let first_edge = 50.0 + 3.6;
        assert!(hs[1].1 > first_edge + 10.0);
        assert!(hs[2].1 < first_edge + 1.0);
    }

    #[test]
    fn boxes_one_under_another_stay_apart() {
        let mut gs = page(&[
            "  ______________",
            "|a   |b   |c   |",
            "|a   |b   |c   |",
            "|____|____|____|",
            "  ______________",
            "|d   |e   |f   |",
            "|d   |e   |f   |",
            "|____|____|____|",
        ]);
        let mut rules = Vec::new();
        draw_rules(&mut gs, &mut rules, 0);
        // Each box its own four edges, none running through the border
        // between them.
        let vs: Vec<&Rule> = rules
            .iter()
            .filter(|r| r.orientation == Orientation::Vertical)
            .collect();
        assert_eq!(vs.len(), 8);
        let gap = 100.0 + 4.0 * PITCH - 0.25 * PITCH;
        assert!(vs
            .iter()
            .all(|r| r.bbox.y1 <= gap + 0.5 || r.bbox.y0 >= gap - 0.5));
    }

    #[test]
    fn a_label_over_two_columns_gets_a_rule_under_it() {
        let mut gs = page(&[
            "  _______________________",
            "|    |Readings   |      |",
            "|Name|Low  |High |Total |",
            "|____|_____|_____|______|",
            "|S1  |     |     |      |",
            "|____|_____|_____|______|",
        ]);
        let mut rules = Vec::new();
        draw_rules(&mut gs, &mut rules, 0);
        // A short rule under `Readings`, from its edge to the next.
        let under: Vec<&Rule> = rules
            .iter()
            .filter(|r| {
                r.orientation == Orientation::Horizontal
                    && (r.axis_pos() - (100.0 + 2.0 * PITCH - 0.75 * PITCH)).abs() < 0.5
            })
            .collect();
        assert_eq!(under.len(), 1);
        assert!((under[0].bbox.x0 - (50.0 + 5.0 * ADV + 3.6)).abs() < 0.5);
        assert!((under[0].bbox.x1 - (50.0 + 17.0 * ADV + 3.6)).abs() < 0.5);
    }

    #[test]
    fn bars_in_running_text_are_not_a_table() {
        let mut gs = page(&[
            "Page 1 | Confidential | Draft",
            "Some prose that follows it.",
            "Page 1 | Confidential | Draft",
        ]);
        let before = gs.len();
        let mut rules = Vec::new();
        assert!(draw_rules(&mut gs, &mut rules, 0).is_empty());
        assert_eq!(gs.len(), before);
    }

    #[test]
    fn a_blank_to_fill_in_is_not_a_rule() {
        let mut gs = page(&[
            "|____|______________|",
            "|Lot:|__________    |",
            "|    |(6 digits)    |",
            "|____|______________|",
        ]);
        let mut rules = Vec::new();
        draw_rules(&mut gs, &mut rules, 0);
        assert!(text(&gs).contains("Lot:__________"));
    }

    #[test]
    fn a_separator_line_apart_from_the_table_stays_text() {
        let mut gs = page(&[
            "-----------------------",
            "",
            "|____|____|____|",
            "|a   |b   |c   |",
            "|____|____|____|",
        ]);
        let mut rules = Vec::new();
        draw_rules(&mut gs, &mut rules, 0);
        assert!(text(&gs).starts_with("-----------------------"));
    }
}
