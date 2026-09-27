//! Correspondence: who a message is from and to, and when it was sent.

/// An address a message is from or to, as the message writes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Mailbox {
    /// The display name: `Ada Park` in `Ada Park <ada@example.com>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The address. Absent where only a name was written, as in the header
    /// block a reply quotes from Outlook, which often shows names alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

impl Mailbox {
    /// The mailbox as a header writes it: `Ada Park <ada@example.com>`, or
    /// whichever half there is.
    pub fn display(&self) -> String {
        match (&self.name, &self.address) {
            (Some(n), Some(a)) => format!("{n} <{a}>"),
            (Some(n), None) => n.clone(),
            (None, Some(a)) => a.clone(),
            (None, None) => String::new(),
        }
    }
}

/// The header of one message: the file's own, or one quoted in its body.
///
/// A reply carries the thread below it, and each message in that thread was
/// written by someone else, at another time, to other people. Read as one
/// body, every claim in it belongs to whoever sent the last reply. So a
/// reader splits the thread, and each message opens with an element carrying
/// this header.
///
/// A quoted message's header is read from the text of the quote, and is as
/// complete as the quote was: `On Fri, Jul 17, 2026 at 10:05 AM Ada Park
/// <ada@example.com> wrote:` gives a sender and a time and nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Message {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub from: Vec<Mailbox>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<Mailbox>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<Mailbox>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bcc: Vec<Mailbox>,
    /// When it was sent, ISO 8601. With a UTC offset where the source states
    /// one, as a `Date:` header does; without, where it does not, as a quoted
    /// `On … wrote:` line does not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// `Message-ID`, without its angle brackets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// `In-Reply-To`: the messages this one answers.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub in_reply_to: Vec<String>,
    /// `References`: the thread this one belongs to, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    /// Quoted or forwarded inside another message's body, rather than the
    /// file's own.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub quoted: bool,
}
