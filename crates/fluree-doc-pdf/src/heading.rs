//! Heading detection and levelling.
//!
//! Three independent signals, combined rather than ranked:
//!
//! 1. **The PDF outline tree** ([`crate::outline`]) — author-provided and
//!    explicitly hierarchical. Where a block matches an outline title, its
//!    level is taken directly. This is the signal no benchmarked engine uses.
//! 2. **Numbering** — `5.3.1` states its own depth. Independent of typography,
//!    so it works where font sizes are uniform.
//! 3. **Font size relative to body** — the classic signal, and the only one
//!    available on documents with neither outline nor numbering. Plenty of
//!    real reports carry no outline at all, so this path cannot be dropped.
//!
//! Deliberately *not* a weighted score summed to a threshold. Each signal is
//! strong enough alone to be worth acting on, and a sum lets two weak signals
//! outvote one authoritative one — the outline should not be overridden by a
//! font being 0.5pt large.

use crate::block::{self, Block};
use crate::outline::OutlineItem;
use std::collections::{BTreeMap, HashMap};

/// A block must exceed the body font size by this fraction to be a heading on
/// typography alone.
///
/// Typography is the *weakest* of the three signals and needs the tightest
/// guards. On one datasheet the modal size is 8pt (dense table text) while
/// body prose runs 9-10pt, so a bare "larger than modal" test promoted 320
/// paragraph fragments to headings. Font size only counts alongside the length
/// and line-count limits below.
pub(crate) const HEADING_SIZE_MARGIN: f64 = 0.15;

/// Shortest word that can end a sentence inside a heading candidate. Below this
/// the dot is abbreviating (`vs.`, `Sec.`, `Fig.`), not terminating.
const MIN_SENTENCE_WORD_CHARS: usize = 4;

/// Most bold heading candidates a page may hold before bold is judged to be
/// doing some other job on this document. Sections are coarse: a page with more
/// than a handful of them is a page of labels.
const MAX_BOLD_HEADINGS_PER_PAGE: usize = 10;

/// Most bare-number section candidates a page may hold before the numbering
/// on it is judged to be a coding scheme rather than an outline. Sections are
/// coarse — a page holds a few — where a questionnaire answers every item
/// with a code, `0 — Never`, `2 — Somewhat agree`, twenty-odd to the page. The repeated-text check below catches an answer
/// given more than once; an answer given once looks exactly like a section,
/// and only the company it keeps tells them apart.
const MAX_BARE_NUMBERED_PER_PAGE: usize = 10;

/// Headings are short. Applies to every signal.
const MAX_HEADING_CHARS: usize = 200;

/// Headings are short in *words*, not only characters.
const MAX_HEADING_WORDS: usize = 12;

/// Most words of a heading that more than its shape vouches for
/// ([`vouched_for`]). `3. Perspective of supply and demand
/// balance of wood pellets and cost structure in Japan` is 15 words.
pub(crate) const MAX_LONG_HEADING_WORDS: usize = 20;
const MAX_LONG_HEADING_CHARS: usize = 160;

/// Size, against the body, of type set clearly larger than it.
const DISPLAY_HEADING_RATIO: f64 = 1.5;

/// Size, against the body, of a numbered line set larger than it.
const NUMBERED_HEADING_RATIO: f64 = 1.1;

/// Tighter limits for the typography-only path, where there is no outline or
/// numbering to corroborate.
const MAX_FONTSIZE_HEADING_CHARS: usize = 90;
const MAX_FONTSIZE_HEADING_LINES: usize = 2;

/// Deepest level we emit, matching `h1`-`h6`.
const MAX_LEVEL: usize = 6;

/// Largest value a single section number may take. Section numbering is small;
/// a bare four-digit number opening a sentence is a year or a quantity, and
/// "2026 was a difficult year…" must not become a level-1 heading.
const MAX_SECTION_NUMBER: u32 = 99;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// Matched a PDF outline entry — the strongest signal available.
    Outline,
    /// Prominent, isolated text near the start of the first page.
    Title,
    /// Section numbering stated the depth.
    Numbering,
    /// A display label immediately precedes a lettered structural item.
    Sequence,
    /// Set larger than body text.
    FontSize,
    /// Bold at body size — the most common heading cue after size.
    Bold,
}

#[derive(Debug, Clone)]
pub struct Heading {
    pub page: usize,
    /// Index of the source block within its page.
    ///
    /// Text is not an identity: a chart or form can repeat the same label
    /// several times on one page, and only one occurrence may be a heading.
    /// Keeping the block address prevents the emission pass from promoting
    /// every duplicate because one copy matched.
    pub block_index: usize,
    pub text: String,
    pub level: usize,
    pub evidence: Evidence,
    /// Set like a heading, but a form's, a field's or a chart's text, or a
    /// line that runs on into the next: `(a) (b)`, `Yes No`, `b Refunded
    /// 37b`, a label over its value, an axis's title. Kept a heading until runs of bold heads have been
    /// judged, as they were before these were told apart, and read as text
    /// after: dropping one from a run would leave its neighbours standing.
    pub form_text: bool,
    style: Style,
}

/// Normalize for matching outline titles against block text: outlines often
/// differ from the rendered heading in whitespace and case, and sometimes omit
/// the section number.
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Whether a block's text is a title the outline declares, as the outline
/// match in [`detect`] reads one.
pub fn outline_names(outline: &[OutlineItem]) -> impl Fn(&str) -> bool {
    let titles: std::collections::HashSet<String> = outline
        .iter()
        .map(|it| norm(&it.title))
        .filter(|k| !k.is_empty())
        .collect();
    move |text: &str| titles.contains(&norm(text))
}

/// Whether a heading other than the document's title comes before block
/// `block` of page `page`: the block is inside a section of the document,
/// not straight under its title.
pub fn in_section(headings: &[Heading]) -> impl Fn(usize, usize) -> bool {
    let mut at: Vec<(usize, usize, Evidence)> = headings
        .iter()
        .map(|h| (h.page, h.block_index, h.evidence))
        .collect();
    at.sort_by_key(|&(p, b, _)| (p, b));
    move |page: usize, block: usize| {
        let before = at.partition_point(|&(p, b, _)| (p, b) < (page, block));
        before > 0 && at[before - 1].2 != Evidence::Title
    }
}

/// Parse a leading section number and return its depth: `5.` → 1,
/// `5.3` → 2, `6.3.1` → 3. Returns `None` when the block does not start with
/// one, so ordinary prose beginning with a figure is not mistaken for a heading.
fn numbering_depth(text: &str) -> Option<usize> {
    // Separator between the label and the title. `01 - Find Open Educational
    // Resources` and `02- Prepare Your Content` both occur in the corpus and
    // neither was parsed when only whitespace was accepted.
    let split_label = |t: &str| -> Option<(String, String)> {
        let head: String = t
            .chars()
            .take_while(|c| c.is_ascii_digit() || c.is_ascii_alphabetic() || *c == '.')
            .collect();
        if head.is_empty() {
            return None;
        }
        let rest =
            t[head.len()..].trim_start_matches([' ', '\t', '-', '\u{2013}', '\u{2014}', ')']);
        let rest = rest.trim_start();
        Some((head, rest.to_string()))
    };

    // A date opens on its day, not a section's number: `05 Juillet 2015`.
    if date_text(text) {
        return None;
    }
    let (head, rest) = split_label(text)?;
    if rest.is_empty() || !rest.chars().next().is_some_and(|c| c.is_alphabetic()) {
        return None;
    }
    // A number hyphenated to the word after it is part of that word, not a
    // label: `3-Way Wiring`, `2-Pole`, a drawing's `4-M4` hole callout. The
    // dash separators above are set with a space after them.
    let mut after = text[head.len()..].chars();
    if matches!(after.next(), Some('-' | '\u{2010}' | '\u{2011}'))
        && after.next().is_some_and(|c| !c.is_whitespace())
    {
        return None;
    }
    // Section labels introduce titles, not sentence continuations. Applying
    // this to every numbering form (not only bare integers) rejects OCR
    // fragments such as `III. his report defines` and chart prose such as
    // `2.6 times that of`.
    if !rest
        .chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(char::is_uppercase)
    {
        return None;
    }
    // Long numbered lines ending in a full stop are overwhelmingly citations,
    // instructions, or prose (`6. Compared to 38% ...`). Genuine numbered
    // headings in the measured corpus may contain abbreviations, but do not
    // end as complete multi-word sentences.
    if rest.ends_with('.') && rest.split_whitespace().count() >= 4 {
        return None;
    }
    let parts: Vec<&str> = head
        .trim_end_matches('.')
        .split('.')
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() || parts.len() > MAX_LEVEL {
        return None;
    }
    // Initials, not a section number: `U.S.A. FOODSERVICE DIVISION`, `U.S.
    // Department of…`. Lettered numbering sets a letter at its first level
    // only (`B.1`, `A.2.3`), so a label of single letters alone is an
    // abbreviation, unless it opens with a roman numeral an outline counts
    // with (`I.A`, `V.C`).
    if parts.len() >= 2
        && parts.iter().all(|p| single_letter(p))
        && !matches!(parts[0], "I" | "V" | "X")
    {
        return None;
    }

    // Each component is a decimal section number, a roman numeral, or a single
    // letter (appendix style: `B.1 Large Language Models`). Mixed forms are
    // fine — `B.1` is a letter then a number.
    let mut saw_alpha = false;
    for p in &parts {
        if let Ok(v) = p.parse::<u32>() {
            if v > MAX_SECTION_NUMBER {
                return None;
            }
            continue;
        }
        if is_roman(p) || single_letter(p) {
            saw_alpha = true;
            continue;
        }
        return None;
    }

    // A single bare number is ambiguous: "7 Variants of SJ Observer Models" is
    // a section heading, "3 of the respondents said otherwise" is prose. The
    // discriminator is what follows — a heading continues in title case, prose
    // in lower case.
    if parts.len() == 1 && !head.contains('.') && !saw_alpha {
        let first_word = rest.split_whitespace().next().unwrap_or("");
        if !first_word
            .chars()
            .find(|c| c.is_alphabetic())
            .is_some_and(|c| c.is_uppercase())
        {
            return None;
        }
    }
    // A bare single letter with no dot is far too weak — "A test of the system"
    // would qualify. Require the dot form for lettered sections.
    if parts.len() == 1 && saw_alpha && !head.contains('.') {
        return None;
    }
    Some(parts.len())
}

fn single_letter(p: &str) -> bool {
    p.len() == 1 && p.chars().all(|c| c.is_ascii_uppercase())
}

/// The letter a block opens with as a label, `B.` followed by a space.
fn letter_label(text: &str) -> Option<char> {
    let mut head = text.split_whitespace().next()?.chars();
    match (head.next(), head.next(), head.next()) {
        (Some(c), Some('.'), None) if c.is_ascii_uppercase() => Some(c),
        _ => None,
    }
}

/// The letter of a lettered section label, `B. Related Works`, or of an
/// initial set the same way, `J. Example`; the caller tells them apart.
fn single_letter_label(text: &str) -> Option<char> {
    letter_label(text).filter(|_| numbering_depth(text).is_some())
}

/// Uppercase roman numeral, used for appendix and front-matter sections
/// (`II. Set Up the Restriction Digests`).
fn is_roman(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 5
        && s.chars()
            .all(|c| matches!(c, 'I' | 'V' | 'X' | 'L' | 'C' | 'D' | 'M'))
}

/// A bare integer (`5 Results`) has less syntactic authority than `5. Results`.
/// Corroborate it with body-sized typography and title-bearing words. This
/// excludes footnote/page labels and mathematical/chart fragments while
/// retaining body-sized section labels used by papers and datasheets.
fn bare_numbering_is_corroborated(text: &str, block: &Block, body_size: f32) -> bool {
    let mut words = text.split_whitespace();
    let Some(label) = words.next() else {
        return false;
    };
    if !label.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    // Sections count from one. A bare zero is a code — `0 — Not started` —
    // or a torn fragment of a time.
    if label.chars().all(|c| c == '0') {
        return false;
    }
    // A number set as a superscript is a footnote's: `95 Ibid.` opens with
    // its reference mark, not a section number.
    if block.lines.first().is_some_and(|l| l.opens_with_script) {
        return false;
    }
    if block.font_size < body_size * 0.95 {
        return false;
    }
    let rest: Vec<&str> = words.collect();
    if rest
        .first()
        .is_some_and(|word| word.chars().any(|c| c.is_ascii_digit()))
    {
        return false;
    }
    rest.iter().any(|word| {
        word.chars()
            .fold((0usize, 0usize), |(run, longest), c| {
                let run = if c.is_alphabetic() { run + 1 } else { 0 };
                (run, longest.max(run))
            })
            .1
            >= 2
    })
}

/// A numbered line whose label is a bare integer — `5 Results`, `2 — Never`
/// — as opposed to `5.` or `5.3`: the weakest numbering form, the one
/// [`bare_numbering_is_corroborated`] exists for.
fn has_bare_number_label(text: &str) -> bool {
    numbering_depth(text) == Some(1)
        && text
            .split_whitespace()
            .next()
            .is_some_and(|w| w.chars().all(|c| c.is_ascii_digit()))
}

/// Whether more than a heading's shape vouches for it, so that it may run
/// longer, on at most two lines: section numbering on a line set larger
/// than the body, or type set clearly larger than it. A line at the body
/// size keeps the ordinary limits, numbered or bold: a long numbered line
/// there is a list's item or a note's (`1. When changing objectives …
/// the following changes will occur:`), and a sentence set bold is no
/// heading.
///
/// And only a title's shape: a long line ending as a sentence ends (`We've
/// cut overall product energy use … since 2008.`, `Want to learn more…?`)
/// or holding a list (`Leticia Morales*1, Beatriz Ricardo 1, Adán Bolaños
/// 1, …`) is no title, and a bare number is a form's question
/// (`12 Did you explain to the taxpayer…`), not a section's label. Nor is a
/// formula (`GFR (mL/min/1.73m²) = 141 x …`) or a bulleted line.
fn vouched_for(text: &str, b: &Block, body: f32) -> bool {
    let size = b.font_size as f64 / (body as f64).max(1.0);
    let t = text.trim();
    let shaped = !t.ends_with(['.', '?', '!'])
        && t.matches(',').count() < 2
        && !t.contains('=')
        && !t
            .chars()
            .next()
            .is_some_and(|c| block::is_bullet(c) || c == '\u{25CB}');
    let labelled = numbering_depth(t).is_some() && !has_bare_number_label(t);
    b.lines.len() <= MAX_FONTSIZE_HEADING_LINES
        && shaped
        && ((labelled && size >= NUMBERED_HEADING_RATIO) || size >= DISPLAY_HEADING_RATIO)
}

/// The most words a block may hold and still be a heading.
fn word_limit(text: &str, b: &Block, body: f32) -> usize {
    if vouched_for(text, b, body) {
        MAX_LONG_HEADING_WORDS
    } else {
        MAX_HEADING_WORDS
    }
}

/// Shape tests every heading candidate must pass, regardless of which signal
/// proposed it.
///
/// Measured precision per source before adding this was FontSize 33.8%,
/// Numbering 33.7%, Bold 51.2% — we emitted 287 headings where the corpus
/// has 193.
///
/// Two commonly used title-shape tests are deliberately *omitted*, because
/// measuring them against the corpus showed they reject real headings:
/// requiring title-case capitalisation loses 12%, and rejecting a trailing full
/// stop loses 9% (`CHAPTER 1.`). The heading metric penalises a missed heading
/// — two sections merge — far more than a spurious one, which merely splits a
/// section.
fn title_like(text: &str) -> bool {
    title_like_within(text, MAX_HEADING_WORDS)
}

/// [`title_like`], with room for `max_words` words.
fn title_like_within(text: &str, max_words: usize) -> bool {
    let t = text.trim();
    let words = t.split_whitespace().count();
    if !(1..=max_words).contains(&words) {
        return false;
    }
    if !(4..=MAX_HEADING_CHARS).contains(&t.chars().count()) {
        return false;
    }
    if !t.chars().any(|c| c.is_alphabetic()) {
        return false;
    }
    // Mid-sentence punctuation means the line continues: prose, not a title.
    if t.ends_with([',', ';']) {
        return false;
    }
    // Dot leaders: a table-of-contents entry.
    if t.contains("...") || t.contains(". . .") {
        return false;
    }
    if runs_into_prose(t) {
        return false;
    }
    // Where to look, write or call, set bold or large at the foot of a
    // data sheet: `seagate.com`, `Learn more at thermofisher.com/centrifuges`,
    // `For technical service call: 1-800-248-9606`. No section is titled
    // with an address.
    if ends_with_contact(t) {
        return false;
    }
    // An article's front matter: its authors, and its publication record.
    if byline(t) || publication_record(t) {
        return false;
    }
    !starts_like_list_item(t)
}

/// The labels an article's publication record sets at the head of a line,
/// before their value: `Received September 7, 2009; Accepted …`,
/// `Editor: …`, `Citation: …`. Lowercase.
const RECORD_LABELS: &[&str] = &[
    "received",
    "accepted",
    "revised",
    "published",
    "published online",
    "available online",
    "submitted",
    "editor",
    "edited by",
    "reviewed by",
    "academic editor",
    "handling editor",
    "citation",
    "copyright",
    "correspondence",
    "specialty section",
    "article history",
];

/// Month names, lowercase, as a publication record dates its steps.
const MONTHS: &[&str] = &[
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "jan",
    "feb",
    "mar",
    "apr",
    "jun",
    "jul",
    "aug",
    "sep",
    "sept",
    "oct",
    "nov",
    "dec",
];

/// Month names, lowercase, in the other languages documents date in:
/// French, German, Dutch, Spanish, Italian and Portuguese.
const OTHER_MONTHS: &[&str] = &[
    "janvier",
    "février",
    "fevrier",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "aout",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
    "decembre",
    "januar",
    "februar",
    "märz",
    "maerz",
    "juni",
    "juli",
    "oktober",
    "dezember",
    "januari",
    "februari",
    "maart",
    "mei",
    "augustus",
    "enero",
    "febrero",
    "marzo",
    "abril",
    "mayo",
    "junio",
    "julio",
    "agosto",
    "septiembre",
    "setiembre",
    "octubre",
    "noviembre",
    "diciembre",
    "gennaio",
    "febbraio",
    "aprile",
    "maggio",
    "giugno",
    "luglio",
    "settembre",
    "ottobre",
    "dicembre",
    "janeiro",
    "fevereiro",
    "março",
    "marco",
    "maio",
    "junho",
    "julho",
    "setembro",
    "outubro",
    "novembro",
    "dezembro",
];

/// A month's name in any of the languages above, its case and a closing
/// stop aside (`Juillet`, `Sept.`).
fn month_name(w: &str) -> bool {
    let w = w.trim_end_matches(['.', ',']).to_lowercase();
    MONTHS.contains(&w.as_str()) || OTHER_MONTHS.contains(&w.as_str())
}

/// The line is a date and nothing else: `05 Juillet 2015`, `July 1, 2014`,
/// `April 2024`, `13.06.2015`, `13/11/2017`, `2015-06-13`. A month's name
/// needs a day or a year beside it: `May` alone is a word.
fn date_text(t: &str) -> bool {
    let t = t.trim();
    // Figures alone: day, month and year parted by one separator.
    if !t.contains(' ') {
        for sep in ['/', '.', '-'] {
            let parts: Vec<&str> = t.trim_end_matches('.').split(sep).collect();
            if parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.len() <= 4 && p.chars().all(|c| c.is_ascii_digit()))
                && parts.iter().any(|p| p.len() >= 2)
            {
                return true;
            }
        }
        return false;
    }
    let words: Vec<&str> = t.split([' ', ',']).filter(|w| !w.is_empty()).collect();
    let day = |w: &str| {
        let w = w.trim_end_matches('.');
        let digits: String = w.chars().take_while(char::is_ascii_digit).collect();
        let suffix = &w[digits.len()..];
        !digits.is_empty()
            && digits.parse::<u32>().is_ok_and(|d| (1..=31).contains(&d))
            && matches!(suffix, "" | "st" | "nd" | "rd" | "th" | "er")
    };
    let year = |w: &str| {
        w.len() == 4
            && w.chars().all(|c| c.is_ascii_digit())
            && w.parse::<u32>().is_ok_and(|y| (1800..=2199).contains(&y))
    };
    let months = words.iter().filter(|w| month_name(w)).count();
    months == 1
        && words.len() >= 2
        && words.len() <= 4
        && words
            .iter()
            .all(|w| month_name(w) || day(w) || year(w) || *w == "de")
}

/// Currency signs and the codes for them that a sum may stand beside.
const CURRENCY_SIGNS: &[char] = &['€', '$', '£', '¥', '₹', '₩', '₽', '₺', '₪'];

/// A figure with two decimals, its thousands parted or not:
/// `1.589,80`, `4.11`, `406,82`, `1,234.56`, `-9,32`.
fn two_decimals(w: &str) -> bool {
    let w = w.trim_start_matches(['-', '+', '(']).trim_end_matches(')');
    let w = w.trim_matches(|c: char| CURRENCY_SIGNS.contains(&c));
    let Some(sep) = w.len().checked_sub(3).filter(|&k| w.is_char_boundary(k)) else {
        return false;
    };
    let (whole, cents) = w.split_at(sep);
    cents.starts_with(['.', ','])
        && cents[1..].chars().all(|c| c.is_ascii_digit())
        && !whole.is_empty()
        && whole.starts_with(|c: char| c.is_ascii_digit())
        && whole
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '\'' | '\u{2009}'))
}

/// The line is a sum and nothing else: a figure with two decimals, with
/// at most a currency's sign or code and a tax's (`€ 717,97`, `$4.11`,
/// `1.589,80`, `29.99 € TTC`).
fn amount_text(t: &str) -> bool {
    let words: Vec<&str> = t.split_whitespace().collect();
    words.iter().filter(|w| two_decimals(w)).count() == 1
        && words.iter().all(|w| {
            two_decimals(w)
                || w.chars().all(|c| CURRENCY_SIGNS.contains(&c))
                || (w.len() == 3 && w.chars().all(|c| c.is_ascii_uppercase()))
        })
}

/// The line states a sum: a figure with two decimals beside a currency's
/// sign (`Gesamtbetrag: 406,82 €`, `Nettobetrag: 358,00 € zuzüglich
/// Umsatzsteuer`).
fn states_amount(t: &str) -> bool {
    let words: Vec<&str> = t.split_whitespace().collect();
    words.iter().enumerate().any(|(k, w)| {
        two_decimals(w)
            && (w.contains(|c: char| CURRENCY_SIGNS.contains(&c))
                || [k.checked_sub(1), Some(k + 1)]
                    .into_iter()
                    .flatten()
                    .any(|j| {
                        words
                            .get(j)
                            .is_some_and(|o| o.chars().all(|c| CURRENCY_SIGNS.contains(&c)))
                    }))
    })
}

/// Least lowercase words in a heading that is a sentence.
const SENTENCE_LOWERCASE_WORDS: usize = 2;

/// Fewest words in a sentence closed by a stop that is no heading.
const MIN_SENTENCE_WORDS: usize = 4;

/// The line is a sentence closed by its stop, with words in lowercase
/// running between: a payment's note (`Volgens onze administratie heeft u
/// de factuur vooruitbetaald met Credit Card.`), a tagline (`Alles voor
/// een glimlach.`). A title closes on no stop, a one-word mark may (`FACTUUR.`),
/// and a numbered heading is judged by its numbering.
fn closed_sentence(t: &str) -> bool {
    let t = t.trim();
    // A numbered line is judged by its numbering: an agreement's `4.4
    // Adjustments to Applicable Conversion Price for Diluting Issues.`
    if !t.ends_with('.') || t.ends_with("..") || t.starts_with(|c: char| c.is_ascii_digit()) {
        return false;
    }
    let words: Vec<&str> = t.split_whitespace().collect();
    words.len() >= MIN_SENTENCE_WORDS
        && words
            .iter()
            .skip(1)
            .filter(|w| w.starts_with(|c: char| c.is_lowercase()))
            .count()
            >= SENTENCE_LOWERCASE_WORDS
}

/// A closed sentence that is prose, not a heading: one stating a date or a
/// sum, set in sentence case, half its words after the first in lowercase
/// (`Bitte überweisen Sie bis zum 04.07.2015.`). Length tells nothing: a
/// slide titles itself in a sentence (`Model the domain, not the org
/// chart.`), a 10-K captions each risk in one (`We are not profitable and
/// expect to continue to incur losses.`), and an agreement closes its
/// title-case headings on a stop (`Adjustments to Applicable Conversion
/// Price for Diluting Issues.`).
fn prose_sentence(t: &str) -> bool {
    let words: Vec<&str> = t.split_whitespace().collect();
    let lower = words
        .iter()
        .skip(1)
        .filter(|w| w.starts_with(|c: char| c.is_lowercase()))
        .count();
    // A date or a sum, not a number: an exercise's `Activity 4.
    // Determining CEC by replacing adsorbed cations.` is its heading.
    let dated = words.iter().any(|w| {
        let w = w.trim_end_matches(['.', ',']);
        date_text(w) || two_decimals(w)
    });
    closed_sentence(t) && lower * 2 >= words.len().saturating_sub(1) && dated
}

/// A line of an article's publication record: one of [`RECORD_LABELS`] and
/// its value, after a colon (`Editor: Dana Davis, …`) or as a date
/// (`Received 15 June 2018`, `Copyright © 2010 …`). A section of that name
/// is the word alone: `Funding`, `Copyright`.
fn publication_record(t: &str) -> bool {
    let lower = t
        .trim_start_matches(['*', '\u{2020}'])
        .trim()
        .to_lowercase();
    RECORD_LABELS.iter().any(|label| {
        let Some(rest) = lower.strip_prefix(label) else {
            return false;
        };
        if rest.starts_with(|c: char| c.is_alphanumeric()) {
            return false;
        }
        let rest = rest.trim_start();
        if let Some(value) = rest.strip_prefix(':') {
            return !value.trim().is_empty();
        }
        // An editor's label set alone over the name: no section is titled
        // so.
        if rest.is_empty() {
            return matches!(
                *label,
                "academic editor" | "handling editor" | "edited by" | "reviewed by"
            );
        }
        // A date within its first few words: a month's name or a year.
        rest.split_whitespace().take(4).any(|w| {
            let w = w.trim_matches(|c: char| !c.is_alphanumeric());
            MONTHS.contains(&w)
                || (w.len() == 4
                    && w.chars().all(|c| c.is_ascii_digit())
                    && (w.starts_with("19") || w.starts_with("20")))
        })
    })
}

/// Words a person's name may carry in lowercase: `van`, `de`, `von`.
const NAME_PARTICLES: &[&str] = &[
    "van", "de", "der", "den", "von", "da", "das", "do", "dos", "del", "della", "di", "du", "la",
    "le", "ten", "ter", "bin", "al", "el",
];

/// Marks set after an author's name to point to their affiliation or to
/// the corresponding author: `1`, `2,3`, `*`, `†`.
fn affiliation_mark(c: char) -> bool {
    c.is_ascii_digit()
        || matches!(
            c,
            '*' | '\u{2020}' | '\u{2021}' | '\u{00A7}' | '\u{00B6}' | ',' | '\u{262F}' | '\u{204E}'
        )
}

