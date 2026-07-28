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
    "rdfs":     "http://www.w3.org/2000/01/rdf-schema#"
  },
  "@graph": [ … ]
}
```

One Fluree namespace, `doc:`, plus three public ontologies and `rdfs` for the
display label. `doco` is the Document Components Ontology; `po` is the Pattern
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
                 "@value": [ { "pageIndex": 0, "width": 612.0, "height": 792.0 } ] } }
```

A `doc:bbox` cannot be placed on a rendered page without these: the consumer
needs the ratio between the page's own units and the pixels it rendered to,
and this is the only place that denominator appears. Sources with no geometry
— Markdown, DOCX — omit the key rather than reporting a zeroed size.

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
