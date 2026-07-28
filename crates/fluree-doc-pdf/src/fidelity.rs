//! Does a model's reading say things the page does not?
//!
//! Escalation is only worth having if the arbiter can tell a better reading
//! from a worse one, and shape cannot: a table with the right number of rows
//! and columns can still carry numbers that were never printed. Measured
//! across seven readers on 115 flagged tables, invented values ran from 0.0%
//! for the strongest through 2.0% mid-tier to 16.7% for the weakest — a
//! spread that row counts are entirely blind to.
//!
//! The page is the reference, so no ground truth is needed and this can run
//! in the pipeline rather than only in an experiment. Every number a reading
//! emits is looked for in the glyph layer; one that appears nowhere was not
//! read, it was produced.
//!
//! Two details are load-bearing, both learned by getting them wrong:
//!
//! * Glyphs are joined per baseline with a gap test. Joined without one, the
//!   neighbouring columns `200.52` and `16.76` fuse into a token that matches
//!   neither, and correct readings score as invented.
//! * Matching tolerates the separators. A font whose decimal point maps to
//!   nothing writes `200<fffd>52` where the page prints `200.52`, and a
//!   literal comparison calls that fabrication. Only digits are compared
//!   exactly.
//!
//! Values carrying fewer than three digits are ignored: a bare `1` or `15`
//! occurs somewhere on almost any page, so counting them would dilute the
//! measure toward zero and hide a reading that really is inventing figures.

use crate::glyph::Glyph;

/// Fewest digits a value must carry to be worth checking.
const MIN_DIGITS: usize = 3;

/// Fraction of a glyph's size that counts as a gap between words.
const GAP_FRACTION: f64 = 0.28;

/// Rebuild a page's text as one string per baseline, in reading order.
pub fn page_lines(glyphs: &[Glyph]) -> Vec<String> {
    let mut gs: Vec<(f64, f64, f64, f64, &str)> = glyphs
        .iter()
        .filter(|g| !g.text.trim().is_empty())
        .map(|g| {
            // The declared font size, not the ink height: a digit's bbox is
            // cap height, about seven tenths of the size, which makes the gap
            // threshold too small. Words then fuse — `120` and `00` become
            // `12000` — and every value that really is printed reads as
            // invented. Our own output measured 51% fabricated that way.
            let size = (g.font_size as f64).max(1.0);
            let advance = g.advance.unwrap_or(size * 0.5);
            (g.origin.1, g.origin.0, size, advance, g.text.as_str())
        })
        .collect();
    gs.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    });

    let mut lines = Vec::new();
    let mut band: Vec<(f64, f64, f64, &str)> = Vec::new();
    let mut baseline: Option<f64> = None;
    let flush = |band: &mut Vec<(f64, f64, f64, &str)>| -> String {
        band.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut s = String::new();
        let mut prev: Option<f64> = None;
        for (x, size, advance, t) in band.iter() {
            if let Some(p) = prev {
                if x - p > size * GAP_FRACTION {
                    s.push(' ');
                }
            }
            s.push_str(t);
            prev = Some(x + advance);
        }
        band.clear();
        s
    };
    for (y, x, size, advance, t) in gs {
        let tol = (size * 0.3).max(1.5);
        match baseline {
            Some(b) if (y - b).abs() <= tol => band.push((x, size, advance, t)),
            Some(_) => {
                lines.push(flush(&mut band));
                band.push((x, size, advance, t));
                baseline = Some(y);
            }
            None => {
                band.push((x, size, advance, t));
                baseline = Some(y);
            }
        }
    }
    if !band.is_empty() {
        lines.push(flush(&mut band));
    }
    lines
}

/// Numeric runs in `text` carrying [`MIN_DIGITS`] digits or more.
pub fn values(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() || (!cur.is_empty() && matches!(ch, ',' | '.')) {
            cur.push(ch);
        } else if !cur.is_empty() {
            push_value(&mut out, &cur);
            cur.clear();
        }
    }
    if !cur.is_empty() {
        push_value(&mut out, &cur);
    }
    out
}

