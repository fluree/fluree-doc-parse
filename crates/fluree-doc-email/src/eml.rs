//! RFC 5322 messages: the headers, the MIME tree under them, and which of
//! its parts is the body.

use crate::mime::{self, Headers, Params};
use crate::{address, date, markup, text, AttachedFile, Body, EmailError, Read};
use fluree_doc_model::budget::Budget;
use fluree_doc_model::Message;

/// Deep enough for any real message; a bound so a hostile one cannot
/// exhaust the stack.
const MAX_DEPTH: usize = 16;

pub fn read(bytes: &[u8]) -> Result<Read, EmailError> {
    let (head, body) = mime::split(bytes);
    let headers = Headers::parse(&mime::header_text(head));
    // Told it is a message, take one message header as enough: a bare MIME
    // entity or a hand-written note is still read. Recognising one by its
    // content asks for more (`looks_like`).
    let any = [
        "from",
        "to",
        "subject",
        "date",
        "message-id",
        "mime-version",
        "content-type",
        "received",
    ];
    if !any.iter().any(|n| headers.get(n).is_some()) {
        return Err(EmailError::NotAnEmail);
    }
    let mut found = Found::default();
    // One budget for every HTML version of the message, each read once or
    // twice: twenty alternatives of 150 KB, each with a budget of its own,
    // took 12 s.
    let mut budget = Budget::default();
    walk(&headers, body, &mut found, 0, &mut budget);
    let own = message(&headers);
    let header = crate::header_text(&own, headers.get("date"));
    let body = match (found.plain, found.html) {
        (Some(p), h) => {
            found.set_aside.extend(h);
            Body::Plain(p)
        }
        (None, Some(h)) => Body::Html(h),
        (None, None) => Body::None,
    };
    // The images an HTML version the body is not shows: shown in the
    // message all the same (see `assemble`). One too large to read shows
    // images no one can name.
    let mut shown = Vec::new();
    let mut shown_unread = false;
    for h in &found.set_aside {
        match fluree_doc_html::cid_ids_within(h, &mut budget) {
            Ok(ids) => shown.extend(ids),
            Err(_) => shown_unread = true,
        }
    }
    Ok(Read {
        own,
        header,
        body,
        attachments: found.attachments,
        shown,
        shown_unread,
        budget,
        provenance: "eml",
    })
}

/// Does this header name a sender or a route, as a message's does?
fn is_message(h: &Headers) -> bool {
    let has = |n: &str| h.get(n).is_some();
    (has("from") && (has("date") || has("subject") || has("to")))
        || has("received")
        || has("message-id")
        || (has("mime-version") && has("content-type"))
}

/// Does the file open like a message: header fields from its first line,
/// and among them a sender or a mail route?
pub fn looks_like(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(16 * 1024)];
    let (h, _) = mime::split(head);
    let text = mime::header_text(h);
    let Some(first) = text.lines().next() else {
        return false;
    };
    let field = first.split_once(':').is_some_and(|(n, _)| {
        !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    });
    let headers = Headers::parse(&text);
    (field || first.starts_with("From ")) && headers.len() >= 2 && is_message(&headers)
}

/// The own message's header, read.
pub fn message(h: &Headers) -> Message {
    let boxes = |n: &str| h.get(n).map(address::list).unwrap_or_default();
    Message {
        from: boxes("from"),
        to: boxes("to"),
        cc: boxes("cc"),
        bcc: boxes("bcc"),
        date: h.get("date").and_then(date::loose),
        subject: h
            .get("subject")
            .map(|s| collapse(&mime::decode_words(s)))
            .filter(|s| !s.is_empty()),
        message_id: h.get("message-id").and_then(|v| ids(v).into_iter().next()),
        in_reply_to: h.get("in-reply-to").map(ids).unwrap_or_default(),
        references: h.get("references").map(ids).unwrap_or_default(),
        quoted: false,
    }
}

