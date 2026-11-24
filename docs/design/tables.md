# Tables

The hardest target, and the main reason the [model
tiers](../concepts/escalation.md) exist.

## Rule-less tables are usually not rule-less

The founding observation, found by dumping the geometry of a table that
*looks* to have no vertical rules rather than trusting its appearance: its
grid is drawn as **per-cell segments**, one short rule per cell edge.

```text
Horizontal axis=77.5  (68,77)-(183,78)
Horizontal axis=77.5  (183,77)-(297,78)
Horizontal axis=77.5  (297,77)-(412,78)
Horizontal axis=77.5  (412,77)-(528,78)
```

Read individually those are four short lines, and nothing looks like a table.
**Clustered by axis position** they are one horizontal grid line, and their
endpoints — 68, 183, 297, 412, 528 — are exactly the four column boundaries.

Every engine benchmarked failed on that table while working from a grid fully
present in the file: one recovered 3 of 4 columns, one lost the header row,
one collapsed it to a single column. Clustering per-cell segments by axis
recovers it exactly.

```bash
fdoc dev rules  report.pdf 12    # the raw geometry
fdoc dev tables report.pdf 12    # the grid that came out of it
```

## Detection strategies

Multiple passes over one parse, rather than multiple services:

- **Stroked rules** — drawn lines, clustered by axis as above.
- **Filled rects** — thin filled rectangles used as rules.
- **Alignment clustering** — column inference from the x-positions of text,
  for tables with genuinely no drawn geometry.
