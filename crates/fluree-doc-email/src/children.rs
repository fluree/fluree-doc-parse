//! An e-mail's attachments read as documents of their own, on request.
//!
//! Attachments come back from [`crate::parse`] as bytes, described: each is
//! a document for its own reader, and this crate reads only e-mail. An
//! embedder that wants them read writes the same recursion every time, so
//! it is here, with the bounds an untrusted message needs: how deep a
//! message forwarded inside a message is followed, how large a file and
//! how many bytes in all are handed to a reader, how many files. The
//! reading itself is the caller's ([`parse_attachments`] takes it as a
//! function), so this crate still compiles no other reader.

use crate::{AttachedFile, Email, Format};
use fluree_doc_model::Attachment;

/// The bounds [`parse_attachments`] reads within.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentLimits {
    /// How deep attachments are followed: 1 reads the message's own, 2 also
    /// those of a message attached to it, and so on.
    pub max_depth: usize,
    /// The largest file handed to the reader, in bytes.
    pub max_bytes: usize,
    /// The most bytes handed to the reader in all.
    pub max_total: usize,
    /// The most files handed to the reader.
    pub max_count: usize,
}

impl Default for AttachmentLimits {
    /// Three levels, 64 MiB a file, 256 MiB and 200 files in all.
    fn default() -> Self {
        AttachmentLimits {
            max_depth: 3,
            max_bytes: 64 << 20,
            max_total: 256 << 20,
            max_count: 200,
        }
    }
}

/// One file an e-mail carries, at any depth, and what came of reading it.
#[derive(Debug, Clone, PartialEq)]
pub struct Child<T> {
    /// Its description; `info.sha256` is its key: a file carried twice,
    /// at any depth and under any name, is one child.
    pub info: Attachment,
    /// 1 for the message's own attachments, 2 for those of a message
    /// attached to it, and so on.
    pub depth: usize,
    /// The SHA-256 of the attached message carrying it, `None` for the
    /// message's own.
    pub parent: Option<String>,
    pub outcome: Outcome<T>,
}

/// What came of one file.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<T> {
    /// The reader read it.
    Read(T),
    /// The reader has nothing for its kind of file.
    NoReader,
    /// The reader failed on it, saying why.
    Failed(String),
    /// Not handed to the reader: `TooLarge` (past `max_bytes`, or past
    /// `max_total` with what was read before it) or `TooMany` (past
    /// `max_count`).
    Skipped(&'static str),
}

/// A reader of attached files, as [`parse_attachments`] takes it: given a
/// file and its depth, what it read, its error, or `None` where it has no
/// reader for the file's kind.
pub type ReadAttachment<'r, T> = dyn FnMut(&AttachedFile, usize) -> Option<Result<T, String>> + 'r;

