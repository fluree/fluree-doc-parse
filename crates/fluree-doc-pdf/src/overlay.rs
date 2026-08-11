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
    SpanResolver::new(elements, pages).highlight(begin, end)
}

/// Resolve many spans over one document without rebuilding per-span state.
///
/// [`highlight`] is O(elements + glyphs-on-page) *per call*: it locates the
/// element by walking from the first one, and it flattens the page's entire
/// glyph table to search it. That is the right trade for the handful of spans
/// an entity overlay needs. It is the wrong one for a caller resolving every
/// word in a document — there the same two scans repeat per word, and the cost
/// is quadratic in document length.
///
/// This type keeps both pieces of state across calls:
///
///   * a **merge-walk** over the elements, so a span resumes from the element
///     the previous one landed in instead of restarting at zero, and
///   * a **per-page glyph index**, built on first use of that page and reused.
///
/// # Ordering contract
///
/// `begin` must be **non-decreasing** across calls. That is what makes the
/// merge-walk sound: the elements partition the projection in order, so a
/// cursor that only moves forward lands where a from-zero scan would. Going
/// backwards does not panic — the cursor has already passed the earlier
/// element, so the span resolves against a later one or not at all — but the
/// answer is wrong. Callers producing spans in document order (a word scan, a
/// sorted mention list) satisfy this for free.
///
/// Results are otherwise identical to calling [`highlight`] per span; the
/// differential test in this module asserts that over a multi-element,
/// multi-page fixture.
pub struct SpanResolver<'a> {
    elements: &'a [Element],
    pages: &'a [Page],
    /// Next element to consider, and the offset its text begins at.
    idx: usize,
    cursor: usize,
    /// Whether the "\n\n" separator applies yet — the first non-empty element
    /// is not preceded by one. Mirrors [`crate::doco::to_text`].
    first: bool,
    /// The element the cursor currently sits in: `(index, start, char len)`.
    current: Option<(usize, usize, usize)>,
    /// Flattened glyph tables, keyed by `Page::index`, built lazily. A
    /// document's words concentrate on one page at a time, but a caller may
    /// revisit, so these are kept rather than swapped.
    indexes: std::collections::HashMap<usize, PageIndex>,
}

impl<'a> SpanResolver<'a> {
    pub fn new(elements: &'a [Element], pages: &'a [Page]) -> Self {
        Self {
            elements,
            pages,
            idx: 0,
            cursor: 0,
            first: true,
            current: None,
            indexes: std::collections::HashMap::new(),
        }
    }

    /// Resolve one span. See the ordering contract on [`SpanResolver`].
    pub fn highlight(&mut self, begin: usize, end: usize) -> Option<Highlight> {
        if end <= begin {
            return None;
        }
        let (element, element_start) = self.seek(begin)?;
        element.bbox?;
        let projection: Vec<char> = fluree_doc_model::doco::projection_text(element)
            .chars()
            .collect();
        let from = begin.checked_sub(element_start)?;
        let to = end.checked_sub(element_start)?.min(projection.len());
        if from >= to {
            return None;
        }
        let wanted: String = projection[from..to].iter().collect();
        let page_index = element.page;
        let page = self.pages.iter().find(|p| p.index == page_index)?;
        // Build this page's flat glyph table once, then reuse it. Split out so
        // the borrow of `self.indexes` ends before `rects_for_glyph_range`.
        let index = self
            .indexes
            .entry(page_index)
            .or_insert_with(|| PageIndex::build(page));
        let (a, b) = glyph_range_in(index, page, &wanted, element.rect())?;
        Some(Highlight {
            page: page_index,
            rects: rects_for_glyph_range(&page.glyphs, a, b),
            text: wanted,
        })
    }

    /// The element containing `offset`, resuming from the last one.
    ///
    /// Returns exactly what [`element_at`] returns, for any non-decreasing
    /// sequence of offsets — it walks the same projection the same way, only
    /// without restarting.
    fn seek(&mut self, offset: usize) -> Option<(&'a Element, usize)> {
        loop {
            if self.current.is_none() {
                while self.idx < self.elements.len() {
                    let text = fluree_doc_model::doco::projection_text(&self.elements[self.idx]);
                    if text.is_empty() {
                        // Empty elements occupy no offset space and get no
                        // separator — `to_text` skips them outright.
                        self.idx += 1;
                        continue;
                    }
                    if !self.first {
                        self.cursor += 2; // the "\n\n" separator
                    }
                    self.first = false;
                    self.current = Some((self.idx, self.cursor, text.chars().count()));
                    break;
                }
                // Past the last element: every later offset is past it too.
                self.current?;
            }
            let (i, start, len) = self.current?;
            if offset < start + len {
                return Some((&self.elements[i], start));
            }
            self.cursor = start + len;
            self.idx = i + 1;
            self.current = None;
        }
    }
}

/// The element containing a projection offset, and where its text begins.
///
/// Walks the projection the same way [`crate::doco::to_text`] builds it, so
/// the two cannot disagree about where an element starts.
///
/// Kept as the reference implementation of that walk: [`SpanResolver::seek`]
/// is the incremental form and the differential test holds the two together.
#[cfg_attr(not(test), allow(dead_code))]
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
///
/// Kept as the reference implementation — rebuild the index, search it —
/// against which the differential test holds [`SpanResolver`]'s cached form.
/// Were this expressed in terms of the cache, that test would compare the
/// cache with itself.
#[cfg_attr(not(test), allow(dead_code))]
fn glyph_range_for(page: &Page, needle: &str, within: BBox) -> Option<(usize, usize)> {
    glyph_range_in(&PageIndex::build(page), page, needle, within)
}

