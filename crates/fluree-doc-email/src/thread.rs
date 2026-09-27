//! Splitting a reply's body into the messages it quotes.
//!
//! Mail clients quote in two ways. Gmail, Apple Mail and most others write
//! an attribution line, `On Fri, Jul 17, 2026 at 10:05 AM Ada Park
//! <ada@example.com> wrote:`, and prefix every quoted line with `>`, nesting
//! a level for each reply further back. Outlook writes the earlier
//! message's header as a block of `From:`, `Sent:`, `To:` and `Subject:`
//! lines and leaves the text under it unmarked, so the thread is a flat
//! run of header blocks. Both are read here, in English and in the German,
//! French, Spanish and Dutch forms the same clients write.

use crate::{address, date};
use fluree_doc_model::{Element, Message};

/// A stretch of a body belonging to one message.
pub struct Segment {
    /// The header the segment opens with, as the body writes it and as
    /// read. `None` for the file's own text, whose header is the file's.
    pub header: Option<(String, Message)>,
    pub lines: Vec<String>,
}

/// Split a plain-text body into its messages, in the order they are read.
pub fn split(text: &str) -> Vec<Segment> {
    let lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).collect();
    let mut out = Vec::new();
    walk(&lines, None, &mut out);
    out
}

fn walk(lines: &[String], header: Option<(String, Message)>, out: &mut Vec<Segment>) {
    let mut cur = Segment {
        header,
        lines: Vec::new(),
    };
    let mut i = 0;
    while i < lines.len() {
        if let Some((n, text, msg)) = attribution(lines, i) {
            let from = (i + n..lines.len())
                .find(|&j| !lines[j].trim().is_empty())
                .unwrap_or(lines.len());
            let to = quote_end(lines, from);
            push(&mut cur, out);
            if to > from {
                let inner: Vec<String> = lines[from..to].iter().map(|l| unquote(l)).collect();
                walk(&inner, Some((text, msg)), out);
                i = to;
            } else {
                // Some clients put the quoted text under an attribution
                // unmarked; then everything after it is that message.
                cur.header = Some((text, msg));
                i += n;
            }
            continue;
        }
        let at_start = i == 0 || lines[i - 1].trim().is_empty();
        if let Some((n, text, msg)) = at_start.then(|| header_block(lines, i)).flatten() {
            push(&mut cur, out);
            cur.header = Some((text, msg));
            i += n;
            continue;
        }
        cur.lines.push(lines[i].clone());
        i += 1;
    }
    push(&mut cur, out);
}

fn push(cur: &mut Segment, out: &mut Vec<Segment>) {
    let seg = std::mem::replace(
        cur,
        Segment {
            header: None,
            lines: Vec::new(),
        },
    );
    if seg.header.is_some() || seg.lines.iter().any(|l| !l.trim().is_empty()) {
        out.push(seg);
    }
}

/// Where a block of `>` lines starting at `from` ends. Blank lines inside
/// it belong to it when more quoted lines follow.
fn quote_end(lines: &[String], from: usize) -> usize {
    let (mut j, mut end) = (from, from);
    while j < lines.len() {
        if lines[j].starts_with('>') {
            j += 1;
            end = j;
        } else if lines[j].trim().is_empty() {
            j += 1;
        } else {
            break;
        }
    }
    end
}

/// A quoted line one level out: `> > text` is `> text`.
fn unquote(line: &str) -> String {
    let rest = line.strip_prefix('>').unwrap_or(line);
    rest.strip_prefix(' ').unwrap_or(rest).to_string()
}

/// An attribution line at `at`, possibly wrapped over up to three lines:
/// the lines it spans, its text, and the message it describes.
fn attribution(lines: &[String], at: usize) -> Option<(usize, String, Message)> {
    let first = lines[at].trim();
    if first.is_empty() || first.starts_with('>') {
        return None;
    }
    let mut text = String::new();
    for n in 1..=3 {
        let line = lines.get(at + n - 1)?.trim();
        if line.is_empty() || (n > 1 && line.starts_with('>')) {
            return None;
        }
        if n > 1 {
            text.push(' ');
        }
        text.push_str(line);
        if let Some(msg) = attribution_text(&text) {
            let written = lines[at..at + n]
                .iter()
                .map(|l| l.trim())
                .collect::<Vec<_>>()
                .join("\n");
            return Some((n, written, msg));
        }
    }
    None
}

