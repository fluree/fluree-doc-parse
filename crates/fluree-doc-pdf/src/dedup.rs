//! Overprint removal: faux bold, and underlines struck over text.
//!
//! PDFs fake bold by drawing the same text twice with a sub-pixel offset. It is
//! common in CJK documents — the Japanese and Chinese government PDFs in
//! `eval/corpus/` both do it, producing `検検討討会会のの構構成成` and
//! `国国务务院院办办公公厅厅` in raw extraction. Rendering confirms the intent is
//! bold, not duplicated content.
//!
//! A same-text-chunk removal pass is the standard remedy for this class of file.

use crate::glyph::Glyph;

/// Fraction of the pen advance within which two draws count as the same mark.
///
/// The advance is the measure that separates the two cases, and nothing else
/// does. A second draw of the same mark has not moved the pen: its offset is a
/// few percent of an advance. The *next* character has moved it by one full
/// advance. So the boundary sits well below 1.0 and well above the sub-pixel
/// offsets a faux-bold producer uses.
const ADVANCE_TOLERANCE: f64 = 0.25;

/// Fallback for glyphs whose font declares no advance, and the vertical test
/// in every case: a fraction of glyph size.
const OFFSET_TOLERANCE: f64 = 0.30;

/// Drop glyphs that are an overprint of a recent identical glyph.
///
/// Compares against a small window rather than only the previous glyph: the
/// duplicate is usually adjacent in draw order, but some producers emit a whole
/// run twice, so the second copy of run element *n* trails the first by the run
/// length. The window keeps that cheap while catching the common cases.
pub fn remove_faux_bold(glyphs: &mut Vec<Glyph>, window: usize) -> usize {
    let mut keep = vec![true; glyphs.len()];
    let mut removed = 0;

    for i in 0..glyphs.len() {
        if !keep[i] {
            continue;
        }
        let lo = i.saturating_sub(window);
        for j in lo..i {
            if !keep[j] || glyphs[j].text != glyphs[i].text {
                continue;
            }
            if is_overprint(&glyphs[j], &glyphs[i]) {
                keep[i] = false;
                removed += 1;
                break;
            }
        }
    }

    let mut it = keep.iter();
    glyphs.retain(|_| *it.next().unwrap());
    removed + remove_redrawn(glyphs)
}

/// Drop glyphs drawn again where the same glyph already stands, however far
/// back in the drawing it was.
///
/// Some producers draw a page's text twice. A document printed to PDF with
/// its highlighting drew every line once under the highlight's band and once
/// over it, the whole page's text apart in draw order; the window above
/// never saw the copies meet, and every line read with each letter twice,
/// `RReeccoorrdd tthhee`. Two identical glyphs at one position print as one, so the
/// second is dropped wherever it comes. Found by baseline, the copies of one
/// line are all that is compared.
///
/// Within a baseline, the glyphs of one text are kept ordered by where they
/// stand, and a glyph is compared with the [`REDRAWN_CANDIDATES`] nearest it
/// on each side, no more: a file that stacks a million `A`s on one baseline
/// (a font a millionth of a point high) would otherwise have each compared
/// with all those before it. A copy is drawn where the first stands, so it is
/// the nearest.
fn remove_redrawn(glyphs: &mut Vec<Glyph>) -> usize {
    remove_redrawn_by(glyphs, is_overprint)
}

/// [`remove_redrawn`], `overprints` saying whether a glyph is a copy of one
/// before it.
fn remove_redrawn_by(
    glyphs: &mut Vec<Glyph>,
    mut overprints: impl FnMut(&Glyph, &Glyph) -> bool,
) -> usize {
    use std::collections::{BTreeMap, HashMap};
    let band = |y: f64| (y / 2.0).floor() as i64;
    // Each text's glyphs on each band, by where they start.
    let mut seen: HashMap<(&str, i64), BTreeMap<OrderedX, Vec<usize>>> = HashMap::new();
    let mut keep = vec![true; glyphs.len()];
    for i in 0..glyphs.len() {
        let g = &glyphs[i];
        let Some(bbox) = g.bbox.filter(|_| !g.text.trim().is_empty()) else {
            continue;
        };
        let b = band(g.origin.1);
        let x = OrderedX::of(bbox.x0);
        let redrawn = (b - 1..=b + 1).any(|k| {
            seen.get(&(g.text.as_str(), k)).is_some_and(|row| {
                let before = row.range(..=x).rev().flat_map(|(_, js)| js.iter().rev());
                let after = row.range(x..).flat_map(|(_, js)| js.iter());
                before
                    .take(REDRAWN_CANDIDATES)
                    .chain(after.take(REDRAWN_CANDIDATES))
                    .any(|&j| overprints(&glyphs[j], g))
            })
        });
        if redrawn {
            keep[i] = false;
        } else {
            seen.entry((g.text.as_str(), b))
                .or_default()
                .entry(x)
                .or_default()
                .push(i);
        }
    }
    let removed = keep.iter().filter(|k| !**k).count();
    if removed > 0 {
        let mut it = keep.iter();
        glyphs.retain(|_| *it.next().unwrap());
    }
    removed
}

