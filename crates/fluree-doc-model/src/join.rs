//! How a line, or a paragraph's part, joins the text after it.
//!
//! A line ending on a hyphen, a closed dash or a web address broken at a
//! slash runs on into the next line with no space, and a hyphen a
//! typesetter set inside a word is dropped. The same rules join a
//! paragraph's two parts where a page break cut it: the source's reader
//! links the parts ([`Element::continues`]) and the writers join them, so
//! a page break and a line break read alike.

use crate::element::Element;

/// The mark a paragraph's part ends on where the page break fell inside a
/// word and the word's hyphen was the typesetter's (`re-` over
/// `strictive`): a soft hyphen, which no glyph's text carries. The writers
/// drop it and join the parts with nothing between, so the word reads
/// whole; a part ending on a hyphen it keeps (`long-` over `term`) holds
/// the hyphen itself.
pub const PART_BREAK: char = '\u{AD}';

/// Whether a line ending in a dash set closed, or in a web address broken
/// at a slash, runs on into the next with no space: `restore—` over `D2D`,
/// `10–` over `20`, `…TechnicalDocumentLibrary/` over its file's name.
///
/// A dash set closed against the word before it joins that word to the
/// next, as the line's own hyphen does; one with a space before it was set
/// apart, and keeps its space. A slash ending a word may have had a space
/// after it on the line the break took (`fuego/ desprendimiento`), but an
/// address holds none.
pub fn closed_join(before: &str, next: &str) -> bool {
    let mut tail = before.chars().rev();
    let (Some(mark), Some(prev)) = (tail.next(), tail.next()) else {
        return false;
    };
    if prev.is_whitespace() || !next.chars().next().is_some_and(|c| !c.is_whitespace()) {
        return false;
    }
    match mark {
        '—' | '–' => true,
        '/' => {
            let token = before.rsplit(char::is_whitespace).next().unwrap_or("");
            token.contains("://") || token.to_ascii_lowercase().starts_with("www.")
        }
        _ => false,
    }
}

/// What a hyphen ending a line is, and so how the line joins the next.
#[derive(Debug, PartialEq)]
pub enum HyphenJoin {
    /// A break wrapping set inside a word (`microcom-` over `puter`):
    /// dropped, and the word read whole.
    Word,
    /// The text's own, in a number, a code, a compound or a range
    /// (`1-800-222-` over `1222`, `Polish-` over `German`, `(A)-` over
    /// `(L)`): kept, the lines joined without a space.
    Hyphen,
    /// A dash, set apart from the word before it (`a pause -`), or a
    /// compound's first half left hanging for the second it shares
    /// (`wage-` over `and price-setting`): kept, the lines joined with a
    /// space.
    Dash,
}

impl HyphenJoin {
    /// Read from the text before the hyphen and the next line.
    ///
    /// A typesetter breaks a word between two letters of one case, and
    /// leaves at least two on each side. Anything else at the end of a line
    /// was printed there: a figure beside it, a capital after a lowercase
    /// letter or before one (`Fly-Casting`, `R-warfarin`), a single letter
    /// (`e-mail`, `X-Forwarded`). An all-capital compound (`BY-PRODUCTS`)
    /// cannot be told from an all-capital word broken (`SO-LAR`), and is
    /// read as the word.
    pub fn of(stem: &str, next: &str) -> HyphenJoin {
        let Some(before) = stem.chars().next_back().filter(|c| !c.is_whitespace()) else {
            return HyphenJoin::Dash;
        };
        let Some(after) = next.chars().next() else {
            return HyphenJoin::Hyphen;
        };
        let left = stem.chars().rev().take_while(|c| c.is_alphabetic()).count();
        let right = next.chars().take_while(|c| c.is_alphabetic()).count();
        // A conjunction after the hyphen joins two compounds that share
        // their second half: no typesetter's break leaves one of these
        // words whole on the next line.
        let first = next
            .split(|c: char| !c.is_alphabetic())
            .next()
            .unwrap_or("");
        if left >= 2 && matches!(first, "and" | "or" | "nor") {
            return HyphenJoin::Dash;
        }
        let one_case = (before.is_lowercase() && after.is_lowercase())
            || (before.is_uppercase() && after.is_uppercase());
        if one_case && left >= 2 && right >= 2 {
            HyphenJoin::Word
        } else {
            HyphenJoin::Hyphen
        }
    }
}

