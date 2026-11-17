//! Page text with a raw↔normalized offset mapping.
//!
//! Two offset spaces exist and must not be confused:
//!
//! * **raw** — one entry per char emitted by the font's ToUnicode mapping.
//!   Resolves to a glyph, and therefore to a bounding box.
//! * **normalized** — NFKC-applied text. What NER, embedding and gazetteer
//!   matching consume.
//!
//! NFKC changes length (`ﬁ` → `fi`), so a normalized offset cannot index the raw
//! string. `PageText` keeps the map both ways so an entity span found in the
//! normalized text still resolves to exact rectangles on the page.

use unicode_normalization::UnicodeNormalization;

/// A superscript or subscript code point, which NFKC would fold to its plain
/// digit or letter. The unit in "m³/h" is a cubic metre; folded it reads
/// "m3/h", a different string from the one printed and from the one every
/// other reader of the same document emits. Ligatures, wide forms and the rest
/// of NFKC's compatibility mappings stay folded.
///
/// The spacing ring above (U+02DA) is kept too. NFKC decomposes it into a
/// space and a combining ring, so a `˚` the [`degrees`] pass did not read as
/// a degree would leave a stray mark on whatever follows.
fn keeps_form(c: char) -> bool {
    matches!(
        c,
        '\u{00B2}' | '\u{00B3}' | '\u{00B9}' | '\u{02DA}' | '\u{2070}'..='\u{209F}'
    )
}

/// A character a document sets for a degree sign that is not one: the
/// masculine ordinal `º`, the spacing ring `˚`, the superscript zero `⁰`, the
/// ring operator `∘` LaTeX sets for `^\circ`, and the white bullet `◦`.
fn looks_like_degree(c: char) -> bool {
    matches!(
        c,
        '\u{00BA}' | '\u{02DA}' | '\u{2070}' | '\u{2218}' | '\u{25E6}'
    )
}

/// Each degree look-alike in `chars` that reads as a degree, as `°`.
///
/// NFKC folds `º` to the letter `o`, so `25 ºC` read `25 oC`, and turns `˚`
/// into a space and a combining ring; neither is the temperature printed, and
/// a search for `°C` finds neither. A look-alike reads as a degree before a
/// temperature's unit (`ºC`, `˚F`, `(⁰C)`), and the ring and the ring
/// operator also after a number (`45˚`). An ordinal stays one: `1º` and `Nº`
/// are not degrees, and NFKC goes on folding them as before.
///
/// Neighbours are the nearest characters that are not whitespace, and the
/// replacement is one for one. So the pass reads the same over line text,
/// which has spaces, as over a page's glyphs with whitespace dropped, which
/// the span lookup searches, and leaves every offset where it was.
pub(crate) fn degrees(chars: &mut [char]) {
    let read: Vec<char> = chars.to_vec();
    let solid = |c: &&char| !c.is_whitespace();
    for (i, c) in chars.iter_mut().enumerate() {
        if !looks_like_degree(*c) {
            continue;
        }
        let next = read[i + 1..].iter().find(solid).copied();
        let prev = read[..i].iter().rev().find(solid).copied();
        let unit = matches!(next, Some('C' | 'F'));
        let after_number =
            matches!(*c, '\u{02DA}' | '\u{2218}') && prev.is_some_and(|p| p.is_ascii_digit());
        if unit || after_number {
            *c = '\u{00B0}';
        }
    }
}

/// NFKC-normalise one character, keeping super- and subscripts as printed
/// (see [`keeps_form`]). The one normalisation line text, page text and span
/// lookup all share, so the three always spell a character the same way.
pub(crate) fn fold(c: char) -> impl Iterator<Item = char> {
    let out: Vec<char> = if keeps_form(c) {
        vec![c]
    } else {
        c.to_string().nfkc().collect()
    };
    out.into_iter()
}

/// [`fold`] over a string. Runs between kept characters are normalised whole,
/// so a base letter and its combining mark still compose.
pub(crate) fn normalize(s: &str) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    degrees(&mut chars);
    let s: String = chars.into_iter().collect();
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(i) = rest.find(keeps_form) {
        out.extend(rest[..i].nfkc());
        let c = rest[i..].chars().next().unwrap_or_default();
        out.push(c);
        rest = &rest[i + c.len_utf8()..];
    }
    out.extend(rest.nfkc());
    out
}

#[derive(Debug, Default)]
pub struct PageText {
    /// Raw concatenation of glyph text.
    pub raw: String,
    /// NFKC-normalized text — the string NER should see.
    pub normalized: String,
    /// raw char offset -> glyph index
    raw_to_glyph: Vec<usize>,
    /// normalized char offset -> raw char offset
    norm_to_raw: Vec<usize>,
}

