//! Resolving an entity span to underline rectangles on a rendered page.
//!
//! This is what lets the UI draw annotations on the real document instead of on
//! extracted text. Because the rectangles and the page render come from the same
//! parse, they agree by construction — no coordinate reconciliation with a second
//! PDF implementation.

use crate::extract::Page;
use crate::geom::BBox;
use crate::glyph::Glyph;
use fluree_doc_model::Element;

/// Where a span of the text projection sits on the page.
#[derive(Debug, Clone, PartialEq)]
pub struct Highlight {
    /// 0-based physical page, the same space as `doc:pageIndex`.
    pub page: usize,
    /// One rectangle per visual line the span crosses, in PDF user units with
    /// a top-left origin — directly usable as CSS.
    pub rects: Vec<BBox>,
    /// The text actually covered, for the caller to check against what it
    /// asked for. A span resolved through a different tokenisation than the
    /// one that produced the offsets will come back subtly wrong, and this is
    /// how the caller sees that rather than drawing a box in the wrong place.
    pub text: String,
}

/// Resolve a span of the text projection to rectangles on a rendered page.
///
/// This is the bridge that closes the loop for annotation overlay: an NER
/// mention is located by character offsets into [`crate::doco::to_text`], and
/// what a viewer needs is a rectangle. `elements` and `pages` must come from
/// the same parse as the projection the offsets index.
///
/// Element-level highlighting needs none of this — the element's own
/// `doc:bbox` is in the graph already. This exists for the span *inside* the
/// element, which is the difference between highlighting a paragraph and
/// highlighting the words someone searched for.
///
/// Returns `None` when the span falls outside every element, when the element
/// it lands in has no geometry (a Markdown or DOCX source), or when its text
/// cannot be located among the page's glyphs. A caller that gets `None` still
/// has the element-level box to fall back to.
///
/// # The offset spaces
///
/// Three are in play and they are not interchangeable. The projection's
/// offsets count characters of element text. A page's glyphs are a different
/// sequence — no synthetic spaces, since only drawn glyphs carry boxes, and
/// in page order rather than reading order. This function crosses between
/// them by locating the span's own characters among the page's glyphs, which
/// is why it returns the text it matched.
pub fn highlight(
    elements: &[Element],
    pages: &[Page],
    begin: usize,
    end: usize,
) -> Option<Highlight> {
    if end <= begin {
        return None;
    }
    let (element, element_start) = element_at(elements, begin)?;
    element.bbox?;
    // The element's own contribution to the projection, which is what the
    // offsets index — not `Element::text`, which differs for a table.
    let projection: Vec<char> = fluree_doc_model::doco::projection_text(element)
        .chars()
        .collect();
    let from = begin.checked_sub(element_start)?;
    let to = end.checked_sub(element_start)?.min(projection.len());
    if from >= to {
        return None;
    }
    let wanted: String = projection[from..to].iter().collect();
    let page = pages.iter().find(|p| p.index == element.page)?;
    let (a, b) = glyph_range_for(page, &wanted, element.rect())?;
    Some(Highlight {
        page: element.page,
        rects: rects_for_glyph_range(&page.glyphs, a, b),
        text: wanted,
    })
}

/// The element containing a projection offset, and where its text begins.
///
/// Walks the projection the same way [`crate::doco::to_text`] builds it, so
/// the two cannot disagree about where an element starts.
fn element_at(elements: &[Element], offset: usize) -> Option<(&Element, usize)> {
    let mut cursor = 0usize;
    let mut first = true;
    for e in elements {
        let text = fluree_doc_model::doco::projection_text(e);
        if text.is_empty() {
            continue;
        }
        if !first {
            cursor += 2; // the "\n\n" separator
        }
        first = false;
        let len = text.chars().count();
        if offset < cursor + len {
            return Some((e, cursor));
        }
        cursor += len;
    }
    None
}

/// NFKC-fold one character, the same normalisation [`crate::line`] applies
/// when it builds line text.
///
/// Line text is normalised on the way into the projection; glyph text keeps
/// whatever the font's ToUnicode said. So the two spell the same character
/// differently — a font emitting MICRO SIGN (U+00B5) against a projection
/// carrying GREEK SMALL LETTER MU (U+03BC), or a ﬁ ligature glyph standing
/// for the two letters it draws. Comparing raw, every such span failed to
/// resolve and the caller drew no highlight at all.
///
/// Per character rather than over the whole string on purpose: the flattened
/// string maps position→glyph, and a whole-string normalisation could compose
/// across a glyph boundary and desynchronise that mapping.
fn fold(c: char) -> impl Iterator<Item = char> {
    use unicode_normalization::UnicodeNormalization;
    c.to_string().nfkc().collect::<Vec<_>>().into_iter()
}