/// The message an attribution line describes, if `text` is one.
pub fn attribution_text(text: &str) -> Option<Message> {
    let t = text.trim();
    if t.chars().count() > 400 {
        return None;
    }
    // (opening, closing): the date and sender are between.
    const FORMS: &[(&str, &str)] = &[
        ("On ", "wrote:"),
        ("Le ", "a écrit :"),
        ("Le ", "a écrit:"),
        ("El ", "escribió:"),
    ];
    for (open, close) in FORMS {
        if let Some(inner) = t.strip_prefix(open).and_then(|r| r.strip_suffix(close)) {
            return date_then_sender(inner);
        }
    }
    // `Am 17.07.2026 um 10:05 schrieb Ada Park <ada@example.com>:`, and the
    // Dutch `Op … schreef …:`, put the verb between the date and the sender.
    for (open, verb) in [("Am ", " schrieb "), ("Op ", " schreef ")] {
        if let Some(inner) = t.strip_prefix(open).and_then(|r| r.strip_suffix(':')) {
            let (when, who) = inner.split_once(verb)?;
            return described(date::loose(when), who, when);
        }
    }
    None
}

/// Split `Fri, Jul 17, 2026 at 10:05 AM Ada Park <ada@example.com>` into its
/// date and its sender. The date ends with its time, or its year when no
/// time is written; whatever follows is who.
fn date_then_sender(inner: &str) -> Option<Message> {
    let inner = inner.trim().trim_end_matches(',');
    let tokens: Vec<(usize, &str)> = inner
        .split_whitespace()
        .map(|t| (t.as_ptr() as usize - inner.as_ptr() as usize, t))
        .collect();
    let ends = |i: usize| tokens[i].0 + tokens[i].1.len();
    let mut cut = None;
    for (i, (_, t)) in tokens.iter().enumerate() {
        let bare = t.trim_end_matches(',');
        if bare.contains(':') && bare.split(':').all(|p| p.parse::<u32>().is_ok()) {
            let mut end = i;
            let ampm = |s: &str| {
                matches!(
                    s.trim_end_matches(',')
                        .to_lowercase()
                        .replace('.', "")
                        .as_str(),
                    "am" | "pm"
                )
            };
            if tokens.get(i + 1).is_some_and(|(_, n)| ampm(n)) {
                end = i + 1;
            }
            cut = Some(ends(end));
            break;
        }
    }
    if cut.is_none() {
        cut = tokens
            .iter()
            .position(|(_, t)| {
                let t = t.trim_end_matches(',');
                t.len() == 4 && t.parse::<u32>().is_ok_and(|y| (1900..3000).contains(&y))
            })
            .map(ends);
    }
    let cut = cut?;
    let (when, who) = inner.split_at(cut);
    described(date::loose(when), who, when)
}

