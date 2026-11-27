//! Email to DoCO-typed document elements: `.eml` (RFC 5322 and MIME) and
//! Outlook's `.msg`.
//!
//! An email is a thread more often than a message. A reply carries the
//! messages before it in its body, each written by someone else at another
//! time, and read as one body every claim in it belongs to whoever sent
//! the last reply. So the body is split into its messages, and each opens
//! with an element carrying its header (`Element::message`): the file's own
//! message from the file's headers, each quoted one from the attribution
//! line or header block that introduces it. That header's text stays in the
//! text, as a printed thread shows it, so an extractor reading the text sees
//! who wrote each part.
//!
//! Where a message has both, the plain-text body is read rather than the
//! HTML. It states the quoting outright, with `>` or a header block, where
//! the HTML states it in markup each client writes differently, and it
//! carries the same words. A body sent only as HTML is read by the HTML
//! reader, and so is one whose plain text a sender's converter left markup
//! in.
//!
//! Attachments are not read here. Each is a document of its own for its
//! own reader, so they come back beside the elements as bytes, described.
//! There is no geometry: `bbox` is `None` and `page` is 0 throughout.
//!
//! An image the message shows in its body (an inline image, by its
//! `cid:`) is one of those files, and it is also drawn somewhere in the
//! body: there it is a `doco:Figure`, listed in [`Email::media`], for an
//! OCR engine's reading of it to be placed after
//! (`fluree_doc_image::place_after_figures`). The figure's text is the
//! image's alternative text, except for an image under the floor a model is
//! sent ([`fluree_doc_model::under_model_floor`]): a signature's icons
//! (`LinkedIn`, `Logo`) would otherwise lend nearly every corporate message
//! their words.

mod address;
mod date;
mod eml;
mod markup;
mod mime;
mod msg;
mod signature;
mod text;
mod thread;

use fluree_doc_model::{
    Attachment, DocumentInfo, Element, Mailbox, Media, MediaSource, Message, Notes,
};

/// An email, read.
#[derive(Debug)]
pub struct Email {
    /// The messages in reading order, newest first as a reply sets them.
    /// Each opens with an element whose `message` is its header, followed
    /// by its body.
    pub elements: Vec<Element>,
    /// The file's own message as document metadata: its subject is the
    /// title, its sender the creator, and when it was sent the creation
    /// date.
    pub info: DocumentInfo,
    pub attachments: Vec<AttachedFile>,
    /// The images the body shows, each drawn as a figure (`figure-0-<n>`,
    /// in the order the body first shows them), its source naming its part:
    /// the `cid:` URL its message names it by where no other part has that
    /// Content-ID, else `attachment <n>`, its place among
    /// [`Email::attachments`] from 1. An inline image the
    /// body does not mark where it is drawn (a plain-text body read where
    /// only the HTML one shows it) is drawn at the end of the file's own
    /// message. Each is one of [`Email::attachments`]; its bytes are
    /// [`Email::media_bytes`].
    pub media: Vec<Media>,
    /// For each of `media`, its index in `attachments`.
    media_at: Vec<usize>,
}

impl Email {
    /// The bytes of the image of [`Email::media`] held under `part` (its
    /// source's [`MediaSource::part`]).
    pub fn media_bytes(&self, part: &str) -> Option<&[u8]> {
        Some(&self.attachments[self.media_attachment(part)?].bytes)
    }

    /// Which of [`Email::attachments`] the image of [`Email::media`] held
    /// under `part` is.
    pub fn media_attachment(&self, part: &str) -> Option<usize> {
        let i = self
            .media
            .iter()
            .position(|m| m.source.part() == Some(part))?;
        Some(self.media_at[i])
    }

    /// What an emitter carries beside the elements: the document's info and
    /// its attachments, described.
    pub fn notes(&self) -> Notes {
        Notes {
            info: self.info.clone(),
            attachments: self.attachments.iter().map(|a| a.info.clone()).collect(),
            ..Default::default()
        }
    }
}

/// A file an email carries: its description and its bytes, decoded.
#[derive(Debug, Clone)]
pub struct AttachedFile {
    pub info: Attachment,
    pub bytes: Vec<u8>,
    /// The id its message's body names it by (`cid:…`), without angle
    /// brackets: a MIME part's `Content-ID`, an Outlook attachment's
    /// content id.
    pub content_id: Option<String>,
}

impl AttachedFile {
    /// A file described as its message declares it, with its size and hash
    /// taken from its bytes.
    pub fn new(
        filename: Option<String>,
        content_type: String,
        inline: bool,
        bytes: Vec<u8>,
    ) -> Self {
        AttachedFile {
            info: Attachment {
                filename,
                content_type,
                size: bytes.len(),
                sha256: fluree_doc_model::sha256_hex(&bytes),
                inline,
            },
            bytes,
            content_id: None,
        }
    }

    /// The file, named by the id its message's body shows it by.
    pub fn with_content_id(mut self, id: Option<&str>) -> Self {
        self.content_id = id
            .map(|id| {
                id.trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .trim()
            })
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        self
    }
}

/// The two email formats this crate reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Eml,
    Msg,
}

impl Format {
    /// The format of a file by its content.
    ///
    /// An `.msg` is an OLE compound file with Outlook's property streams at
    /// its root; the magic number alone would take in `.doc` and `.xls`.
    /// An `.eml` has no magic number and is recognised by its opening: a
    /// run of header fields that names a sender or a mail route.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if msg::is_msg(bytes) {
            return Some(Self::Msg);
        }
        eml::looks_like(bytes).then_some(Self::Eml)
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Eml => "message/rfc822",
            Self::Msg => "application/vnd.ms-outlook",
        }
    }
}

#[derive(Debug)]
pub enum EmailError {
    /// No header a message has.
    NotAnEmail,
    /// A compound file that is not an Outlook message, or one that could
    /// not be read.
    Msg(String),
}

impl std::fmt::Display for EmailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnEmail => write!(f, "not an email: no message headers"),
            Self::Msg(e) => write!(f, "not a readable Outlook message: {e}"),
        }
    }
}

impl std::error::Error for EmailError {}

/// Parse an email, `.eml` or `.msg`, by its content.
pub fn parse(bytes: &[u8]) -> Result<Email, EmailError> {
    if msg::is_compound(bytes) {
        parse_msg(bytes)
    } else {
        parse_eml(bytes)
    }
}

/// Parse an RFC 5322 message: an `.eml` file, or one saved from any client.
pub fn parse_eml(bytes: &[u8]) -> Result<Email, EmailError> {
    Ok(assemble(eml::read(bytes)?))
}

/// Parse an Outlook `.msg` file.
pub fn parse_msg(bytes: &[u8]) -> Result<Email, EmailError> {
    Ok(assemble(msg::read(bytes)?))
}

/// A message as a reader found it, before it becomes elements.
pub(crate) struct Read {
    pub own: Message,
    /// The own message's header as text: `From: …`, `To: …`, `Date: …`.
    pub header: String,
    pub body: Body,
    pub attachments: Vec<AttachedFile>,
    /// The content ids of the images the message's HTML versions other
    /// than its body show (`<img src="cid:…">`).
    pub shown: Vec<String>,
    pub provenance: &'static str,
}

