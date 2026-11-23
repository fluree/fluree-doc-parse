//! Headings a document shows without declaring them.
//!
//! A statute or a contract heads its parts with a keyword and a number —
//! `CHAPTER IV`, `Article 30`, `ARTICLE II`, `ANNEX III` — set as a paragraph
//! of its own, its title often in the paragraph after it. Publishers mark
//! these in markup of their own (the Official Journal's classes, a filer's
//! centred bold), and a reader built on one publisher's markup would serve
//! that publisher alone. The shape is common to all of them, so it is the
//! shape that is read here, in the paragraphs any reader produces.
//!
//! A heading is a paragraph that is nothing but the keyword and its number,
//! or those and a short title (`§ 7001. Definitions.`, `ARTICLE 1. GENERAL
//! PROVISIONS`), or a code's section number alone (`1798.100.`). A heading
//! run into its text (`Section 2.1 Closing. Unless this Agreement…`) stays a
//! paragraph: taking it would make a heading of the clause it opens.

use std::collections::HashMap;

use crate::element::Element;

/// What a heading keyword divides a document into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A division of the body: a part, a title, a chapter.
    Division,
    /// A numbered unit of the text: an article, a section.
    Unit,
    /// Matter set after the body: an annex, a schedule, an exhibit.
    Attachment,
}

/// The keywords, lowercase, each with what it divides and the keyword it
/// is a subdivision of.
const KEYWORDS: &[(&str, Kind, Option<&str>)] = &[
    ("book", Kind::Division, None),
    ("part", Kind::Division, None),
    ("subpart", Kind::Division, Some("part")),
    ("title", Kind::Division, None),
    ("subtitle", Kind::Division, Some("title")),
    ("division", Kind::Division, None),
    ("chapter", Kind::Division, None),
    ("subchapter", Kind::Division, Some("chapter")),
    ("article", Kind::Unit, None),
    ("section", Kind::Unit, None),
    ("subsection", Kind::Unit, Some("section")),
    ("annex", Kind::Attachment, None),
    ("appendix", Kind::Attachment, None),
    ("schedule", Kind::Attachment, None),
    ("exhibit", Kind::Attachment, None),
];

fn keyword(name: &str) -> Option<(&'static str, Kind, Option<&'static str>)> {
    let name = match name {
        // `SEC. 2.` in a public law, `§ 7002` in a code.
        "sec." | "§" => "section",
        n => n,
    };
    KEYWORDS.iter().copied().find(|(k, _, _)| *k == name)
}

/// Numbers spelled out, as `PART ONE` and `TITLE II` share a document.
const NUMBER_WORDS: &[&str] = &[
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
];

/// Is this a heading's number: `30`, `1.01`, `6a`, `59.1-575`, `IV`, `C`,
/// `ONE`?
fn is_number(s: &str) -> bool {
    let s = s.strip_suffix('.').unwrap_or(s);
    if s.is_empty() {
        return false;
    }
    let arabic = s.split(['.', '-']).all(|part| {
        let digits = part.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_digit())
            && part.len() - digits.len() <= 1
    });
    let roman = s.chars().all(|c| "IVXLCDM".contains(c));
    let letter = s.len() == 1 && s.chars().all(|c| c.is_ascii_uppercase());
    let word = NUMBER_WORDS.contains(&s.to_lowercase().as_str());
    arabic || roman || letter || word
}

/// What a heading paragraph says of its own title.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Title {
    /// None: `Article 30`. The paragraph after it may be its title.
    Apart,
    /// One that stops at the line's end without a full stop, which a wrapped
    /// line may finish: `§ 7004. Requirements … and Obtaining`.
    Open,
    /// One that is whole: `§ 7001. Definitions.`
    Whole,
}

/// A paragraph read as a heading: the keyword it names and what it says of
/// its title.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Heading {
    keyword: &'static str,
    title: Title,
}