/// A line of an article's authors: people's names, each of two to four
/// words, with the marks that point to their affiliations
/// (`Roberta Lattanzi 1, Silvia Rief2, … and*`, `Mariana Spetea 2*`). One
/// name with a plain number is too like a title (`Study 2`, `Model Year
/// 2020`), and one with a star like a heading with a note
/// (`Posted Transactions*`): a line of one name is a byline when it marks
/// an affiliation and the corresponding author (`2*`) or a list of
/// affiliations (`1,2`).
fn byline(t: &str) -> bool {
    let mut names = 0;
    let mut marked = false;
    let mut strong = false;
    let units = t
        .replace(" and ", ", ")
        .replace(" & ", ", ")
        .replace(';', ",");
    for unit in units.split(", ") {
        let unit = unit.trim();
        let mut words: Vec<&str> = unit.split_whitespace().collect();
        // A list run on to the next line ends on its `and`.
        if words
            .last()
            .is_some_and(|w| w.trim_end_matches(affiliation_mark) == "and")
        {
            words.pop();
        }
        // The marks: after the last word, or set apart from it.
        let mut marks = String::new();
        while let Some(last) = words.last() {
            let name = last.trim_end_matches(affiliation_mark);
            marks.insert_str(0, &last[name.len()..]);
            if name.is_empty() {
                words.pop();
            } else {
                let n = words.len();
                words[n - 1] = name;
                break;
            }
        }
        if words.is_empty() {
            // Marks alone continue the name before them: `1,2` after `Lee
            // 1,`, or after the last name on the line above (`3,4, Steve W.
            // Lindsay5‡, …`).
            if !marks.is_empty() {
                marked = true;
                strong |= strong_mark(&marks);
                continue;
            }
            return false;
        }
        let is_name_word = |w: &&str| {
            NAME_PARTICLES.contains(w)
                || (w.chars().next().is_some_and(char::is_uppercase)
                    && w.chars()
                        .all(|c| c.is_alphabetic() || matches!(c, '.' | '-' | '\'' | '\u{2019}')))
        };
        if !(2..=4).contains(&words.len()) || !words.iter().all(is_name_word) {
            return false;
        }
        names += 1;
        if marks.chars().any(|c| c != ',') {
            marked = true;
            // A number past two figures is no affiliation's.
            if marks
                .split(',')
                .any(|m| m.trim_matches(|c: char| !c.is_ascii_digit()).len() > 2)
            {
                return false;
            }
            strong |= strong_mark(&marks);
        }
    }
    names > 0 && marked && (names >= 2 || strong)
}

/// A mark only an author's name carries: an affiliation's number with a
/// symbol for the corresponding author (`2*`), or a list of affiliations
/// (`1,2`). A symbol alone is as often a note's (`Posted Transactions*`).
fn strong_mark(marks: &str) -> bool {
    let digits = marks.chars().any(|c| c.is_ascii_digit());
    let symbol = marks.chars().any(|c| !c.is_ascii_digit() && c != ',');
    (digits && symbol) || marks.trim_matches(',').contains(',')
}

/// Words a title does not end on: an article, a conjunction, `of`, `not`.
/// A line ending on one runs on into the next: a form's numbered caption
/// cut at its first line (`5 Expenses recorded on books this year not`), a
/// side label set a line a word (`Tax and` over `Credits`). In lowercase
/// only, so that `Part A` and `Schedule A` stay titles.
const DANGLING_WORDS: &[&str] = &["a", "an", "the", "and", "or", "nor", "of", "not", "but"];

/// The line stops mid-phrase, on one of [`DANGLING_WORDS`].
fn ends_mid_phrase(t: &str) -> bool {
    t.split_whitespace()
        .next_back()
        .is_some_and(|w| DANGLING_WORDS.contains(&w))
}

/// The line closes with a web address, a domain, an e-mail address or a
/// phone number, alone or after a few words leading in to it.
pub(crate) fn ends_with_contact(text: &str) -> bool {
    let tokens: Vec<&str> = text
        .split_whitespace()
        .map(|w| w.trim_end_matches(['.', ',', ';', ':', ')', '\u{AE}', '\u{2122}']))
        .filter(|w| !w.is_empty())
        .collect();
    let Some(last) = tokens.last() else {
        return false;
    };
    web_address(last) || phone_number(&tokens)
}

/// Common top-level domains. A bare domain must end in one of them, so a
/// product name with a dot in it (`Node.js`) is not an address.
const TOP_LEVEL_DOMAINS: &[&str] = &[
    "com", "org", "net", "gov", "edu", "mil", "int", "info", "biz", "io", "co", "us", "uk", "ca",
    "de", "fr", "eu", "es", "it", "nl", "be", "ch", "at", "se", "dk", "fi", "no", "ie", "pl", "pt",
    "cz", "jp", "cn", "kr", "in", "au", "nz", "br", "mx",
];

/// `https://…`, `www.…`, `name@host.tld`, or a lowercase host with a known
/// top-level domain and an optional path (`hermanmiller.com/aeron-chair`).
/// Lowercase, because a capitalised `ASP.NET` is a name.
fn web_address(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("www.") {
        return true;
    }
    let (user, host) = match token.split_once('@') {
        Some((user, host)) => (Some(user), host),
        None => (None, token),
    };
    if user.is_some_and(str::is_empty) {
        return false;
    }
    let host = host.split('/').next().unwrap_or("");
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
        && labels[labels.len() - 2].chars().count() >= 2
        && TOP_LEVEL_DOMAINS.contains(labels.last().unwrap_or(&""))
}

/// A phone number closing the line: `1-800-248-9606`, `(636)272-2408`,
/// `800.424.8783`, a vanity `1-800-GET-PINK`, or an international number
/// opening with `+`. North American numbers end in a group of three and a
/// group of four, which a date (`2019-07-25`), a standard's year
/// (`2043-2013`) or a count (`COVID-19`) does not, and a decimal set to four
/// places (`259.3490`) is no local number.
fn phone_number(tokens: &[&str]) -> bool {
    let phoneish = |t: &str| {
        t.chars().any(|c| c.is_ascii_digit())
            && t.chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase() || "()+-./".contains(c))
    };
    let run = tokens.iter().rev().take_while(|t| phoneish(t)).count();
    if run == 0 {
        return false;
    }
    let run = &tokens[tokens.len() - run..];
    let groups: Vec<&str> = run
        .iter()
        .flat_map(|t| t.split(['(', ')', '+', '-', '.', '/']))
        .filter(|g| !g.is_empty())
        .collect();
    let digit_group = |g: &str| g.chars().all(|c| c.is_ascii_digit());
    let letter_group = |g: &str| g.chars().all(|c| c.is_ascii_uppercase());
    if !groups.iter().all(|g| digit_group(g) || letter_group(g)) || !digit_group(groups[0]) {
        return false;
    }
    let digits: usize = groups.iter().map(|g| g.len()).sum();
    if run[0].starts_with('+') {
        return digits >= 8;
    }
    // Two groups must come from one token, `272-2408`: two numbers set
    // apart by a space are not a phone number.
    let joined = groups.len() >= 3 || run.len() == 1;
    let n = groups.len();
    // Two groups set apart by a point alone are a decimal: a structure's
    // `Molecular Weight: 259.3490`, not a local number.
    if n == 2 && run.iter().all(|t| !t.contains(['-', '(', ')', '/'])) {
        return false;
    }
    joined && n >= 2 && digits >= 7 && groups[n - 1].len() == 4 && groups[n - 2].len() == 3
}

/// A bold run-in lead-in, not a heading: `**Filtered task names.** We present
/// task names…`.
///
/// LaTeX's `\paragraph{}` sets its argument bold on the same line as the text it
/// introduces, so the block reads as one bold phrase followed by prose. Weight
/// and length cannot tell it from a section title — but a title is a single
/// phrase, while this carries a sentence boundary with a new sentence after it.
///
/// Requires a following *word*, not just a capital: `Sec. 3` and `Fig. 1 Results`
/// keep their abbreviating dot, and a title ending in a full stop (`CHAPTER 1.`)
/// has nothing after it and is untouched.
fn runs_into_prose(text: &str) -> bool {
    let bytes = text.as_bytes();
    for (i, w) in bytes.windows(2).enumerate() {
        if !matches!(w[0], b'.' | b'?' | b'!') || w[1] != b' ' {
            continue;
        }
        // The word carrying the dot decides. Abbreviations are short — `vs.`,
        // `Sec.`, `Fig.`, `et al.`, `No.` — and a heading may contain any of
        // them without ending a sentence; ordinary words are longer.
        let head = &text[..=i];
        let last = head.split_whitespace().next_back().unwrap_or("");
        if last.trim_end_matches(['.', '?', '!']).chars().count() < MIN_SENTENCE_WORD_CHARS {
            continue;
        }
        // Require real text on both sides, so a title ending in a full stop
        // (`CHAPTER 1.`) and a trailing fragment are both left alone.
        if head.split_whitespace().count() >= 2 && text[i + 2..].split_whitespace().count() >= 2 {
            return true;
        }
    }
    false
}

/// `Figure 2.1: Surveyed MSMEs…`, `Table 3 — results`, `Diagram 4 …`: a
/// caption. The label word plus a number is the test; a heading that merely
/// *mentions* a figure mid-phrase does not start with the pattern.
///
/// A caption is never a heading on bold or size evidence
/// ([`not_a_typographic_heading`]), and is the figure's where a figure has
/// taken it as its label, whatever its style. The benchmark's ground truth
/// keeps some prominent captions as headings, so the exclusion costs a
/// little there (−0.0004 when first measured, and −0.0005 on one page whose
/// real heading was missed, the caption standing in for it).
pub(crate) fn is_caption(text: &str) -> bool {
    let mut words = text.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    let label = matches!(
        first
            .trim_end_matches(['.', ':'])
            .to_ascii_lowercase()
            .as_str(),
        "figure"
            | "fig"
            | "table"
            | "chart"
            | "diagram"
            | "exhibit"
            | "graph"
            | "map"
            | "box"
            | "plate"
    );
    if !label {
        return false;
    }
    // The next token is a number, dotted or hyphenated by chapter ("2.1",
    // "3", "B.2", "5-1").
    words.next().is_some_and(|w| {
        let w = w.trim_end_matches([':', '.', ',']);
        !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '\u{2013}'))
            && w.chars().any(|c| c.is_ascii_digit())
    })
}

/// A block that announces a table of contents.
///
/// Everything after it on the same page is a ToC entry, not a heading. A
/// contents page's ground truth has exactly one heading, `Contents`, while
/// treating each entry as its own heading emits one per numbered line
/// beneath it.
pub(crate) fn is_toc_marker(text: &str) -> bool {
    let t = text.trim().trim_end_matches(':').to_ascii_lowercase();
    matches!(
        t.as_str(),
        "contents" | "table of contents" | "index" | "toc"
    )
}

/// A bullet or dash opening the block marks a list item, not a heading.
/// Datasheet feature lists are set larger than the surrounding table text and
/// would otherwise all qualify on size alone, and an option list's bold
/// items (`❑ Peg rack`) on weight.
///
/// The plain bullets and the boxes to tick, not every bullet: the arrows and
/// diamonds of the dingbat fonts are set before headings as often as before
/// items, as an annotation guideline marks each of its subsections
/// `❖ Gene`, `❖ Disease`. Those the heading pass decides as it would any
/// block; a block it leaves is still a list item.
fn starts_like_list_item(text: &str) -> bool {
    matches!(
        text.chars().next(),
        Some(
            '\u{2022}' // •
                | '\u{F0B7}' // the Symbol font's •, as Word maps it
                | '\u{25A0}' // ■
                | '\u{25CF}' // ●
                | '\u{2751}' // ❑
                | '\u{2752}' // ❒
                | '-'
                | '\u{2013}'
                | '\u{2014}'
        )
    )
}

fn style_of(b: &Block) -> Style {
    let caps = {
        let t = b.text();
        let letters: Vec<char> = t.chars().filter(|c| c.is_alphabetic()).collect();
        !letters.is_empty() && letters.iter().all(|c| c.is_uppercase())
    };
    // A heading is styled by its leading run: a bracketed note set lighter
    // after it (`raft-R-mate® Attic Vents` bold, then `(Ventilation
    // Baffles)`) leaves it a bold heading, ranked with the other bold heads
    // at its size. A sentence that opens on a bold name runs on unbracketed,
    // and keeps its weight.
    let noted = {
        let t = b.text();
        let t = t.trim_end();
        t.ends_with(')') && t.contains(" (")
    };
    let leads_bold = noted && b.lines.first().is_some_and(|l| l.opens_bold);
    Style {
        neg_size: -((b.font_size * 4.0).round() as i32),
        not_bold: !(b.bold || leads_bold),
        not_caps: !caps,
        italic: b.lines.iter().filter(|l| l.italic).count() * 2 > b.lines.len(),
    }
}

/// A block's x-height as a share of its size: the median of its lines'
/// (see [`crate::line::Line::x_height`]).
fn x_height_of(b: &Block) -> Option<f32> {
    let mut hs: Vec<f32> = b.lines.iter().filter_map(|l| l.x_height).collect();
    hs.sort_by(f32::total_cmp);
    (!hs.is_empty()).then(|| hs[hs.len() / 2])
}

/// Modal font size across blocks, weighted by line count — the body size.
/// Weighting by lines rather than blocks stops a page of one-line captions
/// from redefining what "body" means.
pub fn body_font_size(pages: &[Vec<Block>]) -> f32 {
    body_font_size_outside(pages, &[])
}

/// The body size, over the blocks `drawn` does not mark: the text of
/// drawings, by page and block, is set aside. A diagram is many one-line
/// labels, and 64 of them outweighed a 25-line column of prose. With
/// nothing left once drawings are set aside, every block counts.
pub fn body_font_size_outside(pages: &[Vec<Block>], drawn: &[Vec<bool>]) -> f32 {
    let is_drawn = |p: usize, i: usize| drawn.get(p).and_then(|d| d.get(i)).copied() == Some(true);
    let mut hist: HashMap<i32, usize> = HashMap::new();
    for (p, blocks) in pages.iter().enumerate() {
        for (i, b) in blocks.iter().enumerate() {
            if !is_drawn(p, i) {
                *hist.entry((b.font_size * 4.0).round() as i32).or_default() += b.lines.len();
            }
        }
    }
    if hist.is_empty() && !drawn.is_empty() {
        return body_font_size(pages);
    }
    hist.iter()
        .max_by(|(size_a, count_a), (size_b, count_b)| {
            count_a
                .cmp(count_b)
                // Equal-frequency sizes are deterministic: prefer the
                // smaller face, which is less likely to be a display title.
                .then_with(|| size_b.cmp(size_a))
        })
        .map(|(k, _)| *k as f32 / 4.0)
        .unwrap_or(10.0)
}

/// Whether the size most of the document's glyphs are set at is set mostly
/// bold, over all of them: its tables' text and figures as much as its
/// paragraphs'. What the tables leave of a page is no measure of it: a
/// form's bold labels, a statement's bold title. `None` without glyphs.
pub fn body_is_bold<'a>(glyphs: impl IntoIterator<Item = &'a crate::glyph::Glyph>) -> Option<bool> {
    let mut by_size: HashMap<i32, (usize, usize)> = HashMap::new();
    for g in glyphs {
        if g.text.trim().is_empty() {
            continue;
        }
        let (all, bold) = by_size
            .entry((g.font_size * 4.0).round() as i32)
            .or_default();
        *all += 1;
        *bold += usize::from(g.weight.is_some_and(|w| w >= 600));
    }
    by_size
        .into_iter()
        .max_by_key(|(size, (all, _))| (*all, std::cmp::Reverse(*size)))
        .map(|(_, (all, bold))| bold * 2 > all)
}

/// The block each bookmark heads, by page and block index, with the
/// bookmark's depth. A bookmark heads one place: a running head that
/// repeats a section's title (`Appendix 1. Supplemental Figures` at the top
/// of every even page) matches its bookmark on every page, and only the
/// copy nearest the bookmark's destination is the heading. Nearest by page,
/// then by the stated position, then the larger type; each block takes one
/// bookmark, and each bookmark one block. Only bookmarks that name a page
/// are placed so; the rest are matched by title alone ([`pick_entry`]).
///
/// Also the copies no bookmark took that stand where another such copy
/// stands on another page, within [`RUNNING_SLOT`]: the running head
/// itself, which is no heading by weight or size either.
fn claim_outline(
    pages: &[Vec<Block>],
    by_title: &HashMap<String, Vec<&OutlineItem>>,
) -> (
    HashMap<(usize, usize), usize>,
    std::collections::HashSet<(usize, usize)>,
) {
    let mut pairs = Vec::new();
    for (pi, blocks) in pages.iter().enumerate() {
        for (bi, b) in blocks.iter().enumerate() {
            let Some(entries) = by_title.get(&norm(&b.text())) else {
                continue;
            };
            for e in entries.iter().copied() {
                let Some(page) = e.page else {
                    continue;
                };
                let dy = e.y.map_or(f64::MAX, |y| (y - b.bbox.y0).abs());
                pairs.push((
                    page.abs_diff(b.page),
                    dy,
                    -b.font_size,
                    pi,
                    bi,
                    e as *const OutlineItem,
                    e.level,
                ));
            }
        }
    }
    pairs.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.total_cmp(&b.1))
            .then(a.2.total_cmp(&b.2))
            .then((a.3, a.4).cmp(&(b.3, b.4)))
    });
    let mut entries_used = std::collections::HashSet::new();
    let mut claimed = HashMap::new();
    let mut copies: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for &(_, _, _, pi, bi, entry, level) in &pairs {
        copies
            .entry(norm(&pages[pi][bi].text()))
            .or_default()
            .push((pi, bi));
        if claimed.contains_key(&(pi, bi)) || !entries_used.insert(entry) {
            continue;
        }
        claimed.insert((pi, bi), level);
    }
    let mut running = std::collections::HashSet::new();
    for blocks in copies.values() {
        let free: Vec<(usize, usize)> = blocks
            .iter()
            .copied()
            .filter(|k| !claimed.contains_key(k))
            .collect();
        for &(pi, bi) in &free {
            let b = &pages[pi][bi].bbox;
            let repeated = free.iter().any(|&(pj, bj)| {
                let o = &pages[pj][bj].bbox;
                pj != pi
                    && (o.x0 - b.x0).abs() <= RUNNING_SLOT
                    && (o.y0 - b.y0).abs() <= RUNNING_SLOT
            });
            if repeated {
                running.insert((pi, bi));
            }
        }
    }
    (claimed, running)
}

/// How far, in points, two copies of a line may stand from each other's
/// place and still be one running line. A running head is placed by the
/// page template, so its copies coincide to within rounding of the
/// coordinates, a small fraction of a point; two points allow for a page
/// box shifted slightly between pages and for glyph boxes that differ with
/// the letters. A fixed amount, not a share of the type: the template, not
/// the line's size, sets where it stands. Well under any line's height, so
/// two lines set one under the other never match.
const RUNNING_SLOT: f64 = 2.0;

/// Choose which of several same-titled bookmarks this block is.
///
/// Nearest page first, then nearest stated position. An entry naming no page
/// cannot be placed, so it only wins when nothing else matched — the old
/// behaviour, kept for files whose bookmarks resolve to nothing.
fn pick_entry(entries: &[&OutlineItem], b: &Block) -> Option<usize> {
    if entries.len() == 1 {
        return Some(entries[0].level);
    }
    let placed: Vec<&&OutlineItem> = entries.iter().filter(|e| e.page.is_some()).collect();
    if placed.is_empty() {
        // No destinations to choose by: promoting is safer than burying a
        // real section, which is what this did before destinations existed.
        return entries.iter().map(|e| e.level).min();
    }
    placed
        .iter()
        .min_by(|x, y| {
            let d = |e: &OutlineItem| {
                (
                    e.page.map_or(usize::MAX, |p| p.abs_diff(b.page)),
                    e.y.map_or(f64::MAX, |v| (v - b.bbox.y0).abs()),
                )
            };
            let (dx, dy) = (d(x), d(y));
            dx.0.cmp(&dy.0)
                .then(dx.1.partial_cmp(&dy.1).unwrap_or(std::cmp::Ordering::Equal))
        })
        .map(|e| e.level)
}

/// Smallest size, against the text, of a bold line read as a heading on its
/// weight. Bold set at three fifths of it or smaller is lettering, not a
/// section's title: a logo's `IN THE USA` at 3.8pt over an 8pt body,
/// a chart's axis and data labels, a figure's callouts. A heading set small
/// is still set as text is read (a sheet's sidebar heads at seven tenths of
/// its body), not as fine print.
const BOLD_HEADING_FLOOR: f64 = 0.6;

/// Detect headings across a document.
/// Smallest size, against the body's, of a numbered line read as a section
/// heading. Notes and footnotes are numbered too, and set smaller.
const NUMBERED_NOTE_RATIO: f64 = 0.85;

/// Widest a body-size title runs per inked character, in its font size: a
/// run of words. Wider is a label and its value set apart across the page.
const SPREAD_ADVANCE: f64 = 0.85;

/// Sizes, in points, within which two candidate titles are set alike.
const SAME_TITLE_SIZE: f32 = 0.25;

/// Most lines a document's title set at display size runs over: a
/// journal article's title, at twice the body's size, runs to five.
const MAX_TITLE_LINES: usize = 6;

/// Least share of the page's text width a title set for display spans when
/// no block above it reads as a title.
const DISPLAY_TITLE_WIDTH: f64 = 0.5;

/// Most words in a document's title set at display size. An article's
/// title states its finding (`Comparison between CHA2DS2-VASc and the new
/// R2CHADS2 and ATRIA scores at predicting thromboembolic event in …`).
const MAX_DISPLAY_TITLE_WORDS: usize = 30;

/// How much larger than the first blocks' title a title read later on the
/// first page must be to be the document's.
const LATE_TITLE_RATIO: f32 = 1.1;

pub fn detect(pages: &[Vec<Block>], outline: &[OutlineItem]) -> Vec<Heading> {
    detect_with_body(pages, outline, body_font_size(pages), None)
}