pub(crate) enum Body {
    Plain(String),
    Html(String),
    None,
}

fn assemble(mut read: Read) -> Email {
    let prov = read.provenance;
    let mut elements = Vec::new();
    // A message with no header at all opens with nothing rather than with
    // an empty element.
    let own = (!read.header.is_empty()).then(|| {
        let mut head = text::element("doco:Paragraph", read.header.clone(), prov);
        head.message = Some(Box::new(read.own.clone()));
        elements.push(head);
        0
    });
    // (element, element that opened the earlier message it goes back to)
    let mut resumes: Vec<(usize, usize)> = Vec::new();
    // Per segment of the body: the element that opened its message, and
    // who sent it.
    let mut opened: Vec<(Option<usize>, Vec<Mailbox>)> = Vec::new();
    // Where the file's own message's body ends, at the end of its first
    // segment: an image it carries and does not mark is drawn there.
    let mut own_end: Option<usize> = None;
    match read.body {
        Body::Plain(body) => {
            for seg in thread::split(&body) {
                let is_own = matches!(seg.open, thread::Open::Own);
                let mut who = owner(seg.open, own, &read.own, &opened);
                // Only a quoted message has a header to open it; one with
                // none opens on its body's first element.
                if let Some(text) = seg.header {
                    let mut e = text::element("doco:Paragraph", text, prov);
                    e.message = who.quoted.take().map(Box::new);
                    who.opener = Some(elements.len());
                    elements.push(e);
                }
                let first = elements.len();
                let mut at = 0;
                for signed in signature::find(&seg.lines, &who.sender, who.resumed) {
                    text::elements(&seg.lines[at..signed.start], prov, &mut elements);
                    let from = elements.len();
                    text::elements(&seg.lines[signed.clone()], prov, &mut elements);
                    for e in &mut elements[from..] {
                        e.signature = true;
                    }
                    at = signed.end;
                }
                text::elements(&seg.lines[at..], prov, &mut elements);
                let sender = who.sender.clone();
                let opener = open_body(&mut elements, first, who, &mut resumes);
                opened.push((opener, sender));
                if is_own && own_end.is_none() {
                    own_end = Some(elements.len());
                }
            }
        }
        Body::Html(html) => {
            let (els, depths, ids) = fluree_doc_html::parse_message(&html);
            read.shown.extend(ids);
            // A paragraph holding nothing but a zero-width character, as
            // Apple Mail and Outlook leave, is not text.
            let (els, depths): (Vec<Element>, Vec<usize>) = els
                .into_iter()
                .zip(depths)
                .filter(|(e, _)| {
                    e.cells.is_some()
                        || e.links.is_some()
                        || e.figure.is_some()
                        || !thread::blank(&e.text)
                })
                .unzip();
            for part in thread::split_elements(els, &depths) {
                let is_own = matches!(part.open, thread::Open::Own);
                let mut who = owner(part.open, own, &read.own, &opened);
                if let Some(mut e) = part.header {
                    e.message = who.quoted.take().map(Box::new);
                    e.provenance = prov;
                    who.opener = Some(elements.len());
                    elements.push(e);
                }
                let first = elements.len();
                // Each element is a paragraph of its own: element `i` is
                // line `2 * i`, with a blank line after it. A figure's
                // alternative text is no line of a signature.
                let lines: Vec<String> = part
                    .elements
                    .iter()
                    .flat_map(|e| {
                        let line = if e.figure.is_some() {
                            String::new()
                        } else {
                            e.text.clone()
                        };
                        [line, String::new()]
                    })
                    .collect();
                let signed = signature::find(&lines, &who.sender, who.resumed);
                for (i, mut e) in part.elements.into_iter().enumerate() {
                    e.provenance = prov;
                    e.signature = signed.iter().any(|r| r.contains(&(2 * i)));
                    elements.push(e);
                }
                let sender = who.sender.clone();
                let opener = open_body(&mut elements, first, who, &mut resumes);
                opened.push((opener, sender));
                if is_own && own_end.is_none() {
                    own_end = Some(elements.len());
                }
            }
        }
        Body::None => {}
    }
    // An image an HTML version shows by its `cid:` is shown in the message,
    // whatever its part's disposition says: a sender's library can mark a
    // related image `attachment` for its file name, and the plain text, read
    // in place of the HTML, does not mark where it is drawn. Decided over the
    // whole message, as the image and the HTML citing it need not be in one
    // multipart: `multipart/related` can hold the alternative or sit in it.
    // Its ids read as the HTML reader reads them (RFC 2392: the scheme in
    // any case, percent signs decoded), and match a part as a figure's do.
    for id in &read.shown {
        if let Some(i) = named(&read.attachments, id, true) {
            if is_image(&read.attachments[i]) {
                read.attachments[i].info.inline = true;
            }
        }
    }
    let own_end = own_end.unwrap_or(elements.len());
    let (media, media_at) = place_images(
        &mut elements,
        &read.attachments,
        own_end,
        prov,
        &mut resumes,
    );
    for (i, e) in elements.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    for (at, opener) in resumes {
        elements[at].resumes = Some(elements[opener].id.clone());
    }
    let own = &read.own;
    Email {
        elements,
        info: DocumentInfo {
            title: own.subject.clone(),
            creators: own.from.iter().map(|m| m.display()).collect(),
            created: own.date.clone(),
            modified: None,
        },
        attachments: read.attachments,
        media,
        media_at,
    }
}

/// Is this file an image: declared as one, or declared as no type in
/// particular (`application/octet-stream`, as a client may send one, or
/// none) and opening as a PNG, JPEG, TIFF, GIF or BMP does? A file declared
/// as another type is that type, whatever its first bytes: a CSV whose
/// first cell is `BMW` is no bitmap.
fn is_image(a: &AttachedFile) -> bool {
    let b = &a.bytes;
    let declared = a.info.content_type.trim();
    if declared.starts_with("image/") {
        return true;
    }
    if !(declared.is_empty() || declared.eq_ignore_ascii_case("application/octet-stream")) {
        return false;
    }
    b.starts_with(b"\x89PNG")
        || b.starts_with(&[0xFF, 0xD8])
        || b.starts_with(b"II*\0")
        || b.starts_with(b"MM\0*")
        || b.starts_with(b"GIF8")
        || b.starts_with(b"BM")
}

/// Whether an image of the message is taken for an icon of a signature:
/// under the floor a model is sent ([`fluree_doc_model::under_model_floor`]),
/// its size read from its header. Such icons carry alternative text
/// (`LinkedIn`, `Logo`) that would read as words of nearly every corporate
/// message's body, so their figure keeps its place without it. An image
/// whose header gives no size is not judged.
fn is_icon(a: &AttachedFile) -> bool {
    fluree_doc_model::stored_size(&a.bytes)
        .is_some_and(|(w, h)| fluree_doc_model::under_model_floor(w, h))
}

/// The file a mark names by `want`, its Content-ID (`by_cid`) or its file
/// name: the one written exactly so, else one written in another case. The
/// one rule for a figure a body places and for an image an HTML version
/// shows, so a message reads the same whichever version is its body.
fn named(attachments: &[AttachedFile], want: &str, by_cid: bool) -> Option<usize> {
    fn have(a: &AttachedFile, by_cid: bool) -> Option<&str> {
        if by_cid {
            a.content_id.as_deref()
        } else {
            a.info.filename.as_deref()
        }
    }
    attachments
        .iter()
        .position(|a| have(a, by_cid) == Some(want))
        .or_else(|| {
            attachments
                .iter()
                .position(|a| have(a, by_cid).is_some_and(|h| h.eq_ignore_ascii_case(want)))
        })
}