/// A quoted message from its sender's text and its date, provided the
/// attribution really carried a date: `On Monday the board wrote:` is a
/// sentence.
fn described(when: Option<String>, who: &str, when_text: &str) -> Option<Message> {
    if when.is_none() && !when_text.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let who = who
        .trim()
        .trim_start_matches(',')
        .trim()
        .trim_end_matches(',')
        .replace("(<", "<")
        .replace(">)", ">");
    let from = address::one(&who)?;
    Some(Message {
        from: vec![from],
        date: when,
        quoted: true,
        ..Default::default()
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Key {
    From,
    Date,
    To,
    Cc,
    Bcc,
    Subject,
    Other,
}

/// The field a quoted header line names, in the languages Outlook writes.
fn key(name: &str) -> Option<Key> {
    let n = name.trim().trim_matches('*').trim().to_lowercase();
    Some(match n.as_str() {
        "from" | "von" | "de" | "van" => Key::From,
        "sent" | "date" | "gesendet" | "datum" | "envoyé" | "enviado" | "fecha" | "verzonden" => {
            Key::Date
        }
        "to" | "an" | "à" | "para" | "aan" => Key::To,
        "cc" => Key::Cc,
        "bcc" | "cci" | "cco" => Key::Bcc,
        "subject" | "betreff" | "objet" | "asunto" | "onderwerp" => Key::Subject,
        "reply-to" | "importance" | "attachments" | "anlagen" | "wichtigkeit" => Key::Other,
        _ => return None,
    })
}

/// `From: Ada Park` as (key, value); `*From:* …` from a bold header too.
fn header_line(line: &str) -> Option<(Key, String)> {
    let (name, value) = line.split_once(':')?;
    if name.chars().count() > 20 {
        return None;
    }
    let value = value.trim().trim_start_matches('*').trim();
    Some((key(name)?, value.replace('*', "")))
}

/// Lines that announce a quoted or forwarded message.
fn is_separator(line: &str) -> bool {
    let t = line.trim();
    if t.chars().count() >= 10 && t.chars().all(|c| c == '_') {
        return true;
    }
    let inner = t
        .trim_matches(|c: char| c == '-' || c.is_whitespace())
        .to_lowercase();
    (t.starts_with("---") || t.ends_with(':'))
        && [
            "original message",
            "forwarded message",
            "begin forwarded message:",
            "ursprüngliche nachricht",
            "weitergeleitete nachricht",
            "message d'origine",
            "message transféré",
            "mensaje original",
            "mensaje reenviado",
            "oorspronkelijk bericht",
            "doorgestuurd bericht",
        ]
        .contains(&inner.as_str())
}

/// An Outlook-style header block at `at`: an optional separator line, then
/// at least two header lines, one of them the sender and one a date,
/// subject or recipient.
fn header_block(lines: &[String], at: usize) -> Option<(usize, String, Message)> {
    let mut i = at;
    if is_separator(&lines[i]) {
        i += 1;
        while lines.get(i).is_some_and(|l| l.trim().is_empty()) {
            i += 1;
        }
    }
    let start = i;
    let mut fields: Vec<(Key, String)> = Vec::new();
    while let Some((k, v)) = lines.get(i).and_then(|l| header_line(l)) {
        fields.push((k, v));
        i += 1;
    }
    let msg = message_from(&fields)?;
    (i - start >= 2).then(|| {
        let text = lines[at..i]
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        (i - at, text, msg)
    })
}

fn message_from(fields: &[(Key, String)]) -> Option<Message> {
    let has = |k: Key| fields.iter().any(|(f, _)| *f == k);
    if !has(Key::From) || !(has(Key::Date) || has(Key::Subject) || has(Key::To)) {
        return None;
    }
    let mut m = Message {
        quoted: true,
        ..Default::default()
    };
    for (k, v) in fields {
        match k {
            Key::From => m.from = address::loose(v),
            Key::To => m.to = address::loose(v),
            Key::Cc => m.cc = address::loose(v),
            Key::Bcc => m.bcc = address::loose(v),
            Key::Date => m.date = date::loose(v),
            Key::Subject => m.subject = Some(v.clone()).filter(|s| !s.is_empty()),
            Key::Other => {}
        }
    }
    Some(m)
}

/// Mark the elements of an HTML body that open a quoted message.
///
/// The HTML reader has already made paragraphs of the body, so an
/// attribution is a paragraph of its own, and an Outlook header block is one
/// paragraph with its fields run together on a line: `From: Ada Park
/// <ada@example.com> Sent: Friday, July 17, 2026 10:05 AM To: …`.
pub fn mark(elements: &mut [Element]) {
    for e in elements.iter_mut() {
        // A header is a few lines; a long paragraph is prose that happens to
        // mention a sender.
        if e.kind != "doco:Paragraph" || e.text.len() > 2000 {
            continue;
        }
        let msg = attribution_text(&e.text).or_else(|| inline_header(&e.text));
        if let Some(m) = msg {
            e.message = Some(Box::new(m));
        }
    }
}

/// A header block run together on one line, split at its field names.
fn inline_header(text: &str) -> Option<Message> {
    let t = text.trim();
    let body = t
        .char_indices()
        .find(|&(i, _)| header_line(&t[i..]).is_some_and(|(k, _)| k == Key::From))
        .map(|(i, _)| &t[i..])?;
    // Everything before the sender line must be a separator, or nothing.
    if !t[..t.len() - body.len()].trim().is_empty() && !is_separator(&t[..t.len() - body.len()]) {
        return None;
    }
    // Field names start where a known name is followed by a colon.
    let mut starts: Vec<(usize, Key, usize)> = Vec::new();
    for (i, _) in body.char_indices() {
        if i > 0 && !body[..i].ends_with(char::is_whitespace) {
            continue;
        }
        let rest = &body[i..];
        let Some(colon) = rest.find(':').filter(|&c| c <= 20) else {
            continue;
        };
        if let Some(k) = key(&rest[..colon]) {
            starts.push((i, k, i + colon + 1));
        }
    }
    let fields: Vec<(Key, String)> = starts
        .iter()
        .enumerate()
        .map(|(n, &(_, k, v))| {
            let end = starts.get(n + 1).map_or(body.len(), |s| s.0);
            (k, body[v..end].trim().replace('*', ""))
        })
        .collect();
    if fields.len() < 2 {
        return None;
    }
    message_from(&fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn senders(segs: &[Segment]) -> Vec<Option<String>> {
        segs.iter()
            .map(|s| {
                s.header
                    .as_ref()
                    .and_then(|(_, m)| m.from.first())
                    .map(|f| f.display())
            })
            .collect()
    }

    #[test]
    fn a_gmail_thread_splits_at_each_attribution() {
        let body = "Thanks, see you then.\n\nKai\n\n\
            On Fri, Jul 17, 2026 at 10:05 AM Lena Holt <lena@example.com> wrote:\n\
            > Tuesday works.\n>\n> Lena\n>\n\
            > On Thu, Jul 16, 2026 at 8:42 AM Kai Moreno <kai@example.com> wrote:\n\
            > > Can we meet Tuesday?\n";
        let segs = split(body);
        assert_eq!(
            senders(&segs),
            vec![
                None,
                Some("Lena Holt <lena@example.com>".into()),
                Some("Kai Moreno <kai@example.com>".into())
            ]
        );
        assert_eq!(segs[1].lines, vec!["Tuesday works.", "", "Lena", ""]);
        assert_eq!(segs[2].lines, vec!["Can we meet Tuesday?"]);
        let (text, m) = segs[1].header.as_ref().unwrap();
        assert!(text.starts_with("On Fri, Jul 17"));
        assert_eq!(m.date.as_deref(), Some("2026-07-17T10:05:00"));
        assert!(m.quoted);
    }

    #[test]
    fn a_wrapped_attribution_is_one() {
        let body = "Yes.\n\nOn Fri, Jul 17, 2026 at 10:05 AM Lena Holt <\nlena@example.com> wrote:\n> Ready?\n";
        let segs = split(body);
        assert_eq!(
            senders(&segs)[1].as_deref(),
            Some("Lena Holt <lena@example.com>")
        );
        assert_eq!(segs[1].lines, vec!["Ready?"]);
    }

    #[test]
    fn an_outlook_thread_splits_at_each_header_block() {
        let body = "Approved.\n\n\
            ________________________________\n\
            From: Holt, Lena <lena@example.com>\n\
            Sent: Thursday, July 16, 2026 8:42 AM\n\
            To: Kai Moreno <kai@example.com>; Ops Team\n\
            Subject: RE: Budget\n\n\
            Can you approve the budget?\n\n\
            -----Original Message-----\n\
            From: Kai Moreno\n\
            Sent: Wednesday, July 15, 2026 4:10 PM\n\
            Subject: Budget\n\n\
            Draft attached.\n";
        let segs = split(body);
        assert_eq!(
            senders(&segs),
            vec![
                None,
                Some("Holt, Lena <lena@example.com>".into()),
                Some("Kai Moreno".into())
            ]
        );
        let m = &segs[1].header.as_ref().unwrap().1;
        assert_eq!(m.date.as_deref(), Some("2026-07-16T08:42:00"));
        assert_eq!(m.subject.as_deref(), Some("RE: Budget"));
        assert_eq!(m.to.len(), 2);
        assert!(segs[1].header.as_ref().unwrap().0.starts_with("________"));
        assert_eq!(segs[2].lines, vec!["", "Draft attached."]);
    }

    #[test]
    fn prose_that_resembles_a_header_is_prose() {
        let body = "On Monday the board wrote: no change.\n\nFrom: the desk of Kai\nThanks.\n";
        let segs = split(body);
        assert_eq!(segs.len(), 1);
        assert!(segs[0].header.is_none());
    }

    #[test]
    fn attributions_in_other_languages() {
        for (line, name) in [
            (
                "Am 17.07.2026 um 10:05 schrieb Lena Holt <lena@example.com>:",
                "Lena Holt",
            ),
            (
                "Le ven. 17 juil. 2026 à 10:05, Lena Holt <lena@example.com> a écrit :",
                "Lena Holt",
            ),
            (
                "El vie, 17 jul 2026 a las 10:05, Lena Holt (<lena@example.com>) escribió:",
                "Lena Holt",
            ),
            (
                "On 17 Jul 2026, at 10:05, Lena Holt <lena@example.com> wrote:",
                "Lena Holt",
            ),
        ] {
            let m = attribution_text(line).unwrap_or_else(|| panic!("{line}"));
            assert_eq!(m.from[0].name.as_deref(), Some(name), "{line}");
            assert_eq!(
                m.from[0].address.as_deref(),
                Some("lena@example.com"),
                "{line}"
            );
            assert_eq!(m.date.as_deref(), Some("2026-07-17T10:05:00"), "{line}");
        }
    }

    #[test]
    fn a_header_block_run_onto_one_line_reads() {
        let m = inline_header(
            "From: Lena Holt <lena@example.com> Sent: Thursday, July 16, 2026 8:42 AM To: Kai Moreno <kai@example.com> Subject: RE: Budget",
        )
        .unwrap();
        assert_eq!(m.from[0].address.as_deref(), Some("lena@example.com"));
        assert_eq!(m.to[0].name.as_deref(), Some("Kai Moreno"));
        assert_eq!(m.subject.as_deref(), Some("RE: Budget"));
        assert!(inline_header("From: here to there, a road.").is_none());
    }
}
