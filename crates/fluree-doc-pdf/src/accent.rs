//! Spacing accents set over their letters.
//!
//! Many typesetters have no accented letter in the font, or choose not to use
//! one, and draw `é` as an `e` and a spacing `´` placed over it: journal
//! production systems for authors' names and affiliations, TeX's `\accent`
//! for every accented letter and every `\hat`, `\tilde` and `\bar` in its
//! mathematics. Read in order those are two characters, `Balague´`,
//! `K¨ahler`, `A˚`, where the page shows one: `Balagué`, `Kähler`, `Å`.
//!
//! An accent is read as the letter it is set on when its ink is centred over
//! (or, for a cedilla or ogonek, under) the letter's ink and close to it. Its
//! spacing form becomes the combining mark, composed with the letter into
//! the precomposed character where Unicode has one (`e` + `´` is `é`) and
//! kept as letter plus mark where it has none (`K` + `ˆ` is `K̂`). The same
//! character set beside a letter, not over it, is left alone: `Spearman´s`
//! with the accent standing in for an apostrophe, a prime after a figure,
//! `β˚`, inch marks after a dimension.

use crate::glyph::Glyph;
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// How far an accent's ink may stand from its letter's, in the letter's em.
///
/// An accent touches its letter or nearly does: the widest gaps measured are
/// a fifth of an em, a hat raised over a capital in a formula. A mark on a
/// line of its own, over the next line's letters, stands at least half an
/// em off them even in a code listing set solid, so it is not taken for
/// their accent.
const REACH: f64 = 0.3;

/// How much an accent's size may differ from its letter's, as a ratio.
///
/// An accent is set at its letter's size, from the same font or, in TeX's
/// mathematics, from a text font at the same size. One step of a type scale
/// (about 1.2) is allowed for rounding.
const SIZE_STEP: f32 = 1.25;

/// The combining mark a spacing accent stands for, and whether it hangs
/// below its letter rather than over it.
fn combining(text: &str) -> Option<(char, bool)> {
    let mut cs = text.chars();
    let c = cs.next()?;
    if cs.next().is_some() {
        return None;
    }
    Some(match c {
        '\u{00B4}' | '\u{02CA}' => ('\u{0301}', false), // acute
        '`' | '\u{02CB}' => ('\u{0300}', false),        // grave
        '\u{00A8}' => ('\u{0308}', false),              // diaeresis
        '^' | '\u{02C6}' => ('\u{0302}', false),        // circumflex
        '~' | '\u{02DC}' => ('\u{0303}', false),        // tilde
        '\u{00AF}' | '\u{02C9}' => ('\u{0304}', false), // macron
        '\u{02D8}' => ('\u{0306}', false),              // breve
        '\u{02D9}' => ('\u{0307}', false),              // dot above
        '\u{02DA}' => ('\u{030A}', false),              // ring above
        '\u{02DD}' => ('\u{030B}', false),              // double acute
        '\u{02C7}' => ('\u{030C}', false),              // caron
        '\u{00B8}' => ('\u{0327}', true),               // cedilla
        '\u{02DB}' => ('\u{0328}', true),               // ogonek
        _ => return None,
    })
}

/// One letter, with any marks already composed onto it.
fn is_letter(text: &str) -> bool {
    let mut cs = text.chars();
    cs.next().is_some_and(char::is_alphabetic) && cs.all(is_combining_mark)
}

