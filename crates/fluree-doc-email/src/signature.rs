//! Where a message's signature starts.
//!
//! A signature is the sender's: the sign-off, their name, title and
//! company, phones, office address, and the legal footer their server adds.
//! Read as body, an address in it is just a place the message mentions, and
//! a quoted signature puts one company's office on whoever else the thread
//! talks about. Marked, it can be kept on its sender.
//!
//! A signature is found from the end of the message, by what opens one:
//!
//! - the `-- ` line that sets a signature apart by convention (RFC 3676);
//! - a sign-off (`Thanks,`, `Best regards`, `Mit freundlichen Grüßen`)
//!   with a name under it, or on its line (`Thanks, Jo`);
//! - the sender's own name on a line of its own, or their first name with
//!   their surname;
//! - a name after a dash, `-Brian`.
//!
//! The lowest such line is the signature, or part of it, and the signature
//! runs up from it over blank lines, rules and more of the same, so `Best,`
//! over `Jennifer` over a blank line over `Jennifer Wilken` is one. It
//! stops at anything else, which is the body. Down from it, the signature
//! runs to the message's end, or to a paragraph of prose: terms pasted
//! under a sign-off, or a calendar invitation's notice, are not the
//! sender's signature, though a legal footer after them is. After `-- `,
//! everything is. A message with none of them but a legal footer at its end
//! has that footer for a signature.

use fluree_doc_model::Mailbox;
use std::ops::Range;

/// Sign-offs, lower case, in the languages the thread splitter reads and in
/// Portuguese and Italian.
const SIGN_OFFS: &[&str] = &[
    "thanks",
    "thank you",
    "thanks so much",
    "thank you so much",
    "thanks again",
    "thanks all",
    "thanks everyone",
    "many thanks",
    "with thanks",
    "thx",
    "tks",
    "rgds",
    "best rgds",
    "kind rgds",
    "br",
    "best",
    "very best",
    "all the best",
    "best regards",
    "best wishes",
    "kind regards",
    "kindest regards",
    "warm regards",
    "warmest regards",
    "regards",
    "cheers",
    "sincerely",
    "sincerely yours",
    "yours",
    "yours truly",
    "yours sincerely",
    "respectfully",
    "cordially",
    "warmly",
    "talk soon",
    "speak soon",
    "take care",
    "mit freundlichen grüßen",
    "mit freundlichen grüssen",
    "viele grüße",
    "beste grüße",
    "liebe grüße",
    "freundliche grüße",
    "gruß",
    "cordialement",
    "bien cordialement",
    "bien à vous",
    "salutations",
    "merci",
    "saludos",
    "un saludo",
    "saludos cordiales",
    "atentamente",
    "gracias",
    "met vriendelijke groet",
    "met vriendelijke groeten",
    "vriendelijke groet",
    "groeten",
    "com os melhores cumprimentos",
    "cumprimentos",
    "atenciosamente",
    "obrigado",
    "obrigada",
    "cordiali saluti",
    "distinti saluti",
    "saluti",
    "grazie",
];

/// Words in a legal footer.
const LEGAL: &[&str] = &[
    "confidential",
    "privileged",
    "intended recipient",
    "intended only for",
    "intended solely",
    "intended to be received",
    "sole use",
    "if you have received this",
    "if you received this",
    "unauthorized",
    "unauthorised",
    "disclaimer",
    "virus",
    "notify the sender",
    "message was secured",
];