impl PageText {
    /// Build from glyph texts, expanding each glyph independently so the
    /// mapping stays exact. Normalizing the whole string at once would be
    /// faster but would lose the per-glyph correspondence.
    pub fn build(glyph_texts: &[String]) -> Self {
        let mut pt = PageText::default();
        let owned: Vec<(usize, char)> = glyph_texts
            .iter()
            .enumerate()
            .flat_map(|(gi, t)| t.chars().map(move |c| (gi, c)))
            .collect();
        // Degrees are read in context, across glyphs, one for one.
        let mut read: Vec<char> = owned.iter().map(|&(_, c)| c).collect();
        degrees(&mut read);
        for (raw_off, (&(gi, ch), &as_read)) in owned.iter().zip(&read).enumerate() {
            pt.raw.push(ch);
            pt.raw_to_glyph.push(gi);
            // Normalize one char at a time: 1 raw char may yield N normalized
            // chars, and every one of them must point back to this raw offset.
            for nch in fold(as_read) {
                pt.normalized.push(nch);
                pt.norm_to_raw.push(raw_off);
            }
        }
        pt
    }

    pub fn glyph_at_raw(&self, raw_off: usize) -> Option<usize> {
        self.raw_to_glyph.get(raw_off).copied()
    }

    pub fn raw_at_norm(&self, norm_off: usize) -> Option<usize> {
        self.norm_to_raw.get(norm_off).copied()
    }

    /// Map a span in normalized space to the inclusive glyph-index range it covers.
    pub fn glyph_range_for_norm_span(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        if start >= end || end > self.norm_to_raw.len() {
            return None;
        }
        let a = self.glyph_at_raw(self.raw_at_norm(start)?)?;
        let b = self.glyph_at_raw(self.raw_at_norm(end - 1)?)?;
        Some((a.min(b), a.max(b)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ligature_normalizes_and_offsets_still_resolve() {
        // "o", "ﬃ", "c", "e" -> normalized "office"
        let g = [
            "o".to_string(),
            "\u{FB03}".to_string(),
            "c".to_string(),
            "e".to_string(),
        ];
        let pt = PageText::build(&g);
        assert_eq!(pt.raw.chars().count(), 4);
        assert_eq!(pt.normalized, "officce".replace("cc", "c")); // "office"
        assert_eq!(pt.normalized, "office");
        // every normalized offset maps back into the raw string
        for i in 0..pt.normalized.chars().count() {
            assert!(pt.raw_at_norm(i).is_some(), "norm offset {i} unmapped");
        }
        // the three chars of the ligature all point at glyph 1
        assert_eq!(pt.glyph_at_raw(pt.raw_at_norm(1).unwrap()), Some(1));
        assert_eq!(pt.glyph_at_raw(pt.raw_at_norm(3).unwrap()), Some(1));
        // and the span "ffi" resolves to exactly glyph 1
        assert_eq!(pt.glyph_range_for_norm_span(1, 4), Some((1, 1)));
    }

    #[test]
    fn no_ligature_codepoints_survive_normalization() {
        let g = ["re\u{FB02}ect".to_string()];
        let pt = PageText::build(&g);
        assert_eq!(pt.normalized, "reflect");
        assert!(!pt
            .normalized
            .chars()
            .any(|c| ('\u{FB00}'..='\u{FB06}').contains(&c)));
    }

    #[test]
    fn superscripts_and_subscripts_are_kept_as_printed() {
        assert_eq!(normalize("m³/h, cm², H₂O, ﬁt"), "m³/h, cm², H₂O, fit");
        let g: Vec<String> = "m³".chars().map(|c| c.to_string()).collect();
        assert_eq!(PageText::build(&g).normalized, "m³");
    }

    #[test]
    fn degree_look_alikes_read_as_degrees() {
        // Before a temperature's unit, with or without a space.
        assert_eq!(normalize("25 ºC, 25ºC, (ºC)"), "25 °C, 25°C, (°C)");
        assert_eq!(
            normalize("25˚C, 77 ˚F, (⁰C), 25∘C, 4 ◦C"),
            "25°C, 77 °F, (°C), 25°C, 4 °C"
        );
        // The ring and the ring operator after a number are angles.
        assert_eq!(normalize("45˚ and 90∘"), "45° and 90°");
        // An ordinal stays one, folded as NFKC always folded it; an exponent
        // stays an exponent; a ring with no degree to read keeps its form
        // instead of leaving a combining mark.
        assert_eq!(
            normalize("1º lugar, Nº 5, 10⁰, x˚y"),
            "1o lugar, No 5, 10⁰, x˚y"
        );
        assert!(!normalize("x˚y").contains('\u{030A}'));
    }

    #[test]
    fn degrees_read_across_glyphs_keep_offsets() {
        let g: Vec<String> = ["2", "5", " ", "\u{00BA}", "C"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let pt = PageText::build(&g);
        assert_eq!(pt.raw, "25 \u{00BA}C");
        assert_eq!(pt.normalized, "25 °C");
        assert_eq!(pt.glyph_range_for_norm_span(3, 5), Some((3, 4)));
    }

    #[test]
    fn plain_ascii_is_identity() {
        let g: Vec<String> = "Hype Cycle".chars().map(|c| c.to_string()).collect();
        let pt = PageText::build(&g);
        assert_eq!(pt.raw, pt.normalized);
        assert_eq!(pt.glyph_range_for_norm_span(0, 4), Some((0, 3)));
    }
}
