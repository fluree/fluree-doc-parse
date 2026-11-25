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
| `foaf` | `http://xmlns.com/foaf/0.1/` |
| `xsd` | `http://www.w3.org/2001/XMLSchema#` |

`doco`, `nif` and `po` are public ontologies — the Document Components
Ontology, the NLP Interchange Format, and the Pattern ontology. `po:contains`
is not optional decoration: DoCO is defined as an extension of the Pattern
ontology, and that is the containment property it specifies. `rdfs` carries
one term, `rdfs:label`, rather than an ontology of its own. `dcterms` is
Dublin Core, for what a document says about itself: its title, creator and
dates. `foaf` carries one class, `foaf:Document`, for the root. `xsd` types
those dates, so a store compares them as times.

`doc` is the single Fluree namespace, covering what none of those define.

## Types

| type | what it is |
|---|---|
| `foaf:Document` | the root |
| `doco:Title` | the title the document declares for itself, held as the root's header |
| `doco:BodyMatter` | the body partition |
| `doco:Section` | a minted node per heading, containing its subtree |
| `doco:SectionTitle` | the heading itself, held as its section's header |
| `doco:Paragraph` | prose |
| `doco:List` | a list; contains a `doco:Paragraph` per item |
| `doco:Table` | a table; contains its cells |
| `doco:Figure` | a chart or diagram's text, a picture or embedded object in a Word file, and [anchor](../integration/anchors.md) placeholders for escalated regions |
| `doc:TableCell` | one cell of a table |
| `doc:Link` | one hyperlink, with its anchor and target |
| `doc:Message` | one message of an [email](../inputs/email.md); contains its elements |
| `doc:Mailbox` | a sender or recipient, one per address in the document |
| `doc:Signature` | a message's signature; contains its elements |

That is the complete set — fourteen types, and no others are emitted. Every
one is a standard class where a standard one exists. DoCO roots a document in
a bibliographic class such as `fabio:JournalArticle`, which a spreadsheet or
an email is not, so the root is FOAF's general `foaf:Document`. DoCO has no
list item class: a list contains its members, each typed as what it is, and
an item here is the paragraph it holds. Its order among them is its
`nif:beginIndex`. The `doc:` types are the parts no standard ontology names: a
table's cells, a link, a message and its mailboxes and signature. Notably
**`doco:Caption` and `doco:FrontMatter` are not produced.** DoCO defines both
and an earlier design assigned them, but caption classification measured
−0.0004 against the benchmark twice (its ground truth blesses prominent
captions as headings often enough that the recall bias punishes excluding
them), so the detector exists in `heading.rs` and is not wired in. Page
furniture is [dropped rather than partitioned](../design/furniture.md), so
nothing becomes `doco:FrontMatter` either.

