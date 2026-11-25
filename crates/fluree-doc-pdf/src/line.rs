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
use fluree_doc_model::marks;

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

/// A script that sets no spaces between its words and so wraps a line
/// between any two of its characters: Chinese and Japanese, with their
/// punctuation and full-width forms. Not Korean, which spaces its words and
/// wraps at the spaces.
pub(crate) fn wraps_anywhere(c: char) -> bool {
    is_spaceless_script(c) && !('\u{AC00}'..='\u{D7AF}').contains(&c)
}

/// Baseline separation above which two glyphs are on different lines, as a
/// fraction of font size. Anchoring on the baseline rather than the bbox top
/// is essential: cap-height, x-height and descender glyphs on one visual line
/// have different tops but share a baseline.
const BASELINE_TOLERANCE: f64 = 0.3;

/// Largest size, as a fraction of its neighbour's, at which a glyph can be a
/// superscript or subscript rather than text of the same run.
const SCRIPT_SIZE_RATIO: f64 = 0.85;

/// Smallest size, as a fraction of its neighbour's, at which a glyph can be
/// its script. Scripts are set at half to four fifths of the base; a glyph
/// a third the size is another run — a body line beside a 33pt display word,
/// whose baseline sits well within the display word's shift tolerance.
const SCRIPT_MIN_RATIO: f64 = 0.45;

/// Smallest size, as a fraction of its word's, at which a registered,
/// trademark or copyright sign is still that word's. A sign annotates the
/// word before it and nothing else, and is set as small as suits the word:
/// a 28pt cover title's `®` at 8.4pt. Below [`SCRIPT_MIN_RATIO`] it was a
/// line of its own, read as a list marker, and given to the text beside
/// the title.
const SIGN_MIN_RATIO: f64 = 0.15;

/// A registered, trademark, service-mark, copyright or sound-recording
/// sign, alone.
pub(crate) fn is_sign(text: &str) -> bool {
    matches!(
        text.trim(),
        "\u{00AE}" | "\u{2122}" | "\u{2120}" | "\u{00A9}" | "\u{2117}"
    )
}

/// Furthest a script glyph's baseline may sit from its neighbour's, in the
/// neighbour's font-size units. Superscripts are raised a third to a half of
/// the size; subscripts are lowered about a quarter.
const SCRIPT_SHIFT: f64 = 0.7;

/// Fraction of a script glyph's ink that must fall inside the neighbour's
/// line box for the two to share a line.
const SCRIPT_OVERLAP: f64 = 0.4;

/// Furthest a script may sit from the base glyph it annotates along the
/// line, in the larger size: a superscript follows its base directly, and a
/// footnote's number precedes its text by no more than a word space.
const SCRIPT_REACH: f64 = 0.5;

/// How far a script may tuck back under the preceding glyph's overhang.
const SCRIPT_OVERHANG: f64 = 0.3;

/// Longest gap, in the script's own size, between glyphs of one script run.
const SCRIPT_RUN_GAP: f64 = 0.5;

/// Baseline clusters on either side of a script run that may hold its base.
const SCRIPT_WINDOW: usize = 4;

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
    /// Set in bold throughout: most of its glyphs bold, and its first and
    /// last words with letters.
    pub bold: bool,
    /// Its first word with a letter is bold: a bold line, or one opening on
    /// a run-in label (`PUMP: With stainless steel pump…`).
    pub opens_bold: bool,
    /// The first inked glyph is a superscript or subscript: a footnote's
    /// number, an affiliation mark. Set from the glyph sizes at assembly,
    /// where the fact is still visible — the text reads `95 Ibid.` either
    /// way.
    pub opens_with_script: bool,
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
    let r = crate::column::ruler(rules, page, (min_x, max_x), &boxed)?;

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
    lines_in_columns(
        glyphs,
        &crate::column::detect(glyphs),
        &walls(rules, glyphs),
    )
}

/// The page's vertical rules that stand between two stacks of text, by
/// their boxes: what may part a row at a word space (see [`part_at_walls`]).
/// Each crosses no glyph along its length, and has two rows of text or more
/// beside it at each side within its height: a box's text beside the next
/// box's. A single row's cell border stands beside one row each way, and
/// its cells read as one line keep a code with what it names (`B12 Annual
/// Report`). A chart's gridline drawn behind its labels
/// crosses them.
fn walls(rules: &[crate::rule::Rule], glyphs: &[Glyph]) -> Vec<BBox> {
    // Each inked glyph's box, baseline and size: rows are told apart by
    // baselines a line apart, as a full stop's box stands lower than its
    // digit's and a formula's scripts stand off its baseline.
    let inked: Vec<(BBox, f64, f64)> = glyphs
        .iter()
        .filter(|g| !g.text.trim().is_empty())
        .filter_map(|g| g.bbox.map(|b| (b, g.origin.1, g.font_size.max(1.0) as f64)))
        .collect();
    let rows = |w: &BBox, left: bool| {
        let x = (w.x0 + w.x1) * 0.5;
        let mut baselines: Vec<(f64, f64)> = inked
            .iter()
            .filter(|(b, _, _)| if left { b.x1 < x } else { b.x0 > x })
            .filter(|(_, base, _)| *base > w.y0 && *base < w.y1)
            .map(|(_, base, fs)| (*base, *fs))
            .collect();
        baselines.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut n = 0;
        let mut last = f64::MIN;
        for (base, fs) in baselines {
            if base - last > fs * WALL_ROW {
                n += 1;
                last = base;
            }
        }
        n
    };
    rules
        .iter()
        .filter(|r| r.orientation == crate::rule::Orientation::Vertical)
        .map(|r| r.bbox)
        .filter(|w| {
            !inked
                .iter()
                .any(|(b, _, _)| b.x0 < w.x1 && b.x1 > w.x0 && b.y0 < w.y1 && b.y1 > w.y0)
        })
        .filter(|w| rows(w, true) >= WALL_STACK && rows(w, false) >= WALL_STACK)
        .collect()
}

/// As [`assemble_columns_with_rules`], given the columns
/// [`crate::column::detect`] already found on these glyphs, for a caller
/// that needs them too.
pub fn assemble_columns_in(
    glyphs: &[Glyph],
    rules: &[crate::rule::Rule],
    page: usize,
    cols: &[crate::column::Column],
) -> Vec<Vec<Line>> {
    if let Some(banded) = ruler_banded(glyphs, rules, page) {
        return banded;
    }
    lines_in_columns(glyphs, cols, &walls(rules, glyphs))
}