/// How many glyphs of the same text on each side of a glyph, nearest
/// first, [`remove_redrawn`] compares it with.
const REDRAWN_CANDIDATES: usize = 8;

/// A coordinate ordered as `f64::total_cmp` orders it, to key a map by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct OrderedX(i64);

impl OrderedX {
    fn of(x: f64) -> Self {
        let bits = x.to_bits() as i64;
        // Negative numbers' bits order backwards: flip all but the sign.
        OrderedX(bits ^ (((bits >> 63) as u64) >> 1) as i64)
    }
}

/// Drop underscores struck over text: how a line printer underlines.
///
/// It prints the line, returns the carriage and prints `_` under the letters
/// to stress them, so a word and its underline are two glyphs a character
/// apart in draw order and none apart on the page. Read by position they
/// interleave, `_D_O_ _N_O_T`, and a warning's key words are the ones lost. A run of underscores of which any one is struck over another mark
/// on its line goes whole; the stress is not text, and the words are. A
/// run struck over nothing is a blank left to be filled in, and stays.
pub fn remove_struck_underlines(glyphs: &mut Vec<Glyph>) -> usize {
    // Marks by baseline, to find what shares a line with each underscore.
    let mut marks: Vec<usize> = (0..glyphs.len())
        .filter(|&i| {
            let t = glyphs[i].text.as_str();
            glyphs[i].bbox.is_some()
                && glyphs[i].is_horizontal()
                && !t.trim().is_empty()
                && t != "_"
        })
        .collect();
    marks.sort_by(|&a, &b| glyphs[a].origin.1.total_cmp(&glyphs[b].origin.1));
    let struck = |u: &Glyph| -> bool {
        let Some(ub) = u.bbox else { return false };
        let tol = u.font_size.max(1.0) as f64 * OFFSET_TOLERANCE;
        let lo = marks.partition_point(|&m| glyphs[m].origin.1 < u.origin.1 - tol);
        marks[lo..]
            .iter()
            .take_while(|&&m| glyphs[m].origin.1 <= u.origin.1 + tol)
            .any(|&m| {
                let b = glyphs[m].bbox.unwrap();
                let overlap = ub.x1.min(b.x1) - ub.x0.max(b.x0);
                overlap > 0.5 * ub.width().min(b.width()).max(0.1)
            })
    };
    // Runs of underscores on one baseline, each a character from the last.
    let mut unders: Vec<usize> = (0..glyphs.len())
        .filter(|&i| glyphs[i].text == "_" && glyphs[i].bbox.is_some() && glyphs[i].is_horizontal())
        .collect();
    unders.sort_by(|&a, &b| {
        let (ga, gb) = (&glyphs[a], &glyphs[b]);
        ga.origin
            .1
            .total_cmp(&gb.origin.1)
            .then(ga.origin.0.total_cmp(&gb.origin.0))
    });
    let mut drop = vec![false; glyphs.len()];
    let mut run: Vec<usize> = Vec::new();
    let flush = |run: &mut Vec<usize>, drop: &mut Vec<bool>| {
        if run.iter().any(|&i| struck(&glyphs[i])) {
            for &i in run.iter() {
                drop[i] = true;
            }
        }
        run.clear();
    };
    for &i in &unders {
        if let Some(&p) = run.last() {
            let (gp, g) = (&glyphs[p], &glyphs[i]);
            let tol = g.font_size.max(1.0) as f64 * OFFSET_TOLERANCE;
            let next = gp.bbox.unwrap().x1 + gp.advance.unwrap_or(0.0).max(1.0) * 0.5;
            if (g.origin.1 - gp.origin.1).abs() > tol || g.bbox.unwrap().x0 > next {
                flush(&mut run, &mut drop);
            }
        }
        run.push(i);
    }
    flush(&mut run, &mut drop);
    let removed = drop.iter().filter(|d| **d).count();
    let mut it = drop.iter();
    glyphs.retain(|_| !*it.next().unwrap());
    removed
}