/// Message identifiers without their angle brackets.
fn ids(v: &str) -> Vec<String> {
    let found: Vec<String> = v
        .split('<')
        .skip(1)
        .filter_map(|s| s.split_once('>').map(|(id, _)| id.trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect();
    if found.is_empty() && !v.trim().is_empty() {
        return vec![v.trim().to_string()];
    }
    found
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Default)]
struct Found {
    plain: Option<String>,
    html: Option<String>,
    attachments: Vec<AttachedFile>,
    /// The HTML versions read that are not the body (an alternative to
    /// the one chosen): they still say which images the message shows.
    set_aside: Vec<String>,
}

fn append(slot: &mut Option<String>, text: String) {
    match slot {
        Some(s) => {
            s.push_str("\n\n");
            s.push_str(&text);
        }
        None => *slot = Some(text),
    }
}

/// Walk a part: a multipart's parts in turn, a text part into the body, and
/// anything else into the attachments.
fn walk(h: &Headers, body: &[u8], found: &mut Found, depth: usize, budget: &mut Budget) {
    if depth > MAX_DEPTH {
        return;
    }
    let ct = Params::parse(h.get("content-type").unwrap_or("text/plain"));
    let kind = if ct.value.contains('/') {
        ct.value.as_str()
    } else {
        "text/plain"
    };
    let cd = h.get("content-disposition").map(Params::parse);
    let filename = cd
        .as_ref()
        .and_then(|d| d.get("filename"))
        .or_else(|| ct.get("name"))
        .and_then(crate::base_name);
    // Only a disposition makes a text part a file: one without, or marked
    // inline, is shown in the message by every client, whatever it is named.
    let attached = cd.as_ref().is_some_and(|d| d.value == "attachment");
    let decoded = || mime::transfer_decode(body, h.get("content-transfer-encoding"));

    if let Some(sub) = kind.strip_prefix("multipart/") {
        let Some(boundary) = ct.get("boundary") else {
            return;
        };
        let parts: Vec<(Headers, &[u8])> = mime::multipart(body, boundary)
            .into_iter()
            .map(|p| {
                let (ph, pb) = mime::split(p);
                (Headers::parse(&mime::header_text(ph)), pb)
            })
            .collect();
        if sub == "alternative" {
            // The same message several ways; one is the body. Files inside
            // any of them (the images an HTML version shows) are kept.
            let mut alts: Vec<Found> = parts
                .iter()
                .map(|(ph, pb)| {
                    let mut f = Found::default();
                    walk(ph, pb, &mut f, depth + 1, budget);
                    f
                })
                .collect();
            // The plain text, unless a sender's converter left markup in it.
            let plain = alts.iter().position(|f| f.plain.is_some());
            let html = alts.iter().rposition(|f| f.html.is_some());
            let use_html = match (plain, html) {
                (Some(p), Some(h)) => {
                    let (p, h) = (&alts[p].plain, &alts[h].html);
                    let (p, h) = (p.as_deref().unwrap_or(""), h.as_deref().unwrap_or(""));
                    markup::unrendered(p, h, budget)
                }
                (p, _) => p.is_none(),
            };
            let chosen = if use_html { html } else { plain };
            for (i, alt) in alts.iter_mut().enumerate() {
                if Some(i) == chosen {
                    if use_html {
                        append(&mut found.html, alt.html.take().unwrap_or_default());
                    } else {
                        append(&mut found.plain, alt.plain.take().unwrap_or_default());
                    }
                }
                found.set_aside.extend(alt.html.take());
                found.attachments.append(&mut alt.attachments);
                found.set_aside.append(&mut alt.set_aside);
            }
            return;
        }
        for (ph, pb) in &parts {
            walk(ph, pb, found, depth + 1, budget);
        }
        return;
    }
    // A signature proves who sent the message and says nothing in it.
    if matches!(
        kind,
        "application/pgp-signature"
            | "application/pkcs7-signature"
            | "application/x-pkcs7-signature"
    ) {
        return;
    }
    if matches!(kind, "text/plain" | "text/html") && !attached {
        let content = mime::decode_charset(&decoded(), ct.get("charset"));
        if kind == "text/html" {
            append(&mut found.html, content);
        } else {
            let flowed = ct
                .get("format")
                .is_some_and(|f| f.eq_ignore_ascii_case("flowed"));
            let delsp = ct
                .get("delsp")
                .is_some_and(|d| d.eq_ignore_ascii_case("yes"));
            append(
                &mut found.plain,
                if flowed {
                    text::unflow(&content, delsp)
                } else {
                    content
                },
            );
        }
        return;
    }
    let bytes = decoded();
    // A message forwarded as an attachment is named by its subject.
    let filename = filename.or_else(|| {
        (kind == "message/rfc822").then(|| {
            let (inner, _) = mime::split(&bytes);
            let subject = Headers::parse(&mime::header_text(inner))
                .get("subject")
                .map(|s| collapse(&mime::decode_words(s)))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "message".into());
            format!("{subject}.eml")
        })
    });
    let inline = match &cd {
        Some(d) => d.value == "inline",
        None => h.get("content-id").is_some(),
    };
    found.attachments.push(
        AttachedFile::new(filename, kind.to_string(), inline, bytes)
            .with_content_id(h.get("content-id")),
    );
}