/// Detect headings across a document whose body size is known.
/// Detect headings over a body size, and over what the page's glyphs say of
/// whether the body is set bold. Without that, it is read from the blocks.
pub fn detect_with_body(
    pages: &[Vec<Block>],
    outline: &[OutlineItem],
    body: f32,
    body_bold: Option<bool>,
) -> Vec<Heading> {
    // Numbered lines that recur, by their text; see the check in the loop
    // below. Counted as occurrences rather than pages: a form answers
    // several items on one page with the same code. A contents entry does
    // not repeat the section it points to — it carries a page number the
    // section does not, and its own text differs.
    let repeated_numbered: std::collections::HashSet<String> = {
        let mut seen: HashMap<String, usize> = HashMap::new();
        for b in pages.iter().flatten() {
            // A list's item gathered with its description repeats a
            // numbered line by its own first line, `1) Zero-Shot`.
            let text = match b.lines.first() {
                Some(l) if block::counts_off(b) => l.text.clone(),
                _ => b.text(),
            };
            if numbering_depth(&text).is_some() {
                *seen.entry(norm(&text)).or_default() += 1;
            }
        }
        seen.into_iter()
            .filter(|(_, n)| *n >= REPEATED_NUMBERING)
            .map(|(t, _)| t)
            .collect()
    };

    // A section number names one section. A bare number that labels two
    // different titles in one document — `1 — Yes`, `1 — Supplier on file`,
    // `1 — Weekly` — is a code, and so is every
    // line it labels, including the ones set only once. This is the
    // document-wide half of the form test; the page cap below is the other.
    let shared_bare_numbers: std::collections::HashSet<String> = {
        let mut titles: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        for b in pages.iter().flatten() {
            let text = b.text();
            if has_bare_number_label(&text) {
                let label = text.split_whitespace().next().unwrap_or("").to_string();
                titles.entry(label).or_default().insert(norm(&text));
            }
        }
        titles
            .into_iter()
            .filter(|(_, t)| t.len() >= 2)
            .map(|(label, _)| label)
            .collect()
    };
    // Letters that open a block as a label somewhere in the document, a
    // heading's or a run-in paragraph's (`A. COMMON STOCK. The
    // Corporation…`). A lettered section has a neighbour in its sequence,
    // `A.` before `B.`; an initial, `J. Example`, has none. I, V and X stand
    // alone as roman numerals often enough (`I. Introduction`) to keep their
    // reading.
    let letter_labels: std::collections::HashSet<char> = pages
        .iter()
        .flatten()
        .filter_map(|b| letter_label(&b.text()))
        .collect();
    let initial = |text: &str| -> bool {
        single_letter_label(text).is_some_and(|c| {
            let near = |d: i32| char::from_u32((c as i32 + d) as u32);
            !matches!(c, 'I' | 'V' | 'X')
                && ![near(-1), near(1)]
                    .iter()
                    .flatten()
                    .any(|n| letter_labels.contains(n))
        })
    };
    let coded = |text: &str| -> bool {
        has_bare_number_label(text)
            && text
                .split_whitespace()
                .next()
                .is_some_and(|label| shared_bare_numbers.contains(label))
    };

    let document_title = pages.first().and_then(|blocks| {
        let largest = blocks.iter().map(|b| b.font_size).fold(0.0_f32, f32::max);
        // The page's largest type, set for display: an article's title, which
        // may run over four or five lines (`An internal deletion in MTH1
        // enables growth on glucose of pyruvate-decarboxylase negative, …`).
        let display = |b: &Block| {
            b.font_size as f64 >= body as f64 * DISPLAY_HEADING_RATIO
                && b.font_size >= largest * 0.98
                && b.lines.len() <= MAX_TITLE_LINES
        };
        let limit = |text: &str, b: &Block| {
            if display(b) {
                MAX_DISPLAY_TITLE_WORDS
            } else {
                word_limit(text, b, body)
            }
        };
        let candidate = |i: usize, b: &Block| -> bool {
            let text = b.text();
            // At most three lines, or a few words to a line, as a cover
            // stacks its title: `PROCAT / SISTEMA DE / AISLAMIENTO
            // SUELTO / PROFESIONAL`.
            // Of words: a badge's text layer stacks `a1111111111`.
            let words = text.split_whitespace().count();
            let inked: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
            let letters = inked.iter().filter(|c| c.is_alphabetic()).count();
            let lettered = letters * 10 >= inked.len() * 6;
            let stacked = words <= b.lines.len() * 2 && lettered;
            // A title stands alone on its row. One of two one-line
            // groups set across a row, a course's name at the left and
            // its code at the right, is a running head's part. A cover
            // title stacked beside its description is not.
            let shares_row = b.lines.len() == 1
                && blocks.iter().any(|o| {
                    !std::ptr::eq(o, b)
                        && o.lines.len() == 1
                        && o.bbox.y1.min(b.bbox.y1) - o.bbox.y0.max(b.bbox.y0)
                            > o.bbox.height().min(b.bbox.height()) * 0.5
                        && (o.bbox.x1 <= b.bbox.x0 || b.bbox.x1 <= o.bbox.x0)
                });
            if b.marker.is_some()
                || shares_row
                || closed_sentence(&text)
                || fields_run(&text)
                || in_postal_address(blocks, i)
                || (b.lines.len() > 3 && !stacked && !(display(b) && lettered))
                || !title_like_within(&text, limit(&text, b))
                || (numbering_depth(&text).is_some() && repeated_numbered.contains(&norm(&text)))
            {
                return false;
            }
            let gap_after = blocks
                .get(i + 1)
                .map(|next| next.bbox.y0 - b.bbox.y1)
                .unwrap_or(0.0);
            let prominent_size = b.font_size >= body * 1.05;
            // At the body's size a title stands as one run of words. A
            // label and its value set apart across the page (`Part No.`
            // and `675 8126H250`, 170pt between) is a field.
            let spread = b.lines.len() == 1
                && b.bbox.width() > inked.len() as f64 * b.font_size as f64 * SPREAD_ADVANCE;
            let isolated_largest =
                b.font_size >= largest * 0.98 && gap_after >= b.font_size as f64 * 0.75 && !spread;
            let normalized = text.trim().trim_end_matches(':').to_lowercase();
            let contents_title = matches!(normalized.as_str(), "contents" | "table of contents");
            prominent_size || isolated_largest || contents_title
        };
        // The page's text width, for telling two columns apart.
        let (left, right) = blocks.iter().fold((f64::MAX, f64::MIN), |(l, r), b| {
            (l.min(b.bbox.x0), r.max(b.bbox.x1))
        });
        let column_apart = (right - left) * 0.25;
        let early = blocks
            .iter()
            .take(4)
            // Once ordinary prose starts, a later isolated equation lead-in
            // is not the document title.
            .take_while(|b| {
                let text = b.text();
                text.split_whitespace().count() <= limit(&text, b)
            })
            .enumerate()
            .filter(|&(i, b)| candidate(i, b))
            // The largest of the candidates, not the first. A cover commonly
            // sets the publisher or programme above the title in smaller type
            // — "Arizona State University" over "Sustainability Impact
            // Review", at 17.9pt against 29.6 — and taking the first in
            // reading order crowns the kicker. The title is then ranked among
            // the ordinary font-size tiers and lands *below* the line above
            // it, which reads as an inverted hierarchy to everything
            // downstream.
            //
            // Two of one size go to the one a column further left, which
            // the page is read from first: a category strip at the right of
            // the page's header (`Wallbox Wiring Devices`) is as large as
            // the title over the text, and set above it. Otherwise the
            // earlier: where two lines share the largest style, the first
            // is the title and the second is its subtitle.
            .fold(None, |best: Option<(usize, &Block)>, (i, b)| match best {
                Some((_, prev)) if (prev.font_size - b.font_size).abs() <= SAME_TITLE_SIZE => {
                    if b.bbox.x0 < prev.bbox.x0 - column_apart {
                        Some((i, b))
                    } else {
                        best
                    }
                }
                Some((_, prev)) if prev.font_size > b.font_size => best,
                _ => Some((i, b)),
            });
        // The page's largest title set in its upper half, read only after a
        // sidebar or a column the reading takes first (`Highlights` at
        // 14pt, read before the 17pt title at the right), when it is
        // clearly larger than the first blocks' title. Where the first
        // blocks hold no title, a large line further down is a section's
        // or a statement's, not the document's, unless it is set for
        // display.
        let foot = blocks.iter().map(|b| b.bbox.y1).fold(0.0_f64, f64::max);
        let late = blocks
            .iter()
            .enumerate()
            .skip(4)
            .filter(|&(i, b)| {
                b.font_size >= largest * 0.98 && b.bbox.y0 < foot * 0.5 && candidate(i, b)
            })
            // Where no early block is a title, a title set for display and
            // across the page: over a journal's banner set across the row
            // (`RESEARCH ARTICLE` and `Open Access`), and its citation and
            // DOI. A narrow line set large is a badge or a callout (a
            // model's number at the right of a sheet's head).
            .find(|(_, b)| {
                early.map_or(
                    display(b) && b.bbox.width() >= (right - left) * DISPLAY_TITLE_WIDTH,
                    |(_, e)| b.font_size > e.font_size * LATE_TITLE_RATIO,
                )
            });
        late.or(early).map(|(i, _)| i)
    });

    // Outline entries by normalized title. A title is not a key: one book
    // carries 122 duplicated bookmark titles among 447, and "Overview" under
    // four parents is four different depths. Every entry is kept, and the
    // heading's own page decides which one it is.
    let mut by_title: HashMap<String, Vec<&OutlineItem>> = HashMap::new();
    for it in outline {
        let k = norm(&it.title);
        if k.is_empty() {
            continue;
        }
        by_title.entry(k).or_default().push(it);
    }
    let (claimed, running) = claim_outline(pages, &by_title);

    // Distinct heading-sized fonts, largest first, for the typography fallback.
    let mut sizes: Vec<i32> = pages
        .iter()
        .flatten()
        .filter(|b| b.font_size as f64 > body as f64 * (1.0 + HEADING_SIZE_MARGIN))
        .map(|b| (b.font_size * 4.0).round() as i32)
        .collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    sizes.dedup();
    let size_level: HashMap<i32, usize> = sizes
        .iter()
        .enumerate()
        .map(|(i, s)| (*s, (i + 1).min(MAX_LEVEL)))
        .collect();

    // Bold marks section titles only where it is used sparingly. On pages that
    // set chart legends, axis labels or table headers bold, the same test
    // proposes a dozen "headings" per page — one page's chart labels alone
    // yield `October 2020`, `Oct 2020`, `Don't know`. No page carries that
    // many sections, so past a
    // plausible density the signal is measuring something else and is dropped
    // for the document. Counted before emission because one page's misuse
    // discredits the cue everywhere in the document, not just on that page.
    let bold_candidates = pages
        .iter()
        .flatten()
        .filter(|b| {
            let t = b.text();
            b.bold
                && b.lines.len() <= MAX_FONTSIZE_HEADING_LINES
                && t.chars().count() <= MAX_FONTSIZE_HEADING_CHARS
                && title_like(&t)
        })
        .count();
    let bold_is_structural = bold_candidates <= MAX_BOLD_HEADINGS_PER_PAGE * pages.len().max(1);

    // If most body text is already bold, weight carries no information.
    // Read from the blocks only where the glyphs were not consulted: what
    // the tables leave of a form is mostly its bold labels, a dozen lines.
    let body_is_bold = body_bold.unwrap_or_else(|| {
        let bold_lines: usize = pages
            .iter()
            .flatten()
            .filter(|b| b.bold)
            .map(|b| b.lines.len())
            .sum();
        let all_lines: usize = pages.iter().flatten().map(|b| b.lines.len()).sum();
        all_lines > 0 && bold_lines * 2 > all_lines
    });

    let mut out = Vec::new();
    // Acronyms set on a line of their own, too short to look like a title,
    // with where they stand: see [`short_caps`].
    let mut acronyms: Vec<(usize, usize, String, Style)> = Vec::new();
    for (page_idx, blocks) in pages.iter().enumerate() {
        // Lettering is set far smaller than the text, measured either way:
        // by the document's body, or by the size the page's level lines are
        // set in. A watermark's letters, set at an angle each as a line of
        // its own, can outnumber a page's lines of text and set the
        // document's body at their size; a title slide or a page of large
        // type sets its own lines larger than the document's text.
        let level = crate::block::body_size(
            blocks
                .iter()
                .flat_map(|b| &b.lines)
                .filter(|l| l.rotation_bucket == 0),
        );
        let lettering_measure = if level > 0.0 {
            level.min(body as f64)
        } else {
            body as f64
        };
        // Suppress headings after a contents marker on the same page: the
        // entries below it are the table of contents, not sections.
        let toc_from = blocks.iter().position(|b| is_toc_marker(&b.text()));
        // A page of coded answers: bare numbers by the dozen are a scheme,
        // not an outline, and none of them is a section.
        let page_is_coded = blocks
            .iter()
            .filter(|b| has_bare_number_label(&b.text()))
            .count()
            > MAX_BARE_NUMBERED_PER_PAGE;
        for (block_idx, b) in blocks.iter().enumerate() {
            if toc_from.is_some_and(|i| block_idx > i) {
                continue;
            }
            let text = b.text();
            let n_chars = text.chars().count();
            if text.trim().is_empty() || n_chars > MAX_HEADING_CHARS {
                continue;
            }

            // A document title is often a one-off style, so it cannot form a
            // repeated font-size tier. Recover it before the generic detectors.
            if page_idx == 0 && document_title == Some(block_idx) {
                out.push(Heading {
                    page: b.page,
                    block_index: block_idx,
                    text,
                    level: 1,
                    evidence: Evidence::Title,
                    form_text: false,
                    style: style_of(b),
                });
                continue;
            }

            // 1. Outline match wins outright — the author named this heading,
            // so the shape tests do not apply. Where the title is ambiguous,
            // the destination decides: the entry landing on this page, and
            // among those the one whose stated position is nearest the block.
            // Most files write `/Fit`, which names a page and no position, so
            // the page alone carries this most of the time — and that is
            // already enough to tell the fourth "Overview" from the first.
            // A copy of a bookmarked title set where it is set on other
            // pages too, not the one its bookmark lands on: a running head.
            if running.contains(&(page_idx, block_idx)) {
                continue;
            }
            let outline_level = match by_title.get(&norm(&text)) {
                Some(entries) if entries.iter().any(|e| e.page.is_some()) => {
                    claimed.get(&(page_idx, block_idx)).copied()
                }
                Some(entries) => pick_entry(entries, b),
                None => None,
            };
            if let Some(level) = outline_level {
                out.push(Heading {
                    page: b.page,
                    block_index: block_idx,
                    text,
                    level,
                    evidence: Evidence::Outline,
                    form_text: false,
                    style: style_of(b),
                });
                continue;
            }
            // An ordered list's item, its number set in its own text: the
            // list counts off from one item to the next, which a section
            // number never does on its own page.
            if !title_like_within(&text, word_limit(&text, b, body)) || block::counts_off(b) {
                if short_caps(&text)
                    && b.lines.len() == 1
                    && b.marker.is_none()
                    && heads_prose(blocks, block_idx, body)
                    && !form_text(&text, blocks, block_idx, body)
                    && !not_a_typographic_heading(&text, blocks, block_idx, body)
                {
                    acronyms.push((b.page, block_idx, text, style_of(b)));
                }
                continue;
            }
            // A line that stops mid-phrase runs on into the next, and a
            // form's keys, captions and labels are its text. Not part of
            // `title_like`, which also counts the bold lines that decide
            // whether bold is used for headings at all.
            let form = ends_mid_phrase(&text) || form_text(&text, blocks, block_idx, body);
            // A numbered title occurs once in a document: sections are not
            // repeated. A numbered line that recurs is a code — `03 —
            // Partial/Moderate assistance`, an assessment form's answer,
            // set larger than the item it answers and numbered like a
            // section, and an `h1` every one of the dozen times it
            // appeared. Every detector below is a guess about typography,
            // and none of them gets a line the numbering has already read.
            if numbering_depth(&text).is_some() && repeated_numbered.contains(&norm(&text)) {
                continue;
            }
            // 2. A display word followed by a lettered item is a structural
            // section label even when it shares the body face and size.
            if blocks
                .get(block_idx + 1)
                .is_some_and(|next| block::display_before_lettered_item(&text, &next.text()))
            {
                out.push(Heading {
                    page: b.page,
                    block_index: block_idx,
                    text,
                    level: 1,
                    evidence: Evidence::Sequence,
                    form_text: false,
                    style: style_of(b),
                });
                continue;
            }
            // 3. Numbering states its own depth — unless the block is a
            // list's item, whose number counts off from the item before it.
            if let Some(depth) = numbering_depth(&text).filter(|_| !initial(&text)) {
                // Set below the body's size, a numbered line is a note: a
                // data sheet's `1) Feature only available with …` at 5pt
                // under 7pt text, or a table's `(1)` notes. Unless set in
                // capitals, as an agreement sets its sections in small
                // capitals over its clauses (`6. AFFIRMATIVE COVENANTS.` at
                // 8pt over 10pt text); a note is set in the text's case.
                let capitals = text.chars().any(char::is_alphabetic)
                    && text
                        .chars()
                        .filter(|c| c.is_alphabetic())
                        .all(char::is_uppercase);
                if (b.font_size as f64) < body as f64 * NUMBERED_NOTE_RATIO && !capitals {
                    continue;
                }
                if b.marker.is_some()
                    || !bare_numbering_is_corroborated(&text, b, body)
                    || coded(&text)
                    || (page_is_coded && has_bare_number_label(&text))
                {
                    continue;
                }
                out.push(Heading {
                    page: b.page,
                    block_index: block_idx,
                    text,
                    level: depth,
                    evidence: Evidence::Numbering,
                    form_text: form,
                    style: style_of(b),
                });
                continue;
            }
            // Typography alone does not make a heading of a caption, a
            // field, a form's label, a list's lead-in or a signature.
            if not_a_typographic_heading(&text, blocks, block_idx, body) {
                continue;
            }
            // Nor of a letterhead: the sender's name over its address and
            // numbers, at the head of the document's first page.
            if page_idx == 0 && at_head(blocks, b) && over_contact(blocks, block_idx) {
                continue;
            }
            // 4. Bold, at body size. Documents very commonly set section
            // titles in the body face at the body size, differing only in
            // weight, which no size-based test can see. Bold set larger is
            // ranked by its size, as other display type is; bold set far
            // smaller is lettering.
            // Read through the form's-text deferral, as other text set
            // like a heading is: dropped here, it would break the run of
            // bold heads #171 reads (`INNOVATION` over `IN THE USA`).
            let lettering = (b.font_size as f64) < lettering_measure * BOLD_HEADING_FLOOR;
            if b.bold
                && !body_is_bold
                && b.font_size as f64 <= body as f64 * (1.0 + HEADING_SIZE_MARGIN)
                && bold_is_structural
                && n_chars <= MAX_FONTSIZE_HEADING_CHARS
                && b.lines.len() <= MAX_FONTSIZE_HEADING_LINES
                && !starts_like_list_item(&text)
            {
                out.push(Heading {
                    page: b.page,
                    block_index: block_idx,
                    text,
                    level: 3,
                    evidence: Evidence::Bold,
                    form_text: form || lettering,
                    style: style_of(b),
                });
                continue;
            }
            // 5. Typography, for documents with neither. Uncorroborated, so
            // it must also look like a heading: short, and not a paragraph.
            // Display type may run longer.
            let char_limit = if b.font_size as f64 >= body as f64 * DISPLAY_HEADING_RATIO {
                MAX_LONG_HEADING_CHARS
            } else {
                MAX_FONTSIZE_HEADING_CHARS
            };
            if n_chars > char_limit
                || b.lines.len() > MAX_FONTSIZE_HEADING_LINES
                || starts_like_list_item(&text)
            {
                continue;
            }
            let key = (b.font_size * 4.0).round() as i32;
            if let Some(&level) = size_level.get(&key) {
                out.push(Heading {
                    page: b.page,
                    block_index: block_idx,
                    text,
                    level,
                    evidence: Evidence::FontSize,
                    form_text: form,
                    style: style_of(b),
                });
                continue;
            }
            // A colon-lead detector ("As a boater:") was tried here in three
            // forms — ungated, gated on a following marked list, gated on
            // inline bullets. Ungated it recovered the poster documents but
            // fired on every "For example:" in the corpus (MHS −0.003 net);
            // gated it recovered nothing because poster block order does not
            // put the introduced list adjacent. Rejected; see MHS_ANALYSIS.
        }
    }
    // An acronym set exactly like a heading on its page, in its size, weight
    // and case, read on the same evidence, and heading prose as a heading
    // does, is one too: `FCC` over a guide's compliance notes, set as its
    // `WATTAGE ADJUSTMENT` is.
    let mut added = false;
    for (page, block_index, text, style) in acronyms {
        let like = out
            .iter()
            .find(|h| {
                h.page == page
                    && h.style == style
                    && !h.form_text
                    && matches!(h.evidence, Evidence::FontSize | Evidence::Bold)
            })
            .map(|h| (h.level, h.evidence));
        if let Some((level, evidence)) = like {
            out.push(Heading {
                page,
                block_index,
                text,
                level,
                evidence,
                form_text: false,
                style,
            });
            added = true;
        }
    }
    if added {
        out.sort_by_key(|h| (h.page, h.block_index));
    }
    open_merged_parts(&mut out, pages, outline);
    drop_kickers(&mut out, pages, document_title);
    normalize_levels(&mut out, pages);
    out
}

/// Block `i` heads prose as a heading does: set apart from the text above
/// it by more than a line's height, with a paragraph of two lines or more at
/// the body's size directly under it. A title's middle word (`OF` between
/// `CERTIFICATE` and the company's name), a person's role under their name,
/// and a form's code over its value do neither.
fn heads_prose(page: &[Block], i: usize, body: f32) -> bool {
    let b = &page[i];
    let fs = (b.font_size as f64).max(1.0);
    let apart = !page.iter().enumerate().any(|(k, o)| {
        k != i
            && o.bbox.y1 <= b.bbox.y0 + fs * 0.2
            && b.bbox.y0 - o.bbox.y1 < fs
            && o.bbox.x1.min(b.bbox.x1) > o.bbox.x0.max(b.bbox.x0)
    });
    let over_text = directly_below(page, i).is_some_and(|k| {
        let p = &page[k];
        p.lines.len() >= 2 && (p.font_size - body).abs() <= 0.15 * body && !p.bold
    });
    apart && over_text
}

/// Fewest and most letters in an acronym set as a heading.
const ACRONYM_LETTERS: std::ops::RangeInclusive<usize> = 2..=3;

/// A line of two or three capitals and nothing else, `FCC`, `UL`: shorter
/// than a title must be to be read as one on its shape alone (a word or
/// two of a sentence, a page's letter, `No`), and a heading only where it
/// is set exactly as the document's headings are.
fn short_caps(text: &str) -> bool {
    let t = text.trim();
    ACRONYM_LETTERS.contains(&t.chars().count()) && t.chars().all(|c| c.is_ascii_uppercase())
}

/// A document merged from files whose outline names the files: no entry's
/// title is printed anywhere in it (`01 - beigebook - Boston_pdf_final_January_2023`,
/// `20250728.PressRelease.ImplementationNote.pdf`). Each entry still lands
/// where its file starts, so the topmost heading on its page, at or below
/// where it lands, opens a part at the entry's level: the district's title
/// on its first page, the implementation note's title under the release
/// line it repeats. An outline that prints any of its titles is the
/// author's, and an entry it does not print is left alone; one whose
/// entries land on pages next to each other marks pages, not files.
fn open_merged_parts(out: &mut [Heading], pages: &[Vec<Block>], outline: &[OutlineItem]) {
    if outline.len() < 2 {
        return;
    }
    let printed = |title: &str| {
        let t = norm(title);
        !t.is_empty() && pages.iter().flatten().any(|b| norm(&b.text()).contains(&t))
    };
    if outline.iter().any(|it| printed(&it.title)) {
        return;
    }
    // Each file runs over pages of its own: an outline that marks every
    // page (`Slide 1`, `Slide 2`) is the pages', not the files'.
    let mut starts: Vec<usize> = outline.iter().filter_map(|it| it.page).collect();
    starts.sort_unstable();
    starts.dedup();
    if starts.windows(2).any(|w| w[1] - w[0] < 2) {
        return;
    }
    for it in outline {
        let Some(pi) = it.page else {
            continue;
        };
        let top = |h: &Heading| {
            pages
                .get(h.page)
                .and_then(|p| p.get(h.block_index))
                .map_or(f64::MAX, |b| b.bbox.y0)
        };
        let lands = it.y.unwrap_or(f64::MIN);
        // The page that carries the document's title opens the whole of
        // it: the entry for the merged document's cover or first file.
        if out
            .iter()
            .any(|h| h.page == pi && h.evidence == Evidence::Title)
        {
            continue;
        }
        let opening = out
            .iter_mut()
            .filter(|h| h.page == pi && !h.form_text)
            .filter(|h| {
                pages
                    .get(h.page)
                    .and_then(|p| p.get(h.block_index))
                    .is_some_and(|b| b.bbox.y1 >= lands)
            })
            .min_by(|a, b| top(a).total_cmp(&top(b)));
        if let Some(h) = opening {
            h.level = it.level;
            h.evidence = Evidence::Outline;
        }
    }
}

/// Widest gap, in ems, between a field's label and its value beside it.
const MAX_LABEL_GAP: f64 = 8.0;

/// A block that bold or size would make a heading but that is something
/// else:
///
/// - a caption, `Figure 5-1. Low-Level Output Voltage`, `Table 4-1. Pin
///   Functions`: the figure's or the table's, never a section's title. The
///   benchmark's ground truth keeps some prominent captions as headings,
///   so this costs a little there;
/// - a label ending in a colon with its value set beside it on its row,
///   `Color:`, or after it on its line at the body's size, `CRI: 90+`: a
///   field (`Current Situation: 2001-2019`, set larger, is a title);
/// - a form's label, a word or three ending in a colon or `#` set large
///   with nothing but its blank beside it, `Location:`, `Model #:`, where
///   only size would make it a heading;
/// - a value set under its bold label at the label's size, in a lighter
///   weight, `K25W951` under `PRODUCT NUMBER`;
/// - a part of a signature block: a party's role and name over the lines
///   it signs on, `INVESTOR:` over `Acme Fund, LP` over `By:`, and a
///   signature page's label, `Signature Page to Voting Agreement`.
///
/// A row of cells set side by side is not tested: a slide's card titles
/// stand side by side as often as a table's header cells do.
fn not_a_typographic_heading(text: &str, page: &[Block], i: usize, body: f32) -> bool {
    let b = &page[i];
    let t = text.trim();
    let larger = b.font_size as f64 > body as f64 * (1.0 + HEADING_SIZE_MARGIN);
    if is_caption(t)
        || (field_on_its_line(t) && (!larger || states_amount(t) || glued_field(t).is_some()))
        || signature_page_label(t)
        || prose_sentence(t)
        || named_value(t) && beside_a_label(page, i)
    {
        return true;
    }
    if labels_a_value(page, i)
        || labels_a_value_under(page, i)
        || in_field_row(page, i)
        || in_postal_address(page, i)
    {
        return true;
    }
    if !b.bold && form_label(t) {
        return true;
    }
    if signs_off(page, i) || value_under_label(page, i) {
        return true;
    }
    let fs = (b.font_size as f64).max(1.0);
    if opens_an_address(b, page) {
        return true;
    }
    t.ends_with(':')
        && page.iter().enumerate().any(|(k, o)| {
            k != i
                && (o.bbox.y0 - b.bbox.y0).abs() <= fs * 0.5
                && o.bbox.x0 >= b.bbox.x1
                && o.bbox.x0 - b.bbox.x1 <= fs * MAX_LABEL_GAP
        })
}

/// A line of fields, each label closing on a colon before its value: `BTW
/// nr. NL: NL8204.71.616.B.02 Nederland IBAN: NL27INGB0000026501 BIC:
/// INGBNL2A`. A title states one label at most (`Policy Rules:
/// Limitations`).
fn fields_run(t: &str) -> bool {
    t.split_whitespace()
        .filter(|w| w.len() > 1 && w.ends_with(':'))
        .count()
        >= 2
}

/// The line is a value that names itself one: a date, a sum, or a code
/// with letters and figures (`13/11/2017`, `€ 717,97`, `MSPE2017`). A bare
/// figure is as often a paragraph's or a page's number, and a step's label
/// (`STEP 4:`) is no value.
fn named_value(t: &str) -> bool {
    date_text(t)
        || amount_text(t)
        || (code_text(t) && t.chars().any(char::is_alphabetic) && !t.trim_end().ends_with(':'))
}

/// Most words in a label set beside a sum it heads (`TOTAL AMOUNT DUE ON
/// August 3 , 2014` beside `$4.11`).
const MAX_SUM_LABEL_WORDS: usize = 8;

/// The block nearest beside block `i` on its row, to its right or left,
/// with nothing between: one line overlapping it by half the shorter's
/// height.
fn beside(page: &[Block], i: usize, right: bool) -> Option<usize> {
    let b = &page[i];
    page.iter()
        .enumerate()
        .filter(|(k, o)| {
            *k != i && {
                let overlap = o.bbox.y1.min(b.bbox.y1) - o.bbox.y0.max(b.bbox.y0);
                overlap >= o.bbox.height().min(b.bbox.height()) * 0.5
                    && if right {
                        o.bbox.x0 >= b.bbox.x1
                    } else {
                        o.bbox.x1 <= b.bbox.x0
                    }
            }
        })
        .min_by(|p, q| {
            let d = |o: &Block| {
                if right {
                    o.bbox.x0 - b.bbox.x1
                } else {
                    b.bbox.x0 - o.bbox.x1
                }
            };
            d(p.1).total_cmp(&d(q.1))
        })
        .map(|(k, _)| k)
}

/// Block `i` is a field's label with its value set beside it on its row,
/// no colon between: a sum anywhere along the row, as an invoice sets its
/// totals against the right margin (`Totaal` … `€ 717,97`), or a date, a
/// figure or a code within [`MAX_LABEL_GAP`] (`Date de facture`
/// `13/11/2017`, `Réf. marché` `MSPE2017`). A heading beside a column of
/// text reaches no value: the text runs on past a few words.
fn labels_a_value(page: &[Block], i: usize) -> bool {
    let b = &page[i];
    let words = b.text().split_whitespace().count();
    let Some(k) = beside(page, i, true) else {
        return false;
    };
    let v = &page[k];
    if v.lines.len() != 1 || b.lines.len() != 1 {
        return false;
    }
    let text = v.text();
    let fs = (b.font_size as f64).max(1.0);
    // The value stands alone on its row: a paragraph's number beside a
    // margin's heading has its paragraph after it (`Rescue equipment` …
    // `156` … `Suitable rescue equipment…`). A row of sums is a totals row
    // across a statement's columns (`Subtotals` … `$1,200.00` … `$35.10`).
    let alone = beside(page, k, true).is_none_or(|n| {
        page[n].bbox.x0 - v.bbox.x1 > fs * MAX_LABEL_GAP || amount_text(&page[n].text())
    });
    alone
        && ((amount_text(&text) && words <= MAX_SUM_LABEL_WORDS)
            || (named_value(&text)
                && words <= MAX_FIELD_LABEL_WORDS
                && v.bbox.x0 - b.bbox.x1 <= fs * MAX_LABEL_GAP))
}

/// Fewest letters and figures in a code that is a field's value. A
/// contents' page reference is shorter (`E-1`, `A-12`).
const MIN_CODE_CHARS: usize = 5;

/// The line is one code: a single token holding a figure (`MSPE2017`,
/// `06AABCO6063D1ZQ`, `FR19787878784`).
fn code_text(t: &str) -> bool {
    let t = t.trim();
    !t.contains(char::is_whitespace)
        && t.chars().filter(|c| c.is_alphanumeric()).count() >= MIN_CODE_CHARS
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '/' | '-' | '_' | '#'))
}

/// Widest gap, in the label's ems, between a label and the code, date or
/// sum set under it.
const UNDER_VALUE_GAP: f64 = 2.0;

/// Block `i` is a field's label with its value set under it: a code of
/// letters and figures, or a sum, on one line, no larger than the label,
/// within two ems (`GSTIN` over `06AABCO6063D1ZQ`). The value may be set
/// bold as the label is. A heading over its section's first line stands
/// over words, and a title over its date (`Statement of Work` over the
/// day it was signed) is a title.
fn labels_a_value_under(page: &[Block], i: usize) -> bool {
    let b = &page[i];
    // In a row of such labels: one heading over a model's code
    // (`Dimensions`) is a heading.
    over_code(page, i)
        && page.iter().enumerate().any(|(j, o)| {
            let overlap = o.bbox.y1.min(b.bbox.y1) - o.bbox.y0.max(b.bbox.y0);
            j != i
                && o.bold == b.bold
                && (o.font_size - b.font_size).abs() <= 0.5
                && overlap >= o.bbox.height().min(b.bbox.height()) * 0.5
                && over_code(page, j)
        })
}

/// Block `i` is a short label over a code of letters and figures, or a
/// sum, on one line no larger than it, within two ems.
fn over_code(page: &[Block], i: usize) -> bool {
    let b = &page[i];
    let t = b.text();
    if b.lines.len() != 1
        || t.split_whitespace().count() > MAX_FORM_LABEL_WORDS
        || t.chars().any(|c| c.is_ascii_digit())
    {
        return false;
    }
    let fs = (b.font_size as f64).max(1.0);
    let under = page
        .iter()
        .enumerate()
        .filter(|(k, o)| {
            *k != i
                && o.bbox.y0 >= b.bbox.y1 - fs * 0.2
                && o.bbox.y0 - b.bbox.y1 <= fs * UNDER_VALUE_GAP
                && o.bbox.x1.min(b.bbox.x1) > o.bbox.x0.max(b.bbox.x0)
        })
        .min_by(|(_, a), (_, c)| a.bbox.y0.total_cmp(&c.bbox.y0));
    let Some((_, v)) = under else {
        return false;
    };
    let text = v.text();
    v.lines.len() == 1
        && v.font_size <= b.font_size + 0.25
        && ((code_text(&text) && text.chars().any(char::is_alphabetic)) || amount_text(&text))
}

/// Block `i` is a label in a row of a form's fields: it shares its row with
/// a label set alike whose value stands beside or under it, and a short
/// line stands under it (`Contact commercial` over `Tony Dubois`, beside
/// `Date de facture` and its `13/11/2017`).
fn in_field_row(page: &[Block], i: usize) -> bool {
    let b = &page[i];
    let t = b.text();
    if b.lines.len() != 1 || t.split_whitespace().count() > MAX_FIELD_LABEL_WORDS {
        return false;
    }
    let under_short = directly_below(page, i).is_some_and(|k| {
        let v = &page[k];
        v.lines.len() <= MAX_FIELD_VALUE_LINES
            && !v.bold
            && v.lines.iter().all(|l| {
                l.text.split_whitespace().count() <= MAX_FIELD_VALUE_WORDS
                    && !l.text.trim_end().ends_with('.')
            })
    });
    under_short
        && page.iter().enumerate().any(|(j, o)| {
            let overlap = o.bbox.y1.min(b.bbox.y1) - o.bbox.y0.max(b.bbox.y0);
            j != i
                && o.bold == b.bold
                && (o.font_size - b.font_size).abs() <= 0.5
                && overlap >= o.bbox.height().min(b.bbox.height()) * 0.5
                && (labels_a_value(page, j) || labels_a_value_under(page, j))
        })
}