- **Characters** — a table a line printer drew with `|` (or `¦`) and lines of
  `_` or `-`, read back into the rules it draws before any of the above run.
  See [Tables drawn with characters](#tables-drawn-with-characters).

Alignment-based detection is **required to be corroborated** by drawn
geometry. Ungated it measured TEDS 0.488 → 0.526 but NID 0.868 → 0.839 and
MHS 0.528 → 0.506 — net worse, because it found tables in ordinary aligned
prose. `fdoc dev aligned` shows what corroboration rejected.

## Grouping and trimming

Where a ruled grid can be grouped more than one way, the grouping chosen is
**whichever survives trimming** — the interpretation that still looks like a
table after empty edge rows and columns are removed.

A page's ruling is not one table, and distance is a poor way to tell where one
ends. Two tables are split apart at a row band **no vertical rule crosses**:
the whitespace between two ruled boxes. The band has to sit directly between
two crossed bands, because in a table ruled with horizontals alone every band
is uncrossed, and splitting on those alone turns a ruled table of contents into
one table per entry.

## Each box is read on its own

Read all at once, a page's rules make one grid before it is cut into tables.
Two tables side by side then share their row lines, a chart's axis or the
page's column divider that lines up with a table's rows becomes one of its
columns, and a small table's rules fall short of the coverage a grid line
needs, measured against the whole page's ruling, so it loses its first and
last rows.

So the rules that touch one another are read first as one drawing each: a
box. A box takes in the loose rules inside it that run between two of its
own lines, stopping a hair short of each (a row drawn short of the
verticals), but not one ending in the open (a link's underline under a
cell's words). Inner boxes are read before the frames round them.

- **Where a box rules a grid,** its own reading replaces the page-wide grid
  over it. The rules no box took are read again within that page-wide grid's
  bounds, which keeps a table ruled with horizontals only beside it: such a
  table has no touching rules at all.
- **Where the two readings find the same grid,** the page-wide one stands.
- **A break in a box's verticals is a row's line unless the row runs on
  across it.** A table drawn with verticals segmented row by row and nothing
  between its rows says where they part only by where its verticals break.
  In a table that rules its rows with horizontals, a break with no rule and
  no shaded band's edge there is the same row's second line when every
  vertical breaks there, the first column (which names each row) sets text
  just above it and none just below, and in every column with text either
  side the two lines are a paragraph's leading apart: `54 1/8` over `1375`,
  the model named once. Rows set further apart than a line (`Stir stressed`
  over `Light stressed`, beside a protein centred across them) stay rows.
- **Where the page-wide reading found no table,** a box is a table only as a
  table would be: three rows or more, two columns or more, and text in half
  its cells. A form's strip of field boxes, a chart's legend and a
  schematic's outlines are boxes too.
- **A box ruling a single column** is left to the page-wide reading, which
  has its own tests for a one-column list. A sidebar parted by a rule is as
  often one.
- **Boxes side by side that one shaded band runs across** are one table. A
  table can leave a column unruled, a value spanning every row in the open
  between two ruled blocks, and its rules then touch in two sets. The header
  band, shaded across the whole table, says they are one. Two tables side by
  side shade their own headers, and a band is a row's height, not a panel
  behind both.

## Column rules drawn in some rows

A spec table gives most rows one value across several columns, and rules the
line between two of them only in the rows where they differ: `35` beside `40`,
one `>1500` across both. Such a line falls short of the coverage a grid line
needs. A vertical that stands on the table's row lines at both ends, and
crosses two rows or more, is a column boundary however few rows draw it; the
rows it is missing from span it. Pieces drawn a few points apart in different
rows, never two in one row, are one line, because a word processor sizes each
row's cells on its own.

## The table's outer edge

A first column of labels that each span several rows (`Ceilings` over four)
leaves those rows ruled from the second column on. Only the rules round each
group reach the table's left edge, too few for their ends to make it a
boundary. Where a table's first and last rules end together, that is its
edge. This is read from one table's rules only: across a whole page, the
first and last rules are as often its running head and foot.

## Tall rows

A band more than 3.5 times the median row height is taken for the gap
between two tables: the table ends before it. (The ratio was tuned on the
benchmark's table scores.) Height alone also ends a table at its first tall
row, a label beside a paragraph of value or a SKU beside a three-line
description, and the rows from there fall out as loose text. So a tall band
stays a row where the table's structure runs on through it and its text is
one row's:

- **The structure runs on.** Two of the table's rules cross the band and the
  band beside it, whether one stroke or each row's own: its sides, or two of
  its column rules. One long rule is as often the page's divider. In a table
  ruled with horizontals alone, the rules over and under the band have one
  extent and break at the same columns, as its rows' rules do.
- **The text is one row's.** The first column holding text sets one line or
  two (a label, wrapped at most once), and another column holds text beside
  it. A heading set between two tables holds one column, and a body whose
  rows were never ruled names a row on every line. In a table ruled with
  horizontals alone, no line runs across a column's edge either, as a
  paragraph set between two tables would.

The band a table opens with, its rules running on through it into the rows
below, is its header row however tall: column labels wrap over as many lines
as they need.

## Labels set on their side

A table's column names can be turned up a tall header row. A cell's text is
read in the table's own direction, and turned glyphs crossing a grid are
usually a sidebar or watermark drawn over it, kept out of the cells. A
turned line lying whole inside one cell that holds no level text is that
cell's label, read in its own direction: left to right across the cell for
text reading up, right to left for text reading down. Where its row never
draws a boundary it straddles, it goes to the cell holding most of it. One
running down a column of cells, a group label beside the rows it groups,
goes to the topmost of them. A turned glyph no cell reads stays in the
page's prose.

A table set on its side, a landscape table on a portrait page, is another
matter: there the grid's rows are the table's columns. Its cells hold turned
text and next to no level text, so where the grid has fewer cells of level
text than turned lines, none are read into it, and its text is left to the
prose rather than read out transposed.

## What is not a table

Rules and alignment find grids in things that are not tables: columns of
body text, a box laid out around paragraphs, a drawing's labels.

- **Columns of prose.** Prose runs on from one line into the next, and a
  table's cells rarely do. Two or three columns of body text cut into one
  row a line give cells that end mid-sentence (`built into the`, `un-`)
  over cells that carry on in lowercase. A column runs on where its cells
  are lines of words (four or more to a cell) and a third of them carry on
  into the next; a grid is prose where half the columns holding text run
  on. A table's figures and codes do not, whatever its one description
  column does. Such a grid is dropped before the page's prose is read, so
  its text is read column by column.
- **A box round paragraphs.** A ruled box whose one row under its labels
  holds a paragraph (sixty words or more in a cell) is a page laid out in
  columns, not a table.
- **A band with no columns of its own** is not a table: the page-wide
  boundaries belong to the tables they were read from, and imposed on a band
  ruled by two link underlines and a rule beside them they cut its words.
- **Columns that cut words.** Justified prose lines its words up into
  columns of its own. A reading from alignment or from horizontal bands
  whose boundaries fall inside words, between letters set a hair apart on
  one line, in two rows or more, is not a table, and neither it nor a
  reading of prose is preferred over a ruled table. A ruled table whose cell
  holds a paragraph would otherwise lose to the paragraph's own justified
  columns.
- **Figures across a divider.** A run of figures read from its text takes in
  its label column on the left, but not across a rule standing between them
  that runs well past them (twice the run's height or more): what lies past
  a page's column divider is the next column's prose. A table's own column
  rules stand between its labels and its figures too, and run no further
  than the table.

## Tables drawn with characters

A line printer, and the systems that still print like one, has no ruling. It
draws a table with the characters it has: a bar at the same position on line
after line for a column's edge, a line of nothing but `_` and `|` between
rows, and underscores struck over a line and across its bars to close a row
beneath its text. Read as text, every row of such a table runs into one line
per page, and its column heading, repeated on every page, is taken for a
running header and dropped.

So the drawing is read back as rules before table detection runs:

- **A column's edge** is a bar at one position on three or more lines in a
  row. Text too long for its cell is printed over the bar, so an edge runs on
  across a line or two where a character stands in its place, and stops at a
  line of rule characters, which is where one box ends and the next begins.
- **A row's edge** is a line of nothing but rule characters across at least
  half the frame, or an underscore run struck over a line's text and across
  one of its bars. A line of underscores the bars do not meet is a separator
  set apart from the table, and a run of underscores with nothing printed
  over it is a blank to be filled in; both stay text.
- **A label set across columns** leaves out the bar between them on its
  line: an edge that starts partway down the first row starts a rule under
  the label, as a table drawn with lines would have.
- **Rows parted by a blank line.** A printer cannot rule a row it means to
  part without a line of its own, so a batch record parts a step's
  instructions with a blank one. Each part becomes a row, beside the result
  blanks it asks for; the step's number spans the parts unless the part below
  names a row of its own.

Rows of a table drawn this way are kept whole however tall. A printer rules
every row it means, so a band between its rules is one row, a step whose
instructions fill the page. A band as tall in a table ruled with lines is as
often a body whose rows were never ruled, and is left for the text to read.

## Currency signs

A cell's text is read word by word, and a currency sign set apart from its
figure is a word of its own that must go to the right cell. A financial
statement pins its `$` at the column's left edge and sets the figure to the
right, several ems away, so a lone sign is read with the figure after it.
Invoices in French and other languages set the sign after the figure,
`4,85 €`, and read the same way, every sign but the last moved to the next
column's figure: `4,85 | € 19,40 €`.

The line says which convention it follows. A sign a word space after a
figure that prices no figure after it (the line's last, or one before a
label) means the line sets its signs after its figures, and then every sign
a word space after a figure is that figure's. On a line whose figures are
all bare, a sign is still the next figure's. In a drawn grid the sign must
also stand in its figure's column: a `$` pinned past a cut is the next
column's.

## A one-column grid reads its columns from its text

A key/value list ruled with horizontals only — a billing summary's `MS-DRG …
207`, `DRG Weight … 9.1742` — draws no vertical, so its grid is one column
and every row read label and value as one cell. The column is there all the
same: a gap of empty space that runs down the rows with text on both sides of
it. A one-column grid of three or more rows is scanned for such gaps, and
each becomes a boundary at its midpoint. Two guards keep prose out: a gap
must be free in nearly every row, and most rows must set text on both sides
of it, or it is the ragged right of short lines.

A row whose text runs across a cut — a boxed notice welded in above the
list, a banner spanning it — is read whole into its first cell and marked
`merged_left` across the row, rather than having its words dealt out by which
side of the cut they fell.

## Banners

A letterhead is a single row of fields ruled side by side, and it is a table.
Treating one row as too few left its ruling to be grouped with the table below
it, whose columns then cut the letterhead's text mid-word — a document number
split across two cells, a company name losing its second line.

One row counts as a table only when a **closed box** encloses it: three or
more columns, and a vertical rule standing *inside* the box's own x-range. The
enclosure is the whole test. A caption's underlines — a short rule beneath each
word — produce the same two baselines and the same scatter of column edges, and
on boundaries alone are indistinguishable from a letterhead; nothing encloses
them. Nor does a decorative rule in a margin vouch for a row it never touched.

## A title over the table

A table's first row can be its title rather than its header. A spec table
often opens with one label across the whole table: a title (`HEX CAP SCREWS
ASME B18.2.1-2012`), a section band (`Mission Critical (10K rpm)`), or a
caption that an aligned run took in as its top row (`Thermo Scientific™ M
Series Centrifuges`). Taken as the header, it named every column, and the
rows that do name the columns read as data.

The first row of a ruled or aligned grid is the table's title when:

- it holds one label alone, under a row with text in two columns or more;
- that label is one cell across the whole row, whether ruled so or read so
  from the alignment, holding one label or text whose words run at their
  ordinary spacing over the cuts between columns; or it is the only text in
  the row, in its first column, with nothing in the row spanned;
- it is short (12 words or fewer) and is not a number.

The title is read out as a paragraph just above the table, and the table is
read from the row under it.

The row under the title is not presumed the header: it earns it. A table
whose title band sits over a block of fields (`Form Factor | SFF (2.5”) |
Carrier | …`) or a list of parts has no header at all. Its first row is a
field or a part like the rest, and taken as the header it named every value
below it. So the row under a title heads the table only when it is set apart
from the rows below in one of these ways:

- it is bold, over a row that is not;
- it is shaded, over a bare row, and is not one stripe of many;
- it is ruled off more heavily than the rows below are from each other, or
  ruled off where they are not;
- it is the top of a stacked header (the hex-screw table's four rows of
  column names, giving paths such as `E / Body Diameter / Min`);
- it puts words over quantities. In most of the columns whose cells below
  are numbers, it names the column instead (`Zone | Lumens` over
  `0-30 | 298`, `M17 Series` over `17,000 xg`). It must also not be set
  like the row under it: not of one type (quantity or words) and one
  weight in most of the columns where both rows hold text. A field list
  passes the first test whenever its values are figures, and fails this
  one, as its rows are set alike all the way down.

A header set just like its data, with nothing but its words to tell it
apart (`Name | Value` over `Colour | Grey`), is read as a row of data under
a title. A field or a part read as a header names every value under it,
which is worse than a header read as one more row.

Labels set apart within their columns, each under one ruled cell across the
table (`Area | Competence`), are the columns' own. So is a group label over
some of the columns (`Dates` over `Start | Next review`). A table read from
its text keeps the header its reading found.


## Two-row headers and row groups

A ruled table says where a header label spans columns: the ruling leaves them
open under it. When the row beneath names each spanned column with a label of
its own (`Primary Method` over `NISTmAb | Amgen mAb`), the two rows are one
header and each column is named by both. The labels must differ from each
other and hold no numbers, because data under a banner repeats itself
(`Short | Short`). Each must be a cell of its own: a value run across the
banner's columns (`3.1 mg in 2 mL` under `Strength`) names nothing. And where
the label above spans every column it is as often a band titling the rows
beneath (`Monotherapy` over `Adult patients with …`) as a header over the
column names (`Establishment` over `Name | Address | ID/FEI`), so the labels
under it must be short, as column names are. A column no label spans is named
by the first row alone: the second row leaves it empty, or the ruling runs its
label down through it. A value of its own there (`FPWINGRF-JP2` under
`Product No.`, beside two under a `Type` set across two columns) makes the
row the first of the data.

### A header restated partway down

A table can name its columns afresh partway down: a VOC table's `By wt |
By vol` under a `LB/Gal | g/L` header, or a SKU table's own header row under
a block of fields in the same box. Such a row sets names in two cells or
more, each its own, and no figure or part number, over two rows or more with
something in a column it names. It is set apart from the rows under it one
of three ways:

- **Bold.** Every value cell is bold, two or more, over a row that is not. A
  first column of row labels keeps its own weight: `VOC Content` is set as
  `Total` is, beside a bold `LB/Gal | g/L`. One bold word beside a label is
  emphasis.
- **Shaded as the header row is,** over two rows that are not; striped rows
  alternate.
- **Names turned on their side** in two cells or more, as narrow columns'
  names are. A group label turned down the rows it groups runs on past the
  row, and names none.

The row is not data. The rows under it, until the next such row, take its
names in place of the header's lowest row, under any rows above that one:
`K25W951 / By wt`. A band across the row is a section's label instead.

### Row headers under a group label

A row is named by its first column. Where that column is a group label set
down the rows it groups, a span (`Vertical Angle` turned beside the rows of
angles, a `Problem` over its causes), the rows under it are told apart by the
column beside it. A value further along is named by both, group first:
`Vertical Angle / 55°`.

A table ruled with horizontals only groups its columns another way. Each
group's label (`2024`, `2023`, `Percent Change`) is set over a rule drawn
under that group's columns alone, and the rules part where the groups do.
The grid begins at those rules, so the line of labels over them is taken in
as the table's top row, each label spanning the columns its rule runs
under, and every column beneath is named by both (`2024 / Underground`).
Only one kind of line is taken: within a line of the rules, every mark on it
over a rule, each group's label one run of text. A line that also stands
over a column no rule runs under, such as a label over the row labels, says
the header above is more than one line of group labels. Taking part of it
would leave a fragment, so none of it is taken. A rule across the whole
table is the table's top edge, and what stands over it is its title. A table
that draws its columns bounds its header's cells itself and is left alone.

A row that holds only a label in its first column, drawn as cells of the
table rather than a band across it, labels the rows beneath it when the row
below fills every column. The full row is what tells a group from a row whose
values are missing: a receipt's `TOTAL` with no amount sits over `Cash` and
`Change`, as sparse as it is. A label wrapped onto the next row (`Median in`
over `months (95% CI)`) is carried on, not a group, and a two-column list is
left alone, since a field left empty there looks the same.

## A list of fields has no header row

A spec sheet sets its data as fields, `Label | value` down two columns and
ruled between rows, with no header row. A table's first row is presumed to be
its header, and taken for one, a field list's first field named every value
under it (`1 µL – 50,000 µL` was a `Positive displacement dispenser`) and was
lost as a field.

A header is set apart from the rows under it, and a field's row is not. Two
columns are read as fields, with no header row, when:

- every row is ruled off from the next across both columns, and the rule
  under the first is no heavier than the others;
- the first row is shaded like some row under it, or bare like one, so the
  first band of a striped table is no header;
- in each column, the first row is set in the size and weight of the rows
  under it. A label column set bold beside plain values is a field list, and
  so is one whose values set a phrase bold here and there;
- a label stands in the first column of every row.

A header set no differently from its rows still names its column, and in
fewer words than any value under it. `Settings | Recommendations` over
sentences of advice keeps its header row. A field's value is one more value:
`Positive displacement dispenser` beside `1` and `No`.

The cells of a field list carry no column header. Each label is the row
header of its value, and the first field is a row like the rest. Markdown
has no table without a header row, so it still prints the first field
there. A two-column grid whose labels a colon ends (below) is a field list
on that alone, ruled or not, in any weight.

## Field lists set without rules

A report's header block or a spec sheet's data, `Weight: | 24 g` down two
columns with no rules, has only alignment to show it is a table, and
alignment alone is what prose set in columns has too. Its type is the
evidence. A label column set bold beside plain values is one sign. A label
that a colon ends is the other, and it holds in any weight: prose does not
end a line with a colon beside another column, row after row. The colon has
to follow a word (`Weight:`, `No.:`, `Liabilities ($):`), not a figure, so a
schedule's `10:30` is no label. The label has to be short, six words at
most. At most one word may follow the colon: that is a label run on into the
next one's first word (`Closure Type: Screw/` over `Snap Fit:`).
Most labels must end with their colon; `Patient: A. Sample | DOB: 1987` is
two fields to a row, not a label beside its value.

Two things that look like it are not field lists:

- **A chart's `NOTE:` and `SOURCE:`.** They are set the same way, but their
  values are sentences, and a run whose values are mostly sentences (more
  than six words, ending in a full stop) is a caption.
- **Paragraphs that open on a label.** A wide space after the colon parts
  their words just as a gutter would, but the last value wraps onto a line
  that starts under the labels and runs on across. A field list wraps a value
  under the value.

A field list is read from each row's glyphs in x order. Rows are grouped by
baseline, so a value set a hundredth of a point above its label sorts ahead
of it, the label seems to start before the value ends, and the row reads as
one cell. A spec sheet that places each cell on its own baseline then breaks
its list into runs too short to find. The other aligned runs keep baseline
order. Read in x order everywhere, more two-column prose, slide panels and
chart axes became tables than corroboration turned away. That cost 0.0018
overall on the benchmark (six documents, one by 0.23), and it shows on real
reports too.

### Within a page column, and without colons

Rows grouped across the whole page miss two kinds of field list:

- **A list set in one column of several.** Its row across the page is
  three cells or more: `OTC | Yes` with a line of prose on each side.
- **A list with no colon after its labels.** `CP | Polished Chrome` and
  `Maximum user weight size A | 300 lb/136 kg` have nothing in their text to
  show they are fields.

So field lists are looked for again between the page's gutters. A list
starts at a row whose label opens its column and whose value is the last
cell before the next gutter. It runs on down every row that puts a label
where the first did and a value where the first value starts, or, for
values set right (`Yes`, `#142, 143`), where it ends.

Three kinds of row do not end the run:

- **A row with nothing between those gutters** belongs to another column
  and is passed over.
- **A raised mark set smaller on a baseline of its own** (`MPI®`'s `®`)
  joins the row below, whose line it marks.
- **A row whose value is set closer to a long label than the gap that
  parts cells** is parted where the list's values start
  (`Adjustable PostureFit SL Support Optional`).

A list whose labels end in colons is held to the rules above. Without
colons, alignment is all the text has, and two columns of prose, two lists
side by side, or two addresses side by side align as well. So a list
without colons needs at least three rows. Its labels must be short and not
prose. That means:

- no more than a quarter of them open in lowercase, as a line running on
  from the one above does;
- fewer than half are a list's enumerators (`(a)`, `iv)`, `10. FIFA`);
- the values do not crowd the right edge the way a column of prose fills
  its width.

It must also not be a contents page: page numbers that never go back down.

The two columns must also be of two kinds, as a label and its value are.
At least one of these holds:

- bold labels beside plain values;
- values that are mostly quantities;
- labels that are all codes, one short word (`CP`, `2MB`, `CIO`);
- values drawn from a short list, at least half of them repeats (`Yes`,
  `No`; `Optional`).

A row whose value is a word, over values that are quantities, names the
columns (`Description | Units` over `Chafing dish | 12`): it is the
list's header. Otherwise the list has none.

## Where a table goes in the reading order

A table is placed after the last block above it that it overlaps across: the
block it follows in its own column.

A table is also never placed ahead of its own heading. That heading is the
nearest one above the table's top, either in its lane, or to its left and
level with its top (within three of its line heights). A catalogue sets each
product's heading at the left over its photo and the product's table to the
right. The table overlaps no block of its own section above it, only the
previous product's table, which runs a little under it, so it went into the
previous product's section. Moved past its heading, it also goes past
whatever is set under the heading above the table's top, such as a note
(`UPS option`) beside its first rows.

A stricter lane was tried first: a block had to overlap a quarter of the
narrower of the two to count. It fixed one page but moved tables on a hundred
others, as often for the worse, so the section, not the overlap, decides.

## Merges

Spanning cells are detected from which boundaries are *not* drawn across a
row, and reported as `merged_down` / `merged_left` flags with the value in the
position where its text was laid out.

`cells` is deliberately left in that rowspan convention. Repeating a spanned
value across the positions it covers contradicts how a reference encodes a
rowspan, and measured worse against the benchmark's ground truth when it was
tried — Markdown is the projection the harness scores, so it prints the grid
as detected. Consumers wanting self-contained rows
[denormalize](../concepts/element-model.md#tables); the DoCO emitter already
does, and the XHTML emitter expresses the merges as `rowspan`/`colspan`
instead.

### Rows drawn but not ruled

An unruled boundary is not, by itself, a merge: reading every one as a span
blanks most of a table whose rows came from elsewhere. But a word processor
draws a table's rows whether it rules them or not, and then the text says
what the ruling does not. Three samples of one product sit on three rows with
no rule between them, the product's name and formulation each set once,
centred on the group and wrapped where the column is narrow. Cut at the rows,
the formulation's `pH` lands on one sample and its `5.0` on the next.

So inside a group of rows ruled off from its neighbours but not within, a
column whose rows each hold text placed the same way (each centred in its row,
or each at its top) gives every row its own value, and another column whose
text runs across the rows' cuts as one wrapped value, or sits as one line
centred on the whole group, is one cell spanning it. A line that carries on
the one above it (it starts lowercase, or follows a comma) is the same value
even when it sits on the next row's baseline, a row's height below.

Three conditions keep this from firing where it should not:

- **The group is ruled off on both sides by rules drawn across the whole
  table, and there are two such groups or more.** A table ruled under its
  header and at its foot is one group, and a sparse column in it is values
  missing from some rows.
- **The rows inside the group are drawn.** The border is stroked row by row
  and breaks at each one. Where the grid took a cut from the gap between a
  label's wrapped lines, the border runs straight through it, and reading the
  cut as rows copied the doses beside the label onto a row of its second line.
- **A column has to give each row its own value.** Without one there are no
  rows to span.

### Shading parts rows

A shaded row can be parted from the next by nothing but the edge of its
shading. A header shaded down to its first row, with no rule between them,
was read as one row with it, so the header named every column after the
first product (`Part No. AFPG430`). A rule drawn under one column only, with
the shading ending on the same line over the others, left those columns open,
and a merge read the shaded labels and the first values as one cell
(`IVD-MD 75002576`).

So the edge of a fill parts rows as a rule there would, in two places:

- **In the grid.** A fill across nine tenths of a table's width, hanging
  from one of its lines (a header band from the top rule, a shaded row from
  the rule above it), whose other edge falls inside a row, a cell's height
  from its lines and through no line of text, is a row boundary too. Only
  inside a grid the rules make, of two rows and two columns or more: an
  edge never makes a table where the rules make none, nor parts a chart's
  frame, its legend boxed over the plot, into rows.
- **In the merges.** Where a rule is drawn under some columns of a row
  boundary, a fill ending on that line over the others closes them. Its
  sides must stand on column edges, as a cell's shading does, not a
  highlight set inside a cell behind a line of its text. Never alone: a
  boundary no rule is drawn on claims no span.

An edge counts only where the shading stops. A column shaded as one block is
often drawn a fill to a row, edge to edge, and the seams are not seen: a fill
butted on to another across the line parts nothing. A background laid under
the whole table is no such fill, and the band on it still shows. And only a
table shaded once: one band across it, under half its height, is a header or
a row set apart, while a table striped row by row has many, whose edges are
no rules, and is left to the rules it draws.

## Tables split by a page break

A table that runs off the foot of one page and on at the head of the next is
read as a grid on each page. Read alone, the second part's first data row is
taken for a header, and a cell open into the rows below it grows that header
deeper still, until a row of data names every column under it.

Two parts are one table when nothing comes between them (the first is the last
thing read on its page, the second the first on the next) and their columns
are drawn at the same places. The second part is linked to the first and has
no header of its own, unless:

- **it repeats the first part's header**, which it keeps, and the renderers
  leave out; or
- **the break fell inside the header.** The first part is then nothing but
  header (below it, at most a sliver of the row the break cut: a
  superscript's top) and the second opens with a bold row of labels, which
  finishes the header. Each column is named by both: `KEYTRUDA … / Grades 3-4 (%)`.

A second part that opens with a styled row of its own (bold over plain, or a
shaded header band) that repeats nothing is a new table with the same
columns, and is left alone. Nor is a second part that repeats the first one
whole, every row: that is the next copy of a form printed box after box down
the pages, and no table carries on by repeating its body. A single shaded first row is no evidence either
way, since a table striped row by row opens a page with one.

A table whose cells hold lettered lists is not a table of lists. A reading
from horizontal bands replaces a coarse ruled grid whose cells hold whole
paragraphs, which is right for a page ruled into a frame with a table set in
one of its columns. But a band reading whose first column is nothing but list
markers is the list in a cell, read cell by cell (`a) | Change the pump
frit`), and taking it for the table pulled the settings beside the lists out
into a table of their own.

The parts stay separate elements, because an element and its box sit on one
page. Each later part names the one before it, and the DoCO and Markdown
emitters read the chain as one table.

### Parts of one table on one page

A table can also come out of one page in two parts, and the lower one has no
column names. Parts that follow one another with nothing between, the lower
one a box's spacing below, are linked in two ways:

- **A header boxed apart.** A spreadsheet laid out for print fills its header
  in one box and rules its rows in another. The upper part sets names and no
  figures, the lower opens on figures, the two share their outer edges, and
  every upper column edge is a lower one, each within a quarter of the
  narrowest lower column (a box drawn a row at a time has its sides in
  pieces a little apart). A name over two of the lower part's columns heads
  both (`Nominal Size` over the size and its decimal), so the header is dealt
  onto the lower part's columns, a name spanning those it covers, and is all
  header: `D / Body Diameter / Max.`.
- **Rows set below a table on its columns.** A run of rows read by alignment
  breaks where a line of something else falls between two of them, such as
  an outer table's label centred on the tall row the inner table sits in.
  The rows under the break carry on the table where they are as many
  columns, each column's middle inside the other's, and open on figures with
  no header of their own, under a table that has a header and figures.

## Escalation signals

A table earns a model tier when its detected structure **disagrees with
itself**:

| signal | meaning |
|---|---|
| `Fragmented` | column boundaries are inconsistent between rows |
| `MergedRows` | rows do not segment cleanly |

```bash
fdoc triage ./corpus/
# 268 tables, 10 with disagreeing structure (3.7%) — model-tier candidates
```

That self-disagreement is a genuinely corpus-independent confidence signal:
it does not depend on a threshold tuned to one document collection, only on
whether two readings of the same geometry agree.

## Why tables still escalate most

Even at tier 5, tables are where the remaining error lives. Table content is
also what makes escalation expensive — a VLM generates it autoregressively, so
a dense table page costs the most seconds of anything in the pipeline. The
combination is why escalation is per-table rather than per-document.

## A drawn lattice is not a table

A designer's baseline grid is drawn with the same primitive a table's ruling
is. One deck's page carries nine full-width rules at a 21.7pt pitch and five
full-height rules at 45pt, and the grid built from them shredded two columns
of prose into a twelve-by-twenty-three "table" whose cells tore words in half
— `ope | n-architect | ure` for *open-architecture*.

Rules that span the full page at a constant pitch **on both axes** are dropped
before anything reads the geometry.

Both axes is the whole safety of it. Evenly spaced rules on one axis are an
ordinary table with equal rows, which is common — one corpus document has six
of them. A lattice that repeats down the other axis too, with every rule
crossing the entire page, is not something a table does: a table's ruling
spans the table. No document in the evaluation corpus matches the two-axis
signature, and adding the rule moved neither score.

The pitch is established from the longest even run and then applied to every
full-bleed rule on it, so a stray rule drawn across the lattice does not hide
half of it. A page border sits off the pitch and survives — it is nobody's row
boundary, but removing it is not this rule's business.
