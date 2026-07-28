//! Glyphs → lines.
//!
//! The first layout pass, and the one everything else depends on: paragraphs,
//! headings, tables and reading order all consume lines rather than glyphs.
//!
//! Two things make this less trivial than "group by y":
//!
//! * **Rotation.** In a mechanical drawing a 90° run is an axis title or a
//!   vertical dimension, not body text. One datasheet's schematic page turned
//!   up 196 glyphs at 90°. Grouping those with horizontal text by y-coordinate would
//!   splice an axis label into a paragraph. We bucket by orientation first and
//!   assemble lines independently within each bucket.
//! * **Missing spaces.** PDFs position words rather than emitting space
//!   characters, so a gap wider than a fraction of the font size must be
//!   reconstructed geometrically.

use crate::geom::BBox;
use crate::glyph::Glyph;
use unicode_normalization::UnicodeNormalization;

/// Gap, as a fraction of font size, above which we insert a synthetic space —
/// the *fallback* when a document's own gap distribution is not clearly
/// bimodal (see [`adaptive_space_ratio`]).
///
/// Re-benchmarked at 0.15/0.18/0.20/0.25 after the pen-advance change moved the
/// measurement basis. 0.20 wins on overall, NID and TEDS; 0.25 on MHS alone.
/// No single value works for every document: one real document puts its word
/// gaps at 0.196-0.209, squarely astride this constant, and lost half its
/// spaces ("It may ofcoursebe saidthatquantummechanics") while other
/// documents kern tightly enough that 0.15 splits words. That spread is why
/// the threshold is adaptive and this constant is only the fallback.
const SPACE_GAP_RATIO: f64 = 0.20;

/// Space threshold for digit-to-digit pairs, which need their own value.
///
/// Fonts commonly use *tabular figures*: every digit occupies the same advance
/// regardless of glyph width, so a narrow `1` leaves a wide edge-to-edge gap.
/// Measured over `eval/corpus` (24.8k digit pairs): intra-number gaps run to
/// 0.25 (85.4% cumulative), then a valley at 0.25-0.35 holding 0.6%. The Latin
/// threshold of 0.25 sat just under a real `1`->`3` gap of 0.2515 and split
/// `Page 13 of 15` into `Page 1 3 of 1 5`.
const DIGIT_SPACE_GAP_RATIO: f64 = 0.32;

/// True for scripts that do not separate words with spaces. Geometric space
/// reconstruction must be suppressed between two such characters: full-width
/// glyphs are wide, so ordinary inter-character advance exceeds the Latin word
/// threshold and would inject spaces into `（別紙２）`.
fn is_spaceless_script(c: char) -> bool {
    matches!(c,
        '\u{3000}'..='\u{303F}' |   // CJK punctuation
        '\u{3040}'..='\u{30FF}' |   // hiragana, katakana
        '\u{3400}'..='\u{4DBF}' |   // CJK ext A
        '\u{4E00}'..='\u{9FFF}' |   // CJK unified
        '\u{F900}'..='\u{FAFF}' |   // CJK compat
        '\u{FF00}'..='\u{FF60}' |   // full-width forms
        '\u{AC00}'..='\u{D7AF}') // hangul
}

/// Baseline separation above which two glyphs are on different lines, as a
/// fraction of font size. Anchoring on the baseline rather than the bbox top
/// is essential: cap-height, x-height and descender glyphs on one visual line
/// have different tops but share a baseline.
const BASELINE_TOLERANCE: f64 = 0.3;

/// Gap, as a fraction of font size, above which glyphs sharing a baseline
/// belong to *different* blocks rather than the same line.
///
/// Measured over `eval/corpus` (305k adjacent-glyph pairs): intra-word gaps end
/// around 0.20, word spaces peak at 0.35-0.40 and are exhausted by ~1.0, and
/// then the distribution is empty until a 1.5% tail beyond 5.0 — column
/// gutters, table cell boundaries, and header left/right groups. 1.5 sits in
/// that empty band, so the split is insensitive to the exact value.
const BLOCK_GAP_RATIO: f64 = 1.5;

/// A run of glyphs sharing one baseline and orientation.
#[derive(Debug, Clone)]
pub struct Line {
    pub text: String,
    pub bbox: BBox,
    pub page: usize,
    pub rotation_bucket: i32,
    /// Indices into the page's glyph vector, in visual order. Keeps the
    /// offset→bbox chain intact through the layout layer.
    pub glyphs: Vec<usize>,
    /// Median font size — the primary heading signal downstream.
    pub font_size: f32,
    /// True when most of the line's glyphs are bold.
    pub bold: bool,
}

impl Line {
    pub fn is_horizontal(&self) -> bool {
        self.rotation_bucket == 0
    }
}

