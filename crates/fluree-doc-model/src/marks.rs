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
        letterlike(c)
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
    if !mark.chars().all(|c| c.is_ascii_digit() || c == ',') || unit_power(text_before, mark) {
        return false;
    }
    // A number after a word too long to be a unit (`Revenue¹`, not `km²`).
    if word.chars().all(char::is_alphabetic) && letters >= 4 {
        return true;
    }
    // After a compound or a code joined by hyphens, a note's number:
    // `3-way¹`, `Multi-location⁴`, `MACL-153M-XX²,³`. Not after a unit of
    // length it ends with (`5-mm²`).
    // A part of two letters or more: `x-y²` is a formula's terms.
    let joined = word.trim_end_matches(|c: char| !c.is_alphanumeric());
    if joined.contains('-')
        && joined
            .split('-')
            .any(|p| p.chars().filter(|c| c.is_alphabetic()).count() >= 2)
        && joined.ends_with(char::is_alphanumeric)
        && !length(trailing_letters(joined))
    {
        return true;
    }
    // After a quantity, a note's number (`26TB¹`, `26 TB¹`). A lone letter
    // after a number is a variable or a unit, and what is raised after it a
    // power (`5x²`, `10 m²`); so is what is raised after a length, whose
    // sign a font may draw without text (`0.51 mm⁻¹` read `mm1`).
    let unit = trailing_letters(word);
    let figures =
        word[..word.len() - unit.len()].trim_start_matches(|c: char| !c.is_alphanumeric());
    let glued = !figures.is_empty()
        && figures.ends_with(|c: char| c.is_ascii_digit())
        && figures
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ',' | '.'));
    let spaced = unit.len() == word.len() && ahead.ends_with(|c: char| c.is_ascii_digit());
    if (glued || spaced) && unit.chars().count() >= 2 {
        return !length(unit);
    }
    // After a bracket that holds words, a note's number (`(49 N)⁵`), unless
    // the bracket closes a formula's term (`(a+b)²`, `weight/(height)²`) or
    // a function's argument (`d(m)²`). A bracket opened on an earlier line
    // may be either.
    if !matches!(before, ')' | ']') {
        return false;
    }
    let Some((open, held)) = bracketed(text_before) else {
        return false;
    };
    let lead = &text_before[..open];
    let function = lead.ends_with(char::is_alphabetic) && !held.contains(char::is_whitespace);
    let operand = lead
        .trim_end()
        .chars()
        .last()
        .is_some_and(|c| formula(c.encode_utf8(&mut [0; 4])));
    held.contains(char::is_alphanumeric) && !function && !operand && !formula(held)
}

/// Is `mark`, set raised right after `text_before`, the square or cube of
/// the unit of length the text ends with? `m²`, `26m²`, `5 cm³`, and the
/// Spanish square foot in `1000 PIES²` and `LB/PIES²`. Or the square of
/// the second an acceleration is per: `m/s²`.
///
/// A note's number after a unit's name would read the same, but a page
/// squares a length far more often than it footnotes one.
pub fn unit_power(text_before: &str, mark: &str) -> bool {
    let mark = mark.trim();
    if !matches!(mark, "2" | "3") {
        return false;
    }
    let unit = trailing_letters(text_before);
    let per = text_before[..text_before.len() - unit.len()].ends_with('/');
    length(unit) || (per && mark == "2" && matches!(unit, "s" | "sec"))
}

/// Is `unit` a unit of length? A symbol is case-sensitive: `M` is mega and
/// `nm` a nanometre, not `NM`.
fn length(unit: &str) -> bool {
    SYMBOLS.contains(&unit) || LENGTHS.contains(&unit.to_lowercase().as_str())
}

/// Unit symbols of length, as written.
const SYMBOLS: &[&str] = &["m", "dm", "nm", "µm", "μm"];

/// A mathematical letter-like symbol (`ℝ`, `ℏ`, `ℓ`), not one of the
/// block's marks and units: a trade mark (`™`, `℠`), a number sign (`№`), a
/// temperature (`℃`, `℉`) or a care-of (`℅`), which a name or a value carries.
fn letterlike(c: char) -> bool {
    ('\u{2100}'..='\u{214F}').contains(&c) && !matches!(c, '™' | '℠' | '№' | '℃' | '℉' | '℅')
}

/// Units of length in any case: symbols that keep their sense in capitals,
/// and the units' names in English, Spanish and French.
const LENGTHS: &[&str] = &[
    "mm", "cm", "km", "in", "ft", "yd", "mi", "inch", "inches", "foot", "feet", "yard", "yards",
    "mile", "miles", "metre", "metres", "meter", "meters", "pie", "pies", "pulg", "pulgada",
    "pulgadas", "metro", "metros", "yarda", "yardas", "milla", "millas", "pied", "pieds", "pouce",
    "pouces", "mètre", "mètres",
];

