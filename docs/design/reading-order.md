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

Rows are the body's height, and a heading straddling the gutter is allowed
for: a row or two that is not empty does not veto it. A line of display type
is still one line, however many body rows its height would fill, so a glyph
fills at most two rows, about its middle. Counted by its height, a poster's
title and the word set huge under it took 21 of its 78 rows, and the two
columns of numbered panels below read as one, line across line.

## Columns a rank of rules states

Decks, brochures and datasheets set panels side by side under a rule each,
below a full-width title. The title fills the gutters for enough rows that the
projection cannot see the panels. The rules show exactly where they are,
though, so a rank of them across the measure states the columns, over the
rows around it where its gaps stay clear. A rank is held to strict tests,
because many things draw that shape:

- **Segment length:** tick marks and legend swatches are too short.
- **Even gaps:** decoration is uneven.
- **Width:** an equation's fraction bars stop short of the measure.
- **The line above runs on through the gaps:** a report that underlines its
  links draws three rules under a line citing three sources,
  `(Ma and Pender 2022b; Baum, Payea, and Steele 2009; Protopsaltis and`. The
  rules are as long and as evenly spaced as a heading rank, and read as one
  they cut a page of prose in three, and its paragraph in two. But that line
  runs on through every gap: its `; ` stands there, with its words within a
  space on either side. A gutter has nothing of the kind. A chart's panels
  can set a tick label in the gap between their rules, but that label stands
  a panel's width from the next, not a space, and the rank stays a ruler.

## A space written as a glyph

Many layout tools write the space after a word as a glyph of its own. It
puts no ink down, so it has no box, and the line pass could not measure the
gap behind it. When the next glyph on that baseline stood across a gutter,
the two columns' lines became one. Column segmentation does not catch these
first when the text across the gutter is ragged: a right-aligned callout, a
diagram's label, a narrow box.

The gap is measured from the last glyph that put ink down, past any spaces,
to the next glyph, against the same block gap as an inked pair. That covers a
footer padded with spaces from its note at the left margin to its page
number at the right, whose advances walk the pen across: the ink gap is what
counts.

The split runs after line assembly, so it can look at the rows around. A
justified line's stretched space can be as wide as a narrow gutter, and
splitting there broke justified prose apart in earlier attempts. What tells
them apart is the paragraph: the lines just above and below, set to the same
measure, put ink in the gap's middle. A gutter stands clear down the rows.
But where every row of two columns ran together, each row looks set to one
measure with the next, and the left column's ragged ends reach into each
other's gaps. A neighbour that is itself cut by a wide gap of its own, ending
where this one's does or spanning its middle, is cut by the same gutter, so
it does not count as running across.

Two cases stay whole: a number or a marker set off from its text by a tab,
`3.1 ⇥ Results` or `12 ⇥ Ibid.`, and a label a word names, `Note 3. ⇥
Business Segment`. When the line so far is a label alone (one token of at
most eight characters and three letters, or a capitalised word and a
number), the gap after it is a tab stop's, not a gutter's. Splitting there
cost the heading and footnote statistics in earlier attempts.

Unless the label heads a column of its own. A margin block, `Issued
07-2025` over `Supersedes 05-2025` at 7pt, stands beside a 6pt note whose
first line falls on nearly the same baseline. The label reads like a word
and a number, so the note's first line ran on from it. What sets it apart
from a heading's number is twofold:

- **The line under the label** starts where the label does, within half an
  em, at most an em of leading down. It ends an em short of the text past
  the gap.
- **Size or baseline.** The label and that text differ in size or baseline
  by a tenth of an em: a step down in type (6pt beside 7pt is a seventh), or
  a baseline off by more than one line's jitter.

A heading's number has its text's size and baseline, and the line under it
runs on across. The label must hold a letter: a bare number over others is a
chart's ticks or a column of figures beside its label (`20` over `10` beside
`Energy`), not a block. Without the size-or-baseline test the cut parted
twice as many documents, most of them chart labels, and lost
`cUR according to CSA …`'s mark from its text.

A running head set as two groups across the page's top now reads as two
lines. The title test passes over a one-line block that shares its row with
another one-line block beside it, so a course code at the right of a
running head is not taken for the document's title. Running strings are
scrubbed from a block, or from a table's cells, only on the rows where
furniture was found: a title that shares a word with the running head keeps
it, and so does a table's note that names the document as its head does
(`refer to the Server QuickSpecs`), now that the head's groups are furniture
one by one.

Two costs. A table header set in pieces across its columns, each piece
bold, used to read as one line, and where the table is not found it now
reads as one block per piece. On a catalogue that is enough bold one-line
blocks to pass the bold cap, which then drops bold as a heading signal for
the whole document. The cap is left as it is here; see #151. And a running
head parted into its groups on a one-page document has no other page to be
found on, so a course code at its right set larger than the text reads as
a heading of its own.

## Regions before columns

Column segmentation finds a page's columns once, from a projection of all
its ink, and they were then read one at a time from the page's top to its
foot. Many pages stack regions instead: a title band over two columns, two
sections in the same pair of columns, columns over full-width notes, a box
set in two columns under full-width text. Read column by column over the
whole page, a block at the foot of the left column came before the top of
the right one, and text beside a title came after all of the left column.