fn lines_in_columns(
    glyphs: &[Glyph],
    cols: &[crate::column::Column],
    walls: &[BBox],
) -> Vec<Vec<Line>> {
    if cols.len() <= 1 {
        return vec![assemble_walled(glyphs, walls)];
    }
    let mut per_col: Vec<Vec<Line>> = crate::column::partition(glyphs, cols)
        .iter()
        .map(|part| assemble_walled(part, walls))
        .collect();
    if std::env::var("FDOC_NO_REJOIN").is_err() {
        rejoin_spanning_lines(&mut per_col, cols);
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

    /// Widest ink gap between the two halves, in font-size units, for them to
    /// be one line the cut bisected.
    ///
    /// This is what separates the two cases, and being near the cut is not.
    /// A bisected line's halves are adjacent — the cut fell inside a word or
    /// at a space — while two columns are held apart by the gutter. Measured:
    /// `…Wide Input R|ange, Step-Down…` sits at 0.03 and `Table of|Contents`
    /// at 0.30, against 2.1-2.7 for every two-column row on the pages this
    /// got wrong. A justified space can stretch, so the bound is set well
    /// above a word and still far below the narrowest gutter seen.
    const MAX_JOIN_GAP: f64 = 0.75;

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

    /// Fewest such fragments that show a layout in two columns. Justified
    /// text gives one on every row of a page's columns; a few rows of a
    /// centred address over a form's small columns are lines the cut
    /// bisected, however large a share of those columns they make.
    const MIN_TWO_COLUMN_ROWS: usize = 6;

    for i in 0..per_col.len().saturating_sub(1) {
        let cut = cols[i].x1;
        // Snapshot the geometry of the lines left of the cut so the retain
        // closure below does not borrow `per_col` while it is being mutated.
        // A line crossing two cuts was rejoined at the first into the column
        // it began in, so every column to the left is looked at, not just
        // the next one.
        let left_edges: Vec<(f64, BBox, i32)> = per_col[..=i]
            .iter()
            .flatten()
            .map(|l| (l.bbox.x1, l.bbox, l.rotation_bucket))
            .collect();
        // The gap from a line on the far side of the cut, flush against it,
        // back to a line ending flush on the near side at its height.
        let gap_back = |r: &Line| -> Option<f64> {
            let fs = r.font_size.max(1.0) as f64;
            if (r.bbox.x0 - cut).abs() > fs * FLUSH {
                return None;
            }
            left_edges
                .iter()
                .filter(|(x1, b, rot)| {
                    (x1 - cut).abs() < fs * FLUSH
                        && r.bbox.x0 - x1 <= fs * MAX_JOIN_GAP
                        && one_row(b, r, fs)
                        && *rot == r.rotation_bucket
                })
                .map(|(x1, ..)| r.bbox.x0 - x1)
                .reduce(f64::min)
        };
        // A cut that falls inside a word, where the halves sit closer than a
        // space, split one line: two columns are held apart by more.
        let in_a_word = |r: &Line, gap: f64| gap <= r.font_size.max(1.0) as f64 * SPACE_GAP_RATIO;
        // Count first: if most rows look like spanning fragments, this is a
        // two-column layout and only the words the cut fell inside are one.
        let candidates = per_col[i + 1]
            .iter()
            .filter(|r| gap_back(r).is_some())
            .count();
        let limit = (per_col[i + 1].len().max(per_col[i].len()) as f64 * MAX_SPANNING_FRACTION)
            .ceil() as usize;
        let only_words = candidates > limit && candidates >= MIN_TWO_COLUMN_ROWS;

        let mut moved: Vec<Line> = Vec::new();
        per_col[i + 1].retain(|r| match gap_back(r) {
            Some(gap) if !only_words || in_a_word(r, gap) => {
                moved.push(r.clone());
                false
            }
            _ => true,
        });
        for m in moved {
            if let Some(l) = per_col[..=i].iter_mut().rev().flatten().find(|l| {
                let fs = l.font_size.max(1.0) as f64;
                (l.bbox.x1 - cut).abs() < fs * FLUSH && one_row(&l.bbox, &m, fs)
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

/// Do a line, `a`, and the piece of it past a cut, `b`, stand on one row:
/// their tops level, or `b` the stop that closes `a`'s sentence, standing
/// on its baseline? A line's last glyph alone, its full stop, is a dot on
/// the baseline, its top the letters' height below theirs.
fn one_row(a: &BBox, b: &Line, fs: f64) -> bool {
    let closing = |c: char| ".,;:!?)]\u{2019}\u{201D}".contains(c);
    let stop = !b.text.trim().is_empty() && b.text.trim().chars().all(closing);
    (a.y0 - b.bbox.y0).abs() < fs * 0.5
        || (stop && b.bbox.height() < fs * 0.5 && (a.y1 - b.bbox.y1).abs() <= fs * 0.35)
}

/// Assemble lines from a single column's glyphs.
///
/// `glyphs` must already have been de-duplicated ([`crate::dedup`]); faux-bold
/// overprint would otherwise double every character in the output.
pub fn assemble(glyphs: &[Glyph]) -> Vec<Line> {
    assemble_walled(glyphs, &[])
}

/// As [`assemble`], parting a row where a rule drawn up between two of its
/// words stands: see [`part_at_walls`].
fn assemble_walled(glyphs: &[Glyph], walls: &[BBox]) -> Vec<Line> {
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
        // y and lines stack along x — and which way along y depends on the
        // bucket, see `Advance`.
        let adv = Advance::of(bucket);
        let vertical = adv.vertical();
        idxs.sort_by(|&a, &b| {
            // Origin, not bbox: outline-less space glyphs have no box but do
            // have a pen position, and they must stay in sequence.
            let (oa, ob) = (glyphs[a].origin, glyphs[b].origin);
            if vertical {
                cmp(oa.0, ob.0).then(cmp(adv.at(oa), adv.at(ob)))
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
        // cannot drift a cluster across genuinely distinct rows. Scripts sit
        // off their baseline by design and form clusters of their own; they
        // are moved into the cluster of the text they annotate afterwards.
        let mut clusters: Vec<Vec<usize>> = Vec::new();
        for &g in &idxs {
            match clusters.last_mut() {
                Some(c) if shares_baseline(&glyphs[c[0]], &glyphs[g], vertical) => c.push(g),
                _ => clusters.push(vec![g]),
            }
        }
        let clusters = loose_signs(clusters, glyphs, adv);
        let (mut clusters, caps) = drop_caps(clusters, glyphs, adv);
        attach_scripts(&mut clusters, glyphs, vertical, adv);
        for c in &mut clusters {
            c.sort_by(|&a, &b| {
                let (oa, ob) = (glyphs[a].origin, glyphs[b].origin);
                cmp(adv.at(oa), adv.at(ob))
            });
        }
        // Each glyph's cluster, and each cluster's reach along the line.
        let mut cluster_of: rustc_hash::FxHashMap<usize, usize> = Default::default();
        let mut spans: Vec<(f64, f64)> = Vec::new();
        for (ci, c) in clusters.iter().enumerate() {
            let mut span = (f64::MAX, f64::MIN);
            for &j in c {
                cluster_of.insert(j, ci);
                if let Some(b) = glyphs[j].bbox {
                    span = (span.0.min(adv.start(&b)), span.1.max(adv.end(&b)));
                }
            }
            spans.push(span);
        }
        let idxs: Vec<usize> = clusters.into_iter().flatten().collect();

        let mut cur: Vec<usize> = Vec::new();
        for i in idxs {
            if cur.is_empty() {
                cur.push(i);
                continue;
            }
            let prev = *cur.last().unwrap();
            // A space the producer set at a raised sign's size and baseline
            // says nothing about where the line goes on: the line goes on
            // from the sign.
            let inked = cur
                .iter()
                .rev()
                .copied()
                .find(|&j| !glyphs[j].text.trim().is_empty())
                .filter(|&j| glyphs[prev].text.trim().is_empty() && is_sign(&glyphs[j].text))
                .unwrap_or(prev);
            // Past a raised sign, the line goes on from the word it marks: a
            // display title's `®`, raised to the cap height of 32pt type, sits
            // far above the baseline the title's next word and the space
            // before it are set on (`PROCAT®` `PROFESSIONAL`).
            let marked = is_sign(&glyphs[inked].text)
                .then(|| {
                    cur.iter()
                        .rev()
                        .copied()
                        .find(|&j| !glyphs[j].text.trim().is_empty() && !is_sign(&glyphs[j].text))
                })
                .flatten()
                .unwrap_or(inked);
            // Between display type and body text the gutter is measured in
            // the body text's size, as the baseline is ([`shares_baseline`]):
            // a margin title's last letter stood 40pt from the column beside
            // it, under 1.5 of its own ems and nearly four of the column's
            // (`Regulatory the three arms of the State`). Measured from the
            // last glyph that put ink down, past an explicit space, and past
            // a sign raised on the word: a title's `®` is the word's, and
            // the title goes on after it at the title's size.
            let display_gap = cur
                .iter()
                .rev()
                .copied()
                .find(|&j| glyphs[j].bbox.is_some() && !is_sign(&glyphs[j].text))
                .is_some_and(|k| {
                    let (a, b) = (&glyphs[k], &glyphs[i]);
                    let big = a.font_size.max(b.font_size) as f64;
                    let small = a.font_size.min(b.font_size).max(1.0) as f64;
                    let gap = match (a.bbox, b.bbox) {
                        (Some(ba), Some(bb)) => adv.start(&bb) - adv.end(&ba),
                        _ => return false,
                    };
                    big >= small * DISPLAY_RATIO && gap > small * BLOCK_GAP_RATIO
                });
            // A run read after the line it is set behind: a cluster of glyphs
            // on a baseline near this one that lies wholly before where the
            // line began, as a paragraph's line lies left of a callout set
            // beside it. Read on into it, the row came out backwards:
            // `GreenCool Technology` set right of a body line, 0.3 of its
            // size higher, read `GreenCool TechnologyThe refrigerated…`.
            // A cluster that reaches into the line, a column's text beside
            // another's, is left to the gutters to part.
            let begun = cur
                .iter()
                .filter_map(|&j| glyphs[j].bbox.map(|b| adv.start(&b)))
                .fold(f64::MAX, f64::min);
            let back = cluster_of.get(&i) != cluster_of.get(&prev)
                && cluster_of.get(&i).is_some_and(|&c| {
                    spans[c].1 <= begun + glyphs[i].font_size.max(1.0) as f64 * 0.2
                });
            // A drop cap heads the line it was put in, its baseline lines
            // below.
            if !back
                && (caps.contains(&prev)
                    || joins_line(&glyphs[prev], &glyphs[i], vertical, adv)
                    || joins_line(&glyphs[inked], &glyphs[i], vertical, adv)
                    || joins_line(&glyphs[marked], &glyphs[i], vertical, adv))
                && !block_gap(&glyphs[prev], &glyphs[i], adv)
                && !display_gap
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
    for (idxs, bucket, _) in &groups {
        if let Some(l) = build_line(glyphs, idxs, *bucket, ratio) {
            lines.push(l);
        }
    }

    let lines = part_at_gutters(lines, glyphs, ratio);
    let lines = part_at_walls(lines, glyphs, walls, ratio);
    let mut lines = label_display(lines, glyphs, ratio);

    // Restore document order across buckets. This is a provisional order —
    // real reading order (XY-Cut++) is a later pass over blocks, not lines.
    lines.sort_by(|a, b| cmp(a.bbox.y0, b.bbox.y0).then(cmp(a.bbox.x0, b.bbox.x0)));

    // Same jitter guard as the glyph sort above, at line level: two fragments
    // of one visual row (split by a block gap) carry tops that differ by the
    // height of an ascender or by plain rounding, and strict (y, x) then puts
    // the right-hand fragment first. Within a row, left must come first.
    // Tops alone mislead for one kind of line: nothing but underscores, a
    // blank to fill in. Its top is below its own baseline, a few points
    // above the top of the line printed under it, and the two do not
    // overlap at all; a fragment of the blank's own row would.
    let blank = |l: &Line| !l.text.trim().is_empty() && l.text.trim().chars().all(|c| c == '_');
    let mut start = 0;
    for i in 1..=lines.len() {
        if i < lines.len() && {
            let (a, b) = (&lines[start], &lines[i]);
            let tol = a.font_size.max(b.font_size).max(1.0) as f64 * BASELINE_TOLERANCE;
            (b.bbox.y0 - a.bbox.y0).abs() < tol
                && (!(blank(a) || blank(b)) || a.bbox.y1.min(b.bbox.y1) > a.bbox.y0.max(b.bbox.y0))
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

/// The direction a rotation bucket's text advances in.
///
/// Rotated text runs along y, and which way along y is the bucket: at 90°
/// (an axis title read bottom to top) the pen climbs the page, at 270° (a
/// `/Rotate 90` page drawn upright, a spine label) it descends. Ordering
/// both by descending y read every descending run backwards — `Cascadia`
/// came out `aidacsaC` — and measuring both as if descending put every gap
/// on an ascending run below zero, so its words ran together. The advance
/// coordinate `at` increases in reading direction whichever way that is,
/// and every sort and gap in assembly is expressed through it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Advance {
    Right,
    Up,
    Down,
}

impl Advance {
    fn of(bucket: i32) -> Self {
        match bucket.rem_euclid(360) {
            90 => Advance::Up,
            270 => Advance::Down,
            _ => Advance::Right,
        }
    }

    fn vertical(self) -> bool {
        self != Advance::Right
    }

    /// A point's position along the advance axis, increasing as text reads.
    fn at(self, (x, y): (f64, f64)) -> f64 {
        match self {
            Advance::Right => x,
            Advance::Down => y,
            Advance::Up => -y,
        }
    }

    /// Where a glyph's ink begins along the advance axis.
    fn start(self, b: &BBox) -> f64 {
        match self {
            Advance::Right => b.x0,
            Advance::Down => b.y0,
            Advance::Up => -b.y1,
        }
    }

    /// Where a glyph's ink ends along the advance axis.
    fn end(self, b: &BBox) -> f64 {
        match self {
            Advance::Right => b.x1,
            Advance::Down => b.y1,
            Advance::Up => -b.y0,
        }
    }
}

/// Two glyphs sit on one baseline, within jitter.
///
/// Jitter is measured in the larger size, except between display type and
/// the text beside it: a glyph at least [`DISPLAY_RATIO`] times the other's
/// size is measured in the smaller one. Text set at two sizes on one line
/// shares its baseline to the point, and a script is attached by its own
/// rule ([`attach_scripts`]), so the larger size's tolerance only ever
/// took in lines of the smaller text. A data sheet sets a 28pt cover title
/// beside a 10pt description, and the description's middle line, 5pt from
/// the title's baseline, was read into the title's line: `PROCAT suelto
/// que no es adherible…`, the paragraph cut around it.
fn shares_baseline(a: &Glyph, b: &Glyph, vertical: bool) -> bool {
    // Compare baselines, not boxes.
    let delta = if vertical {
        a.origin.0 - b.origin.0
    } else {
        a.origin.1 - b.origin.1
    };
    let (big, small) = (
        a.font_size.max(b.font_size).max(1.0) as f64,
        a.font_size.min(b.font_size).max(1.0) as f64,
    );
    let scale = if big >= small * DISPLAY_RATIO {
        small
    } else {
        big
    };
    delta.abs() < scale * BASELINE_TOLERANCE
}

/// Join a label set small right against display type to the display
/// type's line: a form's `Form`, at 6pt and a little raised, before its 18pt
/// number `7004`.
///
/// Their baselines are measured in the label's size, as display type's
/// are against any text beside it ([`shares_baseline`]), and the label sat
/// off the number's by more than that. A label ends within an em of its own
/// size before the display line begins, inside its height, and is the only
/// line of small text that does: a logo with two lines of its name set
/// beside its last letter is left as it is.
fn label_display(lines: Vec<Line>, glyphs: &[Glyph], ratio: f64) -> Vec<Line> {
    let n = lines.len();
    let against = |a: &Line, b: &Line| {
        let small = (a.font_size.max(1.0)) as f64;
        a.rotation_bucket == 0
            && b.rotation_bucket == 0
            && b.font_size as f64 >= small * DISPLAY_RATIO
            && a.bbox.x1 <= b.bbox.x0 + small * 0.5
            && b.bbox.x0 - a.bbox.x1 <= small
            && a.bbox.y0 >= b.bbox.y0 - small * 0.5
            && a.bbox.y1 <= b.bbox.y1 + small * 0.5
            && a.text.split_whitespace().count() <= 3
    };
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for j in 0..n {
        let labels: Vec<usize> = (0..n)
            .filter(|&i| i != j && against(&lines[i], &lines[j]))
            .collect();
        if let [i] = labels[..] {
            pairs.push((i, j));
        }
    }
    if pairs.is_empty() {
        return lines;
    }
    let mut taken = vec![false; n];
    let mut joined: Vec<Option<Line>> = vec![None; n];
    for (i, j) in pairs {
        if taken[i] || taken[j] {
            continue;
        }
        let mut idxs: Vec<usize> = lines[i]
            .glyphs
            .iter()
            .chain(&lines[j].glyphs)
            .copied()
            .collect();
        idxs.sort_by(|&a, &b| cmp(glyphs[a].origin.0, glyphs[b].origin.0));
        if let Some(l) = build_line(glyphs, &idxs, 0, ratio) {
            taken[i] = true;
            taken[j] = true;
            joined[j] = Some(l);
        }
    }
    lines
        .into_iter()
        .enumerate()
        .filter_map(|(k, l)| match joined[k].take() {
            Some(m) => Some(m),
            None if taken[k] => None,
            None => Some(l),
        })
        .collect()
}

/// Size ratio from which the larger of two glyphs is display type beside
/// the smaller, and their baselines are compared in the smaller size. A
/// cover or margin title is set at three times its text or so; a form's
/// rule of 5pt underscores beside its 10pt labels is no display type.
const DISPLAY_RATIO: f64 = 2.5;

/// Consecutive glyphs belong to one line: on one baseline, or a script
/// beside the text it annotates.
fn joins_line(a: &Glyph, b: &Glyph, vertical: bool, adv: Advance) -> bool {
    shares_baseline(a, b, vertical)
        || (script_attached(a, b, vertical) && script_adjacent(a, b, adv))
}

/// Gap along the advance axis from the end of `a` to the start of `b`, in
/// pen space where a glyph has no ink.
fn forward_gap(a: &Glyph, b: &Glyph, adv: Advance) -> f64 {
    let end_a = match a.bbox {
        Some(ba) => adv.end(&ba),
        None => adv.at(a.origin) + a.advance.unwrap_or(0.0),
    };
    let start_b = match b.bbox {
        Some(bb) => adv.start(&bb),
        None => adv.at(b.origin),
    };
    start_b - end_a
}

/// A script sits beside its base — within half a size along the line, and
/// never behind it. Size and shift alone cannot tell a script from a small
/// line set near a large one: a 7pt line four and a half points above an
/// 11pt title is, by those measures, the title's superscript, and reading
/// it as one interleaved the two rows and tore every word of the small line
/// apart. A backwards pen jump is the next row beginning.
fn script_adjacent(a: &Glyph, b: &Glyph, adv: Advance) -> bool {
    let size = a.font_size.max(b.font_size).max(1.0) as f64;
    let gap = forward_gap(a, b, adv);
    gap >= -size * SCRIPT_OVERHANG && gap <= size * SCRIPT_REACH
}

/// Take out of its cluster each sign ([`is_sign`]) that no glyph of the
/// cluster stands beside, into a cluster of its own after it, so that
/// [`attach_scripts`] can give it to the word it follows.
///
/// A raised sign can sit on the baseline of a line it has nothing to do
/// with: a cover title's `®`, raised against the 28pt word, lands within a
/// fraction of a point of the first line of the 10pt text beside the title,
/// and was read as that line's first glyph and then as its list marker.
fn loose_signs(clusters: Vec<Vec<usize>>, glyphs: &[Glyph], adv: Advance) -> Vec<Vec<usize>> {
    let mut out = Vec::with_capacity(clusters.len());
    for c in clusters {
        // A space beside the sign draws nothing, and stands beside it
        // wherever the producer set it.
        let loose = |&g: &usize| {
            is_sign(&glyphs[g].text)
                && !c.iter().any(|&o| {
                    o != g
                        && !glyphs[o].text.trim().is_empty()
                        && (script_adjacent(&glyphs[o], &glyphs[g], adv)
                            || script_adjacent(&glyphs[g], &glyphs[o], adv))
                })
        };
        let (signs, rest): (Vec<usize>, Vec<usize>) = c.iter().copied().partition(loose);
        if signs.is_empty() || rest.is_empty() {
            out.push(c);
            continue;
        }
        // A sign and the space after it go together.
        let space_after = |s: usize, o: usize| {
            glyphs[o].text.trim().is_empty() && script_adjacent(&glyphs[s], &glyphs[o], adv)
        };
        let (taken, rest): (Vec<usize>, Vec<usize>) = rest
            .into_iter()
            .partition(|&o| signs.iter().any(|&s| space_after(s, o)));
        if !rest.is_empty() {
            out.push(rest);
        }
        for s in signs {
            let mut sign = vec![s];
            sign.extend(taken.iter().copied().filter(|&o| space_after(s, o)));
            out.push(sign);
        }
    }
    out
}

/// Smallest size, against the text beside it, of a drop cap.
const DROP_CAP_RATIO: f64 = 2.5;

/// Put each drop cap at the head of the first line it stands beside.
///
/// A drop cap is a paragraph's first letter set large, its top at the top
/// of the paragraph's first line and its foot on the baseline of the second,
/// third or fourth. Its baseline is the last spanned line's, so it was read
/// into that line, or by the large glyph's own baseline tolerance into the
/// one above it: `T of those stories`, `ndia suffers from` with the `I` two
/// lines down. Reading order then broke too, as the line carrying the cap
/// took its top and sorted first.
///
/// A single letter at [`DROP_CAP_RATIO`] times the size of the text beside
/// it or more, with two or more lines starting just right of it whose
/// baselines fall between its top and its foot, is moved into the cluster
/// of the topmost. Returns the clusters and the caps moved.
fn drop_caps(
    mut clusters: Vec<Vec<usize>>,
    glyphs: &[Glyph],
    adv: Advance,
) -> (Vec<Vec<usize>>, std::collections::HashSet<usize>) {
    let mut caps = std::collections::HashSet::new();
    if adv != Advance::Right {
        return (clusters, caps);
    }
    // Each line beside a cap opens with a glyph the cap is DROP_CAP_RATIO
    // times the size of, and a cap needs two such lines. Counting the glyphs
    // that small, from the page's sizes in order, turns a letter at the
    // text's own size away before the scans below, each over the whole page:
    // without it every letter on the page was scanned against every glyph.
    let mut sizes: Vec<f64> = glyphs
        .iter()
        .filter(|o| o.bbox.is_some())
        .map(|o| o.font_size.max(1.0) as f64)
        .collect();
    sizes.sort_by(|a, b| cmp(*a, *b));
    let mut moves: Vec<(usize, usize, usize)> = Vec::new();
    for (ci, c) in clusters.iter().enumerate() {
        for &g in c {
            let cap = &glyphs[g];
            let Some(cb) = cap.bbox else { continue };
            let mut chars = cap.text.chars();
            if !(chars.next().is_some_and(char::is_alphabetic) && chars.next().is_none()) {
                continue;
            }
            let size = cap.font_size as f64;
            if sizes.partition_point(|&fs| size >= fs * DROP_CAP_RATIO) < 2 {
                continue;
            }
            // A drop cap opens its word: no letter of its size stands
            // right before it, as one does before a logo's last letter
            // (`JNER` beside a journal's name).
            let opens = !glyphs.iter().any(|o| {
                o.font_size as f64 >= size * 0.5
                    && o.bbox.is_some_and(|ob| {
                        ob.x1 <= cb.x0 + size * 0.1
                            && ob.x1 >= cb.x0 - size * 0.3
                            && ob.y1 > cb.y0
                            && ob.y0 < cb.y1
                    })
            });
            if !opens {
                continue;
            }
            // The clusters it stands beside: each one's first glyph past
            // the cap, and that glyph's baseline.
            let beside: Vec<(usize, f64)> = clusters
                .iter()
                .enumerate()
                .filter_map(|(cj, d)| {
                    // Right of the cap: a line can share its row with
                    // text on the cap's other side, a title in the margin.
                    let first = d
                        .iter()
                        .filter(|&&i| i != g)
                        .map(|&i| &glyphs[i])
                        .filter(|o| {
                            o.bbox
                                .is_some_and(|b| b.x0 >= cb.x1 - o.font_size.max(1.0) as f64 * 0.5)
                        })
                        .min_by(|a, b| cmp(a.bbox.unwrap().x0, b.bbox.unwrap().x0))?;
                    let fs = first.font_size.max(1.0) as f64;
                    let x0 = first.bbox.unwrap().x0;
                    let y = first.origin.1;
                    (size >= fs * DROP_CAP_RATIO
                        && x0 >= cb.x1 - fs * 0.5
                        && x0 <= cb.x1 + fs * 2.0
                        && y > cb.y0
                        && y <= cap.origin.1 + fs * 0.5)
                        .then_some((cj, y))
                })
                .collect();
            if beside.len() < 2 {
                continue;
            }
            let (top, _) = beside.iter().copied().min_by(|a, b| cmp(a.1, b.1)).unwrap();
            if top != ci {
                moves.push((ci, g, top));
            }
        }
    }
    for (from, g, to) in moves {
        clusters[from].retain(|&i| i != g);
        clusters[to].push(g);
        caps.insert(g);
    }
    (clusters, caps)
}

/// Move script runs into the cluster of the text they annotate.
///
/// A cluster holds one baseline, so a line's superscripts form a cluster
/// above it and its subscripts one below. Each such cluster is split into
/// runs along the line — `9`, `95`, `HH` — and a run joins a neighbouring
/// cluster of larger text when it is script-sized against that text, shifted
/// and overlapping as a script is, and adjacent to one of its glyphs. A small
/// line that merely sits near a large one touches none of its glyphs and
/// stays where it is.
fn attach_scripts(clusters: &mut [Vec<usize>], glyphs: &[Glyph], vertical: bool, adv: Advance) {
    // A space draws nothing: a raised `®` set with a space after it is
    // still a sign alone. Only glyphs that put ink down decide, where a
    // cluster has any.
    let inked = |c: &[usize]| -> Vec<usize> {
        let ink: Vec<usize> = c
            .iter()
            .copied()
            .filter(|&i| !glyphs[i].text.trim().is_empty())
            .collect();
        if ink.is_empty() {
            c.to_vec()
        } else {
            ink
        }
    };
    let size_of = |c: &[usize]| {
        c.iter()
            .map(|&i| glyphs[i].font_size)
            .fold(0.0f32, f32::max)
    };
    for s in 0..clusters.len() {
        if clusters[s].is_empty() {
            continue;
        }
        let signs_alone = inked(&clusters[s])
            .iter()
            .all(|&i| is_sign(&glyphs[i].text));
        // A sign is measured in its ink: a `™` and the space after it, set
        // at the title's size, is a sign of the title's, not a run as large
        // as it.
        let small = if signs_alone {
            size_of(&inked(&clusters[s]))
        } else {
            size_of(&clusters[s])
        } as f64;
        let min_ratio = if signs_alone {
            SIGN_MIN_RATIO
        } else {
            SCRIPT_MIN_RATIO
        };
        let lo = s.saturating_sub(SCRIPT_WINDOW);
        let hi = (s + SCRIPT_WINDOW).min(clusters.len() - 1);
        // A cluster of spaces alone is no text to annotate.
        let bases: Vec<usize> = (lo..=hi)
            .filter(|&b| b != s && !clusters[b].is_empty())
            .filter(|&b| {
                clusters[b]
                    .iter()
                    .any(|&i| !glyphs[i].text.trim().is_empty())
            })
            .filter(|&b| {
                let big = size_of(&clusters[b]) as f64;
                (min_ratio..=SCRIPT_SIZE_RATIO).contains(&(small / big.max(1.0)))
            })
            .collect();
        if bases.is_empty() {
            continue;
        }
        let mut members = std::mem::take(&mut clusters[s]);
        members.sort_by(|&a, &b| cmp(adv.at(glyphs[a].origin), adv.at(glyphs[b].origin)));
        let mut runs: Vec<Vec<usize>> = Vec::new();
        for g in members {
            match runs.last_mut() {
                Some(r)
                    if forward_gap(&glyphs[*r.last().unwrap()], &glyphs[g], adv)
                        <= small * SCRIPT_RUN_GAP =>
                {
                    r.push(g)
                }
                _ => runs.push(vec![g]),
            }
        }
        let mut kept: Vec<usize> = Vec::new();
        for run in runs {
            let first = &glyphs[run[0]];
            let last = &glyphs[*run.last().unwrap()];
            // A raised sign is a word's: a base whose ink it stands beside
            // comes before one it reaches only through a space, which the
            // producer may have set on another line's baseline.
            let signed = run
                .iter()
                .all(|&i| is_sign(&glyphs[i].text) || glyphs[i].text.trim().is_empty());
            let beside = |b: usize, inked_only: bool| {
                clusters[b].iter().any(|&i| {
                    let g = &glyphs[i];
                    if inked_only && g.text.trim().is_empty() {
                        return false;
                    }
                    (script_attached(g, first, vertical) && script_adjacent(g, first, adv))
                        || (script_attached(last, g, vertical) && script_adjacent(last, g, adv))
                })
            };
            let base = bases
                .iter()
                .copied()
                .find(|&b| signed && beside(b, true))
                .or_else(|| bases.iter().copied().find(|&b| beside(b, false)));
            match base {
                Some(b) => {
                    // The space set right after a raised sign is the sign's,
                    // on whatever baseline the producer gave it: it parts
                    // the word from the next (`Comfortboard® 80`).
                    if signed {
                        for o in lo..=hi {
                            if o == b || o == s {
                                continue;
                            }
                            if let Some(k) = clusters[o].iter().position(|&i| {
                                let g = &glyphs[i];
                                let across = if vertical {
                                    (g.origin.0 - last.origin.0).abs()
                                } else {
                                    (g.origin.1 - last.origin.1).abs()
                                };
                                g.text.trim().is_empty()
                                    && across < g.font_size.min(last.font_size * 2.0) as f64 * 0.7
                                    && script_adjacent(last, g, adv)
                            }) {
                                let space = clusters[o].remove(k);
                                clusters[b].push(space);
                                break;
                            }
                        }
                    }
                    clusters[b].extend(run)
                }
                None => kept.extend(run),
            }
        }
        clusters[s] = kept;
    }
}

/// A superscript or subscript belongs to the line it annotates.
///
/// `cm³/s` sets its exponent smaller and half a size above the baseline —
/// past the baseline tolerance — so the `3` fell out of its line and became a
/// one-character paragraph on every line that used the unit, while the line
/// it came from read `cm/s`. A script glyph is smaller than its neighbour, its
/// baseline is shifted by less than the neighbour's size, and its ink sits
/// inside the neighbour's line box. A smaller line set *beneath* fails the
/// last test: its ink starts below the neighbour's descenders. Whether it
/// is *beside* the neighbour is [`script_adjacent`]'s question.
fn script_attached(a: &Glyph, b: &Glyph, vertical: bool) -> bool {
    let (big, small) = if a.font_size >= b.font_size {
        (a, b)
    } else {
        (b, a)
    };
    let size = big.font_size.max(1.0) as f64;
    let ratio = small.font_size as f64 / size;
    let min_ratio = if is_sign(&small.text) {
        SIGN_MIN_RATIO
    } else {
        SCRIPT_MIN_RATIO
    };
    if !(min_ratio..=SCRIPT_SIZE_RATIO).contains(&ratio) {
        return false;
    }
    let (baseline, shift, ink) = if vertical {
        (
            big.origin.0,
            small.origin.0 - big.origin.0,
            small.bbox.map(|b| (b.x0, b.x1)),
        )
    } else {
        (
            big.origin.1,
            small.origin.1 - big.origin.1,
            small.bbox.map(|b| (b.y0, b.y1)),
        )
    };
    if shift.abs() > size * SCRIPT_SHIFT {
        return false;
    }
    // Outline-less — an explicit space set in the script's size: the shift
    // alone decides, and the neighbouring glyphs settle the line.
    let Some((lo, hi)) = ink else {
        return true;
    };
    // The line box: a full size above the baseline to a quarter below it,
    // in page space where y grows downward. A vertical run's box is
    // symmetric, because which side is "up" depends on the run's direction.
    let (box_lo, box_hi) = if vertical {
        (baseline - size, baseline + size)
    } else {
        (baseline - size, baseline + size * 0.25)
    };
    let overlap = hi.min(box_hi) - lo.max(box_lo);
    overlap >= (hi - lo).max(f64::EPSILON) * SCRIPT_OVERLAP
}

/// Lines a space glyph kept whole across a gutter, parted there.
///
/// A space written as a glyph has no ink and no box, so the gap after a word
/// ended by one was never measured, and a column's line ran on across the
/// gutter into the next: `fit easily into` and the callout `Wide range of
/// rotors for` beside it, one line. Measured from the last glyph that put
/// ink down, past the space, the gap is a gutter's when it is wider than the
/// block gap and no line just above or below, set to the same measure, puts
/// ink in it. A justified line's stretched space is crossed by its
/// paragraph's words, on lines that start and end where it does; a gutter
/// stands clear down the rows, whatever a title over it does. Where every
/// row of two columns ran together, each looks set to one measure with the
/// next, and their ragged ends reach into each other's gaps; but those rows
/// are cut by the gutter too, their own gap ending where this one's does or
/// spanning its middle, and do not count as running across it. A number or
/// a label set off from its text by a tab keeps it (`3.1 ⇥ Results`, `Note
/// 3. ⇥ Business Segment…`).
fn part_at_gutters(lines: Vec<Line>, glyphs: &[Glyph], ratio: f64) -> Vec<Line> {
    let boxes: Vec<(BBox, f64, bool)> = lines
        .iter()
        .map(|l| (l.bbox, l.font_size.max(1.0) as f64, l.is_horizontal()))
        .collect();
    // Each line's ink, as the spans its glyphs cover.
    let ink: Vec<Vec<(f64, f64)>> = lines
        .iter()
        .map(|l| {
            let mut spans: Vec<(f64, f64)> = l
                .glyphs
                .iter()
                .filter_map(|&j| glyphs[j].bbox.map(|b| (b.x0, b.x1)))
                .collect();
            spans.sort_by(|a, b| a.0.total_cmp(&b.0));
            spans
        })
        .collect();
    // Is line `j` parted by a gap of its own over the gap `x0..x1`: one
    // whose text resumes where the gap's does, or that spans its middle?
    // Then it does not run across that gutter but is cut by it too.
    let parted_over = |j: usize, x0: f64, x1: f64, size: f64| {
        let quarter = (x1 - x0) * 0.25;
        let mut reach = f64::MIN;
        ink[j].iter().any(|&(g0, g1)| {
            let hit = reach > f64::MIN
                && g0 - reach > size * BLOCK_GAP_RATIO
                && ((g0 - x1).abs() <= size || (reach < x1 - quarter && g0 > x0 + quarter));
            reach = reach.max(g1);
            hit
        })
    };
    let crossed = |me: usize, x0: f64, x1: f64, size: f64| {
        let b = boxes[me].0;
        // The paragraph's own lines: set to the line's measure, starting
        // and ending where it does.
        boxes.iter().enumerate().any(|(j, (o, _, horizontal))| {
            j != me
                && *horizontal
                && o.y0 < b.y1 + size * GUTTER_ROWS
                && o.y1 > b.y0 - size * GUTTER_ROWS
                && o.x0 < x0 + 1.0
                && o.x1 > x1 - 1.0
                && (o.x0 - b.x0).abs() <= size * SAME_MEASURE
                && (o.x1 - b.x1).abs() <= size * SAME_MEASURE
                && {
                    // In the gap's middle: a ragged edge reaching a point
                    // into it is the edge, not a word set across it.
                    let quarter = (x1 - x0) * 0.25;
                    ink[j]
                        .iter()
                        .any(|&(g0, g1)| g0 < x1 - quarter && g1 > x0 + quarter)
                }
                // Two columns the line pass ran together row after row:
                // each row's own gap ends where the next column starts, or
                // a line beside it in the other column stands in the gap.
                && !parted_over(j, x0, x1, size)
        })
    };
    let mut out = Vec::with_capacity(lines.len());
    for (me, l) in lines.into_iter().enumerate() {
        if !l.is_horizontal() {
            out.push(l);
            continue;
        }
        let idxs = &l.glyphs;
        let mut cuts: Vec<usize> = Vec::new();
        for k in 1..idxs.len() {
            let (prev, next) = (&glyphs[idxs[k - 1]], &glyphs[idxs[k]]);
            if prev.bbox.is_some() || next.bbox.is_none() {
                continue;
            }
            let from = cuts.last().copied().unwrap_or(0);
            let Some(inked) = idxs[from..k]
                .iter()
                .rev()
                .find(|&&j| glyphs[j].bbox.is_some())
            else {
                continue;
            };
            let (Some(a), Some(b)) = (glyphs[*inked].bbox, next.bbox) else {
                continue;
            };
            let size = glyphs[*inked].font_size.max(next.font_size).max(1.0) as f64;
            if b.x0 - a.x1 > size * BLOCK_GAP_RATIO
                && !lone_label(glyphs, &idxs[from..k])
                && !crossed(me, a.x1, b.x0, size)
            {
                cuts.push(k);
            }
        }
        if cuts.is_empty() {
            out.push(l);
            continue;
        }
        let mut edges = vec![0];
        edges.extend(&cuts);
        edges.push(idxs.len());
        let parts: Vec<Line> = edges
            .windows(2)
            .filter_map(|w| build_line(glyphs, &idxs[w[0]..w[1]], l.rotation_bucket, ratio))
            .collect();
        if parts.len() == cuts.len() + 1 {
            out.extend(parts);
        } else {
            out.push(l);
        }
    }
    out
}

/// Share of a line's height, at its top and at its foot, a rule beside its
/// words may fall short by and still stand between them: a box's side runs
/// the box's height, past the lines in it.
const WALL_REACH: f64 = 0.25;

/// Narrowest gap, in ems, a rule parts a line at: a word space's width. A
/// chart's line drawn between two letters of a label parts nothing.
const WALL_GAP: f64 = 0.25;

/// Fewest rows of text a rule must stand beside on each side to part a
/// line: two, a box's text beside the next box's, not a row's two cells.
const WALL_STACK: usize = 2;

/// Least distance, in the text's size, between two rows' baselines: a
/// line's spacing, short of which a raised or lowered script in a formula
/// stands on its own line's row.
const WALL_ROW: f64 = 0.8;

/// A rule drawn up between two words of a row parts it, however narrow the
/// gap: the side of a box between a letterhead's `… CO., INC.` and the form
/// box's `Project Name:` beside it, on one baseline. The rule is one of the
/// page's [`walls`], runs the line's height, and stands in a word space at
/// least. A rule between two letters of a word, or a short tick under the
/// baseline, parts nothing.
fn part_at_walls(lines: Vec<Line>, glyphs: &[Glyph], walls: &[BBox], ratio: f64) -> Vec<Line> {
    if walls.is_empty() {
        return lines;
    }
    let mut out = Vec::with_capacity(lines.len());
    for l in lines {
        let reach = (l.bbox.y1 - l.bbox.y0) * WALL_REACH;
        let standing: Vec<f64> = walls
            .iter()
            .filter(|w| {
                w.y0 <= l.bbox.y0 + reach
                    && w.y1 >= l.bbox.y1 - reach
                    && w.x0 > l.bbox.x0
                    && w.x1 < l.bbox.x1
            })
            .map(|w| (w.x0 + w.x1) * 0.5)
            .collect();
        if !l.is_horizontal() || standing.is_empty() {
            out.push(l);
            continue;
        }
        let idxs = &l.glyphs;
        let space = l.font_size.max(1.0) as f64 * WALL_GAP;
        let mut cuts: Vec<usize> = Vec::new();
        let mut ink: Option<f64> = None;
        for (k, &j) in idxs.iter().enumerate() {
            let Some(b) = glyphs[j].bbox else { continue };
            if ink.is_some_and(|x1| {
                b.x0 - x1 >= space && standing.iter().any(|&x| x > x1 && x < b.x0)
            }) {
                cuts.push(k);
            }
            ink = Some(ink.map_or(b.x1, |x1| x1.max(b.x1)));
        }
        if cuts.is_empty() {
            out.push(l);
            continue;
        }
        let mut edges = vec![0];
        edges.extend(&cuts);
        edges.push(idxs.len());
        let parts: Vec<Line> = edges
            .windows(2)
            .filter_map(|w| build_line(glyphs, &idxs[w[0]..w[1]], l.rotation_bucket, ratio))
            .collect();
        if parts.len() == cuts.len() + 1 {
            out.extend(parts);
        } else {
            out.push(l);
        }
    }
    out
}

/// How far, in the line's size, another line's ends may stand from its own
/// for the two to be set to one measure.
const SAME_MEASURE: f64 = 1.5;

/// Rows above and below, in the line's size, whose lines show a gap is no
/// gutter by running across it.
const GUTTER_ROWS: f64 = 2.5;

/// Is the line so far a number or a marker alone, `3.1`, `12`, `IV.`,
/// `(a)`, `•`, or one a word names, `Note 3.`, `Item 1A.`, `Section 2.1`,
/// `Article IV`? Set off from its text by a tab, it heads that text.
fn lone_label(glyphs: &[Glyph], cur: &[usize]) -> bool {
    let text: String = cur.iter().map(|&j| glyphs[j].text.as_str()).collect();
    let number = |t: &str| {
        let letters = t.chars().filter(|c| c.is_alphabetic()).count();
        !t.is_empty() && t.chars().count() <= LABEL_MAX_CHARS && letters <= LABEL_MAX_LETTERS
    };
    let words: Vec<&str> = text.split_whitespace().collect();
    match words[..] {
        [t] => number(t),
        [w, t] => {
            w.chars().next().is_some_and(char::is_uppercase)
                && w.chars().all(char::is_alphabetic)
                && w.chars().count() <= LABEL_WORD_CHARS
                && number(t)
                && t.chars().any(|c| c.is_ascii_digit() || c.is_uppercase())
        }
        _ => false,
    }
}

/// Longest the word naming a label is: `Schedule`, `Appendix`.
const LABEL_WORD_CHARS: usize = 10;

/// Longest a number or marker set before a tab is, in characters: `1.2.3.4`.
const LABEL_MAX_CHARS: usize = 8;

/// Most letters such a label holds: a roman numeral or a letter (`iv.`,
/// `A.1`), not a word.
const LABEL_MAX_LETTERS: usize = 3;

/// True when two glyphs on one baseline are separated by enough space to be
/// separate blocks — a column gutter, a table cell boundary, or a header's
/// left and right groups. Splitting here is what stops `www.ti.com` and
/// `SLFS022K` becoming one token (defect L1).
fn block_gap(a: &Glyph, b: &Glyph, adv: Advance) -> bool {
    let (Some(ba), Some(bb)) = (a.bbox, b.bbox) else {
        return false;
    };
    let gap = adv.start(&bb) - adv.end(&ba);
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

/// The run of raised, smaller glyphs that `idxs[k]` begins, as text, where
/// it follows a glyph of the line's own size: a superscript, which may be a
/// note's mark. With it, the first character after the run that is not
/// whitespace. `None` mid-run, and on a vertical line.
fn raised_mark(
    glyphs: &[Glyph],
    idxs: &[usize],
    k: usize,
    adv: Advance,
    degrees: &[usize],
) -> Option<(String, Option<char>)> {
    if adv.vertical() {
        return None;
    }
    let g = &glyphs[idxs[k]];
    let base = idxs[..k]
        .iter()
        .rev()
        .map(|&i| &glyphs[i])
        .find(|p| p.bbox.is_some())?;
    let size = base.font_size.max(1.0) as f64;
    let ratio = g.font_size as f64 / size;
    // Raised: its baseline above the word's, in page space where y grows
    // downward.
    let raised = base.origin.1 - g.origin.1 >= size * MARK_RAISE;
    let scripted_size = (SCRIPT_MIN_RATIO..=SCRIPT_SIZE_RATIO).contains(&ratio) && raised;
    // Or a font's own superior figures, set at the text's size on its
    // baseline: `Lengths` and its `7`, `(48 mm)` and its `6`.
    let on_line = (g.font_size as f64 - size).abs() <= size * 0.1
        && (g.origin.1 - base.origin.1).abs() < size * 0.2;
    // Mid-run when the glyph before is a superior too: `25` is one mark,
    // not a `5` raised over a `2`.
    // A superior stands high, its ink's top as high as the word's: a
    // display font's small figures centred on the line (`1⁰1⁰`) are not.
    let high = match (g.bbox, base.bbox) {
        (Some(m), Some(b)) => m.y0 <= b.y0 + size * 0.1,
        _ => false,
    };
    let superiors = !scripted_size && on_line && high && superior(g) && !superior(base);
    if !scripted_size && !superiors {
        return None;
    }
    // A word that holds a script of its own is a formula's term (`cₚhᵖ`),
    // and what is raised after it is an index or a power. A degree read
    // already is no script: `20°C/min→250°C` sets its second like its first.
    let word = idxs[..k]
        .iter()
        .rev()
        .take_while(|&&i| glyphs[i].bbox.is_some() && !glyphs[i].text.trim().is_empty());
    let scripted = word.filter(|i| !degrees.contains(i)).any(|&i| {
        let p = &glyphs[i];
        (p.font_size as f64) <= size * SCRIPT_SIZE_RATIO
            || (p.origin.1 - base.origin.1).abs() >= size * MARK_RAISE
    });
    if scripted {
        return None;
    }
    let mut mark = String::new();
    let mut end = idxs.len();
    for (j, &i) in idxs.iter().enumerate().skip(k) {
        let m = &glyphs[i];
        let same = m.bbox.is_some()
            && (m.font_size - g.font_size).abs() <= g.font_size * 0.1
            && (m.origin.1 - g.origin.1).abs() < g.font_size.max(1.0) as f64 * 0.2
            && (!superiors || superior(m));
        if !same {
            end = j;
            break;
        }
        mark.push_str(&m.text);
    }
    let next = idxs[end..]
        .iter()
        .flat_map(|&i| glyphs[i].text.chars())
        .find(|c| !c.is_whitespace());
    Some((mark, next))
}

/// Whether `g` is a superior figure: a digit, or the comma between two, a
/// font draws small and high above the baseline it is set on. Its ink stands
/// clear of its own baseline and short of a figure's height. No figure set
/// on the line is drawn so; a quote or a degree sign is no figure.
fn superior(g: &Glyph) -> bool {
    let size = g.font_size.max(1.0) as f64;
    g.text.chars().count() == 1
        && g.text.chars().all(|c| c.is_ascii_digit() || c == ',')
        && g.bbox.is_some_and(|b| {
            g.origin.1 - b.y1 >= size * MARK_RAISE && b.height() <= size * SUPERIOR_HEIGHT
        })
}

/// Tallest a superior figure's ink stands, as a share of its size: a
/// figure set on the line stands about seven tenths.
const SUPERIOR_HEIGHT: f64 = 0.5;

/// The run of raised figures `idxs[k]` begins where it leads a word, after a
/// space or at the line's head: a note's number set before the note, or
/// before the name the note is on (`⁴M17 series centrifuges only`). Its last
/// glyph's index, where the word's first glyph follows it with no gap.
fn leading_mark(glyphs: &[Glyph], idxs: &[usize], k: usize, adv: Advance) -> Option<usize> {
    if adv.vertical() {
        return None;
    }
    let g = &glyphs[idxs[k]];
    let figure = |m: &Glyph| m.bbox.is_some() && m.text.chars().all(|c| c.is_ascii_digit());
    if !figure(g) {
        return None;
    }
    let mut end = k;
    while let Some(&i) = idxs.get(end + 1) {
        let m = &glyphs[i];
        let same = m.bbox.is_some()
            && (m.font_size - g.font_size).abs() <= g.font_size * 0.1
            && (m.origin.1 - g.origin.1).abs() < g.font_size.max(1.0) as f64 * 0.2
            && m.text.chars().all(|c| c.is_ascii_digit() || c == ',');
        if !same {
            break;
        }
        end += 1;
    }
    let last = &glyphs[idxs[end]];
    let word = glyphs.get(*idxs.get(end + 1)?)?;
    let first = word.bbox?;
    // A raised `0` before a temperature's unit is its degree (`75 °F`).
    if !word.text.starts_with(char::is_alphabetic)
        || (end == k && g.text == "0" && matches!(word.text.as_str(), "C" | "F"))
    {
        return None;
    }
    // A word, of three letters and figures or more: a formula's term after
    // a raised number (`¹τ`, `²Qk`, `¹R(x`) is no note's name.
    let name: String = idxs[end + 1..]
        .iter()
        .map(|&i| &glyphs[i])
        .take_while(|m| m.bbox.is_some())
        .flat_map(|m| m.text.chars())
        .take_while(|c| c.is_alphanumeric())
        .collect();
    let latin = |c: char| c.is_ascii_alphanumeric() || ('\u{00C0}'..='\u{024F}').contains(&c);
    if name.chars().count() < 3 || !name.chars().all(latin) {
        return None;
    }
    let size = word.font_size.max(1.0) as f64;
    let ratio = g.font_size as f64 / size;
    let raised = (SCRIPT_MIN_RATIO..=SCRIPT_SIZE_RATIO).contains(&ratio)
        && word.origin.1 - g.origin.1 >= size * MARK_RAISE;
    let superiors = (g.font_size as f64 - size).abs() <= size * 0.1
        && idxs[k..=end].iter().all(|&i| superior(&glyphs[i]))
        && !superior(word)
        && g.bbox.is_some_and(|m| m.y0 <= first.y0 + size * 0.1);
    let pen = last.advance.map_or(
        last.bbox.map_or(adv.at(last.origin), |b| adv.end(&b)),
        |a| adv.at(last.origin) + a,
    );
    let touching = adv.start(&first) - pen <= size * 0.2;
    ((raised || superiors) && touching).then_some(end)
}

/// Whether `idxs[k]`, with nothing inked before it on its line, is a
/// degree set as a raised `o`, `0` or `º` before the `C` or `F` of its unit:
/// a units column's `°C`. With no figure before it to measure against, it
/// is measured against the capital after it, and the capital ends the word.
fn leading_degree(glyphs: &[Glyph], idxs: &[usize], k: usize, adv: Advance) -> bool {
    let g = &glyphs[idxs[k]];
    if adv.vertical()
        || !matches!(g.text.trim(), "o" | "0" | "º")
        || idxs[..k].iter().any(|&i| glyphs[i].bbox.is_some())
    {
        return false;
    }
    let mut after = idxs[k + 1..]
        .iter()
        .map(|&i| &glyphs[i])
        .filter(|p| p.bbox.is_some());
    let Some(unit) = after.next() else {
        return false;
    };
    let size = unit.font_size.max(1.0) as f64;
    let ratio = g.font_size as f64 / size;
    matches!(unit.text.trim(), "C" | "F")
        && (SCRIPT_MIN_RATIO..=SCRIPT_SIZE_RATIO).contains(&ratio)
        && unit.origin.1 - g.origin.1 >= size * MARK_RAISE
        && after.next().is_none_or(|p| {
            !p.text.starts_with(char::is_alphabetic) || {
                // The word ends where the next glyph stands apart.
                let end = unit
                    .advance
                    .map_or(unit.bbox.unwrap().x1, |a| adv.at(unit.origin) + a);
                adv.start(&p.bbox.unwrap()) - end > size * 0.2
            }
        })
}

/// The fraction `idxs[k]` begins, set against the whole number before it:
/// the index of its last glyph.
///
/// A mixed number is typeset with no space between its parts: `22⅜` as `22`,
/// a numerator set small and high, a slash, and a denominator set small.
/// The text layer gives the digits alone, and read as one number `22 3/8`
/// was `223/8`. Small is the ink's height against the whole number's
/// figures, which a fraction's figures share whether a smaller size sets
/// them or the font's own fraction figures do. High is the numerator's foot
/// above the whole number's baseline: old-style figures are as short and
/// stand on it.
fn fraction_at(glyphs: &[Glyph], idxs: &[usize], k: usize, adv: Advance) -> Option<usize> {
    if adv.vertical() {
        return None;
    }
    let figure = |g: &Glyph| !g.text.is_empty() && g.text.chars().all(|c| c.is_ascii_digit());
    let whole = idxs[..k]
        .iter()
        .rev()
        .map(|&i| &glyphs[i])
        .find(|p| p.bbox.is_some() || !p.text.trim().is_empty())?;
    let ink = whole.bbox.filter(|_| figure(whole))?;
    // A fraction the font draws whole, `½`, which its text map spells out.
    let spelled = glyphs[idxs[k]]
        .text
        .split_once(['/', '\u{2044}'])
        .is_some_and(|(n, d)| {
            [n, d]
                .iter()
                .all(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()))
        });
    if spelled {
        return Some(k);
    }
    let small = |g: &Glyph| {
        figure(g)
            && g.bbox
                .is_some_and(|b| b.height() <= ink.height() * FRACTION_FIGURE)
    };
    let run = |from: usize| {
        (from..idxs.len())
            .take_while(|&j| small(&glyphs[idxs[j]]))
            .last()
    };
    let numerator = run(k)?;
    let high = idxs[k..=numerator].iter().all(|&i| {
        glyphs[i]
            .bbox
            .is_some_and(|b| ink.y1 - b.y1 >= ink.height() * FRACTION_RAISE)
    });
    let slash = numerator + 1;
    if !high
        || !idxs
            .get(slash)
            .is_some_and(|&i| matches!(glyphs[i].text.as_str(), "/" | "\u{2044}"))
    {
        return None;
    }
    let denominator = run(slash + 1)?;
    let ends = idxs
        .get(denominator + 1)
        .is_none_or(|&i| !glyphs[i].text.starts_with(|c: char| c.is_ascii_digit()));
    ends.then_some(denominator)
}

/// Tallest a fraction's figure stands, against the whole number's.
const FRACTION_FIGURE: f64 = 0.8;

/// Least height, against the whole number's figures, of a numerator's foot
/// above their baseline.
const FRACTION_RAISE: f64 = 0.25;

/// Signs a power may carry beside its digits.
const POWER_SIGNS: &[char] = &['-', '+', '\u{2212}', '\u{2013}'];

/// A power's character as Unicode sets it raised.
fn superscript(c: char) -> char {
    match c {
        '0' => '\u{2070}',
        '1' => '\u{00B9}',
        '2' => '\u{00B2}',
        '3' => '\u{00B3}',
        '4'..='9' => char::from_u32(0x2070 + (c as u32 - '0' as u32)).unwrap_or(c),
        '+' => '\u{207A}',
        '-' | '\u{2212}' | '\u{2013}' => '\u{207B}',
        _ => c,
    }
}

/// Least rise, as a share of the word's size, of a superscript's baseline.
const MARK_RAISE: f64 = 0.15;

/// Word gaps in letter-spaced text sit this far above the letter gaps.
const TRACKING_WORD_FACTOR: f64 = 1.5;

/// The explicit space at `idxs[k]` is a stray: drawn out of turn, and set
/// on a mark's ink rather than between two marks.
///
/// A space stands where the pen stops after one word and before the next.
/// A browser printing a page draws the space at each place it wrapped a
/// line, and one producer draws that space after the line's last word but
/// at a position inside an earlier word of it: `Produits` read `Pro
/// duits`, `papier` read `papi er`, because the space sorted in between the
/// letters it was set on. Such a space parts nothing, and the word reads
/// whole.
///
/// Both tests are needed. A space drawn in turn, between the marks it sits
/// between on the line, is where the pen stopped, however close the next
/// mark begins: a justified line closes its word gaps up, and a run set in
/// another size starts where the space does. And a space drawn out of turn
/// in a gap is still a gap: some producers draw a line's words and then
/// its spaces. On ink means inside the advance of the mark ahead of it,
/// well short of where that mark's pen stops, or where the mark after it
/// begins. Not behind the mark ahead: a line that runs on into the next row
/// jumps back to its start, and the space there parts the two rows.
fn stray_space(glyphs: &[Glyph], idxs: &[usize], k: usize, adv: Advance) -> bool {
    let space = &glyphs[idxs[k]];
    let at = adv.at(space.origin);
    let drawn = |i: &&usize| glyphs[**i].bbox.is_some();
    let before = idxs[..k].iter().rev().find(drawn).map(|&i| &glyphs[i]);
    let after = idxs[k + 1..].iter().find(drawn).map(|&i| &glyphs[i]);
    let width = |g: &Glyph| match (g.advance, g.bbox) {
        (Some(a), _) => a,
        (None, Some(b)) => adv.end(&b) - adv.at(g.origin),
        (None, None) => 0.0,
    };
    let em = space.font_size.max(1.0) as f64;
    // In turn: drawn between its neighbours on the line, in whichever
    // direction the line was drawn; or drawn just after a mark, where that
    // mark's pen stopped. A chart sets its labels' marks among each other's
    // on one line, and a label's own space is still where its pen stopped.
    let between = match (before, after) {
        (Some(b), Some(a)) => {
            let (lo, hi) = if b.draw_index <= a.draw_index {
                (b.draw_index, a.draw_index)
            } else {
                (a.draw_index, b.draw_index)
            };
            lo < space.draw_index && space.draw_index < hi
        }
        _ => true,
    };
    let follows = idxs.iter().map(|&i| &glyphs[i]).any(|p| {
        p.bbox.is_some()
            && p.draw_index + 1 == space.draw_index
            && (adv.at(p.origin) + width(p) - at).abs() <= SPACE_INSIDE * em
    });
    // In turn, but set back onto the mark ahead's ink, with the mark after
    // it where that mark's pen stopped: the page shows no space, and the
    // space parts nothing. A producer sets one inside a ligature, its pen
    // moved back over it (`Scientiﬁ c`, `certiﬁ ed`).
    let swallowed = match (before, after) {
        (Some(b), Some(a)) => {
            let stop = adv.at(b.origin) + width(b);
            at < stop - SPACE_INSIDE * em && (adv.at(a.origin) - stop).abs() <= SPACE_INSIDE * em
        }
        _ => false,
    };
    if swallowed {
        return true;
    }
    if between || follows {
        return false;
    }
    let same = SPACE_SAME_PLACE * em;
    before.is_some_and(|b| {
        let start = adv.at(b.origin);
        at >= start - same && at < start + width(b) - SPACE_INSIDE * em
    }) || after.is_some_and(|a| (adv.at(a.origin) - at).abs() < same)
}

/// How far, in ems, a space must sit short of the pen's stop after the mark
/// ahead of it to be inside that mark: further than a kern moves a space.
const SPACE_INSIDE: f64 = 0.15;

/// Marks this close, in ems, begin at the same place.
const SPACE_SAME_PLACE: f64 = 0.02;

/// The weight of one word of a line, from the glyphs of it that put ink
/// down. A face that declares no weight counts as regular.
#[derive(Default)]
struct Word {
    inked: usize,
    heavy: usize,
    letters: usize,
    heavy_letters: usize,
    /// Holds a letter or a digit.
    spells: bool,
    /// Set after a gap the line pass parts columns at ([`BLOCK_GAP_RATIO`]),
    /// which it kept this line whole across: a number set before a tab.
    parted: bool,
}

impl Word {
    fn add(&mut self, g: &Glyph) {
        let heavy = usize::from(g.weight.is_some_and(|w| w >= 600));
        self.inked += 1;
        self.heavy += heavy;
        if g.text.chars().any(char::is_alphabetic) {
            self.letters += 1;
            self.heavy_letters += heavy;
        }
        self.spells |= g.text.chars().any(char::is_alphanumeric);
    }

    /// Mostly bold, by its letters where it has any. A bold face's digits
    /// and signs are often set from another face that declares no weight
    /// (`E3/E3x`, every letter 700 and neither `3` nor `/`).
    fn bold(&self) -> bool {
        if self.letters > 0 {
            self.heavy_letters * 2 > self.letters
        } else {
            self.heavy * 2 > self.inked
        }
    }
}

fn build_line(glyphs: &[Glyph], idxs: &[usize], bucket: i32, space_ratio: f64) -> Option<Line> {
    if idxs.is_empty() {
        return None;
    }
    let adv = Advance::of(bucket);
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
                let start = adv.start(&b).min(adv.at(g.origin));
                if let Some(pe) = prev {
                    gaps.push((start - pe) / g.font_size.max(1.0) as f64);
                }
                prev = Some(adv.at(g.origin) + a);
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
    // A power being read: its size and baseline, and whether it is set in
    // superior figures, at the text's own size and baseline.
    let mut exponent: Option<(f32, f64, bool)> = None;
    // Where a raised number that leads a word ends, the word parting from it.
    let mut leading: Option<usize> = None;
    // The last glyph of a mixed number's fraction being read.
    let mut fraction: Option<usize> = None;
    // The glyphs read as degree signs so far.
    let mut degrees: Vec<usize> = Vec::new();
    let mut words: Vec<Word> = Vec::new();
    // A drop cap at the head of the line ([`drop_caps`]) is its first
    // word's first letter, set apart from the rest by the cap's indent
    // alone: `This`, not `T his`.
    let cap_head = adv == Advance::Right && {
        let mut sz: Vec<f64> = idxs.iter().map(|&i| glyphs[i].font_size as f64).collect();
        sz.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut bl: Vec<f64> = idxs.iter().map(|&i| glyphs[i].origin.1).collect();
        bl.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let (fs, base) = (sz[sz.len() / 2].max(1.0), bl[bl.len() / 2]);
        let g0 = &glyphs[idxs[0]];
        g0.font_size as f64 >= fs * DROP_CAP_RATIO && g0.origin.1 - base > fs * 0.8
    };

    for (k, &i) in idxs.iter().enumerate() {
        let g = &glyphs[i];
        sizes.push(g.font_size);
        match g.bbox {
            Some(b) => {
                // Prefer the pen position over the glyph box for the *start*
                // too, so a left side bearing does not manufacture a gap.
                let start = adv.start(&b).min(adv.at(g.origin));
                let gap = prev_end.map(|pe| start - pe);
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
                    let after_cap = cap_head && text.chars().count() == 1;
                    if !both_cjk
                        && !after_cap
                        && start - pe > g.font_size as f64 * ratio
                        && !text.ends_with(' ')
                    {
                        text.push(' ');
                    }
                }
                // A mixed number's fraction parts from its whole number:
                // `22 3/8`, not `223/8`.
                if fraction.is_none_or(|end| k > end) && !text.ends_with(' ') {
                    fraction = fraction_at(glyphs, idxs, k, adv);
                    if fraction.is_some() {
                        text.push(' ');
                    }
                }
                let in_fraction = fraction.is_some_and(|end| k <= end);
                // A raised number that leads a word parts from it, as a
                // note's number before its note does: `⁴M17`, read `4 M17`.
                if leading.is_some_and(|end| k == end + 1) && !text.ends_with(' ') {
                    text.push(' ');
                }
                if !in_fraction
                    && leading.is_none_or(|end| k > end)
                    && (text.is_empty() || text.ends_with(' '))
                {
                    leading = leading_mark(glyphs, idxs, k, adv);
                }
                // A note's mark raised after a word parts from it:
                // `Acrylonitrile` and its `a`, not `Acrylonitrilea`. After a
                // space, a raised mark is read only as a degree before its
                // unit, the space kept as printed: `75 °F`.
                let spaced = text.ends_with(' ');
                let raised = if in_fraction {
                    None
                } else {
                    raised_mark(glyphs, idxs, k, adv, &degrees).filter(|(m, next)| {
                        !spaced
                            || (matches!(next, Some('C' | 'F'))
                                && marks::degree(text.trim_end(), m, *next))
                    })
                };
                // A degree set as a raised `o` or `0` reads as one: `145°`,
                // and a units column's `°C` with nothing before it.
                let degree = raised
                    .as_ref()
                    .is_some_and(|(m, next)| marks::degree(text.trim_end(), m, *next))
                    || (text.trim().is_empty() && leading_degree(glyphs, idxs, k, adv));
                if degree {
                    degrees.push(i);
                }
                let raised = raised.filter(|_| !degree).map(|(m, _)| m);
                if raised
                    .as_deref()
                    .is_some_and(|m| marks::note_mark(&text, m))
                {
                    text.push(' ');
                }
                // A power raised after a number keeps its place: `10⁶` and
                // `10⁻³` as superscripts, never flattened into the number as
                // `106` and `10-3`. So does a unit's: `m²`, `PIES²`.
                let continues = exponent.is_some_and(|(size, y, superiors)| {
                    (g.font_size - size).abs() <= size * 0.1
                        && (g.origin.1 - y).abs() < size.max(1.0) as f64 * 0.2
                        && (!superiors || superior(g))
                });
                if in_fraction {
                    exponent = None;
                } else if !continues {
                    exponent = raised
                        .as_deref()
                        .filter(|m| {
                            let power = text.ends_with(|c: char| c.is_ascii_digit())
                                && m.chars().any(|c| c.is_ascii_digit())
                                && m.chars()
                                    .all(|c| c.is_ascii_digit() || POWER_SIGNS.contains(&c));
                            power || marks::unit_power(&text, m)
                        })
                        .map(|_| (g.font_size, g.origin.1, superior(g)));
                }
                // End of the previous glyph in *pen* space, not ink space.
                // A right overhang (`f`, `y`, `r`) makes the ink box extend
                // past where the pen actually stops, shrinking the measured gap
                // and swallowing the following space — `of the` became `ofthe`
                // on 296 occurrences across the corpus against 56 in ground
                // truth. Falls back to the ink box when no advance is known.
                prev_end = Some(match g.advance {
                    Some(a) => adv.at(g.origin) + a,
                    None => adv.end(&b),
                });
                bbox = Some(match bbox {
                    Some(acc) => acc.union(&b),
                    None => b,
                });
                if text.is_empty() || text.ends_with(' ') {
                    let parted =
                        gap.is_some_and(|d| d > g.font_size.max(1.0) as f64 * BLOCK_GAP_RATIO);
                    words.push(Word {
                        parted,
                        ..Word::default()
                    });
                }
                if let Some(w) = words.last_mut() {
                    w.add(g);
                }
                if degree {
                    text.push('\u{00B0}');
                } else if exponent.is_some() {
                    text.extend(g.text.chars().map(superscript));
                } else {
                    text.push_str(&g.text);
                }
            }
            None => {
                // Outline-less but text-bearing: an explicit space. Emit it and
                // leave prev_end alone so the following glyph is measured from
                // the last *visible* mark, not from a zero-width position.
                // On a tracked line the explicit spaces ARE the tracking, so
                // the geometric decision above takes over instead.
                if !tracked
                    && !text.is_empty()
                    && !text.ends_with(' ')
                    && !stray_space(glyphs, idxs, k, adv)
                {
                    text.push(' ');
                }
            }
        }
    }

    let bbox = bbox?;
    // Normalized here (ligatures read as letters; see `text::fold`) so line
    // text is directly usable by NER and search. The raw offsets that resolve
    // to bounding boxes live in `glyphs`, so normalizing the display text
    // costs us nothing (T1.4 in eval/TEST_PLAN.md).
    let text = crate::text::normalize(text.trim());
    if text.is_empty() {
        return None;
    }
    // Median, not mean: robust to a stray oversized glyph (drop cap, symbol).
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let font_size = sizes[sizes.len() / 2];
    // A drop cap at the head of the line reaches down beside the lines
    // under it, and the line is as tall as its own text: taking the cap's
    // height put the line's top far above the next line's and parted the
    // paragraph there (see [`drop_caps`]).
    let bbox = {
        let mut base: Vec<f64> = idxs.iter().map(|&i| glyphs[i].origin.1).collect();
        base.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let base = base[base.len() / 2];
        let fs = font_size.max(1.0) as f64;
        let capped =
            |g: &Glyph| g.font_size as f64 >= fs * DROP_CAP_RATIO && g.origin.1 - base > fs * 0.8;
        let own = idxs
            .iter()
            .map(|&i| &glyphs[i])
            .filter(|g| !capped(g))
            .filter_map(|g| g.bbox)
            .reduce(|a, b| a.union(&b));
        match own {
            Some(o) if adv == Advance::Right && o != bbox => BBox {
                x0: bbox.x0,
                x1: bbox.x1,
                y0: o.y0,
                y1: o.y1,
            },
            _ => bbox,
        }
    };

    // Bold throughout, not in part: a bold word inside a sentence, or a
    // bold label opening a paragraph, does not make the line a heading.
    //
    // Most of the glyphs that put ink down are bold, a face that declares
    // no weight counting as regular. Many documents declare one for the
    // bold face alone, and counting only the glyphs that declare one made a
    // run-in label (`PUMP: With stainless steel pump…`) most of its line.
    //
    // And the line opens and closes on bold words. A label long enough to
    // fill most of its line still ends in the plain text it introduces
    // (`MICROCOMPUTER CONTROL SYSTEM: Hobart`), and one wrapped from the
    // line above opens it plain (`Sani-Dwell. Chemical Sanitizing (65 racks
    // per hour) – 50`). Words with letters decide: a number may open a
    // heading in another face (`3.2`), a sign may stand at either end
    // (`®`), and a chart's bold labels may take every figure from a face
    // that declares no weight (`October 2020`).
    let (inked, heavy) = words
        .iter()
        .fold((0, 0), |(n, h), w| (n + w.inked, h + w.heavy));
    //
    // The last word is the last before a gutter. Where the line pass kept a
    // line whole across one, words past it are not the line's own: a
    // heading run on into the next column's plain text (`9.5. Adapting to
    // the New Normal: Changing` and, across the gutter, `demand during
    // COVID-19;`) read as ending plain.
    let first = words.iter().position(|w| w.letters > 0);
    let own = first.map_or(words.len(), |f| {
        words[f + 1..]
            .iter()
            .position(|w| w.parted)
            .map_or(words.len(), |p| f + 1 + p)
    });
    let opens = first.map(|f| &words[f]);
    let closes = words[..own].iter().rev().find(|w| w.letters > 0);
    let bold = heavy * 2 > inked && opens.is_none_or(Word::bold) && closes.is_none_or(Word::bold);
    let opens_bold = opens.is_some_and(Word::bold);
    let opens_with_script = idxs
        .iter()
        .map(|&i| &glyphs[i])
        .find(|g| g.bbox.is_some())
        .is_some_and(|g| g.font_size as f64 <= font_size as f64 * SCRIPT_SIZE_RATIO);

    Some(Line {
        text,
        bbox,
        page: glyphs[idxs[0]].page,
        rotation_bucket: bucket,
        glyphs: idxs.to_vec(),
        font_size,
        bold,
        opens_bold,
        opens_with_script,
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

    /// A word set from `x`, its letters 5 wide and abutting, on the row at
    /// `y`.
    fn set(text: &str, x: f64, y: f64) -> Vec<Glyph> {
        text.chars()
            .enumerate()
            .map(|(i, c)| g(&c.to_string(), x + 5.0 * i as f64, y, 5.0, 0.0))
            .collect()
    }

    /// A rule drawn up at `x` from `y0` to `y1`.
    fn wall(x: f64, y0: f64, y1: f64) -> crate::rule::Rule {
        crate::rule::Rule {
            bbox: BBox {
                x0: x - 0.4,
                y0,
                x1: x + 0.4,
                y1,
            },
            orientation: crate::rule::Orientation::Vertical,
            page: 0,
        }
    }

    /// The lines of `glyphs`, parted at whichever of `rules` the page's
    /// text makes walls.
    fn walled(glyphs: &[Glyph], rules: &[crate::rule::Rule]) -> Vec<String> {
        assemble_walled(glyphs, &walls(rules, glyphs))
            .into_iter()
            .map(|l| l.text)
            .collect()
    }

    #[test]
    fn a_rule_between_two_boxes_parts_their_row() {
        // A box's side drawn down between a heading's last word and the
        // label in the next box, 7 apart on one baseline: under the gap a
        // gutter takes. Two rows stand beside it each way.
        let mut v = set("ACME", 40.0, 100.0);
        v.extend(set("Name:", 67.0, 100.0));
        v.extend(set("Ltd", 40.0, 116.0));
        v.extend(set("Date:", 67.0, 116.0));
        let side = [wall(63.5, 92.0, 132.0)];
        assert_eq!(walled(&v, &[]), vec!["ACME Name:", "Ltd Date:"]);
        assert_eq!(walled(&v, &side), vec!["ACME", "Name:", "Ltd", "Date:"]);
        // Drawn through a word of another row, it is a chart's line behind
        // its labels, and parts nothing.
        let mut crossed = v.clone();
        crossed.extend(set("Total", 52.0, 124.0));
        assert_eq!(walled(&crossed, &side).len(), 3);
        // Nor does one drawn between two letters of a word.
        let mut letters = set("Range", 40.0, 100.0);
        letters.extend(set("Scope", 40.0, 116.0));
        letters.extend(set("Index", 80.0, 100.0));
        letters.extend(set("Value", 80.0, 116.0));
        assert_eq!(walled(&letters, &[wall(50.0, 92.0, 132.0)]).len(), 2);
        // Nor a single row's cell border, beside one row each way: the
        // row's code and what it names read as one line.
        let mut row = set("B12", 40.0, 200.0);
        row.extend(set("Annual Report", 61.0, 200.0));
        assert_eq!(
            walled(&row, &[wall(57.5, 194.0, 216.0)]),
            vec!["B12 Annual Report"]
        );
        // Nor one too short to run the line's height: a tick under it.
        assert_eq!(
            walled(&v, &[wall(63.5, 108.0, 112.0)]),
            vec!["ACME Name:", "Ltd Date:"]
        );
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

    /// `word` set from `x` in the given weight, one 5-wide glyph a letter.
    fn weighted(word: &str, x: f64, weight: Option<u32>) -> Vec<Glyph> {
        word.chars()
            .enumerate()
            .map(|(k, c)| Glyph {
                weight,
                ..g(&c.to_string(), x + 5.0 * k as f64, 0.0, 5.0, 0.0)
            })
            .collect()
    }

    #[test]
    fn a_bold_label_beside_a_face_declaring_no_weight_is_not_a_bold_line() {
        // Only the bold face declares a weight; the regular one declares
        // none. The label is 5 glyphs of 25.
        let mut v = weighted("PUMP:", 0.0, Some(700));
        v.extend(weighted("Withstainless", 30.0, None));
        v.extend(weighted("steel", 100.0, None));
        let lines = assemble(&v);
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].bold, "{}", lines[0].text);
    }

    #[test]
    fn a_bold_line_with_a_sign_in_another_face_stays_bold() {
        let mut v = weighted("Features", 0.0, Some(700));
        v.extend(weighted("®", 40.0, None));
        assert!(assemble(&v)[0].bold);
        // Digits and signs from a face that declares no weight, between
        // bold letters.
        let mut v = weighted("Multipette", 0.0, Some(700));
        for (k, (c, w)) in [
            ("E", 700),
            ("3", 0),
            ("/", 0),
            ("E", 700),
            ("3", 0),
            ("x", 700),
        ]
        .into_iter()
        .enumerate()
        {
            v.extend(weighted(c, 55.0 + 5.0 * k as f64, (w > 0).then_some(w)));
        }
        let line = &assemble(&v)[0];
        assert_eq!(line.text, "Multipette E3/E3x");
        assert!(line.bold);
    }

    #[test]
    fn a_label_filling_most_of_its_line_is_not_a_bold_line() {
        // Bold to the colon, then the first plain word of the paragraph.
        let mut v = weighted("CONTROLSYSTEM:", 0.0, Some(700));
        v.extend(weighted("Hobart", 75.0, Some(400)));
        assert!(!assemble(&v)[0].bold);
    }

    #[test]
    fn a_figure_from_a_face_declaring_no_weight_does_not_end_a_bold_line() {
        // A chart's bold labels, every digit from another face.
        let mut v = weighted("October", 0.0, Some(700));
        v.extend(weighted("2020", 40.0, None));
        assert!(assemble(&v)[0].bold);
    }

    #[test]
    fn a_label_opening_mid_line_is_not_a_bold_line() {
        let mut v = weighted("Sani-Dwell.", 0.0, Some(400));
        v.extend(weighted("ChemicalSanitizing", 60.0, Some(700)));
        assert!(!assemble(&v)[0].bold);
    }

    #[test]
    fn a_heading_numbered_in_another_face_stays_bold() {
        let mut v = weighted("3.2", 0.0, Some(400));
        v.extend(weighted("Results", 20.0, Some(700)));
        assert!(assemble(&v)[0].bold);
    }

    #[test]
    fn a_heading_run_on_across_a_gutter_ends_where_its_column_does() {
        // A bold heading, a space that puts no ink down, then the next
        // column's plain text 3.5 ems on: the gap is measured past the
        // space, and the heading's line ends at the gutter.
        let mut v = weighted("Adapting", 0.0, Some(700));
        v.push(Glyph {
            bbox: None,
            advance: Some(2.5),
            ..g(" ", 40.0, 0.0, 2.5, 0.0)
        });
        v.extend(weighted("demand", 75.0, None));
        let lines = assemble(&v);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].bold, "{}", lines[0].text);
        assert!(!lines[1].bold, "{}", lines[1].text);
        // Without the gutter, the plain word ends it.
        let mut v = weighted("Adapting", 0.0, Some(700));
        v.extend(weighted("demand", 45.0, None));
        assert!(!assemble(&v)[0].bold);
    }

    /// A glyph set at `size` with its baseline at `baseline`, cap height
    /// seven tenths of the size.
    fn sized(text: &str, x: f64, baseline: f64, size: f32) -> Glyph {
        let cap = size as f64 * 0.7;
        Glyph {
            text: text.into(),
            bbox: Some(BBox {
                x0: x,
                y0: baseline - cap,
                x1: x + size as f64 * 0.5,
                y1: baseline,
            }),
            page: 0,
            origin: (x, baseline),
            rotation_deg: 0.0,
            font_size: size,
            weight: None,
            advance: Some(size as f64 * 0.5),
            draw_index: 0,
        }
    }

    #[test]
    fn a_superscript_joins_its_line() {
        // `cm³/s`: the exponent is 80% of the size and raised half a size —
        // past the baseline tolerance, so it fell out of its line and became
        // a one-character paragraph on its own.
        let v = vec![
            sized("c", 0.0, 10.0, 10.0),
            sized("m", 5.0, 10.0, 10.0),
            sized("3", 10.0, 5.0, 8.0),
            sized("/", 14.0, 10.0, 10.0),
            sized("s", 19.0, 10.0, 10.0),
        ];
        let lines = assemble(&v);
        assert_eq!(
            lines.len(),
            1,
            "the exponent must stay on its line: {lines:?}"
        );
        assert_eq!(lines[0].text, "cm³/s");
        assert_eq!(
            lines[0].font_size, 10.0,
            "one small glyph does not move the median"
        );
    }

    #[test]
    fn a_footnote_opens_with_its_script_number() {
        let v = vec![
            sized("9", 0.0, 6.0, 7.0),
            sized("5", 3.5, 6.0, 7.0),
            sized("I", 8.0, 10.0, 10.0),
            sized("b", 13.0, 10.0, 10.0),
        ];
        let lines = assemble(&v);
        assert_eq!(lines.len(), 1);
        // Its number stays with a word of two letters, as a formula's term
        // keeps what is raised before it.
        assert_eq!(lines[0].text, "95Ib");
        assert!(lines[0].opens_with_script);
        assert!(!assemble(&v[2..])[0].opens_with_script);
    }

    #[test]
    fn a_body_line_beside_a_display_word_is_not_its_script() {
        // A 33pt chapter word with 10.5pt body text wrapped around it: the
        // body line 13pt above the word's baseline is within the shift a
        // 33pt script could have, but nothing a third the size is a script.
        let v = vec![
            sized("R", 100.0, 168.0, 33.0),
            sized("e", 120.0, 168.0, 33.0),
            sized("a", 360.0, 155.0, 10.5),
            sized("s", 365.0, 155.0, 10.5),
        ];
        let lines = assemble(&v);
        assert_eq!(lines.len(), 2, "{lines:?}");
    }

    /// A word set glyph by glyph at one size, from `x`.
    fn sized_word(text: &str, x: f64, baseline: f64, size: f32) -> Vec<Glyph> {
        text.chars()
            .enumerate()
            .map(|(i, c)| {
                sized(
                    &c.to_string(),
                    x + i as f64 * size as f64 * 0.5,
                    baseline,
                    size,
                )
            })
            .collect()
    }

    #[test]
    fn a_body_line_beside_display_type_keeps_to_its_own_line() {
        // A 28pt cover title beside a 10pt description: the description's
        // middle line sits 5pt from the title's baseline, inside a 28pt
        // glyph's jitter but not a 10pt one's.
        let mut v = sized_word("PROCAT", 181.0, 173.8, 28.0);
        v.extend(sized_word("El", 294.0, 157.5, 10.0));
        v.extend(sized_word("suelto", 294.0, 168.5, 10.0));
        v.extend(sized_word("exclusivamente", 294.0, 179.5, 10.0));
        let texts: Vec<String> = assemble(&v).into_iter().map(|l| l.text).collect();
        assert_eq!(texts.len(), 4, "{texts:?}");
        assert!(texts.iter().any(|t| t == "PROCAT"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "suelto"), "{texts:?}");
        // Text set at two sizes on one baseline is still one line.
        let mut v = sized_word("PRICE", 0.0, 100.0, 24.0);
        v.extend(sized_word("each", 70.0, 100.0, 10.0));
        assert_eq!(assemble(&v).len(), 1);
    }

    #[test]
    fn a_drop_cap_heads_the_first_line_beside_it() {
        // A 53pt `T` whose foot sits on the third line's baseline.
        let mut v = vec![sized("T", 66.0, 224.1, 53.0)];
        v.extend(sized_word("his", 99.0, 197.1, 10.5));
        v.extend(sized_word("broad", 99.0, 210.6, 10.5));
        v.extend(sized_word("of", 99.0, 224.1, 10.5));
        v.extend(sized_word("tales", 66.0, 237.6, 10.5));
        let lines = assemble(&v);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts[0], "This", "{texts:?}");
        assert_eq!(&texts[1..], ["broad", "of", "tales"]);
        // The line is as tall as its own text, not the cap.
        assert!(lines[0].bbox.y0 > 189.0, "{:?}", lines[0].bbox);
        // A large letter beside a single line is no drop cap.
        let mut v = vec![sized("A", 66.0, 210.0, 40.0)];
        v.extend(sized_word("grade", 99.0, 197.1, 10.5));
        let texts: Vec<String> = assemble(&v).into_iter().map(|l| l.text).collect();
        assert!(!texts.iter().any(|t| t.starts_with("Agrade")), "{texts:?}");
    }

    #[test]
    fn a_logos_last_letter_is_no_drop_cap() {
        // `JNER` at 40pt, the journal's name in two lines beside the `R`.
        let mut v = sized_word("JNE", 20.0, 60.0, 40.0);
        v.push(sized("R", 80.0, 60.0, 40.0));
        v.extend(sized_word("JOURNAL", 104.0, 40.0, 9.0));
        v.extend(sized_word("AND", 104.0, 52.0, 9.0));
        let texts: Vec<String> = assemble(&v).into_iter().map(|l| l.text).collect();
        assert!(
            !texts.iter().any(|t| t.starts_with("RJOURNAL")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_margin_title_stops_at_the_column_beside_it() {
        // A 33pt title word, 40pt clear of a 10.5pt column whose line sits
        // 3pt from its baseline: under 1.5 of the title's ems, but nearly
        // four of the column's.
        let mut v = sized_word("Regulatory", 100.0, 168.3, 33.0);
        v.push(Glyph {
            bbox: None,
            text: " ".into(),
            ..sized(" ", 266.0, 168.3, 33.0)
        });
        v.extend(sized_word("the", 306.6, 171.2, 10.5));
        let texts: Vec<String> = assemble(&v).into_iter().map(|l| l.text).collect();
        assert_eq!(texts.len(), 2, "{texts:?}");
    }

    #[test]
    fn a_label_set_small_against_display_type_shares_its_line() {
        // A form's `Form` at 6pt, raised a little, right before its 18pt
        // number.
        let mut v = sized_word("Form", 36.5, 48.7, 5.9);
        v.extend(sized_word("7004", 53.2, 52.0, 17.9));
        let texts: Vec<String> = assemble(&v).into_iter().map(|l| l.text).collect();
        assert_eq!(texts, ["Form 7004"]);
    }

    #[test]
    fn a_trademark_sign_is_its_words_at_any_size() {
        // The title's sign, raised and a third its size, lands on the
        // baseline of the body text beside the title.
        let mut v = sized_word("PROCAT", 181.0, 173.8, 28.0);
        v.push(sized("\u{00AE}", 266.0, 157.8, 8.4));
        v.extend(sized_word("El", 294.0, 157.5, 10.0));
        let texts: Vec<String> = assemble(&v).into_iter().map(|l| l.text).collect();
        assert!(texts.iter().any(|t| t == "PROCAT\u{00AE}"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "El"), "{texts:?}");
        // In running text, a 3.5pt sign after a 10pt word.
        let mut v = sized_word("ProCat", 0.0, 100.0, 10.0);
        v.push(sized("\u{00AE}", 30.5, 95.5, 3.5));
        v.extend(sized_word("es", 36.0, 100.0, 10.0));
        let lines = assemble(&v);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].text.starts_with("ProCat\u{00AE}"),
            "{:?}",
            lines[0].text
        );
    }

    #[test]
    fn a_display_title_goes_on_past_its_raised_sign() {
        let space = |x: f64, baseline: f64, size: f32| Glyph {
            bbox: None,
            ..sized(" ", x, baseline, size)
        };
        // A 9.6pt `®` raised 19pt, to the cap height of 32pt type, then a
        // space on the title's baseline and the title's next word.
        let mut v = sized_word("PROCAT", 277.0, 55.61, 32.0);
        v.push(sized("\u{00AE}", 374.0, 36.61, 9.6));
        v.push(space(382.0, 55.61, 14.4));
        v.extend(sized_word("PROFESSIONAL", 390.0, 55.61, 32.0));
        let texts: Vec<String> = assemble(&v).into_iter().map(|l| l.text).collect();
        assert_eq!(texts, ["PROCAT\u{00AE} PROFESSIONAL"]);
        // A `™` at the end of a 17pt title, with a space after it at the
        // title's size, on a baseline of its own between the two.
        let mut v = sized_word("Symmetry", 116.0, 110.4, 16.98);
        v.push(sized("\u{2122}", 185.0, 104.4, 10.98));
        v.push(space(196.0, 102.9, 16.98));
        let texts: Vec<String> = assemble(&v)
            .into_iter()
            .map(|l| l.text.trim().to_string())
            .collect();
        assert_eq!(texts, ["Symmetry\u{2122}"]);
    }

    #[test]
    fn a_sign_keeps_its_word_whatever_space_the_producer_sets_after_it() {
        let space = |x: f64, baseline: f64, size: f32| Glyph {
            bbox: None,
            ..sized(" ", x, baseline, size)
        };
        // A space at the sign's own size and raised baseline.
        let mut v = sized_word("Comfortboard", 72.0, 79.07, 36.0);
        v.push(sized("\u{00AE}", 290.0, 60.08, 10.49));
        v.push(space(295.2, 60.08, 10.49));
        v.extend(sized_word("80", 300.0, 79.07, 36.0));
        let texts: Vec<String> = assemble(&v).into_iter().map(|l| l.text).collect();
        assert_eq!(texts, ["Comfortboard\u{00AE} 80"]);
        // A space at another size, on a baseline of its own.
        let mut v = sized_word("PROCAT", 157.5, 149.2, 28.0);
        v.push(sized("\u{00AE}", 242.0, 131.94, 5.71));
        v.push(space(245.0, 125.88, 16.32));
        v.extend(sized_word("PROFESSIONAL", 80.7, 182.2, 28.0));
        let texts: Vec<String> = assemble(&v).into_iter().map(|l| l.text).collect();
        assert!(
            texts.iter().any(|t| t.starts_with("PROCAT\u{00AE}")),
            "{texts:?}"
        );
        // A space on the baseline of a smaller line beside the title.
        let mut v = sized_word("Comfortboard", 36.0, 48.28, 26.0);
        v.push(sized("\u{00AE}", 192.5, 34.95, 7.58));
        v.push(space(196.3, 38.62, 11.66));
        v.extend(sized_word("80", 200.0, 48.28, 26.0));
        v.extend(sized_word("Technical", 400.0, 38.62, 9.0));
        let texts: Vec<String> = assemble(&v).into_iter().map(|l| l.text).collect();
        assert!(
            texts.contains(&"Comfortboard\u{00AE} 80".to_string()),
            "{texts:?}"
        );
        assert!(texts.contains(&"Technical".to_string()), "{texts:?}");
    }

    #[test]
    fn a_superscript_ahead_in_order_does_not_shed_the_subscripts() {
        // `p²N₍HH₎ = …`: the superscript's origin sorts first, so it opens the
        // baseline cluster. The subscripts sit a quarter below the text and
        // three quarters below the superscript — the cluster must be judged
        // against the text, not the mark that happened to come first.
        let v = vec![
            sized("p", 0.0, 10.0, 10.0),
            sized("2", 5.0, 5.0, 7.0),
            sized("N", 9.0, 10.0, 10.0),
            sized("H", 14.0, 12.5, 7.0),
            sized("H", 17.5, 12.5, 7.0),
        ];
        let lines = assemble(&v);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].text, "p2NHH");
    }

    #[test]
    fn a_small_row_beside_a_large_row_is_not_its_script() {
        // A page's right-hand column sets `Region: North` at 7.4pt four and
        // a half points above the 11.5pt title `Quarterly Review` in the
        // left column, and a 7pt line right under the small one. By size and
        // shift the small rows are the title's scripts; by position they
        // touch none of its glyphs. Every row must come out whole.
        let mut v = Vec::new();
        for (k, c) in "Region: North".chars().enumerate() {
            v.push(sized(&c.to_string(), 340.0 + k as f64 * 3.7, 196.16, 7.4));
        }
        for (k, c) in "Quarterly Review".chars().enumerate() {
            v.push(sized(&c.to_string(), 47.0 + k as f64 * 5.75, 200.66, 11.5));
        }
        for (k, c) in "Summary".chars().enumerate() {
            v.push(sized(&c.to_string(), 341.0 + k as f64 * 3.5, 205.8, 7.0));
        }
        let lines = assemble(&v);
        let mut texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        texts.sort_unstable();
        assert_eq!(
            texts,
            vec!["Quarterly Review", "Region: North", "Summary"],
            "{lines:?}"
        );
    }

    #[test]
    fn a_subscript_joins_its_line() {
        // H₂O: lowered a quarter of the size.
        let v = vec![
            sized("H", 0.0, 10.0, 10.0),
            sized("2", 5.0, 12.5, 7.0),
            sized("O", 9.0, 10.0, 10.0),
        ];
        let lines = assemble(&v);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "H2O");
    }

    #[test]
    fn a_smaller_line_set_beneath_stays_separate() {
        // A 7pt line 6.5pt under a 10pt one: within the script shift, but its
        // ink starts below the larger line's descenders, so it is a line of
        // its own.
        let v = vec![
            sized("A", 0.0, 10.0, 10.0),
            sized("B", 5.0, 10.0, 10.0),
            sized("a", 0.0, 16.5, 7.0),
            sized("b", 4.0, 16.5, 7.0),
        ];
        let lines = assemble(&v);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0].text, "AB");
        assert_eq!(lines[1].text, "ab");
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
    fn text_running_down_the_page_reads_forwards() {
        // A `/Rotate 90` page drawn upright, or a spine label: the pen
        // descends the page, so the glyph lowest on the page is the last
        // letter, not the first. Baseline direction (0, +1) in a y-down
        // frame is -90°.
        let word = "Cascadia";
        let glyphs: Vec<Glyph> = word
            .chars()
            .enumerate()
            .map(|(i, c)| {
                let y = 40.0 + i as f64 * 6.0;
                Glyph {
                    text: c.to_string(),
                    bbox: Some(BBox {
                        x0: 770.0,
                        y0: y,
                        x1: 778.0,
                        y1: y + 6.0,
                    }),
                    page: 0,
                    origin: (770.0, y),
                    rotation_deg: -90.0,
                    font_size: 10.0,
                    weight: None,
                    advance: Some(6.0),
                    draw_index: i,
                }
            })
            .collect();
        let lines = assemble(&glyphs);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Cascadia");
    }

    #[test]
    fn text_running_up_the_page_keeps_its_word_gaps() {
        // An axis title read bottom to top: the pen climbs. Two words with a
        // word gap between them come out as two words, and letters within
        // a word do not gain spaces from their ink boxes.
        let mut glyphs = Vec::new();
        let mut y = 300.0;
        for (wi, word) in ["of", "the"].iter().enumerate() {
            if wi > 0 {
                y -= 4.0; // a word space, ~0.4 em
            }
            for c in word.chars() {
                glyphs.push(Glyph {
                    text: c.to_string(),
                    bbox: Some(BBox {
                        x0: 100.0,
                        y0: y - 5.0,
                        x1: 108.0,
                        y1: y - 1.0, // narrow ink inside a 6pt advance
                    }),
                    page: 0,
                    origin: (100.0, y),
                    rotation_deg: 90.0,
                    font_size: 10.0,
                    weight: None,
                    advance: Some(6.0),
                    draw_index: glyphs.len(),
                });
                y -= 6.0;
            }
        }
        let lines = assemble(&glyphs);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "of the");
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
        // space here would corrupt `（別紙２）` into `（ 別 紙 ２ ）`.
        let v = vec![
            g("\u{FF08}", 0.0, 0.0, 9.0, 0.0),
            g("\u{5225}", 12.0, 0.0, 9.0, 0.0),
            g("\u{7D19}", 24.0, 0.0, 9.0, 0.0),
        ];
        let lines = assemble(&v);
        assert_eq!(lines[0].text, "（別紙", "no spaces between CJK chars");
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

    /// `text` set letter by letter from `x`, pen to pen, 5pt a letter,
    /// drawn in order from `first`.
    fn run(text: &str, x: f64, first: usize) -> Vec<Glyph> {
        text.chars()
            .enumerate()
            .map(|(i, c)| {
                let mut l = g(&c.to_string(), x + 5.0 * i as f64, 0.0, 4.5, 0.0);
                l.advance = Some(5.0);
                l.draw_index = first + i;
                l
            })
            .collect()
    }

    /// An explicit space at `x`, drawn `nth`.
    fn space(x: f64, nth: usize) -> Glyph {
        let mut sp = g(" ", x, 0.0, 0.0, 0.0);
        sp.bbox = None;
        sp.advance = Some(2.0);
        sp.draw_index = nth;
        sp
    }

    #[test]
    fn a_space_drawn_out_of_turn_on_a_letter_parts_nothing() {
        // `Produits`, and a space drawn after the line's last word at the
        // `o`'s own position: where a browser put the space of a line it
        // wrapped.
        let mut v = run("Produits", 100.0, 0);
        v.push(space(110.0, 20));
        assert_eq!(assemble(&v)[0].text, "Produits");
        // Drawn first, sorting ahead of the letter it sits on.
        let mut v = vec![space(110.0, 20)];
        v.extend(run("Produits", 100.0, 0));
        assert_eq!(assemble(&v)[0].text, "Produits");
        // Or inside a letter's advance.
        let mut v = run("Produits", 100.0, 0);
        v.push(space(111.5, 20));
        assert_eq!(assemble(&v)[0].text, "Produits");
    }

    #[test]
    fn a_space_glyph_does_not_hide_a_gutter() {
        // A column's line ends `fit easily into` and its space glyph; a
        // callout across the gutter, 37pt on, starts `Wide`.
        let mut v = run("into", 0.0, 0);
        v.push(space(20.0, 4));
        v.extend(run("Wide", 57.0, 5));
        let lines = assemble(&v);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0].text, "into");
        assert_eq!(lines[1].text, "Wide");
        // A word space the glyph stands in stays one: `of the`.
        let mut v = run("of", 0.0, 0);
        v.push(space(10.0, 2));
        v.extend(run("the", 14.0, 3));
        assert_eq!(assemble(&v).len(), 1);
    }

    /// `text` at `x` on the row `y`, its glyphs drawn from `first`.
    fn run_at(text: &str, x: f64, y: f64, first: usize) -> Vec<Glyph> {
        text.chars()
            .enumerate()
            .map(|(i, c)| {
                let mut l = g(&c.to_string(), x + 5.0 * i as f64, y, 4.5, 0.0);
                l.advance = Some(5.0);
                l.draw_index = first + i;
                l
            })
            .collect()
    }

    #[test]
    fn columns_run_together_row_after_row_part_at_their_gutter() {
        // Two rows of a ragged left column, each ended by a space glyph,
        // and the right column's text at x 200 on both. Each row's end
        // reaches into the other's gap, and both rows span both columns.
        let mut v = run_at("aaaaaaaaaa", 0.0, 0.0, 0);
        v.push(space(50.0, 10));
        v.extend(run_at("bbbb", 200.0, 0.0, 11));
        let mut low = run_at("cccccccccccccccccccccccc", 0.0, 14.0, 20);
        let mut sp = space(120.0, 44);
        sp.origin.1 = 24.0;
        low.push(sp);
        low.extend(run_at("dddd", 200.0, 14.0, 45));
        v.extend(low);
        let texts: Vec<String> = assemble(&v).into_iter().map(|l| l.text).collect();
        assert_eq!(
            texts,
            ["aaaaaaaaaa", "bbbb", "cccccccccccccccccccccccc", "dddd"],
            "{texts:?}"
        );
    }

    #[test]
    fn a_justified_space_crossed_by_its_paragraph_stays() {
        // A line stretched to the measure, a wide space in it, between two
        // lines of its paragraph that run across where it stands.
        let mut v = run_at("cccccccccccccccccccccccccccccccccccccccc", 0.0, -14.0, 0);
        v.extend(run_at("aaaaaaaaaa", 0.0, 0.0, 50));
        v.push(space(50.0, 60));
        v.extend(run_at("bbbbbbbbbbbbbbbbbbbbbbbbb", 75.0, 0.0, 61));
        v.extend(run_at(
            "dddddddddddddddddddddddddddddddddddddddd",
            0.0,
            14.0,
            90,
        ));
        let lines = assemble(&v);
        assert_eq!(lines.len(), 3, "{lines:?}");
    }

    #[test]
    fn a_number_set_off_by_a_tab_keeps_its_text() {
        // `3.1 ⇥ Results`: a heading's number, a space glyph, and the
        // heading's text at the tab stop.
        let mut v = run("3.1", 0.0, 0);
        v.push(space(15.0, 3));
        v.extend(run("Results", 45.0, 4));
        let lines = assemble(&v);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lone_label(&run("12", 0.0, 0), &[0, 1]));
        assert!(!lone_label(&run("NOTE:", 0.0, 0), &[0, 1, 2, 3, 4]));
        let label = |t: &str| {
            let v = run(t, 0.0, 0);
            lone_label(&v, &(0..v.len()).collect::<Vec<_>>())
        };
        assert!(label("Note 3."));
        assert!(label("Item 1A."));
        assert!(label("Article IV"));
        assert!(!label("fit easily"));
        assert!(!label("Rated voltage"));
    }

    #[test]
    fn a_space_drawn_in_turn_is_kept_however_close_the_next_word() {
        // Pen to pen: the space glyph is the only sign of the boundary.
        let mut v = run("of", 0.0, 0);
        v.push(space(10.0, 2));
        v.extend(run("the", 12.0, 3));
        assert_eq!(assemble(&v)[0].text, "of the");
        // `the` begins just past where the space does: a justified line
        // closed up, or a run in another size set where the space is.
        let mut v = run("of", 0.0, 0);
        v.push(space(10.0, 2));
        v.extend(run("the", 10.1, 3));
        assert_eq!(assemble(&v)[0].text, "of the");
    }

    #[test]
    fn a_space_set_back_into_a_ligature_parts_nothing() {
        // `Scientiﬁ` then a space drawn on the ﬁ's ink, its pen moved back,
        // and `c` where the ﬁ's pen stopped.
        let mut v = run("Scienti", 0.0, 0);
        let mut fi = g("\u{FB01}", 35.0, 0.0, 6.0, 0.0);
        fi.advance = Some(6.5);
        fi.draw_index = 7;
        v.push(fi);
        v.push(space(38.0, 8));
        let mut c = g("c", 41.6, 0.0, 4.5, 0.0);
        c.advance = Some(5.0);
        c.draw_index = 9;
        v.push(c);
        assert_eq!(assemble(&v)[0].text, "Scientific");
        // A space where the pen stopped, the next word a space on, stays.
        let mut v = run("of", 0.0, 0);
        v.push(space(10.0, 2));
        v.extend(run("the", 13.0, 3));
        assert_eq!(assemble(&v)[0].text, "of the");
    }

    #[test]
    fn a_space_drawn_out_of_turn_in_a_gap_is_kept() {
        // A line's words drawn first and its spaces after them.
        let mut v = run("of", 0.0, 0);
        v.extend(run("the", 12.0, 2));
        v.push(space(10.0, 9));
        assert_eq!(assemble(&v)[0].text, "of the");
    }

    /// `text` at 10pt from `x`, 5pt a letter, pen to pen; `raised` sets it
    /// at 6pt, 3.5pt above the baseline, as a superscript.
    fn word(text: &str, x: f64, raised: bool) -> Vec<Glyph> {
        let (size, adv, lift) = if raised {
            (6.0, 3.0, 3.5)
        } else {
            (10.0, 5.0, 0.0)
        };
        text.chars()
            .enumerate()
            .map(|(i, c)| {
                let cx = x + adv * i as f64;
                let mut l = g(&c.to_string(), cx, -lift, adv - 0.5, 0.0);
                l.font_size = size;
                l.advance = Some(adv);
                l
            })
            .collect()
    }

    #[test]
    fn a_note_mark_raised_after_a_word_parts_from_it() {
        let mut v = word("Acrylonitrile", 0.0, false);
        v.extend(word("a", 65.0, true));
        assert_eq!(assemble(&v)[0].text, "Acrylonitrile a");
        let mut v = word("Revenue", 0.0, false);
        v.extend(word("12", 35.0, true));
        assert_eq!(assemble(&v)[0].text, "Revenue 12");
    }

    #[test]
    fn raised_text_of_the_word_stays_in_it() {
        // An exponent, an ordinal's ending.
        for (base, raised) in [("1", "st"), ("e", "x")] {
            let mut v = word(base, 0.0, false);
            v.extend(word(raised, 5.0 * base.len() as f64, true));
            assert_eq!(assemble(&v)[0].text, format!("{base}{raised}"));
        }
        // A unit's power, raised: `PIES²` is the Spanish square foot.
        for (base, raised, want) in [("cm", "3", "cm³"), ("1000 PIES", "2", "1000 PIES²")] {
            let mut v = word(base, 0.0, false);
            v.extend(word(raised, 5.0 * base.len() as f64, true));
            assert_eq!(assemble(&v)[0].text, want);
        }
    }

    #[test]
    fn a_degree_set_as_a_raised_o_reads_as_one() {
        for (base, raised, after, want) in [
            ("~ 145", "o", "", "~ 145°"),
            ("25", "o", "C", "25°C"),
            ("5", "0", "C", "5°C"),
            ("1", "o", "", "1o"),
        ] {
            let mut v = word(base, 0.0, false);
            v.extend(word(raised, 5.0 * base.len() as f64, true));
            v.extend(word(after, 5.0 * base.len() as f64 + 3.0, false));
            assert_eq!(assemble(&v)[0].text, want);
        }
    }

    #[test]
    fn a_degree_reads_as_one_after_a_space_alone_or_after_another() {
        // After a number and a space, before its unit: the space as printed.
        let mut v = word("75", 0.0, false);
        v.extend(word("o", 14.0, true));
        v.extend(word("F", 17.0, false));
        assert_eq!(assemble(&v)[0].text, "75 °F");
        // After a space with no unit it stays as it was.
        let mut v = word("45", 0.0, false);
        v.extend(word("o", 14.0, true));
        assert_eq!(assemble(&v)[0].text, "45 o");
        // Alone at the head of a units cell, measured against its capital.
        let mut v = word("o", 0.0, true);
        v.extend(word("C", 3.0, false));
        assert_eq!(assemble(&v)[0].text, "°C");
        // A capital that runs on into a word is no unit.
        let mut v = word("o", 0.0, true);
        v.extend(word("Cat", 3.0, false));
        assert_eq!(assemble(&v)[0].text, "oCat");
        // A second degree in one run, after the first.
        let mut v = word("20", 0.0, false);
        v.extend(word("o", 10.0, true));
        v.extend(word("C/min→250", 13.0, false));
        v.extend(word("o", 58.0, true));
        v.extend(word("C", 61.0, false));
        assert_eq!(assemble(&v)[0].text, "20°C/min→250°C");
    }

    #[test]
    fn a_note_number_after_a_bracket_or_a_quantity_parts_from_it() {
        for (base, raised, want) in [("(49 N)", "5", "(49 N) 5"), ("26TB", "1", "26TB 1")] {
            let mut v = word(base, 0.0, false);
            v.extend(word(raised, 5.0 * base.len() as f64, true));
            assert_eq!(assemble(&v)[0].text, want);
        }
    }

    /// A superior figure at `x`: set at the text's size on its baseline,
    /// its ink small and high, as a font draws its own superiors.
    fn superior_figure(text: &str, x: f64) -> Glyph {
        let mut s = g(text, x, 0.0, 3.0, 0.0);
        s.bbox = Some(BBox {
            x0: x,
            y0: 0.5,
            x1: x + 3.0,
            y1: 4.5,
        });
        s.advance = Some(3.5);
        s
    }

    #[test]
    fn a_callout_beside_a_line_on_a_near_baseline_stays_its_own_line() {
        // A body line, its raised `™`, and a callout set right of it at the
        // same size, its baseline 0.29 of the size higher: read on from the
        // callout, the body line came after it, unspaced.
        let run = |text: &str, x: f64, base: f64, size: f32| -> Vec<Glyph> {
            let w = size as f64 * 0.5;
            text.chars()
                .enumerate()
                .map(|(i, c)| {
                    let cx = x + w * i as f64;
                    let mut l = g(&c.to_string(), cx, base - size as f64, w - 0.5, 0.0);
                    l.bbox = l.bbox.map(|b| BBox { y1: base, ..b });
                    l.origin = (cx, base);
                    l.font_size = size;
                    l.advance = Some(w);
                    if c == ' ' {
                        l.bbox = None;
                    }
                    l
                })
                .collect()
        };
        let mut v = run("The refrigerated models feature", 36.0, 400.49, 9.0);
        v.extend(run("™", 175.5, 397.0, 5.0));
        v.extend(run("GreenCool Technology", 322.0, 397.91, 9.0));
        let lines: Vec<String> = assemble(&v).into_iter().map(|l| l.text).collect();
        assert!(
            lines.contains(&"GreenCool Technology".to_string()),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("The refrigerated models feature")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_superior_figure_is_a_raised_mark() {
        // A note's number after a word or a bracket parts from it, as a
        // smaller raised one does: `Lengths 7`, not `Lengths7`.
        for (base, want) in [("Lengths", "Lengths 7"), ("(48 mm)", "(48 mm) 7")] {
            let mut v = word(base, 0.0, false);
            v.push(superior_figure("7", 5.0 * base.len() as f64));
            assert_eq!(assemble(&v)[0].text, want);
        }
        // After a number it is a power, and the text after it is not.
        let mut v = word("25", 0.0, false);
        v.push(superior_figure("5", 10.0));
        v.extend(word("max", 18.0, false));
        assert_eq!(assemble(&v)[0].text, "25⁵ max");
        // Two superiors are one mark, read as the run they are: no power of
        // the first over the second, no degree. After the word's stop, a
        // note's number, set apart.
        let mut v = word("comfit.", 0.0, false);
        v.push(superior_figure("5", 35.0));
        v.push(superior_figure("0", 38.5));
        assert_eq!(assemble(&v)[0].text, "comfit. 50");
        // A figure set on the line is no superior.
        let mut v = word("Model7", 0.0, false);
        v.extend(word("x", 30.0, false));
        assert_eq!(assemble(&v)[0].text, "Model7x");
    }

    #[test]
    fn a_raised_number_leading_a_word_parts_from_it() {
        // A note's number before the name it is on, and at a note's head.
        let mut v = word("UK.", 0.0, false);
        v.extend(word("4", 17.5, true));
        v.extend(word("M17 series", 20.5, false));
        let lines: Vec<String> = assemble(&v).into_iter().map(|l| l.text).collect();
        assert_eq!(lines, ["UK. 4 M17 series"]);
        let mut v = word("1", 0.0, true);
        v.extend(word("Biocontainment", 3.0, false));
        assert_eq!(assemble(&v)[0].text, "1 Biocontainment");
        // A raised `0` before its temperature's unit is a degree.
        let mut v = word("0", 0.0, true);
        v.extend(word("C", 3.0, false));
        assert_eq!(assemble(&v)[0].text, "°C");
    }

    #[test]
    fn a_power_raised_after_a_number_keeps_its_place() {
        // `kg/10⁶ dscm`, `4.0x10⁻³`: read flat, the unit is a hundredfold
        // off and the value a different number.
        for (base, raised, want) in [("kg/10", "6", "kg/10⁶"), ("4.0x10", "-3", "4.0x10⁻³")] {
            let mut v = word(base, 0.0, false);
            v.extend(word(raised, 5.0 * base.len() as f64, true));
            assert_eq!(assemble(&v)[0].text, want);
        }
        // A note's letters after a value stand apart, a list after any.
        for (base, raised, want) in [("110", "e", "110 e"), ("66", "e,h", "66 e,h")] {
            let mut v = word(base, 0.0, false);
            v.extend(word(raised, 5.0 * base.len() as f64, true));
            assert_eq!(assemble(&v)[0].text, want);
        }
    }

    /// Figures from `x`, each 5 wide, at `size`, with their ink from `top`
    /// down to `foot` over a baseline at 10.
    fn figures(text: &str, x: f64, size: f32, top: f64, foot: f64) -> Vec<Glyph> {
        text.chars()
            .enumerate()
            .map(|(i, c)| {
                let cx = x + 5.0 * i as f64;
                let mut f = g(&c.to_string(), cx, 0.0, 4.5, 0.0);
                f.bbox = Some(BBox {
                    x0: cx,
                    y0: top,
                    x1: cx + 4.5,
                    y1: foot,
                });
                f.font_size = size;
                f.advance = Some(5.0);
                f
            })
            .collect()
    }

    /// A whole number, then a numerator, a slash and a denominator, each
    /// with its ink at `(top, foot)`; the whole number's figures stand 3 to
    /// 10, and the slash runs their height.
    fn mixed(whole: &str, num: (&str, f64, f64), den: (&str, f64, f64)) -> String {
        let mut v = figures(whole, 0.0, 10.0, 3.0, 10.0);
        let mut x = 5.0 * whole.len() as f64;
        v.extend(figures(num.0, x, 10.0, num.1, num.2));
        x += 5.0 * num.0.len() as f64;
        v.extend(figures("/", x, 10.0, 3.0, 10.0));
        v.extend(figures(den.0, x + 5.0, 10.0, den.1, den.2));
        assemble(&v)[0].text.clone()
    }

    #[test]
    fn a_mixed_numbers_fraction_parts_from_its_whole_number() {
        // The font's fraction figures: the numerator short and high, the
        // denominator short on the baseline.
        assert_eq!(mixed("22", ("3", 3.0, 7.0), ("8", 6.0, 10.0)), "22 3/8");
        // The whole fraction set small and raised.
        assert_eq!(mixed("21", ("13", 3.0, 6.5), ("16", 3.0, 6.5)), "21 13/16");
        // A fraction the font draws whole, which its text map spells out.
        let mut v = figures("2", 0.0, 10.0, 3.0, 10.0);
        v.extend(figures("x", 5.0, 10.0, 3.0, 10.0));
        v[1].text = "1/2".into();
        assert_eq!(assemble(&v)[0].text, "2 1/2");
    }

    #[test]
    fn a_numerator_set_raised_is_no_power() {
        // A numerator set smaller and raised, read as `78⁵/16` by the rule
        // that keeps a power raised.
        let mut v = figures("78", 0.0, 10.0, 3.0, 10.0);
        let mut num = word("5", 10.0, true);
        num[0].bbox = Some(BBox {
            x0: 10.0,
            y0: 3.0,
            x1: 12.5,
            y1: 6.5,
        });
        v.extend(num);
        v.extend(figures("/", 13.0, 10.0, 3.0, 10.0));
        v.extend(figures("16", 18.0, 6.0, 6.0, 10.0));
        assert_eq!(assemble(&v)[0].text, "78 5/16");
    }

    #[test]
    fn short_figures_on_the_baseline_or_over_full_ones_are_no_fraction() {
        // Old-style figures are as short, and stand on the baseline.
        assert_eq!(mixed("20", ("12", 6.0, 10.0), ("13", 6.0, 10.0)), "2012/13");
        // A power over a whole number is no fraction's numerator: drawn
        // small and high at the number's size, it is the number's power.
        assert_eq!(mixed("10", ("5", 3.0, 7.0), ("16", 3.0, 10.0)), "10⁵/16");
    }

    #[test]
    fn line_text_reads_ligatures_as_letters() {
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

    /// A line, positioned, for the rejoin tests.
    fn ln(text: &str, x0: f64, x1: f64, y: f64, size: f32) -> Line {
        Line {
            text: text.into(),
            bbox: BBox {
                x0,
                y0: y,
                x1,
                y1: y + f64::from(size),
            },
            page: 0,
            rotation_bucket: 0,
            glyphs: Vec::new(),
            font_size: size,
            bold: false,
            opens_with_script: false,
            opens_bold: false,
        }
    }

    #[test]
    fn a_bisected_line_rejoins_but_two_columns_do_not() {
        let cols = vec![
            crate::column::Column {
                x0: 27.0,
                x1: 303.0,
            },
            crate::column::Column {
                x0: 303.0,
                x1: 585.0,
            },
        ];
        // The genuine case: the cut fell inside a word, so the halves are
        // adjacent — measured at 0.5pt on a 15pt title.
        let mut split = vec![
            vec![ln(", Wide Input R", 60.0, 301.5, 100.0, 15.0)],
            vec![ln("ange, Step-Down", 302.0, 500.0, 100.0, 15.0)],
        ];
        rejoin_spanning_lines(&mut split, &cols);
        assert_eq!(split[1].len(), 0, "a bisected title belongs to one line");

        // Two columns: same baseline, both flush to the cut, but held apart
        // by the gutter — measured at 24pt on 10pt text.
        let mut columns = vec![
            vec![ln(
                "…anode and a cathode. During use, the",
                27.0,
                291.1,
                249.8,
                10.0,
            )],
            vec![ln(
                "overseas mines with controversial",
                315.4,
                560.0,
                249.8,
                10.0,
            )],
        ];
        rejoin_spanning_lines(&mut columns, &cols);
        assert_eq!(columns[1].len(), 1, "a gutter is not a word space");
        assert_eq!(columns[0].len(), 1);
    }

    #[test]
    fn a_full_stop_past_the_cut_rejoins_its_line() {
        // A note across the page ends just past a column's edge: its full
        // stop, alone on the far side, is a dot on the baseline, its top
        // the letters' height below theirs.
        let cols = vec![
            crate::column::Column {
                x0: 30.0,
                x1: 250.0,
            },
            crate::column::Column {
                x0: 250.0,
                x1: 400.0,
            },
        ];
        let mut stop = ln(".", 252.0, 252.6, 506.2, 9.0);
        stop.bbox.y1 = 507.0;
        let mut split = vec![
            vec![ln(
                "the terms apply to every party the agreement lists",
                32.0,
                251.5,
                500.0,
                9.0,
            )],
            vec![stop],
        ];
        rejoin_spanning_lines(&mut split, &cols);
        assert!(split[1].is_empty(), "{:?}", split[1]);
        assert!(split[0][0].text.ends_with("lists."), "{}", split[0][0].text);
    }

    #[test]
    fn a_word_space_at_the_cut_still_rejoins() {
        // `Table of|Contents` measured 3.55pt on 12pt text: a space, not a
        // gutter, so the two halves are one line.
        let cols = vec![
            crate::column::Column {
                x0: 27.0,
                x1: 303.0,
            },
            crate::column::Column {
                x0: 303.0,
                x1: 585.0,
            },
        ];
        let mut v = vec![
            vec![ln("Table of", 240.0, 301.2, 80.0, 12.0)],
            vec![ln("Contents", 304.8, 380.0, 80.0, 12.0)],
        ];
        rejoin_spanning_lines(&mut v, &cols);
        assert_eq!(v[1].len(), 0);
    }

    fn cols_at(cuts: &[f64]) -> Vec<crate::column::Column> {
        let mut out = Vec::new();
        let mut left = 20.0;
        for &c in cuts {
            out.push(crate::column::Column { x0: left, x1: c });
            left = c;
        }
        out.push(crate::column::Column {
            x0: left,
            x1: 590.0,
        });
        out
    }

    #[test]
    fn a_line_across_two_cuts_rejoins_whole() {
        // A disclaimer under a three-column page: its middle part goes back
        // to the first column at the first cut, and its last part must find
        // it there at the second.
        let cols = cols_at(&[214.0, 396.0]);
        let mut v = vec![
            vec![ln("The information … by or on", 36.0, 213.5, 700.0, 7.0)],
            vec![ln("beh", 214.2, 395.6, 700.0, 7.0)],
            vec![ln("alf of The Sherwin-Williams", 396.1, 578.0, 700.0, 7.0)],
        ];
        rejoin_spanning_lines(&mut v, &cols);
        assert_eq!(v[1].len() + v[2].len(), 0, "{v:?}");
        assert_eq!(
            v[0][0].text,
            "The information … by or onbehalf of The Sherwin-Williams"
        );
    }

    #[test]
    fn a_few_rows_the_cut_bisected_over_short_columns_rejoin() {
        // A centred address block over a form's short columns: its rows are
        // more than a quarter of the columns' rows, but four rows are no
        // page of two-column text. Each is one line, whether the cut fell
        // inside a word (`Toll F|ree`, 0.9pt on 7pt) or at a space.
        let cols = cols_at(&[166.0]);
        let mut v = vec![
            vec![
                ln("Fax (636)272-2408 • Toll F", 80.0, 165.4, 80.0, 7.0),
                ln("2001 East Terra Lane •", 80.0, 165.0, 90.0, 7.0),
                ln("Parts Dept.", 80.0, 165.2, 100.0, 7.0),
                ln("Phone", 120.0, 165.1, 110.0, 7.0),
            ],
            vec![
                ln("ree (800)325-6152", 166.3, 320.0, 80.0, 7.0),
                ln("O'Fallon, Missouri", 167.5, 320.0, 90.0, 7.0),
                ln("(800)424-TRUE", 167.6, 320.0, 100.0, 7.0),
                ln("(636)240-2400", 167.4, 320.0, 110.0, 7.0),
            ],
        ];
        rejoin_spanning_lines(&mut v, &cols);
        assert_eq!(v[0][0].text, "Fax (636)272-2408 • Toll Free (800)325-6152");
        assert_eq!(v[0][1].text, "2001 East Terra Lane • O'Fallon, Missouri");
        assert!(v[1].is_empty(), "every row is one line: {v:?}");
    }

    #[test]
    fn a_word_the_cut_fell_inside_rejoins_on_a_page_of_columns() {
        // Rows that end at the cut and go on just past it, row after row:
        // a page of two columns, and the guard holds back. A row cut inside
        // a word is still one line; one cut at a space stays apart.
        let cols = cols_at(&[166.0]);
        let mut left = vec![ln("Fax (636)272-2408 • Toll F", 80.0, 165.4, 80.0, 7.0)];
        let mut right = vec![ln("ree (800)325-6152", 166.3, 320.0, 80.0, 7.0)];
        for k in 0..6 {
            let y = 90.0 + 10.0 * k as f64;
            left.push(ln("the column's text at its edge", 80.0, 165.0, y, 7.0));
            right.push(ln("Another column's text", 167.5, 320.0, y, 7.0));
        }
        let mut v = vec![left, right];
        rejoin_spanning_lines(&mut v, &cols);
        assert_eq!(v[0][0].text, "Fax (636)272-2408 • Toll Free (800)325-6152");
        assert_eq!(v[1].len(), 6, "rows cut at a space stay apart: {v:?}");
    }
}
