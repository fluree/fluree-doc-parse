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

/// How a stretch of a body begins.
pub enum Open {
    /// The file's own message, whose header is the file's.
    Own,
    /// A message quoted in the body, with its header as read. A quote with
    /// no attribution over it has a header that says only that it is
    /// quoted.
    Quoted(Box<Message>),
    /// More of the message an earlier segment opened (by its index), after
    /// a quote nested in it.
    Resume(usize),
}

/// A stretch of a plain-text body belonging to one message.
pub struct Segment {
    pub open: Open,
    /// The text that introduces a quoted message, as the body writes it:
    /// its attribution line or header block.
    pub header: Option<String>,
    /// The lines, without their quote marks.
    pub lines: Vec<String>,
}

/// Split a plain-text body into its messages, in the order they are read.
///
/// A line's quote depth is how many `>` it starts with, and a message's
/// text is set at one depth. Text deeper than the message it follows is
/// quoted from an earlier one, introduced by an attribution or header block
/// or by nothing at all. Text shallower than it returns to whichever message
/// was open at that depth: the reply goes on after what it quoted, as a
/// signature and a server's footer do.
pub fn split(text: &str) -> Vec<Segment> {
    let lines: Vec<(usize, String)> = text.lines().map(|l| depth_of(l.trim_end())).collect();
    let mut segs = vec![Segment {
        open: Open::Own,
        header: None,
        lines: Vec::new(),
    }];
    let mut nest = Nesting::new();
    let mut i = 0;
    while i < lines.len() {
        let (depth, content) = &lines[i];
        if blank(content) {
            segs.last_mut().unwrap().lines.push(String::new());
            i += 1;
            continue;
        }
        let opener = attribution(&lines, i).or_else(|| header_block(&lines, i));
        if let Some((n, set_at, text, msg)) = opener {
            let next = lines[i + n..]
                .iter()
                .find(|(_, l)| !blank(l))
                .map_or(set_at, |(d, _)| *d);
            let depth = nest.effective(set_at, next);
            segs.push(Segment {
                open: Open::Quoted(Box::new(msg)),
                header: Some(text),
                lines: Vec::new(),
            });
            nest.open(depth, segs.len() - 1);
            i += n;
            continue;
        }
        match nest.step(*depth) {
            Step::Same => {}
            Step::Deeper => {
                segs.push(Segment {
                    open: Open::Quoted(Box::new(unattributed())),
                    header: None,
                    lines: Vec::new(),
                });
                nest.open(*depth, segs.len() - 1);
            }
            Step::Back(m) => segs.push(Segment {
                open: Open::Resume(m),
                header: None,
                lines: Vec::new(),
            }),
        }
        segs.last_mut().unwrap().lines.push(content.clone());
        i += 1;
    }
    segs
}

/// A stretch of an HTML body belonging to one message.
pub struct Part {
    pub open: Open,
    /// The paragraph that introduces a quoted message: its attribution or
    /// header block.
    pub header: Option<Element>,
    pub elements: Vec<Element>,
}

/// Split the elements of an HTML body into its messages, given how many
/// cited quotations enclose each one (`depths`), the way [`split`] splits
/// plain text by its `>` marks.
///
/// The HTML reader has already made paragraphs of the body, so an
/// attribution is a paragraph of its own, and an Outlook header block is one
/// paragraph with its fields run together on a line: `From: Ada Park
/// <ada@example.com> Sent: Friday, July 17, 2026 10:05 AM To: …`.
pub fn split_elements(elements: Vec<Element>, depths: &[usize]) -> Vec<Part> {
    let mut parts = vec![Part {
        open: Open::Own,
        header: None,
        elements: Vec::new(),
    }];
    let mut nest = Nesting::new();
    for (i, e) in elements.into_iter().enumerate() {
        let depth = depths.get(i).copied().unwrap_or(0);
        // A header is a few lines; a long paragraph is prose that happens to
        // mention a sender.
        let msg = (e.kind == "doco:Paragraph" && e.text.len() <= 2000)
            .then(|| attribution_text(&e.text).or_else(|| inline_header(&e.text)))
            .flatten();
        if let Some(m) = msg {
            let next = depths.get(i + 1).copied().unwrap_or(depth);
            let set_at = nest.effective(depth, next);
            // `Begin forwarded message:` or `-----Original Message-----`,
            // a paragraph of its own just before, announces this message,
            // and opens it.
            let last = &mut parts.last_mut().unwrap().elements;
            let (header, elements) = match last.pop_if(|p| is_separator(&p.text)) {
                Some(separator) => (separator, vec![e]),
                None => (e, Vec::new()),
            };
            parts.push(Part {
                open: Open::Quoted(Box::new(m)),
                header: Some(header),
                elements,
            });
            nest.open(set_at, parts.len() - 1);
            continue;
        }
        match nest.step(depth) {
            Step::Same => {}
            Step::Deeper => {
                parts.push(Part {
                    open: Open::Quoted(Box::new(unattributed())),
                    header: None,
                    elements: Vec::new(),
                });
                nest.open(depth, parts.len() - 1);
            }
            Step::Back(m) => parts.push(Part {
                open: Open::Resume(m),
                header: None,
                elements: Vec::new(),
            }),
        }
        parts.last_mut().unwrap().elements.push(e);
    }
    parts
}