So the page's columns are regrouped by a recursive XY cut. A region is a run
of adjacent columns over a stretch of the page. It is cut across first, at:

- **a run of lines across its columns**, a title or an author list over
  several lines, taken with a short last line set under it. The lines of a
  run stand a line's space apart at the larger type's size: a display
  title's lines are more than a body line apart. A line set just over the
  run, at its size and nearer it than to the text above, is its label and
  goes with it: `Notes:` in the left column over notes set across the page.
  A run alone in its band that opens with a title reads that title first
  and as one, whatever columns its lines start in: a right-aligned display
  title whose first line lies wholly right of the gutter and whose second
  crosses it. A title's lines are words at one size, set large or bold, one
  to a row, and no longer than a heading set large may be (twenty words).
  Lines side by side are columns, and a longer run is a caption, a lead
  paragraph or body lines run together across a narrow gutter;
- **a table across two of its columns or more**;
- **a horizontal rule** across most of the region's width;
- **a gap no line crosses**, at least twice the page's line height. A
  picture counts as content: the space a photo stands in is no gap.

Failing a cut, the region is split down its widest gutter, and each part is
read the same way.

**A cut runs through no paragraph.** A caption, a table or a rule over some
of a region's columns must find, in each column it does not cross, white
space where it falls, as wide as a gap cut needs, or no text on one side.
A picture fills its column as text does: the rules a benefits column draws
between its items, level with a photo in the column beside it, part
nothing there.
A caption under a picture over two of four columns, a table in two of five,
and the rules a feature column draws between its own items each cut the
whole region before this rule, and split every paragraph beside them. A
rule must also stand in a line's space within the text it crosses: a link
underlined across most of the measure lies in the leading under its line.

**Gaps across columns must dominate.** Two charts side by side, each a
title, a plot and notes, leave gaps that line up row for row; cut there, they
were read band by band, a line of one chart's notes and then a line of the
other's. A gap across columns parts them only when it is wider than one and
a half times the ninetieth percentile of the gaps inside them. And when
every column opens with a line at least its text's size, side by side on
one row, each is a flow of its own and no gap parts them. The exception is a
list's items run on from one column into the next, `1. 8Z Pellicle` beside
`4. Three sizes`: the list and the section after it part at the gap between
them. Gaps are judged widest first: the region under a page's title is
judged on its own, where its columns' titles stand side by side. A gap
nearly as wide as the widest is taken with it, unless the part it falls in
opens with titles side by side (a line bold, or larger than the line under
it) and not every column opens another title under the gap. Then the gap is
inside one column's section, level with the next column's next title, and
the text under it is still that section's. Where every column opens a title
under it, as a contract set in two languages side by side opens its next
article, the gap parts two sections in the same columns.