fn is_overprint(a: &Glyph, b: &Glyph) -> bool {
    if a.page != b.page || a.rotation_bucket() != b.rotation_bucket() {
        return false;
    }
    let (Some(ba), Some(bb)) = (a.bbox, b.bbox) else {
        // Whitespace has no outline; two spaces in a row are legitimate content,
        // not overprint, so never drop them on geometry we do not have.
        return false;
    };
    let scale = ba.height().max(bb.height()).max(1e-6);
    let dx = (ba.x0 - bb.x0).abs();
    let dy = (ba.y0 - bb.y0).abs();
    // Horizontally, measure against the advance. Height is uncorrelated with
    // width, and using it deleted the second letter of every tight double:
    // a 10pt lowercase `l` is 7.17 tall and advances 1.82, so a tolerance of
    // 30% of height is 2.15 -- wider than the whole character. `small` came
    // out `smal`, `allows` `alows`, `collaboration` `colaboration`, silently,
    // on a page whose Unicode rate reads a perfect 1.00 so nothing escalates.
    // Measured on the affected files, every real double sits at 0.94-1.02
    // advances and every faux-bold overprint far below.
    let reach = match a.advance.filter(|v| *v > 0.0) {
        Some(advance) => advance * ADVANCE_TOLERANCE,
        // No declared advance: fall back to the size-based test rather than
        // dropping the check, which is what the CJK overprint files need.
        None => scale * OFFSET_TOLERANCE,
    };
    dx < reach && dy < scale * OFFSET_TOLERANCE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::BBox;

    fn g(text: &str, x: f64, y: f64, idx: usize) -> Glyph {
        Glyph {
            text: text.into(),
            bbox: Some(BBox {
                x0: x,
                y0: y,
                x1: x + 10.0,
                y1: y + 10.0,
            }),
            page: 0,
            origin: (x, y + 10.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            advance: None,
            draw_index: idx,
        }
    }

    #[test]
    fn removes_adjacent_overprint() {
        let mut v = vec![
            g("検", 0.0, 0.0, 0),
            g("検", 0.4, 0.0, 1),
            g("討", 10.0, 0.0, 2),
        ];
        assert_eq!(remove_faux_bold(&mut v, 4), 1);
        assert_eq!(
            v.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(),
            ["検", "討"]
        );
    }

    #[test]
    fn keeps_genuine_repeats_that_are_far_apart() {
        // "aa" in a word: same text, but a full glyph width apart.
        let mut v = vec![g("a", 0.0, 0.0, 0), g("a", 10.0, 0.0, 1)];
        assert_eq!(remove_faux_bold(&mut v, 4), 0);
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn keeps_same_text_on_different_lines() {
        let mut v = vec![g("x", 0.0, 0.0, 0), g("x", 0.0, 20.0, 1)];
        assert_eq!(remove_faux_bold(&mut v, 4), 0);
    }

    #[test]
    fn does_not_drop_repeated_whitespace() {
        let mut a = Glyph {
            bbox: None,
            ..g(" ", 0.0, 0.0, 0)
        };
        let b = Glyph {
            bbox: None,
            ..g(" ", 0.0, 0.0, 1)
        };
        a.text = " ".into();
        let mut v = vec![a, b];
        assert_eq!(remove_faux_bold(&mut v, 4), 0);
    }

    /// A glyph with the real geometry of a lowercase `l`: tall, narrow, and
    /// carrying its declared advance.
    fn narrow(text: &str, x: f64, idx: usize) -> Glyph {
        Glyph {
            text: text.into(),
            bbox: Some(BBox {
                x0: x,
                y0: 0.0,
                x1: x + 1.0,
                y1: 7.173,
            }),
            page: 0,
            origin: (x, 7.173),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            advance: Some(1.824),
            draw_index: idx,
        }
    }

    #[test]
    fn a_tight_double_letter_is_two_letters() {
        // Measured from the field report: `small` in a 10pt face, where the
        // two `l`s sit 1.800 apart and the glyph is 7.173 tall. Judged against
        // height the offset looks like an overprint; judged against the 1.824
        // advance it is plainly the next character. This produced `smal`,
        // `alows`, `colaboration` — silently, on pages reporting a perfect
        // Unicode rate, so nothing could escalate to catch it.
        let mut v = vec![narrow("l", 0.0, 0), narrow("l", 1.800, 1)];
        assert_eq!(remove_faux_bold(&mut v, 8), 0);
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn a_narrow_glyph_drawn_twice_is_still_overprint() {
        // The same narrow letter faux-bolded: a few percent of an advance,
        // not a whole one.
        let mut v = vec![narrow("l", 0.0, 0), narrow("l", 0.12, 1)];
        assert_eq!(remove_faux_bold(&mut v, 8), 1);
    }

    #[test]
    fn glyphs_stacked_on_one_baseline_are_compared_with_their_neighbours_only() {
        // Forty thousand `A`s a thousandth of a point apart, none a copy of
        // another: each is compared with a few of its neighbours, not with
        // every one before it.
        let n = 40_000;
        let mut v: Vec<Glyph> = (0..n).map(|i| narrow("A", i as f64 * 1e-3, i)).collect();
        let mut compared = 0usize;
        let removed = remove_redrawn_by(&mut v, |_, _| {
            compared += 1;
            false
        });
        assert_eq!(removed, 0);
        assert!(compared <= 2 * REDRAWN_CANDIDATES * n, "{compared}");
        // Copies still found among them: each drawn again where it stood.
        let mut v: Vec<Glyph> = (0..n)
            .chain(0..n)
            .map(|i| narrow("A", i as f64 * 10.0, i))
            .collect();
        assert_eq!(remove_redrawn(&mut v), n);
    }

    #[test]
    fn a_page_drawn_twice_reads_once() {
        // The whole line, then other drawing, then the whole line again:
        // the copies are further apart than any window.
        let mut v: Vec<Glyph> = "Record"
            .chars()
            .enumerate()
            .map(|(i, c)| narrow(&c.to_string(), i as f64 * 3.0, i))
            .collect();
        for k in 0..40 {
            v.push(g("x", 500.0, 300.0 + k as f64 * 20.0, 100 + k));
        }
        v.extend(
            "Record"
                .chars()
                .enumerate()
                .map(|(i, c)| narrow(&c.to_string(), i as f64 * 3.0, 200 + i)),
        );
        remove_faux_bold(&mut v, 8);
        let line: String = v
            .iter()
            .filter(|g| g.origin.1 < 50.0)
            .map(|g| g.text.as_str())
            .collect();
        assert_eq!(line, "Record");
    }

    /// A line-printer glyph: 7.2pt cells on a baseline at 100.
    fn lp(text: &str, col: usize, idx: usize) -> Glyph {
        let x = col as f64 * 7.2;
        let bbox = if text == "_" {
            BBox {
                x0: x,
                y0: 101.0,
                x1: x + 7.2,
                y1: 101.6,
            }
        } else {
            BBox {
                x0: x + 0.6,
                y0: 93.0,
                x1: x + 6.6,
                y1: 100.0,
            }
        };
        Glyph {
            text: text.into(),
            bbox: Some(bbox),
            page: 0,
            origin: (x, 100.0),
            rotation_deg: 0.0,
            font_size: 12.0,
            weight: None,
            advance: Some(7.2),
            draw_index: idx,
        }
    }

    #[test]
    fn an_underline_struck_over_words_goes() {
        // `DO NOT` printed, then underscores struck under it.
        let mut v: Vec<Glyph> = "DO NOT"
            .chars()
            .enumerate()
            .filter(|(_, c)| *c != ' ')
            .map(|(i, c)| lp(&c.to_string(), i, i))
            .collect();
        for i in 0..6 {
            v.push(lp("_", i, 20 + i));
        }
        assert_eq!(remove_struck_underlines(&mut v), 6);
        let t: String = v.iter().map(|g| g.text.as_str()).collect();
        assert_eq!(t, "DONOT");
    }

    #[test]
    fn a_blank_to_fill_in_stays() {
        // `Lot ______` — the underscores follow the label, under nothing.
        let mut v = vec![lp("L", 0, 0), lp("o", 1, 1), lp("t", 2, 2)];
        for i in 4..10 {
            v.push(lp("_", i, i));
        }
        assert_eq!(remove_struck_underlines(&mut v), 0);
        assert_eq!(v.len(), 9);
    }

    #[test]
    fn without_a_declared_advance_the_size_test_still_applies() {
        // `g` declares no advance, so this is the CJK overprint path.
        let mut v = vec![g("検", 0.0, 0.0, 0), g("検", 0.4, 0.0, 1)];
        assert_eq!(remove_faux_bold(&mut v, 4), 1);
    }
}