/// How far `accent`'s ink centre stands from `letter`'s, when the accent is
/// set on the letter: centred on it, over it (or under it), and within
/// [`REACH`].
///
/// Centred is either ink's centre within the other's span: an accent's over
/// a round letter, or a dotless i's narrow stem under a wider acute.
fn set_on(accent: &Glyph, letter: &Glyph, below: bool) -> Option<f64> {
    let (a, b) = (accent.bbox?, letter.bbox?);
    if !letter.is_horizontal() || !is_letter(&letter.text) || letter.font_size <= 0.0 {
        return None;
    }
    let ratio = accent.font_size / letter.font_size;
    if !(1.0 / SIZE_STEP..=SIZE_STEP).contains(&ratio) {
        return None;
    }
    let off = ((a.x0 + a.x1) - (b.x0 + b.x1)).abs() * 0.5;
    if off > a.width().max(b.width()) * 0.5 {
        return None;
    }
    let ay = (a.y0 + a.y1) * 0.5;
    let reach = letter.font_size as f64 * REACH;
    // Page space runs down: over the letter is above its ink's top.
    let placed = if below {
        ay > b.y1 && a.y0 - b.y1 <= reach
    } else {
        ay < b.y0 && b.y0 - a.y1 <= reach
    };
    placed.then_some(off)
}

