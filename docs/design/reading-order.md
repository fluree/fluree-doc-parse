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

Within a bucket, text is read the way it runs. An axis title at 90° climbs the
page; a spine label, or a `/Rotate 90` page whose content was drawn upright,
descends it. Both are assembled along their own direction of advance, so
neither comes out backwards, and word gaps are measured from the pen advance
in every orientation.

### A page that is all one rotation is a turned page

A viewer honours `/Rotate`, and so does the glyph transform here. A page whose
content was drawn upright and then rotated for display arrives with every
glyph in one non-horizontal bucket — which is exactly what a viewer shows, and
exactly what a person turns the page to read. When at least 85% of a page's
text glyphs share one quarter-turn bucket, the page is turned as a whole
before anything reads it: glyphs, rules, fills and images are mapped into the
frame where the text is upright, and lines, tables and headings are found
there. Left unturned, such a page assembled as one rotated run per column,
every string reversed, and its table as one seven-point-wide figure.

The output's boxes are always in the displayed frame. The turn is recorded on
the page and undone on every element's box on the way out, so a box drawn on
a page render lands where the text is. The threshold is deliberately high: a
page that mixes a sideways table with upright prose is not a turned page, and
its rotated run is read as one by the bucketing above.

## A superscript stays on its line

Lines are grouped by baseline, and a superscript is set off its baseline by
design: `cm³/s` raises its exponent half a size, past the tolerance, so the
`3` fell out of its line and became a one-character paragraph — on every line
that used the unit, while the line it came from read `cm/s`.

A script glyph is smaller than its neighbour (half to four fifths of the
size), its baseline is shifted by less than the neighbour's size, its ink
sits inside the neighbour's line box — and it sits *beside* one of the
neighbour's glyphs, within half a size along the line. The last condition is
load-bearing: a 7pt line four and a half points above an 11pt title is, by
size and shift alone, the title's superscript, and reading it as one
interleaved the two rows and tore every word of the small line apart. Lines
are clustered by baseline first; a script run then moves into the cluster of
the text it touches. A smaller line set *beneath* fails the overlap test,
because its ink starts below the neighbour's descenders; body text wrapped
around a 33pt display word fails the size test, because nothing a third the
size is a script. The exponent joins its line as plain text — `cm3/s`, as
any text extractor reads it — and the line remembers that it opened with a
script glyph, which is how `95 Ibid.` is known to be a footnote and not
section ninety-five.

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