/// Assemble lines for a page, segmenting columns first.
///
/// This is the entry point callers should use. Column segmentation must precede
/// line assembly: [`assemble`] groups glyphs sharing a baseline, and in a
/// multi-column layout the columns share baselines, so it would concatenate
/// them (defect L8).
pub fn assemble_page(glyphs: &[Glyph]) -> Vec<Line> {
    assemble_columns(glyphs).into_iter().flatten().collect()
}

/// Assemble lines grouped by column, one Vec per column in left-to-right order.
///
/// Downstream passes need the grouping, not just the flattened order: block
/// assembly decides whether a line was *wrapped* by comparing it to the right
/// edge of its own column, and a page-wide edge makes every line in a narrow
/// left column look short.
pub fn assemble_columns(glyphs: &[Glyph]) -> Vec<Vec<Line>> {
    assemble_columns_with_rules(glyphs, &[], 0)
}

/// Columns stated by a rank of heading rules, applied only to the rows they
/// actually govern.
///
/// A ruler's columns are not the page's columns. The rank sits below a
/// full-width title and intro, and often below the headings it underlines, so
/// applying its cuts to the whole page slices the title into fragments -
/// "Doing Bu | A Focus" - which is worse than the fusion it set out to fix.
///
/// The band is where the ruler's own gutters are clear of ink: everything
/// above the lowest glyph that crosses a gutter is one full-width region and
/// keeps its reading order, everything below is partitioned by the cuts. That
/// is the same test [`crate::column::doubt`] uses to find where a page is
/// genuinely in columns, asked of a gutter that geometry stated rather than
/// one the projection inferred.
fn ruler_banded(
    glyphs: &[Glyph],
    rules: &[crate::rule::Rule],
    page: usize,
) -> Option<Vec<Vec<Line>>> {
    let boxed: Vec<&Glyph> = glyphs
        .iter()
        .filter(|g| g.bbox.is_some() && g.is_horizontal() && !g.text.trim().is_empty())
        .collect();
    if boxed.is_empty() {
        return None;
    }
    let min_x = boxed
        .iter()
        .map(|g| g.bbox.unwrap().x0)
        .fold(f64::MAX, f64::min);
    let max_x = boxed
        .iter()
        .map(|g| g.bbox.unwrap().x1)
        .fold(f64::MIN, f64::max);
    let r = crate::column::ruler(rules, page, (min_x, max_x))?;

    // The band is the run of rows around the rank where its gutters are
    // clear. Bounded both ways: a title above and a footer below both cross
    // the gutters, and taking "everything below the last crossing" would let
    // the footer close the band to nothing.
    let crosses = |b: &crate::geom::BBox| {
        r.gutters
            .iter()
            .any(|(g0, g1)| b.x1 > *g0 + 0.5 && b.x0 < *g1 - 0.5)
    };
    let crossing: Vec<crate::geom::BBox> = boxed
        .iter()
        .map(|g| g.bbox.unwrap())
        .filter(crosses)
        .collect();
    let top = crossing
        .iter()
        .filter(|b| b.y1 <= r.axis)
        .map(|b| b.y1)
        .fold(f64::MIN, f64::max);
    let bottom = crossing
        .iter()
        .filter(|b| b.y0 >= r.axis)
        .map(|b| b.y0)
        .fold(f64::MAX, f64::min);

    let band_of = |g: &Glyph| -> u8 {
        let y = match g.bbox {
            Some(b) => (b.y0 + b.y1) * 0.5,
            None => g.origin.1,
        };
        if y <= top {
            0
        } else if y >= bottom {
            2
        } else {
            1
        }
    };
    let mut above: Vec<Glyph> = Vec::new();
    let mut inside: Vec<Glyph> = Vec::new();
    let mut after: Vec<Glyph> = Vec::new();
    for g in glyphs {
        match band_of(g) {
            0 => above.push(g.clone()),
            2 => after.push(g.clone()),
            _ => inside.push(g.clone()),
        }
    }
    if inside.is_empty() {
        return None;
    }
    let below = inside;

    let cols: Vec<crate::column::Column> = {
        let mut out = Vec::with_capacity(r.cuts.len() + 1);
        let mut left = min_x;
        for c in &r.cuts {
            out.push(crate::column::Column { x0: left, x1: *c });
            left = *c;
        }
        out.push(crate::column::Column {
            x0: left,
            x1: max_x + 2.0,
        });
        out
    };

    let mut result: Vec<Vec<Line>> = Vec::with_capacity(cols.len() + 1);
    if !above.is_empty() {
        result.push(assemble(&above));
    }
    for part in crate::column::partition(&below, &cols) {
        if !part.is_empty() {
            result.push(assemble(&part));
        }
    }
    if !after.is_empty() {
        result.push(assemble(&after));
    }
    Some(result)
}

