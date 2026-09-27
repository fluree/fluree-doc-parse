# Email: .eml and .msg

```bash
fdoc convert reply.eml -f doco
fdoc convert reply.msg -f md
fdoc convert reply.eml --attachments ./attachments
```

An `.eml` is a message as it travels between mail servers (RFC 5322 and
MIME); every client can save one. A `.msg` is Outlook's own format. Both
are read into the same elements.

An email is usually a thread. A reply carries the earlier messages in its
body, each written by someone else, at another time, to other people. Read
as one body, every claim in it would belong to whoever sent the last reply.
So the body is split into its messages:

```
From: Lena Holt <lena@example.com>
To: Kai Moreno <kai@example.com>
Date: Fri, 17 Jul 2026 13:48:00 -0500
Subject: RE: Pilot

The credit is fair.

On Fri, Jul 17, 2026 at 10:05 AM Kai Moreno <kai@example.com> wrote:

We will apply a credit of $15,000.
```

Each message opens with its header and continues with its body. The header
stays **in the text**, the way a printed thread shows it, so an extractor
reading the text sees who wrote each part. There is no geometry
(`bbox` is absent and `page` is `0`), and nothing to escalate.

## Messages

The element that opens each message carries its header as structured
fields, in `message`:

| field | meaning |
|---|---|
| `from`, `to`, `cc`, `bcc` | mailboxes: `{name?, address?}` |
| `date` | when it was sent, ISO 8601 |
| `subject` | the subject line |
| `message_id`, `in_reply_to`, `references` | message identifiers, without angle brackets |
| `quoted` | `true` for a message quoted or forwarded in another's body |

The elements after it, up to the next element with a `message`, are that
message's body.

- **The file's own message** takes its header from the file's headers. Its
  `date` keeps the sender's UTC offset: `2026-07-17T13:48:00-05:00`.
- **A quoted message** takes its header from the text that introduces it,
  and is as complete as that text is. An attribution line (`On Fri, Jul 17,
  2026 at 10:05 AM Kai Moreno <kai@example.com> wrote:`) gives a sender and
  a time. An Outlook header block (`From:`, `Sent:`, `To:`, `Subject:`) gives
  more. Its `date` has no offset, because the quote does not state one.

In [DoCO](../formats/doco.md#emails-are-threads-of-messages), each message
is a `doc:Message` node containing its elements, and each sender or
recipient is a `doc:Mailbox` node you can join to a contact by its
address.

## Quoted and forwarded messages

Clients quote in two ways, and both are read:

- **Attribution and `>`**, as Gmail, Apple Mail and most others write it:
  an `On … wrote:` line (possibly wrapped over two or three lines), then the
  quoted text with `>` before each line, nested a level deeper for each
  earlier reply. The `>` marks are removed.
- **Header blocks**, as Outlook writes it: an optional separator
  (`-----Original Message-----`, a line of underscores,
  `---------- Forwarded message ---------`), then `From:`, `Sent:` or
  `Date:`, `To:`, `Cc:` and `Subject:` lines, with the earlier message under
  them unmarked.

Both forms are recognised in English, German, French, Spanish and Dutch.
Messages appear in reading order, newest first, the way a reply sets them.
Text that follows a quote at the same level, as in a reply written below
the quote, is kept but is counted as part of the quoted message.

## Bodies

A message sent as both plain text and HTML (`multipart/alternative`, which
is what Gmail and Outlook send) is read from its **plain text**. The plain
text states the quoting outright, while the HTML states it in markup that
each client writes differently, and both carry the same words. A message
sent only as HTML is read by the [HTML reader](office-and-web.md#html),
and split into messages the same way.

In plain text, a blank line ends a paragraph, and the line breaks inside a
paragraph are kept, so a signature's name, title and company stay on
separate lines. Lines starting with `-`, `*`, `•` or `1.` are list items.
`format=flowed` text is rejoined where the sender's client wrapped it.
Outlook's `[cid:image001.png@…]` placeholders for inline images are
removed.

Bodies are decoded from base64 and quoted-printable, and from their declared
charset. Text labelled `us-ascii` or `iso-8859-1` that is valid UTF-8 is
read as UTF-8, since that mislabelling is common. Headers encoded as
`=?utf-8?Q?…?=`, and attachment names encoded as `filename*=utf-8''…`,
are decoded.

## Attachments

An attachment is a document of its own, so its content is **not** part of
this output. Attachments are described instead:

- In DoCO, on the document node as `doc:attachments`: filename, content
  type, size in bytes, and whether the file is shown inline in the body
  (an image pasted into the message).
- On stderr, `fdoc` lists the files it left out.

`--attachments DIR` saves each email's files under `DIR/<email name>/` so
you can convert them on their own:

```bash
fdoc convert reply.eml --attachments ./att
fdoc convert ./att/reply/
```

A message forwarded *as an attachment* comes back as an `.eml` of its own,
including one embedded in an Outlook `.msg`. Signature parts
(`application/pkcs7-signature`, `application/pgp-signature`) are skipped.
Attachment names never contain a directory: `../../report.pdf` is
`report.pdf`.

In Rust, `fluree_doc_email::parse` returns the attachments beside the
elements, as bytes with their description, for your own pipeline to read:

```rust
let email = fluree_doc_email::parse(&bytes)?;
for file in &email.attachments {
    // file.info.filename, file.info.content_type, file.bytes
}
let notes = email.notes(); // document info and attachment descriptions, for the emitters
```

## Document metadata

The file's own message is the document's metadata:

| DoCO, on `doco:Document` | from |
|---|---|
| `dcterms:title` | `Subject` |
| `dcterms:creator` | `From` |
| `dcterms:created` | `Date` |

## Outlook .msg

A `.msg` that Outlook received keeps the message's original internet
headers, and those are read first. One that Outlook sent or saved as a
draft has no internet headers, so it is read from Outlook's own
properties. Its date is then in UTC. Senders inside an Exchange
organisation are given their SMTP address, not their internal directory
name.

The body is read from the plain text Outlook stores beside the formatted
one. A `.msg` whose body exists only as compressed RTF, which is rare, is
read as having no body; its header and attachments are still read.

## Detection

An email is recognised by its **content**, so a message saved as `.txt`,
or with no extension, still reads as one:

- **.msg**: an OLE compound file with Outlook's message properties at its
  root. Word, Excel and PowerPoint's old binary formats share the container
  and are not mistaken for mail.
- **.eml**: header fields from the first line, among them a sender with a
  date, subject or recipient, or a mail route (`Received:`, `Message-ID:`).

Files named `.eml` or `.msg` are read as email whatever their content, and
need only one message header to be read. The same content check applies to
`fdoc convert -` on stdin.

## Errors

A file with no message headers is not an email: `EmailError::NotAnEmail`.
A compound file that is not an Outlook message, or cannot be read, is
`EmailError::Msg`. A message with no body is not an error; its header and
attachments are still read.
