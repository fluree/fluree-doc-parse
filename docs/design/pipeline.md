# The pipeline

Glyphs in, [DoCO elements](../concepts/element-model.md) out. The order is
load-bearing at every step — each stage exists partly to protect the next one
from a specific failure.

```
glyphs
  │
  ├─ 1. dedup       faux-bold overprint
  ├─ 2. tables      grids from ruling geometry; their glyphs withheld
  ├─ 3. columns     vertical whitespace projection
  ├─ 4. lines       within orientation buckets
  ├─ 5. furniture   headers, footers, watermarks
  ├─ 6. blocks      lines → paragraphs
  └─ 7. headings    over blocks, using the outline tree
  │
  └─► elements
```

## Why this order

**1. dedup, before anything counts glyphs.** PDFs fake bold by drawing text
twice at a small offset. Every later stage counts glyphs — for word gaps, for
weight statistics, for routing — so a doubled glyph run corrupts all of them
if it survives to stage 2.

**2. tables, before prose.** Grids are found from ruling geometry, and the
glyphs inside them are **withheld** from prose assembly. Without this a
table's cells appear twice: once as cells and again as paragraphs.

**3. columns, before lines.** Line assembly groups glyphs sharing a baseline,
and in a two-column layout the two columns share baselines — so line assembly
would concatenate them:

```
"that integrates a low-resistance, high-side N-channel – TPS5430: 5.5V to 36V"
 └───────────── left column ─────────────┘ └──── right column ────┘
```

**4. lines.** See [Reading order and columns](reading-order.md).

**5. furniture, before blocks.** A footer must be removed before paragraph
assembly or it is absorbed into the last paragraph of the page. For tables the
requirement is sharper still: a leaked `|` breaks a Markdown column count.

**6. blocks.** Lines → paragraphs, on a per-document leading threshold.

**7. headings.** Over blocks, because a heading is a property of a whole
block, and using the [outline tree](headings.md) where the document has one.

## Paragraph breaks are relative

A paragraph break is a *relative* judgement. Measured baseline-to-baseline
distance, normalized by font size:

```
long-form prose report    mode 1.45–1.55
dense technical datasheet mode 1.15–1.25
```

Both are ordinary single-spaced body text; the documents simply set different
leading. A fixed threshold that splits one correctly would over- or
under-split the other, so the **modal leading is derived per document** and
the break test is a multiple of it.

This pattern — derive the norm from the document, then judge relative to it —
recurs throughout. Font size for headings, gap width for word breaks, and rule
length for table edges all work the same way, for the same reason.

## Then: routing and arbitration

The elements from stage 7 are the deterministic result. Everything after is
[escalation](../concepts/escalation.md): the [router](router.md) decides what
was unusable, and arbitration decides what a model tier is allowed to change.