A consumer should therefore not branch on either type expecting to see it.

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
| `doc:sourceDocument` | all but table cells, with `--doc-iri` | the document IRI to retract by |
| `doc:sha256` | `foaf:Document` | the input's bytes as lowercase hex SHA-256 |
| `doc:sourceName` | `foaf:Document` | what the caller calls the input: its file name, or `--source-name` |
| `doc:pages` | `foaf:Document` | JSON literal: `[{pageIndex, width, height, folio?}]`, PDF units as displayed; `folio` is the printed page number where there is one |
| `doc:unreadPages` | `foaf:Document` | JSON literal: `[{pageIndex, reason}]` — content nothing transcribed |
| `doc:unreadRegions` | `foaf:Document` | JSON literal: `[{pageIndex, bbox, reason}]` — raster regions of otherwise read pages nothing transcribed; `bbox` as `doc:bbox`; `reason` `Region` (routed), `Small` or `NotTexty` (declined by the router), `Unprobed` (not examined: the run decoded no image, or none that small) |
| `doc:parseWarnings` | `foaf:Document` | JSON literal: `[{pageIndex, kind, count}]` — what the PDF interpreter reported, once per page and kind: `ImageDecodeFailure` |
| `doc:unreadImages` | `foaf:Document` | JSON literal: `[{figure, image, part, reason}]` — images placed in the flow whose text nothing read |
| `doc:ocrReadings` | `foaf:Document` | JSON literal: `[{pageIndex or image, crop, engine?, words, meanConfidence?, minConfidence?}]` — the [OCR readings taken](../formats/doco.md#ocr-readings), with the engine's own confidence: information, not a routing signal |
| `doc:runningText` | `foaf:Document` | JSON literal: the header/footer text stripped from the body |
| `doc:attachments` | `foaf:Document` | JSON literal: `[{filename?, contentType, size, sha256, inline?}]`, the files the document carries |
| `dcterms:title` | `foaf:Document` | the title the file declares: a PDF's `/Title`, an Office file's `dc:title`, an HTML `<title>`, an email's subject |
| `dcterms:creator` | `foaf:Document` | who made it, as the file names them: `/Author`, `dc:creator`, `<meta name="author">`, an email's sender |
| `dcterms:created` / `dcterms:modified` | `foaf:Document` | `xsd:dateTime` (or `xsd:date`): when it was made and last saved, or an email sent |

The `reason` of an image in `doc:unreadImages` is one of those
[the document node describes](../formats/doco.md#images-nothing-read):
`NotRead`, `Missing`, `UnsupportedFormat`, `Failed`, `PastAllowance` for
one past what one document may have read (256 images, 512 million pixels),
`Small` for one the model was not sent, under its floor of pixels, or
`NotTexty` for one the pixel probe does not take for text, or whose OCR
reading was noise. `Small`
and `NotTexty` are listed, and not counted on stderr, as the regions of
`doc:unreadRegions` that carry them are not.

The two lists share these two reasons, and neither is measured the same way
in each. `Small` says in both that no model was sent the image and that an
OCR engine reads it. In `doc:unreadRegions` it is a share of the page: a
region of a PDF page whose pixels look like text, under a tenth of the
page. In `doc:unreadImages` it is a count of pixels: a picture under 400 on
its longer side or 48 on its shorter, wherever it is placed, since an
e-mail has no page to measure it against (a PNG logo is under the floor on
nearly every signed message; `--verbose` names each). So the same capture
can be `Small` in a DOCX and not in a PDF, or the other way round.
`NotTexty` says in both that the pixel probe does not take its pixels for
text, so that nothing read it. In `doc:unreadRegions` that is all it says:
a region of a tenth of the page or more, which no reader is offered (an
OCR reading of a routed or small region that is noise leaves it listed
under its own reason). In `doc:unreadImages` it is also a picture the
probe took for text in which OCR read only noise.

A reading that found no text does not clear the three lists alike, and one
rule says which. `doc:unreadPages` and `doc:unreadRegions` list what the
router took for content (a scan, a raster region the text layer does not
cover) that the output does not hold: a reading, by OCR or by a model, that
came back with no text leaves it listed, since nothing in the output stands
for it. A bare image is read as such a page, so a photograph with no text in
it stays listed (`Scanned`), and warned of. `doc:unreadImages` lists the
pictures a document places that nothing read: no router sends a reader to
them as content (the pixel probe only keeps any reader from those it does
not take for text), and most are logos and screenshots, so any reading
clears the entry, an empty one too, except an OCR reading set aside
(`NotTexty` for noise, `Failed` for words that could not be placed on it).

**Transcript turns**

| property | on | value |
|---|---|---|
| `doc:speaker` | [transcript](../inputs/transcripts.md) turns that name one | the speaker as the file names them, unresolved |
| `doc:startMs` | transcript turns | integer, milliseconds from the start of the recording to the turn's start |
| `doc:endMs` | transcript turns | integer, milliseconds from the start of the recording to the turn's end |

**Structure**

| property | on | value |
|---|---|---|
| `po:contains` | `Document`, `BodyMatter`, `Section`, `Table` | children (IRI-coerced) |
| `po:containsAsHeader` | `foaf:Document`, `doco:Section` | the `doco:Title` or `doco:SectionTitle` heading it, also in `po:contains` (IRI-coerced) |
| `doc:sectionLevel` | `doco:Section` | heading depth, 1–6 |
| `doc:figure` | `doco:Figure` | shared id for fragments of one drawing |
| `doc:link` | any text-bearing element | its hyperlinks (IRI-coerced) |
| `doc:continues` | `doco:Table` | the part of the same table on the page before (IRI-coerced) |
| `doc:xhtmlTag` | most | the equivalent HTML tag |

**Links**

| property | value |
|---|---|
| `doc:linkTarget` | an address outside the document, an `xsd:anyURI` literal |
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
| `doc:signer` | `doc:Signature` | the mailbox nodes of the message's senders (IRI-coerced) |
| `doc:address` | `doc:Mailbox` | the email address, as written |
| `doc:name` | `doc:Mailbox` | the display name, as written |

**Table cells**

| property | value |
|---|---|
| `doc:cellValue` | the cell's text |
| `doc:cellDatum` | what the source stores under that text, typed (`xsd:decimal`, `xsd:date`, `xsd:time`, `xsd:dateTime`, `xsd:boolean`); only where the source declares a type, as a workbook does |
| `nif:beginIndex` / `nif:endIndex` | where `doc:cellValue` sits in the text projection; absent for a value a merge copied in |
| `doc:rowIndex` | 0-based row |
| `doc:columnIndex` | 0-based column |
| `doc:rowHeader` | the row's header text, denormalized |
| `doc:columnHeader` | the column's header text, denormalized; a label spanning columns names those with none of their own; under a stacked header, its labels from the top down joined with ` / ` (`Price / Unit`), each once |
| `doc:sectionLabel` | the label of the group the row belongs to, if any: a sub-header band above it, or a row holding only a label |

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
{base_iri}-element-{n}      the document, its body, and text elements
{base_iri}-section-{n}      minted section nodes
{base_iri}-table-{n}        tables
{base_iri}-cell-{n}         table cells
{base_iri}-link-{n}         hyperlinks
{base_iri}-message-{n}      email messages
{base_iri}-mailbox-{n}      email senders and recipients
{base_iri}-signature-{n}    email signatures
```

One counter shared across all of them, in emission order, so `n` is never
reused within a document. `base_iri` is `--base-iri`, else `--doc-iri`, else
`urn:fluree-doc-parse:<stem>-<first 12 hex digits of the SHA-256>`. A base
ending in `/`, `#` or `:` is followed directly by the kind, without the `-`.
See [IRIs and re-extraction](../formats/doco.md#iris-and-re-extraction).

## What is not emitted

`provenance` (which engine) appears in [`-f json`](../formats/json.md) but not
in the graph. The `doc:evidence` values `route` and `page-tier` are the
model-tier markers available in DoCO.