/// As [`assemble_columns`], also given the page's drawn rules so a rank of
/// heading rules can state columns the whitespace projection cannot see.
pub fn assemble_columns_with_rules(
    glyphs: &[Glyph],
    rules: &[crate::rule::Rule],
    page: usize,
) -> Vec<Vec<Line>> {
    if let Some(banded) = ruler_banded(glyphs, rules, page) {
        return banded;
    }
    let cols = crate::column::detect(glyphs);
    if cols.len() <= 1 {
        return vec![assemble(glyphs)];
    }
    let mut per_col: Vec<Vec<Line>> = crate::column::partition(glyphs, &cols)
        .iter()
        .map(|part| assemble(part))
        .collect();
    if std::env::var("FDOC_NO_REJOIN").is_err() {
        rejoin_spanning_lines(&mut per_col, &cols);
    }
    per_col
}

/// Repair lines that a column cut split in half.
///
/// A full-width title or rule crosses the gutter, so partitioning by glyph
/// centre puts its first half in one column and its second in the next —
/// `"TPS543x 3A, Wide Input R"` / `"ange, Step-Down Converter"`. Where a line
/// ends flush against a cut and another begins flush on the far side at the
/// same baseline, they were one line; move the fragment back and rejoin.
fn rejoin_spanning_lines(per_col: &mut [Vec<Line>], cols: &[crate::column::Column]) {
    // Tolerance for "flush against the cut", in font-size units.
    const FLUSH: f64 = 1.5;

    /// Largest fraction of a column's lines that may look like spanning
    /// fragments before we conclude they are not.
    ///
    /// A genuine spanning line — a full-width title over two columns — is rare.
    /// Justified two-column body text is the opposite: *every* left line ends
    /// at the gutter and every right line starts just past it, on the same
    /// baseline. Without this test the rejoin fired on every row and silently
    /// undid column segmentation, merging `…gratitude to the teams` with
    /// `Ethics Statement` on the academic papers that dominate the MHS gap.
    const MAX_SPANNING_FRACTION: f64 = 0.25;

    for i in 0..per_col.len().saturating_sub(1) {
        let cut = cols[i].x1;
        // Snapshot the left column's line geometry so the retain closure below
        // does not borrow `per_col` while it is being mutated.
        let left_edges: Vec<(f64, f64, f64, i32)> = per_col[i]
            .iter()
            .map(|l| {
                (
                    l.bbox.x1,
                    l.bbox.y0,
                    l.font_size.max(1.0) as f64,
                    l.rotation_bucket,
                )
            })
            .collect();
        // Count first: if most rows look like spanning fragments, this is a
        // two-column layout and none of them are.
        let candidates = per_col[i + 1]
            .iter()
            .filter(|r| {
                let fs = r.font_size.max(1.0) as f64;
                (r.bbox.x0 - cut).abs() <= fs * FLUSH
                    && left_edges.iter().any(|(x1, y0, _, rot)| {
                        (x1 - cut).abs() < fs * FLUSH
                            && (y0 - r.bbox.y0).abs() < fs * 0.5
                            && *rot == r.rotation_bucket
                    })
            })
            .count();
        let limit = (per_col[i + 1].len().max(per_col[i].len()) as f64 * MAX_SPANNING_FRACTION)
            .ceil() as usize;
        if candidates > limit {
            continue;
        }

        let mut moved: Vec<Line> = Vec::new();
        per_col[i + 1].retain(|r| {
            let fs = r.font_size.max(1.0) as f64;
            if (r.bbox.x0 - cut).abs() > fs * FLUSH {
                return true;
            }
            let matches = left_edges.iter().any(|(x1, y0, _, rot)| {
                (x1 - cut).abs() < fs * FLUSH
                    && (y0 - r.bbox.y0).abs() < fs * 0.5
                    && *rot == r.rotation_bucket
            });
            if matches {
                moved.push(r.clone());
                false
            } else {
                true
            }
        });
        for m in moved {
            if let Some(l) = per_col[i].iter_mut().find(|l| {
                let fs = l.font_size.max(1.0) as f64;
                (l.bbox.x1 - cut).abs() < fs * FLUSH && (l.bbox.y0 - m.bbox.y0).abs() < fs * 0.5
            }) {
                // Fragments of one word must not gain a space: rejoin exactly
                // unless the geometry shows a real gap.
                let fs = l.font_size.max(1.0) as f64;
                if m.bbox.x0 - l.bbox.x1 > fs * SPACE_GAP_RATIO && !l.text.ends_with(' ') {
                    l.text.push(' ');
                }
                l.text.push_str(&m.text);
                l.bbox = l.bbox.union(&m.bbox);
                l.glyphs.extend(m.glyphs.iter().copied());
            }
        }
    }
}

