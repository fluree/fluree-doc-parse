# DoCO JSON-LD

```bash
fdoc convert report.pdf -f doco
fdoc convert report.pdf -f doco --base-iri https://example.org/docs/report
fdoc convert report.pdf -f doco --doc-iri https://example.org/docs/report
```

The richest output: a JSON-LD graph with explicit section containment, table
cells as addressable nodes, character offsets, and page/bbox provenance. It is
insertable into a [Fluree](https://flur.ee) ledger as-is.

## The context

```json
{
  "@context": {
    "doc":      "https://ns.flur.ee/doc#",
    "doco":     "http://purl.org/spar/doco/",
    "nif":      "http://persistence.uni-leipzig.org/nlp2rdf/ontologies/nif-core#",
    "po":       "http://www.essepuntato.it/2008/12/pattern#",
    "po:contains": { "@type": "@id" },
    "rdfs":     "http://www.w3.org/2000/01/rdf-schema#",
    "dcterms":  "http://purl.org/dc/terms/",
    "xsd":      "http://www.w3.org/2001/XMLSchema#"
  },
  "@graph": [ … ]
}
```

One Fluree namespace, `doc:`, plus three public ontologies, `rdfs` for the
display label, and Dublin Core (`dcterms`) for what a document declares about
itself, with `xsd` typing its dates. `doco` is the Document Components Ontology; `po` is the Pattern
ontology DoCO extends, and `po:contains` is the containment property DoCO
itself specifies; `nif` is the NLP Interchange Format, whose character offsets
are the join point with annotation and NER tooling. Everything
Fluree-specific — placement, evidence, table cells, structure hints — lives
under `doc:`. See the full [vocabulary](../reference/vocabulary.md).

`po:contains` is IRI-coerced, so containment edges are real references rather
than strings — which is what makes the graph traversable after insertion.

## An element

```json
{
  "@id": "urn:fluree-doc-parse:report/element/2",
  "@type": "doco:Paragraph",
  "doc:bbox": "91.17,185.64,145.00,195.09",
  "doc:evidence": "layout",
  "doc:pageIndex": 0,
  "doc:xhtmlTag": "p",
  "nif:beginIndex": 0,
  "nif:endIndex": 5,
  "nif:isString": "小田切 亘",
  "rdfs:label": "小田切 亘"
}
```

| term | what it carries |
|---|---|
| `nif:isString` | the full element text |
| `rdfs:label` | a display preview, capped at 100 chars |
| `nif:beginIndex` / `nif:endIndex` | char offsets into [`-f text`](text.md) |
| `doc:pageIndex` | 0-based physical page — [not the printed number](../reference/vocabulary.md#why-pageindex-and-not-pagenumber) |
| `doc:bbox` | `"x0,y0,x1,y1"`, PDF units, top-left origin |
| `doc:evidence` | [which signal classified it](../concepts/provenance.md) |
| `doc:xhtmlTag` | the equivalent HTML tag |
| `po:contains` | children, for `doco:Document`, `BodyMatter`, `Section`, `Table` |
| `doc:sectionLevel` | heading depth, on `doco:Section` |

`doc:bbox` is absent for sources without geometry — see [Measured vs
declared structure](../concepts/geometry-vs-declared.md).

## The document node carries page geometry

```json
{ "@id": "urn:fluree-doc-parse:report/element/0",
  "@type": "doco:Document",
  "doc:pages": { "@type": "@json",
                 "@value": [ { "pageIndex": 0, "width": 612.0, "height": 792.0 },
                             { "pageIndex": 1, "width": 612.0, "height": 792.0,
                               "folio": "2" } ] } }
```

A `doc:bbox` cannot be placed on a rendered page without these: the consumer
needs the ratio between the page's own units and the pixels it rendered to,
and this is the only place that denominator appears. Sources with no geometry
— Markdown, DOCX — omit the key rather than reporting a zeroed size.

The size is the page *as displayed*. A page that displays sideways — a
`/Rotate 90` page, a landscape table bound into a portrait document — is read
in the frame where its text is upright, and its elements' boxes are mapped
back to the displayed frame, so a box and a page render always agree.

`folio` is the page number printed on the page, where the document numbers
its pages: the one piece of [running furniture](../design/furniture.md) that
identifies a page rather than the document. A string, because front matter is
`iv`. Absent on pages that carry no page number.

## What the pages say about the document

```json
{ "@type": "doco:Document",
  "doc:runningText": { "@type": "@json",
                       "@value": [ "CHURCH &", "DWIGHT",
                                   "TM004361 Rev:005 Production" ] } }
```

The header and footer text [furniture detection](../design/furniture.md)
removed from the body, kept once here.

Repetition is what makes a line noise *inside* the body and exactly what makes
it identify the document. A controlled procedure puts its owner, title and
number in a block on every page — on a three-page one, all three — so removing
the repetition removes the identity, and the document comes out anonymous.

Kept on the document node rather than in the text, because putting it back in
the body would restore what was removed from it and shift every character
offset in the graph. Bare page numbers are excluded: a folio identifies
nothing about the document — it identifies a page, and lives on the page's
entry in `doc:pages`.

## Pages nothing read

```json
{ "@type": "doco:Document",
  "doc:unreadPages": { "@type": "@json",
                       "@value": [ { "pageIndex": 0, "reason": "NearBlank" } ] } }
```

Present only when a page carries content the output does not hold — a scan, a
vector drawing, glyphs whose Unicode cannot be trusted — and no reader
supplied it. Without this an empty page and an unread page look identical;
one document of engineering drawings produced 126 bytes of XHTML with nothing
saying 99% of it was missing.

`reason` is the router's verdict: `Scanned`, `NearBlank` or `BrokenText`.
Escalating the page clears the entry, because then something did read it.

Deliberately not an element. A marker element would have to carry text to be
visible, and inventing text puts characters into the projection that every
`nif:beginIndex` in the graph is counted against.

## Sections are explicit

The flat element list becomes a tree here. A `doco:Section` node is minted per
heading, carrying `doc:sectionLevel` and containing the title plus everything
under it:

```json
{ "@id": "urn:fluree-doc-parse:report/section/2",
  "@type": "doco:Section",
  "doc:sectionLevel": 1,
  "po:contains": [ ".../element/3", ".../element/4", ".../element/5" ] }
```

Sections and elements share one counter in emission order, so IRIs are
`{base}/section/{n}` and `{base}/element/{n}` with `n` never reused.

## Table cells are nodes

Each cell is addressable, with its headers denormalized onto it:

```json
{ "@id": "urn:fluree-doc-parse:report/element/6",
  "@type": "doc:TableCell",
  "doc:cellValue": "1",
  "doc:columnHeader": "a",
  "doc:rowHeader": "…",
  "doc:columnIndex": 0,
  "doc:rowIndex": 0 }
```

This is what lets a query ask for "the Supply voltage row of the LM358B
column" without the consumer reconstructing the grid. Merged cells are
denormalized first, so every cell stands on its own.

Because a cell is a node rather than a position in a grid, there is no merge
shape this format fails to represent. That is not true of
[`xhtml`](xhtml.md), where a region HTML cannot tile is dropped along with its
text — so where completeness matters more than fidelity to the drawn spans,
this is the format to read.

## Transcripts say who spoke and when

```json
{
  "@type": "doco:Paragraph",
  "doc:speaker": "Ada Park",
  "doc:startMs": 272404,
  "doc:endMs": 279512,
  "doc:evidence": "vtt",
  "nif:beginIndex": 146,
  "nif:endIndex": 193,
  "nif:isString": "Ada Park: I want to say this is my last review."
}
```

A turn read from a [transcript](../inputs/transcripts.md) carries its
speaker and where it sits in the recording, in place of the `doc:bbox` a
page would give it. The times are integers, in milliseconds from the start
of the recording, so "what was said in the fifth minute" is a numeric
comparison in any query language.

`doc:speaker` is the name as the file writes it, unresolved: `Speaker 2`
stays `Speaker 2`. The same name opens `nif:isString`, followed by `": "`, so
the speaker's mention spans `nif:beginIndex` to `nif:beginIndex` plus the
length of the name. That is the span to link to a person record. The times
never appear in the text, so they never become entities.

## Emails are threads of messages

```json
{ "@type": "doco:Document",
  "dcterms:title": "RE: Pilot",
  "dcterms:creator": ["Lena Holt <lena@example.com>"],
  "dcterms:created": { "@value": "2026-07-17T13:48:00-05:00", "@type": "xsd:dateTime" },
  "doc:attachments": { "@type": "@json",
                       "@value": [ { "filename": "quote.pdf",
                                     "contentType": "application/pdf", "size": 48213 } ] } }

{ "@id": "urn:fluree-doc-parse:reply/message/7",
  "@type": "doc:Message",
  "doc:from": ["urn:fluree-doc-parse:reply/mailbox/2"],
  "doc:to":   ["urn:fluree-doc-parse:reply/mailbox/3"],
  "doc:sentAt": { "@value": "2026-07-17T13:48:00-05:00", "@type": "xsd:dateTime" },
  "doc:subject": "RE: Pilot",
  "doc:messageId": "3@example.com",
  "doc:inReplyTo": ["2@example.com"],
  "po:contains": [ "…/element/8", "…/element/9" ] }

{ "@id": "urn:fluree-doc-parse:reply/mailbox/2",
  "@type": "doc:Mailbox",
  "doc:address": "lena@example.com",
  "doc:name": "Lena Holt" }
```

An [email](../inputs/email.md) is split into its messages: the file's own
and each one quoted in its body. Each is a `doc:Message` under the body,
side by side in reading order rather than nested as the quoting nests them,
so every message in a thread is one step from its sender. A message
contains its elements, starting with the paragraph that holds its header as
the text shows it.

Every sender and recipient is a `doc:Mailbox`, one per address in the
document, so every message a person sent or received points at the same
node. `doc:address` is the value to join on when linking a mailbox to a
contact record. Mailboxes are minted per document, like every other node,
so re-extracting one email never touches another's. A mailbox written with
a name and no address, as a quoted Outlook header often has, joins the
addressed mailbox of the same name when the document has one.

A quoted message's `doc:sentAt` has no offset, because the line that quotes
it states none. The file's own always has one.

A message that goes on after a quote nested in it, as a signature set below
the quoted thread does, goes on in its own `doc:Message`: the elements after
the quote are contained by the message they belong to, not by the one quoted
before them.

```json
{ "@id": "urn:fluree-doc-parse:reply/signature/31",
  "@type": "doc:Signature",
  "doc:signer": ["urn:fluree-doc-parse:reply/mailbox/4"],
  "nif:isString": "Best,\nKai\n\nKai Moreno\nExample Data Inc.\n1 Main Street, Springfield",
  "nif:beginIndex": 1180, "nif:endIndex": 1243,
  "po:contains": [ "…/element/32", "…/element/33" ] }
```

Each message's [signature](../inputs/email.md#signatures) is a
`doc:Signature` inside its `doc:Message`, containing the signature's
elements, with `doc:signer` pointing at the message's senders. Its span
covers its elements' text in the projection. A place named in a signature
is the signer's: an address under a name is that sender's office, not the
office of a company the message talks about, and `doc:signer` is the join
that keeps it there.

## Links are nodes

A hyperlink becomes its own node, referenced from the element whose text
carries it. A node rather than a property, because one paragraph can hold
several links and each has its own anchor — flattened onto the element they
would be a set of targets with no way to tell which words point where.

```json
{ "@id": "urn:fluree-doc-parse:report/element/12",
  "@type": "doco:Paragraph",
  "doc:link": [ "urn:fluree-doc-parse:report/link/13" ],
  "nif:beginIndex": 1866, "nif:endIndex": 1910,
  "nif:isString": "Learn more at www.example.org/plan" }

{ "@id": "urn:fluree-doc-parse:report/link/13",
  "@type": "doc:Link",
  "doc:linkTarget": { "@id": "https://www.example.org/plan" },
  "nif:beginIndex": 1880, "nif:endIndex": 1910,
  "nif:isString": "www.example.org/plan" }
```

| term | what it carries |
|---|---|
| `doc:linkTarget` | an address outside the document, IRI-coerced |
| `doc:linkPage` | a jump inside the document: 0-based page index |
| `nif:isString` | the anchor text |
| `nif:beginIndex` / `nif:endIndex` | the anchor's offsets into [`-f text`](text.md) |

Exactly one of `doc:linkTarget` and `doc:linkPage` is present. `doc:link` and
`doc:linkTarget` are both IRI-coerced in the context, so a link ingests as a
reference you can follow rather than a string about one — which is what lets a
query ask which documents point at a domain.

The anchor's offsets are in the same space as every other offset in the graph,
so the interval lookup that finds an entity mention finds a link anchor too.
They are absent where the annotation covers something with no text of its own,
and on tables, whose projection joins cells with tabs — an offset into the
element's own text would index nothing there.

## IRIs and re-extraction

`--base-iri` sets the namespace for minted IRIs. Default:
`urn:fluree-doc-parse:<stem>`.

`--doc-iri` stamps every element with `doc:sourceDocument → <iri>`. That tag
is what a re-extraction retracts by: delete everything pointing at the
document IRI, insert the new graph, and the ledger holds exactly one
extraction of that document without a diff.

```bash
fdoc convert report.pdf -f doco \
  --base-iri https://example.org/docs/report/v2 \
  --doc-iri  https://example.org/docs/report
```

See [Loading into a Fluree ledger](../integration/ledger-ingest.md).

## Pairing with text

`nif:beginIndex` / `nif:endIndex` index into the string
[`-f text`](text.md) produces — not into the Markdown, and not into
`nif:isString` concatenated. Generate both from the same run.
