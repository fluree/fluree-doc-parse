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
under it must be short, as column names are.

A row that holds only a label in its first column, drawn as cells of the
table rather than a band across it, labels the rows beneath it when the row
below fills every column. The full row is what tells a group from a row whose
values are missing: a receipt's `TOTAL` with no amount sits over `Cash` and
`Change`, as sparse as it is. A label wrapped onto the next row (`Median in`
over `months (95% CI)`) is carried on, not a group, and a two-column list is
left alone, since a field left empty there looks the same.

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