**Gutters only a region has.** A region of one column is cut across too, and
each piece is checked for a gutter only it has: the widest gap no line
crosses, at least half a line high, with lines of words on both sides, side
by side. One side must be running text, a line going on in the next whose
first word opens in lower case (`350lbs` under `300 lb/136 kg` does not). A
side of more than two lines that never runs on and has each line level with
one across the gap is the other's rows, a field list's values, even where a
sentence above the labels runs on. A line or two across the gutter (a lead
paragraph over a box, a callout's line run into the column beside it) parts
the piece, and each part is read at the gutter. Turned text, a side tab's
labels at the page's edge, is not weighed, either for the gutter or for the
width a line must span.

**A panel beside the column.** A fact sheet sets a shaded box beside its
text (`Highlights`, a list of facts), and the text under the box runs the
page's width. The page-wide pass then sees one column, no gutter stands
over the stretch, and read row by row, the box's lines fell between the
column's. A panel is a box drawn to be seen: shaded in a colour other than
white, or outlined, and shorter than four-fifths of the page, with no rule
or other shape drawn inside it. A white fill laid under a page or a half of
a spread is no panel, nor is a figure's frame or a chart's plot, which hold
the drawing's axes, lines and marks. A stretch's panel is read apart from
the column's text when all of these hold:
- it holds three lines or more, wholly inside it, half of them three words
  or more, and no line runs into it from outside;
- three lines of prose (five words or more) stand beside it, level with its
  lines;
- it is between a fifth and three-quarters of the stretch's width, at the
  stretch's edge, and holds under half its lines;
- no line over or under it stands on its own side of the column (a heading
  over a box makes the box part of its column), and the text beside it is
  one column, with no gutter of its own.

The column's lines read first, to the panel's foot and on to the end of the
sentence running past it, then the panel, then the rest. The column's lines
beside the panel set their own measure: the page column's takes in the
panel's width, and against it every line of the column would read short.

Where the page-wide pass does see the panel's column, the box is read after
the column's region, and the column's last paragraph, running on under the
box across the page, was cut by it. A group lying wholly in a panel, three
lines of words or more, is read after the lines that run on: when the group
before it ends a line of prose beside the panel without closing a sentence,
the group after it opens in lower case at that group's left edge under the
panel's foot, and a line of the sentence it runs on reaches under the panel.

**What downstream passes keep.** Blocks are assembled per group, but a
group that is a stretch of its page column takes the column's measure: a
two-line display title read as a region of its own reaches the column's
edge, not the end of its own longer line, and a figure's callout number at
the head of a stretch is no list marker unless it stands at the column's
margin. Only a box read at its own gutter sets its own measure. Side heads
are paired over whole page columns, consecutive groups of one column taken
together, as before. A list marker must also stand within five of its
item's sizes of the text it marks; a number farther off is a label of its
own, a drawing's callout beside another.

## Missing spaces

PDFs position words rather than emitting space characters. A gap wider than a
fraction of the font size has to be reconstructed geometrically — which means
the *text itself* is partly an inference, not just its structure.

The threshold is derived per document, following the same
[relative-judgement pattern](pipeline.md#paragraph-breaks-are-relative) as
paragraph breaks. `fdoc dev gaps` shows the distribution a document actually
has, and `fdoc dev pair` measures the gap around a specific character pair
when a word came out wrong.

### A line set tighter than its column

The threshold is drawn from all of a column's lines, so a line set tighter
than the rest can space its words under it:
- a table caption in a small face under body text spaced wider:
  `TABLE2 | Modelfitandmeasurement…`;
- references in a smaller face: `RademakerM, OakleyA.Digitalmonitoring…`;
- a heading in capitals: `SCHEDULEII—VALUATIONANDQUALIFYINGACCOUNTS`;
- a display title with one word space narrower than the rest:
  `Treasury andAgency`.

A line that writes no space characters spaces its words by its own gaps
where they part into two groups:
- **Letters:** three gaps or more of at most 0.08 em (kerning, a hair of
  tracking).
- **Words:** two gaps or more of 0.12 em or more (a tight justified line's
  are an eighth of an em and up).
- **A valley:** at least 0.1 em wide between the widest letter gap and the
  narrowest word gap, with nothing in it.

The line's cut is the valley's middle, and it only ever lowers the column's
threshold. These keep the column's threshold:
- **Pairs of figures:** they have their own threshold.
- **Glyphs of two sizes:** a raised note mark's gap, measured in its own
  smaller ems, reads wide.
- **Punctuation against a word:** a kern before a full stop or inside a
  bracket is no word space. Nor is the room a lone italic letter's
  overhang leaves before a bracket, a slash or a hyphen (`P(h, v)`, `P/P0`,
  `T-gate`): a maths symbol's. A word's gap before a bracket, roman or
  italic, is still read by the line's own gaps (`solve (34)`, a title's
  `Bethe (The Bethe wavefunction)`).
- **A line that writes its spaces:** it says where its words part, and a
  wider gap there is a kern (`CETIRIZ INE`'s `Z` and `I`).

## The resulting order

Elements are emitted in the order the layout passes produce them: regions
top to bottom; within a region, columns left to right; within a column, top
to bottom; orientation buckets kept separate. There is no `reading_order`
field because the list *is* the reading order.

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

The halves must stand on one row, their tops level. A line that ends just
past the cut leaves only its full stop on the far side, a dot on the
baseline whose top is the letters' height below theirs. Compared by top
alone, that dot was a row of its own, and a note across the page lost its
final `.` to a paragraph. So a closing stop (`. , ; : ! ?` or a closing
bracket or quote) under half a line high counts as the line's when its
bottom stands on the line's. Overlap in height was tried and dropped: a
short fragment overlaps the line above or below it as readily, and joined
the wrong row.

A second guard remains behind it: if more than a quarter of a column's rows
look like spanning fragments, the page is in columns and none of them are.
That one catches *justified* two-column text, where every line reaches the
gutter. Ragged-right text defeats it, because only some lines do — which is
how this survived until a field report. The guard needs six such rows at
least: justified text gives one on every row of a page's columns, while a
centred address of three lines over a form's columns of eight or eleven
lines each is a large share of small columns, and its lines were dealt into
them.

## A line across more than one cut, or on a page of columns

Two refinements keep the rejoin working where the page's gutters fall across
full-width lines above or below its columns:

- **A line can cross more than one cut.** A disclaimer under a three-column
  page is cut twice. At the first cut its middle part moves back into the
  column it began in, so at the second cut its last part is matched against
  the lines of every column to its left, not only the next one.
- **The quarter guard holds back on a page of short columns.** A centred
  address block or a few full-width lines can be more than a quarter of the
  rows beside them. Where a cut falls inside a word, the halves sit closer
  than a space (`Toll F` | `ree`, `FP7 s` | `eries`), and two columns are
  always held apart by more than that. Over the guard, those halves are still
  rejoined; halves cut at a space stay apart.
- **A space the text layer draws at the cut is kept.** A monospaced face
  draws the space with no gap of its own, so the gap between the halves is
  zero and only the space glyph shows there was one (`Screenshot|of what`).
  A fragment is joined back to the line it abuts, the nearest across the
  cut and then on the baseline.
