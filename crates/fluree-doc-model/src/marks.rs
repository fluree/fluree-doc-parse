//! Note marks: the raised letter, number or symbol that sends a reader to a
//! note, told apart from raised text that belongs to its word.

/// Endings an ordinal sets raised after its number: `1st`, `2e`, `1er`,
/// `3º`, and after a Roman numeral: `XIXe`.
const ORDINALS: &[&str] = &[
    "st", "nd", "rd", "th", "e", "er", "re", "ère", "eme", "ème", "o", "a", "º", "ª",
];

/// Is `mark`, set raised right after `text_before`, a note's mark rather
/// than part of the word?
///
/// Read into the word, a note's mark changes it: `April 2026` revised
/// becomes `April 2026r`, and a footnoted `Acrylonitrile` becomes
/// `Acrylonitrilea`. Raised text that belongs to the word stays in it: an
/// exponent or a unit's power (`10⁶`, `m²`, `2ᵖ`), an ordinal's ending (`1st`,
/// `1er`, `XIXe`) and a formula's indices (`ρwLv²`, `= cₚhᵖ`). So a mark is a
/// symbol (`*`, `†`), letters after a word or after a value they are not the
/// ordinal of, or a number after a word too long to be a unit (`Revenue¹`,
/// not `km²`).
pub fn note_mark(text_before: &str, mark: &str) -> bool {
    let mark = mark.trim();
    let Some(before) = text_before.chars().last().filter(|c| !c.is_whitespace()) else {
        return false;
    };
    if mark.is_empty() || mark.chars().count() > 4 {
        return false;
    }
    let word = text_before.rsplit(char::is_whitespace).next().unwrap_or("");
    // A term of a formula: what is raised after it is a power or an index
    // (`= ρwLv²`, `+ cₚhᵖ`).
    let ahead = text_before[..text_before.len() - word.len()].trim_end();
    if ahead.ends_with(['=', '+', '×', '<', '>', '≤', '≥', '±', '≈', '∝']) {
        return false;
    }
    // A formula's terms: Greek beside Latin in one run (`ρwLv²`), or a
    // mathematical sign or letter (`Vol(∂Ω)ⁿ`).
    let greek = word.chars().any(|c| ('\u{0370}'..='\u{03FF}').contains(&c));
    let math = word.chars().any(|c| {
        ('\u{2100}'..='\u{214F}').contains(&c)
            || ('\u{2200}'..='\u{22FF}').contains(&c)
            || ('\u{1D400}'..='\u{1D7FF}').contains(&c)
    });
    if math || (greek && word.chars().any(|c| c.is_ascii_alphabetic())) {
        return false;
    }
    // After a lone letter, a raised letter or sign is notation (`eˣ`, `r*`).
    let letters = word.chars().filter(|c| c.is_alphabetic()).count();
    let closed = matches!(before, ')' | ']' | '.' | ',' | ';' | ':');
    if mark.chars().all(|c| "*†‡§¶#".contains(c)) {
        return letters != 1 || closed;
    }
    // A note's letters: one, or a list of them (`a`, `e,f`).
    let lettered = mark
        .split(',')
        .all(|p| !p.is_empty() && p.chars().all(char::is_alphabetic));
    if lettered {
        let ending = mark.to_lowercase();
        let ordinal = !mark.contains(',') && ORDINALS.contains(&ending.as_str());
        if before.is_ascii_digit() {
            // After a small whole number a raised letter is its power (`2ᵖ`,
            // `10ⁿ`); after a value, a note (`2026ʳ`, `1,234ᵃ`, `110ᵉ`).
            let number: String = word
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || matches!(c, ',' | '.'))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let digits = number.chars().filter(char::is_ascii_digit).count();
            let parted = number.contains([',', '.']);
            // Only a whole number takes an ordinal's ending, and one it
            // agrees with.
            let ordinal = ordinal && !parted && ordinal_of(&number, &ending);
            // A list of notes is no power, after any number (`66ᵉ,ʰ`).
            let list = mark.contains(',');
            return (digits >= 3 || parted || list) && !ordinal;
        }
        let roman = word.chars().count() >= 2 && word.chars().all(|c| "IVXLC".contains(c));
        return !(ordinal && roman) && (letters >= 2 || closed);
    }
    mark.chars().all(|c| c.is_ascii_digit() || c == ',')
        && word.chars().all(char::is_alphabetic)
        && word.chars().count() >= 4
}

/// Whether `ending` is the ordinal ending of the whole number `number`.
/// English endings agree with the number they follow (`1st`, `22nd`,
/// `113th`, `118th`); the other languages' follow a number of one or two
/// digits (`2e`, `21e`, `3º`), so a value's raised note (`110ᵉ`) is no
/// ordinal.
fn ordinal_of(number: &str, ending: &str) -> bool {
    let Ok(n) = number.parse::<u64>() else {
        return false;
    };
    match ending {
        "st" | "nd" | "rd" | "th" => {
            let agrees = match (n % 100, n % 10) {
                (11..=13, _) => "th",
                (_, 1) => "st",
                (_, 2) => "nd",
                (_, 3) => "rd",
                _ => "th",
            };
            ending == agrees
        }
        _ => number.len() <= 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_after_a_word_is_a_note() {
        assert!(note_mark("Acrylonitrile", "a"));
        assert!(note_mark("(vinylidene chloride)", "a"));
        assert!(note_mark("Revenue", "1"));
        assert!(note_mark("Total", "*"));
        assert!(note_mark("April 2026", "r"));
        assert!(note_mark("Lee", "†"));
        assert!(note_mark("1,234", "a"));
        assert!(note_mark("4.1", "b"));
        assert!(note_mark("110", "e"));
        assert!(note_mark("610", "e,f"));
        assert!(note_mark("66", "e,h"));
        assert!(note_mark("Benzene", "a,b"));
    }

    #[test]
    fn raised_text_of_the_word_is_no_note() {
        assert!(!note_mark("km", "2"));
        assert!(!note_mark("10", "6"));
        assert!(!note_mark("1", "st"));
        assert!(!note_mark("le 1", "er"));
        assert!(!note_mark("le XIX", "e"));
        assert!(!note_mark("the 118", "th"));
        assert!(!note_mark("le 21", "e"));
        assert!(!note_mark("", "a"));
        assert!(!note_mark("word ", "a"));
        assert!(!note_mark("e", "x"));
        assert!(!note_mark("where a", "n"));
        assert!(!note_mark("the rate r", "*"));
        assert!(!note_mark("Re = ρwLv", "2"));
        assert!(!note_mark("x + cph", "p"));
        assert!(!note_mark("ρwLv", "2"));
        assert!(!note_mark("V ol(∂Ω)", "n"));
        assert!(!note_mark("multiply by 2", "p"));
        assert!(!note_mark("10", "n"));
    }
}