/// Locate a string among a page's glyphs, preferring a match inside `within`.
///
/// Glyph text carries no synthetic spaces, so the needle is compared with
/// whitespace removed — the same reason link anchors are matched that way —
/// and NFKC-folded so it meets the projection's normalisation (see [`fold`]).
fn glyph_range_for(page: &Page, needle: &str, within: BBox) -> Option<(usize, usize)> {
    let squeezed: String = needle
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(fold)
        .collect();
    if squeezed.is_empty() {
        return None;
    }
    // Glyph index → its characters, skipping whitespace-only glyphs, so a
    // position in the flattened string maps back to the glyph that drew it.
    let mut flat = String::new();
    let mut owner: Vec<usize> = Vec::new();
    for (i, g) in page.glyphs.iter().enumerate() {
        for c in g.text.chars().filter(|c| !c.is_whitespace()).flat_map(fold) {
            flat.push(c);
            owner.push(i);
        }
    }
    // Several occurrences are ordinary; the one inside the element's own box
    // is the one the offsets meant.
    let mut best: Option<(usize, usize)> = None;
    let mut from = 0usize;
    while let Some(rel) = flat[from..].find(&squeezed) {
        let at = from + rel;
        let start_c = flat[..at].chars().count();
        let len_c = squeezed.chars().count();
        let (a, b) = (owner[start_c], owner[start_c + len_c - 1]);
        let inside = page.glyphs[a].bbox.is_some_and(|g| within.intersects(&g));
        if inside {
            return Some((a, b));
        }
        best.get_or_insert((a, b));
        from = at + 1;
        if from >= flat.len() {
            break;
        }
    }
    best
}