/// Block `i` is the value set beside a label that closes on a colon, within
/// [`MAX_LABEL_GAP`]: `Prélèvement à partir du :` … `05 Juillet 2015`.
fn beside_a_label(page: &[Block], i: usize) -> bool {
    let b = &page[i];
    let Some(k) = beside(page, i, false) else {
        return false;
    };
    let label = &page[k];
    let fs = (label.font_size as f64).max(1.0);
    label.text().trim_end().ends_with(':')
        && b.lines.len() == 1
        && b.text().split_whitespace().count() <= MAX_FIELD_VALUE_WORDS
        && b.bbox.x0 - label.bbox.x1 <= fs * MAX_LABEL_GAP
}

/// A form's or a chart's text set like a heading: a grid's keys and answer
/// heads, a line's caption, a footer that refers elsewhere, a form's
/// number, a label over its value or among its header's labels; an axis's
/// title, set up its side or under its scale.
fn form_text(text: &str, page: &[Block], i: usize, body: f32) -> bool {
    let t = text.trim();
    keys_or_answers(t)
        || refers_elsewhere(t)
        || form_number(t)
        || in_header_band(page, i, body)
        || line_caption(t)
        || label_over_value(page, i)
        || stacked_field(page, i)
        || turned(page, &page[i])
        || under_a_scale(page, i)
        || heads_figures(page, i)
}

/// Answers a form offers to tick: the heads of its answer columns.
const ANSWER_WORDS: &[&str] = &["yes", "no", "n/a", "na"];

/// A grid's column keys or a form's answer heads, and nothing else:
/// `(a) (b)`, `(f) (g)`, `Yes No`. They head columns, not sections.
fn keys_or_answers(t: &str) -> bool {
    let key = |w: &str| {
        w.strip_prefix('(')
            .and_then(|w| w.strip_suffix(')'))
            .is_some_and(|k| {
                (1..=3).contains(&k.chars().count()) && k.chars().all(char::is_alphanumeric)
            })
    };
    let mut words = t.split_whitespace().peekable();
    words.peek().is_some()
        && words.all(|w| key(w) || ANSWER_WORDS.contains(&w.to_lowercase().as_str()))
}

/// A sentence that sends the reader elsewhere, `For Paperwork Reduction
/// Act Notice, see your tax return instructions.`: a form's footer. No
/// section is titled with a cross-reference; a title that only mentions
/// one keeps it in brackets, `Other Information (see instructions)`.
fn refers_elsewhere(t: &str) -> bool {
    let lower = t.to_lowercase();
    if lower.contains(", see ") {
        return true;
    }
    // An instruction to look elsewhere, `See the separate instructions.`,
    // `See page 2`. A section of further reading is `See also`.
    let mut words = lower.split_whitespace();
    words.next() == Some("see")
        && words
            .next()
            .is_some_and(|w| REFERRED_TO.contains(&w.trim_matches(|c: char| !c.is_alphanumeric())))
}

/// What an instruction sends the reader to, after `See`.
const REFERRED_TO: &[&str] = &[
    "the",
    "instructions",
    "page",
    "pages",
    "note",
    "notes",
    "separate",
    "attached",
    "below",
    "above",
    "schedule",
    "form",
    "line",
    "lines",
    "part",
    "item",
    "back",
    "reverse",
];

/// A form's number or edition, set in its head or foot: `OMB No.
/// 1545-0112`, `Cat. No. 37768Z`, `Schedule D (Form 1040) 2017`,
/// `Form 8949 (2025)`. A part a form names, `Schedule K`, has no edition
/// beside it.
fn form_number(t: &str) -> bool {
    let lower = t.to_lowercase();
    if lower.starts_with("omb no")
        || lower.starts_with("cat. no")
        || lower.starts_with("omb control")
    {
        return true;
    }
    // The form it belongs to, in brackets: `Schedule D (Form 1040) 2017`.
    let in_brackets = lower
        .split_once("(form ")
        .is_some_and(|(_, rest)| rest.starts_with(|c: char| c.is_ascii_digit()));
    // A form's number and its edition: `Form 8949 (2025)`, `Form 1040 (Rev. 1-2024)`.
    let words: Vec<&str> = t.split_whitespace().collect();
    let edition = words.len() >= 3
        && words[0] == "Form"
        && words[1].starts_with(|c: char| c.is_ascii_digit())
        && words[2].starts_with('(')
        && (words[2][1..].starts_with(|c: char| c.is_ascii_digit())
            || words[2].starts_with("(Rev"));
    in_brackets || edition
}

/// Most words in one of the labels of a form's header band.
const MAX_BAND_WORDS: usize = 8;

/// Fewest labels set beside a form's header label, within a line of it.
const MIN_BAND_LABELS: usize = 3;

/// A label in a form's header band, its grid not read as a table: set
/// bold at the body's size or smaller, with three or more short labels or
/// figures beside it, within a line above or below. Columns' heads stack
/// over several lines and start at different heights, `Adjustment, if
/// any, to gain or loss` over `If you enter an amount in column (g),`
/// beside `(e)`, `(h)` and `Cost or other basis`; a statement's
/// `Subtotals` leads its row of figures. A slide's card titles, side by
/// side, are set larger than the text under them, and a heading in one of
/// a page's columns heads the paragraph under it.
fn in_header_band(page: &[Block], i: usize, body: f32) -> bool {
    let b = &page[i];
    if b.font_size > body * 1.05 || b.text().split_whitespace().count() > MAX_BAND_WORDS {
        return false;
    }
    let h = b.bbox.height().max(1.0) / b.lines.len().max(1) as f64;
    // A list's item, its bullet set apart or in its text.
    let listed = |o: &Block| o.marker.is_some() || block::strip_leading_bullet(&o.text()).is_some();
    // A heading over its text: a paragraph of two lines or more, a long
    // line or a list's item, directly under it in its column (`PVC and
    // phthalates` over its paragraph, beside the other columns' heads;
    // `Performance` over its bullets). A header's label has the next label
    // or the grid under it.
    let over_text = page
        .iter()
        .enumerate()
        .filter(|(k, o)| {
            *k != i
                && o.bbox.y0 >= b.bbox.y1 - h * 0.2
                && o.bbox.y0 - b.bbox.y1 <= h * 2.0
                && o.bbox.x1.min(b.bbox.x1) > o.bbox.x0.max(b.bbox.x0)
        })
        .min_by(|(_, x), (_, y)| x.bbox.y0.total_cmp(&y.bbox.y0))
        .is_some_and(|(_, o)| {
            o.lines.len() >= 2 || o.text().split_whitespace().count() > MAX_BAND_WORDS || listed(o)
        });
    if over_text {
        return false;
    }
    let (mut left, mut right) = (0, 0);
    for (k, o) in page.iter().enumerate() {
        // A list's items are no form's labels: another column's bullets.
        if k == i
            || listed(o)
            || o.text().split_whitespace().count() > MAX_BAND_WORDS
            || o.bbox.y1 <= b.bbox.y0 - h
            || o.bbox.y0 >= b.bbox.y1 + h
        {
            continue;
        }
        if o.bbox.x1 <= b.bbox.x0 {
            left += 1;
        } else if o.bbox.x0 >= b.bbox.x1 {
            right += 1;
        }
    }
    // A band runs across the grid, on both sides of a label within it. At
    // its edge, the last column's head has the band all on one side; a
    // stamp or a sidebar beside a title has a line or three.
    (left > 0 && right > 0 && left + right >= MIN_BAND_LABELS)
        || left.max(right) >= MIN_EDGE_BAND_LABELS
}

/// Fewest labels beside a header label on one side alone: the band's
/// edge column's head.
const MIN_EDGE_BAND_LABELS: usize = 5;

/// A form's line caption with its line's letter before it and its number
/// after it: `b Refunded 37b`.
fn line_caption(t: &str) -> bool {
    let words: Vec<&str> = t.split_whitespace().collect();
    let letter = |w: &str| w.len() == 1 && w.chars().all(|c| c.is_ascii_lowercase());
    let number = |w: &str| {
        let digits = w.trim_end_matches(|c: char| c.is_ascii_lowercase());
        w.len() == digits.len() + 1
            && (1..=3).contains(&digits.len())
            && digits.chars().all(|c| c.is_ascii_digit())
    };
    words.len() >= 3 && letter(words[0]) && number(words[words.len() - 1])
}

/// Most words in a value under its label, `Jun 5, 2026`, counting only
/// words with a letter or figure: a catalogue's group over its first line
/// (`Coussinets` over `223-3000 Coussinets d'axes de piston`) runs longer.
const MAX_LABEL_VALUE_WORDS: usize = 3;

/// A bold label over its value on the next line: the value set lighter at
/// the label's size within an em, a single short line holding a figure or
/// an answer, and followed by the next label or nothing (`Date of
/// Preparation` over `Jun 5, 2026`, `FLASH POINT` over `N.A.`,
/// `Photochemically Reactive` over `No`). A section's heading stands over
/// sentences, or over more lines than one.
fn label_over_value(page: &[Block], i: usize) -> bool {
    let label = &page[i];
    if !label.bold {
        return false;
    }
    let Some(k) = directly_below(page, i) else {
        return false;
    };
    let v = &page[k];
    let fs = (label.font_size as f64).max(1.0);
    // The value stands alone: the next label or nothing follows it. Under a
    // heading, its first line runs on into more of the same (`Power and
    // Ratings` over `120 V~ 60 Hz` over the rest of its ratings).
    let alone = directly_below(page, k).is_none_or(|n| page[n].bold);
    let text = v.text();
    alone
        && !v.bold
        && (v.font_size - label.font_size).abs() <= 0.25
        && v.bbox.y0 - label.bbox.y1 <= fs * VALUE_GAP
        && v.lines.len() == 1
        && text
            .split_whitespace()
            .filter(|w| w.chars().any(char::is_alphanumeric))
            .count()
            <= MAX_LABEL_VALUE_WORDS
        && (text.chars().any(|c| c.is_ascii_digit()) || answers(&text))
}

/// Most lines of a field's value set under its label: an address's
/// street, town and postcode.
const MAX_FIELD_VALUE_LINES: usize = 3;

/// Least a field's value is set in from its label, in the label's ems: a
/// value hangs under its label, where a section's text starts from its
/// heading's edge.
const FIELD_INDENT: f64 = 0.5;

/// The block under bold label `i` that is its value: lighter, at the
/// label's size, within an em under it, set in from it, a line or three
/// that is no sentence and no list's item. A value is a name, a code or an
/// address; a question's answer set as a bullet under it is the question's
/// text.
fn field_value(page: &[Block], i: usize) -> Option<usize> {
    let label = &page[i];
    if !label.bold {
        return None;
    }
    let k = directly_below(page, i)?;
    let v = &page[k];
    let fs = (label.font_size as f64).max(1.0);
    (!v.bold
        && (v.font_size - label.font_size).abs() <= 0.25
        && v.bbox.y0 - label.bbox.y1 <= fs * VALUE_GAP
        && v.bbox.x0 - label.bbox.x0 >= fs * FIELD_INDENT
        && v.lines.len() <= MAX_FIELD_VALUE_LINES
        && !v.text().trim_end().ends_with('.')
        && v.marker.is_none()
        && !starts_like_list_item(&v.text()))
    .then_some(k)
}

/// A bold label over its value, in a stack of labels set alike, each over
/// its own: `PRODUCT NUMBER` over `K25W951`, `PRODUCT NAME` over `Symmetry
/// Interior Latex Matte, Extra White`, `MANUFACTURER'S NAME` over its
/// address. A label set alike is bold at the same size from the same edge,
/// within half an em, and stands under this label's value or over the
/// value this label stands under. One label over a line set in is a
/// heading over its text as often as a field: the stack tells them apart.
/// A field's label is a few words and states no section number; a
/// numbered title over its table's first lines is a heading.
fn stacked_field(page: &[Block], i: usize) -> bool {
    let label = &page[i];
    let words = label.text().split_whitespace().count();
    if words > MAX_FIELD_LABEL_WORDS || numbering_depth(&label.text()).is_some() {
        return false;
    }
    let Some(k) = field_value(page, i) else {
        return false;
    };
    let fs = (label.font_size as f64).max(1.0);
    let alike = |j: usize| {
        let o = &page[j];
        j != i
            && o.bold
            && (o.font_size - label.font_size).abs() <= 0.25
            && (o.bbox.x0 - label.bbox.x0).abs() <= fs * 0.5
            && field_value(page, j).is_some()
    };
    let next = directly_below(page, k).is_some_and(alike);
    let prev = (0..page.len())
        .any(|j| alike(j) && field_value(page, j).and_then(|v| directly_below(page, v)) == Some(i));
    next || prev
}

/// The line is an answer and nothing else: `No`, `N.A.`.
fn answers(t: &str) -> bool {
    let mut words = t.split_whitespace().peekable();
    words.peek().is_some()
        && words.all(|w| ANSWER_WORDS.contains(&w.replace('.', "").to_lowercase().as_str()))
}

/// Most words in a field's label, `Initial Closing Date`, and in its
/// value after the colon, `May 15, 2019`.
const MAX_FIELD_LABEL_WORDS: usize = 4;
const MAX_FIELD_VALUE_WORDS: usize = 6;

/// A label and its value on one line, the value a figure, a date or a
/// code, short or opening with its figure: `CRI: 90+`, `Hours: 8:30 am -
/// 6:00 pm EST (Monday - Friday)`. A title with a
/// subtitle, `Policy Rules: Limitations`, `Part II: Application of
/// Section 67 Limitation to Specific Deductions`, states no figure, or
/// runs on past a value's length.
fn field_on_its_line(t: &str) -> bool {
    let Some((label, value)) = t.split_once(": ").or_else(|| glued_field(t)) else {
        return false;
    };
    let lw = label.split_whitespace().count();
    let vw = value.split_whitespace().count();
    let figure = value.trim_start().starts_with(|c: char| c.is_ascii_digit());
    (1..=MAX_FIELD_LABEL_WORDS).contains(&lw)
        && (figure || (1..=MAX_FIELD_VALUE_WORDS).contains(&vw))
        && value.chars().any(|c| c.is_ascii_digit())
}

/// A label and its figure joined by a colon with no space, as a form
/// prints a field (`Kundennummer:2`). A title sets a space after its colon.
fn glued_field(t: &str) -> Option<(&str, &str)> {
    let (label, value) = t.split_once(':')?;
    (!label.is_empty()
        && !label.ends_with(' ')
        && label
            .chars()
            .all(|c| c.is_alphabetic() || c == ' ' || c == '.')
        && value.starts_with(|c: char| c.is_ascii_digit()))
    .then_some((label, value))
}

/// Most words in a form's label.
const MAX_FORM_LABEL_WORDS: usize = 3;

/// A form's label: a word or three ending in a colon or a `#`, `Location:`,
/// `Model #:`, `Item #`.
pub(crate) fn form_label(t: &str) -> bool {
    (t.ends_with(':') || t.ends_with('#')) && t.split_whitespace().count() <= MAX_FORM_LABEL_WORDS
}

/// `Signature Page to Voting Agreement`, `[Signature Page Follows]`: the
/// label an agreement sets on the page its parties sign.
fn signature_page_label(t: &str) -> bool {
    t.trim_start_matches(['[', '('])
        .to_lowercase()
        .starts_with("signature page")
}

/// How many blocks after a party's role or name the lines it signs on
/// may start: `INVESTOR:` over the fund's name over `By:`.
const SIGNATURE_REACH: usize = 3;

/// Most words in a party's role or name.
const MAX_SIGNATORY_WORDS: usize = 10;

/// Block `i` is a party's role or name in a signature block: a line to
/// sign on follows it, `By:`, `Its:`, `(Signature)`, a rule of
/// underscores, or a `Name:` with a `Title:` after it, with only other
/// roles and names between. Text between, a paragraph that ends a section
/// before its signature (`Notation Vote` over the vote's paragraph over
/// the Secretary's rule), says the block heads that text instead.
fn signs_off(page: &[Block], i: usize) -> bool {
    let short = |b: &Block| b.text().split_whitespace().count() <= MAX_SIGNATORY_WORDS;
    if !short(&page[i]) {
        return false;
    }
    // Under a line to sign on, alone on its line, or a signature typed as
    // `/s/`: the signer's name and office printed under it (`____` over
    // `Joshua Gallin` over `Secretary`, `/s/ J. Duato` over `Joaquin
    // Duato`).
    for b in page[..i].iter().rev().take(SIGNATURE_REACH - 1) {
        let t = b.text();
        if bare_signing_rule(t.trim()) || t.trim_start().starts_with("/s/") {
            return true;
        }
        if !short(b) {
            break;
        }
    }
    // Over an office: the signer's name directly over it, or a company's
    // over the signer's name over it (`Arcadia Consulting Group` over
    // `Chiara Fontaine` over `Commercial Counsel`). A section over a list
    // of officers (`Board of Directors` over a name over `Chair`) is
    // neither.
    let next = |k: usize| page.get(i + k).map(|b| b.text());
    let same = |a: &str, b: &str| a.split_whitespace().eq(b.split_whitespace());
    if next(1).is_some_and(|t| office_line(&t))
        // A signature set as text over the same name printed, over the
        // office (`Kate E. Brandt` in a script over `Kate E. Brandt` over
        // `Chief Sustainability Officer, Google`).
        || (next(1).is_some_and(|t| same(&t, &page[i].text()))
            && next(2).is_some_and(|t| office_line(&t)))
        || (company_name(&page[i].text())
            && next(1).is_some_and(|t| t.split_whitespace().count() <= 4)
            && next(2).is_some_and(|t| office_line(&t)))
    {
        return true;
    }
    let after: Vec<&Block> = page.iter().skip(i + 1).take(SIGNATURE_REACH).collect();
    for (k, b) in after.iter().enumerate() {
        let t = b.text();
        let lower = t.trim().to_lowercase();
        let signs = lower.starts_with("by:")
            || lower.starts_with("its:")
            || lower.starts_with("(signature")
            || lower.starts_with("signature:")
            || lower.starts_with("/s/")
            || signing_rule(t.trim())
            || (lower.starts_with("name:")
                && after
                    .iter()
                    .skip(k + 1)
                    .any(|n| n.text().trim().to_lowercase().starts_with("title:")));
        if signs {
            return true;
        }
        if !short(b) {
            return false;
        }
    }
    false
}

/// A line to sign on and nothing else: underscores alone.
fn bare_signing_rule(t: &str) -> bool {
    t.chars().filter(|c| *c == '_').count() >= 4 && t.chars().all(|c| c == '_' || c.is_whitespace())
}

/// Words that name an office a signer holds.
const OFFICE_WORDS: &[&str] = &[
    "secretary",
    "president",
    "chairman",
    "chairwoman",
    "chairperson",
    "director",
    "officer",
    "counsel",
    "treasurer",
    "manager",
    "partner",
    "trustee",
    "governor",
    "commissioner",
    "ceo",
    "cfo",
    "coo",
    "cto",
    "founder",
    "notary",
    "attorney",
    "signatory",
];

/// A signer's office on a line of its own: a few words naming it, no
/// figures or colon (`Secretary`, `Chief Executive Officer`, `Commercial
/// Counsel`, `Vice President, Finance`). Before a comma the office comes
/// first: `Jerome H. Powell, Chair` is a name with the office after it, a
/// list's entry under `Attendance`. Nor does an office make a sentence:
/// `CHAIR POWELL. Good morning, everyone.` is a speaker's line, and
/// `MANAGER NAME` a form's field. The office opens its line, `Secretary
/// of State`: `CERTIFICATE OF SECRETARY` is a document's title.
fn office_line(t: &str) -> bool {
    let t = t.trim();
    let words: Vec<&str> = t.split_whitespace().collect();
    (1..=5).contains(&words.len())
        && t.starts_with(|c: char| c.is_uppercase())
        && !t.contains(':')
        && !t.chars().any(|c| c.is_ascii_digit())
        && !t.contains(". ")
        && !t.ends_with(['.', '!', '?'])
        && !words
            .iter()
            .any(|w| FIELD_WORDS.contains(&w.to_lowercase().as_str()))
        && office_in(
            t.to_lowercase()
                .split(',')
                .next()
                .unwrap_or("")
                .split(" of ")
                .next()
                .unwrap_or(""),
        )
}

/// Words that make a line a form's field rather than an office:
/// `MANAGER NAME`, `Officer Signature`.
const FIELD_WORDS: &[&str] = &[
    "name",
    "signature",
    "date",
    "title",
    "number",
    "address",
    "approval",
];

/// The words name an office: one of [`OFFICE_WORDS`] among them.
fn office_in(t: &str) -> bool {
    t.split_whitespace().any(|w| {
        OFFICE_WORDS.contains(
            &w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
                .as_str(),
        )
    })
}

/// What closes a company's name: `Group`, `Inc.`, `LLC`, `Ltd`, `LP`.
const COMPANY_SUFFIXES: &[&str] = &[
    "inc",
    "llc",
    "ltd",
    "lp",
    "llp",
    "plc",
    "corp",
    "corporation",
    "company",
    "co",
    "group",
    "gmbh",
    "ag",
    "sa",
    "bv",
    "pbc",
    "partners",
    "bank",
    "trust",
    "foundation",
];

/// A company's name: its last word one of [`COMPANY_SUFFIXES`].
fn company_name(t: &str) -> bool {
    t.split_whitespace().next_back().is_some_and(|w| {
        COMPANY_SUFFIXES.contains(
            &w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
                .as_str(),
        )
    })
}

/// A rule to sign on, typed as underscores: most of the line is `_`.
fn signing_rule(t: &str) -> bool {
    let underscores = t.chars().filter(|c| *c == '_').count();
    underscores >= 4 && underscores * 2 >= t.chars().filter(|c| !c.is_whitespace()).count()
}

/// Block `i`, in a lighter weight, is set at its size directly under a bold
/// label, alone or under other such lines: the label's value, `K25W951`
/// under `PRODUCT NUMBER`, a company's name and address under
/// `MANUFACTURER'S NAME`. A section under its title is set apart from it
/// by size or a gap; a value sits on the label's next line.
fn value_under_label(page: &[Block], i: usize) -> bool {
    if page[i].bold {
        return false;
    }
    let same = |a: &Block, b: &Block| (a.font_size - b.font_size).abs() <= 0.25;
    let mut below = &page[i];
    for above in page[..i].iter().rev() {
        let fs = (below.font_size as f64).max(1.0);
        let gap = below.bbox.y0 - above.bbox.y1;
        let overlaps = above.bbox.x1.min(below.bbox.x1) > above.bbox.x0.max(below.bbox.x0);
        if !same(above, below) || !overlaps || gap < -fs * 0.2 || gap > fs * VALUE_GAP {
            return false;
        }
        if above.bold {
            return above.lines.len() == 1;
        }
        below = above;
    }
    false
}

/// The block runs at an angle to the page's text: an axis's title set up
/// its side (`Concentration of PFHpS, in ng/L`, `Recovery, in percent`), a
/// tab or a mark in the margin. A page set on its side as a whole turns
/// its headings with its text, and they stay. Level text is never turned:
/// where a table is read, what it leaves may be mostly its turned column
/// labels.
fn turned(page: &[Block], b: &Block) -> bool {
    let mut by_angle: HashMap<i32, usize> = HashMap::new();
    for l in page.iter().flat_map(|o| &o.lines) {
        *by_angle.entry(l.rotation_bucket).or_default() += 1;
    }
    let text = by_angle
        .into_iter()
        .max_by_key(|(angle, n)| (*n, *angle == 0))
        .map_or(0, |(angle, _)| angle);
    b.lines
        .iter()
        .any(|l| l.rotation_bucket != 0 && l.rotation_bucket != text)
}

/// A table's figure, a cell of nothing else: `2023 2022`, `1000 -`,
/// `64.1 - 90.0`, `80.0 (28/35)`, `$ 2,395`.
fn bare_figures(t: &str) -> bool {
    let t = t.trim();
    t.chars().any(|c| c.is_ascii_digit())
        && t.chars().all(|c| {
            c.is_ascii_digit()
                || c.is_whitespace()
                || ".,%-−–+×/()$€£:".contains(c)
                || ('\u{2070}'..='\u{209F}').contains(&c)
        })
}

/// Widest gap, in ems, from a column's head to the first figure under it:
/// a table's rows stand further apart than a paragraph's lines.
const FIGURES_GAP: f64 = 2.5;

/// Most words in a cell set level with a table's column head.
const MAX_HEAD_CELL_WORDS: usize = 4;

/// A table's column head or the stub's, the table not read as one: the
/// block stands directly over a line of bare figures (`Total SOMA` over
/// `2023 2022`, `TWA STEL` over `1000 -`), or stands on a row of other
/// short cells, it or one of them over such figures (`Item` beside
/// `2024:Q2–2025:Q2` and `1997–2025:Q2`, `95%CI` beside `95% CI` and `%`).
/// A slide's card titles stand side by side too, over text, not figures.
fn heads_figures(page: &[Block], i: usize) -> bool {
    let over = |k: usize| {
        let b = &page[k];
        let fs = (b.font_size as f64).max(1.0);
        page.iter()
            .enumerate()
            .filter(|(n, o)| {
                *n != k
                    && o.bbox.y0 >= b.bbox.y1 - fs * 0.2
                    && o.bbox.y0 - b.bbox.y1 <= fs * FIGURES_GAP
                    && o.bbox.x1.min(b.bbox.x1) > o.bbox.x0.max(b.bbox.x0)
            })
            .min_by(|(_, x), (_, y)| x.bbox.y0.total_cmp(&y.bbox.y0))
            .is_some_and(|(_, o)| o.lines.len() == 1 && bare_figures(&o.text()))
    };
    if over(i) {
        return true;
    }
    let b = &page[i];
    let fs = (b.font_size as f64).max(1.0);
    page.iter().enumerate().any(|(k, o)| {
        k != i
            && o.lines.len() == 1
            && o.text().split_whitespace().count() <= MAX_HEAD_CELL_WORDS
            && (o.bbox.y0 - b.bbox.y0).abs() <= fs * 0.5
            && (o.bbox.x0 >= b.bbox.x1 || o.bbox.x1 <= b.bbox.x0)
            && over(k)
    })
}

/// Fewest values on a row for the row to be a chart's scale.
const MIN_SCALE_TICKS: usize = 3;

/// A tick's value: a figure with its signs and separators, `1,000`,
/// `0.25`, `−30`, `10⁻¹`, `50%`.
fn tick_value(t: &str) -> bool {
    let t = t.trim();
    t.chars().any(|c| c.is_ascii_digit())
        && t.chars().all(|c| {
            c.is_ascii_digit()
                || matches!(c, '.' | ',' | '%' | '-' | '−' | '+' | '×' | ' ')
                || ('\u{2070}'..='\u{209F}').contains(&c)
                || matches!(c, '¹' | '²' | '³')
        })
}

/// An axis's title under its scale: the block is set directly under a row
/// of [`MIN_SCALE_TICKS`] or more bare values that spans it (`I
/// OutputCurrentA` under `1 1.5 2 2.5 3 3.5`, `Expected concentration, in
/// ng/L` under `1 10 100 1,000`). A chart drawn as vectors keeps its labels
/// among the page's text, out of any figure region.
fn under_a_scale(page: &[Block], i: usize) -> bool {
    let b = &page[i];
    let fs = (b.font_size as f64).max(1.0);
    let ticks: Vec<&Block> = page
        .iter()
        .enumerate()
        .filter(|(k, o)| {
            *k != i
                && o.lines.len() == 1
                && tick_value(&o.text())
                && o.bbox.y1 <= b.bbox.y0 + fs * 0.3
                && b.bbox.y0 - o.bbox.y1 <= fs * BELOW_GAP
        })
        .map(|(_, o)| o)
        .collect();
    // The ticks set on one row, together reaching across the block.
    ticks.iter().any(|t| {
        let row: Vec<&&Block> = ticks
            .iter()
            .filter(|o| (o.bbox.y0 - t.bbox.y0).abs() <= fs * 0.3)
            .collect();
        let x0 = row.iter().map(|o| o.bbox.x0).fold(f64::MAX, f64::min);
        let x1 = row.iter().map(|o| o.bbox.x1).fold(f64::MIN, f64::max);
        row.len() >= MIN_SCALE_TICKS && x0 <= b.bbox.x1 && x1 >= b.bbox.x0
    })
}