/// Compose each spacing accent set on a letter into that letter, and drop
/// the accent's glyph. The letter keeps its place, pen and advance; its ink
/// box grows by the accent's. Returns the number composed.
///
/// The letter is found by position on the page, not by draw order: TeX
/// draws an accent before its letter, most producers after, and a formula
/// may draw its hats in a pass of their own.
pub fn compose(glyphs: &mut Vec<Glyph>) -> usize {
    let accents: Vec<(usize, char, bool)> = glyphs
        .iter()
        .enumerate()
        .filter(|(_, g)| g.bbox.is_some() && g.is_horizontal())
        .filter_map(|(i, g)| combining(&g.text).map(|(m, below)| (i, m, below)))
        .collect();
    if accents.is_empty() {
        return 0;
    }
    let mut keep = vec![true; glyphs.len()];
    let mut composed = 0;
    for (i, mark, below) in accents {
        let accent = &glyphs[i];
        let Some(j) = (0..glyphs.len())
            .filter(|&j| j != i && keep[j])
            .filter_map(|j| set_on(accent, &glyphs[j], below).map(|d| (j, d)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(j, _)| j)
        else {
            continue;
        };
        let ink = accent.bbox;
        let letter = &mut glyphs[j];
        // A dotless i or j is the form an accent is set on: the letter is
        // `i` or `j`, as `í` is `i` and an acute.
        let base = match (letter.text.as_str(), below) {
            ("\u{0131}", false) => "i",
            ("\u{0237}", false) => "j",
            (t, _) => t,
        };
        letter.text = format!("{base}{mark}").nfc().collect();
        letter.bbox = letter.bbox.zip(ink).map(|(b, a)| b.union(&a));
        keep[i] = false;
        composed += 1;
    }
    if composed > 0 {
        let mut it = keep.iter();
        glyphs.retain(|_| *it.next().unwrap());
    }
    composed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::BBox;

    /// A glyph at `x` on a 10-pt line with baseline 100: a letter's ink from
    /// `top` down to the baseline, `w` wide.
    fn glyph(text: &str, x: f64, w: f64, top: f64, bottom: f64, n: usize) -> Glyph {
        Glyph {
            text: text.to_string(),
            bbox: Some(BBox {
                x0: x,
                y0: top,
                x1: x + w,
                y1: bottom,
            }),
            page: 0,
            origin: (x, 100.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            advance: Some(w),
            weight: None,
            italic: false,
            draw_index: n,
            ocr: None,
        }
    }

    fn word(glyphs: &[Glyph]) -> String {
        glyphs.iter().map(|g| g.text.as_str()).collect()
    }

    #[test]
    fn an_accent_set_over_its_letter_composes_with_it() {
        // `Rene´`: the acute drawn after the `e`, over it; then `c¸a`, a
        // cedilla hung under its `c`.
        let mut g = vec![
            glyph("R", 0.0, 6.5, 93.0, 100.0, 0),
            glyph("e", 6.5, 5.0, 95.0, 100.0, 1),
            glyph("n", 11.5, 5.5, 95.0, 100.0, 2),
            glyph("e", 17.0, 5.0, 95.0, 100.0, 3),
            glyph("\u{00B4}", 18.0, 3.0, 93.0, 94.5, 4),
            glyph(" ", 22.0, 2.5, 95.0, 100.0, 5),
            glyph("c", 25.0, 4.5, 95.0, 100.0, 6),
            glyph("\u{00B8}", 25.5, 3.0, 100.0, 101.8, 7),
            glyph("a", 29.5, 5.0, 95.0, 100.0, 8),
        ];
        assert_eq!(compose(&mut g), 2);
        assert_eq!(word(&g), "René ça");
        // The letter keeps its pen; its ink takes in the accent's.
        assert_eq!(g[3].origin.0, 17.0);
        assert_eq!(g[3].bbox.unwrap().y0, 93.0);
    }

    #[test]
    fn an_accent_drawn_before_or_apart_from_its_letter_composes() {
        // TeX's `\accent`: the ring set first, on a raised baseline, then
        // the `A` it stands over. A formula's hat drawn after the rest of
        // the line, over a letter with no precomposed form, stays a
        // combining mark.
        let mut ring = glyph("\u{02DA}", 2.0, 2.5, 89.0, 91.0, 0);
        ring.origin.1 = 97.5;
        let mut g = vec![
            ring,
            glyph("A", 0.0, 7.0, 93.0, 100.0, 1),
            glyph("K", 10.0, 6.5, 93.0, 100.0, 2),
            glyph("=", 18.0, 6.0, 96.0, 98.0, 3),
            glyph("0", 25.0, 5.0, 93.0, 100.0, 4),
            glyph(".", 30.0, 2.5, 99.0, 100.0, 5),
            glyph("5", 32.5, 5.0, 93.0, 100.0, 6),
            glyph("\u{02C6}", 11.5, 3.0, 90.0, 92.0, 7),
        ];
        assert_eq!(compose(&mut g), 2);
        assert_eq!(word(&g), "\u{00C5}K\u{0302}=0.5");
    }

    #[test]
    fn an_accent_over_a_dotless_i_reads_as_an_accented_i() {
        // The stem is narrower than the acute, whose centre stands just
        // past it.
        let mut g = vec![
            glyph("\u{0131}", 0.0, 0.7, 95.0, 100.0, 0),
            glyph("\u{00B4}", 0.1, 1.6, 93.0, 94.5, 1),
            glyph("a", 1.5, 5.0, 95.0, 100.0, 2),
        ];
        compose(&mut g);
        assert_eq!(word(&g), "\u{00ED}a");
    }

    #[test]
    fn an_accent_beside_a_letter_is_left_alone() {
        // `n´s`: an acute standing in for an apostrophe, its ink past the
        // `n`'s; `2´`, a prime after a figure; and a tilde on the line, in
        // `a ~ b`, over nothing.
        let mut g = vec![
            glyph("n", 0.0, 5.0, 95.0, 100.0, 0),
            glyph("\u{00B4}", 5.5, 3.0, 93.0, 94.5, 1),
            glyph("s", 8.5, 4.0, 95.0, 100.0, 2),
            glyph("2", 14.0, 5.0, 93.0, 100.0, 3),
            glyph("\u{00B4}", 18.0, 3.0, 93.0, 94.5, 4),
            glyph("a", 22.0, 5.0, 95.0, 100.0, 5),
            glyph("~", 28.0, 5.0, 96.5, 97.5, 6),
            glyph("b", 34.0, 5.0, 93.0, 100.0, 7),
        ];
        assert_eq!(compose(&mut g), 0);
        assert_eq!(word(&g), "n\u{00B4}s2\u{00B4}a~b");
    }

    #[test]
    fn an_accent_too_far_above_or_at_another_size_is_left_alone() {
        // An acute an em over the letter belongs to no letter on this line;
        // one at twice the letter's size is a sign, not its accent.
        let mut big = glyph("\u{00B4}", 0.5, 3.0, 88.0, 92.0, 2);
        big.font_size = 20.0;
        let mut g = vec![
            glyph("e", 0.0, 5.0, 95.0, 100.0, 0),
            glyph("\u{00B4}", 1.0, 3.0, 83.0, 84.5, 1),
            big,
        ];
        assert_eq!(compose(&mut g), 0);
    }
}
