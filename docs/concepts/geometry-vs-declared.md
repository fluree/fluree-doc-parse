# Measured vs declared structure

Two kinds of source feed the same [element model](element-model.md), and the
difference between them explains most of this engine's design.

## PDF declares nothing

A PDF says *put this glyph at this point in this font*. It does not say "this
is a heading", "these cells form a table", or even "these glyphs are a word."
Paragraphs, headings, lists, tables, columns and reading order are all
**inferred from geometry** — from where things sit relative to each other.

Consequences that run through the whole pipeline:

- A heading is a guess, supported by font size, weight, numbering, isolation
  and the [bookmark tree](../design/headings.md).
- A table may have no drawn rules at all; its columns are inferred from the
  x-positions of the text inside it.
- Word boundaries are measured from inter-glyph gaps, so the *text itself* is
  partly an inference.
- Everything carries a bounding box, because everything came from a position.

This is why the [escalation tiers](escalation.md) exist at all: they are for
the cases where the inference is too weak to trust.

## DOCX, PPTX, HTML and Markdown declare everything

`<h1>` is a heading because it says so. A `<table>` has the rows it has. These
readers **map rather than measure** — there is no inference to be uncertain
about, and correspondingly nothing to escalate.

They also carry **no geometry**. A DOCX paragraph has no position on a page,
because the page does not exist until something lays it out.

## Therefore: `bbox` is `Option`

```rust
pub bbox: Option<BBox>,
```

Elements from declared-structure sources have **no `bbox` field at all** in
the serialized output — not a zeroed one.

This is deliberate, and it is the single easiest thing to get wrong when
consuming the output. A zeroed box reads as a real position to every consumer
that trusts coordinates, and [entity overlay](../integration/entity-overlay.md)
is one of them: highlights would silently stack in the top-left corner of the
page instead of failing visibly.

```jsonc
// from a PDF
{ "id": "elem-00001", "type": "doco:Paragraph", "page": 0,
  "bbox": { "x0": 91.17, "y0": 185.64, "x1": 145.0, "y1": 195.09 },
  "text": "…" }

// from a DOCX — no bbox key
{ "id": "elem-00001", "type": "doco:Paragraph", "page": 0,
  "text": "…" }
```

Consumers deciding *whether* geometry exists must read `bbox` directly.
`Element::rect()` collapses the distinction — returning an empty box when
there is none — and exists only so layout code inside the PDF path stays
readable. Do not use it to decide whether a position is real.

## Pages

`page` is 0-based, and formats without pagination report `0` throughout.
PPTX is the interesting middle case: slides *are* pages, so `page` is the
slide index, but slides still carry no bounding boxes.

## What this means for you

| you want | source |
|---|---|
| text and structure | any of the five |
| coordinates, overlay, page rendering | PDF only |
| certainty about structure | the declared formats |
| the same graph shape regardless | all five, by construction |