/// The lines of `lines`, the body of a message from `sender`, that are its
/// signature, in order: the signature, and a legal footer set apart from it
/// by prose. `resumed` is set where the lines go on with a message after a
/// quote nested in it, rather than open it.
pub fn find(lines: &[String], sender: &[Mailbox], resumed: bool) -> Vec<Range<usize>> {
    let lines: Vec<String> = lines.iter().map(|l| crate::text::visible(l)).collect();
    let n = lines.len();
    let footer = |from: usize| legal_footer(&lines[from..]).map(|i| from + i..n);
    let names = name_tokens(sender);
    let opens = |i: usize| opens_signature(&lines, i, &names);
    let Some(lowest) = (0..n).rev().find(|&i| opens(i)) else {
        return footer(0).into_iter().collect();
    };
    let mut from = lowest;
    for i in (0..lowest).rev() {
        if lines[i].is_empty() || is_rule(&lines[i]) {
            continue;
        }
        if !opens(i) {
            break;
        }
        from = i;
    }
    // A message does not open with the sender's name as its signature,
    // unless it is little else: a notification's first line naming the
    // sender is its masthead. What goes on after a quote, and `-- `, may
    // open with one.
    let first = lines.iter().position(|l| !l.is_empty());
    let after = lines[from..].iter().filter(|l| !l.is_empty()).count();
    let named = !is_sign_off(&lines[from]) && lines[from] != "--";
    if !resumed && Some(from) == first && named && after > 4 {
        return footer(0).into_iter().collect();
    }
    let dashed = lines[from..=lowest].iter().any(|l| l == "--");
    let to = match prose_after(&lines, lowest + 1) {
        Some(p) if !dashed => p,
        _ => return std::iter::once(from..n).collect(),
    };
    std::iter::once(from..to).chain(footer(to)).collect()
}

