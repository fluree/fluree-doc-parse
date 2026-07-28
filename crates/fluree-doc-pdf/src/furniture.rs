//! Header, footer, page-number and watermark detection.
//!
//! This must run **before** table assembly. Page furniture that leaks into the
//! body does more than add noise: on one regression document a leaked
//! `Acme Analytics, Inc. | AA00733622` landed inside a table cell, and the `|`
//! broke the markdown column count on 2 of 47 rows. It also corrupts NER — the
//! same footer contributes 15 phantom `Acme Analytics, Inc.` organisation
//! mentions and the watermark yields a spurious person/PII entity from an
//! embedded email address, when neither is document content.
//!
//! The signal is **cross-page repetition at a stable position**. A single page
//! carries no evidence; furniture is only identifiable in aggregate.

use crate::line::Line;
use std::collections::HashMap;

/// Fraction of page height from the top/bottom edge within which a line is
/// eligible to be furniture. Wide enough for multi-line headers and for a
/// footer/watermark pair sitting as low as 0.91 and 0.95 of page height.
const EDGE_BAND: f64 = 0.18;

/// Fraction of pages a repeated line must appear on. Deliberately low: a
/// document may change its running head between front matter and body, and a
/// footer that appears on only a third of pages is still a footer.
const MIN_PAGE_FRACTION: f64 = 0.30;

/// Minimum pages before repetition means anything at all.
const MIN_PAGES: usize = 3;

/// Positional agreement required across occurrences, as a fraction of page
/// height. Running heads sit at the same y on every page; a body line that
/// happens to repeat does not.
const POSITION_TOLERANCE: f64 = 0.02;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Furniture {
    Header,
    Footer,
    /// Page number: position is stable, text varies by page.
    PageNumber,
    /// Repeated at a stable position but not near an edge — a watermark or
    /// running side-note.
    Watermark,
}

/// Replace the digits in a line with a placeholder so `Page 1 of 15` and
/// `Page 13 of 15` compare equal. Without this, page numbers never repeat and
/// are never detected.
fn signature(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_num = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            if !in_num {
                out.push('#');
                in_num = true;
            }
        } else {
            in_num = false;
            out.push(c);
        }
    }
    out
}

/// Classify lines across a whole document. Returns, per page, the indices of
/// lines that are furniture and what kind.
///
/// `pages` is one Vec<Line> per page, each paired with that page's height.
pub fn detect(pages: &[(Vec<Line>, f64)]) -> Vec<HashMap<usize, Furniture>> {
    let n_pages = pages.len();
    let mut out: Vec<HashMap<usize, Furniture>> = vec![HashMap::new(); n_pages];
    if n_pages < MIN_PAGES {
        // Repetition is the only evidence we have; too few pages means we
        // cannot distinguish a running head from a one-off line, and guessing
        // would strip real content.
        return out;
    }

    // signature -> occurrences (page, line index, relative y)
    let mut groups: HashMap<String, Vec<(usize, usize, f64, String)>> = HashMap::new();
    for (pi, (lines, height)) in pages.iter().enumerate() {
        if *height <= 0.0 {
            continue;
        }
        for (li, l) in lines.iter().enumerate() {
            let rel = l.bbox.y0 / height;
            // Digit-collapsing is scoped to the edge bands, where pagination
            // lives. Applied everywhere it is too permissive: body text that
            // differs only by a number ("...on page 3" vs "...on page 4")
            // would collapse to one signature and be stripped as furniture.
            // Away from the edges, only exactly-repeated text counts, which is
            // what a watermark or running side-note actually is.
            let in_edge_band = !(EDGE_BAND..=1.0 - EDGE_BAND).contains(&rel);
            let key = if in_edge_band {
                signature(&l.text)
            } else {
                l.text.clone()
            };
            groups
                .entry(key)
                .or_default()
                .push((pi, li, rel, l.text.clone()));
        }
    }

    let needed = ((n_pages as f64 * MIN_PAGE_FRACTION).ceil() as usize).max(MIN_PAGES);

    for (sig, occ) in groups {
        if sig.trim().is_empty() {
            continue;
        }
        // Count distinct pages, not occurrences: a line repeated many times on
        // one page is not furniture.
        let mut pages_seen: Vec<usize> = occ.iter().map(|o| o.0).collect();
        pages_seen.sort_unstable();
        pages_seen.dedup();
        if pages_seen.len() < needed {
            continue;
        }

        // Position must be stable. Use the median as the reference so a single
        // outlier occurrence does not disqualify the group.
        let mut ys: Vec<f64> = occ.iter().map(|o| o.2).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = ys[ys.len() / 2];

        let stable = occ
            .iter()
            .filter(|o| (o.2 - median).abs() < POSITION_TOLERANCE)
            .count();
        if stable < needed {
            continue;
        }

        // A page number is a furniture line whose digits *change* between
        // pages — that is what distinguishes "Page 13 of 15" from a static
        // running head that merely happens to contain a number.
        let mut raw: Vec<&str> = occ.iter().map(|o| o.3.as_str()).collect();
        raw.sort_unstable();
        raw.dedup();
        let digits_vary = sig.contains('#') && raw.len() > 1;

        let kind = classify(median, digits_vary);

        // A watermark appears once per page. `发布机构:` ("issuing agency")
        // occurs four times per page in one government filing as a field
        // label in a record listing, at stable positions — indistinguishable
        // from a watermark by position alone, but clearly content. Requiring
        // at-most-once-per-page separates them. Edge-band furniture is exempt:
        // a header and footer legitimately share a signature on some layouts.
        if kind == Furniture::Watermark {
            let mut per_page: HashMap<usize, usize> = HashMap::new();
            for o in &occ {
                *per_page.entry(o.0).or_default() += 1;
            }
            if per_page.values().any(|&n| n > 1) {
                continue;
            }
        }
        for (pi, li, rel, _) in &occ {
            if (rel - median).abs() < POSITION_TOLERANCE {
                out[*pi].insert(*li, kind);
            }
        }
    }

    out
}