/// Assemble lines from a single column's glyphs.
///
/// `glyphs` must already have been de-duplicated ([`crate::dedup`]); faux-bold
/// overprint would otherwise double every character in the output.
pub fn assemble(glyphs: &[Glyph]) -> Vec<Line> {
    let mut buckets: std::collections::BTreeMap<i32, Vec<usize>> = Default::default();
    for (i, g) in glyphs.iter().enumerate() {
        // Keep outline-less glyphs when they carry text. A space character has
        // no outline, but when the PDF encodes one explicitly that is ground
        // truth about a word boundary — strictly better than re-deriving it
        // from a geometric gap. Dropping them produced `buyertargeting`
        // (defect L2): the space was present in the content stream and we
        // discarded it. `origin` still positions them, so line grouping works.
        if g.bbox.is_none() && g.text.is_empty() {
            continue;
        }
        buckets.entry(g.rotation_bucket()).or_default().push(i);
    }

    let mut lines = Vec::new();
    let mut groups: Vec<(Vec<usize>, i32, bool)> = Vec::new();
    for (bucket, mut idxs) in buckets {
        // For 90°/270° runs the roles of x and y swap: the text advances along
        // y and lines stack along x.
        let vertical = bucket.rem_euclid(180) == 90;
        idxs.sort_by(|&a, &b| {
            // Origin, not bbox: outline-less space glyphs have no box but do
            // have a pen position, and they must stay in sequence.
            let (oa, ob) = (glyphs[a].origin, glyphs[b].origin);
            if vertical {
                cmp(oa.0, ob.0).then(cmp(ob.1, oa.1))
            } else {
                cmp(oa.1, ob.1).then(cmp(oa.0, ob.0))
            }
        });

        // The lexicographic sort lets sub-point baseline jitter override the
        // advance axis entirely. One document draws two runs of one visual
        // line at y=98.54 and y=98.53; the right-hand run sorted first and the
        // line read "for each moment ofDefinition 1. A universe U…" — and the
        // backwards pen jump is a negative gap, so no space is inserted
        // either. Subscripts are the same failure at larger scale: an origin
        // 1.5pt below the baseline sorted the glyph after its whole row.
        //
        // Within a baseline cluster, re-sort along the advance axis. Clusters
        // are anchored at their first glyph — not chained pairwise — so jitter
        // cannot drift a cluster across genuinely distinct rows.
        let mut start = 0;
        for i in 1..=idxs.len() {
            if i < idxs.len() && same_line(&glyphs[idxs[start]], &glyphs[idxs[i]], vertical) {
                continue;
            }
            idxs[start..i].sort_by(|&a, &b| {
                let (oa, ob) = (glyphs[a].origin, glyphs[b].origin);
                if vertical {
                    cmp(ob.1, oa.1)
                } else {
                    cmp(oa.0, ob.0)
                }
            });
            start = i;
        }

        let mut cur: Vec<usize> = Vec::new();
        for i in idxs {
            if cur.is_empty() {
                cur.push(i);
                continue;
            }
            let prev = *cur.last().unwrap();
            if same_line(&glyphs[prev], &glyphs[i], vertical)
                && !block_gap(&glyphs[prev], &glyphs[i], vertical)
            {
                cur.push(i);
            } else {
                groups.push((std::mem::replace(&mut cur, vec![i]), bucket, vertical));
            }
        }
        if !cur.is_empty() {
            groups.push((cur, bucket, vertical));
        }
    }

    // Text is built only after every group exists, because the space threshold
    // is derived from the whole column's gap distribution, not fixed.
    let ratio = adaptive_space_ratio(glyphs, &groups);
    for (idxs, bucket, vertical) in &groups {
        if let Some(l) = build_line(glyphs, idxs, *bucket, *vertical, ratio) {
            lines.push(l);
        }
    }

    // Restore document order across buckets. This is a provisional order —
    // real reading order (XY-Cut++) is a later pass over blocks, not lines.
    lines.sort_by(|a, b| cmp(a.bbox.y0, b.bbox.y0).then(cmp(a.bbox.x0, b.bbox.x0)));

    // Same jitter guard as the glyph sort above, at line level: two fragments
    // of one visual row (split by a block gap) carry tops that differ by the
    // height of an ascender or by plain rounding, and strict (y, x) then puts
    // the right-hand fragment first. Within a row, left must come first.
    let mut start = 0;
    for i in 1..=lines.len() {
        if i < lines.len() && {
            let (a, b) = (&lines[start], &lines[i]);
            let tol = a.font_size.max(b.font_size).max(1.0) as f64 * BASELINE_TOLERANCE;
            (b.bbox.y0 - a.bbox.y0).abs() < tol
        } {
            continue;
        }
        lines[start..i].sort_by(|a, b| cmp(a.bbox.x0, b.bbox.x0));
        start = i;
    }
    lines
}

