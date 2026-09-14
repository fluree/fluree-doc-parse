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

## Breaks the spacing does not show

Leading is the main paragraph signal, not the only one. Three others end a
block where the spacing says it continues, each learned from a document that
read wrong without it.

**A fill's edge is a row's edge.** A form can shade each item on its own
band, with the item's label wrapped onto two lines inside it, and leave the
gap between bands a hundredth under the paragraph threshold, so whether two
items merge comes down to rounding. Two lines under different
fills are two rows however close their baselines; a box drawn around a whole
paragraph contains every line and separates nothing.

**A ragged right edge is a set of hand breaks.** A checklist sets one field
per line at body leading, and a page of fields read as one paragraph. A wrapping typesetter ends a line only when the next word does not
fit, so in a wrapped paragraph no line but the last leaves room for the word
that follows it. Where most lines of a left-aligned block of three or more
do, the breaks were put there — a form, an address, a listing — and each line
is a block. Lines of such a block that count off in sequence (`1. Label four
plastic bags`, `2. Weigh 20 g of soil`) are an ordered list's items and carry
their numbers as markers, so a step never passes for a numbered section.
Centred text is exempt: its left edge wanders, and that is the layout.

**A size step across a hand-ended line.** The lines of one paragraph do not
measure alike — an 11pt paper reads 10.9 on one line and 11.1 on the next —
so the size tolerance between full lines is loose. Across a line that was
ended by hand it is the smallest step a typesetter makes, half a point: a
service log's 9pt code label over its 8.5pt description.

## Then: routing and arbitration

The elements from stage 7 are the deterministic result. Everything after is
[escalation](../concepts/escalation.md): the [router](router.md) decides what
was unusable, and arbitration decides what a model tier is allowed to change.
