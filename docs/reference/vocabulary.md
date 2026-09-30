# Vocabulary

Every term the [DoCO output](../formats/doco.md) emits.

## Namespaces

| prefix | IRI |
|---|---|
| `doc` | `https://ns.flur.ee/doc#` |
| `doco` | `http://purl.org/spar/doco/` |
| `nif` | `http://persistence.uni-leipzig.org/nlp2rdf/ontologies/nif-core#` |
| `po` | `http://www.essepuntato.it/2008/12/pattern#` |
| `rdfs` | `http://www.w3.org/2000/01/rdf-schema#` |
| `dcterms` | `http://purl.org/dc/terms/` |
| `xsd` | `http://www.w3.org/2001/XMLSchema#` |

`doco`, `nif` and `po` are public ontologies — the Document Components
Ontology, the NLP Interchange Format, and the Pattern ontology. `po:contains`
is not optional decoration: DoCO is defined as an extension of the Pattern
ontology, and that is the containment property it specifies. `rdfs` carries
one term, `rdfs:label`, rather than an ontology of its own. `dcterms` is
Dublin Core, for what a document says about itself: its title, creator and
dates. `xsd` types those dates, so a store compares them as times.

`doc` is the single Fluree namespace, covering what those four do not.

A [record](../inputs/records.md) read by a source format adds terms of its
own: the class and the properties the format declares. They belong to the
model the format was written for, and are written as absolute IRIs, under
no prefix of this context.

## Types

| type | what it is |
|---|---|
| `doco:Document` | the root |
| `doco:BodyMatter` | the body partition |
| `doco:Section` | a minted node per heading, containing its subtree |
| `doco:SectionTitle` | the heading itself |
| `doco:Paragraph` | prose |
| `doco:List` / `doco:ListItem` | lists |
| `doco:Table` | a table; contains its cells |
| `doco:Figure` | [anchor](../integration/anchors.md) placeholders for escalated regions |
| `doc:TableCell` | one cell of a table |
| `doc:Link` | one hyperlink, with its anchor and target |
| `doc:Message` | one message of an [email](../inputs/email.md); contains its elements |
| `doc:Mailbox` | a sender or recipient, one per address in the document |

That is the complete set — thirteen types, and no others are emitted. Notably
**`doco:Caption` and `doco:FrontMatter` are not produced.** DoCO defines both
and an earlier design assigned them, but caption classification measured
−0.0004 against the benchmark twice (its ground truth blesses prominent
captions as headings often enough that the recall bias punishes excluding
them), so the detector exists in `heading.rs` and is not wired in. Page
furniture is [dropped rather than partitioned](../design/furniture.md), so
nothing becomes `doco:FrontMatter` either.

A consumer should therefore not branch on either type expecting to see it.

**A declared class.** A document read from a
[record](../inputs/records.md) whose source format declares a
`documentClass` has two types: `doco:Document` and the declared class, as an
absolute IRI.

```json
"@type": [ "doco:Document", "https://example.org/model#NewsArticle" ]
```

The class stands beside `doco:Document` and not in its place: the structure
is a document's, whatever the record describes. With no declared class,
`@type` stays the plain string `"doco:Document"`.

## Properties

**Text and identity**

| property | on | value |
|---|---|---|
| `nif:isString` | text elements | the full text |
| `rdfs:label` | text elements | display preview, ≤100 chars |
| `nif:beginIndex` | text elements | char offset into [`-f text`](../formats/text.md) |
| `nif:endIndex` | text elements | end offset, exclusive |

**Placement and provenance**