fn cmp(a: f64, b: f64) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}

fn same_line(a: &Glyph, b: &Glyph, vertical: bool) -> bool {
    // Compare baselines, not boxes.
    let delta = if vertical {
        a.origin.0 - b.origin.0
    } else {
        a.origin.1 - b.origin.1
    };
    let scale = a.font_size.max(b.font_size).max(1.0) as f64;
    delta.abs() < scale * BASELINE_TOLERANCE
}

/// True when two glyphs on one baseline are separated by enough space to be
/// separate blocks — a column gutter, a table cell boundary, or a header's
/// left and right groups. Splitting here is what stops `www.ti.com` and
/// `SLFS022K` becoming one token (defect L1).
fn block_gap(a: &Glyph, b: &Glyph, vertical: bool) -> bool {
    let (Some(ba), Some(bb)) = (a.bbox, b.bbox) else {
        return false;
    };
    let gap = if vertical {
        bb.y0 - ba.y1
    } else {
        bb.x0 - ba.x1
    };
    let scale = a.font_size.max(b.font_size).max(1.0) as f64;
    gap > scale * BLOCK_GAP_RATIO
}

/// Derive the synthetic-space threshold from this column's own gap statistics.
///
/// Word gaps vary by typeface and typesetter: one document sets them at
/// 0.196–0.209 font-size units with intra-word gaps ending at 0.07, so its
/// ideal threshold is ~0.13 — while other corpus documents pack intra-word
/// gaps past 0.15. No constant serves both; the global sweep proved it
/// (0.15/0.18/0.20/0.25 all within noise of each other, each splitting a
/// different set of documents wrongly).
///
/// The distribution itself says where the cut belongs: intra-word gaps form
/// one mode near zero, word spaces another, with a valley between. The
/// threshold is the valley's midpoint — but only when the histogram is
/// genuinely bimodal. Two guards keep degenerate shapes on the fallback:
///
/// * A document whose PDF encodes explicit space characters has *no* upper
///   mode — every gap is kerning. There the longest empty run stretches to
///   the histogram's end with nothing above it, and cutting inside it would
///   split words at every wide kern. Requiring mass above the valley rejects
///   this shape.
/// * A sparse column has too few pairs for the histogram to mean anything.
fn adaptive_space_ratio(glyphs: &[Glyph], groups: &[(Vec<usize>, i32, bool)]) -> f64 {
    /// Fewest same-line gaps before the histogram is trusted.
    const MIN_PAIRS: usize = 200;
    /// Bin width in font-size units; range covers [0, 0.6).
    const BIN: f64 = 0.01;
    const N_BINS: usize = 60;
    /// A valley must be at least this wide (in fs units) to be a valley and
    /// not a chance hole in one mode.
    const MIN_VALLEY: f64 = 0.04;
    /// Fraction of pairs that must sit *above* a valley for it to separate
    /// words from spaces rather than spaces from nothing.
    const MIN_UPPER_MASS: f64 = 0.02;
    /// A bin counts as empty up to this fraction of pairs — stray math and
    /// punctuation kerning sprinkle the valley without filling it.
    const EMPTY_TOLERANCE: f64 = 0.002;
    /// Sanity range for the derived threshold.
    const CLAMP: (f64, f64) = (0.10, 0.30);

    let mut hist = [0usize; N_BINS];
    let mut n_pairs = 0usize;
    for (idxs, _, vertical) in groups {
        if *vertical {
            continue;
        }
        for w in idxs.windows(2) {
            let (a, b) = (&glyphs[w[0]], &glyphs[w[1]]);
            // Mirror build_line's measurement exactly: pen end to ink start.
            let (Some(adv), Some(bb)) = (a.advance, b.bbox) else {
                continue;
            };
            let last = a.text.chars().last();
            let next = b.text.chars().next();
            if last.is_some_and(is_spaceless_script) && next.is_some_and(is_spaceless_script) {
                continue;
            }
            let gap = (bb.x0.min(b.origin.0) - (a.origin.0 + adv)) / b.font_size.max(1.0) as f64;
            n_pairs += 1;
            let bin = (gap / BIN).floor();
            if (0.0..N_BINS as f64).contains(&bin) {
                hist[bin as usize] += 1;
            }
        }
    }
    if n_pairs < MIN_PAIRS {
        return SPACE_GAP_RATIO;
    }

    let empty_max = ((n_pairs as f64 * EMPTY_TOLERANCE).floor() as usize).max(1);
    let above: Vec<usize> = {
        // above[i] = pairs in bins i.. — for the upper-mass guard.
        let mut v = vec![0usize; N_BINS + 1];
        for i in (0..N_BINS).rev() {
            v[i] = v[i + 1] + hist[i];
        }
        v
    };

    // Longest run of empty bins with real mass above it.
    let mut best: Option<(usize, usize)> = None; // (start, len)
    let mut run: Option<usize> = None;
    for i in 0..=N_BINS {
        let is_empty = i < N_BINS && hist[i] <= empty_max;
        match (is_empty, run) {
            (true, None) => run = Some(i),
            (false, Some(s)) => {
                let len = i - s;
                let upper = above[i] as f64 / n_pairs as f64;
                if len as f64 * BIN >= MIN_VALLEY
                    && upper >= MIN_UPPER_MASS
                    && best.is_none_or(|(_, l)| len > l)
                {
                    best = Some((s, len));
                }
                run = None;
            }
            _ => {}
        }
    }
    match best {
        Some((s, len)) => ((s as f64 + len as f64 / 2.0) * BIN).clamp(CLAMP.0, CLAMP.1),
        None => SPACE_GAP_RATIO,
    }
}

