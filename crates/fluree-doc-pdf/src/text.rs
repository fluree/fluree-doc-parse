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
        for (gi, t) in glyph_texts.iter().enumerate() {
            for ch in t.chars() {
                let raw_off = pt.raw.chars().count();
                pt.raw.push(ch);
                pt.raw_to_glyph.push(gi);
                // Normalize one char at a time: 1 raw char may yield N normalized
                // chars, and every one of them must point back to this raw offset.
                for nch in ch.to_string().nfkc() {
                    pt.normalized.push(nch);
                    pt.norm_to_raw.push(raw_off);
                }
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
    fn plain_ascii_is_identity() {
        let g: Vec<String> = "Hype Cycle".chars().map(|c| c.to_string()).collect();
        let pt = PageText::build(&g);
        assert_eq!(pt.raw, pt.normalized);
        assert_eq!(pt.glyph_range_for_norm_span(0, 4), Some((0, 3)));
    }
}
