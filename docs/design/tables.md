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

Two tables can share one frame, its sides running on down through the blank
band between them. The band is crossed by the frame's two sides alone and
holds no text, and either side of it stands a table by its own ruling: three
columns or more over three rows or more, the columns set at different places
above and below. The tables part there, and each reads its own columns. A
band set across the table with a label in it, a group's banner, is crossed by
the frame alone too and stays a row; so does a blank row inside one table,
its columns ruled alike either side, and the band over a box of one row or a
diagram's panel, which is no table to part off.

A grid's top and bottom are trimmed of bands no vertical crosses: a heading's
underline welded on above a ruled box. Only verticals within a row's height
of its rows count.
Read page-wide, a grid takes in every rule on the page, and a logo's strokes
at the page foot crossed none of a horizontals-only spec table's rows, so it
was trimmed to its last two.

A grid the trim runs down to the three lines a grid needs without finding a
band any vertical crosses has only its rules to say where its rows are.
Lines of text between two rules drawn over different stretches of the page,
sharing none, are no row, and the grid is no table: two lines or more under
one of the rules, in its stretch, are a column's paragraphs or list. Such
rules divide the sections of a page set in two columns, one column's rule
over the other's. Kept, the last two bands of such a page became one table,
the end of one column's list in one cell and the next section in the other.
Two tables set side by side, each ending on its own rule a little above or
below the other's, leave between those rules a sliver holding nothing, or
each table's last line, and they stand. So does a table ruled with
horizontals only under a stroke of decoration the trim counts: its rules
share their stretch of the page.

A table ruled with horizontals only takes its columns from where its rules
break: a rule drawn a cell at a time has pieces meeting end to end at the
columns' edges. A rule drawn a cell at a time can run each cell's piece on
past its edge into the next, a few points or a column's padding. The two
ends inside such an overlap are one column's edge, at the overlap's middle,
not two edges with an empty column between them. Pieces a longer rule is
drawn over whole (group underlines under a rule across all of them) keep
their ends.

### Row lines ruled alike, read together

A page with two tables ruled at different columns can leave the page-wide
reading with nothing: across the page no edge but the outer two has the
support of half the lines, and a one-column grid with verticals inside it is
no table. Read in touching components instead, a table ruled with
horizontals only falls apart into lines, none touching the next. So where the
page-wide reading finds nothing, row lines ruled alike are read together: three
lines or more, one under the next, each with the same extent and broken at
the same places, its columns stated over them: a vertical standing on one
of the breaks (a shaded header's separators), or a shaded band drawn a
column at a time, fills of one height meeting end to end across the lines'
extent with an edge on every break. Its ends are not trimmed: the lines are its rows by how they are
drawn. It is kept as a table where:

- it has two rows and two columns or more;
- its lines part its text's lines, three to a row at most (a statement
  ruled only under its header and its totals sets many rows between them);
- two of its columns or more hold text in half its rows (a chart's
  gridlines leave one column of tick labels).

A band the components read inside it on its own, a shaded header whose
separators touch its rule, is one of its rows. A statement's figures,
underlined a column at a time, break alike too, but no vertical stands on
their breaks and no band shades them; a chart's bars between gridlines
broken alike stand apart, each its own height.

