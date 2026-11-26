# The pipeline

Glyphs in, [DoCO elements](../concepts/element-model.md) out. The order is
load-bearing at every step — each stage exists partly to protect the next one
from a specific failure.

```
glyphs
  │
  ├─ 1. dedup       overprint: faux bold, redrawn text, struck underlines;
  │                 accents set over letters composed with them;
  │                 tables drawn in characters read back into ruling
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
if it survives to stage 2. The same stage drops underscores struck over text
and reads a table drawn with `|` and `_` back into the rules it draws: both
are drawing set in the font, and stage 2 has to see a drawn table as rules,
not as text, or its glyphs go to prose and its repeated header row to the
furniture pass. A spacing accent drawn over its letter (`e` and `´`) is
composed into it (`é`) here too, once any second copy of either is gone:
left as a glyph of its own it parts the word it belongs to, and a raised
accent forms a line of its own above the line it sits on.

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

**Three shapes the ragged-edge test misses.** It needs three lines, a block
of one left edge, and most lines ending early against the block's own
widest line. Lists printed one item per line pass none of that:

- *Numbered items with hanging lines.* `1. Avoid exposure…`, then `2. Avoid
  exposure…` whose wrapped lines hang an em in, the last running full. A
  line opening with `n.` set where the latest line opening `n−1.` is, with
  only lines at or right of that edge between, starts an item, and both carry
  their labels. Where a slightly wider space parts the list into blocks, the
  count carries from block to block at one left edge.
- *A paragraph ended by hand.* `…lo antes / posible.` over `El aislamiento…`
  at the same leading. The short line stops short of where the paragraph's
  lines and the next one reach by more than the next line's first word and
  two ems, and the next opens with a capital. The margin is that wide
  because typesetters that break a paragraph's lines together, and designers
  setting a rag, end a line a word short of a greedy break (`…8Z Pellicle,
  and` over `Harmonic 2 Tilt…`). The test needs the column's text to show a
  wrap margin, and leaves display type, a weight change, a hyphen or a comma
  at the line's end to the other tests.
- *Entries one per line.* Eight standards of about one length agree on a
  right edge of their own, so against the block none ends early. Against the
  column's measure every line does, by more than the next line's first word,
  and none goes on in lower case as a wrapped line of a narrow box does: each
  line is a block. Measured against the column only here, where every line
  opens with a capital; a narrow box's prose keeps its paragraphs.
- *Bold heads stacked.* Bold lines are left whole by the entries test, as a
  title broken over two lines must be. Two lines, both bold, at one size
  from the body's up to display type and set at one indent, are parted where
  the second opens with a capital (not a bracket), and does not open on a
  word that runs on or hold a year, its first word would have fitted on the
  line before, and that
  line ends on no comma, hyphen or word that runs on (`of`, `and`, `in`):
  `Materials and methods` over `Animals` is two headings, `Effect of AZT on
  telomerase activity in` over `HO-8910 cells` one.

**And the other way: a sentence that plainly runs on.** Spacing and weight
can part a paragraph where its sentence goes on. Footnotes set at nearly
twice their size's leading pass the paragraph spacing after every line,
and a warning that sets its middle sentences in bold capitals changes
weight mid-sentence (`…lead dust. LEAD IS` over `TOXIC. EXPOSURE…`), in a
box narrower than its column, so the column's measure cannot show the line
ran out of room. The lines stay one paragraph when the line is full against
the lines at hand, ends without terminal punctuation, and either the next
opens in lower case (within a quarter again of the paragraph spacing) or
the break falls inside the run of capitals the weight change belongs to,
with lines of the paragraph above it (within the paragraph spacing). A
heading in bold capitals stands alone over text that opens in mixed case,
and a contents entry's leader is never a full line. Under an item whose
lines hang under its first, a line opening in lower case where those lines
are set goes on with it, full line or not: a list's bold-italic `or` under
an item of two lines whose second is the shorter.

**A size step across a hand-ended line.** The lines of one paragraph do not
measure alike — an 11pt paper reads 10.9 on one line and 11.1 on the next —
so the size tolerance between full lines is loose. Across a line that was
ended by hand it is the smallest step a typesetter makes, half a point: a
service log's 9pt code label over its 8.5pt description.

## Then: routing and arbitration

The elements from stage 7 are the deterministic result. Everything after is
[escalation](../concepts/escalation.md): the [router](router.md) decides what
was unusable, and arbitration decides what a model tier is allowed to change.