fn classify(rel_y: f64, digits_vary: bool) -> Furniture {
    if digits_vary {
        return Furniture::PageNumber;
    }
    if rel_y < EDGE_BAND {
        Furniture::Header
    } else if rel_y > 1.0 - EDGE_BAND {
        Furniture::Footer
    } else {
        Furniture::Watermark
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::BBox;

    fn line(text: &str, y: f64) -> Line {
        Line {
            text: text.into(),
            bbox: BBox {
                x0: 10.0,
                y0: y,
                x1: 200.0,
                y1: y + 12.0,
            },
            page: 0,
            rotation_bucket: 0,
            glyphs: vec![],
            font_size: 10.0,
            bold: false,
        }
    }

    fn doc(n: usize, f: impl Fn(usize) -> Vec<Line>) -> Vec<(Vec<Line>, f64)> {
        (0..n).map(|i| (f(i), 1000.0)).collect()
    }

    #[test]
    fn detects_repeated_footer() {
        let pages = doc(10, |i| {
            vec![
                line(&format!("Body text unique to page {i}"), 400.0),
                line("Acme Analytics, Inc. | AA00733622", 950.0),
            ]
        });
        let r = detect(&pages);
        assert_eq!(r[0].get(&1), Some(&Furniture::Footer));
        assert_eq!(r[0].get(&0), None, "body must not be flagged");
    }

    #[test]
    fn detects_page_number_despite_varying_digits() {
        let pages = doc(10, |i| vec![line(&format!("Page {} of 10", i + 1), 950.0)]);
        let r = detect(&pages);
        assert_eq!(r[3].get(&0), Some(&Furniture::PageNumber));
    }

    #[test]
    fn detects_watermark_away_from_edges() {
        let pages = doc(10, |_| vec![line("CONFIDENTIAL DRAFT", 500.0)]);
        let r = detect(&pages);
        assert_eq!(r[0].get(&0), Some(&Furniture::Watermark));
    }

    #[test]
    fn ignores_repeated_text_at_unstable_positions() {
        // Same text, but it drifts down the page — body content, not furniture.
        let pages = doc(10, |i| vec![line("See note", 100.0 + i as f64 * 60.0)]);
        let r = detect(&pages);
        assert!(
            r.iter().all(|m| m.is_empty()),
            "drifting text is not furniture"
        );
    }

    #[test]
    fn repeated_field_label_is_not_a_watermark() {
        // Four occurrences per page at stable positions: a record-listing field
        // label, not a watermark. Position alone cannot tell them apart.
        let pages = doc(10, |_| {
            vec![
                line("Issuer:", 200.0),
                line("Issuer:", 400.0),
                line("Issuer:", 600.0),
            ]
        });
        assert!(detect(&pages).iter().all(|m| m.is_empty()));
    }

    #[test]
    fn short_documents_are_left_alone() {
        // Two pages give no repetition evidence; stripping would risk real content.
        let pages = doc(2, |_| vec![line("Header", 20.0)]);
        assert!(detect(&pages).iter().all(|m| m.is_empty()));
    }

    #[test]
    fn static_running_head_with_a_number_is_not_a_page_number() {
        // Same number on every page: a document ID in the footer, not pagination.
        let pages = doc(10, |_| {
            vec![line("Acme Analytics, Inc. | AA00733622", 950.0)]
        });
        let r = detect(&pages);
        assert_eq!(r[0].get(&0), Some(&Furniture::Footer));
    }

    #[test]
    fn identical_body_line_at_a_fixed_position_is_indistinguishable() {
        // Documented limitation: repetition at a stable position is the only
        // evidence we have, so a genuinely identical body line in the same spot
        // on every page will be stripped. Accepted - the alternative is missing
        // real furniture.
        let pages = doc(10, |_| vec![line("Identical body line", 400.0)]);
        assert_eq!(detect(&pages)[0].get(&0), Some(&Furniture::Watermark));
    }

    #[test]
    fn signature_collapses_digit_runs() {
        assert_eq!(signature("Page 13 of 15"), "Page # of #");
        assert_eq!(signature("Page 1 of 15"), "Page # of #");
        assert_ne!(signature("Chapter One"), signature("Chapter Two"));
    }
}

/// Remove a known furniture string from text captured inside a table cell.
///
/// Grids take their glyphs before furniture detection runs, so a footer that
/// crosses a table region on one page is invisible to the cross-page pass —
/// the classic leak is a repeated footer landing inside an appendix table's
/// cells. Furniture whose digits vary across pages (page numbers) matches
/// digit-insensitively: each digit run in the pattern matches any digit run
/// in the cell.
pub fn scrub_cell(cell: &str, furniture: &[(String, bool)]) -> String {
    let mut out = cell.to_string();
    for (pat, digits_vary) in furniture {
        if pat.is_empty() {
            continue;
        }
        if !*digits_vary {
            while let Some(i) = out.find(pat.as_str()) {
                out.replace_range(i..i + pat.len(), " ");
            }
            continue;
        }
        // Segment the pattern at digit runs; match literal segments with any
        // digit run between them.
        let mut segs: Vec<&str> = Vec::new();
        let mut rest = pat.as_str();
        while let Some(d0) = rest.find(|c: char| c.is_ascii_digit()) {
            segs.push(&rest[..d0]);
            let after = rest[d0..]
                .find(|c: char| !c.is_ascii_digit())
                .map(|k| d0 + k)
                .unwrap_or(rest.len());
            rest = &rest[after..];
        }
        segs.push(rest);
        if segs.len() < 2 {
            continue;
        }
        // A pattern that is nothing but digits — a bare page number — has no
        // literal text to anchor on, so digit-insensitive matching would
        // strip the leading digit run out of *any* cell it is applied to: a
        // financial statement's `85,159` became `,159`, silently changing
        // the figure. With nothing to anchor, the only safe reading is that
        // the whole cell is that page number.
        //
        // "All digits" alone is not enough to conclude that. Patterns are
        // deduplicated by digit *shape*, so this list holds one representative
        // per page-number width (`1`, `10`, `100`) rather than every value —
        // and matching on shape alone deleted every all-digit block in the
        // document. A bar chart's `2024` and `2023` axis labels went that way,
        // leaving four figures with no years attached. Width is the evidence
        // the pattern actually carries: a four-digit label is not a
        // three-digit page number.
        if segs.iter().all(|s| s.is_empty()) {
            let t = out.trim();
            let width = pat.chars().filter(char::is_ascii_digit).count();
            if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) && t.chars().count() == width
            {
                out = String::new();
            }
            continue;
        }
        #[allow(clippy::while_let_loop)] // labeled break from a nested match reads clearer here
        'scan: loop {
            let Some(start) = (if segs[0].is_empty() {
                Some(0)
            } else {
                out.find(segs[0])
            }) else {
                break;
            };
            let mut pos = start + segs[0].len();
            for seg in &segs[1..] {
                let digit_end = out[pos..]
                    .find(|c: char| !c.is_ascii_digit())
                    .map(|k| pos + k)
                    .unwrap_or(out.len());
                if digit_end == pos {
                    break 'scan; // expected a digit run, none present
                }
                if !out[digit_end..].starts_with(seg) {
                    break 'scan;
                }
                pos = digit_end + seg.len();
            }
            out.replace_range(start..pos, " ");
        }
    }
    // A line that is a long prefix of a known furniture text is that
    // furniture wrapped or truncated differently on its page (the watermark
    // crossing a table region breaks at a different point than it does in
    // the running footer).
    let trimmed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.chars().count() >= 24 && furniture.iter().any(|(pat, _)| pat.starts_with(&trimmed)) {
        return String::new();
    }
    trimmed
}