/// The letters `text` ends with.
fn trailing_letters(text: &str) -> &str {
    let start = text
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphabetic())
        .last()
        .map_or(text.len(), |(i, _)| i);
    &text[start..]
}

/// Where the bracket `text` ends with opens, and what it holds, when it
/// opens in `text`.
fn bracketed(text: &str) -> Option<(usize, &str)> {
    let mut depth = 0usize;
    for (i, c) in text.char_indices().rev() {
        match c {
            ')' | ']' => depth += 1,
            '(' | '[' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((i, &text[i + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// Does the bracketed `text` hold a formula: an operator, a mathematical
/// letter or sign, or a minus beside a variable (`x-1`, `1-x`)? A hyphen in
/// a range or a compound is no operator (`2023-24`, `non-GAAP`).
fn formula(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let letters_beside = |from: usize, step: isize| {
        let mut n = 0;
        let mut i = from as isize + step;
        while i >= 0 && (i as usize) < chars.len() && chars[i as usize].is_alphabetic() {
            n += 1;
            i += step;
        }
        n
    };
    // The run of letters and figures on one side of `i`.
    let term = |from: usize, step: isize| {
        let mut n = 0;
        let mut letters = 0;
        let mut i = from as isize + step;
        while i >= 0 && (i as usize) < chars.len() && chars[i as usize].is_alphanumeric() {
            n += 1;
            letters += usize::from(chars[i as usize].is_alphabetic());
            i += step;
        }
        (n, letters)
    };
    // A slash between names or codes parts them, as `mm/in` and `512e/4Kn`
    // do: an operator's slash stands by a variable or a figure (`a/b`, `x/2`).
    let names = |i: usize| {
        let (before, after) = (term(i, -1), term(i, 1));
        before.0 >= 2 && before.1 >= 1 && after.0 >= 2 && after.1 >= 1
    };
    chars.iter().enumerate().any(|(i, &c)| match c {
        '+' | '=' | '×' | '·' | '*' | '^' | '±' | '−' | '<' | '>' | '≤' | '≥' => true,
        '/' => !names(i),
        '-' => letters_beside(i, -1) == 1 || letters_beside(i, 1) == 1,
        _ => {
            ('\u{0370}'..='\u{03FF}').contains(&c)
                || letterlike(c)
                || ('\u{2200}'..='\u{22FF}').contains(&c)
                || ('\u{1D400}'..='\u{1D7FF}').contains(&c)
        }
    })
}

/// Is `mark`, set raised right after `text_before` and with `next` the
/// first character after it that is not whitespace, a degree sign set as a
/// raised letter `o`, figure `0` or ordinal `º`?
///
/// Word sets a degree this way (`~ 145°`, `25°C`), and read as printed the
/// angle became `145 o`, a note's letter, and the temperature `25oC`. It is
/// a degree wherever it cannot be an ordinal's ending or a power: before a
/// temperature's unit, after three figures or more, after a decimal, or
/// after a signed number. `1º` and `2º` stay ordinals and `10⁰` a power. A
/// comma is no decimal here: in `4,4′-dibromo` the raised figure is a
/// prime a font set as `0`.
pub fn degree(text_before: &str, mark: &str, next: Option<char>) -> bool {
    if !matches!(mark.trim(), "o" | "0" | "º") {
        return false;
    }
    let number = text_before
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_digit() || *c == '.')
        .last()
        .map_or("", |(i, _)| &text_before[i..])
        .trim_start_matches('.');
    if !number.ends_with(|c: char| c.is_ascii_digit()) {
        return false;
    }
    let ahead = &text_before[..text_before.len() - number.len()];
    let signed = ahead.ends_with(['−', '±'])
        || (ahead.ends_with(['-', '+'])
            && !ahead[..ahead.len() - 1].ends_with(|c: char| c.is_alphanumeric()));
    let figures = number.chars().filter(char::is_ascii_digit).count();
    // After an approximation's sign or a bracket's opening, a raised `o` is
    // no ordinal: no text counts `~ 95th` or `(45th)`. An angle's degree
    // (`Beam Angle ~ 95°`, `Gloss (45°)`). Not a raised `0`, which may be
    // a power there (`(10⁰)`).
    let set_apart = mark.trim() != "0"
        && ahead.trim_end().ends_with(['~', '≈', '∼', '(', '['])
        && !ahead.ends_with(|c: char| c.is_alphanumeric());
    matches!(next, Some('C' | 'F')) || figures >= 3 || number.contains('.') || signed || set_apart
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

    #[test]
    fn a_number_after_a_quantity_or_a_bracket_is_a_note() {
        assert!(note_mark("capacities up to 26TB", "1"));
        assert!(note_mark("up to 550TB", "2"));
        assert!(note_mark("up to 26 TB", "1"));
        assert!(note_mark("11 lbf (49 N)", "5"));
        assert!(note_mark("Revenue (2023-24)", "1"));
        assert!(note_mark("Adjusted (non-GAAP)", "3"));
        // A formula's bracket, or a variable after a number, takes a power.
        assert!(!note_mark("(a+b)", "2"));
        assert!(!note_mark("(x-1)", "2"));
        assert!(!note_mark("5x", "2"));
        assert!(!note_mark("10 m", "2"));
        assert!(!note_mark("distance, d(m)", "2"));
        assert!(!note_mark("BMI = weight/ (height)", "2"));
        assert!(!note_mark("µ = 0.51 mm", "1"));
        assert!(!note_mark("(𝑁𝑁 + 1)", "2"));
        // Nothing to tell from: a bracket opened on an earlier line.
        assert!(!note_mark(")", "2"));
        assert!(!note_mark("as amended)", "5"));
    }

    #[test]
    fn a_number_after_a_compound_a_code_or_names_in_a_bracket_is_a_note() {
        assert!(note_mark("Single-pole / 3-way", "1"));
        assert!(note_mark("Multi-location", "4"));
        assert!(note_mark("MACL-153M-XX", "2,3"));
        assert!(note_mark("FastFormat (512e/4Kn)", "1,2"));
        assert!(note_mark("FastFormat™ (512e/4Kn)", "1"));
        assert!(note_mark("Depth (mm/in, max)", "4"));
        // A formula's terms, a slash by a variable or a figure, and a unit
        // of length after a hyphen still take a power.
        assert!(!note_mark("x-y", "2"));
        assert!(!note_mark("(a/b)", "2"));
        assert!(!note_mark("(x/2)", "2"));
        assert!(!note_mark("a 5-mm", "2"));
    }

    #[test]
    fn a_unit_of_length_takes_a_power() {
        for before in [
            "BOLSAS POR CADA 1000 PIES",
            "COBERTURA NETA MÁXIMA, PIES",
            "PESO MÍNIMO/ PIES",
            "LB/PIES",
            "5 cm",
            "26m",
            "AREA, FT",
            "g (m/s",
            "ft/sec",
        ] {
            assert!(unit_power(before, "2"), "{before}");
            assert!(!note_mark(before, "2"), "{before}");
        }
        assert!(unit_power("5 cm", "3"));
        // Only a square or a cube, and only of a length.
        assert!(!unit_power("5 cm", "1"));
        assert!(!unit_power("26TB", "2"));
        assert!(!unit_power("$26M", "2"));
        assert!(!unit_power("Pastries", "2"));
        assert!(!unit_power("10 s", "2"));
        assert!(!unit_power("m/s", "3"));
        assert!(note_mark("Revenue", "2"));
    }

    #[test]
    fn a_raised_o_or_zero_that_cannot_be_an_ordinal_is_a_degree() {
        assert!(degree("angle: ~ 145", "o", None));
        assert!(degree("5", "0", Some('C')));
        assert!(degree("25", "o", Some('C')));
        assert!(degree("77", "º", Some('F')));
        assert!(degree("tilt 1.5", "o", None));
        assert!(degree("from -25", "o", Some('t')));
        assert!(degree("122", "o", Some('W')));
        // An ordinal stays one, and a power one.
        assert!(!degree("el 1", "o", None));
        assert!(!degree("2", "º", Some('l')));
        assert!(!degree("10", "0", None));
        assert!(!degree("10-15", "o", None));
        assert!(!degree("145", "a", None));
        assert!(!degree("No", "o", None));
        // A prime set as a raised `0`, after a list of positions.
        assert!(!degree("4,4", "0", Some('-')));
        assert!(!degree("(upstream),5", "0", Some('(')));
        // After an approximation's sign or a bracket's opening, an angle.
        assert!(degree("Beam Angle ~ 95", "o", None));
        assert!(degree("Gloss (45", "o", Some(')')));
        assert!(degree("≈ 30", "º", None));
        // A raised `0` there may be a power; a word before is no sign.
        assert!(!degree("(10", "0", Some(')')));
        assert!(!degree("el 2", "o", None));
    }
}