/// Widest gap, in ems, between a label and its value on the next line.
const VALUE_GAP: f64 = 1.0;

/// Share of a page's text, from its top, that its head takes: where a
/// letterhead is set.
const PAGE_HEAD: f64 = 0.15;

/// The block starts in the head of its page's text.
fn at_head(page: &[Block], b: &Block) -> bool {
    let top = page.iter().map(|o| o.bbox.y0).fold(f64::MAX, f64::min);
    let bottom = page.iter().map(|o| o.bbox.y1).fold(f64::MIN, f64::max);
    b.bbox.y0 <= top + (bottom - top) * PAGE_HEAD
}

/// Widest gap, in ems, from a block to the one it sets next under it: a
/// letterhead's name over its smaller address lines.
const BELOW_GAP: f64 = 1.5;

/// The block set next under block `i`, sharing some of its width, within
/// a line's space: the next line of what `i` begins, whatever the reading
/// order puts between them.
fn directly_below(page: &[Block], i: usize) -> Option<usize> {
    let b = &page[i];
    let fs = (b.font_size as f64).max(1.0);
    page.iter()
        .enumerate()
        .filter(|(k, o)| {
            *k != i
                && o.bbox.y0 >= b.bbox.y1 - fs * 0.2
                && o.bbox.y0 - b.bbox.y1 <= fs * BELOW_GAP
                && o.bbox.x1.min(b.bbox.x1) > o.bbox.x0.max(b.bbox.x0)
        })
        .min_by(|(_, a), (_, c)| a.bbox.y0.total_cmp(&c.bbox.y0))
        .map(|(k, _)| k)
}

/// Lines a letterhead may set between its name and its address: a
/// division's name.
const LETTERHEAD_LINES: usize = 2;

/// Widest difference in size, in points, between a letterhead's name and a
/// line it sets under it as part of the name.
const LETTERHEAD_SIZE_STEP: f32 = 0.5;

/// Block `i` stands over an address block, directly or over a line or two
/// set like it: `TRUE MANUFACTURING CO., INC.` over `U.S.A. FOODSERVICE
/// DIVISION` over its street, phone and web address. A line set otherwise
/// between them is not the name's: an article's title stands over its
/// authors, and they over the affiliations that give their e-mail.
fn over_contact(page: &[Block], i: usize) -> bool {
    let name = &page[i];
    let mut k = i;
    for _ in 0..=LETTERHEAD_LINES {
        let Some(next) = directly_below(page, k) else {
            return false;
        };
        let b = &page[next];
        if states_contact(b) {
            return true;
        }
        if b.bold != name.bold || (b.font_size - name.font_size).abs() > LETTERHEAD_SIZE_STEP {
            return false;
        }
        k = next;
    }
    false
}

/// Lines an address runs over, at most, from its first to the line with
/// its phone number or web address.
const ADDRESS_LINES: usize = 3;