/// Read the files `email` carries with `read`, and those of every message
/// attached to it, down to `limits.max_depth`, in the order they are
/// found, each once by its SHA-256.
///
/// `read` is given each file and its depth, and returns `None` where it has
/// no reader for it. An attached message (`.eml`, `.msg`) is given to
/// `read` like any file, and its own attachments are then followed here,
/// taken from it without making its body into elements again (an HTML body
/// is read for the images it shows, within the message's budget); one that
/// does not read as a message is not followed.
pub fn parse_attachments<T>(
    email: &Email,
    limits: &AttachmentLimits,
    read: &mut ReadAttachment<'_, T>,
) -> Vec<Child<T>> {
    let mut out: Vec<Child<T>> = Vec::new();
    // Each file's SHA-256 once met: looked up, not searched for, so a
    // message of a hundred thousand files costs that many lookups.
    let mut met: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut total = 0usize;
    let mut handed = 0usize;
    // Depth-first, in the order the files are found: each message's
    // attachments right after it.
    let mut stack: Vec<(AttachedFile, usize, Option<String>)> = email
        .attachments
        .iter()
        .rev()
        .map(|f| (f.clone(), 1, None))
        .collect();
    while let Some((file, depth, parent)) = stack.pop() {
        if !met.insert(file.info.sha256.clone()) {
            continue;
        }
        let skipped = if handed >= limits.max_count {
            Some("TooMany")
        } else if file.bytes.len() > limits.max_bytes
            || total.saturating_add(file.bytes.len()) > limits.max_total
        {
            Some("TooLarge")
        } else {
            None
        };
        let outcome = match skipped {
            Some(why) => Outcome::Skipped(why),
            None => {
                handed += 1;
                total += file.bytes.len();
                match read(&file, depth) {
                    None => Outcome::NoReader,
                    Some(Ok(t)) => Outcome::Read(t),
                    Some(Err(e)) => Outcome::Failed(e),
                }
            }
        };
        let followed =
            skipped.is_none() && depth < limits.max_depth && Format::sniff(&file.bytes).is_some();
        if followed {
            // Its files only: its body was the reader's to make, and
            // making it again here would hold a second copy of it beside
            // whatever the reader holds. Its HTML is parsed for the images
            // it shows, which are not files to read, within a budget.
            if let Ok(inner) = crate::attachments_of(&file.bytes) {
                for f in inner.into_iter().rev() {
                    stack.push((f, depth + 1, Some(file.info.sha256.clone())));
                }
            }
        }
        out.push(Child {
            info: file.info,
            depth,
            parent,
            outcome,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message from Alex Sample carrying `parts`, each `(name, type,
    /// body)`, its bodies base64-free (`7bit`).
    fn message(subject: &str, parts: &[(&str, &str, &str)]) -> String {
        let mut m = format!(
            "From: Alex Sample <alex@example.com>\r\nTo: Sam Example <sam@example.com>\r\n\
             Subject: {subject}\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"b-{subject}\"\r\n\r\n\
             --b-{subject}\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n"
        );
        for (name, ty, body) in parts {
            m.push_str(&format!(
                "--b-{subject}\r\nContent-Type: {ty}\r\n\
                 Content-Disposition: attachment; filename=\"{name}\"\r\n\r\n{body}\r\n"
            ));
        }
        m.push_str(&format!("--b-{subject}--\r\n"));
        m
    }

    fn names<T>(children: &[Child<T>]) -> Vec<(Option<&str>, usize)> {
        children
            .iter()
            .map(|c| (c.info.filename.as_deref(), c.depth))
            .collect()
    }

    #[test]
    fn attachments_are_read_down_through_attached_messages_once_each() {
        let inner = message("inner", &[("deep.txt", "text/plain", "Deeper still")]);
        let outer = message(
            "outer",
            &[
                ("notes.txt", "text/plain", "Ledger notes"),
                ("fwd.eml", "message/rfc822", &inner),
                ("again.txt", "text/plain", "Ledger notes"),
                ("blob.bin", "application/octet-stream", "????"),
            ],
        );
        let email = crate::parse(outer.as_bytes()).unwrap();
        let mut seen = Vec::new();
        let children = parse_attachments(
            &email,
            &AttachmentLimits::default(),
            &mut |f: &AttachedFile, depth| {
                seen.push(depth);
                match f.info.content_type.as_str() {
                    "text/plain" => Some(Ok(String::from_utf8_lossy(&f.bytes).trim().to_string())),
                    "message/rfc822" => Some(Ok("a message".to_string())),
                    _ => None,
                }
            },
        );
        // `again.txt` is `notes.txt` again: one child.
        assert_eq!(
            names(&children),
            [
                (Some("notes.txt"), 1),
                (Some("fwd.eml"), 1),
                (Some("deep.txt"), 2),
                (Some("blob.bin"), 1)
            ]
        );
        assert_eq!(children[0].outcome, Outcome::Read("Ledger notes".into()));
        assert_eq!(
            children[2].parent.as_deref(),
            Some(children[1].info.sha256.as_str())
        );
        assert_eq!(children[3].outcome, Outcome::NoReader);
        assert_eq!(seen, [1, 1, 2, 1]);
        // One level only: the attached message is read, not followed.
        let shallow = parse_attachments(
            &email,
            &AttachmentLimits {
                max_depth: 1,
                ..Default::default()
            },
            &mut |_: &AttachedFile, _| Some(Ok(())),
        );
        assert_eq!(shallow.len(), 3);
    }

    #[test]
    fn an_attached_messages_own_images_stay_inline() {
        // The forwarded message's HTML body draws its logo by `cid:`; the
        // part says `attachment`, as a sender's library can for its name.
        // Shown in the message, it is not a file to read.
        let inner = "From: Kai Moreno <kai@example.com>\r\nSubject: inner\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/related; boundary=\"rel\"\r\n\r\n\
             --rel\r\nContent-Type: text/html\r\n\r\n<p>Hello</p><img src=\"cid:logo@example.org\">\r\n\
             --rel\r\nContent-Type: image/png\r\nContent-ID: <logo@example.org>\r\n\
             Content-Disposition: attachment; filename=\"logo.png\"\r\n\r\npixels\r\n--rel--\r\n";
        let outer = message("outer", &[("fwd.eml", "message/rfc822", inner)]);
        let email = crate::parse(outer.as_bytes()).unwrap();
        let children = parse_attachments::<()>(
            &email,
            &AttachmentLimits::default(),
            &mut |_: &AttachedFile, _| Some(Ok(())),
        );
        assert_eq!(
            names(&children),
            [(Some("fwd.eml"), 1), (Some("logo.png"), 2)]
        );
        assert!(children[1].info.inline);
        // As the message read on its own marks it.
        assert!(
            crate::parse(inner.as_bytes()).unwrap().attachments[0]
                .info
                .inline
        );
    }

    #[test]
    fn an_attached_message_too_large_to_make_still_has_its_files_followed() {
        // A body the HTML reader refuses within what is left, as the
        // reader refused the message: its files are followed all the same,
        // and its body is not made a second time to find them.
        let body = "<p>Quarterly figures</p>".repeat(4000);
        let inner = message("inner", &[("deep.txt", "text/plain", "Deeper still")]).replace(
            "Content-Type: text/plain\r\n\r\nSee attached.",
            &format!("Content-Type: text/html\r\n\r\n{body}"),
        );
        let outer = message("outer", &[("fwd.eml", "message/rfc822", &inner)]);
        let email = crate::parse(outer.as_bytes()).unwrap();
        let children = fluree_doc_model::budget::within(64 << 10, || {
            assert!(crate::parse(inner.as_bytes()).is_err());
            parse_attachments::<()>(
                &email,
                &AttachmentLimits::default(),
                &mut |_: &AttachedFile, _| Some(Err("too large".to_string())),
            )
        });
        assert_eq!(
            names(&children),
            [(Some("fwd.eml"), 1), (Some("deep.txt"), 2)]
        );
    }

    #[test]
    fn files_past_the_bounds_are_listed_not_read() {
        let email = crate::parse(
            message(
                "bounds",
                &[
                    ("a.txt", "text/plain", "aaaa"),
                    ("big.txt", "text/plain", &"b".repeat(100)),
                    ("c.txt", "text/plain", "cccc"),
                    ("d.txt", "text/plain", "dddd"),
                ],
            )
            .as_bytes(),
        )
        .unwrap();
        let limits = AttachmentLimits {
            max_bytes: 50,
            max_count: 2,
            ..Default::default()
        };
        let mut read = 0;
        let children = parse_attachments(&email, &limits, &mut |_: &AttachedFile, _| {
            read += 1;
            Some(Ok(()))
        });
        let outcomes: Vec<_> = children.iter().map(|c| c.outcome.clone()).collect();
        assert_eq!(
            outcomes,
            [
                Outcome::Read(()),
                Outcome::Skipped("TooLarge"),
                Outcome::Read(()),
                Outcome::Skipped("TooMany")
            ]
        );
        assert_eq!(read, 2);
        let total = AttachmentLimits {
            max_total: 6,
            ..Default::default()
        };
        let children = parse_attachments(&email, &total, &mut |_: &AttachedFile, _| Some(Ok(())));
        assert_eq!(children[0].outcome, Outcome::Read(()));
        assert_eq!(children[2].outcome, Outcome::Skipped("TooLarge"));
    }
}