/// Merge a glyph range into one rect per visual line.
///
/// Whitespace glyphs carry no box; they are skipped rather than breaking the run,
/// so a span like "Hype Cycle" yields a single rect rather than two.
pub fn rects_for_glyph_range(glyphs: &[Glyph], start: usize, end_inclusive: usize) -> Vec<BBox> {
    let mut out: Vec<BBox> = Vec::new();
    let hi = end_inclusive.min(glyphs.len().saturating_sub(1));
    for g in glyphs.iter().take(hi + 1).skip(start) {
        let Some(b) = g.bbox else { continue };
        match out.last_mut() {
            Some(last) if last.same_line(&b) => *last = last.union(&b),
            _ => out.push(b),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(x: f64, y: f64) -> Glyph {
        Glyph {
            text: "a".into(),
            bbox: Some(BBox {
                x0: x,
                y0: y,
                x1: x + 5.0,
                y1: y + 10.0,
            }),
            page: 0,
            origin: (x, y + 10.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            advance: None,
            draw_index: 0,
        }
    }

    #[test]
    fn merges_one_rect_per_line() {
        let v = vec![g(0.0, 0.0), g(5.0, 0.0), g(0.0, 20.0)];
        let r = rects_for_glyph_range(&v, 0, 2);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].x0, 0.0);
        assert_eq!(r[0].x1, 10.0);
    }

    /// A font's ToUnicode and the text projection legitimately spell the
    /// same character differently — the projection is NFKC-normalised on
    /// the way in, glyph text is not. Comparing raw, "0.22 μm" (GREEK MU,
    /// as the projection carries it) never located the glyphs drawing
    /// "0.22 µm" (MICRO SIGN, as the font emits it), so the span resolved
    /// to no highlight at all and a reviewer saw an unannotated page.
    #[test]
    fn micro_sign_glyphs_match_a_greek_mu_needle() {
        let within = BBox {
            x0: 0.0,
            y0: 0.0,
            x1: 200.0,
            y1: 20.0,
        };
        // Glyphs spell it with MICRO SIGN; the needle with GREEK MU.
        let page = page_of(&[("0.22\u{b5}m", 0.0, 0.0)]);
        assert_eq!(
            glyph_range_for(&page, "0.22 \u{3bc}m", within),
            Some((0, 5)),
            "micro-sign glyphs must match a Greek-mu needle"
        );
        // A ligature glyph stands for the letters it draws.
        assert!(
            glyph_range_for(&page_of(&[("\u{fb01}lter", 0.0, 0.0)]), "filter", within).is_some(),
            "ligature glyph must match its spelled-out needle"
        );
        // Different text still finds nothing — the match keeps its teeth.
        assert!(glyph_range_for(
            &page_of(&[("0.45\u{b5}m", 0.0, 0.0)]),
            "0.22 \u{3bc}m",
            within
        )
        .is_none());
    }

    #[test]
    fn whitespace_does_not_split_a_run() {
        let mut v = vec![g(0.0, 0.0), g(5.0, 0.0), g(12.0, 0.0)];
        v[1].bbox = None; // a space
        let r = rects_for_glyph_range(&v, 0, 2);
        assert_eq!(r.len(), 1, "space should not break the line run");
        assert_eq!(r[0].x1, 17.0);
    }

    fn page_of(words: &[(&str, f64, f64)]) -> Page {
        // One glyph per character, laid out left to right on the given line.
        let mut glyphs = Vec::new();
        for (word, x0, y) in words {
            let mut x = *x0;
            for ch in word.chars() {
                glyphs.push(Glyph {
                    text: ch.to_string(),
                    bbox: Some(BBox {
                        x0: x,
                        y0: *y,
                        x1: x + 5.0,
                        y1: y + 9.0,
                    }),
                    page: 0,
                    origin: (x, y + 9.0),
                    rotation_deg: 0.0,
                    font_size: 10.0,
                    advance: Some(5.0),
                    weight: None,
                    draw_index: glyphs.len(),
                });
                x += 5.0;
            }
        }
        Page {
            index: 0,
            glyphs,
            images: Vec::new(),
            rules: Vec::new(),
            fills: Vec::new(),
            width: 600.0,
            height: 800.0,
        }
    }

    fn para(text: &str, bbox: BBox) -> Element {
        Element {
            id: String::new(),
            kind: "doco:Paragraph".into(),
            page: 0,
            bbox: Some(bbox),
            text: text.into(),
            level: None,
            cells: None,
            header_rows: None,
            sub_headers: None,
            merged_down: None,
            merged_left: None,
            figure: None,
            links: None,
            provenance: "rust",
            evidence: "layout",
        }
    }

    #[test]
    fn a_span_inside_the_second_element_resolves_to_its_own_words() {
        // The projection is "alpha beta\n\ngamma delta", so `delta` begins at
        // 10 + 2 + 6 = 18 — an offset that lands in the second element.
        let page = page_of(&[("alphabeta", 10.0, 20.0), ("gammadelta", 10.0, 40.0)]);
        let elements = vec![
            para(
                "alpha beta",
                BBox {
                    x0: 10.0,
                    y0: 20.0,
                    x1: 55.0,
                    y1: 29.0,
                },
            ),
            para(
                "gamma delta",
                BBox {
                    x0: 10.0,
                    y0: 40.0,
                    x1: 60.0,
                    y1: 49.0,
                },
            ),
        ];
        let text = fluree_doc_model::to_text(&elements);
        let begin = text.find("delta").unwrap();
        let h = highlight(&elements, &[page], begin, begin + 5).expect("resolved");
        assert_eq!(h.text, "delta");
        assert_eq!(h.page, 0);
        assert_eq!(h.rects.len(), 1);
        // `gammadelta` starts at x=10 with 5pt glyphs, so `delta` starts at 35.
        assert!((h.rects[0].x0 - 35.0).abs() < 0.01, "{:?}", h.rects);
    }

    #[test]
    fn the_same_words_elsewhere_do_not_win_over_the_element_that_owns_them() {
        // `beta` is drawn twice; the offsets point at the second element, so
        // the rectangle must be the second occurrence.
        let page = page_of(&[("beta", 10.0, 20.0), ("beta", 10.0, 40.0)]);
        let elements = vec![
            para(
                "beta",
                BBox {
                    x0: 10.0,
                    y0: 20.0,
                    x1: 30.0,
                    y1: 29.0,
                },
            ),
            para(
                "beta",
                BBox {
                    x0: 10.0,
                    y0: 40.0,
                    x1: 30.0,
                    y1: 49.0,
                },
            ),
        ];
        let text = fluree_doc_model::to_text(&elements);
        let begin = text.rfind("beta").unwrap();
        let h = highlight(&elements, &[page], begin, begin + 4).unwrap();
        assert_eq!(h.rects[0].y0, 40.0, "must be the second occurrence");
    }

    #[test]
    fn a_source_with_no_geometry_has_no_rectangle_to_give() {
        let mut e = para("alpha beta", BBox::default());
        e.bbox = None;
        assert!(highlight(&[e], &[page_of(&[("alphabeta", 10.0, 20.0)])], 0, 5).is_none());
    }

    #[test]
    fn an_offset_past_the_projection_resolves_to_nothing() {
        let elements = vec![para(
            "alpha",
            BBox {
                x0: 10.0,
                y0: 20.0,
                x1: 35.0,
                y1: 29.0,
            },
        )];
        let page = page_of(&[("alpha", 10.0, 20.0)]);
        assert!(highlight(&elements, &[page], 900, 905).is_none());
    }

    #[test]
    fn an_empty_or_inverted_span_is_refused() {
        let elements = vec![para(
            "alpha",
            BBox {
                x0: 10.0,
                y0: 20.0,
                x1: 35.0,
                y1: 29.0,
            },
        )];
        let pages = [page_of(&[("alpha", 10.0, 20.0)])];
        assert!(highlight(&elements, &pages, 3, 3).is_none());
        assert!(highlight(&elements, &pages, 4, 2).is_none());
    }
}