A stack whose labels stand outside it, to its left, is read from its text
with them, as tables ruled with horizontals only are. The text's gutters can
read figures set closer than a gutter's width as one column (`$ 4,137 $ 3,610
$ 3,547` under `2016 2015 2014`), where the stack's lines break between them.
A reading that puts in one cell figures on both sides of a break, with no
glyph across it but a closing bracket hung past the edge, says something
wrong of every cell it reads so. Neither it nor the stack stands, and the
text is read as text. The breaks are the stack's own: a grid ruled some other
way says less of where its columns are drawn.

### Tables side by side, ruled with whole lines

Two tables set side by side in a page's two columns, each ruled under its rows
with lines drawn whole across its own width, leave the page-wide reading
nothing to stand on. Their lines interleave down the page a few points apart,
or fall level and read as one line across both, and no line breaks where a
column does. Each stack of such lines is read from its text where all of these
hold:

- it has four lines or more, one under the next at an even pitch, each drawn
  unbroken across the same stretch. A line's pieces end only at its two ends;
  two pieces with a gap between them are two lines, one over each table;
- another such stack stands beside it, sharing half the height of the shorter;
- no grid already holds most of it;
- three bands in four or more between its lines hold text. A table ruled under
  its rows sets a row in every band, a blank row aside; a chart's gridlines
  stand over its bars, a drawing's lines over nothing;
- the reading stands within its lines, give or take a twentieth of their
  width for a header label hung past them. A stack narrower than the table it
  rules is one column's lines in a wider table.

Labels left of the stack are not taken in from under the other table's rules:
a horizontal over the text that ends short of the stack, within its height.
Read as any region is, the left table's last column would be the right one's
labels. A grid with no verticals inside such a stack, read again from its
text, keeps the same guard.

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
- **Boxes stacked a hair apart are two drawings.** A spec sheet sets its
  option tables one under another, each boxed with its own title band and
  its own columns (`Trims` over two, `Housings` under it over two others),
  closer than rules must be to touch. Read as one drawing, the two boxes'
  dividers made three columns neither has. Where a line runs across the
  whole set on either side of a gap less than a row tall, no vertical
  crosses the gap, each side is closed at both ends, and each rules a
  column the other does not, the set is two boxes. A header ruled off by a
  double line, its verticals broken across it, rules the columns the rows
  under it do, or fewer, and stays one table; so do boxes whose sides run
  on across the gap.
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

A catalogue's product table often has no first or last rule there: its
header is shaded across every column, and its rows are ruled from the
second column on, each product's name spanning its group (`FP0R-E8
Expansion Unit` beside six rows). Where the band shaded across the grid's
first row runs on past its left edge to the grid's far edge, its text stands
over the strip it adds (`Product name`), and words stand in that strip in
the rows below, the band's edge is the table's. A band merely wider than
its table, with no name over the strip or nothing under it, adds nothing,
and no grid is widened onto another.

A table left open at the top can set a band over the columns its figures
share (`BOLSAS ADICIONALES NECESARIAS POR CADA 100 PIES DE LA LÍNEA DEL
TECHO` over three roof pitches) and rule under it across those columns
alone. The column of labels beside them (`VALOR-R`) stands open from the
band's top down through the header, and its rule runs on up beside the band.
The rules alone start the grid under the band, which then reads as a heading
over the table, and each pitch is named by itself. Where the top rule runs
over a group of two columns or more and leaves only edge columns open, and
the rule at the group's open side runs on above it, the grid is raised to
where that rule ends. The band's text must stand over the group alone: a
title running on over the label column is the table's. The band is then the
top of a stacked header (`BOLSAS ADICIONALES … / 4:12 INCLINACIÓN DEL
TECHO`), and the label column a cell from the band's top down.

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
- or it is one cell across all of the table but columns too narrow to hold
  a label set across them, 2.5 ems of its text together: a margin, or the
  room a label turned down the rows takes, whose rule may run on up through
  the band (`Average Luminance Table (cd/m²)` over every column but the one
  `Vertical Angle` is turned in). A line turned down a column takes about
  two ems, a word set across one three or more; a group's label over part
  of the table leaves columns a label fits;
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
some of the columns (`Dates` over `Start | Next review`).

A table read from its text takes a title band the same way, under stricter
conditions:

- the band is read whole across the row;
- it is set as a title: opening on a capital or a figure, with no colon;
- it sits at the table's left edge or is centred on it;
- the reading took it for the whole header, not for a banner over column
  names (`December 31, 2017` over `2017 | 2016`).

A catalogue's band (`ENGINE BEARINGS` over `Rod Bearing | 2001-2002 | …`)
opens such a table where the page cut its list at the band. The table goes
on from the row under it, with its spans and row bands moved up a row. The
looser test the ruled grids use took, in tables read from text, lines
wrapped from the header above, cut-off values and field lines for titles.


## Two-row headers and row groups

A ruled table says where a header label spans columns: the ruling leaves them
open under it. When the row beneath names each spanned column with a label of
its own (`Primary Method` over `NISTmAb | Amgen mAb`), the two rows are one
header and each column is named by both. The labels must differ from each
other and hold no numbers, because data under a banner repeats itself
(`Short | Short`). Keys in figures name the columns as surely
(`Horizontal Angle` over `0° | 45° | 90°`, `Total` over `2025 | 2026`,
`Component` over `1 | 2 | 3`) where the row's own stub is empty and the rows
under it have one: a row of the data names its row in the stub. A span the
ruling leaves open over the stub's column, empty in both rows, names the
columns from where its label stands; a span with no label over it names
nothing. Each must be a cell of its own: a value run across the
banner's columns (`3.1 mg in 2 mL` under `Strength`) names nothing. And where
the label above spans every column it is as often a band titling the rows
beneath (`Monotherapy` over `Adult patients with …`) as a header over the
column names (`Establishment` over `Name | Address | ID/FEI`), so the labels
under it must be short, as column names are. A column no label spans is named
by the first row alone: the second row leaves it empty, or the ruling runs its
label down through it. A value of its own there (`FPWINGRF-JP2` under
`Product No.`, beside two under a `Type` set across two columns) makes the
row the first of the data.

A header grows down through the rows a header cell is ruled open into
(`Nominal Screw Size` beside `Nominal Screw Length` over its five lengths),
as far as rows of labels go and a rule closes it off from the data. A stub
labelling header and body alike, its cell running on through every row of
the table (`Tolerance on Length` beside every row), is ruled under nowhere
there; the rule closing the header may leave its column open where the rows
taken in name their columns, each label its own, none repeated among them or
in the data under them. A table's data repeats itself (`Yes` row after row)
under a stub run down a continued page, and its rows stay data.

### A header restated partway down

A table can name its columns afresh partway down: a VOC table's `By wt |
By vol` under a `LB/Gal | g/L` header, or a SKU table's own header row under
a block of fields in the same box. Such a row sets names in two cells or
more, each its own, and no figure or part number, over a row or more with
something in a column it names. One name alone does where it stands in a
value column, the first column empty: a unit restated over the one column
the rows under it fill (`By wt` over `LVP-VOC | 0.0%`), set apart as below,
bold as a whole, and under a row of the data: straight under the header it
is the header's own label wrapped (`Body weight` over `(kg)`). Alone in the
first column, a name labels a group of rows. One row under a row of names is as likely
its value, but a header set apart names one row's columns as surely as ten:
a short VOC table's `VOC Content | LB/Gal | g/L` over its one `Total`. It is
set apart from the rows under it one of three ways:

- **Bold.** Every value cell is bold, two or more, over a row that is not. A
  first column of row labels keeps its own weight: `VOC Content` is set as
  `Total` is, beside a bold `LB/Gal | g/L`. One bold word beside a label is
  emphasis.
- **Shaded as the header row is,** over two rows that are not; striped rows
  alternate. The row and the header are shaded by fills of their own,
  starting and ending within them. A drawing laid under the page (a
  watermark's grey bolts) shades neither: drawn under a table's top rows and
  ending above the rest, it made the last row over it look like a restated
  header. The rows under it must be unshaded by any fill: a table laid on a
  background is shaded throughout, and no row of it stands apart.
- **Names turned on their side** in two cells or more, as narrow columns'
  names are. A group label turned down the rows it groups runs on past the
  row, and names none.

The row is not data. The rows under it, until the next such row, take its
names in place of the header's lowest row, under any rows above that one:
`K25W951 / By wt`. A band across the row is a section's label instead.

### Rows of parameters

A grid of figures can be headed by the rows of its parameters rather than by
names. An IES coefficient-of-utilization table opens with `RC %: | 80` and
`RW %: | 70 | 50 | 30 | 10` over `RCR: 0 | 119 | 119 | …`. Where nothing else
heads such a table, the rows that open it with a label ending in a colon and
only figures beside it are its header, over three rows or more, half of them
or more figures: each value is named by its parameters (`80 / 70`). So is a
sweep's `Speed (rpm): | 1500 | 2500 | 3500` over the readings taken at each
speed. The parameters' values stand over the columns the body's figures
fill, and the row nearest the body names two of them or more, half or more:
it sets the value each column of figures is for. A list of fields over a
grid of figures gives each field one value, and a table of such rows all
the way down is a list of fields, each with its values; none of their rows
heads it.

### Row headers under a group label

A row is named by its first column. Where that column is a group label set
down the rows it groups, a span (`Vertical Angle` turned beside the rows of
angles, a `Problem` over its causes), the rows under it are told apart by the
column beside it. A value further along is named by both, group first:
`Vertical Angle / 55°`. Only a label beside the group tells its rows apart. A
figure there is the row's value (`158,571`, two figures a slash parts,
`0 / 0`, or a bound, `<5%`), and the row is named by the group alone. Whether the
column beside carries a header of its own says nothing either way: a
`Probable Cause` column under its own header names its rows as surely as a
sub-property column under its property's.
Groups can stand over sub-groups, each set down a column of its own and
spanning fewer rows than the group it parts: each is named in turn, and the
column after them is the row's own label (`FP2 Backplane / Conventional
type / 7-module type (for master and expansion)`). A column spanning
exactly the group's rows says more of the group (a tip's colour, its
volumes carried down), and is read as the row's own label, as before:
`0.1 mL / white`.

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
would leave a fragment, so none of it is taken. A table that draws its
columns bounds its header's cells itself and is left alone.

A rule across the whole table is its top edge, and what stands over it is
its title, unless it is the table's column heads. A table can rule under its
header and not over it: an exposure-limit table's `OSHA` over `TWA` and
`NIOSH` over `STEL`, each over its column, a heavy rule under them. Its grid
begins at that rule, and the heads were read as text, the bold ones as
headings, with no value named. The lines just over such a rule are the
table's top row when the nearest sits within a line of it, and every run of
text on each line stands within one column, inside the table's width, in
two columns or more. Lines further up come with them while they are set as
close as a head's wrapped lines and stand in the columns the same way. A
single run there is a head wrapped onto two lines: over a column past the
first, or over the first where it starts where the head under it does, in
its size and weight (`Supp. Figure 3I: Levels of α-KG` over `with WT or
mutant IDH1`, the stub's head beside `Mean | SD | N`). A line that runs
across a column's edge or past the table, a title, a caption or prose,
stops the header there.

Such a table often shades its header's cells and then every other row from
the first. The shaded run at the top then takes in the first row of data
as well. Where the rows from the run's last row on are shaded every other
row, and that row holds figures as the rows under it do (`ethanol | 1000 |
- | 1000`), it is the body's first stripe, and the header ends above it.

A row that holds only a label in its first column, drawn as cells of the
table rather than a band across it, labels the rows beneath it when the row
below fills every column. The full row is what tells a group from a row whose
values are missing: a receipt's `TOTAL` with no amount sits over `Cash` and
`Change`, as sparse as it is. A label wrapped onto the next row (`Median in`
over `months (95% CI)`) is carried on, not a group, and a two-column list is
left alone, since a field left empty there looks the same. A label over
another such label is a group over its sub-group (`Power Management` over
`Average power requirements (W)` over `Operational | 6.7W | …`), and the
rows under both carry both.

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

A list of fields can set more than its names bold: a field's name and the
size each value is for (`PRODUCT CODE | 8′2" | 1198826`), both bold on every
row beside values bold on none. Leading columns bold on every row, the rest
bold on none, the first row's included, make a list of fields whatever their
number. A header row names every column in its weight, the values' too, and
keeps its place.

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