| property | on | value |
|---|---|---|
| `doc:pageIndex` | all | 0-based physical page, slide, or sheet — see below |
| `doc:bbox` | PDF elements | `"x0,y0,x1,y1"`, PDF units, top-left origin |
| `doc:evidence` | all | [which signal classified it](../concepts/provenance.md) |
| `doc:sourcePath` | elements read from a [record](../inputs/records.md) or a [media asset](../inputs/media-assets.md) | the field or track the element was read from: `/record/body`, `/text/0`, `Stratum:CLOSED_CAPTION` |
| `doc:sourceDocument` | all, with `--doc-iri` | the document IRI to retract by |
| `doc:pages` | `doco:Document` | JSON literal: `[{pageIndex, width, height, folio?}]`, PDF units as displayed; `folio` is the printed page number where there is one |
| `doc:unreadPages` | `doco:Document` | JSON literal: `[{pageIndex, reason}]` — content nothing transcribed |
| `doc:runningText` | `doco:Document` | JSON literal: the header/footer text stripped from the body |
| `doc:attachments` | `doco:Document` | JSON literal: `[{filename?, contentType, size, inline?}]`, the files the document carries |
| `doc:sourceFields` | `doco:Document` | JSON literal: `[{path, role, value?, truncated?, property?, iri?}]`, the fields of the [record](../inputs/records.md#what-every-output-carries) the document was read from, in the record's order |
| `dcterms:title` | `doco:Document` | the title the document declares: an email's subject, the field a source format names as `title` |
| `dcterms:creator` | `doco:Document` | who made it, as the document names them: an email's sender |
| `dcterms:created` / `dcterms:modified` | `doco:Document` | `xsd:dateTime`: when it was made, or an email sent. From a record, the fields a source format names as `created` and `modified`, and an `xsd:date` where the record states a day and no time |

**Transcript turns**

| property | on | value |
|---|---|---|
| `doc:speaker` | [transcript](../inputs/transcripts.md) turns that name one | the speaker as the file names them, unresolved |
| `doc:startMs` | transcript turns; the turns and section titles of a [media asset](../inputs/media-assets.md) | integer, milliseconds from the start of the recording to the turn's or the section's start |
| `doc:endMs` | the same | integer, milliseconds from the start of the recording to the turn's or the section's end |

**Declared properties**

| property | on | value |
|---|---|---|
| the `property` of a `metadata` entry, an absolute IRI | `doco:Document` | a plain string, or `{"@value": …, "@type": <datatype>}` where the entry declares a datatype and the value fits it |
| the `property` of an `enums` entry, an absolute IRI | `doco:Document` | `{"@id": <concept>}` for a value the declared list holds, a plain string for one it does not |

These are the statements a [source format](../inputs/records.md#the-declaration)
declares. A property stated more than once has an array of its values, in
the order the record gives them. A declared property never replaces what
the emitter states itself, such as `@id`, `@type` and the Dublin Core
terms.

**Structure**

| property | on | value |
|---|---|---|
| `po:contains` | `Document`, `BodyMatter`, `Section`, `Table` | children (IRI-coerced) |
| `doc:sectionLevel` | `doco:Section` | heading depth, 1–6 |
| `doc:figure` | `doco:Figure` | shared id for fragments of one drawing |
| `doc:link` | any text-bearing element | its hyperlinks (IRI-coerced) |
| `doc:xhtmlTag` | most | the equivalent HTML tag |

**Links**

| property | value |
|---|---|
| `doc:linkTarget` | an address outside the document, IRI-coerced |
| `doc:linkPage` | a jump inside the document: 0-based page index |
| `nif:isString` | the anchor text |
| `nif:beginIndex` / `nif:endIndex` | the anchor's offsets into the text projection |

Exactly one of `doc:linkTarget` and `doc:linkPage` appears on a `doc:Link`.
The offsets are absent where the annotation covers something with no text of
its own — an image, a whole table cell.

**Email messages**

| property | on | value |
|---|---|---|
| `doc:from`, `doc:to`, `doc:cc`, `doc:bcc` | `doc:Message` | mailbox nodes (IRI-coerced) |
| `doc:sentAt` | `doc:Message` | `xsd:dateTime`: with an offset for the file's own message, without for a quoted one |
| `doc:subject` | `doc:Message` | the subject line |
| `doc:messageId` | `doc:Message` | `Message-ID`, without angle brackets |
| `doc:inReplyTo`, `doc:references` | `doc:Message` | the identifiers of the messages it answers and the thread before it |
| `doc:quoted` | `doc:Message` | `true` for a message quoted or forwarded inside another |
| `doc:address` | `doc:Mailbox` | the email address, as written |
| `doc:name` | `doc:Mailbox` | the display name, as written |

**Table cells**

| property | value |
|---|---|
| `doc:cellValue` | the cell's text |
| `doc:rowIndex` | 0-based row |
| `doc:columnIndex` | 0-based column |
| `doc:rowHeader` | the row's header text, denormalized |
| `doc:columnHeader` | the column's header text, denormalized |
| `doc:sectionLabel` | the enclosing sub-header band's text, if any |

## Why `pageIndex` and not `pageNumber`

`doc:pageIndex` is the 0-based physical position in the page sequence, and
deliberately not the printed page number. A printed number is authored
metadata: title pages and inserts carry none, front matter is often numbered
in roman numerals, and the folio commonly runs at an offset from the physical
position. It is a label that may be absent, repeat, or disagree with itself.
The physical index is always defined and always unique, and it is what a
renderer needs to fetch the page.

The same field carries the slide index for PPTX and the sheet index for
XLSX, and `0` for paginationless sources, which `pageNumber` would
misdescribe.

## IRI shapes

```
{base_iri}/element/{n}      elements and cells
{base_iri}/section/{n}      minted section nodes
```

One counter shared across both, in emission order, so `n` is never reused
within a document. `base_iri` defaults to `urn:fluree-doc-parse:<stem>` and is set
with `--base-iri`.

## What is not emitted

`provenance` (which engine) appears in [`-f json`](../formats/json.md) but not
in the graph. The `doc:evidence` values `route` and `page-tier` are the
model-tier markers available in DoCO.