#[cfg(test)]
mod scrub_tests {
    use super::scrub_cell;

    #[test]
    fn constant_footer_is_removed_from_a_cell() {
        let f = vec![("Acme Analytics, Inc. | AA00733622".to_string(), false)];
        assert_eq!(
            scrub_cell("Riding the Curve Acme Analytics, Inc. | AA00733622", &f),
            "Riding the Curve"
        );
    }

    #[test]
    fn page_numbers_match_digit_insensitively() {
        let f = vec![("Page 4 of 15".to_string(), true)];
        assert_eq!(scrub_cell("body Page 12 of 15 more", &f), "body more");
    }

    #[test]
    fn a_truncated_watermark_prefix_is_furniture() {
        let f = vec![(
            "This research note is restricted to the personal use of user@example.com.".to_string(),
            false,
        )];
        assert_eq!(
            scrub_cell(
                "This research note is restricted to the personal use of user@exa",
                &f
            ),
            ""
        );
    }

    #[test]
    fn ordinary_cell_text_is_untouched() {
        let f = vec![("Acme Analytics, Inc. | AA00733622".to_string(), false)];
        assert_eq!(
            scrub_cell("Emerging or Adolescent", &f),
            "Emerging or Adolescent"
        );
    }

    #[test]
    fn scrubbing_is_idempotent() {
        let f = vec![("Page 4 of 15".to_string(), true)];
        let once = scrub_cell("body Page 12 of 15 more", &f);
        assert_eq!(scrub_cell(&once, &f), once);
    }

