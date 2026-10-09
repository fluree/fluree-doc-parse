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
    "po:containsAsHeader": { "@type": "@id" },
    "rdfs":     "http://www.w3.org/2000/01/rdf-schema#",
    "dcterms":  "http://purl.org/dc/terms/",
    "foaf":     "http://xmlns.com/foaf/0.1/",
    "xsd":      "http://www.w3.org/2001/XMLSchema#"
  },
  "@graph": [ … ]
}
```

One Fluree namespace, `doc:`, for what no standard ontology defines, plus
three public ontologies, `rdfs` for the display label, Dublin Core
(`dcterms`) for what a document declares about itself, with `xsd` typing its
dates, and FOAF for the root's class, `foaf:Document`. `doco` is the Document Components Ontology; `po` is the Pattern
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
  "@id": "urn:fluree-doc-parse:report-661511bb2b30-element-2",
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
| `po:contains` | children, for `foaf:Document`, `BodyMatter`, `Section`, `Table` |
| `doc:sectionLevel` | heading depth, on `doco:Section` |

`doc:bbox` is absent for sources without geometry — see [Measured vs
declared structure](../concepts/geometry-vs-declared.md).

## The document node says what was read

```json
{ "@id": "urn:fluree-doc-parse:report-661511bb2b30-element-0",
  "@type": "foaf:Document",
  "doc:sha256": "661511bb2b30c4e8a9f2d71b05e3c6a48f90d2b17e5a3c8f4b6d1e09a7c25f3e",
  "doc:sourceName": "report.pdf" }
```

`doc:sha256` is the input's bytes as lowercase hex SHA-256, always present.
The same file found twice — two copies on a drive, one file under two names —
has one value, so duplicates are a query rather than a guess. It identifies
bytes, not text: a PDF exported again, or a document saved again, is a new
hash even where nothing it says has changed.

`doc:sourceName` is what the caller calls the input: the file name `fdoc
convert` was given, or `--source-name`. Standard input has none. A name is
not an identifier — names repeat, and change when a file is moved — which is
why the hash is the one to join on.

What the file declares about itself rides beside them, in Dublin Core:

```json
{ "@type": "foaf:Document",
  "dcterms:title": "Quarterly Report",
  "dcterms:creator": ["Ada Park"],
  "dcterms:created": { "@value": "2019-07-12T15:10:45-06:00", "@type": "xsd:dateTime" },
  "dcterms:modified": { "@value": "2019-07-12T15:11:13-06:00", "@type": "xsd:dateTime" } }