### Tables of three columns or more in one column of a page

A table set in one column of a page of several has the same trouble as a
field list there. Across the page each of its rows shares a baseline with
the other columns' lines, so the aligned reader never finds them a run. It
is looked for between the gutters too, only on pages of more than one
column:

- It starts at a row whose first cell opens its page column, with three
  cells or more before the next gutter.
- It runs on down every row with as many cells, each set where the first
  row's cell is: starting with it, ending with it, or centred under it. A
  header is often centred over figures set right (`oz. per gallon:` over
  `0-7` and `10-20`).
- A header may sit a line further from the rows under it than they sit from
  each other.

To be read it needs:

- at least three rows of short cells, few of them sentences;
- either a header row or a column of quantities. A header row is labels each
  ending in a colon, or a bold row over rows that are not, or a row naming
  columns of quantities.

A table wider than one column has wide gaps of its own between its label
column and its figures, and they read as the page's gutters. So a run is not
read here when, on half its rows or more, the cell just across a gutter is
short: under a third of that page column's width. That is a table running on
past the gutter, not a column of text beside it.

Once such a table is taken out, the page's prose is read again within the
gutters it was found between. Found afresh without the table's rows, a
gutter can fall somewhere else, and the two columns' lines are then read
across each other.

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

A column no cell starts in is dropped once the spans are read: on every row,
its cell is empty and the rest of the one to its left. The grid cut it where
another row's ruling, a band over the header or a nested table, parts the
columns, and it carries nothing of its own. An environmental data sheet's
three-column VOC tables were ruled as six and written with an empty column
after each, their cells in columns 0, 2 and 4. A column that is merely
empty, its cells never merged into their neighbours', stays: the page draws
it. A table the page break split drops a column only where every part does,
so its parts keep one set of columns and read on as one table.

