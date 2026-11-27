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

The elements after it, up to the next element that opens a message or
returns to one, are that message's body. A message can go on after a quote
nested in it (see [below](#text-after-a-quote)); the element where it does
carries `resumes`, the `id` of the element that opened it.

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

Clients quote in two ways, and both are read, alone or mixed in one thread:

- **Attribution and `>`**, as Gmail, Apple Mail and most others write it:
  an `On … wrote:` line (possibly wrapped over two or three lines), then the
  quoted text with `>` before each line, nested a level deeper for each
  earlier reply. Gmail writes the attribution outside the quote; Apple Mail
  writes it inside, as the quote's first line. The `>` marks are removed.
- **Header blocks**, as Outlook writes it: an optional separator
  (`-----Original Message-----`, a line of underscores,
  `---------- Forwarded message ---------`, `Begin forwarded message:`),
  then `From:`, `Sent:` or `Date:`, `To:`, `Cc:` and `Subject:` lines, with
  the earlier message under them unmarked. Outlook sets the separator
  straight under the reply's last line, and wraps a long recipient list
  over several lines; both are read.

A thread that passed through both kinds of client carries both: an Apple
Mail reply quotes an Outlook thread as a run of header blocks inside its
`>` quote, and each block is a message of its own, with its own sender,
date and subject.

Quoted text with nothing to say whose it is, a `>` quote with no
attribution above it, is a quoted message all the same. Its `message` says
only that it is quoted.

In HTML, a quote is what the markup marks as one: a `<blockquote>` with
`type="cite"` (Apple Mail, Thunderbird), Gmail's `gmail_quote`, and the
containers Yahoo and Proton put around a quote. Gmail's indent button also
writes a `<blockquote>`, with no quotation in it, and is not taken for one.

All of these are recognised in English, German, French, Spanish and Dutch.
Messages appear in reading order, newest first, the way a reply sets them.

### Text after a quote

A message does not always end where the quote in it starts. Gmail can set a
reply's signature below the message it quotes, and a mail server adds its
footer below everything:

```
> On Sep 29, 2026, at 5:05 PM, Kai Moreno <kai@example.com> wrote:
>
> Is the ontology included?
>
> On Tue, Sep 29, 2026 at 1:01 PM Lena Holt <lena@example.com> wrote:
>> The brief is attached.
>
> --
> Kai Moreno, Senior Director
> 1 Main Street, Springfield
```

The signature is Kai's, though it comes after Lena's message. Text that
returns to a shallower quote level, or out of a quote altogether, returns
to the message open at that level. The element where it does carries
`resumes`, and in DoCO its elements go back into that message's
`doc:Message`. A reply written below the quote it answers is the sender's
own text, the same way.

Outlook quotes without marks, so its header blocks follow one another at
one level, and nothing in a thread of them says where a message would go
on. Text after the last block is the last message's.

## Signatures

Each message's signature is marked: the sign-off, the name, title and
company under it, phones, the office address, and a legal footer. Every
element in it carries `signature: true`. In DoCO the elements sit in a
`doc:Signature` inside the message, with `doc:signer` pointing at the
message's sender.

```
Thanks so much,

-Ada Park
Executive Director, Learner Network
Example State University
```

A signature belongs to whoever sent the message it ends, quoted messages
included, so each signature in a thread is its own sender's. That is what
lets a consumer keep what a signature says, an office address above all,
on the sender and their organisation, rather than on whatever company the
message discusses.

A signature is found from the end of each message, by the lines that open
one:

- `-- `, which sets a signature apart by convention. Everything after it is
  signature.
- A sign-off (`Thanks,`, `Best regards`, `Mit freundlichen Grüßen`,
  `Cordialement`) with a name under it, or on its line (`Thanks, Kai`).
- The sender's name on a line of its own, as the header gives it, or their
  first name with a surname: `Kai`, `Kai Moreno`, `Moreno, Kai`.
- A name after a dash: `-Kai`, `— Kai`.

The lowest of these lines is in the signature, and the signature runs up
from it over blank lines, rules and more of the same: `Best,` over `Kai`
over a blank line over `Kai Moreno` is one signature. It runs down to the
end of the message, or to a paragraph of prose: terms pasted under a
sign-off are not part of it, though a legal footer below them is, as a
second signature. A message with none of these lines but a legal footer at
its end has that footer for its signature.

A message does not start with its signature unless it is little else: a
notification that opens with the sender's name has a masthead, not a
signature. A message's text after a quote nested in it can start with its
signature, and often does.

## Bodies

A message sent as both plain text and HTML (`multipart/alternative`, which
is what Gmail and Outlook send) is read from its **plain text**. The plain
text states the quoting outright, while the HTML states it in markup that
each client writes differently, and both carry the same words. A message
sent only as HTML is read by the [HTML reader](office-and-web.md#html),
and split into messages the same way.

Some senders, mostly billing and notification systems, build the plain
text by stripping the tags from the HTML and stopping there. The style
sheet's rules and the character references (`03&#47;14&#47;2026`) are left
in it as text, or whole runs of markup are. Such a plain text is set aside
and the HTML is read instead. Markup only counts when the HTML does not show
it as text, so a message that writes `&amp;` or `<td>` on purpose, as one
about markup might, keeps its plain text.

In plain text, a blank line ends a paragraph, and the line breaks inside a
paragraph are kept, so a signature's name, title and company stay on
separate lines. Lines starting with `-`, `*`, `•` or `1.` are list items.
`format=flowed` text is rejoined where the sender's client wrapped it.
Placeholders for inline images, Outlook's `[cid:image001.png@…]` and Apple
Mail's `<image001.png>`, are not text: they are removed, and the image they
name is drawn there (see [Images in the body](#images-in-the-body)). So are
the zero-width characters clients leave at the ends of lines.

Bodies are decoded from base64 and quoted-printable, and from their declared
charset. Text labelled `us-ascii` or `iso-8859-1` that is valid UTF-8 is
read as UTF-8, since that mislabelling is common. Headers encoded as
`=?utf-8?Q?…?=`, and attachment names encoded as `filename*=utf-8''…`,
are decoded.

## Attachments

An attachment is a document of its own, so its content is **not** part of
this output. Attachments are described instead:

- In DoCO, on the document node as `doc:attachments`: filename, content
  type, size in bytes, the SHA-256 of its bytes, and whether the file is
  shown inline in the body (an image pasted into the message).
- On stderr, `fdoc` lists the files it left out. An inline image read into
  the body (see [Images in the body](#images-in-the-body)) is not one of
  them: the note counts it apart, as read.

`--attachments DIR` saves each email's files under `DIR/<email name>/`, the
name without its extension (two inputs of one name in a batch keep theirs,
then are numbered), so you can convert them on their own:

```bash
fdoc convert reply.eml --attachments ./att
fdoc convert ./att/reply/
```

A saved attachment converted on its own has that same SHA-256 as its
document's `doc:sha256`, which is the join from an email to the documents it
carried. The same file forwarded in fifty threads has one hash, whatever it
was called each time.

A message forwarded *as an attachment* comes back as an `.eml` of its own,
including one embedded in an Outlook `.msg`. Signature parts
(`application/pkcs7-signature`, `application/pgp-signature`) are skipped.
Attachment names never contain a directory: `../../report.pdf` is
`report.pdf`. A name `--parse-attachments` could write a conversion under
(64 hexadecimal digits, alone or before a `.`) is saved with a `_` before
it, so no file the sender names passes for another's conversion.

In Rust, `fluree_doc_email::parse` returns the attachments beside the
elements, as bytes with their description, for your own pipeline to read:

```rust
let email = fluree_doc_email::parse(&bytes)?;
for file in &email.attachments {
    // file.info.filename, file.info.content_type, file.info.sha256, file.bytes
}
let notes = email.notes(); // document info and attachment descriptions, for the emitters
```

### Reading the attachments too

`--parse-attachments` (with `--attachments DIR`) also converts each file,
in the format asked for, into `DIR/<email name>/<sha256>.<format>` beside
the saved files: one output per file by its bytes, however often and under
whatever names it was sent, so the output's name is the join to
`doc:attachments`. A message attached to the message is converted too, and
its own attachments followed, three levels deep. Each file is converted as
`convert` would convert it on its own, but as a document of its own: the
flags naming the e-mail's document (`--doc-iri`, `--base-iri`,
`--source-name`) are not its. An image the body shows is read with the body
(see below), not again; another image only when something is configured to
read it. At most 200 files, of 64 MiB each and 256 MiB in all, are
converted; one note on stderr counts what was and says why each other was
not (`no reader`, its error, `TooLarge`, `TooMany`).

```bash
fdoc convert reply.eml --attachments ./att --parse-attachments -f doco
```

In Rust, `fluree_doc_email::parse_attachments` does the walk with a reader
you supply, so this crate still compiles no other reader:

```rust
let children = fluree_doc_email::parse_attachments(
    &email,
    &fluree_doc_email::AttachmentLimits::default(), // depth 3, 64 MiB, 256 MiB, 200 files
    &mut |file, depth| match file.info.content_type.as_str() {
        "text/csv" => Some(fluree_doc_csv::parse(&file.bytes).map_err(|e| e.to_string())),
        _ => None, // no reader: `Outcome::NoReader`
    },
);
for c in &children {
    // c.info (sha256 is the key), c.depth, c.parent (the attached message's sha256), c.outcome
}
```

## Images in the body

An image the message shows in its body — a screenshot pasted into a reply,
a scanned form, a signature's logo — is one of its files, listed with
`inline: true`, and it is also drawn where the body shows it: a
`doco:Figure`, `figure-0-<n>` in the order the body first shows its images,
after the paragraph, list item or table holding it. The body says where by
Outlook's `[cid:…]` or Apple Mail's `<name.png>` in plain text, or by
`<img src="cid:…">` in HTML, matched to the file by its `Content-ID` (or
its name): the one written exactly so, else one written in another case.
An image shown twice is one image, its figure drawn twice.

A figure's text is the image's alternative text (`alt`), where the HTML
gives it one, and nothing where the plain text marks it. A signature's
icons carry such text (`LinkedIn`, `Logo`), which would otherwise read as
words of nearly every corporate message: `fdoc convert` leaves out the
alternative text of an image under the floor a model is sent (below; a
GIF's or a BMP's size read from its header too), and keeps its figure. In
Markdown and plain text a figure with no words prints nothing; in the
graph, JSON and XHTML it still marks where the image is shown. In Rust,
`email.elements` keeps every figure's alternative text, for a caller to
judge. With no reader configured, each image the body shows is a figure all
the same, the elements after it numbered one further, and the graph lists
it in [`doc:unreadImages`](../formats/doco.md#images-nothing-read)
(`NotRead`).

Its text is read as a DOCX picture's is (see
[Office files](office-and-web.md#docx)): by OCR (`--ocr`, `--ocr-words`,
`<stem>_m<n>.json`), by a model's reading supplied (`--tier-results`,
`<stem>_m<n>.json`) or by the configured model, and placed right after its
figure. `fdoc dev render-routed` writes each
image as `<stem>_m<n>.png`, `kind: "media"`, reason `embedded_image`
(`small_image` under the floor a model is sent, below), with
its `figure` and its `part`: the `cid:` URL the message names it by, or
`attachment <n>` (its place in `doc:attachments`, from 1) where no
Content-ID names that part alone. Each part is one image, whatever its file
name: two pasted images a client called `image.png` alike are read apart. An
image nothing read is counted on stderr and listed in
[`doc:unreadImages`](../formats/doco.md#images-nothing-read); one no engine
decodes, such as a signature's GIF or BMP logo, is told by its first bytes
whatever is configured, and listed there as `UnsupportedFormat`, but neither
counted nor warned about, since nearly every signed message carries one.

With a model configured and no OCR, an image the body shows is sent to it
as a document's picture is ([the rules](office-and-web.md#docx)): when its
header reads, when it has at least 400 pixels on its longer side and 48 on
its shorter (a message has no page to measure it against, and a small
signature logo, a social icon or a rule is under that floor), and when the
pixel probe takes it for text: a capture of a few lines on a wide screen, a
one-line capture of a form, a labelled chart or a phone's photograph of a
page are sent, each a request the provider bills, and each image leaving
the machine. A photograph, a logo drawn large or an illustration the probe
does not take for text is not sent, and listed as `NotTexty`, not counted on
stderr either. A small dialog box can be under the floor (380 pixels wide):
an image not sent is listed in `doc:unreadImages` as `Small`, and not
counted in the note on stderr, since a PNG logo is under the floor on nearly
every signed message; `--verbose` names each, and `--ocr` reads them.
`escalation.max_crops` caps how many a message sends, and `--no-escalate`
sends none. With `--ocr` or `--ocr-words`, only an image OCR read no text in
is sent; with `--tier-results`, none is. OCR reads every image, whatever the
probe makes of it, and words written for an image (`--ocr-words`) and a
reading supplied (`--tier-results`) are taken whatever it shows: neither
costs a call.

Where the body does not mark it — the plain text is read and only the HTML
version shows the image, as most clients send it — an inline image is drawn
at the end of the file's own message, before anything it quotes. An image
the HTML shows by its `cid:` is inline whatever its part's disposition
says, wherever the message nests its `multipart/related` (inside the
`multipart/alternative` or around it): some libraries mark a related image
`attachment` for its file name. The `cid:` is read as the HTML body reads
it (RFC 2392): `CID:` as `cid:`, `%40` as `@`, and an image the HTML hides
shows nothing; it is matched to its part as a figure's is, so a message
reads the same whichever version is its body. A mark
naming no image the message carries is dropped as before: a reply keeps the
`[cid:…]` of images its quoted messages no longer hold. A message without
images converts as it did.

In Rust, `email.media` lists the images (figure, `cid:` or `attachment <n>`
part),
`email.media_bytes(&m)` gives one's bytes, and
`fluree_doc_image::place_after_figures` puts what was read after its
figure.

## Document metadata

The file's own message is the document's metadata:

| DoCO, on `foaf:Document` | from |
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
one, or from the HTML when markup was left in the plain text, as with
`.eml`. A `.msg` whose body exists only as compressed RTF, which is rare, is
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