/// Does `b` open an address: the lines set flush under it, each within a
/// line and a half of the last, reach one closing on a phone number or a
/// web address (`Owens Corning Insulating Systems, LLC` over `One Owens
/// Corning Parkway…` over `1-800-GET-PINK® www.owenscorning.com`)? Its
/// first line names a company or a person, set bold, and is no section's
/// title.
fn opens_an_address(b: &Block, page: &[Block]) -> bool {
    let fs = (b.font_size as f64).max(1.0);
    // An address's lines are short: a heading over a list of references,
    // each closing on its link, opens none.
    let short = |o: &Block| o.text().split_whitespace().count() <= ADDRESS_WORDS;
    if !short(b) {
        return false;
    }
    let mut last = b;
    for step in 0..ADDRESS_LINES {
        let next = page
            .iter()
            .filter(|o| {
                !std::ptr::eq(*o, last)
                    && (o.bbox.x0 - b.bbox.x0).abs() <= fs
                    && (-0.5..=fs * 1.5).contains(&(o.bbox.y0 - last.bbox.y1))
            })
            .min_by(|p, q| {
                p.bbox
                    .y0
                    .partial_cmp(&q.bbox.y0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        let Some(next) = next else {
            return false;
        };
        if !short(next) {
            return false;
        }
        // A street or a city between the name and the number: a heading
        // set straight over a line to call is a heading.
        if contact_line(&next.text()) {
            return step > 0;
        }
        last = next;
    }
    false
}

/// A house's number: a figure of one to five digits, a letter after it at
/// most (`42`, `93B`).
fn house_number(w: &str) -> bool {
    let w = w.trim_end_matches(',');
    let digits = w.chars().take_while(char::is_ascii_digit).count();
    (1..=5).contains(&digits)
        && w.len() - digits <= 1
        && w[digits..].chars().all(|c| c.is_ascii_alphabetic())
}

/// Most words in a street's line (`35 rue de la République`, `150 Royal
/// Street, Suite 101`).
const STREET_WORDS: usize = 6;

/// Words a line wrapped mid-phrase ends on, as no street's name does.
const JOINING_WORDS: &[&str] = &[
    "and", "or", "of", "the", "to", "for", "in", "on", "with", "a",
];

/// A street's line of an address: its name and the house's number, either
/// way round (`27 rue Henri Rolland`, `Bahnstr. 42`, `Sessamestreet 46`).
/// Every other word has a letter. A title wrapped after a year is none
/// (`2024 Annual Report on Form 10-K and` over `2025 Proxy Statement`): it
/// runs longer, or ends on a word that joins it to the next line.
fn street_line(t: &str) -> bool {
    let words: Vec<&str> = t.split_whitespace().collect();
    if words.len() > STREET_WORDS
        || words
            .last()
            .is_some_and(|w| JOINING_WORDS.contains(&w.to_lowercase().as_str()))
    {
        return false;
    }
    let named = |ws: &[&str]| {
        !ws.is_empty()
            && ws
                .iter()
                .all(|w| w.chars().any(char::is_alphabetic) && !w.ends_with(':'))
    };
    (2..=ADDRESS_WORDS).contains(&words.len())
        && ((house_number(words[0]) && named(&words[1..]))
            || (house_number(words[words.len() - 1]) && named(&words[..words.len() - 1])))
}

/// Most words in a town's name after its postcode (`Saint Denis de la
/// réunion`).
const TOWN_WORDS: usize = 6;

/// A postcode's line of an address: four or five figures, a country's
/// letter before them or the Dutch letters after, then the town (`69001
/// Lyon`, `5555 NH TommyCity`, `D-12345 Berlin`, `88802Spielkreis`); or an
/// American town, state and ZIP code (`Springfield, IL 62701`).
fn postcode_line(t: &str) -> bool {
    let t = t.trim();
    let rest = match t.split_once('-') {
        Some((country, rest))
            if (1..=2).contains(&country.len())
                && country.chars().all(|c| c.is_ascii_uppercase()) =>
        {
            rest
        }
        _ => t,
    };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if (4..=5).contains(&digits) {
        let mut town = rest[digits..].trim_start();
        let letters: String = town.chars().take(2).collect();
        if letters.len() == 2
            && letters.chars().all(|c| c.is_ascii_uppercase())
            && town[2..].starts_with(' ')
        {
            town = town[2..].trim_start();
        }
        let words: Vec<&str> = town.split_whitespace().collect();
        return (1..=TOWN_WORDS).contains(&words.len())
            && town.starts_with(|c: char| c.is_uppercase())
            && words.iter().all(|w| {
                w.chars()
                    .all(|c| c.is_alphabetic() || matches!(c, '-' | '.' | '\'' | '’' | '(' | ')'))
            });
    }
    let Some((town, state_zip)) = t.rsplit_once(',') else {
        return false;
    };
    let sz: Vec<&str> = state_zip.split_whitespace().collect();
    sz.len() == 2
        && sz[0].len() == 2
        && sz[0].chars().all(|c| c.is_ascii_uppercase())
        && sz[1].len() >= 5
        && sz[1][..5].chars().all(|c| c.is_ascii_digit())
        && town.chars().any(char::is_alphabetic)
}

/// Widest gap, in the lines' ems, between two lines of one address: an
/// envelope's window sets its lines apart.
const ADDRESS_LINE_GAP: f64 = 3.5;

/// Lines an address sets over its street: a person's or a company's name,
/// or the label that heads it (`Adresse de livraison`).
const ADDRESS_NAME_LINES: usize = 2;

/// Most words in a name over an address's street.
const ADDRESS_NAME_WORDS: usize = 6;

/// Block `i` is part of a postal address: a street's line set directly over
/// a postcode's, and the line or two over the street that name whom it
/// reaches (`Theodor Est` over `Bahnstr. 42` over `88802Spielkreis`; `27 rue
/// Henri Rolland` over `69100 Villeurbanne`). The address's lines are read
/// down the blocks set flush under each other, as one block or several.
fn in_postal_address(page: &[Block], i: usize) -> bool {
    let b = &page[i];
    let fs = (b.font_size as f64).max(1.0);
    let flush = |a: &Block, o: &Block| {
        (a.bbox.x0 - o.bbox.x0).abs() <= fs
            && (-0.5..=fs * ADDRESS_LINE_GAP).contains(&(o.bbox.y0 - a.bbox.y1))
    };
    // The run of blocks set flush with this one, upward and downward.
    let below = |a: &Block| {
        page.iter()
            .enumerate()
            .filter(|(_, o)| !std::ptr::eq(*o, a) && flush(a, o))
            .min_by(|p, q| p.1.bbox.y0.total_cmp(&q.1.bbox.y0))
            .map(|(k, _)| k)
    };
    let mut run = vec![i];
    let mut top = i;
    for _ in 0..ADDRESS_NAME_LINES + 1 {
        let up = page
            .iter()
            .enumerate()
            .filter(|(_, o)| !std::ptr::eq(*o, &page[top]) && flush(o, &page[top]))
            .max_by(|p, q| p.1.bbox.y1.total_cmp(&q.1.bbox.y1))
            .map(|(k, _)| k);
        let Some(k) = up else { break };
        if below(&page[k]) != Some(top) {
            break;
        }
        run.insert(0, k);
        top = k;
    }
    let mut last = i;
    for _ in 0..ADDRESS_NAME_LINES + 2 {
        let Some(k) = below(&page[last]) else { break };
        run.push(k);
        last = k;
    }
    // Its lines, each with the block it is in.
    let lines: Vec<(usize, String)> = run
        .iter()
        .flat_map(|&k| page[k].lines.iter().map(move |l| (k, l.text.clone())))
        .collect();
    lines.windows(2).enumerate().any(|(s, w)| {
        if !(street_line(&w[0].1) && postcode_line(&w[1].1)) {
            return false;
        }
        // The lines over the street name whom it reaches, in a few words.
        let mut first = s;
        while first > 0
            && s - first < ADDRESS_NAME_LINES
            && lines[first - 1].1.split_whitespace().count() <= ADDRESS_NAME_WORDS
        {
            first -= 1;
        }
        lines[first..=s + 1].iter().any(|(k, _)| *k == i)
    })
}

/// Any line of the block states a web address, an e-mail address or a
/// phone number: an address block's.
fn states_contact(b: &Block) -> bool {
    b.lines.iter().any(|l| contact_line(&l.text)) || b.text().split(['•', '|']).any(contact_line)
}

/// A line of an address: text ending on a web address, an e-mail address
/// or a phone number that is no sentence closing on it, its stop after the
/// address and words before it (`Meeting held virtually at
/// www.example.com/meeting.`). That is prose giving an address, not a line
/// of one: an address line states the address alone, or after a label or a
/// phone number, and no stop closes it.
fn contact_line(t: &str) -> bool {
    let t = t.trim_end();
    ends_with_contact(t) && !(t.ends_with('.') && t.split_whitespace().count() > 2)
}

/// Most words on one line of an address.
const ADDRESS_WORDS: usize = 10;

/// A kicker is not a section: a category, series name or tagline set in
/// smaller type directly above the title (`Scalable. Responsive.
/// Innovative.` over `Exos X16`, `Centrifugation` over `Thermo Scientific
/// M Series Centrifuges`). The title is level 1, so the kicker ranks below
/// it, and a deeper section opened just ahead of the title closes at the
/// title with nothing in it. Read as text, it stays where the page sets it.
///
/// Only typography makes a kicker a heading, and only blocks with no text
/// between them and the title, and smaller than it, are kickers.
fn drop_kickers(out: &mut Vec<Heading>, pages: &[Vec<Block>], title: Option<usize>) {
    let (Some(t), Some(blocks)) = (title, pages.first()) else {
        return;
    };
    let Some(title_block) = blocks.get(t) else {
        return;
    };
    for k in (0..t).rev() {
        let b = &blocks[k];
        let Some(pos) = out
            .iter()
            .position(|h| h.page == title_block.page && h.block_index == k)
        else {
            break;
        };
        if !matches!(out[pos].evidence, Evidence::FontSize | Evidence::Bold)
            || b.font_size >= title_block.font_size
        {
            break;
        }
        // A division's label over its title is the title's own, `Chapter
        // 3` over `Numerical differentiation`: a heading at the title's
        // level, not text.
        if division_label(&b.text()) {
            out[pos].evidence = Evidence::Title;
            out[pos].level = 1;
            continue;
        }
        out.remove(pos);
    }
}

/// Words that name a division of a document, in the languages the corpus
/// sets them in.
const DIVISION_WORDS: &[&str] = &[
    "chapter",
    "part",
    "section",
    "unit",
    "lesson",
    "module",
    "appendix",
    "annex",
    "book",
    "volume",
    "chapitre",
    "partie",
    "annexe",
    "capítulo",
    "capitulo",
    "parte",
    "sección",
    "seccion",
    "anexo",
    "kapitel",
    "teil",
    "abschnitt",
    "anhang",
];

/// A division's label alone: a division word and its number, a roman
/// numeral or a single letter (`Chapter 3`, `PART II`, `Appendix B`).
pub(crate) fn division_label(text: &str) -> bool {
    let mut words = text.split_whitespace();
    let (Some(word), Some(number), None) = (words.next(), words.next(), words.next()) else {
        return false;
    };
    let number = number.trim_end_matches(['.', ':']);
    DIVISION_WORDS.contains(&word.to_lowercase().as_str())
        && !number.is_empty()
        && (number.chars().all(|c| c.is_ascii_digit()) && number.len() <= 3
            || is_roman(&number.to_ascii_uppercase())
            || (number.chars().count() == 1 && number.chars().all(|c| c.is_alphabetic())))
}

/// Visual style of a heading, for ranking depth.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
struct Style {
    /// Quarter-point font size, negated so larger sorts first.
    neg_size: i32,
    /// Bold before regular at the same size.
    not_bold: bool,
    /// All-caps before mixed case at the same size and weight.
    not_caps: bool,
    /// Upright before italic at the same size, weight and case: a journal's
    /// subsections in bold italic under its sections in bold
    /// (`Sense of Humour Scale` under `Materials`).
    italic: bool,
}

/// Assign levels from the document's own heading styles.
///
/// Each detector previously supplied its own level on its own scale — font size
/// ranked by size, numbering used its depth, caps and bold returned fixed
/// constants — so a document mixing signals got incoherent depths. Measured
/// against ground truth, only 38.7% of correctly-*found* headings had the right
/// level.
///
/// Instead, rank the distinct styles actually used for headings in this
/// document and assign 1..n. Numbering is authoritative where present, since it
/// states the depth explicitly; the outline likewise. Everything else is
/// levelled by style rank.
fn normalize_levels(hs: &mut [Heading], pages: &[Vec<Block>]) {
    let ranked = |h: &Heading| {
        !matches!(
            h.evidence,
            Evidence::Outline | Evidence::Numbering | Evidence::Sequence
        )
    };
    // Italic is a level of its own where it sets two headings or more, as a
    // face must, or where it heads an upright heading set directly under it:
    // one italic line among bold heads is as likely a word set in italic (a
    // species' name ending a bold line) as a level.
    let alike = |x: &Style, y: &Style| {
        (x.neg_size, x.not_bold, x.not_caps) == (y.neg_size, y.not_bold, y.not_caps)
    };
    let mut italic_count: BTreeMap<Style, usize> = BTreeMap::new();
    for h in hs.iter().filter(|h| ranked(h) && h.style.italic) {
        *italic_count.entry(h.style).or_default() += 1;
    }
    for (a, b) in hs.iter().zip(hs.iter().skip(1)) {
        if a.page == b.page
            && b.block_index == a.block_index + 1
            && ranked(a)
            && ranked(b)
            && a.style.italic
            && !b.style.italic
            && alike(&a.style, &b.style)
        {
            *italic_count.entry(a.style).or_default() += MIN_FACE_HEADINGS;
        }
    }
    for h in hs.iter_mut() {
        if h.style.italic && italic_count.get(&h.style).copied().unwrap_or(0) < MIN_FACE_HEADINGS {
            h.style.italic = false;
        }
    }
    let x_height = |h: &Heading| {
        pages
            .get(h.page)
            .and_then(|p| p.get(h.block_index))
            .and_then(x_height_of)
    };
    // The faces each style is set in, told apart by their x-heights.
    let mut heights: BTreeMap<Style, Vec<f32>> = BTreeMap::new();
    for h in hs.iter().filter(|h| ranked(h)) {
        if let Some(x) = x_height(h) {
            heights.entry(h.style).or_default().push(x);
        }
    }
    let faces: BTreeMap<Style, Vec<f32>> = heights
        .into_iter()
        .map(|(style, xs)| (style, faces_by_height(xs)))
        .collect();
    let face = |h: &Heading| {
        let floors = faces.get(&h.style)?;
        let x = x_height(h)?;
        floors.iter().position(|f| x >= *f)
    };
    // Which way round a style's variants nest: upright over italic and the
    // taller face over the shorter, unless the document sets them the other
    // way. A heading set directly over another, nothing between, heads it
    // (a section opening on its first subsection): a 10-K's `Item 1.
    // Business` in bold italic directly over `General` in bold.
    let (mut italic_heads, mut upright_heads, mut shorter_heads, mut taller_heads) = (0, 0, 0, 0);
    for (a, b) in hs.iter().zip(hs.iter().skip(1)) {
        let stacked = a.page == b.page && b.block_index == a.block_index + 1;
        if !stacked || !ranked(a) || !ranked(b) || !alike(&a.style, &b.style) {
            continue;
        }
        match (a.style.italic, b.style.italic) {
            (true, false) => italic_heads += 1,
            (false, true) => upright_heads += 1,
            _ => {}
        }
        if a.style == b.style {
            match (face(a), face(b)) {
                (Some(x), Some(y)) if x > y => shorter_heads += 1,
                (Some(x), Some(y)) if x < y => taller_heads += 1,
                _ => {}
            }
        }
    }
    let italic_over = italic_heads > upright_heads;
    let shorter_over = shorter_heads > taller_heads;
    let order = |(style, f): (Style, usize)| {
        (
            style.neg_size,
            style.not_bold,
            style.not_caps,
            style.italic != italic_over,
            if shorter_over { usize::MAX - f } else { f },
        )
    };
    let mut styles: Vec<(Style, usize)> = hs
        .iter()
        .filter(|h| ranked(h))
        .map(|h| (h.style, face(h).unwrap_or(0)))
        .collect();
    styles.sort_by_key(|k| order(*k));
    styles.dedup();
    // A variant takes a level of its own only where the document has one to
    // spare. Past the deepest level, ranks fold together, and a level for
    // an italic or a second face would fold two sizes into one: a drug
    // label's table titles set bold at one size into its column labels a
    // size smaller.
    // Counted over the styles that head something: a form's or a chart's
    // text set like a heading is read as text once its run is judged.
    let mut kept: Vec<(Style, usize)> = hs
        .iter()
        .filter(|h| ranked(h) && !h.form_text)
        .map(|h| (h.style, face(h).unwrap_or(0)))
        .collect();
    kept.sort_by_key(|k| order(*k));
    kept.dedup();
    let plain = |(style, _): &(Style, usize)| (style.neg_size, style.not_bold, style.not_caps);
    let mut sizes: Vec<_> = kept.iter().map(plain).collect();
    sizes.dedup();
    let variants = kept.len() <= MAX_LEVEL || kept.len() == sizes.len();
    if !variants {
        for h in hs.iter_mut() {
            h.style.italic = false;
        }
        styles = hs
            .iter()
            .filter(|h| ranked(h))
            .map(|h| (h.style, 0))
            .collect();
        styles.sort();
        styles.dedup();
    }
    let face = |h: &Heading| if variants { face(h) } else { None };

    for h in hs.iter_mut() {
        if !ranked(h) || h.evidence == Evidence::Title {
            continue;
        }
        let key = (h.style, face(h).unwrap_or(0));
        if let Some(rank) = styles.iter().position(|s| *s == key) {
            h.level = (rank + 1).min(MAX_LEVEL);
        }
    }
}

/// Share by which one face's x-height must fall short of another's, at one
/// size, weight, case and slant, to set a level below it. Two faces a
/// journal sets its sections and subsections in differ by a sixth
/// (Helvetica's 0.55 of the size against Minion's 0.465); one face measured
/// over different words varies by a hundredth or two, and one face embedded
/// twice under two names (`ArialNarrow-Bold` and `F1`) not at all.
const FACE_STEP: f32 = 0.08;

/// Headings a face must set to be a level of its own: one heading set in
/// another face at the same size is as likely an odd font as a level.
const MIN_FACE_HEADINGS: usize = 2;

/// The faces a style's headings are set in, from their x-heights: the
/// lowest x-height of each face, tallest face first. A face sets a level
/// below the one before it; a face of fewer than [`MIN_FACE_HEADINGS`]
/// joins the face next to it.
fn faces_by_height(mut xs: Vec<f32>) -> Vec<f32> {
    xs.sort_by(|a, b| b.total_cmp(a));
    // Each face as (tallest, lowest, headings).
    let mut faces: Vec<(f32, f32, usize)> = Vec::new();
    for x in xs {
        match faces.last_mut() {
            Some(f) if x >= f.1 * (1.0 - FACE_STEP) => {
                f.1 = x;
                f.2 += 1;
            }
            _ => faces.push((x, x, 1)),
        }
    }
    while faces.len() > 1 {
        let Some(i) = faces.iter().position(|f| f.2 < MIN_FACE_HEADINGS) else {
            break;
        };
        // Join the neighbour nearer in height.
        let j = match (i.checked_sub(1), faces.get(i + 1)) {
            (Some(a), Some(b)) => {
                if faces[a].1 - faces[i].0 <= faces[i].1 - b.0 {
                    a
                } else {
                    i + 1
                }
            }
            (Some(a), None) => a,
            (None, _) => i + 1,
        };
        let (lo, hi) = (i.min(j), i.max(j));
        let merged = (faces[lo].0, faces[hi].1, faces[lo].2 + faces[hi].2);
        faces[lo] = merged;
        faces.remove(hi);
    }
    // The floor of the last face takes every lower height in.
    let n = faces.len();
    faces
        .iter()
        .enumerate()
        .map(|(k, f)| if k + 1 == n { f32::MIN } else { f.1 })
        .collect()
}

/// Headings per element above which a document's hierarchy is doubtful.
///
/// A document is mostly prose; when a quarter of what it emits claims to be a
/// heading, the detector is promoting body text. Measured over the 105
/// benchmark documents that have both headings and a hierarchy score, mean
/// heading score falls monotonically with this ratio -- 0.890 below 0.1,
/// 0.798 to 0.2, 0.773 to 0.3, 0.732 to 0.5, and 0.505 above it. Set at 0.4
/// the signal fires on 7 documents, 4 of them scoring under 0.6, which is
/// 2.6x the base rate; their mean hierarchy score is 0.481 against 0.822
/// across the corpus.
///
/// Deliberately narrow. At 0.2 it fires on 41 documents for 1.34x, which is
/// barely better than guessing and would escalate a third of everything.
const DOUBTFUL_DENSITY: f64 = 0.4;

/// A page whose heading hierarchy rests on weak evidence.
///
/// The two existing triggers ask whether the *text* could be read -- an
/// unreadable page, a table disagreeing with itself. Neither fires on a page
/// that extracts perfectly and is organised wrongly, which is where the
/// remaining loss sits: of the documents that escalate nothing, the ones
/// scoring worst read their text well and their hierarchy badly.
///
/// Measured **per page**, not per document, because the ratio does not
/// survive averaging. A deck whose stat pages are four fifths headings sits
/// at 0.36 across twenty-eight pages and never fires, while six of those
/// pages exceed the threshold alone -- one of them at 0.80. The evaluation
/// corpus hid this: every document in it is a single page, so the two
/// measures were the same number and the weaker one was chosen by accident.
#[derive(Debug, Clone, PartialEq)]
pub struct Doubt {
    /// The page this doubt is about.
    pub page: usize,
    pub titles: usize,
    pub elements: usize,
    /// Headings as a fraction of all elements.
    pub density: f64,
    /// Headings resting on an outline entry or a numbering pattern rather
    /// than on relative font size, which is a guess about intent.
    pub corroborated: usize,
    /// Headings set in a style the document repeats as a heading across
    /// its pages — a template's block labels — and so discounted from
    /// `density`; see [`template_levels`].
    pub template: usize,
}

/// Distinct pages a heading's text must recur on, as a heading, before the
/// style it is set in counts as a template style.
///
/// Three, not two: one repeat is a section title that happens to share its
/// wording with a later one. Three pages carrying the same title in the
/// same style is a form.
const TEMPLATE_MIN_PAGES: usize = 3;

/// A numbered line set this many times in one document is a code, not a
/// section title. Sections are not repeated; a form's answers are.
const REPEATED_NUMBERING: usize = 2;

/// The heading levels a document uses for its template.
///
/// A chart, a form, a report generated from one layout puts the same block
/// labels on every page — `Assessment & Plan`, `Reason for Encounter` —
/// set bold or large, corroborated by nothing on the page: no outline, no
/// numbering. Judged page by page that is the weakest hierarchy there is,
/// and every page of an 800-page chart escalated for it. Judged across the
/// pages it is the strongest: a label that recurs as a heading on page
/// after page in one style is a heading by construction, and so is
/// everything else the document sets in that style. Repetition across pages
/// is corroboration — the same evidence furniture detection rests on.
///
/// `titles` is every heading in the document as `(page, level, text)`; the
/// result is the set of levels whose style carries a title recurring on at
/// least [`TEMPLATE_MIN_PAGES`] pages. Empty for a single-page document,
/// which is why this cannot change how one is read.
pub fn template_levels<'a>(
    titles: impl Iterator<Item = (usize, Option<usize>, &'a str)>,
) -> std::collections::HashSet<usize> {
    let mut pages_by_text: HashMap<String, (Vec<usize>, Option<usize>)> = HashMap::new();
    for (page, level, text) in titles {
        let key = norm(text);
        if key.is_empty() {
            continue;
        }
        let e = pages_by_text.entry(key).or_insert((Vec::new(), level));
        e.0.push(page);
    }
    let mut levels = std::collections::HashSet::new();
    for (mut pages, level) in pages_by_text.into_values() {
        pages.sort_unstable();
        pages.dedup();
        if pages.len() >= TEMPLATE_MIN_PAGES {
            if let Some(l) = level {
                levels.insert(l);
            }
        }
    }
    levels
}

/// Report a doubtful hierarchy, or `None` when it looks sound.
///
/// Reported, never acted on here -- the same contract as
/// [`crate::table::suspect_tables`].
pub fn doubt(kinds: &[(&str, &str)]) -> Option<Doubt> {
    doubt_on_page(0, kinds)
}

/// As [`doubt`], recording which page the ratio was measured over.
pub fn doubt_on_page(page: usize, kinds: &[(&str, &str)]) -> Option<Doubt> {
    doubt_on_page_with(page, kinds, &[])
}

/// As [`doubt_on_page`], with `template` marking, per entry of `kinds`, a
/// title set in one of the document's [`template_levels`]. Those are
/// corroborated by the document's other pages and do not count toward the
/// density; a page that is half template labels is a form, not a doubt.
/// An empty `template` marks nothing.
pub fn doubt_on_page_with(page: usize, kinds: &[(&str, &str)], template: &[bool]) -> Option<Doubt> {
    // Splice anchors are addresses, not content. Counting them changes the
    // ratio according to whether the caller happens to have asked for them,
    // so two callers looking at the same document disagreed about whether to
    // doubt it — and the one that routes escalation was the one that had
    // them.
    let kinds: Vec<(&str, &str, bool)> = kinds
        .iter()
        .enumerate()
        .filter(|(_, (_, e))| !matches!(*e, "route" | "table-confidence" | "table-missing"))
        .map(|(i, (k, e))| (*k, *e, template.get(i).copied().unwrap_or(false)))
        .collect();
    let elements = kinds.len();
    if elements < 4 {
        // Too little to judge: a two-element page of one title and one
        // paragraph is 50% headings and perfectly correct.
        return None;
    }
    let titles: Vec<&(&str, &str, bool)> = kinds
        .iter()
        .filter(|(k, _, _)| *k == "doco:SectionTitle")
        .collect();
    if titles.is_empty() {
        return None;
    }
    let template = titles.iter().filter(|(_, _, t)| *t).count();
    let density = (titles.len() - template) as f64 / elements as f64;
    if density <= DOUBTFUL_DENSITY {
        return None;
    }
    let corroborated = titles
        .iter()
        .filter(|(_, e, _)| matches!(*e, "outline" | "numbering" | "sequence"))
        .count();
    Some(Doubt {
        page,
        titles: titles.len(),
        elements,
        density,
        corroborated,
        template,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::BBox;
    use crate::line::Line;

    fn blk(text: &str, fs: f32, lines: usize) -> Block {
        let l = Line {
            text: text.into(),
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 100.0,
                y1: fs as f64,
            },
            page: 0,
            rotation_bucket: 0,
            glyphs: vec![],
            font_size: fs,
            bold: false,
            opens_with_script: false,
            italic: false,
            x_height: None,
            opens_bold: false,
        };
        Block {
            lines: vec![l; lines.max(1)],
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 100.0,
                y1: fs as f64,
            },
            page: 0,
            font_size: fs,
            bold: false,
            gap_above: None,
            marker: None,
            marker_x: None,
        }
    }

    /// A one-line block at `x0..x1`, `y0` down, bold or not.
    fn placed(text: &str, fs: f32, x0: f64, y0: f64, x1: f64, bold: bool) -> Block {
        let mut b = blk(text, fs, 1);
        b.bold = bold;
        b.bbox = BBox {
            x0,
            y0,
            x1,
            y1: y0 + fs as f64,
        };
        for l in &mut b.lines {
            l.bold = bold;
            l.bbox = b.bbox;
        }
        b
    }

    #[test]
    fn dates_sums_and_codes_are_values() {
        for d in [
            "05 Juillet 2015",
            "July 1, 2014",
            "April 2024",
            "13/11/2017",
            "13.06.2015",
            "2015-06-13",
            "1er mars 2024",
        ] {
            assert!(date_text(d), "{d}");
        }
        for d in [
            "May",
            "5 Results",
            "Study 2",
            "2024 Highlights",
            "5 May Street",
        ] {
            assert!(!date_text(d), "{d}");
        }
        // A date opens on its day, not a section's number.
        assert_eq!(numbering_depth("05 Juillet 2015"), None);
        assert_eq!(numbering_depth("5 Results"), Some(1));
        for a in ["€ 717,97", "1.589,80", "$4.11", "29.99 € TTC", "-9,32"] {
            assert!(amount_text(a), "{a}");
        }
        for a in ["2024", "12.5 million", "3.2", "Total 4.11"] {
            assert!(!amount_text(a), "{a}");
        }
        assert!(states_amount("Gesamtbetrag: 406,82 €"));
        assert!(!states_amount("Results for 2024"));
        assert!(code_text("MSPE2017") && code_text("FR19787878784"));
        // A contents' page reference is no field's value.
        assert!(!code_text("E-1") && !code_text("Results"));
        // A label glued to its figure is a field; a title sets a space.
        assert!(field_on_its_line("Kundennummer:2"));
        assert!(!field_on_its_line("Policy Rules: Limitations"));
    }

    #[test]
    fn a_postal_address_is_no_heading() {
        assert!(street_line("27 rue Henri Rolland") && street_line("Bahnstr. 42"));
        assert!(postcode_line("69100 Villeurbanne") && postcode_line("5555 NH TommyCity"));
        assert!(postcode_line("88802Spielkreis") && postcode_line("Springfield, IL 62701"));
        assert!(!postcode_line("Results") && !street_line("Results and discussion"));
        // An envelope's window: a name over a street over a postcode, set
        // larger than the text and lines apart.
        let page = vec![
            placed("Theodor Est", 12.0, 70.0, 170.0, 150.0, false),
            placed("Bahnstr. 42", 12.0, 70.0, 200.0, 130.0, false),
            placed("88802 Spielkreis", 12.0, 70.0, 240.0, 160.0, false),
            placed("ordinary body prose here", 10.0, 70.0, 300.0, 500.0, false),
        ];
        for i in 0..3 {
            assert!(in_postal_address(&page, i), "{}", page[i].text());
        }
        assert!(!in_postal_address(&page, 3));
        // A section's heading over its text is none.
        let page = vec![
            placed("Methods", 12.0, 70.0, 100.0, 130.0, true),
            placed(
                "Study 2 enrolled forty patients.",
                10.0,
                70.0,
                116.0,
                300.0,
                false,
            ),
            placed("2024 Highlights", 12.0, 70.0, 132.0, 160.0, true),
        ];
        assert!(!in_postal_address(&page, 0) && !in_postal_address(&page, 2));
    }

    #[test]
    fn a_label_with_its_value_beside_or_under_it_is_a_field() {
        let page = vec![
            // A field's label and its date, no colon between.
            placed("Date de facture", 8.0, 35.0, 224.0, 106.0, true),
            placed("13/11/2017", 8.0, 156.0, 224.0, 203.0, false),
            // A total and its sum against the right margin.
            placed("Totaal", 8.0, 437.0, 300.0, 461.0, true),
            placed("€ 717,97", 8.0, 518.0, 300.0, 549.0, false),
            // A margin's heading beside its paragraph's number and text.
            placed("Rescue equipment", 8.0, 35.0, 400.0, 110.0, true),
            placed("156", 8.0, 130.0, 400.0, 142.0, false),
            placed(
                "Suitable rescue equipment must be",
                8.0,
                150.0,
                400.0,
                400.0,
                false,
            ),
            // Labels side by side, each over its code.
            placed("GSTIN", 9.0, 57.0, 583.0, 84.0, true),
            placed("06AABCO6063D1ZQ", 8.3, 57.0, 600.0, 135.0, true),
            placed("CIN", 9.0, 302.0, 583.0, 316.0, true),
            placed("U63090DL2012PTC231770", 8.3, 302.0, 600.0, 402.0, true),
            // A heading over a model's code, alone on its row.
            placed("Dimensions", 9.0, 57.0, 700.0, 110.0, true),
            placed("AFPE224300", 8.0, 57.0, 712.0, 110.0, false),
        ];
        assert!(labels_a_value(&page, 0));
        assert!(labels_a_value(&page, 2));
        assert!(!labels_a_value(&page, 4));
        assert!(labels_a_value_under(&page, 7));
        assert!(!labels_a_value_under(&page, 11));
        // The label sharing a row with a field, over a short line, is one
        // too: `Contact commercial` over `Tony Dubois`.
        let mut row = page[..2].to_vec();
        row.push(placed("Contact commercial", 8.0, 327.0, 224.0, 421.0, true));
        row.push(placed("Tony Dubois", 8.0, 327.0, 241.0, 381.0, false));
        assert!(in_field_row(&row, 2));
        // A value beside a label closing on its colon.
        let page = vec![
            placed("Prélèvement à partir du :", 8.0, 380.0, 576.0, 479.0, false),
            placed("05 Juillet 2015", 8.0, 497.0, 576.0, 554.0, true),
        ];
        assert!(beside_a_label(&page, 1) && named_value(&page[1].text()));
        // A step's label beside the step before it is no value.
        assert!(!named_value("STEP 4:"));
    }

    #[test]
    fn a_closed_sentence_is_no_heading() {
        assert!(prose_sentence("Bitte überweisen Sie bis zum 04.07.2015."));
        assert!(prose_sentence(
            "Betaald op 21-04-2015 met € 717,97 via iDEAL."
        ));
        // Length tells nothing: a slide's or a 10-K's risk's caption is a
        // sentence too.
        for t in [
            "We are not profitable and expect to continue to incur losses.",
            "Activity 4. Determining CEC by replacing adsorbed cations.",
            "Model the domain, not the org chart.",
            "Adjustments to Applicable Conversion Price for Diluting Issues.",
            "4.4 Adjustments to Applicable Conversion Price for Diluting Issues.",
            "FACTUUR.",
        ] {
            assert!(!prose_sentence(t), "{t}");
        }
        // A sentence is no document's title, even a short one.
        assert!(closed_sentence("Dat betaal ik zelf wel."));
        // Labels each closing on a colon make a line of fields.
        assert!(fields_run("BTW nr. NL: NL82 IBAN: NL27 BIC: INGB"));
        assert!(!fields_run("Policy Rules: Limitations"));
    }

    #[test]
    fn numbering_depth_reads_section_numbers() {
        assert_eq!(numbering_depth("5. Specifications"), Some(1));
        assert_eq!(
            numbering_depth("5.3 Recommended Operating Conditions"),
            Some(2)
        );
        assert_eq!(numbering_depth("6.3.1 Oscillator Frequency"), Some(3));
    }

    #[test]
    fn numbering_reads_separators_letters_and_romans() {
        // Digit with a dash separator.
        assert_eq!(
            numbering_depth("01 - Find Open Educational Resources"),
            Some(1)
        );
        assert_eq!(numbering_depth("02- Prepare Your Content"), Some(1));
        // Appendix-style lettered sections.
        assert_eq!(numbering_depth("B.1 Large Language Models"), Some(2));
        assert_eq!(numbering_depth("B. Related Works"), Some(1));
        // Roman numerals.
        assert_eq!(
            numbering_depth("II. Set Up the Restriction Digests"),
            Some(1)
        );
        // A bare capital with no dot is too weak to be a section label.
        assert_eq!(numbering_depth("A test of the system"), None);
    }

    #[test]
    fn a_number_hyphenated_into_a_word_is_not_a_label() {
        assert_eq!(
            numbering_depth("3-Way Wiring (Using 3-Way Mechanical Switch)"),
            None
        );
        assert_eq!(numbering_depth("4-M4 M0.16"), None);
        assert_eq!(numbering_depth("2-Pole Breakers"), None);
        // A dash set off by a space still separates the label.
        assert_eq!(numbering_depth("02- Prepare Your Content"), Some(1));
        assert_eq!(
            numbering_depth("01 - Find Open Educational Resources"),
            Some(1)
        );
    }

    #[test]
    fn initials_are_not_lettered_numbering() {
        assert_eq!(numbering_depth("U.S.A. FOODSERVICE DIVISION"), None);
        assert_eq!(numbering_depth("U.S. Department of Energy"), None);
        assert_eq!(numbering_depth("E.U. Regulation"), None);
        // A letter after a roman numeral, or a number after a letter, is an
        // outline.
        assert_eq!(numbering_depth("I.A. Purpose"), Some(2));
        assert_eq!(numbering_depth("B.1 Large Language Models"), Some(2));
        assert_eq!(single_letter_label("J. Example"), Some('J'));
        assert_eq!(single_letter_label("B. Related Works"), Some('B'));
        assert_eq!(single_letter_label("B.1 Large Language Models"), None);
        assert_eq!(single_letter_label("Results"), None);
    }

    #[test]
    fn a_lone_initial_is_not_a_section_but_a_lettered_sequence_is() {
        let page = vec![
            blk("Prepared by:", 10.0, 1),
            blk("J. Example", 10.0, 1),
            blk("Some body text that runs on for a while here.", 10.0, 1),
        ];
        let hs = detect(&[page], &[]);
        assert!(hs.iter().all(|h| h.text != "J. Example"), "{hs:?}");
        let page = vec![
            blk("A. Purpose", 10.0, 1),
            blk("Some body text that runs on for a while here.", 10.0, 1),
            blk("B. Scope", 10.0, 1),
            blk("Some body text that runs on for a while here.", 10.0, 1),
        ];
        let hs = detect(&[page], &[]);
        assert_eq!(
            hs.iter().map(|h| h.text.as_str()).collect::<Vec<_>>(),
            ["A. Purpose", "B. Scope"]
        );
        let page = vec![
            blk("I. Introduction", 10.0, 1),
            blk("Some body text that runs on for a while here.", 10.0, 1),
        ];
        assert_eq!(detect(&[page], &[]).len(), 1);
    }

    #[test]
    fn a_kicker_above_the_title_is_not_a_section() {
        let page = vec![
            blk("DATA SHEET", 6.0, 1),
            blk("Scalable. Responsive. Innovative.", 12.7, 1),
            blk("Exos X16", 20.2, 1),
            blk(
                "Seagate manufactures hard drives for the hyperscale market and more.",
                9.0,
                3,
            ),
            blk("Best-Fit Applications", 12.7, 1),
            blk(
                "Seagate manufactures hard drives for the hyperscale market and more.",
                9.0,
                3,
            ),
        ];
        let hs = detect(&[page], &[]);
        let texts: Vec<&str> = hs.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(texts, ["Exos X16", "Best-Fit Applications"], "{hs:?}");
    }

    fn body() -> Block {
        blk(
            "Some body text that runs on for a while here and more.",
            10.0,
            3,
        )
    }

    #[test]
    fn a_divisions_label_over_its_title_stays_a_heading_at_its_level() {
        let page = vec![
            blk("Chapter 3", 14.0, 1),
            blk("Numerical differentiation", 24.0, 1),
            body(),
            blk("3.1 Introduction", 14.0, 1),
            body(),
        ];
        let hs = detect(&[page], &[]);
        let got: Vec<(&str, usize)> = hs.iter().map(|h| (h.text.as_str(), h.level)).collect();
        assert_eq!(got[0], ("Chapter 3", 1), "{hs:?}");
        assert_eq!(got[1], ("Numerical differentiation", 1), "{hs:?}");
        assert!(division_label("PART II"));
        assert!(division_label("Appendix B"));
        assert!(division_label("Capítulo 12"));
        assert!(!division_label("Chapter 3 Numerical differentiation"));
        assert!(!division_label("Scalable. Responsive."));
        assert!(!division_label("Section"));
    }

    #[test]
    fn a_numbered_note_set_small_is_no_heading() {
        let page = vec![
            blk("1. Scope", 10.0, 1),
            body(),
            body(),
            body(),
            blk("1) Feature only available with the larger model", 7.0, 1),
            // A section set in small capitals under the text's size.
            blk("2. AFFIRMATIVE COVENANTS.", 8.0, 1),
            body(),
        ];
        let hs = detect(&[page], &[]);
        let texts: Vec<&str> = hs.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(texts, ["1. Scope", "2. AFFIRMATIVE COVENANTS."], "{hs:?}");
    }

    #[test]
    fn the_title_is_picked_by_size_and_column_not_reading_order() {
        let at = |text: &str, fs: f32, x0: f64, y0: f64, x1: f64| {
            let mut b = blk(text, fs, 1);
            b.bbox = BBox {
                x0,
                y0,
                x1,
                y1: y0 + fs as f64,
            };
            b.lines[0].bbox = b.bbox;
            b
        };
        let title = |hs: &[Heading]| {
            hs.iter()
                .find(|h| h.evidence == Evidence::Title)
                .map(|h| h.text.clone())
        };
        // A sidebar read first, its heading smaller than the title at the
        // right, read after it.
        let mut page = vec![at("Highlights", 14.0, 37.0, 283.0, 103.0)];
        for k in 0..5 {
            page.push(at(
                "A range of capacities for demanding workloads.",
                8.5,
                37.0,
                310.0 + 37.0 * k as f64,
                180.0,
            ));
        }
        page.push(at("Gold Enterprise Class Drive", 17.0, 236.0, 153.0, 543.0));
        page.push(at(
            "Drives provide high performance for tower workstations and more.",
            8.5,
            235.0,
            199.0,
            559.0,
        ));
        assert_eq!(
            title(&detect(&[page], &[])).as_deref(),
            Some("Gold Enterprise Class Drive")
        );
        // A category strip at the right, as large as the title at the left.
        let page = vec![
            at("Wallbox Wiring Devices", 14.0, 402.0, 46.0, 558.0),
            at("120 V Multi-Location Smart Dimmer", 14.0, 55.0, 83.0, 430.0),
            at(
                "The Smart Dimmer requires a neutral wire connection here.",
                10.0,
                55.0,
                120.0,
                400.0,
            ),
            at(
                "Companion dimmers cannot be used as a single-pole switch.",
                10.0,
                55.0,
                140.0,
                400.0,
            ),
        ];
        assert_eq!(
            title(&detect(&[page], &[])).as_deref(),
            Some("120 V Multi-Location Smart Dimmer")
        );
        // A label and its value set apart across the page, at the body's
        // size, is a field.
        let page = vec![
            at("Part No. 675 8126H250", 10.0, 71.0, 89.0, 305.0),
            at(
                "A wide mouth bottle of 250ml, made for the laboratory and the field, in clear plastic.",
                10.0,
                71.0,
                108.0,
                520.0,
            ),
            at(
                "Each is supplied with its lid screwed on, and they are packed twelve bottles to a pack.",
                10.0,
                71.0,
                128.0,
                520.0,
            ),
        ];
        assert_eq!(title(&detect(&[page], &[])), None);
    }

    #[test]
    fn a_caption_or_a_field_label_is_no_heading_by_typography() {
        let mut caption = blk("Figure 5-1. Low-Level Output Voltage", 10.0, 1);
        caption.bold = true;
        let mut head = blk("Typical Characteristics", 10.0, 1);
        head.bold = true;
        let page = vec![head, body(), caption, body()];
        let hs = detect(&[page], &[]);
        let texts: Vec<&str> = hs.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(texts, ["Typical Characteristics"], "{hs:?}");
        // A label with its value beside it on its row.
        let mut label = blk("Color:", 10.0, 1);
        label.bold = true;
        let mut value = blk("Most colors", 10.0, 1);
        value.bbox.x0 = label.bbox.x1 + 10.0;
        value.bbox.x1 = value.bbox.x0 + 60.0;
        value.bbox.y0 = label.bbox.y0;
        value.bbox.y1 = label.bbox.y1;
        let page = vec![label.clone(), value];
        assert!(not_a_typographic_heading("Color:", &page, 0, 10.0));
        assert!(!not_a_typographic_heading("Color:", &page[..1], 0, 10.0));
    }

    /// `text` at size `fs`, its top at `y`.
    fn at(text: &str, fs: f32, y: f64, bold: bool) -> Block {
        let mut b = blk(text, fs, 1);
        b.bbox.y0 = y;
        b.bbox.y1 = y + fs as f64;
        b.bold = bold;
        b
    }

    #[test]
    fn an_articles_front_matter_is_no_heading() {
        for t in [
            "Ann Lee 1, Bo Chen2, Cy Wu2 and*",
            "Dee Park 2*",
            "Ann B. Lee 1,2, Bo Chen3",
            "3,4, Cy D. Wu5\u{2021}, Ed F. Ng1\u{2021}",
            "Fay Gu and Hal Ito *",
        ] {
            assert!(byline(t), "{t}");
        }
        // A heading with a note's star, and a label's number sign.
        assert!(!byline("Posted Transactions*"));
        assert!(!byline("Comparable Sale #1"));
        for t in [
            "Study 2",
            "Model Year 2020",
            "Results and Discussion",
            "Supplementary Figure 1",
            "Terms and Conditions",
            "Ann Lee",
        ] {
            assert!(!byline(t), "{t}");
        }
        for t in [
            "Received March 3, 2011; Accepted May 9, 2011; Published June 1, 2011",
            "Editor: Ann Lee, Example University, United States of America",
            "Academic editor",
            "Copyright \u{00A9} 2011 Lee et al.",
            "Submitted 9 July 2018 Accepted 24 August 2018",
        ] {
            assert!(publication_record(t), "{t}");
        }
        for t in [
            "Received signal strength",
            "Funding",
            "Copyright",
            "Published works",
            "Editorial board",
        ] {
            assert!(!publication_record(t), "{t}");
        }
    }

    #[test]
    fn an_articles_long_title_is_its_title_and_its_banner_no_heading() {
        // A journal's banner set across the row, over a title of four lines
        // at twice the body's size, over the authors.
        let mut kind = at("ORIGINAL STUDY", 13.0, 90.0, false);
        kind.bbox.x0 = 60.0;
        kind.bbox.x1 = 160.0;
        let mut access = at("Free to Read", 13.0, 90.0, false);
        access.bbox.x0 = 450.0;
        access.bbox.x1 = 535.0;
        let mut title = blk("How a small protein and its partners", 22.0, 1);
        for line in [
            "shape growth of yeast cells on glucose",
            "under stress across many generations",
            "in the lab",
        ] {
            let l = blk(line, 22.0, 1).lines.remove(0);
            title.lines.push(l);
        }
        title.bbox = BBox {
            x0: 57.0,
            y0: 120.0,
            x1: 520.0,
            y1: 220.0,
        };
        let mut authors = at("Ann Lee 1, Bo Chen2 and Cy Wu1*", 10.0, 235.0, true);
        authors.bbox.x1 = 400.0;
        let mut text = Vec::new();
        for k in 0..4 {
            let mut b = body();
            b.bbox.y0 = 260.0 + 40.0 * k as f64;
            b.bbox.y1 = b.bbox.y0 + 30.0;
            text.push(b);
        }
        let mut page = vec![kind, access, title, authors];
        page.extend(text);
        let hs = detect(&[page], &[]);
        assert!(
            hs.iter()
                .any(|h| h.text.starts_with("How a small protein") && h.evidence == Evidence::Title),
            "{hs:?}"
        );
        assert!(
            hs.iter().all(
                |h| !matches!(h.text.as_str(), "ORIGINAL STUDY" | "Free to Read")
                    && !h.text.starts_with("Ann Lee")
            ),
            "{hs:?}"
        );
    }

    #[test]
    fn a_forms_numbers_instructions_and_header_labels_are_no_headings() {
        for t in [
            "OMB No. 1545-0074",
            "Cat. No. 12345A",
            "Schedule B (Form 1040) 2024",
            "Form 2210 (2024)",
            "Form 2210 (Rev. 1-2024)",
        ] {
            assert!(form_number(t), "{t}");
        }
        for t in ["Schedule K", "Form 1099-B", "Formation of the Committee"] {
            assert!(!form_number(t), "{t}");
        }
        for t in [
            "See the separate instructions.",
            "See page 4",
            "See instructions",
        ] {
            assert!(refers_elsewhere(t), "{t}");
        }
        for t in ["See also", "Seeing the Forest"] {
            assert!(!refers_elsewhere(t), "{t}");
        }
        // A form's header: column heads stacked over their notes, keys and
        // short labels around them, all at the body's size.
        let cell = |t: &str, x0: f64, y0: f64, bold: bool| {
            let mut b = at(t, 8.0, y0, bold);
            b.bbox.x0 = x0;
            b.bbox.x1 = x0 + 6.0 * t.len() as f64;
            b
        };
        let page = vec![
            cell("Fee or refund, if any", 400.0, 300.0, true),
            cell("(c)", 360.0, 306.0, false),
            cell("(f)", 560.0, 306.0, false),
            cell("Cost", 300.0, 309.0, false),
            cell("Enter a code in column (g).", 400.0, 310.0, false),
        ];
        assert!(in_header_band(&page, 0, 8.0));
        // Over its paragraph, beside other columns' heads: a heading.
        let mut para = blk(
            "A paragraph of the column runs over two lines here.",
            8.0,
            2,
        );
        para.bbox = BBox {
            x0: 400.0,
            y0: 311.0,
            x1: 520.0,
            y1: 327.0,
        };
        let mut page2 = page.clone();
        page2[4] = para;
        assert!(!in_header_band(&page2, 0, 8.0));
        // Set larger than the text, as a slide's card titles are: no label.
        let mut big = page.clone();
        big[0].font_size = 14.0;
        assert!(!in_header_band(&big, 0, 8.0));
    }

    #[test]
    fn a_forms_keys_answers_footers_and_line_captions_are_no_headings() {
        for t in ["(a) (b)", "(f) (g)", "Yes No", "YES NO", "Yes No N/A"] {
            assert!(keys_or_answers(t), "{t}");
        }
        for t in ["Part I", "(a) Definitions", "Notes", "No Surprises"] {
            assert!(!keys_or_answers(t), "{t}");
        }
        assert!(refers_elsewhere(
            "For Paperwork Reduction Act Notice, see your tax return instructions."
        ));
        assert!(!refers_elsewhere("Other Information (see instructions)"));
        assert!(line_caption("b Refunded 37b"));
        for t in ["Phase 2b", "a Introduction", "Schedule K", "b Refunded 37"] {
            assert!(!line_caption(t), "{t}");
        }
        // A line that stops mid-phrase runs on into the next.
        for t in ["5 Expenses recorded on books this year not", "Tax and"] {
            assert!(ends_mid_phrase(t), "{t}");
        }
        for t in ["Part A", "Schedule A", "Terms and Conditions"] {
            assert!(!ends_mid_phrase(t), "{t}");
        }
        let page = vec![
            blk("(c) (d)", 8.0, 1),
            blk(
                "For Paperwork Reduction Act Notice, see separate instructions.",
                8.0,
                1,
            ),
            body(),
        ];
        let mut page = page;
        page[0].bold = true;
        page[1].bold = true;
        // Kept headings until runs of bold heads are judged, marked as a
        // form's text.
        let hs = detect(&[page], &[]);
        assert!(!hs.is_empty() && hs.iter().all(|h| h.form_text), "{hs:?}");
    }

    #[test]
    fn a_tables_heads_over_its_figures_are_no_headings() {
        let cell = |t: &str, x0: f64, x1: f64, y: f64| {
            let mut b = at(t, 8.0, y, true);
            b.bbox.x0 = x0;
            b.bbox.x1 = x1;
            b
        };
        // A header row over rows of figures, the rows 22pt apart.
        let page = vec![
            cell("95% CI", 140.0, 170.0, 158.0),
            cell("%", 210.0, 216.0, 158.0),
            cell("95%CI", 256.0, 282.0, 158.0),
            cell("64.1 - 90.0", 141.0, 175.0, 181.0),
        ];
        assert!(heads_figures(&page, 2), "a head beside one over figures");
        assert!(heads_figures(&page, 0));
        // A stub over its row's figures.
        let page = vec![
            cell("Total assets, net", 50.0, 120.0, 100.0),
            cell("2,482 2,395", 60.0, 110.0, 110.0),
        ];
        assert!(heads_figures(&page, 0));
        // A slide's card titles side by side, over text.
        let page = vec![
            cell("Our Purpose", 50.0, 110.0, 100.0),
            cell("Our Mission", 200.0, 260.0, 100.0),
            cell("We build tools people trust", 50.0, 180.0, 112.0),
            cell("Every team, every day", 200.0, 300.0, 112.0),
        ];
        assert!(!heads_figures(&page, 0) && !heads_figures(&page, 1));
        assert!(
            bare_figures("80.0 (28/35)") && bare_figures("$ 2,395") && !bare_figures("Q2 2025")
        );
    }

    #[test]
    fn a_charts_axis_titles_are_no_headings() {
        let placed = |t: &str, x0: f64, x1: f64, y: f64, bold: bool| {
            let mut b = at(t, 8.0, y, bold);
            b.bbox.x0 = x0;
            b.bbox.x1 = x1;
            b
        };
        // An x-axis title under its scale's values.
        let mut page: Vec<Block> = ["0", "0.5", "1", "1.5", "2"]
            .iter()
            .enumerate()
            .map(|(k, t)| {
                placed(
                    t,
                    100.0 + 40.0 * k as f64,
                    106.0 + 40.0 * k as f64,
                    300.0,
                    false,
                )
            })
            .collect();
        page.push(placed("I - Output Current - A", 140.0, 230.0, 310.0, true));
        assert!(under_a_scale(&page, 5));
        // A heading over a paragraph that opens on figures is no axis's.
        let page = vec![
            placed("12 14 18", 100.0, 130.0, 300.0, false),
            placed("Results", 100.0, 140.0, 310.0, true),
        ];
        assert!(!under_a_scale(&page, 1));
        // A y-axis title set up the plot's side; the page's text runs across.
        let mut side = placed("Recovery, in percent", 40.0, 48.0, 100.0, true);
        side.lines[0].rotation_bucket = 90;
        let page = vec![side, body(), body()];
        assert!(turned(&page, &page[0]));
        // On a page turned as a whole, its headings turn with its text.
        let mut landscape = vec![blk("Summary", 10.0, 1), body(), body()];
        for b in &mut landscape {
            for l in &mut b.lines {
                l.rotation_bucket = 90;
            }
        }
        assert!(!turned(&landscape, &landscape[0]));
        // Level text beside more turned labels than level lines.
        let mut labels: Vec<Block> = (0..4)
            .map(|k| {
                let mut b = placed("SFF Carriers", 40.0, 48.0, 100.0 + 20.0 * k as f64, false);
                b.lines[0].rotation_bucket = 90;
                b
            })
            .collect();
        labels.push(blk("HDD SKU Description Decoder", 14.0, 1));
        assert!(!turned(&labels, &labels[4]));
    }

    #[test]
    fn a_bold_label_over_its_value_is_no_heading() {
        let mut label = at("Date of Preparation", 7.0, 100.0, true);
        label.bbox.x1 = 200.0;
        let mut value = at("Jun 5, 2026", 7.0, 108.0, false);
        value.bbox.x1 = 160.0;
        let mut head = at("Volatile Ingredients", 7.0, 200.0, true);
        head.bbox.x1 = 200.0;
        let mut text = at(
            "The product contains water and a binder in the amounts below.",
            7.0,
            208.0,
            false,
        );
        text.bbox.x1 = 400.0;
        let mut answered = at("Photochemically Reactive", 7.0, 300.0, true);
        answered.bbox.x1 = 200.0;
        let mut answer = at("No", 7.0, 308.0, false);
        answer.bbox.x1 = 90.0;
        let mut ratings = at("Power and Ratings", 7.0, 400.0, true);
        ratings.bbox.x1 = 200.0;
        let mut first = at("120 V~ 60 Hz", 7.0, 408.0, false);
        first.bbox.x1 = 150.0;
        let mut more = at("Load rating 600 W incandescent", 7.0, 416.0, false);
        more.bbox.x1 = 250.0;
        let page = vec![
            label, value, head, text, answered, answer, ratings, first, more,
        ];
        assert!(label_over_value(&page, 0));
        assert!(!label_over_value(&page, 2), "a heading over a sentence");
        assert!(label_over_value(&page, 4));
        assert!(
            !label_over_value(&page, 6),
            "a heading over lines of specifications"
        );
    }

    #[test]
    fn an_acronym_set_like_the_documents_heads_is_one() {
        // An 8pt body under 10pt capital heads; an acronym set as they are
        // is one of them, set at the body's size or at a size no head uses
        // it is not.
        let mut page = Vec::new();
        let mut y = 50.0;
        for head in ["MOUNTING THE BRACKET", "UL", "WIRING THE FIXTURE"] {
            page.push(at(head, 10.0, y, false));
            for k in 0..2 {
                let mut para = at(
                    "Fasten the bracket to the box with the screws supplied.",
                    8.0,
                    y + 14.0 + 30.0 * k as f64,
                    false,
                );
                para.lines = vec![para.lines[0].clone(); 3];
                para.bbox.y1 = para.bbox.y0 + 26.0;
                page.push(para);
            }
            y += 90.0;
        }
        page.push(at("CE", 8.0, y, false));
        page.push(at("IP", 14.0, y + 20.0, false));
        let hs = detect(&[page], &[]);
        let level = |t: &str| hs.iter().find(|h| h.text == t).map(|h| h.level);
        assert!(level("UL").is_some());
        assert_eq!(level("UL"), level("MOUNTING THE BRACKET"));
        assert_eq!(level("CE"), None);
        assert_eq!(level("IP"), None);
        // Nor where it heads no prose: a role under a name, set as the
        // heads are.
        let mut page = vec![at("MOUNTING THE BRACKET", 10.0, 50.0, false)];
        let mut para = at("Fasten the bracket to the box.", 8.0, 64.0, false);
        para.lines = vec![para.lines[0].clone(); 3];
        para.bbox.y1 = 90.0;
        page.push(para);
        page.push(at("Dana Reyes", 10.0, 120.0, false));
        page.push(at("CTO", 10.0, 132.0, false));
        page.push(at("Leads the team.", 8.0, 146.0, false));
        let hs = detect(&[page], &[]);
        assert!(hs.iter().all(|h| h.text != "CTO"));
    }

    #[test]
    fn a_heads_bracketed_note_set_lighter_keeps_its_weight() {
        let mut noted = at("Roof Vents (Ventilation Baffles)", 10.0, 0.0, false);
        noted.lines[0].opens_bold = true;
        let mut bold = at("Wall Kit", 10.0, 0.0, true);
        bold.lines[0].opens_bold = true;
        assert_eq!(style_of(&noted), style_of(&bold));
        // A sentence opening on a bold name runs on, unbracketed.
        let mut sentence = at(
            "Gun Foam accessories include cleaner and guns.",
            10.0,
            0.0,
            false,
        );
        sentence.lines[0].opens_bold = true;
        assert_ne!(style_of(&sentence), style_of(&bold));
    }

    #[test]
    fn bold_lettering_far_below_the_body_is_form_text() {
        // A 10pt body; a bold head at its size, a bold sidebar head at
        // seven tenths of it, and a badge's bold lettering at two fifths.
        let mut page = vec![at("Overview", 10.0, 100.0, true)];
        for k in 0..8 {
            page.push(at(
                "The bracket mounts on any level wall in minutes.",
                10.0,
                120.0 + 14.0 * k as f64,
                false,
            ));
        }
        page.push(at("Quick Facts", 7.0, 300.0, true));
        page.push(at("BUILT TO LAST", 4.0, 400.0, true));
        // A watermark's letters, turned and each a line of its own, outnumber
        // the page's text: they are no measure of it.
        let mut marked = page.clone();
        for k in 0..30 {
            let mut w = at("W", 30.0, 20.0 * k as f64, false);
            w.lines[0].rotation_bucket = 45;
            marked.push(w);
        }
        let hs = detect(&[page], &[]);
        let flag = |t: &str| hs.iter().find(|h| h.text == t).map(|h| h.form_text);
        assert_eq!(flag("Overview"), Some(false));
        assert_eq!(flag("Quick Facts"), Some(false));
        assert_eq!(flag("BUILT TO LAST"), Some(true));
        let hs = detect(&[marked], &[]);
        let flag = |t: &str| hs.iter().find(|h| h.text == t).map(|h| h.form_text);
        assert_eq!(flag("Overview"), Some(false));
        assert_eq!(flag("BUILT TO LAST"), Some(true));
    }

    #[test]
    fn a_stack_of_labels_over_values_set_in_is_no_heading() {
        // Three bold labels, each over its value set in by an em at its
        // size: a lot number, a name, and an address of two lines.
        let pair = |label: &str, value: &str, lines: usize, y: f64| {
            let mut l = at(label, 9.0, y, true);
            l.bbox.x0 = 40.0;
            l.bbox.x1 = 140.0;
            let mut v = at(value, 9.0, y + 12.0, false);
            v.lines = vec![v.lines[0].clone(); lines];
            v.bbox.x0 = 49.0;
            v.bbox.x1 = 220.0;
            v.bbox.y1 = v.bbox.y0 + 12.0 * lines as f64;
            [l, v]
        };
        let mut page: Vec<Block> = Vec::new();
        page.extend(pair("LOT CODE", "A7-301", 1, 100.0));
        page.extend(pair("ITEM NAME", "Coated Steel Hinge, Black", 1, 125.0));
        page.extend(pair("MAKER", "Northwind Hardware Works", 2, 150.0));
        for i in [0, 2, 4] {
            assert!(stacked_field(&page, i), "label {i}");
        }
        // One bold head over text set in is a heading over its text: no
        // label stands with it.
        let lone = pair(
            "Installation",
            "Mount the bracket on a level wall",
            2,
            100.0,
        );
        assert!(!stacked_field(&lone, 0));
        // Nor is a value a sentence.
        let mut page: Vec<Block> = Vec::new();
        page.extend(pair("Scope", "This guide covers the hinge.", 1, 100.0));
        page.extend(pair("Care", "Wipe it with a dry cloth.", 1, 125.0));
        assert!(!stacked_field(&page, 0) && !stacked_field(&page, 2));
        // Nor a list's item: questions, each over its answer as a bullet.
        let mut page: Vec<Block> = Vec::new();
        page.extend(pair(
            "Why a bracket?",
            "• It holds the shelf level",
            1,
            100.0,
        ));
        page.extend(pair("Why steel?", "• It does not warp in heat", 1, 125.0));
        assert!(!stacked_field(&page, 0) && !stacked_field(&page, 2));
    }

    #[test]
    fn a_signers_name_and_office_are_no_headings() {
        // Under a line to sign on: the name, and the office under it.
        let page = vec![
            at("____________________", 10.0, 500.0, false),
            at("Ann Lee", 10.0, 512.0, false),
            at("Secretary", 10.0, 524.0, false),
        ];
        assert!(signs_off(&page, 1) && signs_off(&page, 2));
        // Under a conformed signature.
        let page = vec![
            at("/s/ A. Lee", 10.0, 500.0, false),
            at("Ann Lee", 10.0, 512.0, false),
            at("Chief Executive Officer", 10.0, 524.0, false),
        ];
        assert!(signs_off(&page, 1));
        // A signature set as text over the name printed, over the office.
        let page = vec![
            at("Ann Lee", 16.0, 480.0, false),
            at("Ann Lee", 10.0, 500.0, false),
            at("Chief Data Officer, Example", 10.0, 512.0, false),
        ];
        assert!(signs_off(&page, 0));
        // A company over its signer over the office.
        let page = vec![
            at("Example Consulting Group", 10.0, 500.0, true),
            at("Bo Chen", 10.0, 512.0, false),
            at("Commercial Counsel", 10.0, 524.0, false),
        ];
        assert!(signs_off(&page, 0));
        // A section over a list of officers, and a list's entry with the
        // office after the name, head their lists.
        let page = vec![
            at("Board of Directors", 12.0, 100.0, true),
            at("Cy Wu", 10.0, 112.0, false),
            at("Chairman", 10.0, 124.0, false),
        ];
        assert!(!signs_off(&page, 0));
        let page = vec![
            at("Attendance", 12.0, 100.0, true),
            at("Dee Park, Chairman", 10.0, 112.0, false),
        ];
        assert!(!signs_off(&page, 0));
        for t in [
            "Secretary",
            "Chief Executive Officer",
            "Vice President, Finance",
            "Secretary of State",
        ] {
            assert!(office_line(t), "{t}");
        }
        for t in [
            "CHAIRMAN PARK. Good morning, everyone.",
            "MANAGER NAME",
            "CERTIFICATE OF SECRETARY",
            "Work Chair",
            "Dee Park, Chairman",
        ] {
            assert!(!office_line(t), "{t}");
        }
    }

    #[test]
    fn fields_forms_and_signatures_are_no_headings() {
        // A label and its value on one line.
        assert!(field_on_its_line("CRI: 90+"));
        assert!(field_on_its_line(
            "Hours: 8:30 am - 6:00 pm EST (Monday - Friday)"
        ));
        assert!(field_on_its_line("Initial Closing Date: May 15, 2019"));
        assert!(!field_on_its_line("Policy Rules: Limitations"));
        assert!(!field_on_its_line(
            "Part II: Application of Section 67 Limitation to Specific Deductions"
        ));
        // A form's label.
        assert!(form_label("Model #:") && form_label("Location:"));
        assert!(!form_label(
            "The rules for excavations should also address:"
        ));
        // A party's role and name over the line it signs on.
        let page = vec![
            at("INVESTOR:", 10.0, 0.0, true),
            at("Acme Fund, LP", 10.0, 14.0, true),
            at("By: ____________", 10.0, 40.0, false),
        ];
        assert!(signs_off(&page, 0) && signs_off(&page, 1));
        assert!(!signs_off(&page, 2));
        // A head over its section's last paragraph, before a signature.
        let page = vec![
            at("Notation Vote", 10.0, 0.0, true),
            blk(
                "By notation vote completed on January 2, the Committee unanimously approved the minutes.",
                10.0,
                2,
            ),
            at("_______________________", 10.0, 60.0, false),
            at("A. Secretary", 10.0, 74.0, true),
        ];
        assert!(!signs_off(&page, 0));
        assert!(signature_page_label("[Signature Page to Voting Agreement]"));
        // A value under its bold label, at its size; not a section under
        // its title, set apart by a gap.
        let page = vec![
            at("PRODUCT NUMBER", 9.6, 0.0, true),
            at("K25W951", 9.6, 13.4, false),
            at("Overview", 9.6, 40.0, true),
            at("A short line", 9.6, 62.0, false),
        ];
        assert!(value_under_label(&page, 1));
        assert!(!value_under_label(&page, 3));
    }

    #[test]
    fn a_letterhead_is_no_heading() {
        let mut name = at("ACME MANUFACTURING CO., INC.", 9.3, 35.0, true);
        name.bbox.x0 = 200.0;
        name.bbox.x1 = 400.0;
        let mut division = at("U.S.A. FOODSERVICE DIVISION", 9.3, 51.0, true);
        division.bbox.x0 = 210.0;
        division.bbox.x1 = 390.0;
        let mut address = at(
            "2001 East Lane, Springfield, Missouri 63366-4434 (636)240-2400",
            7.2,
            69.0,
            false,
        );
        address.bbox.x0 = 180.0;
        address.bbox.x1 = 420.0;
        let mut rest = body();
        rest.bbox.y0 = 600.0;
        rest.bbox.y1 = 640.0;
        let page = vec![name, division, address, rest];
        assert!(over_contact(&page, 0) && over_contact(&page, 1));
        let hs = detect(&[page], &[]);
        assert!(hs.is_empty(), "{hs:?}");
        // An article's title over its authors, over an affiliation that
        // gives an e-mail address: the authors are not set like the title.
        let mut title = at(
            "Effect of Process Parameters on Alloy Properties",
            17.9,
            115.0,
            false,
        );
        title.bbox.x0 = 77.0;
        title.bbox.x1 = 500.0;
        let mut authors = at("Ann Lee, Bo Chen and Cy Wu *", 10.0, 140.0, false);
        authors.bbox.x0 = 76.0;
        authors.bbox.x1 = 400.0;
        let mut affiliation = at(
            "School of Materials, Taiyuan 030051, China; ann@example.edu",
            9.0,
            155.0,
            false,
        );
        affiliation.bbox.x0 = 82.0;
        affiliation.bbox.x1 = 520.0;
        let page = vec![title, authors, affiliation];
        assert!(!over_contact(&page, 0));
    }

    #[test]
    fn a_long_title_is_a_heading_when_numbered_and_set_larger_or_set_as_display() {
        let long = "3. Perspective of supply and demand balance of wood pellets and cost structure in Japan";
        let page = vec![blk(long, 13.0, 1), body(), body(), body()];
        let hs = detect(&[page], &[]);
        assert!(hs.iter().any(|h| h.text == long), "{hs:?}");
        // Numbered at the body size: a list's item or a note's.
        let item = "1. When changing objectives from scanning power to lower power to high power the following changes will occur:";
        let page = vec![blk("Steps", 14.0, 1), body(), blk(item, 10.0, 1), body()];
        let hs = detect(&[page], &[]);
        assert!(hs.iter().all(|h| h.text != item), "{hs:?}");
        // Display type: a slide's statement.
        let statement = "Upstage offers 3 AI packs that process unstructured information and data, making a tangible impact";
        let page = vec![
            blk("AI Pack", 13.0, 1),
            blk(statement, 22.0, 1),
            body(),
            body(),
        ];
        let hs = detect(&[page], &[]);
        assert!(hs.iter().any(|h| h.text == statement), "{hs:?}");
    }

    #[test]
    fn an_addresss_first_line_is_no_heading() {
        let at = |t: &str, y: f64| {
            let mut b = blk(t, 9.0, 1);
            b.bbox.x0 = 449.0;
            b.bbox.y0 = y;
            b.bbox.y1 = y + 9.0;
            for l in &mut b.lines {
                l.bbox = b.bbox;
            }
            b
        };
        let page = vec![
            at("ACME Building Products, LLC", 634.0),
            at("One ACME Way Springfield, OH 45501 USA", 646.0),
            at("1-800-555-0100 www.acme.com", 658.0),
            at("Installation", 700.0),
            at(
                "Blow the insulation to the thickness the chart gives",
                712.0,
            ),
        ];
        assert!(opens_an_address(&page[0], &page));
        assert!(!opens_an_address(&page[3], &page));
    }

    #[test]
    fn contact_lines_are_not_title_like() {
        for t in [
            "seagate.com",
            "hermanmiller.com/aeron-chair",
            "Learn more at thermofisher.com/centrifuges",
            "Your local distributor: www.eppendorf.com/contact",
            "2016.02 panasonic.net/id/pidsx/global",
            "sales@example.com",
            "For technical service call: 1-800-248-9606",
            "Customer Service 1-800-346-1272 | Help Line 1-800-GET-PINK\u{AE}",
            "Fax (636) 272-2408",
            "Phone 800.424.8783",
            "Telefon +49 40 538 01-0",
            "https://example.org/report",
        ] {
            assert!(!title_like(t), "{t}");
        }
        for t in [
            "Introduction to ASP.NET",
            "Node.js Guide",
            "Meeting Minutes 2019-07-25",
            "UL 2043-2013 Compliance",
            "COVID-19",
            "Highlights 2023 2024",
            "Route 101 2024",
            "T-49-HC",
            "Molecular Weight: 259.3490",
        ] {
            assert!(title_like(t), "{t}");
        }
        // A local number is hyphenated; set apart by a point, it is a
        // decimal.
        assert!(ends_with_contact("Fax 272-2408"));
        // A line of an address, not a sentence closing on one.
        assert!(contact_line("1-800-GET-PINK® www.owenscorning.com"));
        assert!(contact_line("www.example.com"));
        assert!(contact_line("Visit www.example.com."));
        assert!(!contact_line(
            "Meeting held virtually at www.example.com/meeting2025."
        ));
        assert!(!contact_line("Questions may be sent to help@example.com."));
        assert!(!ends_with_contact("Exact Mass: 259.1572"));
    }

    #[test]
    fn numbering_ignores_prose_starting_with_a_number() {
        // A year or quantity opening a sentence must not become a heading.
        assert_eq!(
            numbering_depth("2026 was a difficult year for the market"),
            None
        );
        assert_eq!(numbering_depth("15% of respondents"), None);
        assert_eq!(numbering_depth("plain prose"), None);
    }

    #[test]
    fn numbering_rejects_sentences_and_lowercase_continuations() {
        assert_eq!(numbering_depth("III. his report defines"), None);
        assert_eq!(numbering_depth("2.6 times that of"), None);
        assert_eq!(
            numbering_depth("6. Compared to 38% in July 2020 and 22% in October 2020."),
            None
        );
    }

    #[test]
    fn bare_numbering_needs_typographic_and_lexical_corroboration() {
        assert!(bare_numbering_is_corroborated(
            "7 Variants of Observer Models",
            &blk("7 Variants of Observer Models", 10.0, 1),
            10.0
        ));
        assert!(!bare_numbering_is_corroborated(
            "68 APPLIED FLUID MECHANICS LAB MANUAL",
            &blk("68 APPLIED FLUID MECHANICS LAB MANUAL", 7.0, 1),
            10.0
        ));
        assert!(!bare_numbering_is_corroborated(
            "1 OCR-F1 92.",
            &blk("1 OCR-F1 92.", 12.0, 1),
            10.0
        ));
        assert!(!bare_numbering_is_corroborated(
            "2 Q(h) − Q(2h)",
            &blk("2 Q(h) − Q(2h)", 10.0, 1),
            10.0
        ));
    }

    #[test]
    fn display_word_before_a_lettered_item_is_a_heading() {
        let pages = vec![vec![
            blk("Replace", 10.0, 1),
            blk("l. Replace Plastics with Recyclable Materials.", 10.0, 2),
            blk("ordinary body prose", 10.0, 20),
        ]];
        let headings = detect(&pages, &[]);
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].text, "Replace");
        assert_eq!(headings[0].evidence, Evidence::Sequence);
    }

    #[test]
    fn a_footnote_number_is_not_a_section_number() {
        let mut b = blk("95 Ibid.", 10.0, 1);
        assert!(bare_numbering_is_corroborated("95 Ibid.", &b, 10.0));
        b.lines[0].opens_with_script = true;
        assert!(!bare_numbering_is_corroborated("95 Ibid.", &b, 10.0));
    }

    #[test]
    fn a_list_item_is_not_a_numbered_section() {
        let mut b = blk("1. Label four plastic bags", 10.0, 1);
        assert_eq!(detect(&[vec![b.clone()]], &[]).len(), 1);
        b.marker = Some("1.".into());
        assert!(detect(&[vec![b]], &[]).is_empty());
    }

    #[test]
    fn a_bare_zero_numbers_nothing() {
        assert!(!bare_numbering_is_corroborated(
            "0 — Not started",
            &blk("0 — Not started", 10.0, 1),
            10.0
        ));
        assert!(bare_numbering_is_corroborated(
            "1 — Yes",
            &blk("1 — Yes", 10.0, 1),
            10.0
        ));
    }

    #[test]
    fn a_bare_number_naming_two_titles_is_a_code() {
        // Sections are numbered once. Where `1` labels two different lines,
        // neither is a section — nor is the `1` set only once on a later
        // page that a page cap would never reach.
        let pages = vec![
            vec![blk("1 — Yes", 10.0, 1), blk("2 — Weekly", 10.0, 1)],
            vec![blk("1 — Supplier on file", 10.0, 1)],
            vec![blk("3 — Approved with changes", 10.0, 1)],
        ];
        let h = detect(&pages, &[]);
        let numbered: Vec<&str> = h
            .iter()
            .filter(|h| h.evidence == Evidence::Numbering)
            .map(|h| h.text.as_str())
            .collect();
        assert_eq!(
            numbered,
            vec!["2 — Weekly", "3 — Approved with changes"],
            "{h:?}"
        );

        // A paper's sections, each number once: untouched.
        let pages = vec![
            vec![
                blk("1 Introduction", 10.0, 1),
                blk("2 Related Work", 10.0, 1),
            ],
            vec![blk("3 Method", 10.0, 1)],
        ];
        let h = detect(&pages, &[]);
        assert_eq!(h.len(), 3);
        assert!(h.iter().all(|h| h.evidence == Evidence::Numbering));
    }

    #[test]
    fn a_page_of_coded_answers_is_not_an_outline() {
        // A questionnaire answers every item with `code — label`, each
        // answer given once on the page. One at a time each is a numbered
        // section; a dozen to the page they are a coding scheme.
        let answers = [
            "1 — Yes",
            "2 — Somewhat agree",
            "3 — Weekly",
            "4 — Approved with changes",
            "5 — Pending review",
            "6 — Supplier on file",
            "7 — Delivered late",
            "8 — Returned unused",
            "9 — Not applicable",
            "11 — Partially complete",
            "12 — Under warranty",
            "13 — Awaiting parts",
        ];
        let page: Vec<Block> = answers.iter().map(|a| blk(a, 10.0, 1)).collect();
        let h = detect(&[page], &[]);
        assert!(
            h.iter().all(|h| h.evidence != Evidence::Numbering),
            "coded answers must not be numbered sections: {h:?}"
        );

        // The same lines three to a page are sections.
        let page: Vec<Block> = answers[..3].iter().map(|a| blk(a, 10.0, 1)).collect();
        let h = detect(&[page], &[]);
        assert_eq!(h.len(), 3);
        assert!(h.iter().all(|h| h.evidence == Evidence::Numbering));
    }

    #[test]
    fn bare_single_number_needs_a_trailing_dot() {
        // "5 Specifications" is real in component datasheets, but indistinguishable
        // from prose without more context, so the outline carries those. A
        // trailing dot is unambiguous.
        // A bare number followed by title case is a section heading...
        assert_eq!(numbering_depth("5 Specifications"), Some(1));
        assert_eq!(numbering_depth("7 Variants of SJ Observer Models"), Some(1));
        assert_eq!(numbering_depth("5. Specifications"), Some(1));
        // ...followed by lower case it is prose.
        assert_eq!(numbering_depth("3 of the respondents said otherwise"), None);
    }

    #[test]
    fn outline_match_beats_typography() {
        // Body-sized text that the outline names is still a heading, and takes
        // the outline's level rather than a font-derived one.
        let pages = vec![vec![
            blk("Introduction", 10.0, 1),
            blk("body text here", 10.0, 8),
        ]];
        let outline = vec![OutlineItem {
            title: "Introduction".into(),
            level: 2,
            page: None,
            y: None,
        }];
        let h = detect(&pages, &outline);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].level, 2);
        assert_eq!(h[0].evidence, Evidence::Outline);
    }

    #[test]
    fn figure_captions_are_not_headings() {
        assert!(is_caption(
            "Figure 2.1: Surveyed MSMEs by size across sectors (%)"
        ));
        assert!(is_caption("Table 3 Results of the second survey"));
        assert!(is_caption("Diagram 4 Distribution of Instagram Content"));
        assert!(is_caption("Figure 5-1. Low-Level Output Voltage"));
        assert!(!is_caption("Figures of speech in modern prose"));
        assert!(!is_caption("The Table Mountain region"));
        assert!(!is_caption("2. General Profile of MSMEs"));
    }

    #[test]
    fn a_bold_run_in_lead_in_is_not_a_heading() {
        // LaTeX \paragraph{}: bold phrase, then the paragraph it introduces.
        assert!(runs_into_prose(
            "Filtered task names. We present task names"
        ));
        assert!(runs_into_prose(
            "Results on data contamination. To show the in-"
        ));
    }

    #[test]
    fn abbreviations_and_trailing_stops_are_not_run_ins() {
        assert!(!runs_into_prose("CHAPTER 1."));
        assert!(!runs_into_prose("Introduction"));
        // The dot abbreviates; the heading continues.
        assert!(!runs_into_prose("Results vs. Baseline Methods"));
        assert!(!runs_into_prose("See Fig. 3 for details"));
        assert!(!runs_into_prose("Method of Doe et al. Revisited"));
    }

    #[test]
    fn one_group_of_a_row_is_not_the_title() {
        // A running head set as two groups across the page's top row: the
        // college at the left, the course's code larger at the right.
        let at = |mut b: Block, x0: f64, y0: f64, x1: f64| {
            let h = b.bbox.height();
            b.bbox = BBox {
                x0,
                y0,
                x1,
                y1: y0 + h,
            };
            b
        };
        let pages = vec![vec![
            at(blk("MOHAVE COMMUNITY COLLEGE", 9.8, 1), 72.0, 39.0, 200.0),
            at(blk("BIO181", 12.0, 1), 489.0, 41.0, 530.0),
            at(blk("body text", 10.0, 20), 72.0, 74.0, 530.0),
        ]];
        let h = detect(&pages, &[]);
        assert!(h.iter().all(|h| h.evidence != Evidence::Title), "{h:?}");
    }

    #[test]
    fn font_size_fallback_when_no_outline() {
        let pages = vec![vec![
            blk("Big Title", 20.0, 1),
            blk("Sub Title", 14.0, 1),
            blk("body text", 10.0, 20),
        ]];
        let h = detect(&pages, &[]);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].level, 1);
        assert_eq!(h[1].level, 2);
        assert_eq!(h[0].evidence, Evidence::Title);
    }

    #[test]
    fn a_cover_title_stacked_a_word_or_two_to_a_line_is_the_title() {
        // Six words on four lines, at nearly three times the body size.
        let stacked = |rows: &[&str]| {
            let mut b = blk(rows[0], 28.0, 1);
            b.lines = rows
                .iter()
                .map(|r| {
                    let mut l = b.lines[0].clone();
                    l.text = (*r).into();
                    l
                })
                .collect();
            b
        };
        let pages = vec![vec![
            stacked(&["PROCAT", "SISTEMA DE", "AISLAMIENTO SUELTO", "PROFESIONAL"]),
            blk("body text", 10.0, 20),
        ]];
        let h = detect(&pages, &[]);
        assert_eq!(h.len(), 1, "{h:?}");
        assert_eq!(h[0].evidence, Evidence::Title);
        // Four lines of ordinary length are not stacked: at display size
        // they are a title all the same, as an article's runs on (#367);
        // set a little larger than the text, they are not.
        let rows = [
            "Regional Economy Report",
            "for the Northern",
            "and Western",
            "Districts Today",
        ];
        let pages = vec![vec![stacked(&rows), blk("body text", 10.0, 20)]];
        assert!(detect(&pages, &[])
            .iter()
            .any(|h| h.evidence == Evidence::Title));
        let mut smaller = stacked(&rows);
        smaller.font_size = 12.0;
        for l in &mut smaller.lines {
            l.font_size = 12.0;
        }
        let pages = vec![vec![smaller, blk("body text", 10.0, 20)]];
        assert!(detect(&pages, &[])
            .iter()
            .all(|h| h.evidence != Evidence::Title));
        // Nor a badge's text layer, stacked, if it is hardly letters.
        let pages = vec![vec![
            stacked(&["a1111111111", "a1111111111", "a1111111111", "a1111111111"]),
            blk("body text", 10.0, 20),
        ]];
        assert!(detect(&pages, &[])
            .iter()
            .all(|h| h.evidence != Evidence::Title));
    }

    #[test]
    fn heading_identity_is_its_source_block_not_its_text() {
        let pages = vec![vec![
            blk("Repeated label", 20.0, 1),
            blk("body text begins here", 10.0, 20),
            blk("Repeated label", 10.0, 1),
        ]];
        let h = detect(&pages, &[]);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].block_index, 0);
    }

    #[test]
    fn isolated_first_page_title_can_share_body_size() {
        let mut title = blk("Print vs. Digital", 11.5, 1);
        title.bbox = BBox {
            x0: 0.0,
            y0: 10.0,
            x1: 100.0,
            y1: 21.5,
        };
        let mut body = blk("ordinary body prose here", 11.5, 20);
        body.bbox = BBox {
            x0: 0.0,
            y0: 40.0,
            x1: 100.0,
            y1: 51.5,
        };
        let hs = detect(&[vec![title, body]], &[]);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].evidence, Evidence::Title);
    }

    #[test]
    fn title_search_stops_when_prose_begins() {
        let mut prose = blk(
            "The jet velocity can be assumed to remain constant over the measured distance",
            11.0,
            2,
        );
        prose.bbox = BBox {
            x0: 0.0,
            y0: 10.0,
            x1: 100.0,
            y1: 32.0,
        };
        let mut equation = blk("Rearranging Equation (8) gives:", 11.0, 1);
        equation.bbox = BBox {
            x0: 0.0,
            y0: 80.0,
            x1: 100.0,
            y1: 91.0,
        };
        let hs = detect(&[vec![prose, equation]], &[]);
        assert!(hs.iter().all(|h| h.evidence != Evidence::Title));
    }

    #[test]
    fn multi_line_blocks_are_not_font_size_headings() {
        // A three-line paragraph set slightly larger than the modal size is
        // still a paragraph — the dense-datasheet false-positive class.
        let pages = vec![vec![
            blk("a fairly long paragraph of body prose that wraps", 10.0, 3),
            blk("table cell", 8.0, 40),
        ]];
        assert!(detect(&pages, &[]).is_empty());
    }

    #[test]
    fn list_items_are_not_font_size_headings() {
        let pages = vec![vec![
            blk("\u{2022} Wide input voltage range", 10.0, 1),
            blk("table cell", 8.0, 40),
        ]];
        assert!(detect(&pages, &[]).is_empty());
        // An option list's boxes and a Word file's symbol bullet, on size
        // or on weight.
        for item in ["\u{2751} Peg rack", "\u{F0B7}\tHigh stiffness"] {
            let pages = vec![vec![blk(item, 10.0, 1), blk("table cell", 8.0, 40)]];
            assert!(detect(&pages, &[]).is_empty(), "{item}");
            let mut bold = blk(item, 8.0, 1);
            bold.bold = true;
            let pages = vec![vec![
                blk("ordinary body prose here", 8.0, 40),
                bold,
                blk("more ordinary body prose here", 8.0, 40),
            ]];
            assert!(detect(&pages, &[]).is_empty(), "bold {item}");
        }
        // A diamond before a bold subsection title leaves it a heading.
        let mut bold = blk("\u{2756} General rules", 8.0, 1);
        bold.bold = true;
        let pages = vec![vec![
            blk("ordinary body prose here", 8.0, 40),
            bold,
            blk("more ordinary body prose here", 8.0, 40),
        ]];
        assert_eq!(detect(&pages, &[]).len(), 1);
    }

    #[test]
    fn levels_are_ranked_by_style_across_the_document() {
        // Three heading styles at one document: 20pt, 14pt, and bold body size.
        // They must come out h1/h2/h3 regardless of which detector found them.
        let mut b = blk("Bold Section", 10.0, 1);
        b.bold = true;
        let pages = vec![vec![
            blk("Big Title", 20.0, 1),
            blk("Sub Title", 14.0, 1),
            b,
            blk("ordinary body prose here", 10.0, 40),
        ]];
        let h = detect(&pages, &[]);
        let mut lv: Vec<usize> = h.iter().map(|x| x.level).collect();
        lv.sort();
        assert_eq!(
            lv,
            vec![1, 2, 3],
            "distinct styles must map to distinct ranks: {h:?}"
        );
    }

    #[test]
    fn a_face_set_shorter_or_italic_at_one_size_is_a_level_below() {
        // A journal sets its sections in a sans, its subsections in a serif
        // with a shorter x-height, and the level under those in the serif's
        // italic, all bold at one size (`Materials and methods` /
        // `Participants` / `Mystic Isle`).
        let head = |text: &str, x_height: f32, italic: bool| {
            let mut b = blk(text, 12.0, 1);
            b.bold = true;
            for l in &mut b.lines {
                l.bold = true;
                l.italic = italic;
                l.x_height = Some(x_height);
            }
            b
        };
        let heads = [
            head("Materials and methods", 0.55, false),
            head("Participants", 0.465, false),
            head("Mystic Isle", 0.465, true),
            head("Vicon", 0.465, false),
            head("Results", 0.559, false),
            head("Outcomes", 0.467, false),
            head("Spatial accuracy", 0.465, true),
        ];
        let mut page = Vec::new();
        for (i, h) in heads.into_iter().enumerate() {
            let y = i as f64 * 120.0;
            let mut h = h;
            h.bbox.y0 = y;
            h.bbox.y1 = y + 12.0;
            let mut text = blk("ordinary body prose here", 10.0, 8);
            text.bbox.y0 = y + 20.0;
            text.bbox.y1 = y + 100.0;
            page.push(h);
            page.push(text);
        }
        let h = detect(&[page], &[]);
        let level = |t: &str| h.iter().find(|x| x.text == t).map(|x| x.level);
        let section = level("Materials and methods").expect("a heading");
        assert_eq!(level("Results"), Some(section), "{h:?}");
        assert_eq!(level("Participants"), Some(section + 1), "{h:?}");
        assert_eq!(level("Vicon"), Some(section + 1), "{h:?}");
        assert_eq!(level("Outcomes"), Some(section + 1), "{h:?}");
        assert_eq!(level("Mystic Isle"), Some(section + 2), "{h:?}");
        assert_eq!(level("Spatial accuracy"), Some(section + 2), "{h:?}");
    }

    #[test]
    fn a_variant_set_directly_over_another_heads_it() {
        // A 10-K sets its items in bold italic, each directly over its first
        // subsection in bold at the same size: there italic is the level
        // above.
        let head = |text: &str, italic: bool| {
            let mut b = blk(text, 12.0, 1);
            b.bold = true;
            for l in &mut b.lines {
                l.bold = true;
                l.italic = italic;
            }
            b
        };
        let mut page = Vec::new();
        let mut y = 0.0;
        let mut put = |mut b: Block, h: f64| {
            b.bbox.y0 = y;
            b.bbox.y1 = y + h;
            y += h + 10.0;
            page.push(b);
        };
        for (item, subs) in [
            ("Item 1. Business", ["General", "Products"]),
            ("Item 2. Properties", ["Locations", "Leases"]),
        ] {
            put(head(item, true), 12.0);
            for sub in subs {
                put(head(sub, false), 12.0);
                put(blk("ordinary body prose here", 10.0, 8), 80.0);
            }
        }
        let h = detect(&[page], &[]);
        let level = |t: &str| h.iter().find(|x| x.text == t).map(|x| x.level);
        let item = level("Item 1. Business").expect("a heading");
        assert_eq!(level("Item 2. Properties"), Some(item), "{h:?}");
        assert_eq!(level("General"), Some(item + 1), "{h:?}");
        assert_eq!(level("Leases"), Some(item + 1), "{h:?}");
    }

    #[test]
    fn a_face_is_told_apart_by_a_step_in_its_x_height() {
        // One face measured over different words, or embedded twice under
        // two names, is one face.
        assert_eq!(faces_by_height(vec![0.53, 0.531, 0.528, 0.53]).len(), 1);
        // Two faces a sixth apart are two, the taller first.
        let faces = faces_by_height(vec![0.465, 0.55, 0.465, 0.559, 0.465]);
        assert_eq!(faces.len(), 2);
        assert!(0.55 >= faces[0] && 0.465 < faces[0]);
        // One heading in an odd face is no level of its own.
        assert_eq!(faces_by_height(vec![0.55, 0.559, 0.546, 0.40]).len(), 1);
        assert_eq!(faces_by_height(vec![0.70, 0.55, 0.559, 0.546]).len(), 1);
    }

    #[test]
    fn numbering_keeps_its_own_depth_through_normalization() {
        let pages = vec![vec![
            blk("6.3.1 Oscillator Frequency", 10.0, 1),
            blk("ordinary body prose here", 10.0, 40),
        ]];
        let h = detect(&pages, &[]);
        assert_eq!(h[0].level, 3, "numbering states its own depth");
    }

    #[test]
    fn a_bookmark_heads_one_place_and_its_running_copies_none() {
        let title = "Appendix 1. Supplemental Figures";
        let page = |p: usize, heading_y: Option<f64>| {
            let mut head = blk(title, 10.0, 1);
            head.page = p;
            head.bold = true;
            head.bbox.x0 = 396.0;
            head.bbox.y0 = 43.0;
            head.bbox.y1 = 53.0;
            let mut blocks = vec![head];
            if let Some(y) = heading_y {
                let mut real = blk(title, 14.0, 1);
                real.page = p;
                real.bold = true;
                real.bbox.y0 = y;
                real.bbox.y1 = y + 14.0;
                blocks.push(real);
            }
            let mut text = body();
            text.page = p;
            blocks.push(text);
            blocks
        };
        // The heading on page 1, where its bookmark lands; the running head
        // at the top of pages 2 to 4.
        let mut pages = vec![page(0, None)];
        pages[0].remove(0);
        pages.push(page(1, Some(80.0)));
        pages[1].remove(0);
        pages.extend((2..5).map(|p| page(p, None)));
        let hs = detect(&pages, &[bookmark(title, 1, Some(1), None)]);
        let found: Vec<(usize, Evidence)> = hs
            .iter()
            .filter(|h| h.text == title)
            .map(|h| (h.page, h.evidence))
            .collect();
        assert_eq!(found, [(1, Evidence::Outline)], "{hs:?}");
    }

    #[test]
    fn bold_line_at_body_size_is_a_heading() {
        let mut h = blk("Market Penetration", 10.0, 1);
        h.bold = true;
        let pages = vec![vec![h, blk("ordinary body prose", 10.0, 30)]];
        let hs = detect(&pages, &[]);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].evidence, Evidence::Bold);
    }

    #[test]
    fn bold_set_larger_than_the_body_is_ranked_by_its_size() {
        let mut part = blk("Guidance", 12.0, 1);
        part.bold = true;
        let mut sub = blk("Market Penetration", 10.0, 1);
        sub.bold = true;
        let title = blk("Managing health and safety", 20.0, 1);
        let pages = vec![vec![title, part, sub, blk("ordinary body prose", 10.0, 30)]];
        let hs = detect(&pages, &[]);
        assert_eq!(hs.len(), 3, "{hs:?}");
        assert_eq!(hs[1].evidence, Evidence::FontSize);
        assert_eq!(hs[2].evidence, Evidence::Bold);
        assert!(hs[1].level < hs[2].level, "{hs:?}");
    }

    #[test]
    fn the_body_is_bold_where_most_glyphs_at_its_size_are() {
        let glyph = |text: &str, size: f32, weight: u32| crate::glyph::Glyph {
            text: text.into(),
            bbox: None,
            page: 0,
            origin: (0.0, 0.0),
            rotation_deg: 0.0,
            font_size: size,
            weight: Some(weight),
            italic: false,
            advance: None,
            draw_index: 0,
        };
        let run = |n: usize, size: f32, weight: u32| (0..n).map(move |_| glyph("a", size, weight));
        // A form: its lines and figures in regular type, read as tables,
        // its bold labels, and larger bold titles. Spaces are not counted.
        let form: Vec<_> = run(400, 9.0, 400)
            .chain((0..300).map(|_| glyph("1", 9.0, 400)))
            .chain(run(500, 9.0, 700))
            .chain((0..900).map(|_| glyph(" ", 9.0, 700)))
            .chain(run(300, 14.0, 700))
            .collect();
        assert_eq!(body_is_bold(&form), Some(false));
        let bold: Vec<_> = run(300, 10.0, 700).chain(run(100, 10.0, 400)).collect();
        assert_eq!(body_is_bold(&bold), Some(true));
        assert_eq!(body_is_bold(&[glyph(" ", 9.0, 700)]), None);
        // Passed in, the glyphs' answer stands over the blocks': a page of
        // short bold labels beside fewer regular lines keeps its bold titles.
        let mut part = blk("Part I Additional Income", 9.0, 1);
        part.bold = true;
        let mut label = blk("Your social security number", 9.0, 1);
        label.bold = true;
        let pages = vec![vec![part, label, blk("Note: report the amounts", 9.0, 1)]];
        assert!(detect(&pages, &[])
            .iter()
            .all(|h| h.evidence != Evidence::Bold));
        let hs = detect_with_body(&pages, &[], 9.0, Some(false));
        assert!(
            hs.iter().any(|h| h.text == "Part I Additional Income"),
            "{hs:?}"
        );
    }

    #[test]
    fn bold_is_ignored_when_the_whole_document_is_bold() {
        let mut a = blk("Heading-ish", 10.0, 1);
        let mut b = blk("body prose", 10.0, 30);
        a.bold = true;
        b.bold = true;
        assert!(detect(&[vec![a, b]], &[]).is_empty());
    }

    #[test]
    fn long_blocks_are_never_headings() {
        let long = "word ".repeat(60);
        let pages = vec![vec![blk(&long, 20.0, 1), blk("body", 10.0, 20)]];
        assert!(detect(&pages, &[]).is_empty());
    }

    #[test]
    fn body_size_is_weighted_by_lines_not_blocks() {
        // Ten one-line captions at 8pt against two big paragraphs at 11pt:
        // the body is 11pt, and block-counting would wrongly say 8pt.
        let mut blocks: Vec<Block> = (0..10).map(|_| blk("caption", 8.0, 1)).collect();
        blocks.push(blk("para", 11.0, 30));
        blocks.push(blk("para", 11.0, 30));
        assert_eq!(body_font_size(&[blocks]), 11.0);
    }

    #[test]
    fn body_size_ties_prefer_the_smaller_font() {
        let blocks = vec![blk("large", 12.0, 10), blk("small", 10.0, 10)];
        assert_eq!(body_font_size(&[blocks]), 10.0);
    }

    #[test]
    fn doubt_carries_the_page_it_measured() {
        let kinds: Vec<(&str, &str)> = vec![
            ("doco:SectionTitle", "size"),
            ("doco:SectionTitle", "size"),
            ("doco:SectionTitle", "size"),
            ("doco:Paragraph", "layout"),
        ];
        assert_eq!(doubt_on_page(7, &kinds).map(|d| d.page), Some(7));
        // The page-less form is the same measurement on page zero, which is
        // why a single-page corpus could not tell the two apart.
        assert_eq!(doubt(&kinds), doubt_on_page(0, &kinds));
    }

    #[test]
    fn a_document_of_mostly_headings_is_doubtful() {
        // 01030000000181: seven of ten elements claim to be titles, none
        // corroborated by an outline or a numbering pattern. Its text scores
        // 0.9654 and its hierarchy 0.2846.
        let kinds: Vec<(&str, &str)> = vec![
            ("doco:SectionTitle", "title"),
            ("doco:SectionTitle", "font-size"),
            ("doco:SectionTitle", "font-size"),
            ("doco:SectionTitle", "font-size"),
            ("doco:SectionTitle", "font-size"),
            ("doco:SectionTitle", "font-size"),
            ("doco:SectionTitle", "font-size"),
            ("doco:Paragraph", "layout"),
            ("doco:Paragraph", "layout"),
            ("doco:Paragraph", "layout"),
        ];
        let d = doubt(&kinds).expect("seven of ten is doubtful");
        assert_eq!(d.titles, 7);
        assert_eq!(d.corroborated, 0, "font size is not corroboration");
    }

    #[test]
    fn an_ordinary_document_is_not_doubtful() {
        let mut kinds: Vec<(&str, &str)> = vec![("doco:SectionTitle", "outline")];
        kinds.extend(std::iter::repeat_n(("doco:Paragraph", "layout"), 9));
        assert!(doubt(&kinds).is_none());
    }

    #[test]
    fn splice_anchors_do_not_change_the_verdict() {
        // The same document, once plain and once with anchors emitted, must
        // be doubted the same way: the caller that routes escalation is the
        // one that asks for anchors, so a ratio counting them made the signal
        // depend on who was looking.
        let plain: Vec<(&str, &str)> = vec![
            ("doco:SectionTitle", "size"),
            ("doco:SectionTitle", "size"),
            ("doco:SectionTitle", "size"),
            ("doco:Paragraph", "layout"),
            ("doco:Paragraph", "layout"),
        ];
        let mut anchored = plain.clone();
        anchored.push(("doco:Figure", "route"));
        anchored.push(("doco:Figure", "table-confidence"));
        anchored.push(("doco:Figure", "table-missing"));
        assert_eq!(doubt(&plain), doubt(&anchored));
        assert!(doubt(&plain).is_some(), "three of five is doubtful");
    }

    #[test]
    fn an_outline_backed_hierarchy_is_reported_as_corroborated() {
        // A slide deck is legitimately mostly titles; the outline says so,
        // and the count is what tells a consumer to trust it.
        let kinds: Vec<(&str, &str)> = vec![
            ("doco:SectionTitle", "outline"),
            ("doco:SectionTitle", "outline"),
            ("doco:SectionTitle", "outline"),
            ("doco:Paragraph", "layout"),
        ];
        let d = doubt(&kinds).expect("density is high");
        assert_eq!(d.corroborated, 3);
    }

    #[test]
    fn a_style_the_document_repeats_as_a_heading_is_a_template() {
        // Three pages carry the same bold block labels; a fourth uses the
        // same style for labels of its own. All of it is the template.
        let titles = [
            (0, Some(3), "Assessment & Plan"),
            (1, Some(3), "Assessment & Plan"),
            (2, Some(3), "Assessment & Plan"),
            (3, Some(3), "Reason for Encounter"),
            (0, Some(1), "Progress Note — Day 1"),
            (1, Some(1), "Progress Note — Day 2"),
        ];
        let levels = template_levels(titles.into_iter());
        assert!(levels.contains(&3));
        assert!(
            !levels.contains(&1),
            "a unique title per page is not a template"
        );
    }

    #[test]
    fn two_pages_sharing_a_title_are_not_yet_a_template() {
        let titles = [(0, Some(2), "Summary"), (5, Some(2), "Summary")];
        assert!(template_levels(titles.into_iter()).is_empty());
    }

    #[test]
    fn template_titles_do_not_count_toward_doubt() {
        let kinds = [
            ("doco:SectionTitle", "bold"),
            ("doco:Paragraph", "prose"),
            ("doco:SectionTitle", "bold"),
            ("doco:Paragraph", "prose"),
            ("doco:SectionTitle", "bold"),
            ("doco:SectionTitle", "bold"),
        ];
        assert!(
            doubt_on_page(0, &kinds).is_some(),
            "two thirds of the page is uncorroborated titles"
        );
        let template = [true, false, true, false, false, false];
        assert_eq!(
            doubt_on_page_with(0, &kinds, &template),
            None,
            "two of four titles are the template: a third of the page is left, below the threshold"
        );
        let d = doubt_on_page_with(0, &kinds, &[true, false, false, false, false, false]);
        assert!(
            d.is_some(),
            "one template title of four leaves half the page in doubt"
        );
        assert_eq!(d.unwrap().template, 1);
    }

    #[test]
    fn template_marks_are_aligned_with_the_unfiltered_kinds() {
        // The anchor is dropped before the ratio; the mark for the title
        // after it must still find that title.
        let kinds = [
            ("doco:Figure", "table-confidence"),
            ("doco:SectionTitle", "bold"),
            ("doco:Paragraph", "prose"),
            ("doco:SectionTitle", "bold"),
            ("doco:Paragraph", "prose"),
        ];
        let template = [false, true, false, true, false];
        assert_eq!(doubt_on_page_with(0, &kinds, &template), None);
    }

    #[test]
    fn a_numbered_line_repeated_across_pages_is_a_code_not_a_section() {
        // An assessment form answers every item with a coded value, set
        // larger than the body, on page after page.
        let page = |item: &str| {
            vec![
                blk(item, 7.6, 1),
                blk("03 — Partial/Moderate assistance", 9.0, 1),
                blk("Some body text follows here.", 7.6, 3),
                blk("More body text follows here.", 7.6, 3),
            ]
        };
        let pages = vec![
            page("GG0170B — Sit to lying"),
            page("GG0170C — Lying to sitting"),
            page("GG0170D — Sit to stand"),
        ];
        let hs = detect(&pages, &[]);
        assert!(
            !hs.iter().any(|h| h.text.starts_with("03 —")),
            "the repeated answer was promoted: {hs:?}"
        );
        // Two answers on one page are the same form.
        let mut one_page = page("GG0170B — Sit to lying");
        one_page.extend(page("GG0170C — Lying to sitting"));
        let hs = detect(&[one_page], &[]);
        assert!(!hs.iter().any(|h| h.text.starts_with("03 —")));
        // Once is a section.
        let hs = detect(&[page("GG0170B — Sit to lying")], &[]);
        assert!(hs.iter().any(|h| h.text.starts_with("03 —")));
    }

    #[test]
    fn too_few_elements_to_judge() {
        // One title and one paragraph is 50% headings and perfectly correct.
        let kinds: Vec<(&str, &str)> = vec![
            ("doco:SectionTitle", "font-size"),
            ("doco:Paragraph", "layout"),
        ];
        assert!(doubt(&kinds).is_none());
    }

    #[test]
    fn a_merged_documents_file_bookmarks_open_its_parts() {
        // Two files merged, the outline naming them. The second file's
        // title, under the release line its page repeats at the body's
        // size, ranks below the first file's title by its size.
        let page = |title: &str, y0: f64| {
            let mut p = vec![
                at("For release at noon", 10.0, y0, false),
                at(title, 14.0, y0 + 40.0, true),
            ];
            for k in 0..6 {
                let mut para = at(
                    "The committee decided to hold the rate.",
                    10.0,
                    y0 + 70.0 + 40.0 * k as f64,
                    false,
                );
                para.lines = vec![para.lines[0].clone(); 3];
                para.bbox.y1 = para.bbox.y0 + 34.0;
                p.push(para);
            }
            p
        };
        let mut first = vec![at("Policy Statement of the Board", 20.0, 40.0, true)];
        first.extend(page("Rate Decision", 100.0));
        // Each file runs over two pages.
        let mut more = vec![];
        for k in 0..6 {
            let mut para = at(
                "The vote was unanimous.",
                10.0,
                60.0 + 40.0 * k as f64,
                false,
            );
            para.lines = vec![para.lines[0].clone(); 3];
            para.bbox.y1 = para.bbox.y0 + 34.0;
            para.page = 1;
            more.push(para);
        }
        let mut second = page("Implementation Note", 60.0);
        for b in &mut second {
            b.page = 2;
        }
        let pages = vec![first, more, second];
        let files = vec![
            bookmark("2030-01-01.Statement.pdf", 1, Some(0), None),
            bookmark("2030-01-01.ImplementationNote.pdf", 1, Some(2), None),
        ];
        let hs = detect(&pages, &files);
        let note = hs.iter().find(|h| h.text == "Implementation Note").unwrap();
        assert_eq!((note.level, note.evidence), (1, Evidence::Outline));
        // The first file's page carries the document's title, which opens
        // the whole: its own heads keep their levels.
        let decision = hs.iter().find(|h| h.text == "Rate Decision").unwrap();
        assert_ne!(decision.evidence, Evidence::Outline);
        // An outline that prints a title is the author's.
        let mut real = files.clone();
        real.push(bookmark("Rate Decision", 2, Some(0), None));
        let hs = detect(&pages, &real);
        let note = hs.iter().find(|h| h.text == "Implementation Note").unwrap();
        assert_ne!(note.evidence, Evidence::Outline);
        // Nor one that marks every page: a deck's `Slide 1`, `Slide 2`.
        let slides = vec![
            bookmark("Slide 1", 1, Some(0), None),
            bookmark("Slide 2", 1, Some(1), None),
            bookmark("Slide 3", 1, Some(2), None),
        ];
        let hs = detect(&pages, &slides);
        let note = hs.iter().find(|h| h.text == "Implementation Note").unwrap();
        assert_ne!(note.evidence, Evidence::Outline);
    }

    fn bookmark(title: &str, level: usize, page: Option<usize>, y: Option<f64>) -> OutlineItem {
        OutlineItem {
            title: title.into(),
            level,
            page,
            y,
        }
    }

    #[test]
    fn a_repeated_bookmark_title_is_resolved_by_where_it_points() {
        // One benefits handbook carries 122 duplicated titles among 447
        // bookmarks. "Overview" under four parents is four different depths,
        // and the title alone cannot say which one a heading is.
        let entries = [
            bookmark("Overview", 2, Some(4), None),
            bookmark("Overview", 5, Some(40), None),
            bookmark("Overview", 3, Some(21), None),
        ];
        let refs: Vec<&OutlineItem> = entries.iter().collect();
        let mut b = blk("Overview", 12.0, 100);
        b.page = 20;
        assert_eq!(pick_entry(&refs, &b), Some(3), "nearest page wins");
        b.page = 3;
        assert_eq!(pick_entry(&refs, &b), Some(2));
        b.page = 39;
        assert_eq!(pick_entry(&refs, &b), Some(5));
    }

    #[test]
    fn a_stated_position_breaks_a_tie_within_a_page() {
        let entries = [
            bookmark("Overview", 2, Some(4), Some(80.0)),
            bookmark("Overview", 4, Some(4), Some(500.0)),
        ];
        let refs: Vec<&OutlineItem> = entries.iter().collect();
        let mut b = blk("Overview", 12.0, 100);
        b.page = 4;
        b.bbox.y0 = 470.0;
        assert_eq!(pick_entry(&refs, &b), Some(4));
    }

    #[test]
    fn bookmarks_that_point_nowhere_still_promote_rather_than_bury() {
        // Most files write `/Fit`, and some resolve to nothing at all. With
        // no destination to choose by, the shallowest level is the safe read.
        let entries = [
            bookmark("Overview", 4, None, None),
            bookmark("Overview", 2, None, None),
        ];
        let refs: Vec<&OutlineItem> = entries.iter().collect();
        assert_eq!(pick_entry(&refs, &blk("Overview", 12.0, 100)), Some(2));
    }
}