    #[test]
    fn independent_furniture_order_does_not_change_output() {
        let a = ("Confidential".to_string(), false);
        let b = ("Page 4 of 15".to_string(), true);
        let cell = "Confidential body Page 12 of 15";
        assert_eq!(
            scrub_cell(cell, &[a.clone(), b.clone()]),
            scrub_cell(cell, &[b, a])
        );
    }
}
#[test]
fn a_bare_page_number_never_truncates_a_figure() {
    // The pattern is only digits, so it has no literal text to anchor
    // on. Applied as a prefix match it strips the leading digit run out
    // of every numeric cell -- a financial statement's 85,159 became
    // ,159, silently changing the figure.
    let furn = vec![("85".to_string(), true)];
    assert_eq!(scrub_cell("85,159", &furn), "85,159");
    assert_eq!(scrub_cell("$88,821", &furn), "$88,821");
    assert_eq!(scrub_cell("27,471", &furn), "27,471");
    // A cell that is only the page number, at that width, is still furniture.
    assert_eq!(scrub_cell("85", &furn).trim(), "");
    assert_eq!(scrub_cell(" 12 ", &furn).trim(), "");
}

#[test]
fn a_page_number_pattern_does_not_delete_a_wider_number() {
    // Patterns are deduplicated by digit *shape*, so the list holds one
    // representative per page-number width -- `1`, `10`, `100` for a
    // 140-page document -- not every value. Deleting any all-digit cell
    // regardless of width therefore removed real content: a bar chart's
    // `2024` and `2023` axis labels matched a two-digit page number and
    // were dropped, leaving `$16.7 $15.1 18.8% 17.7%` with no years.
    //
    // Width is the evidence the pattern carries. A four-digit label is not
    // a two-digit page number, and nothing else here can tell them apart.
    let furn = vec![("85".to_string(), true)];
    assert_eq!(scrub_cell("2024", &furn), "2024");
    assert_eq!(scrub_cell("2023", &furn), "2023");
    // Every width the document actually shows still scrubs, because every
    // width present produces its own pattern.
    let widths = vec![("1".to_string(), true), ("10".to_string(), true)];
    assert_eq!(scrub_cell("7", &widths).trim(), "");
    assert_eq!(scrub_cell("29", &widths).trim(), "");
    assert_eq!(scrub_cell("2024", &widths), "2024");
}
