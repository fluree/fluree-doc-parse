# The element model

Every reader — PDF, Markdown, HTML, DOCX, PPTX — produces a flat
`Vec<Element>` in reading order. Every output format is a projection of that
list. Nothing else is shared between a reader and an emitter, which is what
lets a Markdown consumer avoid compiling a PDF engine.

## The element

```rust
pub struct Element {
    pub id: String,             // "elem-00001"
    pub kind: String,           // DoCO class, e.g. "doco:Paragraph"
    pub page: usize,            // 0-based; 0 for formats without pages
    pub bbox: Option<BBox>,     // absent when the source has no geometry
    pub text: String,
    pub level: Option<usize>,      // 1-6, only on doco:SectionTitle
    pub cells: Option<Vec<Vec<String>>>,  // row-major, only on doco:Table
    pub header_rows: Option<usize>,
    pub sub_headers: Option<Vec<usize>>,
    pub merged_down: Option<Vec<bool>>,   // cell continues the one above
    pub merged_left: Option<Vec<bool>>,   // cell continues the one to its left
    pub figure: Option<String>,           // shared id for fragments of one drawing
    pub links: Option<Vec<Link>>,         // hyperlinks over this element's text
    pub provenance: &'static str,         // "rust" | "vlm"
    pub evidence: &'static str,           // which signal classified it
}
```

## The DoCO classes

Structure is typed with [DoCO](http://purl.org/spar/doco), the Document
Components Ontology:

| class | what it is |
|---|---|
| `doco:Document` | the root |
| `doco:BodyMatter` | the body partition |
| `doco:Section` | a heading and everything under it |
| `doco:SectionTitle` | the heading itself, with `level` |
| `doco:Paragraph` | a block of prose |
| `doco:List` / `doco:ListItem` | lists |
| `doco:Table` | a table, with `cells` |
| `doco:Figure` | text inside a drawing, or an [anchor](../integration/anchors.md) for an escalated region |

Readers emit five of these — `SectionTitle`, `Paragraph`, `ListItem`, `Table`
and `Figure`. `Document`, `BodyMatter`, `Section`, `List` and `doc:TableCell`
are minted by the [DoCO emitter](../formats/doco.md) when the flat list
becomes a graph.

`doco:Caption` and `doco:FrontMatter` are **not** emitted — see the
[vocabulary](../reference/vocabulary.md#types) for why.

The flat list carries containment implicitly through order and heading level.
The [DoCO JSON-LD output](../formats/doco.md) makes it explicit as `po:contains`
edges, deriving `doco:Section` nesting from heading levels.

## Why flat

Reading order *is* the document. A tree would force the reader to commit to a
containment decision at parse time, before headings are known — and heading
detection is the least certain part of the pipeline, the one most likely to be
revised by a [later tier](escalation.md). Keeping the list flat means an
arbitration pass can promote a paragraph to a heading without restructuring
anything; the tree is derived once, at emission.

## Tables

`cells` is row-major `Vec<Vec<String>>`. Two extra fields carry what the
geometry measured and a consumer cannot re-derive:

- **`header_rows`** — how many leading rows are header. `None` means
  undetected (model-provided tables); treat that as 1.
- **`sub_headers`** — row indices below the header block that are a single
  full-width cell labelling the rows beneath them. The banner bands that split
  a matrix into sections.

Spanning cells follow the **rowspan convention**: the value sits where the
text was laid out and the other spanned positions are blank, with
`merged_down` / `merged_left` flagging the continuations. `cells` is left that
way on purpose: repeating a spanned value across the positions it covers
contradicts how a rowspan is normally encoded, and it cannot be undone by a
consumer that wanted the real structure.

A consumer that needs self-contained rows builds a `Merges` from those flags
and denormalises:

```rust
use fluree_doc_model::{denormalize, Merges};

let mut rows = element.cells.clone().unwrap_or_default();
let ncols = rows.first().map(Vec::len).unwrap_or(0);
let m = Merges {
    continues_above: element.merged_down.clone()
        .unwrap_or_else(|| vec![false; rows.len() * ncols]),
    continues_left: element.merged_left.clone()
        .unwrap_or_else(|| vec![false; rows.len() * ncols]),
    full_width_row: (0..rows.len())
        .map(|r| element.sub_headers.as_ref().is_some_and(|s| s.contains(&r)))
        .collect(),
};
denormalize(&mut rows, &m);
```

The [Markdown](../formats/markdown.md) and [DoCO](../formats/doco.md) emitters
already do this; the flags are there for consumers of
[`-f json`](../formats/json.md), which reports the grid as detected.

## What is absent

There is no `confidence` scalar and no `reading_order` index. Reading order is
the order of the list. The classification's basis is reported as
[`evidence`](provenance.md) — a name for the signal that fired — which is what
a consumer deciding whether to trust an element actually needs.