/// A code's section number set as a heading of its own: `1798.100.`, and
/// not a date (`4.5.2016`) or a paragraph's number (`1.`).
fn code_number(t: &str) -> bool {
    let Some(n) = t.strip_suffix('.') else {
        return false;
    };
    let parts: Vec<&str> = n.split('.').collect();
    parts.len() >= 2
        && parts[0].len() >= 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// The heading a paragraph is, if it is one: `CHAPTER IV`, `Article 30`,
/// `Article\u{a0}43`, `§ 7002`, `SEC. 2.`; `§ 7001. Definitions.`, `ARTICLE 1.
/// GENERAL PROVISIONS`; `1798.100.`.
///
/// A title on the heading's own line follows a number closed by a full
/// stop, is short and one sentence at most, and is in title case or
/// capitals, except after `§`, where a code's own titles are in sentence
/// case (`Scope; exemptions.`). A paragraph that goes on past that is a
/// clause that opens with its number.
fn heading(text: &str) -> Option<Heading> {
    let t = text.trim();
    if code_number(t) {
        return Some(Heading {
            keyword: "section",
            title: Title::Apart,
        });
    }
    if !t.starts_with(|c: char| c.is_uppercase() || c == '§') {
        return None;
    }
    // `§7002` sets its number against the sign.
    let (name, rest) = match t.strip_prefix('§') {
        Some(rest) => ("§".to_string(), rest.trim_start()),
        None => {
            let (name, rest) = t.split_once(char::is_whitespace)?;
            (name.to_lowercase(), rest.trim_start())
        }
    };
    let keyword = keyword(&name)?.0;
    let (number, after) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let after = after.trim();
    if !is_number(number) {
        return None;
    }
    if after.is_empty() {
        return (t.chars().count() <= 40).then_some(Heading {
            keyword,
            title: Title::Apart,
        });
    }
    let first = after
        .trim_start_matches(['"', '\'', '“', '‘', '«'])
        .chars()
        .next();
    let cased = name == "§" || title_case(after) || all_caps(after);
    let one_sentence = !after.trim_end_matches('.').contains(". ");
    if !number.ends_with('.')
        || after.split_whitespace().count() > 16
        || !first.is_some_and(|c| c.is_alphabetic() && c.is_uppercase())
        || !cased
        || !one_sentence
    {
        return None;
    }
    Some(Heading {
        keyword,
        title: if after.ends_with('.') {
            Title::Whole
        } else {
            Title::Open
        },
    })
}

/// Is this line, standing alone, a heading? A plain-text reader asks, to
/// set a heading apart from the lines under it that no blank line divides.
pub fn is_heading(line: &str) -> bool {
    heading(line).is_some()
}

/// Words a title leaves in lowercase.
const SMALL_WORDS: &[&str] = &[
    "a", "an", "and", "as", "at", "by", "for", "from", "in", "into", "of", "on", "or", "the", "to",
    "under", "with",
];

/// Is every word capitalised but the small ones?
fn title_case(text: &str) -> bool {
    text.split_whitespace().all(|w| {
        let first = w.chars().find(|c| c.is_alphabetic());
        first.is_none_or(|c| c.is_uppercase())
            || SMALL_WORDS.contains(&w.trim_matches(|c: char| !c.is_alphabetic()))
    })
}

/// Letters, and no lowercase among them.
fn all_caps(text: &str) -> bool {
    text.chars().any(|c| c.is_alphabetic()) && !text.chars().any(|c| c.is_lowercase())
}

/// Does this text stop mid-phrase, on a word that joins it to more
/// (`REPRESENTATIONS AND WARRANTIES OF`)?
fn breaks_off(text: &str) -> bool {
    let last = text.split_whitespace().last().unwrap_or("").to_lowercase();
    last == "&" || SMALL_WORDS.contains(&last.as_str())
}

/// Could this paragraph be the title of the heading before it?
///
/// A title is short, starts with a capital, and is not a sentence: it does
/// not end in a colon or a comma, and ends in a full stop only in title case
/// (`Definitions.`). What usually follows a heading with no title is a
/// numbered paragraph (`1. Each controller…`), a sentence, or the first of
/// its own units (`Section 2.01 Fees.`), and none of them is one.
fn title_like(text: &str) -> bool {
    let t = text.trim();
    let words = t.split_whitespace().count();
    if t.is_empty() || words > 24 || t.chars().count() > 250 {
        return false;
    }
    let opening: String = t.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    if heading(t).is_some() || heading(&opening).is_some() {
        return false;
    }
    let first = t
        .trim_start_matches(['"', '\'', '“', '‘', '«'])
        .chars()
        .next();
    if !first.is_some_and(|c| c.is_alphabetic() && c.is_uppercase()) {
        return false;
    }
    match t.chars().last() {
        Some(':' | ';' | ',') => false,
        Some('.') => title_case(t),
        _ => true,
    }
}

/// The level each keyword found sits at, 1 for the outermost.
///
/// A document's own nesting decides, not a fixed table: the EU's acts set
/// sections above articles and America's contracts set articles above
/// sections, and a code's parts sit under its chapters where a regulation's
/// sit over them. What holds is that the outer heading is the rarer, so
/// keywords rank by how often they occur, the earlier first where as often.
/// Divisions rank above units, and a subdivision below what it divides. An
/// attachment ranks with the outermost division, beside the body as the
/// annexes of a regulation sit beside its chapters; where the body has no
/// divisions it ranks above its units, which an attached agreement numbers
/// afresh.
fn levels(found: &[&'static str]) -> HashMap<&'static str, usize> {
    let mut seen: Vec<(&'static str, usize, usize)> = Vec::new();
    for (at, k) in found.iter().enumerate() {
        match seen.iter_mut().find(|(s, _, _)| s == k) {
            Some(s) => s.1 += 1,
            None => seen.push((k, 1, at)),
        }
    }
    let kind = |k: &str| keyword(k).map(|(_, kind, _)| kind);
    let ranked = |want: Kind| {
        let mut ks: Vec<_> = seen
            .iter()
            .filter(|(k, _, _)| kind(k) == Some(want))
            .collect();
        ks.sort_by_key(|(_, count, first)| (*count, *first));
        let mut order: Vec<&'static str> = ks.iter().map(|(k, _, _)| *k).collect();
        // A subdivision goes just below what it divides.
        for (sub, _, parent) in KEYWORDS
            .iter()
            .filter(|(_, k, p)| *k == want && p.is_some())
        {
            let parent = parent.unwrap();
            if let (Some(s), Some(_)) = (
                order.iter().position(|k| k == sub),
                order.iter().position(|k| *k == parent),
            ) {
                order.remove(s);
                let p = order.iter().position(|k| *k == parent).unwrap();
                order.insert(p + 1, sub);
            }
        }
        order
    };
    let body: Vec<&'static str> = ranked(Kind::Division)
        .into_iter()
        .chain(ranked(Kind::Unit))
        .collect();
    let attached = ranked(Kind::Attachment);
    let divided = body
        .first()
        .is_some_and(|k| kind(k) == Some(Kind::Division));
    let shift = usize::from(!attached.is_empty() && !divided);
    let mut out = HashMap::new();
    for k in attached {
        out.insert(k, 1);
    }
    for (i, k) in body.into_iter().enumerate() {
        out.insert(k, i + 1 + shift);
    }
    out
}

/// Append a title paragraph to the heading it names, its links moved along.
fn absorb(head: &mut Element, tail: Element) {
    let offset = head.text.chars().count() + 1;
    head.text.push(' ');
    head.text.push_str(&tail.text);
    if let Some(links) = tail.links {
        let moved = links.into_iter().map(|mut l| {
            l.begin = l.begin.map(|b| b + offset);
            l.end = l.end.map(|e| e + offset);
            l
        });
        head.links.get_or_insert_with(Vec::new).extend(moved);
    }
}

/// Read the headings a document shows by keyword and number as section
/// titles, each with the title the paragraph after it gives.
///
/// Only where the document declares no headings of its own: one that does
/// keeps them as declared. A heading's title is the next paragraph when it
/// reads as one (`Records of processing activities`), and a title in
/// capitals broken mid-phrase over two paragraphs (`REPRESENTATIONS AND
/// WARRANTIES OF` / `PARENT AND MERGER SUB`) is joined whole. A title that
/// is whole leaves the line under it alone, though it is in capitals too:
/// `BY AND AMONG` begins the parties. Returns whether anything
/// changed; a reader that numbers its elements renumbers them then.
pub fn infer_headings(elements: &mut Vec<Element>) -> bool {
    if elements.iter().any(|e| e.kind == "doco:SectionTitle") {
        return false;
    }
    // An attachment's label with nothing of the body before it names the
    // document, not a part of it: the exhibit it is filed as (`Exhibit
    // 10.22`), or the annex to another document that it is. The body has
    // begun at its first heading of another kind, list, table, or paragraph
    // of more than a dozen words; a filing's envelope lines are shorter.
    let mut body = false;
    let heads: Vec<Option<Heading>> = elements
        .iter()
        .map(|e| {
            let h = (e.kind == "doco:Paragraph")
                .then(|| heading(&e.text))
                .flatten();
            let attachment = h
                .and_then(|h| keyword(h.keyword))
                .is_some_and(|(_, kind, _)| kind == Kind::Attachment);
            let named = attachment && !body;
            body |= (h.is_some() && !attachment)
                || e.kind != "doco:Paragraph"
                || e.text.split_whitespace().count() > 12;
            h.filter(|_| !named)
        })
        .collect();
    let found: Vec<&'static str> = heads.iter().flatten().map(|h| h.keyword).collect();
    if found.is_empty() {
        return false;
    }
    let levels = levels(&found);
    let is_title = |e: &Option<Element>| {
        e.as_ref()
            .is_some_and(|e| e.kind == "doco:Paragraph" && title_like(&e.text))
    };
    let mut old: Vec<Option<Element>> = std::mem::take(elements).into_iter().map(Some).collect();
    for i in 0..old.len() {
        let Some(mut e) = old[i].take() else { continue };
        if let Some(h) = heads[i] {
            // A title its own line left open goes on in the next paragraph
            // when that one ends it: `§ 7004. Requirements … and Obtaining`
            // / `Consumer Consent.`
            let finishes = |n: &Option<Element>| {
                is_title(n) && n.as_ref().is_some_and(|n| n.text.ends_with('.'))
            };
            if h.title == Title::Open && old.get(i + 1).is_some_and(finishes) {
                let rest = old[i + 1].take().unwrap();
                absorb(&mut e, rest);
            }
            if h.title == Title::Apart && old.get(i + 1).is_some_and(is_title) {
                let title = old[i + 1].take().unwrap();
                let caps = all_caps(&title.text) && breaks_off(&title.text);
                let words = title.text.split_whitespace().count();
                absorb(&mut e, title);
                let goes_on = old.get(i + 2).and_then(|n| n.as_ref()).is_some_and(|n| {
                    let w = n.text.split_whitespace().count();
                    caps && is_title(&old[i + 2])
                        && all_caps(&n.text)
                        && !n.text.ends_with('.')
                        && w <= 12
                        && words + w <= 24
                });
                if goes_on {
                    let more = old[i + 2].take().unwrap();
                    absorb(&mut e, more);
                }
            }
            e.kind = "doco:SectionTitle".into();
            e.level = levels.get(h.keyword).copied();
            e.evidence = "keyword";
        }
        elements.push(e);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::Link;

    fn para(text: &str) -> Element {
        Element::new("doco:Paragraph", text, "html", "html")
    }

    fn read(texts: &[&str]) -> Vec<Element> {
        let mut els: Vec<Element> = texts.iter().map(|t| para(t)).collect();
        infer_headings(&mut els);
        els
    }

    fn outline(els: &[Element]) -> Vec<(Option<usize>, &str)> {
        els.iter()
            .map(|e| {
                (
                    (e.kind == "doco:SectionTitle").then(|| e.level.unwrap()),
                    e.text.as_str(),
                )
            })
            .collect()
    }

    #[test]
    fn a_regulation_reads_as_chapters_sections_and_articles() {
        // How the Official Journal sets the GDPR: no heading markup, each
        // heading a paragraph with its title in the next.
        let els = read(&[
            "CHAPTER IV",
            "Controller and processor",
            "Section 1",
            "General obligations",
            "Article 24",
            "Responsibility of the controller",
            "1. Taking into account the nature of processing, the controller shall act.",
            "Article 25",
            "Data protection by design and by default",
            "For the purposes of this Regulation:",
            "Section 2",
            "Security of personal data",
            "Article\u{a0}32",
            "Security of processing",
            "Article 33",
            "This Regulation shall enter into force.",
        ]);
        assert_eq!(
            outline(&els),
            [
                (Some(1), "CHAPTER IV Controller and processor"),
                (Some(2), "Section 1 General obligations"),
                (Some(3), "Article 24 Responsibility of the controller"),
                (
                    None,
                    "1. Taking into account the nature of processing, the controller shall act."
                ),
                (
                    Some(3),
                    "Article 25 Data protection by design and by default"
                ),
                (None, "For the purposes of this Regulation:"),
                (Some(2), "Section 2 Security of personal data"),
                (Some(3), "Article\u{a0}32 Security of processing"),
                // A sentence is not a title.
                (Some(3), "Article 33"),
                (None, "This Regulation shall enter into force."),
            ]
        );
        assert!(els
            .iter()
            .filter(|e| e.kind == "doco:SectionTitle")
            .all(|e| e.evidence == "keyword"));
    }

    #[test]
    fn a_contract_reads_as_articles_inside_its_exhibits() {
        let els = read(&[
            "ARTICLE I",
            "DEFINITIONS",
            "Section 1.1 Definitions. For purposes of this Agreement, terms have these meanings.",
            "ARTICLE II",
            "REPRESENTATIONS AND WARRANTIES OF",
            "PARENT AND MERGER SUB",
            "Subject to the exceptions set forth in the Disclosure Schedule, Parent represents:",
            "Exhibit C",
            "AGENCY AGREEMENT",
            "BY AND AMONG",
            "ARTICLE 1",
            "APPOINTMENT",
            "ARTICLE 2",
            "Section 2.01 Fees.",
        ]);
        assert_eq!(
            outline(&els),
            [
                (Some(2), "ARTICLE I DEFINITIONS"),
                // A heading run into its clause stays the clause.
                (
                    None,
                    "Section 1.1 Definitions. For purposes of this Agreement, terms have these meanings."
                ),
                (
                    Some(2),
                    "ARTICLE II REPRESENTATIONS AND WARRANTIES OF PARENT AND MERGER SUB"
                ),
                (
                    None,
                    "Subject to the exceptions set forth in the Disclosure Schedule, Parent represents:"
                ),
                // An attached agreement numbers its articles afresh, inside it.
                (Some(1), "Exhibit C AGENCY AGREEMENT"),
                (None, "BY AND AMONG"),
                (Some(2), "ARTICLE 1 APPOINTMENT"),
                (Some(2), "ARTICLE 2"),
                (None, "Section 2.01 Fees."),
            ]
        );
    }

    #[test]
    fn a_label_naming_the_document_is_not_one_of_its_parts() {
        // An EDGAR exhibit file: the filing's envelope, then the exhibit
        // number the document is filed under, then the document.
        let els = read(&[
            "EX-10.1",
            "ex101.htm",
            "Exhibit 10.1",
            "EXECUTION COPY",
            "AGENCY AGREEMENT",
            "This Agreement is made between the Company and the Agent on the date below.",
            "ARTICLE I",
            "DEFINITIONS",
            "Schedule A",
            "Covered lines of business",
        ]);
        assert_eq!(
            outline(&els)[2..],
            [
                (None, "Exhibit 10.1"),
                (None, "EXECUTION COPY"),
                (None, "AGENCY AGREEMENT"),
                (
                    None,
                    "This Agreement is made between the Company and the Agent on the date below."
                ),
                (Some(2), "ARTICLE I DEFINITIONS"),
                (Some(1), "Schedule A Covered lines of business"),
            ]
        );
    }

    #[test]
    fn annexes_sit_beside_the_chapters() {
        let els = read(&[
            "CHAPTER I",
            "GENERAL PROVISIONS",
            "Article 1",
            "Subject matter",
            "Article 2",
            "Scope",
            "ANNEX I",
            "List of Union harmonisation legislation",
            "Section 1",
            "Information to be provided",
        ]);
        assert_eq!(
            outline(&els),
            [
                (Some(1), "CHAPTER I GENERAL PROVISIONS"),
                (Some(3), "Article 1 Subject matter"),
                (Some(3), "Article 2 Scope"),
                (Some(1), "ANNEX I List of Union harmonisation legislation"),
                (Some(2), "Section 1 Information to be provided"),
            ]
        );
    }

    #[test]
    fn a_code_sets_its_parts_under_its_chapters_by_how_often_they_occur() {
        let els = read(&[
            "TITLE 5",
            "CHAPTER 1",
            "PART A",
            "§ 101",
            "§ 102",
            "PART B",
            "§ 103",
            "CHAPTER 2",
            "PART A",
            "§7002",
            "SEC. 3.",
        ]);
        let levels: Vec<Option<usize>> = els.iter().map(|e| e.level).collect();
        assert_eq!(
            levels,
            [
                Some(1),
                Some(2),
                Some(3),
                Some(4),
                Some(4),
                Some(3),
                Some(4),
                Some(2),
                Some(3),
                Some(4),
                Some(4)
            ]
        );
    }

    #[test]
    fn only_a_heading_and_at_most_its_title_is_one() {
        for not in [
            "See Article 30.",
            "Article 30 applies.",
            "article 30",
            "Article thirty-one",
            "Chapter",
            "Section 1.1 Definitions. For purposes of this Agreement.",
            "Item 7",
            "Article (a)",
            // A clause that opens with its number, in sentence case.
            "Section 3. The parties agree to the terms set forth herein.",
            // Two sentences: a heading run into its text.
            "Section 2.1. Closing. Unless this Agreement terminates, it closes.",
            "§ 7002. Definitions. A business shall comply.",
            // A date, and a paragraph's number.
            "4.5.2016",
            "1.",
            "12.5.",
        ] {
            assert_eq!(heading(not), None, "{not}");
        }
        use Title::*;
        for (yes, k, title) in [
            ("CHAPTER IV", "chapter", Apart),
            ("Article\u{a0}43", "article", Apart),
            ("ARTICLE I.", "article", Apart),
            ("Section 4.2", "section", Apart),
            ("§ 7002", "section", Apart),
            ("SEC. 2.", "section", Apart),
            ("Article 6a", "article", Apart),
            ("PART ONE", "part", Apart),
            ("Exhibit 10.22", "exhibit", Apart),
            ("Schedule B", "schedule", Apart),
            ("1798.100.", "section", Apart),
            ("§ 7001. Definitions.", "section", Whole),
            ("§ 59.1-576. Scope; exemptions.", "section", Whole),
            ("ARTICLE 1. GENERAL PROVISIONS", "article", Open),
            (
                "DIVISION 6. CALIFORNIA PRIVACY PROTECTION AGENCY",
                "division",
                Open,
            ),
            (
                "TITLE 1.81.5. California Consumer Privacy Act of 2018 [1798.100 - 1798.199.100]",
                "title",
                Open,
            ),
        ] {
            assert_eq!(heading(yes), Some(Heading { keyword: k, title }), "{yes}");
        }
    }

    #[test]
    fn a_code_reads_with_its_titles_on_their_own_lines() {
        // How a state's regulations are set: each heading with its title on
        // its line, one wrapped onto the next.
        let els = read(&[
            "TITLE 11. LAW",
            "DIVISION 6. CALIFORNIA PRIVACY PROTECTION AGENCY",
            "CHAPTER 1. CALIFORNIA CONSUMER PRIVACY ACT REGULATIONS",
            "ARTICLE 1. GENERAL PROVISIONS",
            "§ 7001. Definitions.",
            "In addition to the definitions set forth in the Civil Code, for purposes of these regulations:",
            "§ 7004. Requirements for Methods for Submitting CCPA Requests and Obtaining",
            "Consumer Consent.",
            "Except as expressly allowed by the CCPA and these regulations, businesses shall comply.",
            "ARTICLE 2. REQUIRED DISCLOSURES TO CONSUMERS",
            "§ 7010. Overview of Required Disclosures.",
            "Summary Of Changes",
        ]);
        assert_eq!(
            outline(&els),
            [
                (Some(1), "TITLE 11. LAW"),
                (Some(2), "DIVISION 6. CALIFORNIA PRIVACY PROTECTION AGENCY"),
                (Some(3), "CHAPTER 1. CALIFORNIA CONSUMER PRIVACY ACT REGULATIONS"),
                (Some(4), "ARTICLE 1. GENERAL PROVISIONS"),
                (Some(5), "§ 7001. Definitions."),
                (
                    None,
                    "In addition to the definitions set forth in the Civil Code, for purposes of these regulations:"
                ),
                (
                    Some(5),
                    "§ 7004. Requirements for Methods for Submitting CCPA Requests and Obtaining Consumer Consent."
                ),
                (
                    None,
                    "Except as expressly allowed by the CCPA and these regulations, businesses shall comply."
                ),
                (Some(4), "ARTICLE 2. REQUIRED DISCLOSURES TO CONSUMERS"),
                // A whole title takes nothing from the paragraph after it.
                (Some(5), "§ 7010. Overview of Required Disclosures."),
                (None, "Summary Of Changes"),
            ]
        );
    }

    #[test]
    fn a_code_section_number_alone_heads_its_section() {
        let els = read(&[
            "PART 4. OBLIGATIONS ARISING FROM PARTICULAR TRANSACTIONS [1738 - 3273.91]",
            "( Part 4 enacted 1872. )",
            "1798.100.",
            "General Duties of Businesses that Collect Personal Information",
            "(a) A business that controls the collection of personal information shall inform consumers.",
            "1798.105.",
            "Consumers’ Right to Delete Personal Information",
        ]);
        assert_eq!(
            outline(&els),
            [
                (
                    Some(1),
                    "PART 4. OBLIGATIONS ARISING FROM PARTICULAR TRANSACTIONS [1738 - 3273.91]"
                ),
                (None, "( Part 4 enacted 1872. )"),
                (
                    Some(2),
                    "1798.100. General Duties of Businesses that Collect Personal Information"
                ),
                (
                    None,
                    "(a) A business that controls the collection of personal information shall inform consumers."
                ),
                (
                    Some(2),
                    "1798.105. Consumers’ Right to Delete Personal Information"
                ),
            ]
        );
    }

    #[test]
    fn a_document_that_declares_its_headings_keeps_them() {
        let mut els = vec![
            Element {
                level: Some(1),
                ..Element::new("doco:SectionTitle", "Terms", "html", "html")
            },
            para("Article 1"),
            para("Scope"),
        ];
        assert!(!infer_headings(&mut els));
        assert_eq!(els.len(), 3);
        assert_eq!(els[1].kind, "doco:Paragraph");
    }

    #[test]
    fn a_title_brings_its_links_along() {
        let mut title = para("Records of processing");
        title.links = Some(vec![Link::uri("https://example.org/r").spanning(11, 21)]);
        let mut els = vec![para("Article 30"), title];
        infer_headings(&mut els);
        let l = &els[0].links.as_ref().unwrap()[0];
        let (b, e) = l.span().unwrap();
        let anchor: String = els[0].text.chars().skip(b).take(e - b).collect();
        assert_eq!(anchor, "processing");
    }
}