A value set once across data columns that each have their own header is
each column's, as a value spanning rows is each row's. A spec sheet centres
`2 years` across its two models' columns, and DoCO wrote it under the first
alone: the second model had no warranty. The reader marks such a run
(`spread_left`) by where its value stands: its only text, worded, centred on
the run and narrower than most of it. A run the ruling merely leaves open
holds its value in its first column (a statement's `$ 11,452`, the next
columns unruled on that row), and figures alone across columns are the
grid's cuts off the page's. DoCO then writes the value under each column of
the run, where each has a header of its own; under one shared header, or in
a band or a restated header row, it stays one cell. Columns that are one
stacked header's children are that header's attributes, not things of their
own: a software product's `Terminal GTWIN CD-ROM` set across the
`Description` group's `Screen size` to `SD memory card slot` is none of
them, and is written once, named `Description`.

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

A table read from its text, a line to a row, shows the same groups. Each is
ruled off by a rule across the table, and some columns are set once on the
group's first line while another column fills every line: a tip size and its
colour over the four volumes tested with it. A column with a value on the
first line and none on the others spans the group, on the same conditions:
rules across the table above and below it, two groups or more, and a column
that fills every line.

### Separate values in a merged block

Where the ruling leaves two columns unparted, a group label spans rows down
the first, and the block's top row sets a value of its own beside the label,
the block is two spans crossing. A row with nothing of its own
under the group label sets its own label in the column beside it, and it
stays there (`Smoke developed` under `Surface-burning characteristics`). A
cell left holding text of its own beside another's after the block is read
is a value, unless the two read as one across the cut between them (an
address label a column cut runs through, beside its `Apt. no.`). It does not
carry on the cell to its left, so DoCO names its row by the group and by it
(`Surface-burning characteristics / Smoke developed`) rather than joining the
two. Cells that do read as one are one cell, their text read whole into the
first: a value set across two model columns beside its row's label
(`11/12 seconds (ventilated); 10/12 seconds (refrigerated)`), in a table ruled
with horizontals only that breaks every row line at the same edges.