/// Resolve the figures the body marks (keyed `cid:<id>` or `name:<file>`,
/// see [`text::elements`]) to the images among the message's files, and
/// number them `figure-0-<n>` in the order the body first shows them. A mark
/// naming no image the message holds is dropped, as it was before images
/// were placed: replies keep the `[cid:…]` of images their quoted messages
/// no longer carry. An inline image no mark names is drawn at `own_end`.
/// Element indices in `resumes` follow the elements. Returns the images, and
/// for each its index in `attachments`.
fn place_images(
    elements: &mut Vec<Element>,
    attachments: &[AttachedFile],
    own_end: usize,
    provenance: &'static str,
    resumes: &mut [(usize, usize)],
) -> (Vec<Media>, Vec<usize>) {
    let find = |key: &str| -> Option<usize> {
        let i = match key.strip_prefix(text::CID_KEY) {
            Some(id) => named(attachments, id, true),
            None => named(attachments, key.strip_prefix(text::NAME_KEY)?, false),
        };
        i.filter(|&i| is_image(&attachments[i]))
    };
    // Each image, numbered as the body first shows it.
    let mut number: std::collections::HashMap<usize, usize> = Default::default();
    let mut order: Vec<usize> = Vec::new();
    let mut resolved: Vec<Option<Option<usize>>> = Vec::with_capacity(elements.len());
    for e in elements.iter() {
        let at = match (&e.kind[..], &e.figure) {
            ("doco:Figure", Some(key)) => Some(find(key)),
            _ => None,
        };
        if let Some(Some(i)) = at {
            number.entry(i).or_insert_with(|| {
                order.push(i);
                order.len()
            });
        }
        resolved.push(at);
    }
    // The inline images the body does not mark, in the message's order.
    let unmarked: Vec<usize> = (0..attachments.len())
        .filter(|i| attachments[*i].info.inline && is_image(&attachments[*i]))
        .filter(|i| !number.contains_key(i))
        .collect();
    for &i in &unmarked {
        order.push(i);
        number.insert(i, order.len());
    }
    let figure = |n: usize| format!("figure-0-{n}");
    let unmarked_figures = || {
        unmarked.iter().map(|i| {
            let mut f = text::element("doco:Figure", String::new(), provenance);
            f.figure = Some(figure(number[i]));
            f
        })
    };
    let old = std::mem::take(elements);
    let n = old.len();
    let mut moved: Vec<Option<usize>> = vec![None; n];
    for (k, (mut e, at)) in old.into_iter().zip(resolved).enumerate() {
        if k == own_end {
            elements.extend(unmarked_figures());
        }
        match at {
            Some(None) => continue,
            Some(Some(i)) => {
                e.figure = Some(figure(number[&i]));
                if is_icon(&attachments[i]) {
                    e.text.clear();
                    e.links = None;
                }
            }
            None => {}
        }
        moved[k] = Some(elements.len());
        elements.push(e);
    }
    if own_end >= n {
        elements.extend(unmarked_figures());
    }
    // A resumed message goes on at an element that is no figure (see
    // `open_body`), so none of them names one that was dropped.
    for (at, opener) in resumes.iter_mut() {
        if let (Some(a), Some(o)) = (moved[*at], moved[*opener]) {
            (*at, *opener) = (a, o);
        }
    }
    let media = order
        .iter()
        .map(|&i| Media {
            figure: figure(number[&i]),
            source: MediaSource::Part(part_key(attachments, i)),
            index: 0,
        })
        .collect();
    (media, order)
}

/// The name an image of the message is read once by: the part itself, not
/// what it is called. Its `cid:` URL where no other part has that
/// Content-ID, else its place among the message's files (`attachment <n>`,
/// from 1, as `doc:attachments` lists them). Never its file name: clients
/// call every pasted image `image.png`, and two images keyed alike would be
/// read as one, the second's text lost and the first's placed twice.
fn part_key(attachments: &[AttachedFile], i: usize) -> String {
    match &attachments[i].content_id {
        Some(id)
            if attachments
                .iter()
                .filter(|a| a.content_id.as_deref() == Some(id))
                .count()
                == 1 =>
        {
            format!("cid:{id}")
        }
        _ => format!("attachment {}", i + 1),
    }
}

/// Whose a segment of the body is.
struct Owner {
    /// The element that opened its message, once one has.
    opener: Option<usize>,
    sender: Vec<Mailbox>,
    /// The header of a quoted message still to be opened.
    quoted: Option<Message>,
    /// The segment goes back to a message opened before it.
    resumed: bool,
}

fn owner(
    open: thread::Open,
    own: Option<usize>,
    own_message: &Message,
    opened: &[(Option<usize>, Vec<Mailbox>)],
) -> Owner {
    match open {
        thread::Open::Own => Owner {
            opener: own,
            sender: own_message.from.clone(),
            quoted: None,
            resumed: false,
        },
        thread::Open::Quoted(m) => Owner {
            opener: None,
            sender: m.from.clone(),
            quoted: Some(*m),
            resumed: false,
        },
        thread::Open::Resume(k) => Owner {
            opener: opened[k].0,
            sender: opened[k].1.clone(),
            quoted: None,
            resumed: true,
        },
    }
}

/// Open a segment's body, which starts at `first`: a quote with no header
/// opens on its first element, and a return to an earlier message is marked
/// on its first. Returns the element that opened the segment's message.
fn open_body(
    elements: &mut [Element],
    first: usize,
    owner: Owner,
    resumes: &mut Vec<(usize, usize)>,
) -> Option<usize> {
    // A message opens, or goes on, on words: an image drawn first is no
    // header's place, and may not be one of the message's files at all.
    let first = (first..elements.len())
        .find(|&i| elements[i].kind != "doco:Figure")
        .unwrap_or(elements.len());
    let has_body = first < elements.len();
    match owner {
        Owner {
            opener: Some(o),
            resumed: true,
            ..
        } => {
            if has_body {
                resumes.push((first, o));
            }
            Some(o)
        }
        Owner {
            opener: None,
            quoted: Some(m),
            ..
        } if has_body => {
            elements[first].message = Some(Box::new(m));
            Some(first)
        }
        Owner { opener, .. } => opener,
    }
}

/// An attachment's name without any directory in it. A message can name a
/// file `../../etc/passwd`, and a consumer that saves attachments by name
/// should not have to know that.
pub(crate) fn base_name(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    (!base.is_empty() && base != "." && base != "..").then(|| base.to_string())
}

