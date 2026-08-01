//! Faux-bold overprint removal.
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
    fn without_a_declared_advance_the_size_test_still_applies() {
        // `g` declares no advance, so this is the CJK overprint path.
        let mut v = vec![g("検", 0.0, 0.0, 0), g("検", 0.4, 0.0, 1)];
        assert_eq!(remove_faux_bold(&mut v, 4), 1);
    }
}