fn push_value(out: &mut Vec<String>, raw: &str) {
    let v = raw.trim_end_matches([',', '.']);
    if v.chars().filter(|c| c.is_ascii_digit()).count() >= MIN_DIGITS {
        out.push(v.to_string());
    }
}

/// A separator the page may have written as something else, or as nothing.
fn is_separator(c: char) -> bool {
    matches!(c, ',' | '.' | '\u{00b7}' | '\u{2027}' | '\u{fffd}') || c.is_whitespace()
}

/// Is `value` printed on this line, allowing for damaged separators?
fn on_line(value: &str, line: &str) -> bool {
    let hay: Vec<char> = line.chars().collect();
    let pat: Vec<char> = value.chars().collect();
    if pat.is_empty() {
        return false;
    }
    'start: for i in 0..hay.len() {
        // A match may not begin mid-number.
        if i > 0 && hay[i - 1].is_ascii_digit() && pat[0].is_ascii_digit() {
            continue;
        }
        let mut h = i;
        for (k, p) in pat.iter().enumerate() {
            if p.is_ascii_digit() {
                if h >= hay.len() || hay[h] != *p {
                    continue 'start;
                }
                h += 1;
            } else {
                // A separator may be absent, or written as another mark.
                while h < hay.len() && hay[h].is_whitespace() {
                    h += 1;
                }
                if h < hay.len() && is_separator(hay[h]) {
                    h += 1;
                }
                while h < hay.len() && hay[h].is_whitespace() {
                    h += 1;
                }
            }
            let _ = k;
        }
        // Nor may it end mid-number.
        if h < hay.len() && hay[h].is_ascii_digit() {
            continue;
        }
        return true;
    }
    false
}

