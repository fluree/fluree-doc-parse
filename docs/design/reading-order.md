# Reading order and columns

Reading order is not a separate pass. It falls out of getting three earlier
things right: orientation buckets, column segmentation, and line assembly.

## Rotation buckets come first

A PDF places each glyph with a transform, from which an exact angle is
recoverable. In a mechanical drawing a 90° run is an axis title or a vertical
dimension — not body text. One datasheet's schematic page measured **196
glyphs at 90°**.

Grouping those with horizontal text by y-coordinate would splice an axis label
into a paragraph. So glyphs are bucketed by orientation *first*, and lines are
assembled independently within each bucket.

```
datasheet-a  p7   90°  196 glyphs  "VOL-Low-LevelOutputVoltage(V)…"   ← Y-axis labels
datasheet-a  p2   90°   27 glyphs  "NCGNDNCCONTNCVCCNC…"              ← vertical pin labels
datasheet-b  p30 ±30°    3 glyphs  "W", "LH"                          ← dimension callouts
```

This is also why engineering drawings [do not
escalate](../concepts/escalation.md#where-escalation-is-wrong): the
deterministic path recovers that text with its orientation intact, and a VLM
returns the drawing as one opaque image.

## Columns: empty, not wide

In a two-column layout the columns share baselines, so line assembly would
concatenate them. Raising the line-level gap threshold cannot fix this — that
constant was set from the corpus gap distribution and sits in an empty band,
so lowering it to catch a narrow gutter would split ordinary wide word spacing
everywhere.

The insight is that **a gutter is not distinguished by being wide but by being
empty down the page**. Column segmentation projects glyph occupancy onto the
x-axis and looks for a band that stays empty over vertical extent. That is a
different measurement from any single gap, and it is why the pass runs before
lines rather than trying to repair them after.

## Missing spaces

PDFs position words rather than emitting space characters. A gap wider than a
fraction of the font size has to be reconstructed geometrically — which means
the *text itself* is partly an inference, not just its structure.

The threshold is derived per document, following the same
[relative-judgement pattern](pipeline.md#paragraph-breaks-are-relative) as
paragraph breaks. `fdoc dev gaps` shows the distribution a document actually
has, and `fdoc dev pair` measures the gap around a specific character pair
when a word came out wrong.

## The resulting order

Elements are emitted in the order the layout passes produce them: within a
column, top to bottom; columns left to right; orientation buckets kept
separate. There is no `reading_order` field because the list *is* the reading
order.

## Rejoining a line the gutter cut, without undoing the columns

Partitioning glyphs by column centre bisects anything that spans the page: a
full-width title comes out as `TPS543x 3A, Wide Input R` and
`ange, Step-Down Converter`. Those halves are moved back and rejoined.

The trap is that on a two-column page *every* row looks like that — a left
line ending near the gutter and a right line starting near it, on one
baseline. Rejoining them all silently undoes column segmentation and splices
right-column sentences into left-column paragraphs.

Proximity to the cut cannot tell the two apart. **The ink gap between the
halves can.** A bisected line's halves are adjacent, because the cut fell
inside a word or at a space; two columns are held apart by the gutter.
Measured in font-size units:

| | gap |
|---|---|
| `…Wide Input R` \| `ange, Step-Down…` | 0.03 |
| `Table of` \| `Contents` | 0.30 |
| two-column body rows | 2.1 – 2.7 |

The bound sits at 0.75 — above any word space, including a justified one that
has stretched, and far below the narrowest gutter observed.

A second guard remains behind it: if more than a quarter of a column's rows
look like spanning fragments, the page is in columns and none of them are.
That one catches *justified* two-column text, where every line reaches the
gutter. Ragged-right text defeats it, because only some lines do — which is
how this survived until a field report.
