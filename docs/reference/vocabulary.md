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

`doco`, `nif` and `po` are public ontologies — the Document Components
Ontology, the NLP Interchange Format, and the Pattern ontology. `po:contains`
is not optional decoration: DoCO is defined as an extension of the Pattern
ontology, and that is the containment property it specifies. `rdfs` carries
one term, `rdfs:label`, rather than an ontology of its own.

`doc` is the single Fluree namespace, covering what those four do not.

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

That is the complete set — eleven types, and no others are emitted. Notably
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
| `doc:sourceDocument` | all, with `--doc-iri` | the document IRI to retract by |
| `doc:pages` | `doco:Document` | JSON literal: `[{pageIndex, width, height}]`, PDF units |
| `doc:unreadPages` | `doco:Document` | JSON literal: `[{pageIndex, reason}]` — content nothing transcribed |
| `doc:runningText` | `doco:Document` | JSON literal: the header/footer text stripped from the body |

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
paginationless sources, which `pageNumber` would misdescribe.

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