/// The header of a quote nothing attributes.
fn unattributed() -> Message {
    Message {
        quoted: true,
        ..Default::default()
    }
}

/// A line with nothing to read on it. Zero-width characters count as
/// nothing: Apple Mail leaves a byte-order mark at the start of a quote.
pub fn blank(line: &str) -> bool {
    line.chars()
        .all(|c| c.is_whitespace() || matches!(c, '\u{feff}' | '\u{200b}'))
}

/// The messages a body's quoting holds open, innermost last, each with the
/// quote depth its text is set at. The first, the file's own message, is
/// never closed.
struct Nesting {
    open: Vec<(usize, usize)>,
}

enum Step {
    /// The text goes on with the innermost open message.
    Same,
    /// The text is quoted deeper than any open message, and nothing says
    /// whose it is.
    Deeper,
    /// The text returns to a message a nested quote interrupted.
    Back(usize),
}

impl Nesting {
    fn new() -> Self {
        Self { open: vec![(0, 0)] }
    }

    fn top(&self) -> (usize, usize) {
        *self.open.last().unwrap()
    }

    /// Open a message whose text is set at `depth`. One open at the same
    /// depth or deeper ends here: it came before this one in a flat thread,
    /// or it was a quote within the one this answers.
    fn open(&mut self, depth: usize, message: usize) {
        while self.open.len() > 1 && self.top().0 >= depth {
            self.open.pop();
        }
        self.open.push((depth, message));
    }

    /// Where text set at `depth` belongs.
    fn step(&mut self, depth: usize) -> Step {
        let mut closed = false;
        while self.open.len() > 1 && self.top().0 > depth {
            self.open.pop();
            closed = true;
        }
        match self.top() {
            (d, _) if d < depth => Step::Deeper,
            (_, m) if closed => Step::Back(m),
            _ => Step::Same,
        }
    }

    /// The depth a quoted message's text is set at, from the depth its
    /// header is written at and the depth of the text after it. Gmail
    /// writes the attribution a level out from what it quotes; Apple Mail
    /// writes it inside, and now and then a level deeper than the text it
    /// introduces, which belongs to it all the same.
    fn effective(&self, header: usize, next: usize) -> usize {
        let floor = self
            .open
            .iter()
            .rev()
            .map(|(d, _)| *d)
            .find(|d| *d < header);
        match floor {
            _ if next > header => next,
            Some(f) if next < header && next > f => next,
            _ => header,
        }
    }
}

/// A line's quote depth and its text without the marks: `> > text` and
/// `>> text` are both depth 2.
fn depth_of(line: &str) -> (usize, String) {
    let mut depth = 0;
    let mut rest = line;
    loop {
        if let Some(r) = rest.strip_prefix('>') {
            depth += 1;
            rest = r;
        } else if let Some(r) = rest.strip_prefix(" >").filter(|_| depth > 0) {
            depth += 1;
            rest = r;
        } else {
            break;
        }
    }
    if depth > 0 {
        rest = rest.strip_prefix(' ').unwrap_or(rest);
    }
    (depth, rest.to_string())
}