/// The own message's header as the text shows it: one field to a line,
/// the fields a reader of a printed email looks for.
pub(crate) fn header_text(m: &Message, date: Option<&str>) -> String {
    let list = |b: &[fluree_doc_model::Mailbox]| {
        b.iter().map(|m| m.display()).collect::<Vec<_>>().join(", ")
    };
    let mut lines = Vec::new();
    for (name, boxes) in [
        ("From", &m.from),
        ("To", &m.to),
        ("Cc", &m.cc),
        ("Bcc", &m.bcc),
    ] {
        if !boxes.is_empty() {
            lines.push(format!("{name}: {}", list(boxes)));
        }
    }
    if let Some(d) = date.or(m.date.as_deref()) {
        lines.push(format!("Date: {d}"));
    }
    if let Some(s) = &m.subject {
        lines.push(format!("Subject: {s}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heads(email: &Email) -> Vec<(String, Option<String>)> {
        email
            .elements
            .iter()
            .filter_map(|e| {
                let m = e.message.as_deref()?;
                Some((m.from.first()?.display(), m.date.clone()))
            })
            .collect()
    }

    fn texts(email: &Email) -> Vec<&str> {
        email.elements.iter().map(|e| e.text.as_str()).collect()
    }

    #[test]
    fn a_plain_reply_reads_as_its_thread() {
        let src = b"From: Lena Holt <lena@example.com>\r\n\
To: Kai Moreno <kai@example.com>\r\n\
Cc: \"Ortiz, Ben\" <ben@example.com>\r\n\
Subject: RE: Pilot\r\n\
Date: Fri, 17 Jul 2026 13:48:00 -0500\r\n\
Message-ID: <3@example.com>\r\n\
In-Reply-To: <2@example.com>\r\n\
References: <1@example.com> <2@example.com>\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Kai,\r\n\
\r\n\
The credit is fair.\r\n\
\r\n\
Lena Holt\r\n\
Finance Director\r\n\
\r\n\
On Fri, Jul 17, 2026 at 10:05 AM Kai Moreno <kai@example.com> wrote:\r\n\
> We will apply a credit of $15,000.\r\n";
        let email = parse(src).unwrap();
        assert_eq!(
            texts(&email),
            vec![
                "From: Lena Holt <lena@example.com>\nTo: Kai Moreno <kai@example.com>\nCc: Ortiz, Ben <ben@example.com>\nDate: Fri, 17 Jul 2026 13:48:00 -0500\nSubject: RE: Pilot",
                "Kai,",
                "The credit is fair.",
                "Lena Holt\nFinance Director",
                "On Fri, Jul 17, 2026 at 10:05 AM Kai Moreno <kai@example.com> wrote:",
                "We will apply a credit of $15,000.",
            ]
        );
        assert_eq!(
            heads(&email),
            vec![
                (
                    "Lena Holt <lena@example.com>".into(),
                    Some("2026-07-17T13:48:00-05:00".into())
                ),
                (
                    "Kai Moreno <kai@example.com>".into(),
                    Some("2026-07-17T10:05:00".into())
                ),
            ]
        );
        let own = email.elements[0].message.as_deref().unwrap();
        assert_eq!(own.message_id.as_deref(), Some("3@example.com"));
        assert_eq!(own.in_reply_to, vec!["2@example.com"]);
        assert_eq!(own.references.len(), 2);
        assert!(!own.quoted);
        assert_eq!(email.info.title.as_deref(), Some("RE: Pilot"));
        assert_eq!(email.info.creators, vec!["Lena Holt <lena@example.com>"]);
        assert_eq!(
            email.info.created.as_deref(),
            Some("2026-07-17T13:48:00-05:00")
        );
        assert_eq!(email.elements[0].provenance, "eml");
        assert_eq!(email.elements[5].id, "elem-00006");
    }

    #[test]
    fn a_client_message_reads_its_plain_part_and_keeps_its_files() {
        let src = b"From: =?utf-8?Q?Ren=C3=A9e_Holt?= <renee@example.com>\r\n\
To: kai@example.com\r\n\
Subject: =?utf-8?B?UXVvdGUgZm9yIDQwIHNlYXRz?=\r\n\
Date: Mon, 14 Sep 2026 09:00:00 +0200\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
\r\n\
--outer\r\n\
Content-Type: multipart/alternative; boundary=\"alt\"\r\n\
\r\n\
--alt\r\n\
Content-Type: text/plain; charset=\"iso-8859-1\"\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
The quote is attached. Na=EFve question: does it include =\r\n\
training?\r\n\
--alt\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<div>The quote is attached.</div>\r\n\
--alt--\r\n\
--outer\r\n\
Content-Type: application/pdf; name=\"quote.pdf\"\r\n\
Content-Disposition: attachment; filename=\"quote.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjcKc3RhbmQtaW4=\r\n\
--outer\r\n\
Content-Type: message/rfc822\r\n\
\r\n\
From: Kai Moreno <kai@example.com>\r\n\
Subject: Seats\r\n\
\r\n\
How many seats?\r\n\
--outer--\r\n";
        let email = parse(src).unwrap();
        assert_eq!(email.info.title.as_deref(), Some("Quote for 40 seats"));
        assert_eq!(email.info.creators, vec!["Renée Holt <renee@example.com>"]);
        assert_eq!(
            email.elements[1].text,
            "The quote is attached. Naïve question: does it include training?"
        );
        assert_eq!(
            email.elements.len(),
            2,
            "the HTML alternative is not read twice"
        );
        let files: Vec<(Option<&str>, &str, usize)> = email
            .attachments
            .iter()
            .map(|a| {
                (
                    a.info.filename.as_deref(),
                    a.info.content_type.as_str(),
                    a.info.size,
                )
            })
            .collect();
        assert_eq!(
            files,
            vec![
                (Some("quote.pdf"), "application/pdf", 17),
                (Some("Seats.eml"), "message/rfc822", 69),
            ]
        );
        assert_eq!(email.attachments[0].bytes, b"%PDF-1.7\nstand-in");
        let fwd = parse(&email.attachments[1].bytes).unwrap();
        assert_eq!(fwd.elements[1].text, "How many seats?");
        let notes = email.notes();
        assert_eq!(notes.attachments.len(), 2);
        assert_eq!(notes.info.title.as_deref(), Some("Quote for 40 seats"));
    }

    #[test]
    fn an_html_only_message_is_read_by_the_html_reader_and_split_the_same_way() {
        let src = b"From: Lena Holt <lena@example.com>\r\n\
Subject: Re: Kickoff\r\n\
Date: Tue, 1 Sep 2026 08:00:00 +0000\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body><div dir=\"ltr\">Tuesday works.<div><br></div><div>Lena</div></div>\
<div class=\"gmail_quote\"><div class=\"gmail_attr\">On Mon, Aug 31, 2026 at 4:10 PM Kai Moreno &lt;kai@example.com&gt; wrote:<br></div>\
<blockquote class=\"gmail_quote\"><div dir=\"ltr\">Can we meet Tuesday?</div></blockquote></div>\
<div id=\"divRplyFwdMsg\"><b>From:</b> Ben Ortiz &lt;ben@example.com&gt;<br><b>Sent:</b> Monday, August 31, 2026 9:00 AM<br><b>Subject:</b> Kickoff</div>\
<p>Who is coming?</p></body></html>\r\n";
        let email = parse(src).unwrap();
        assert_eq!(
            texts(&email)[1..],
            [
                "Tuesday works.",
                "Lena",
                "On Mon, Aug 31, 2026 at 4:10 PM Kai Moreno <kai@example.com> wrote:",
                "Can we meet Tuesday?",
                "From: Ben Ortiz <ben@example.com> Sent: Monday, August 31, 2026 9:00 AM Subject: Kickoff",
                "Who is coming?",
            ]
        );
        assert_eq!(
            heads(&email),
            vec![
                (
                    "Lena Holt <lena@example.com>".into(),
                    Some("2026-09-01T08:00:00Z".into())
                ),
                (
                    "Kai Moreno <kai@example.com>".into(),
                    Some("2026-08-31T16:10:00".into())
                ),
                (
                    "Ben Ortiz <ben@example.com>".into(),
                    Some("2026-08-31T09:00:00".into())
                ),
            ]
        );
        assert_eq!(
            (email.elements[1].provenance, email.elements[1].evidence),
            ("eml", "html")
        );
    }

    #[test]
    fn a_plain_part_left_with_markup_gives_way_to_the_html() {
        // A billing system's converter stripped the tags and stopped: the
        // style sheet and the character references are still in it.
        let src = b"From: Billing <billing@example.com>\r\n\
Subject: Your bill is ready\r\n\
Content-Type: multipart/alternative; boundary=\"alt\"\r\n\
\r\n\
--alt\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
.banner\r\n\
{\r\n\
    color: &#35;EEEEEE;\r\n\
}\r\n\
Invoice Date\r\n\
\r\n\
03&#47;14&#47;2026\r\n\
--alt\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><head><style>.banner { color: &#35;EEEEEE; }</style></head><body>\
<table width=\"100%\"><tr><td><table>\
<tr><td>Invoice Date</td><td>Amount Due</td></tr>\
<tr><td>03&#47;14&#47;2026</td><td>$1,200.00</td></tr>\
</table></td></tr></table></body></html>\r\n\
--alt--\r\n";
        let email = parse(src).unwrap();
        assert_eq!(email.elements.len(), 2);
        assert_eq!(
            email.elements[1].cells.as_ref().unwrap()[1],
            vec!["03/14/2026", "$1,200.00"]
        );
        assert_eq!(
            (email.elements[1].provenance, email.elements[1].evidence),
            ("eml", "html")
        );
    }

    /// Each element as a consumer reads it: its text, whose message it is
    /// in (by sender), and whether it is signature.
    fn owned(email: &Email) -> Vec<(String, String, bool)> {
        let mut by_id: std::collections::HashMap<&str, String> = Default::default();
        let mut owner = String::new();
        let mut out = Vec::new();
        for e in &email.elements {
            if let Some(m) = &e.message {
                owner = m.from.first().map_or("?".into(), |f| f.display());
                by_id.insert(&e.id, owner.clone());
            } else if let Some(r) = &e.resumes {
                owner = by_id[r.as_str()].clone();
            }
            out.push((e.text.clone(), owner.clone(), e.signature));
        }
        out
    }

    fn signatures(email: &Email) -> Vec<(String, String)> {
        owned(email)
            .into_iter()
            .filter(|(_, _, sig)| *sig)
            .map(|(text, who, _)| (who, text))
            .collect()
    }

    #[test]
    fn apple_mail_quotes_the_attribution_and_the_chain_it_carries() {
        // Apple Mail, plain text only: the attribution inside the quote, a
        // narrow no-break space before AM, an Outlook-style header block in
        // the quote, and a Gmail attribution with the text under it
        // unmarked.
        let src = "From: Ben Ortiz <ben@example.com>\r\n\
To: Ada Park <ada@example.org>\r\n\
Date: Fri, 25 Sep 2026 11:11:40 -0600\r\n\
Subject: Re: Pilot\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: 8bit\r\n\
\r\n\
Ada, I can join today.\r\n\
\r\n\
> On Sep 24, 2026, at 7:41\u{202f}AM, Ada Park <ada@example.org> wrote:\r\n\
> \r\n\
> Thanks Kai! Ben, good to meet you.\r\n\
> \r\n\
> Best,\r\n\
> Ada Park\r\n\
> \r\n\
> From: Ben Ortiz <ben@example.com>\r\n\
> Date: Thursday, September 24, 2026 at 7:58\u{202f}AM\r\n\
> To: Kai Moreno <kai@example.net>\r\n\
> Cc: Ada Park <ada@example.org>\r\n\
> Subject: Re: Pilot\r\n\
> \r\n\
> Thanks Kai, glad to meet Ada.\r\n\
> \r\n\
> On Wed, Sep 23, 2026 at 18:13 Kai Moreno <kai@example.net> wrote:\r\n\
> Hi Ben,\r\n\
> Ada runs the account and has a demo built.\r\n\
> Best,\r\n\
> Kai\r\n\
> \r\n\
> Kai Moreno | Data Lead\r\n\
> Example Data Inc. | 1 Main Street, Springfield\r\n";
        let email = parse(src.as_bytes()).unwrap();
        assert_eq!(
            heads(&email),
            vec![
                (
                    "Ben Ortiz <ben@example.com>".into(),
                    Some("2026-09-25T11:11:40-06:00".into())
                ),
                (
                    "Ada Park <ada@example.org>".into(),
                    Some("2026-09-24T07:41:00".into())
                ),
                (
                    "Ben Ortiz <ben@example.com>".into(),
                    Some("2026-09-24T07:58:00".into())
                ),
                (
                    "Kai Moreno <kai@example.net>".into(),
                    Some("2026-09-23T18:13:00".into())
                ),
            ]
        );
        assert_eq!(
            signatures(&email),
            vec![
                (
                    "Ada Park <ada@example.org>".into(),
                    "Best,\nAda Park".into()
                ),
                ("Kai Moreno <kai@example.net>".into(), "Best,\nKai".into()),
                (
                    "Kai Moreno <kai@example.net>".into(),
                    "Kai Moreno | Data Lead\nExample Data Inc. | 1 Main Street, Springfield".into()
                ),
            ]
        );
        // The quote marks are gone from what was quoted.
        assert!(texts(&email).contains(&"Thanks Kai, glad to meet Ada."));
    }

    #[test]
    fn a_signature_under_what_it_quoted_is_its_senders() {
        // The reply's signature comes after a quote nested in it, as Gmail
        // sets it, and that is where the address is.
        let src = "From: Ada Park <ada@example.org>\r\n\
Date: Thu, 1 Oct 2026 12:20:06 -0400\r\n\
Subject: Re: Brief\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Thanks, will do.\r\n\
\r\n\
-Ada\r\n\
\r\n\
> On Sep 29, 2026, at 5:05 PM, Kai Moreno <kai@example.net> wrote:\r\n\
> \r\n\
> Is the ontology included?\r\n\
> \r\n\
> Best,\r\n\
> Kai\r\n\
> \r\n\
> On Tue, Sep 29, 2026 at 1:01 PM Ada Park <ada@example.org <mailto:ada@example.org>> wrote:\r\n\
>> Kai,\r\n\
>> \r\n\
>> The brief is attached.\r\n\
>> \r\n\
>> -Ada\r\n\
> \r\n\
> -- \r\n\
> Kai Moreno, Senior Director\r\n\
> Example Data Inc.\r\n\
> 1 Main Street, Springfield\r\n";
        let email = parse(src.as_bytes()).unwrap();
        let kai = "Kai Moreno <kai@example.net>".to_string();
        let ada = "Ada Park <ada@example.org>".to_string();
        assert_eq!(
            signatures(&email),
            vec![
                (ada.clone(), "-Ada".into()),
                (kai.clone(), "Best,\nKai".into()),
                (ada.clone(), "-Ada".into()),
                (
                    kai.clone(),
                    "--\nKai Moreno, Senior Director\nExample Data Inc.\n1 Main Street, Springfield"
                        .into()
                ),
            ]
        );
        // The return is marked on the element, by the id of the element
        // that opened Kai's message.
        let back = email.elements.iter().find(|e| e.resumes.is_some()).unwrap();
        let opener = email
            .elements
            .iter()
            .find(|e| Some(&e.id) == back.resumes.as_ref())
            .unwrap();
        assert_eq!(opener.message.as_ref().unwrap().from[0].display(), kai);
    }

    #[test]
    fn an_outlook_reply_splits_where_its_rule_follows_the_text() {
        // Outlook 365: the rule straight under the reply, an image
        // placeholder and zero-width spaces in the signature.
        let src = "From: Lena Holt <lena@example.com>\r\n\
Date: Thu, 1 Oct 2026 18:11:09 +0000\r\n\
Subject: Re: Board call\r\n\
Content-Type: multipart/alternative; boundary=\"alt\"\r\n\
\r\n\
--alt\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Moved to Monday.\r\n\
________________________________\r\n\
From: Lena Holt <lena@example.com>\r\n\
Sent: Thursday, October 1, 2026 2:06 PM\r\n\
To: Kai Moreno <kai@example.net>\r\n\
Subject: Board call\r\n\
\r\n\
Is the call still on?\r\n\
\r\n\
Lena\r\n\
[cid:image001.png@01DC0000.00000000]\r\n\
Lena Holt \u{200b}\u{200b}\r\n\
Founder, Example Health\r\n\
PO Box 100 | Springfield | 49588\r\n\
--alt\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<div>Moved to Monday.</div><div id=\"appendonsend\"></div><hr>\
<div id=\"divRplyFwdMsg\"><b>From:</b> Lena Holt</div><div>Is the call still on?</div>\r\n\
--alt--\r\n";
        let email = parse(src.as_bytes()).unwrap();
        assert_eq!(heads(&email).len(), 2);
        assert_eq!(
            signatures(&email),
            vec![
                ("Lena Holt <lena@example.com>".into(), "Lena".into()),
                (
                    "Lena Holt <lena@example.com>".into(),
                    "Lena Holt\nFounder, Example Health\nPO Box 100 | Springfield | 49588".into()
                ),
            ]
        );
    }

    #[test]
    fn html_quotes_nest_by_their_markup() {
        // Gmail's message, quoted whole by Apple Mail: its own quote and,
        // after it, its signature.
        let src = b"From: Ada Park <ada@example.org>\r\n\
Subject: Re: Brief\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<div>Will do.</div><div>-Ada</div>\
<blockquote type=\"cite\"><div>On Sep 29, 2026, at 5:05 PM, Kai Moreno &lt;kai@example.net&gt; wrote:</div>\
<div dir=\"ltr\">Is the ontology included?</div>\
<div class=\"gmail_quote\"><div class=\"gmail_attr\">On Tue, Sep 29, 2026 at 1:01 PM Ada Park &lt;ada@example.org&gt; wrote:<br></div>\
<blockquote class=\"gmail_quote\" style=\"margin:0 0 0 .8ex\"><div>The brief is attached.</div></blockquote></div>\
<br clear=\"all\"><span class=\"gmail_signature_prefix\">-- </span><br>\
<div class=\"gmail_signature\"><div>Kai Moreno, Senior Director</div><div>1 Main Street, Springfield</div></div>\
</blockquote>\r\n";
        let email = parse(src).unwrap();
        let kai = "Kai Moreno <kai@example.net>".to_string();
        let owners: Vec<(String, String)> = owned(&email)
            .into_iter()
            .skip(1)
            .map(|(text, who, _)| (text, who))
            .collect();
        assert_eq!(
            owners[owners.len() - 3..],
            [
                ("--".to_string(), kai.clone()),
                ("Kai Moreno, Senior Director".to_string(), kai.clone()),
                ("1 Main Street, Springfield".to_string(), kai.clone()),
            ]
        );
        assert!(email.elements[email.elements.len() - 3].resumes.is_some());
        assert!(email.elements.iter().rev().take(3).all(|e| e.signature));
    }

    #[test]
    fn a_quote_nothing_attributes_is_a_message_and_the_reply_under_it_is_not() {
        let src = b"From: Kai Moreno <kai@example.net>\r\n\
Subject: Re: Meeting\r\n\
\r\n\
> Can we meet Tuesday?\r\n\
\r\n\
Tuesday works.\r\n\
\r\n\
-Kai\r\n";
        let email = parse(src).unwrap();
        let quote = email.elements[1].message.as_deref().unwrap();
        assert!(quote.quoted && quote.from.is_empty());
        assert_eq!(email.elements[2].resumes.as_deref(), Some("elem-00001"));
        assert_eq!(
            owned(&email)[1..],
            [
                ("Can we meet Tuesday?".into(), "?".into(), false),
                (
                    "Tuesday works.".into(),
                    "Kai Moreno <kai@example.net>".into(),
                    false
                ),
                ("-Kai".into(), "Kai Moreno <kai@example.net>".into(), true),
            ]
        );
    }

    #[test]
    fn an_html_message_is_read_a_paragraph_at_a_time_for_its_footer() {
        // Each block of HTML is a paragraph of its own: a notice at the end
        // is the footer, not everything above it.
        let src = b"From: Example Bank <alerts@example.com>\r\n\
Subject: Payment processed\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<div>Payment processed</div><div>The payment of $120.00 to Example Ltd has been sent.</div>\
<div>This is an account service message, intended solely for the addressee.</div>\r\n";
        let email = parse(src).unwrap();
        let signed: Vec<bool> = email.elements.iter().map(|e| e.signature).collect();
        assert_eq!(signed, [false, false, false, true]);
    }

    #[test]
    fn an_attachment_name_carries_no_directory() {
        assert_eq!(base_name("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(base_name("C:\\Users\\a\\q.pdf").as_deref(), Some("q.pdf"));
        assert_eq!(base_name("../"), None);
    }

    #[test]
    fn email_is_told_apart_by_content() {
        assert_eq!(
            Format::sniff(
                b"Return-Path: <a@example.com>\r\nReceived: from x\r\nSubject: s\r\n\r\nbody"
            ),
            Some(Format::Eml)
        );
        assert_eq!(
            Format::sniff(b"From: Ada <a@example.com>\nSubject: Hi\n\nbody"),
            Some(Format::Eml)
        );
        assert_eq!(Format::sniff(b"# Notes\n\nFrom: me\n"), None);
        assert_eq!(Format::sniff(b"Title: Notes\nAuthor: me\n\nbody"), None);
        assert_eq!(Format::sniff(b"%PDF-1.7"), None);
        assert_eq!(Format::sniff(&msg::tests::sample()), Some(Format::Msg));
        assert!(matches!(parse(b"just words"), Err(EmailError::NotAnEmail)));
    }

    /// The figures of an email: (index, figure id), in order.
    fn figures(email: &Email) -> Vec<(usize, &str)> {
        email
            .elements
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind == "doco:Figure")
            .map(|(i, e)| (i, e.figure.as_deref().unwrap()))
            .collect()
    }

    #[test]
    fn an_inline_image_only_the_html_shows_is_drawn_after_the_own_message() {
        // Plain text and HTML alternatives; the HTML, and its image, related.
        let src = b"From: a@example.org\r\n\
To: b@example.org\r\n\
Subject: Record\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"alt\"\r\n\
\r\n\
--alt\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
See below.\r\n\
\r\n\
On Thu, Jul 16, 2026 at 8:42 AM Kai Moreno <kai@example.com> wrote:\r\n\
> Send the record.\r\n\
--alt\r\n\
Content-Type: multipart/related; boundary=\"rel\"\r\n\
\r\n\
--rel\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>See below.</p><img src=\"cid:shot1@example.org\">\r\n\
--rel\r\n\
Content-Type: image/png\r\n\
Content-ID: <shot1@example.org>\r\n\
\r\n\
pixels\r\n\
--rel--\r\n\
--alt--\r\n";
        let email = parse(src).unwrap();
        assert_eq!(texts(&email)[1], "See below.");
        // After the file's own message, before the one it quotes.
        assert_eq!(figures(&email), [(2, "figure-0-1")]);
        assert!(email.elements[3].message.as_deref().unwrap().quoted);
        assert_eq!(texts(&email).last(), Some(&"Send the record."));
        assert_eq!(email.media.len(), 1);
        let m = &email.media[0];
        assert_eq!(m.source.name(), "cid:shot1@example.org");
        assert_eq!(m.tag(), "m1");
        assert_eq!(email.media_bytes(m.source.name()), Some(&b"pixels"[..]));
        // Still one of its files.
        assert_eq!(email.attachments.len(), 1);
        assert!(email.attachments[0].info.inline);
    }

    #[test]
    fn only_a_file_of_no_declared_type_is_taken_for_an_image_by_its_first_bytes() {
        // Two inline parts the body does not mark: a bitmap sent as
        // `application/octet-stream`, drawn, and a CSV whose first cell
        // starts as a bitmap does, which stays a file.
        let src = b"From: a@example.org\r\n\
To: b@example.org\r\n\
Subject: Record\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"mix\"\r\n\
\r\n\
--mix\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
See below.\r\n\
--mix\r\n\
Content-Type: application/octet-stream\r\n\
Content-Disposition: inline; filename=\"shot\"\r\n\
\r\n\
BMpixels\r\n\
--mix\r\n\
Content-Type: text/csv\r\n\
Content-Disposition: inline; filename=\"fleet.csv\"\r\n\
\r\n\
BMW 320d,2019\r\n\
--mix--\r\n";
        let email = parse(src).unwrap();
        assert_eq!(email.attachments.len(), 2);
        let drawn: Vec<&str> = email.media.iter().map(|m| m.source.name()).collect();
        assert_eq!(drawn, ["attachment 1"]);
    }

    #[test]
    fn an_image_the_html_shows_is_drawn_whatever_its_disposition() {
        // The related image marked `attachment` for its file name, as some
        // libraries write it, and an image attached whose id the HTML only
        // starts with: the first is shown, the second is a file. Wherever
        // `multipart/related` sits: inside the alternative (Apple Mail),
        // around it (Gmail, Outlook), under a `multipart/mixed`, or holding
        // the only HTML; and however the URL is spelled (RFC 2392: the
        // scheme in any case, the id percent-encoded, or in another case
        // than its part's, matched alike whichever version is the body). An
        // image named in other letters than ASCII is no part of it.
        for url in [
            "cid:shot1@example.org",
            "CID:shot1@example.org",
            "cid:shot1%40example.org",
            "cid:SHOT1@Example.org",
        ] {
            let plain = "--alt\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nSee below.\r\n";
            let html = format!(
                "Content-Type: text/html; charset=utf-8\r\n\r\n\
<p>See below.</p><img src=\"{url}\"><img src=\"日本.png\">\r\n"
            );
            let images = "--rel\r\n\
Content-Type: image/png\r\n\
Content-Disposition: attachment; filename=\"shot.png\"\r\n\
Content-ID: <shot1@example.org>\r\n\
\r\n\
pixels\r\n\
--rel\r\n\
Content-Type: image/png\r\n\
Content-Disposition: attachment; filename=\"photo.png\"\r\n\
Content-ID: <shot1@example>\r\n\
\r\n\
other\r\n\
--rel--\r\n";
            let alt = "Content-Type: multipart/alternative; boundary=\"alt\"\r\n\r\n";
            let rel = "Content-Type: multipart/related; boundary=\"rel\"\r\n\r\n";
            let inside = format!("{alt}{plain}--alt\r\n{rel}--rel\r\n{html}{images}--alt--\r\n");
            let around = format!("{rel}--rel\r\n{alt}{plain}--alt\r\n{html}--alt--\r\n{images}");
            let mixed = format!(
                "Content-Type: multipart/mixed; boundary=\"mix\"\r\n\r\n--mix\r\n{around}--mix--\r\n"
            );
            let html_only = format!("{rel}--rel\r\n{html}{images}");
            for (shape, mime) in [
                ("inside", inside),
                ("around", around),
                ("mixed", mixed),
                ("html only", html_only),
            ] {
                let src = format!(
                    "From: a@example.org\r\nTo: b@example.org\r\nSubject: Record\r\nMIME-Version: 1.0\r\n{mime}"
                );
                let email = parse(src.as_bytes()).unwrap();
                assert_eq!(figures(&email).len(), 1, "{url} {shape}");
                assert_eq!(email.media.len(), 1, "{url} {shape}");
                assert_eq!(
                    email.media[0].source.name(),
                    "cid:shot1@example.org",
                    "{url} {shape}"
                );
                let inline: Vec<bool> = email.attachments.iter().map(|a| a.info.inline).collect();
                assert_eq!(inline, [true, false], "{url} {shape}");
            }
        }
    }

    #[test]
    fn an_image_outlook_marks_in_plain_text_is_drawn_where_it_is_marked() {
        // The reply's screenshot between two paragraphs; the quoted
        // message's image is no longer carried, and its mark is dropped.
        let src = b"From: Lena Holt <lena@example.com>\r\n\
To: Kai Moreno <kai@example.com>\r\n\
Subject: RE: Record\r\n\
Date: Fri, 17 Jul 2026 13:48:00 -0500\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"rel\"\r\n\
\r\n\
--rel\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Kai, the record:\r\n\
\r\n\
[cid:image001.png@01DC0000.00000000]\r\n\
\r\n\
Lena\r\n\
\r\n\
From: Kai Moreno <kai@example.com>\r\n\
Sent: Thursday, July 16, 2026 8:42 AM\r\n\
Subject: Record\r\n\
\r\n\
Which one? [cid:image002.png@01DC0000.00000000]\r\n\
--rel\r\n\
Content-Type: image/png; name=\"image001.png\"\r\n\
Content-ID: <image001.png@01DC0000.00000000>\r\n\
\r\n\
pixels\r\n\
--rel--\r\n";
        let email = parse(src).unwrap();
        let t = texts(&email);
        let f = figures(&email);
        assert_eq!(f.len(), 1, "{t:?}");
        let at = f[0].0;
        assert_eq!(t[at - 1], "Kai, the record:");
        assert_eq!(t[at + 1], "Lena");
        assert_eq!(
            email.media[0].source.name(),
            "cid:image001.png@01DC0000.00000000"
        );
        // The quoted message still opens where it did.
        assert!(email.elements[at + 2].message.as_deref().unwrap().quoted);
        assert_eq!(t[at + 3], "Which one?");
    }

    #[test]
    fn an_image_shown_twice_is_one_image_and_a_message_without_any_is_unchanged() {
        let src = b"From: a@example.org\r\n\
Subject: Logo\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"rel\"\r\n\
\r\n\
--rel\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p><img src=\"cid:logo\" alt=\"Logo\"> Hello</p><p>Bye <img src=\"cid:logo\"></p><p><img src=\"cid:pdf\"></p>\r\n\
--rel\r\n\
Content-Type: image/gif\r\n\
Content-ID: <logo>\r\n\
\r\n\
GIF89a\r\n\
--rel\r\n\
Content-Type: application/pdf\r\n\
Content-ID: <pdf>\r\n\
\r\n\
%PDF-1.7\r\n\
--rel--\r\n";
        let email = parse(src).unwrap();
        let f: Vec<&str> = figures(&email).iter().map(|f| f.1).collect();
        // A PDF the body shows by `cid:` is no image.
        assert_eq!(f, ["figure-0-1", "figure-0-1"]);
        assert_eq!(email.media.len(), 1);
        assert_eq!(texts(&email)[1..], ["Hello", "Logo", "Bye", ""]);
        // Without images, as before: no figure, nothing listed.
        let plain =
            parse(b"From: a@example.org\r\nSubject: s\r\n\r\nHi [cid:gone@x] there <old.png>\r\n")
                .unwrap();
        assert!(plain.media.is_empty());
        assert_eq!(texts(&plain)[1..], ["Hi  there"]);
    }

    #[test]
    fn a_signatures_icon_keeps_its_figure_but_not_its_alternative_text() {
        // A GIF's header: its logical screen, `w` × `h`.
        let gif = |w: u16, h: u16| {
            let mut g = b"GIF89a".to_vec();
            g.extend(w.to_le_bytes());
            g.extend(h.to_le_bytes());
            g.extend(b"\x80\x00\x00\xff\xff\xff\x00\x00\x00\x3b");
            g
        };
        let mut src = b"From: a@example.org\r\n\
Subject: Chart\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"rel\"\r\n\
\r\n\
--rel\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>See below.</p><p><img src=\"cid:chart@x\" alt=\"Org chart\"></p>\
<p>Ann Smith <img src=\"cid:in@x\" alt=\"LinkedIn\"></p>\r\n"
            .to_vec();
        for (id, bytes) in [("chart@x", gif(800, 600)), ("in@x", gif(16, 16))] {
            src.extend(
                format!("--rel\r\nContent-Type: image/gif\r\nContent-ID: <{id}>\r\n\r\n").bytes(),
            );
            src.extend(bytes);
            src.extend(b"\r\n");
        }
        src.extend(b"--rel--\r\n");
        let email = parse(&src).unwrap();
        assert_eq!(
            texts(&email)[1..],
            ["See below.", "Org chart", "Ann Smith", ""]
        );
        assert_eq!(
            figures(&email).iter().map(|f| f.1).collect::<Vec<_>>(),
            ["figure-0-1", "figure-0-2"]
        );
        assert_eq!(email.media.len(), 2);
    }

    /// A message of plain `text`, then `parts` (headers, bytes) as inline
    /// files.
    fn with_parts(text: &str, parts: &[(&str, &str)]) -> Email {
        let mut src = format!(
            "From: a@example.org\r\nSubject: s\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"rel\"\r\n\r\n\
--rel\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{text}\r\n"
        );
        for (headers, bytes) in parts {
            src.push_str(&format!("--rel\r\n{headers}\r\n\r\n{bytes}\r\n"));
        }
        src.push_str("--rel--\r\n");
        parse(src.as_bytes()).unwrap()
    }

    /// Each image's key, and the bytes and file its key gives back.
    fn keyed(email: &Email) -> Vec<(String, String, usize)> {
        email
            .media
            .iter()
            .map(|m| {
                let part = m.source.part().unwrap();
                let bytes = String::from_utf8(email.media_bytes(part).unwrap().to_vec()).unwrap();
                (
                    part.to_string(),
                    bytes,
                    email.media_attachment(part).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn two_images_of_one_file_name_are_two_images() {
        // Pasted images a client calls alike, with no Content-ID: each is
        // read on its own, and its key gives back its own bytes.
        let image =
            "Content-Type: image/png\r\nContent-Disposition: inline; filename=\"image.png\"";
        let email = with_parts("See below.", &[(image, "ALPHA"), (image, "BRAVO")]);
        assert_eq!(
            keyed(&email),
            [
                ("attachment 1".into(), "ALPHA".into(), 0),
                ("attachment 2".into(), "BRAVO".into(), 1)
            ]
        );
        assert_eq!(fluree_doc_model::pictures(email.media.clone()).len(), 2);
    }

    #[test]
    fn two_images_of_one_content_id_are_two_images() {
        // A Content-ID two parts carry names neither: the body's mark is
        // drawn as the first, and the second, unmarked, after the message;
        // each is read by its place among the files. A Content-ID no other
        // part has still keys its part.
        let dup = |name: &str| {
            format!("Content-Type: image/png\r\nContent-Disposition: inline; filename=\"{name}\"\r\nContent-ID: <x@example.org>")
        };
        let (a, b) = (dup("a.png"), dup("b.png"));
        let own =
            "Content-Type: image/png\r\nContent-Disposition: inline\r\nContent-ID: <y@example.org>";
        let email = with_parts(
            "One [cid:x@example.org] two [cid:y@example.org]",
            &[(&a, "ALPHA"), (&b, "BRAVO"), (own, "CHARLIE")],
        );
        assert_eq!(
            keyed(&email),
            [
                ("attachment 1".into(), "ALPHA".into(), 0),
                ("cid:y@example.org".into(), "CHARLIE".into(), 2),
                ("attachment 2".into(), "BRAVO".into(), 1)
            ]
        );
        assert_eq!(fluree_doc_model::pictures(email.media.clone()).len(), 3);
    }

    #[test]
    fn an_image_marked_twice_is_read_once() {
        // Apple Mail's mark of one file, twice: one image, drawn twice.
        let image =
            "Content-Type: image/png\r\nContent-Disposition: inline; filename=\"image.png\"";
        let email = with_parts(
            "Look <image.png>\r\n\r\nAgain <image.png>",
            &[(image, "ALPHA")],
        );
        let f: Vec<&str> = figures(&email).iter().map(|f| f.1).collect();
        assert_eq!(f, ["figure-0-1", "figure-0-1"]);
        assert_eq!(keyed(&email), [("attachment 1".into(), "ALPHA".into(), 0)]);
        let pictures = fluree_doc_model::pictures(email.media.clone());
        assert_eq!(pictures.len(), 1);
    }
}