/// Where the first paragraph of prose at or after line `from` starts: more
/// words than a signature's lines run to, and not a legal notice.
fn prose_after(lines: &[String], from: usize) -> Option<usize> {
    let mut i = from;
    while i < lines.len() {
        if lines[i].is_empty() {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && !lines[i].is_empty() {
            i += 1;
        }
        let text = lines[start..i].join(" ");
        // Links and addresses are one token however long they are.
        let words = text
            .split_whitespace()
            .filter(|w| !w.contains("://") && !w.contains('@') && !w.starts_with('<'))
            .count();
        if words > 25 && !is_legal(&text) {
            return Some(start);
        }
    }
    None
}

fn is_legal(text: &str) -> bool {
    let t = text.to_lowercase();
    LEGAL.iter().any(|w| t.contains(w))
}

/// Does line `i` open a signature, or go on with one?
fn opens_signature(lines: &[String], i: usize, names: &[String]) -> bool {
    let line = lines[i].as_str();
    if line == "--" || names_sender(line, names) || dashed_name(line, names) {
        return true;
    }
    // `Thanks, Jo` on one line.
    if let Some((before, after)) = line.split_once([',', '-', '–', '—']) {
        if is_sign_off(before) && (name_like(after) || names_sender(after, names)) {
            return true;
        }
    }
    // A sign-off is one when a name, or nothing, comes after it: `Thanks,`
    // above a paragraph is that paragraph's first word.
    is_sign_off(line)
        && lines[i + 1..]
            .iter()
            .filter(|l| !l.is_empty())
            .take(1)
            .all(|l| name_like(l) || names_sender(l, names) || dashed_name(l, names))
}

/// `Best regards,`, and `Com os melhores cumprimentos / Best regards,` in
/// two languages at once.
fn is_sign_off(line: &str) -> bool {
    line.split(['/', '|']).any(|part| {
        let t = part
            .trim()
            .trim_end_matches([',', '.', '!', ':', '-', '–', '—'])
            .trim()
            .to_lowercase();
        let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
        SIGN_OFFS.contains(&t.as_str())
    })
}

/// The words a sender's name and mailbox are made of, lower case: `Holoweiko,
/// Seth E <seth.holoweiko@example.com>` gives `holoweiko`, `seth`, `e`.
fn name_tokens(sender: &[Mailbox]) -> Vec<String> {
    let words = |s: &str, min: usize| -> Vec<String> {
        s.split(|c: char| !c.is_alphabetic())
            .filter(|w| w.chars().count() >= min)
            .map(str::to_lowercase)
            .collect()
    };
    let mut out = Vec::new();
    for m in sender {
        if let Some(n) = &m.name {
            out.extend(words(n, 1));
        }
        if let Some(local) = m.address.as_deref().and_then(|a| a.split('@').next()) {
            out.extend(words(local, 2));
        }
    }
    out
}

/// The sender's name on a line of its own: all of its words from the
/// sender's name or mailbox, or a name that starts with one, as `Anne
/// Fulton` does for `anne@example.com`.
fn names_sender(line: &str, names: &[String]) -> bool {
    if names.is_empty() {
        return false;
    }
    let t = line.trim().trim_end_matches([',', '.']);
    if !t.chars().all(|c| {
        c.is_alphabetic() || c.is_whitespace() || matches!(c, '.' | '-' | '\'' | '’' | ',')
    }) {
        return false;
    }
    let words: Vec<String> = t
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(|w| w.trim_matches(|c: char| !c.is_alphabetic()).to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() || words.len() > 4 {
        return false;
    }
    // A first name with a surname the mailbox does not give, `Anne Fulton`
    // from `anne@example.com`. `The` from `The Daily Brief` is no first name.
    const NOT_NAMES: &[&str] = &[
        "the", "a", "an", "and", "of", "for", "from", "team", "your", "our", "news", "info",
        "support", "hello", "reply", "noreply",
    ];
    words.iter().all(|w| names.contains(w))
        || (names.contains(&words[0])
            && !NOT_NAMES.contains(&words[0].as_str())
            && words.len() <= 3
            && name_like(t))
}

/// `-Brian`, `— Kai Moreno`: a name signed after a dash. A hyphen with a
/// space after it is a list item, and not one.
fn dashed_name(line: &str, names: &[String]) -> bool {
    let rest = if let Some(r) = line.strip_prefix(['–', '—', '~']) {
        r.trim_start()
    } else if let Some(r) = line.strip_prefix('-') {
        if r.starts_with(char::is_whitespace) {
            return false;
        }
        r
    } else {
        return false;
    };
    name_like(rest) || names_sender(rest, names)
}

/// One to four words a name could be: letters only, capitalised.
fn name_like(text: &str) -> bool {
    let t = text.trim().trim_end_matches([',', '.']);
    let words: Vec<&str> = t.split_whitespace().collect();
    (1..=4).contains(&words.len())
        && words.iter().all(|w| {
            w.chars()
                .all(|c| c.is_alphabetic() || matches!(c, '.' | '-' | '\'' | '’'))
        })
        && words
            .iter()
            .all(|w| w.chars().next().is_some_and(char::is_uppercase))
}

/// A rule drawn in characters: `____`, `-----`, `====`.
fn is_rule(line: &str) -> bool {
    line.chars().count() >= 4
        && line
            .chars()
            .all(|c| matches!(c, '_' | '-' | '=' | '*' | '~' | '─' | '━'))
}

/// The paragraphs of legal text a message ends with, by where they start.
fn legal_footer(lines: &[String]) -> Option<usize> {
    let mut from = None;
    let mut end = lines.len();
    loop {
        while end > 0 && lines[end - 1].is_empty() {
            end -= 1;
        }
        if end == 0 {
            break;
        }
        let mut begin = end;
        while begin > 0 && !lines[begin - 1].is_empty() {
            begin -= 1;
        }
        if !is_legal(&lines[begin..end].join(" ")) {
            break;
        }
        from = Some(begin);
        end = begin;
    }
    from
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sender(name: &str, address: &str) -> Vec<Mailbox> {
        vec![Mailbox {
            name: (!name.is_empty()).then(|| name.to_string()),
            address: Some(address.to_string()),
        }]
    }

    /// The signature's stretches, each as its lines.
    fn at(body: &str, from: &[Mailbox]) -> Vec<String> {
        let lines: Vec<String> = body.lines().map(String::from).collect();
        find(&lines, from, false)
            .into_iter()
            .map(|r| lines[r].join("\n").trim_end().to_string())
            .collect()
    }

    #[test]
    fn a_sign_off_over_a_name_over_the_full_block_is_one_signature() {
        let lena = sender("Holt, Lena", "lena.holt@example.com");
        assert_eq!(
            at(
                "Hi Kai,\n\nThe credit is fair.\n\nBest,\nLena\n\nLena Holt\nFinance Director\nExample Corp\n12 Harbor Street, Suite 400\n+1 555 010 2000",
                &lena
            ),
            ["Best,\nLena\n\nLena Holt\nFinance Director\nExample Corp\n12 Harbor Street, Suite 400\n+1 555 010 2000"]
        );
        // The sign-off on a line of its own after the last sentence.
        assert_eq!(
            at("Approved, see you then.\nThanks\nLena", &lena),
            ["Thanks\nLena"]
        );
    }

    #[test]
    fn a_name_alone_a_dash_and_a_rule_open_one() {
        let kai = sender("Kai Moreno", "kai@example.com");
        assert_eq!(at("Can we meet Tuesday?\n\n-Kai", &kai), ["-Kai"]);
        assert_eq!(
            at(
                "See you there.\n\nKai\n_______________\nKai Moreno\nExample Ltd",
                &kai
            ),
            ["Kai\n_______________\nKai Moreno\nExample Ltd"]
        );
        // Two languages on one line.
        assert_eq!(
            at(
                "Segue em anexo.\n\nCom os melhores cumprimentos / Best regards,\nKai Moreno",
                &kai
            ),
            ["Com os melhores cumprimentos / Best regards,\nKai Moreno"]
        );
        // A first name the mailbox gives, with a surname it does not.
        let ben = sender("", "ben@example.com");
        assert_eq!(
            at("Done.\n\nRegards\nBen Ortiz\n+1 555 010 3000", &ben),
            ["Regards\nBen Ortiz\n+1 555 010 3000"]
        );
    }

    #[test]
    fn after_the_dashes_everything_is_signature() {
        let kai = sender("Kai Moreno", "kai@example.com");
        let note = "Out of office until the twenty-first of the month, with no access to email or phone, so responses to anything sent before then will be slow.";
        assert_eq!(
            at(
                &format!("Here it is.\n-- \nKai\n\n{note}\nExample Ltd"),
                &kai
            ),
            [format!("-- \nKai\n\n{note}\nExample Ltd")]
        );
    }

    #[test]
    fn prose_under_a_sign_off_is_not_signature_but_a_footer_after_it_is() {
        let kai = sender("Kai Moreno", "kai@example.com");
        let terms = "Terms below, as discussed: ten percent simple interest, an eighteen month maturity, a twenty percent conversion discount, and no valuation cap on any of the notes issued.";
        assert_eq!(
            at(
                &format!("See the terms.\n\n-Kai\n\n{terms}\n\nThis email is confidential and for the intended recipient only."),
                &kai
            ),
            [
                "-Kai".to_string(),
                "This email is confidential and for the intended recipient only.".to_string()
            ]
        );
    }

    #[test]
    fn a_body_that_only_resembles_one_is_body() {
        let kai = sender("Kai Moreno", "kai@example.com");
        // `Thanks,` opening a paragraph, and a list item after a hyphen.
        assert!(at("Thanks,\nthe draft reads well and I signed it.", &kai).is_empty());
        assert!(at("Next steps:\n- Book Room", &kai).is_empty());
        // A sign-off high in the message is not the signature when prose
        // follows: the name at the end is.
        assert_eq!(
            at(
                "Thanks Lena.\n\nI will send the agreement tomorrow with the board's comments on it.\n\nKai",
                &kai
            ),
            ["Kai"]
        );
        // A sender named like a publication does not make its headlines a
        // name, and a notification's masthead is not its signature.
        let brief = sender("The Daily Brief", "brief@example.com");
        assert!(at("Top stories\n\nThe Daily Markets\nShares rose.", &brief).is_empty());
        let app = sender("Scheduler", "notify@example.com");
        assert!(at(
            "Scheduler\nHi Kai,\nA new event is booked.\nType: 30 minutes\nInvitee: Lena Holt\nWhen: Tuesday",
            &app
        )
        .is_empty());
        // But a message of a sign-off and a name is all signature, and so
        // is a message's text after its quote, or after `-- `.
        assert_eq!(at("Thanks,\nKai", &kai), ["Thanks,\nKai"]);
        let block =
            "Kai Moreno\nDirector\nExample Ltd\n1 Main Street\nSpringfield\n+1 555 010 1000";
        let lines: Vec<String> = block.lines().map(String::from).collect();
        assert_eq!(find(&lines, &kai, true), [Range { start: 0, end: 6 }]);
        assert_eq!(at(&format!("-- \n{block}"), &kai).len(), 1);
    }

    #[test]
    fn a_footer_alone_is_the_signature() {
        let kai = sender("Kai Moreno", "kai@example.com");
        assert_eq!(
            at(
                "Sounds good to me, go ahead with it.\n\nThis email is confidential and intended for the recipient only.\nIf you received this in error, notify the sender.",
                &kai
            ),
            ["This email is confidential and intended for the recipient only.\nIf you received this in error, notify the sender."]
        );
    }
}