/// A page's glyphs flattened once into a searchable string.
///
/// Building this is O(glyphs on the page) and allocates two collections, which
/// is why [`SpanResolver`] holds one per page rather than letting every span
/// rebuild it. Depends only on the page, so caching it changes no result.
pub struct PageIndex {
    /// Every non-whitespace glyph character, NFKC-folded, concatenated.
    flat: String,
    /// `flat` character position → the glyph index that drew it.
    owner: Vec<usize>,
}

impl PageIndex {
    pub fn build(page: &Page) -> Self {
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
        Self { flat, owner }
    }
}

/// [`glyph_range_for`] against an already-built index.
///
/// The search always starts at the beginning of `flat`: a page may draw the
/// same word many times and the one inside `within` is the one the offsets
/// meant, so this cannot carry a cursor the way the element walk does.
fn glyph_range_in(
    index: &PageIndex,
    page: &Page,
    needle: &str,
    within: BBox,
) -> Option<(usize, usize)> {
    let squeezed: String = needle
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(fold)
        .collect();
    if squeezed.is_empty() {
        return None;
    }
    let (flat, owner) = (&index.flat, &index.owner);
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
        // Advance one CHARACTER, not one byte: `flat` is UTF-8, and a
        // byte step lands inside a multibyte character, so the next
        // slice panics. Latent until NFKC folding let micro-sign
        // needles match at all — then a page whose text contains µ
        // brought the whole run down.
        from = at + flat[at..].chars().next().map_or(1, char::len_utf8);
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

    /// The needle scan walks `flat` looking for further occurrences.
    /// Advancing by a byte lands inside a multibyte character and the
    /// next slice panics — which took down a whole extraction run once
    /// micro-sign needles started matching. Two occurrences with a µ
    /// between them force the loop past that boundary.
    #[test]
    fn scanning_past_a_match_never_splits_a_character() {
        let within = BBox {
            x0: 0.0,
            y0: 0.0,
            x1: 500.0,
            y1: 20.0,
        };
        // "ab" appears twice, with a micro sign in between; finding the
        // first match must not panic while looking for the second.
        let page = page_of(&[("ab\u{b5}ab", 0.0, 0.0)]);
        assert!(glyph_range_for(&page, "ab", within).is_some());
        // A needle that appears only AFTER the multibyte character is
        // reachable — the scan must actually get there.
        let page = page_of(&[("\u{b5}xy", 0.0, 0.0)]);
        assert!(glyph_range_for(&page, "xy", within).is_some());
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

    /// The from-scratch resolution `highlight` performed before
    /// [`SpanResolver`] existed: locate the element by walking from the first,
    /// rebuild the page's glyph table, search it. Held here so the cached form
    /// is compared against something independent of it.
    fn highlight_from_scratch(
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

    /// Every span a word scan would ask for, both ways, over a document with
    /// the awkward cases in it: an element with no geometry (contributes
    /// offsets but resolves to `None`), an empty element (contributes none and
    /// takes no separator), a repeated word (the bbox tie-break decides which
    /// occurrence wins), and a word that straddles the element boundary.
    ///
    /// This is the guard that makes the merge-walk a performance change rather
    /// than a behavioural one.
    #[test]
    fn resolver_matches_from_scratch_span_for_span() {
        let bbox = |x0: f64, y0: f64, x1: f64, y1: f64| BBox { x0, y0, x1, y1 };
        let mut second = para("gamma alpha", bbox(0.0, 20.0, 100.0, 30.0));
        second.page = 0;
        // No geometry: its offsets are real, its highlights are not.
        let mut ghost = para("hidden text", bbox(0.0, 0.0, 0.0, 0.0));
        ghost.bbox = None;
        let elements = vec![
            para("alpha beta", bbox(0.0, 0.0, 100.0, 10.0)),
            para("", bbox(0.0, 15.0, 100.0, 16.0)),
            second,
            ghost,
            para("delta", bbox(0.0, 40.0, 100.0, 50.0)),
        ];
        let pages = vec![page_of(&[
            ("alphabeta", 0.0, 0.0),
            ("gammaalpha", 0.0, 20.0),
            ("delta", 0.0, 40.0),
        ])];

        // Walk the projection exactly as `word_boxes` does.
        let text = fluree_doc_model::doco::to_text(&elements);
        let chars: Vec<char> = text.chars().collect();
        let mut resolver = SpanResolver::new(&elements, &pages);
        let mut begin: Option<usize> = None;
        let mut compared = 0usize;
        for i in 0..=chars.len() {
            let is_break = i == chars.len() || chars[i].is_whitespace();
            match (is_break, begin) {
                (false, None) => begin = Some(i),
                (true, Some(start)) => {
                    begin = None;
                    assert_eq!(
                        resolver.highlight(start, i),
                        highlight_from_scratch(&elements, &pages, start, i),
                        "span {start}..{i} ({:?}) resolved differently",
                        chars[start..i].iter().collect::<String>()
                    );
                    compared += 1;
                }
                _ => {}
            }
        }
        assert!(compared >= 6, "fixture should exercise several words");

        // Spans that are not word-shaped, including the separator offsets a
        // caller could hand us and the tail past the last element.
        let mut edges = SpanResolver::new(&elements, &pages);
        for (s, e) in [(0, 0), (10, 12), (11, 13), (chars.len(), chars.len() + 4)] {
            assert_eq!(
                edges.highlight(s, e),
                highlight_from_scratch(&elements, &pages, s, e),
                "edge span {s}..{e} resolved differently"
            );
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