/// Word gaps in letter-spaced text sit this far above the letter gaps.
const TRACKING_WORD_FACTOR: f64 = 1.5;

fn build_line(
    glyphs: &[Glyph],
    idxs: &[usize],
    bucket: i32,
    vertical: bool,
    space_ratio: f64,
) -> Option<Line> {
    if idxs.is_empty() {
        return None;
    }
    // Letter-spaced (tracked) display text defeats the ordinary space rule:
    // when *every* letter gap exceeds the threshold, the line comes out as
    // "H O W C A N" (a poster headline set in tracked caps). Tracked text still
    // keeps its word gaps above its letter gaps, so when most gaps on a line
    // are over the threshold and the glyphs are single letters, the line's
    // own median gap becomes the yardstick instead. Digit rows (chart axes)
    // are excluded — "1 2 3 4 5" is five numbers, not a tracked word.
    let (space_ratio, tracked) = {
        let mut gaps: Vec<f64> = Vec::new();
        let mut prev: Option<f64> = None;
        let mut letters = 0usize;
        let mut drawn = 0usize;
        for &i in idxs {
            let g = &glyphs[i];
            // Bridge across bbox-less glyphs (explicit spaces): tracked text
            // is sometimes typed with real space characters between letters,
            // and the geometry must be measured through them.
            if let (Some(b), Some(a)) = (g.bbox, g.advance) {
                let start = if vertical { b.y0 } else { b.x0.min(g.origin.0) };
                if let Some(pe) = prev {
                    gaps.push((start - pe) / g.font_size.max(1.0) as f64);
                }
                prev = Some(if vertical { b.y1 } else { g.origin.0 + a });
                drawn += 1;
                if g.text.chars().count() == 1 && g.text.chars().all(char::is_alphabetic) {
                    letters += 1;
                }
            }
        }
        let above = gaps.iter().filter(|g| **g > space_ratio).count();
        if gaps.len() >= 5 && above * 10 >= gaps.len() * 6 && letters * 10 >= drawn * 7 {
            let mut sorted = gaps;
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            (
                (sorted[sorted.len() / 2] * TRACKING_WORD_FACTOR).max(space_ratio),
                true,
            )
        } else {
            (space_ratio, false)
        }
    };
    let mut bbox: Option<BBox> = None;
    let mut text = String::new();
    let mut prev_end: Option<f64> = None;
    let mut sizes: Vec<f32> = Vec::new();

    for &i in idxs {
        let g = &glyphs[i];
        sizes.push(g.font_size);
        match g.bbox {
            Some(b) => {
                let start = if vertical { b.y0 } else { b.x0 };
                // Prefer the pen position over the glyph box for the *start*
                // too, so a left side bearing does not manufacture a gap.
                let start = if vertical {
                    start
                } else {
                    start.min(g.origin.0)
                };
                // Geometric fallback: many PDFs position words without emitting
                // a space character at all, so a wide gap has to become one.
                if let Some(pe) = prev_end {
                    let prev_c = text.chars().last();
                    let next_c = g.text.chars().next();
                    let both_cjk = prev_c.is_some_and(is_spaceless_script)
                        && next_c.is_some_and(is_spaceless_script);
                    // Digit-adjacent covers more than digit pairs: tabular
                    // figures leave the same wide gap before a decimal point or
                    // a unit sign, giving "1 .22V" and "1 %".
                    let numeric_pair = prev_c.is_some_and(|c| c.is_ascii_digit())
                        && next_c
                            .is_some_and(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '%'));
                    let both_digit = numeric_pair
                        || (prev_c.is_some_and(|c| matches!(c, '.' | ','))
                            && next_c.is_some_and(|c| c.is_ascii_digit()));
                    let ratio = if both_digit {
                        DIGIT_SPACE_GAP_RATIO
                    } else {
                        space_ratio
                    };
                    if !both_cjk && start - pe > g.font_size as f64 * ratio && !text.ends_with(' ')
                    {
                        text.push(' ');
                    }
                }
                // End of the previous glyph in *pen* space, not ink space.
                // A right overhang (`f`, `y`, `r`) makes the ink box extend
                // past where the pen actually stops, shrinking the measured gap
                // and swallowing the following space — `of the` became `ofthe`
                // on 296 occurrences across the corpus against 56 in ground
                // truth. Falls back to the ink box when no advance is known.
                prev_end = Some(if vertical {
                    b.y1
                } else {
                    match g.advance {
                        Some(a) => g.origin.0 + a,
                        None => b.x1,
                    }
                });
                bbox = Some(match bbox {
                    Some(acc) => acc.union(&b),
                    None => b,
                });
                text.push_str(&g.text);
            }
            None => {
                // Outline-less but text-bearing: an explicit space. Emit it and
                // leave prev_end alone so the following glyph is measured from
                // the last *visible* mark, not from a zero-width position.
                // On a tracked line the explicit spaces ARE the tracking, so
                // the geometric decision above takes over instead.
                if !tracked && !text.is_empty() && !text.ends_with(' ') {
                    text.push(' ');
                }
            }
        }
    }

    let bbox = bbox?;
    // NFKC here so line text is directly usable by NER and search. The raw
    // offsets that resolve to bounding boxes live in `glyphs`, so normalizing
    // the display text costs us nothing (T1.4 in eval/TEST_PLAN.md).
    let text: String = text.trim().nfkc().collect();
    if text.is_empty() {
        return None;
    }
    // Median, not mean: robust to a stray oversized glyph (drop cap, symbol).
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let font_size = sizes[sizes.len() / 2];

    // Most, not any: a single bold word inside a sentence does not make the
    // line a heading.
    let weighted = idxs.iter().filter(|&&i| glyphs[i].weight.is_some()).count();
    let heavy = idxs
        .iter()
        .filter(|&&i| glyphs[i].weight.unwrap_or(400) >= 600)
        .count();
    let bold = weighted > 0 && heavy * 2 > weighted;

    Some(Line {
        text,
        bbox,
        page: glyphs[idxs[0]].page,
        rotation_bucket: bucket,
        glyphs: idxs.to_vec(),
        font_size,
        bold,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(text: &str, x: f64, y: f64, w: f64, rot: f32) -> Glyph {
        Glyph {
            text: text.into(),
            bbox: Some(BBox {
                x0: x,
                y0: y,
                x1: x + w,
                y1: y + 10.0,
            }),
            page: 0,
            origin: (x, y + 10.0),
            rotation_deg: rot,
            font_size: 10.0,
            weight: None,
            advance: None,
            draw_index: 0,
        }
    }

    #[test]
    fn groups_one_line_and_reconstructs_spaces() {
        let v = vec![
            g("H", 0.0, 0.0, 5.0, 0.0),
            g("y", 5.0, 0.0, 5.0, 0.0),
            g("C", 20.0, 0.0, 5.0, 0.0),
            g("y", 25.0, 0.0, 5.0, 0.0),
        ];
        let lines = assemble(&v);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Hy Cy");
    }

    #[test]
    fn no_space_inserted_within_a_word() {
        let v = vec![
            g("c", 0.0, 0.0, 5.0, 0.0),
            g("a", 5.0, 0.0, 5.0, 0.0),
            g("t", 10.0, 0.0, 5.0, 0.0),
        ];
        assert_eq!(assemble(&v)[0].text, "cat");
    }

    #[test]
    fn splits_on_new_line() {
        let v = vec![g("a", 0.0, 0.0, 5.0, 0.0), g("b", 0.0, 20.0, 5.0, 0.0)];
        assert_eq!(assemble(&v).len(), 2);
    }

    #[test]
    fn justified_two_column_text_is_not_rejoined() {
        // Every left line ends at the gutter and every right line starts just
        // past it, on the same baseline — the shape of justified two-column
        // body text. Rejoining these silently undoes column segmentation.
        let mut v = Vec::new();
        for row in 0..30 {
            let y = row as f64 * 12.0;
            for i in 0..20 {
                v.push(g("a", 50.0 + i as f64 * 10.0, y, 9.0, 0.0));
            }
            for i in 0..20 {
                v.push(g("b", 280.0 + i as f64 * 10.0, y, 9.0, 0.0));
            }
        }
        let cols = assemble_columns(&v);
        assert_eq!(cols.len(), 2, "two columns expected");
        assert!(
            cols[0].iter().all(|l| !l.text.contains('b')),
            "left column must not absorb right-column text"
        );
    }

    #[test]
    fn rotated_text_never_merges_with_horizontal() {
        // A 90° axis label sharing the y-range of horizontal body text —
        // the case that motivates orientation bucketing.
        let v = vec![
            g("b", 0.0, 0.0, 5.0, 0.0),
            g("o", 5.0, 0.0, 5.0, 0.0),
            g("A", 50.0, 0.0, 5.0, 90.0),
            g("x", 50.0, 10.0, 5.0, 90.0),
        ];
        let lines = assemble(&v);
        assert_eq!(
            lines.len(),
            2,
            "horizontal and 90deg runs must stay separate"
        );
        let rots: Vec<i32> = lines.iter().map(|l| l.rotation_bucket).collect();
        assert!(rots.contains(&0) && rots.contains(&90));
    }

    #[test]
    fn tabular_figures_do_not_split_a_number() {
        // A narrow `1` on a full digit advance leaves a 0.28 gap - above the
        // Latin word threshold but below the digit threshold.
        let v = vec![g("1", 0.0, 0.0, 2.0, 0.0), g("3", 4.8, 0.0, 5.0, 0.0)];
        assert_eq!(assemble(&v)[0].text, "13");
    }

    #[test]
    fn genuinely_separated_numbers_still_split() {
        let v = vec![g("1", 0.0, 0.0, 5.0, 0.0), g("3", 9.0, 0.0, 5.0, 0.0)];
        assert_eq!(assemble(&v)[0].text, "1 3");
    }

    #[test]
    fn no_geometric_spaces_between_cjk_characters() {
        // Full-width glyphs advance wider than the Latin word threshold; a
        // space here would corrupt `（別紙２）` into `( 別 紙 2 )`.
        let v = vec![
            g("\u{FF08}", 0.0, 0.0, 9.0, 0.0),
            g("\u{5225}", 12.0, 0.0, 9.0, 0.0),
            g("\u{7D19}", 24.0, 0.0, 9.0, 0.0),
        ];
        let lines = assemble(&v);
        assert_eq!(lines[0].text, "(別紙", "no spaces between CJK chars");
    }

    #[test]
    fn latin_still_gets_geometric_spaces() {
        let v = vec![g("a", 0.0, 0.0, 5.0, 0.0), g("b", 12.0, 0.0, 5.0, 0.0)];
        assert_eq!(assemble(&v)[0].text, "a b");
    }

    #[test]
    fn wide_gap_splits_into_separate_lines() {
        // Two groups on one baseline, separated by 3x font size: a column
        // gutter, not a word space.
        let v = vec![
            g("a", 0.0, 0.0, 5.0, 0.0),
            g("b", 5.0, 0.0, 5.0, 0.0),
            g("c", 60.0, 0.0, 5.0, 0.0),
        ];
        let lines = assemble(&v);
        assert_eq!(lines.len(), 2, "wide gap must split blocks");
        assert_eq!(lines[0].text, "ab");
        assert_eq!(lines[1].text, "c");
    }

    #[test]
    fn explicit_space_glyph_is_honoured() {
        // A space encoded in the content stream has no outline; it is still
        // ground truth about a word boundary (defect L2).
        let mut sp = g(" ", 10.0, 0.0, 0.0, 0.0);
        sp.bbox = None;
        let v = vec![g("a", 0.0, 0.0, 5.0, 0.0), sp, g("b", 12.0, 0.0, 5.0, 0.0)];
        let lines = assemble(&v);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "a b");
    }

    #[test]
    fn line_text_is_nfkc_normalized() {
        let v = vec![
            g("pro", 0.0, 0.0, 15.0, 0.0),
            g("\u{FB01}", 15.0, 0.0, 5.0, 0.0),
            g("le", 20.0, 0.0, 10.0, 0.0),
        ];
        let lines = assemble(&v);
        assert_eq!(lines[0].text, "profile");
        assert!(!lines[0]
            .text
            .chars()
            .any(|c| ('\u{FB00}'..='\u{FB06}').contains(&c)));
    }

    #[test]
    fn glyph_indices_survive_into_the_line() {
        let v = vec![g("a", 0.0, 0.0, 5.0, 0.0), g("b", 5.0, 0.0, 5.0, 0.0)];
        let lines = assemble(&v);
        assert_eq!(
            lines[0].glyphs,
            vec![0, 1],
            "offset->bbox chain must stay intact"
        );
    }
}