A row the ruling leaves open across the table is a section's band only where
it does span: in a table ruled with horizontals only every row is left open,
and a row whose values stand apart in their own cells is a row of values.

### Shading parts rows

A shaded row can be parted from the next by nothing but the edge of its
shading. A header shaded down to its first row, with no rule between them,
was read as one row with it, so the header named every column after the
first product (`Part No. AFPG430`). A rule drawn under one column only, with
the shading ending on the same line over the others, left those columns open,
and a merge read the shaded labels and the first values as one cell
(`IVD-MD 75002576`).

So the edge of a fill parts rows as a rule there would, in two places:

- **In the grid.** A band across nine tenths of a table's width, from one
  of its edges to the other or hanging from one of its lines (a header band
  from the top rule, a shaded row from the rule above it), whose edge falls
  inside a row, a cell's height from its lines and through no line of text,
  is a row boundary too. A band drawn a cell at a time, one fill a column on
  one line, is one band: a spec table's section bands (`Performance`,
  `Power Management`) have no rule over or under them. Such a band, hanging
  from no line, parts a row from it only where that row holds text: a
  heading's band set in the blank between two field lists (`Dimensions`)
  parts nothing, and the blank stays the gap between the two tables. A
  band from edge to edge set just over the table's first rule, a row at
  most between them, is the table's top where it names two of the table's
  columns or more, each name a run of text inside its column: its header
  shaded, its rules drawn only under the rows below. A title band over a
  table (`Technical data`) is one run of text, however many columns its
  words fall across, and stays its title. Only inside a grid the rules make, of two rows and two columns or
  more: an edge never makes a table where the rules make none, nor parts a
  chart's frame, its legend boxed over the plot, into rows.