/// Whether the element at `i` carries on the paragraph just before it: a
/// paragraph a page break cut, the later part linked to the earlier by
/// [`Element::continues`]. The writers write the two as one paragraph.
pub fn carries_on_paragraph(elements: &[Element], i: usize) -> bool {
    let (Some(prev), Some(e)) = (i.checked_sub(1).map(|p| &elements[p]), elements.get(i)) else {
        return false;
    };
    continues_paragraph(prev, e)
}

/// Whether `e` carries on the paragraph `prev`, set just before it.
pub fn continues_paragraph(prev: &Element, e: &Element) -> bool {
    let plain = |e: &Element| e.kind == "doco:Paragraph" && e.turn.is_none() && e.message.is_none();
    plain(prev) && plain(e) && e.continues.as_deref() == Some(prev.id.as_str())
}

/// What goes between a paragraph's part and the part that carries it on,
/// from their texts.
///
/// Nothing where the earlier part ends on [`PART_BREAK`] (the writers
/// drop the mark), or on a hyphen the text keeps, as a line's end joins
/// the next ([`HyphenJoin`]: `long-` over `term`); a space after a dash set
/// apart (`a pause -`) or a compound's first half left hanging (`wage-`
/// over `and price-setting`). Nothing after a closed dash or a broken web
/// address ([`closed_join`]). A space otherwise.
pub fn part_separator(before: &str, next: &str) -> &'static str {
    let before = before.trim_end_matches(|c: char| c.is_whitespace() && c != PART_BREAK);
    let next = next.trim_start();
    if before.ends_with(PART_BREAK) {
        return "";
    }
    if let Some(stem) = before.strip_suffix('-') {
        return match HyphenJoin::of(stem, next) {
            HyphenJoin::Dash => " ",
            HyphenJoin::Word | HyphenJoin::Hyphen => "",
        };
    }
    if closed_join(before, next) {
        ""
    } else {
        " "
    }
}

/// A paragraph's text as written: without the [`PART_BREAK`] its end may
/// carry, which only says how it joins the part after it.
pub fn part_text(text: &str) -> &str {
    text.strip_suffix(PART_BREAK).unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(id: &str, text: &str, continues: Option<&str>) -> Element {
        Element {
            id: id.into(),
            continues: continues.map(Into::into),
            ..Element::new("doco:Paragraph", text, "rust", "layout")
        }
    }

    #[test]
    fn a_word_broken_at_the_page_reads_whole() {
        // The typesetter's hyphen, set down as the break mark: dropped.
        let before = format!("hold rates at a re{PART_BREAK}");
        assert_eq!(part_separator(&before, "strictive level"), "");
        assert_eq!(part_text(&before), "hold rates at a re");
    }

    #[test]
    fn a_compounds_own_hyphen_stays_and_joins_closed() {
        assert_eq!(part_separator("a long-", "term view"), "");
        assert_eq!(part_text("a long-"), "a long-");
        // A number's or a code's hyphen too.
        assert_eq!(part_separator("call 1-800-", "222-1222"), "");
    }

    #[test]
    fn a_dash_set_apart_and_a_word_join_with_a_space() {
        assert_eq!(part_separator("a pause -", "then more"), " ");
        assert_eq!(part_separator("wage-", "and price-setting"), " ");
        assert_eq!(part_separator("economic decisions and", "can affect"), " ");
        assert_eq!(part_separator("in turn,", "the rate"), " ");
        // A closed dash joins as a line's end does.
        assert_eq!(part_separator("from 10–", "20 percent"), "");
    }

    #[test]
    fn only_a_linked_plain_paragraph_carries_on() {
        let a = para("e1", "economic decisions and", None);
        let b = para("e2", "can affect the persistence of inflation.", Some("e1"));
        assert!(carries_on_paragraph(&[a.clone(), b.clone()], 1));
        // Not linked: two paragraphs.
        let c = para("e2", "can affect", None);
        assert!(!carries_on_paragraph(&[a.clone(), c], 1));
        // Linked to something else.
        let d = para("e2", "can affect", Some("e9"));
        assert!(!carries_on_paragraph(&[a.clone(), d], 1));
        // A table part's link is the table's.
        let mut t = b.clone();
        t.kind = "doco:Table".into();
        assert!(!carries_on_paragraph(&[a, t], 1));
    }
}