```

From a PDF's information dictionary, an Office file's core properties, an
HTML page's `<title>` and author, or an email's headers. Declared, never
inferred: a title guessed from a page's largest line would be a reading of
the page, and the [elements](#sections-are-explicit) are where that goes. A
date that is not a real one is left out, since typed as `xsd:dateTime` it
would fail a store's insert.

## The document node carries page geometry

```json
{ "@id": "urn:fluree-doc-parse:report-661511bb2b30-element-0",
  "@type": "foaf:Document",
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
{ "@type": "foaf:Document",
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
{ "@type": "foaf:Document",
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
under it. The title is the section's header, held by `po:containsAsHeader` as
DoCO specifies, and by `po:contains` with the rest, so everything a section
holds is one hop:

```json
{ "@id": "urn:fluree-doc-parse:report-661511bb2b30-section-2",
  "@type": "doco:Section",
  "doc:sectionLevel": 1,
  "po:containsAsHeader": "…-element-3",
  "po:contains": [ "…-element-3", "…-element-4", "…-table-5" ] }
```

A title the file declares for itself — Word's Title style, a deck's title
slide — is a `doco:Title`, and the document node's header in the same way:

```json
{ "@type": "foaf:Document",
  "po:containsAsHeader": "…-element-2",
  "po:contains": [ "…-element-1", "…-element-2" ] }

{ "@id": "…-element-2", "@type": "doco:Title", "nif:isString": "Annual Report" }
```

It opens no section: what follows it up to the first heading is the body's.
A document has one title, so a second reads as a top-level heading. A PDF
declares no title on its pages, so its graph has none: a title guessed from
the largest line would be a reading of the page, and stays a heading.

`po:contains` is a set, as JSON-LD arrays are. Reading order is
`nif:beginIndex`, which every node holding text carries; DoCO's own
suggestion, a linked list of items, would be a node per item in every
container.

Every node of a document shares one counter in emission order, so IRIs are
`{base}-section-{n}`, `{base}-element-{n}`, `{base}-table-{n}` and so on, with
`n` never reused. See [IRIs and re-extraction](#iris-and-re-extraction) for
the base.

## Lists contain their items

A run of list items is a `doco:List` containing one `doco:Paragraph` per
item, with `doc:xhtmlTag` `li`. DoCO has no list item class: a list's members
are typed as what they are, and being contained by the list is what makes
them members. Their order is their `nif:beginIndex`.

```json
{ "@type": "doco:List", "po:contains": [ "…-element-7", "…-element-8" ] }
{ "@id": "…-element-7", "@type": "doco:Paragraph", "doc:xhtmlTag": "li",
  "nif:isString": "Sign the order" }
```

The element model marks an item `doc:ListItem`, and [`-f json`](json.md)
shows that mark, because a flat list of elements has no list to be contained
by.

## Table cells are nodes

Each cell is addressable, with its headers denormalized onto it:

```json
{ "@id": "urn:fluree-doc-parse:report-661511bb2b30-cell-6",
  "@type": "doc:TableCell",
  "doc:cellValue": "1",
  "doc:columnHeader": "a",
  "doc:rowHeader": "…",
  "doc:columnIndex": 0,
  "doc:rowIndex": 0,
  "nif:beginIndex": 212, "nif:endIndex": 213 }
```

This is what lets a query ask for "the Supply voltage row of the LM358B
column" without the consumer reconstructing the grid. Merged cells are
denormalized first, so every cell stands on its own.

A cell's offsets slice the projection to its `doc:cellValue`, which is its
text, so it carries no `nif:isString` repeating it. A value copied into a cell
by a merge has no place in the projection and no offsets.

Where the source stores a typed value under the text it shows — a
[workbook](../inputs/office-and-web.md#xlsx) does — the cell also carries it,
at full precision:

```json
{ "@type": "doc:TableCell",
  "doc:cellValue": "12%",
  "doc:cellDatum": { "@value": "0.12345", "@type": "xsd:decimal" } }
```

`doc:cellValue` stays what the author saw, and what the text projection reads;
`doc:cellDatum` is what to compute with. Other formats store text, and their
cells have none: a type guessed from text is a reading of the page, and
`45584888` may be an order number, `03/04/2026` either of two days.

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
{ "@type": "foaf:Document",
  "dcterms:title": "RE: Pilot",
  "dcterms:creator": ["Lena Holt <lena@example.com>"],
  "dcterms:created": { "@value": "2026-07-17T13:48:00-05:00", "@type": "xsd:dateTime" },
  "doc:attachments": { "@type": "@json",
                       "@value": [ { "filename": "quote.pdf",
                                     "contentType": "application/pdf", "size": 48213,
                                     "sha256": "c41e09a7…" } ] } }

{ "@id": "urn:fluree-doc-parse:reply-5c0e2a91d7b3-message-7",
  "@type": "doc:Message",
  "doc:from": ["urn:fluree-doc-parse:reply-5c0e2a91d7b3-mailbox-2"],
  "doc:to":   ["urn:fluree-doc-parse:reply-5c0e2a91d7b3-mailbox-3"],
  "doc:sentAt": { "@value": "2026-07-17T13:48:00-05:00", "@type": "xsd:dateTime" },
  "doc:subject": "RE: Pilot",
  "doc:messageId": "3@example.com",
  "doc:inReplyTo": ["2@example.com"],
  "po:contains": [ "…-element-8", "…-element-9" ] }

{ "@id": "urn:fluree-doc-parse:reply-5c0e2a91d7b3-mailbox-2",
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
{ "@id": "urn:fluree-doc-parse:reply-5c0e2a91d7b3-signature-31",
  "@type": "doc:Signature",
  "doc:signer": ["urn:fluree-doc-parse:reply-5c0e2a91d7b3-mailbox-4"],
  "nif:isString": "Best,\nKai\n\nKai Moreno\nExample Data Inc.\n1 Main Street, Springfield",
  "nif:beginIndex": 1180, "nif:endIndex": 1243,
  "po:contains": [ "…-element-32", "…-element-33" ] }
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
{ "@id": "urn:fluree-doc-parse:report-661511bb2b30-element-12",
  "@type": "doco:Paragraph",
  "doc:link": [ "urn:fluree-doc-parse:report-661511bb2b30-link-13" ],
  "nif:beginIndex": 1866, "nif:endIndex": 1910,
  "nif:isString": "Learn more at www.example.org/plan" }

{ "@id": "urn:fluree-doc-parse:report-661511bb2b30-link-13",
  "@type": "doc:Link",
  "doc:linkTarget": { "@value": "https://www.example.org/plan", "@type": "xsd:anyURI" },
  "nif:beginIndex": 1880, "nif:endIndex": 1910,
  "nif:isString": "www.example.org/plan" }
```

| term | what it carries |
|---|---|
| `doc:linkTarget` | an address outside the document, as an `xsd:anyURI` literal |
| `doc:linkPage` | a jump inside the document: 0-based page index |
| `nif:isString` | the anchor text |
| `nif:beginIndex` / `nif:endIndex` | the anchor's offsets into [`-f text`](text.md) |

Exactly one of `doc:linkTarget` and `doc:linkPage` is present. `doc:link` is
IRI-coerced in the context, so an element's links are nodes you can follow.
The target is a literal, not a node: a store that keys namespaces on an IRI's
path, as Fluree does, would otherwise keep one for every directory of every
address a corpus links to, and which ones is up to the documents. Which
documents point at a domain is a string test on it:

```sparql
SELECT DISTINCT ?doc
WHERE { ?link doc:linkTarget ?target ; doc:sourceDocument ?doc .
        FILTER (STRSTARTS(STR(?target), "https://www.example.org/")) }
```

The anchor's offsets are in the same space as every other offset in the graph,
so the interval lookup that finds an entity mention finds a link anchor too.
They are absent where the annotation covers something with no text of its own,
and on tables, whose projection joins cells with tabs — an offset into the
element's own text would index nothing there.

## IRIs and re-extraction

Every node IRI is the base IRI, a `-`, the kind of node and its number:
`urn:fluree-doc-parse:report-661511bb2b30-element-12`. The base is, in order:

1. `--base-iri`, when given;
2. else `--doc-iri`, so a document's nodes are named after it;
3. else `urn:fluree-doc-parse:`, the file's stem with anything outside
   letters, digits, `.`, `_` and `~` turned into `-`, and the first twelve hex
   digits of the file's SHA-256 (just the digits, from standard input).

The hash is there because names repeat: `report.pdf` and `report.docx` share a
stem, and so does every `report.docx` on a shared drive. Nodes minted under one
base are one set of nodes in a store, so two documents under the same base
would merge.

Nothing minted adds a `/`, `#` or `:` to the base, so every node of a document
lands in the namespace its base already sits in. Fluree splits an IRI into a
namespace and a name at the last of those, and encodes each namespace once; a
whole corpus minted this way costs one namespace, not one per document. A base
ending in `/`, `#` or `:` keeps its own namespace per document, and the `-` is
left off: `https://example.org/doc/7f3a/` mints
`https://example.org/doc/7f3a/element-12`.

`--doc-iri` stamps nodes with `doc:sourceDocument → <iri>`. That tag is what a
re-extraction retracts by: delete everything pointing at the document IRI,
insert the new graph, and the ledger holds exactly one extraction of that
document without a diff. Table cells are not stamped: a cell is always one
`po:contains` below its table, which is, and cells are most of a table-heavy
graph.

```bash
fdoc convert report.pdf -f doco --doc-iri urn:doc:finance-q3-report
```

See [Loading into a Fluree ledger](../integration/ledger-ingest.md).

## Pairing with text

`nif:beginIndex` / `nif:endIndex` index into the string
[`-f text`](text.md) produces — not into the Markdown, and not into
`nif:isString` concatenated. Generate both from the same run.