- **In the merges.** Where a rule is drawn under some columns of a row
  boundary, a fill ending on that line over the others closes them. Its
  sides must stand on column edges, as a cell's shading does, not a
  highlight set inside a cell behind a line of its text. Never alone: a
  boundary no rule is drawn on claims no span.

An edge counts only where the shading stops. A column shaded as one block is
often drawn a fill to a row, edge to edge, and the seams are not seen: a fill
butted on to another across the line parts nothing. A background laid under
the whole table is no such fill, and the band on it still shows. And not a
table striped row by row: its bands are a row or so apart, their edges are
no rules, and it is left to the rules it draws. Bands set rows apart are a
header or sections, and part rows.

A path that fills a table's shaded cells, drawn as one, is a fill each.
Read whole, its box was the size of the table, and the bands it shades were
none. Only cells: every rectangle too thick either way to be a rule, set
beside another on its line touching it, none inside another, and the bands
they make rows apart, a table's sections or its header. A chart's bars stand
apart, a glyph's `=` is two bars, a frame is a rectangle inside a rectangle,
and stripes down a table, drawn a cell at a time, are a row apart: all are
read whole, as before.

A table ruled with horizontals only is read from its text, its rules' ends
not taken for columns, unless it is ruled row by row: a line at every
boundary between its bands, and no band holding two rows of their own. The
edge of a band shaded across the table, edge to edge, is such a line, as
plain as a rule; stripes are not, nor a row of cells a group's cell beside it
leaves short. And a band whose values each wrap under themselves under a
labelled line (`1.47 lb (.67 kg)` over `±10%`), the first column left empty,
holds one row, not two; lines with no label under none (a statement's
header, years over units) are lines of their own.

Read from its text, a table runs on through lines that carry figures in its
columns. A line ruled off above and below, after a line ruled the same way,
holding its label and a value that measures something, is a row too, whatever
the value says: a list of dimensions ruled under every row keeps `Packaging
(W x H x D) | 31.4 cm x 10.9 cm x 15.7 cm` among its lengths and weights.
Not a header ruled off as its rows are (`(millions) | 2017 | 2016`), which
names no measure and starts the next table, nor a header ruled above and
below under a table ruled only round its own.

A table ruled under its rows with horizontals alone, with no vertical and
its rows not lumped, standing beside lines of the page's text on half its
rows, is set in a column of its own beside a column of prose. The prose's
columns are found with its text counted in, as it would have been had the
table been prose. Read without it, the column it stands in can end higher
on the page, and the prose below would be read straight across the page
instead of down that column.

The test is made on the ruled grid that seeds the table read from its text;
in practice only such seeds give tables of this shape. Made on every table
the page's readers keep, it changed two documents more, one for the better
and one for the worse. Other tables are left out. Counted in, a table's own
gutters read as the page's columns and part the text that crosses them (a
title over an invoice table read `MLS-721 40`), and a table across a gutter
the prose leaves open closes it.

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

Nor is a long cell a sign of a frame when the wider reading lies within one
row of a ruled grid of four rows or more: that is a grid nested in the row
(a property list whose acoustic row sets nine columns of figures in its
Performance cell), and the ruled grid stands. A frame's table runs down
across the frame's rows, or the frame has too few rows to tell. A reading
with three times the ruled grid's cells still replaces it as before.

### A grid nested in a ruled cell

A cell of a ruled table can hold a table of its own: an acoustic row's
Performance cell sets nine columns of figures under their own header,
`Thickness (in.) | 125 Hz | … | SAA`. In a ruled table of four rows or
more, most of them a table's rows (two cells or more filled, each short),
each cell that holds twenty glyphs or more is read for alignment on its
own. Text lined up into three columns or more over three rows or more,
taking nearly all of the cell, its first row naming the columns over rows
of figures, is a table of its own. A cell's figures stacked line under line
under the outer table's header line up into columns too, but name nothing,
and stay in the cell. It is read after the table it sits in, and the
cell keeps only what that table does not take. A band under its header
names the property it sits under (`Acoustical Performance`), so its cells
carry it as their section. Wrapped prose in a cell lines up into no columns.
A frame round a table set in one of its cells is read as before. In the
reading order, a table set inside another's box follows it.

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