/// An attribution line at `at`, possibly wrapped over up to three lines at
/// the same depth: the lines it spans, its depth, its text, and the message
/// it describes.
fn attribution(lines: &[(usize, String)], at: usize) -> Option<(usize, usize, String, Message)> {
    let (depth, first) = &lines[at];
    if blank(first) {
        return None;
    }
    let mut text = String::new();
    for n in 1..=3 {
        let (d, line) = lines.get(at + n - 1)?;
        let line = line.trim();
        if d != depth || line.is_empty() {
            return None;
        }
        if n > 1 {
            text.push(' ');
        }
        text.push_str(line);
        if let Some(msg) = attribution_text(&text) {
            let written = lines[at..at + n]
                .iter()
                .map(|(_, l)| l.trim())
                .collect::<Vec<_>>()
                .join("\n");
            return Some((n, *depth, written, msg));
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
/// subject or recipient. Returns the lines it spans, the depth its fields
/// are written at, its text, and the message it describes.
///
/// A block starts a paragraph, or at its separator, which Outlook sets
/// straight under the reply's last line; one under a line of text has to be
/// complete, with a sender, a date and a subject or recipient, as Outlook
/// writes it when it sets no separator. A recipient list too long for one
/// line goes on over the next ones. Apple Mail writes `Begin forwarded
/// message:` a level out from the header under it.
fn header_block(lines: &[(usize, String)], at: usize) -> Option<(usize, usize, String, Message)> {
    let (depth, first) = &lines[at];
    let separated = is_separator(first);
    let starts = at == 0 || blank(&lines[at - 1].1) || lines[at - 1].0 != *depth;
    if !separated && header_line(first).is_none() {
        return None;
    }
    let mut i = at;
    if separated {
        i += 1;
        while lines.get(i).is_some_and(|(_, l)| blank(l)) {
            i += 1;
        }
    }
    let set_at = lines.get(i).map_or(*depth, |(d, _)| *d);
    let mut fields: Vec<(Key, String)> = Vec::new();
    while let Some((_, line)) = lines.get(i).filter(|(d, _)| *d == set_at) {
        if let Some(field) = header_line(line) {
            fields.push(field);
        } else if let Some((Key::From | Key::To | Key::Cc | Key::Bcc, value)) = fields.last_mut() {
            // `Cc: Ben Ortiz <ben@example.com>; Ada Park` wrapped onto the
            // next line, as a plain-text client wraps a long list.
            let goes_on = value.trim_end().ends_with([';', ',']) || line.contains('@');
            if !goes_on || blank(line) {
                break;
            }
            value.push(' ');
            value.push_str(line.trim());
        } else {
            break;
        }
        i += 1;
    }
    let msg = message_from(&fields)?;
    let complete = fields.len() >= 3 && msg.date.is_some();
    (fields.len() >= 2 && (separated || starts || complete)).then(|| {
        let text = lines[at..i]
            .iter()
            .map(|(_, l)| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        (i - at, set_at, text, msg)
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

    /// Each segment as how it opens (`own`, the quoted sender or `?` for a
    /// quote with no attribution, `back to N`) and its non-blank lines.
    fn read(body: &str) -> Vec<(String, Vec<String>)> {
        split(body)
            .into_iter()
            .map(|s| {
                let open = match &s.open {
                    Open::Own => "own".to_string(),
                    Open::Quoted(m) => m.from.first().map_or("?".into(), |f| f.display()),
                    Open::Resume(k) => format!("back to {k}"),
                };
                let lines = s.lines.into_iter().filter(|l| !l.is_empty()).collect();
                (open, lines)
            })
            .collect()
    }

    fn seg(open: &str, lines: &[&str]) -> (String, Vec<String>) {
        (open.into(), lines.iter().map(|l| l.to_string()).collect())
    }

    #[test]
    fn a_gmail_thread_splits_at_each_attribution() {
        let body = "Thanks, see you then.\n\nKai\n\n\
            On Fri, Jul 17, 2026 at 10:05 AM Lena Holt <lena@example.com> wrote:\n\
            > Tuesday works.\n>\n> Lena\n>\n\
            > On Thu, Jul 16, 2026 at 8:42 AM Kai Moreno <kai@example.com> wrote:\n\
            > > Can we meet Tuesday?\n";
        assert_eq!(
            read(body),
            vec![
                seg("own", &["Thanks, see you then.", "Kai"]),
                seg("Lena Holt <lena@example.com>", &["Tuesday works.", "Lena"]),
                seg("Kai Moreno <kai@example.com>", &["Can we meet Tuesday?"]),
            ]
        );
        let segs = split(body);
        assert!(segs[1]
            .header
            .as_deref()
            .unwrap()
            .starts_with("On Fri, Jul 17"));
        let Open::Quoted(m) = &segs[1].open else {
            panic!()
        };
        assert_eq!(m.date.as_deref(), Some("2026-07-17T10:05:00"));
        assert!(m.quoted);
    }

    #[test]
    fn a_wrapped_attribution_is_one() {
        let body = "Yes.\n\nOn Fri, Jul 17, 2026 at 10:05 AM Lena Holt <\nlena@example.com> wrote:\n> Ready?\n";
        assert_eq!(
            read(body)[1],
            seg("Lena Holt <lena@example.com>", &["Ready?"])
        );
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
        assert_eq!(
            read(body),
            vec![
                seg("own", &["Approved."]),
                seg(
                    "Holt, Lena <lena@example.com>",
                    &["Can you approve the budget?"]
                ),
                seg("Kai Moreno", &["Draft attached."]),
            ]
        );
        let segs = split(body);
        let Open::Quoted(m) = &segs[1].open else {
            panic!()
        };
        assert_eq!(m.date.as_deref(), Some("2026-07-16T08:42:00"));
        assert_eq!(m.subject.as_deref(), Some("RE: Budget"));
        assert_eq!(m.to.len(), 2);
        assert!(segs[1].header.as_deref().unwrap().starts_with("________"));
    }

    #[test]
    fn an_outlook_separator_can_follow_the_reply_without_a_blank_line() {
        // Outlook sets its rule straight under the signature, and a long
        // recipient list wraps.
        let body = "Agreed.\nKai\n\
            -----Original Message-----\n\
            From: Lena Holt <lena@example.com>\n\
            Sent: Thursday, July 16, 2026 8:42 AM\n\
            Cc: Ben Ortiz <ben@example.com>; Ada Park\n\
            <ada@example.com>; Sam Lee <sam@example.com>\n\
            Subject: Budget\n\n\
            Approve it?\n";
        assert_eq!(
            read(body),
            vec![
                seg("own", &["Agreed.", "Kai"]),
                seg("Lena Holt <lena@example.com>", &["Approve it?"]),
            ]
        );
        let Open::Quoted(m) = &split(body)[1].open else {
            panic!()
        };
        assert_eq!(m.cc.len(), 3);
        assert_eq!(m.subject.as_deref(), Some("Budget"));
    }

    #[test]
    fn a_complete_header_block_needs_no_blank_line_above_it() {
        let body = "Approved.\n\
            Kai Moreno | Example Ltd\n\
            From: Lena Holt <lena@example.com>\n\
            Sent: Thursday, July 16, 2026 8:42 AM\n\
            To: Kai Moreno <kai@example.net>\n\
            Subject: Budget\n\
            Approve it?\n";
        assert_eq!(
            read(body)[1],
            seg("Lena Holt <lena@example.com>", &["Approve it?"])
        );
        // Two fields under a line are a sentence's worth, not a header.
        let segs = split("Noted.\nFrom: Lena Holt\nTo: the board\n");
        assert_eq!(segs.len(), 1);
    }

    #[test]
    fn apple_mail_quotes_its_attribution_and_outlook_chains_inside() {
        // Apple Mail puts the attribution inside the quote, and a thread
        // that came from Outlook is a flat run of header blocks in there.
        let body = "Let us do it.\n\n-Kai\n\n\
            > On Jul 17, 2026, at 10:05\u{202f}AM, Lena Holt <lena@example.com> wrote:\n\
            > \n> Are you in?\n> \n\
            > From: Kai Moreno <kai@example.com <mailto:kai@example.com>>\n\
            > Sent: Thursday, July 16, 2026 8:42 AM\n\
            > To: Lena Holt <lena@example.com>\n\
            > Subject: Pilot\n> \n\
            > Shall we run the pilot?\n";
        assert_eq!(
            read(body),
            vec![
                seg("own", &["Let us do it.", "-Kai"]),
                seg("Lena Holt <lena@example.com>", &["Are you in?"]),
                seg("Kai Moreno <kai@example.com>", &["Shall we run the pilot?"]),
            ]
        );
    }

    #[test]
    fn text_after_a_nested_quote_goes_back_to_its_message() {
        // Lena's signature, below the message she quoted, is Lena's.
        let body = "Done.\n\n\
            > On Fri, Jul 17, 2026 at 10:05 AM Lena Holt <lena@example.com> wrote:\n\
            > Tuesday works.\n>\n\
            > On Thu, Jul 16, 2026 at 8:42 AM Kai Moreno <kai@example.com> wrote:\n\
            >> Can we meet?\n>\n\
            > -- \n> Lena Holt\n> 12 Harbor Street\n";
        assert_eq!(
            read(body),
            vec![
                seg("own", &["Done."]),
                seg("Lena Holt <lena@example.com>", &["Tuesday works."]),
                seg("Kai Moreno <kai@example.com>", &["Can we meet?"]),
                seg("back to 1", &["--", "Lena Holt", "12 Harbor Street"]),
            ]
        );
    }

    #[test]
    fn a_quote_with_no_attribution_is_quoted_all_the_same() {
        // And a reply written under it is the sender's again.
        assert_eq!(
            read("> Can we meet Tuesday?\n> Kai\n\nTuesday works.\n"),
            vec![
                seg("own", &[]),
                seg("?", &["Can we meet Tuesday?", "Kai"]),
                seg("back to 0", &["Tuesday works."]),
            ]
        );
    }

    #[test]
    fn an_attribution_a_level_deeper_than_its_text_still_introduces_it() {
        // Apple Mail now and then quotes an attribution once more than the
        // text under it.
        let body = "> On Fri, Jul 17, 2026 at 10:05 AM Lena Holt <lena@example.com> wrote:\n\
            > Yes.\n\
            >>> On Thu, Jul 16, 2026 at 8:42 AM Kai Moreno <kai@example.com> wrote:\n\
            >> Ready?\n\
            > Lena\n";
        assert_eq!(
            read(body)[1..],
            [
                seg("Lena Holt <lena@example.com>", &["Yes."]),
                seg("Kai Moreno <kai@example.com>", &["Ready?"]),
                seg("back to 1", &["Lena"]),
            ]
        );
    }

    #[test]
    fn prose_that_resembles_a_header_is_prose() {
        let body = "On Monday the board wrote: no change.\n\nFrom: the desk of Kai\nThanks.\n";
        let segs = split(body);
        assert_eq!(segs.len(), 1);
        assert!(matches!(segs[0].open, Open::Own));
    }

    #[test]
    fn html_quotes_nest_by_their_depth() {
        fn p(text: &str) -> Element {
            crate::text::element("doco:Paragraph", text.into(), "html")
        }
        let parts = split_elements(
            vec![
                p("Tuesday works."),
                p("On Mon, Aug 31, 2026 at 4:10 PM Kai Moreno <kai@example.com> wrote:"),
                p("Can we meet Tuesday?"),
                p("--"),
                p("Lena Holt"),
            ],
            &[0, 1, 2, 0, 0],
        );
        let opens: Vec<String> = parts
            .iter()
            .map(|p| match &p.open {
                Open::Own => "own".into(),
                Open::Quoted(m) => m.from[0].display(),
                Open::Resume(k) => format!("back to {k}"),
            })
            .collect();
        assert_eq!(opens, ["own", "Kai Moreno <kai@example.com>", "back to 0"]);
        assert_eq!(parts[2].elements.len(), 2);

        // The line announcing a forward opens the message it announces.
        let parts = split_elements(
            vec![
                p("See below."),
                p("Begin forwarded message:"),
                p("From: Lena Holt <lena@example.com> Subject: Budget Date: July 16, 2026"),
                p("Approve it?"),
            ],
            &[0, 0, 1, 1],
        );
        assert_eq!(parts[0].elements.len(), 1);
        assert_eq!(
            parts[1].header.as_ref().unwrap().text,
            "Begin forwarded message:"
        );
        assert_eq!(parts[1].elements.len(), 2);
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