/// Treat a plain string as a single searchable line, so a reading can be
/// asked the same question a page is: does this value appear in you?
pub fn page_lines_of(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

/// Is `value` printed anywhere on the page?
pub fn on_page(value: &str, lines: &[String]) -> bool {
    lines.iter().any(|l| on_line(value, l))
}

/// Fraction of the numbers in `text` that appear nowhere on the page.
///
/// Returns `None` when the text carries no checkable value, which is not the
/// same as a clean reading: a table of names cannot be judged this way and
/// must not be waved through as though it had been.
pub fn fabrication_rate(text: &str, lines: &[String]) -> Option<f64> {
    let vals = values(text);
    if vals.is_empty() {
        return None;
    }
    let bad = vals.iter().filter(|v| !on_page(v, lines)).count();
    Some(bad as f64 / vals.len() as f64)
}

/// How much of the page's letter mass a reading carries, or `None` when the
/// page has too little text to judge.
///
/// Not a quality measure — a page escalates precisely because its
/// deterministic reading is wrong, and a reading that fixes the order carries
/// the same letters. It is a *transport* measure: a response truncated at its
/// token ceiling, or a candidate withheld, produces a fragment that reads
/// like a complete reading of a shorter page, and nothing else downstream can
/// tell the difference.
///
/// Letters, not words, and counted rather than matched. [`page_lines`] exists
/// to locate numeric values, so it sets word boundaries from advance gaps and
/// fuses prose into runs like `SemanticSearchPack:Value`; a word-set
/// comparison against it scores a perfect reading near zero. Counting letters
/// is indifferent to spacing, reflowing and reordering, all of which a good
/// reading does on purpose.
pub fn letter_retention(text: &str, page_lines: &[String]) -> Option<f64> {
    let letters = |s: &str| s.chars().filter(|c| c.is_alphabetic()).count();
    let page = letters(&page_lines.join(" "));
    // A scanned page has no glyph layer to compare against, which is the very
    // reason it escalated. Judging a reading against nothing would reject
    // every one of them.
    if page < MIN_LETTERS_TO_JUDGE {
        return None;
    }
    // Capped: a reading may legitimately carry more than the glyph layer had,
    // which is the whole point of reading a page nobody could extract.
    Some((letters(text) as f64 / page as f64).min(1.0))
}

/// Below this many letters on the page, retention says nothing.
const MIN_LETTERS_TO_JUDGE: usize = 100;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::BBox;

    fn glyph(t: &str, x: f64, y: f64) -> Glyph {
        Glyph {
            text: t.into(),
            bbox: Some(BBox {
                x0: x,
                y0: y - 8.0,
                x1: x + 5.0,
                y1: y,
            }),
            page: 0,
            origin: (x, y),
            rotation_deg: 0.0,
            font_size: 8.0,
            advance: None,
            weight: None,
            draw_index: 0,
        }
    }

    fn line_of(s: &str, y: f64) -> Vec<Glyph> {
        s.chars()
            .enumerate()
            .map(|(i, c)| glyph(&c.to_string(), 10.0 + i as f64 * 5.0, y))
            .collect()
    }

    #[test]
    fn a_printed_number_is_on_the_page() {
        let g = line_of("Total 1,505,162", 100.0);
        let lines = page_lines(&g);
        assert!(on_page("1,505,162", &lines));
        assert!(!on_page("1,505,163", &lines));
    }

    #[test]
    fn a_damaged_decimal_point_still_matches() {
        // stress_erp2023 maps its decimal point to nothing, so the glyph
        // layer holds 200<fffd>52 where the page prints 200.52.
        let g = line_of("200\u{fffd}52", 100.0);
        let lines = page_lines(&g);
        assert!(on_page("200.52", &lines), "the page does print this");
    }

    #[test]
    fn a_value_may_not_match_across_a_column_boundary() {
        // `200.52 16.76` must not satisfy a reading that claims `5216`.
        let g = line_of("200.52 16.76", 100.0);
        let lines = page_lines(&g);
        assert!(!on_page("5216", &lines));
        assert!(on_page("16.76", &lines));
    }

    #[test]
    fn short_numbers_are_not_worth_checking() {
        assert_eq!(values("row 1 of 15"), Vec::<String>::new());
        assert_eq!(values("total 1,272 and 9,651"), vec!["1,272", "9,651"]);
    }

    #[test]
    fn a_reading_of_only_names_cannot_be_judged() {
        let lines = vec!["Cambodia Indonesia".to_string()];
        assert_eq!(fabrication_rate("Cambodia", &lines), None);
    }

    #[test]
    fn invented_figures_are_counted() {
        let g = line_of("1,272 9,651", 100.0);
        let lines = page_lines(&g);
        let r = fabrication_rate("1,272 9,651 4,444 5,555", &lines).unwrap();
        assert!((r - 0.5).abs() < 1e-9, "two of four are not printed: {r}");
    }

    #[test]
    fn a_whole_reading_keeps_the_page_and_a_truncated_one_does_not() {
        // `page_lines` fuses prose into runs, which is why this counts
        // letters rather than matching words.
        let page = vec![
            "SemanticSearchPack:Value".to_string(),
            "ofastructured".to_string(),
        ];
        let whole = "Semantic Search Pack: Value of a structured";
        assert_eq!(letter_retention(whole, &page), None, "too short to judge");

        let page: Vec<String> = (0..8)
            .map(|_| "SemanticSearchPackValue".to_string())
            .collect();
        let whole = "Semantic Search Pack Value ".repeat(8);
        assert!(letter_retention(&whole, &page).unwrap() > 0.99);
        // Half a page arrived: the shape a truncated response takes.
        let half = "Semantic Search Pack Value ".repeat(3);
        assert!(letter_retention(&half, &page).unwrap() < 0.5);
    }

    #[test]
    fn a_reading_richer_than_the_glyph_layer_is_capped_not_rewarded() {
        let page: Vec<String> = (0..10).map(|_| "abcdefghijkl".to_string()).collect();
        let more = "abcdefghijkl ".repeat(40);
        assert_eq!(letter_retention(&more, &page), Some(1.0));
    }

    #[test]
    fn a_scanned_page_cannot_be_judged() {
        // No glyph layer is the reason it escalated; judging against nothing
        // would reject every page reading there is.
        assert_eq!(letter_retention("a full reading of a scan", &[]), None);
    }
}
