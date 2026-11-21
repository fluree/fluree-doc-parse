# DOCX, PPTX, XLSX, HTML, Markdown and plain text

Four of these **declare** their structure. A `w:pStyle` names the heading level;
`<h1>` is a heading because it says so. So these readers map rather than
measure, and nothing they produce is a hypothesis a model tier could improve.

Two consequences apply to all of them:

- **No geometry.** `bbox` is absent — not zeroed. See [Measured vs declared
  structure](../concepts/geometry-vs-declared.md).
- **No escalation.** There is nothing to arbitrate.

And what a file declares about itself goes on the [DoCO](../formats/doco.md)
document node: an Office file's core properties (`dc:title`, `dc:creator`,
`dcterms:created`, `dcterms:modified`) and an HTML page's `<title>` and
`<meta name="author">` become `dcterms:title`, `dcterms:creator`,
`dcterms:created` and `dcterms:modified`. A date that is not a real one is
left out.

## DOCX

```bash
fdoc convert report.docx -f doco
```

Word states outright what a PDF makes us infer: `w:pStyle` gives the heading
level, `w:numPr` marks a list item, `w:tbl` bounds a real table, and
`w:gridSpan` / `w:vMerge` state the cell merges the PDF engine has to read
back out of ruling geometry. A paragraph in Word's built-in Title style is the
document's `doco:Title`, found by the style's name, which a localised Word
keeps in English while it renames the style's id.

List numbering is one thing Word does not store: a clause numbered `9.1.5.`
holds no number in its text, and Word computes it when it lays the page out.
The reader computes it the same way and puts it at the head of the paragraph,
as Word shows it, so `9.1.5. Record the reading` reads as it is
cited. That covers numbering on the paragraph and numbering a paragraph style
carries, multi-level labels (`%1.%2.%3.`), decimal, letter and roman formats,
legal numbering, where a list restarts or carries on, and a numbered
paragraph with no text, such as a step column's `1.` and `2.`. Bullets add no
text. Formats spelled out in words (`First`, `Second`) are written as
numbers.

A document whose outline is only in its numbering is read for it. Many
procedures use no heading style: `1. SCOPE` and `4.2. Sample Handling` are
list paragraphs like the steps under them, set apart only by being short and
worded as titles. A numbered paragraph that reads that way and heads
something (items numbered deeper in its own list after it, or text before
its next sibling) is a `doco:SectionTitle` at the level it is numbered at,
and its section contains what follows. One that heads nothing is a step
written in capitals (`DO NOT PROCEED`) and stays one.

A title is set in capitals or in title case: `SAMPLE HANDLING and STORAGE`
and `Storage Conditions and Limits` both are, with articles,
conjunctions and prepositions in lowercase, and an aside in parentheses may
say anything. `Check the label` is a step, whatever follows it. Title case is
the weaker sign, because a list of deliverables or of roles is in title case
too, and one of its entries with a note under it heads something. So an
entry in title case is a section title only when most of its siblings (the
same list, at the same level, under the same parent) are titles that head
something, as an outline's sections are.

**Headers and footers.** A controlled document keeps its number, revision and
title in its page header, and its body never repeats them. The header Word
shows is read once, before the body, and the footer once, after it, as
paragraphs and tables whose `evidence` is `header` or `footer`, so a consumer
that wants the body alone can leave them out. A page number is the page Word
last laid the file out on, whichever that was, so its field contributes
nothing: `Page 3 of 7` reads `Page of`. A picture in a header is its logo, and is
left out.

**Footnotes, endnotes and comments.** A citation is often only in a
footnote, and a reviewer's question only in a comment. Each footnote and
endnote the body cites is read after the body, in the order the body first
cites it, its paragraphs marked `evidence` `footnote` or `endnote` and its
first one opening with the label Word shows for it: `1`, `2`, … for
footnotes and `i`, `ii`, … for endnotes, or the format the document sets
(`w:footnotePr`, `w:endnotePr`). The reference mark in the body adds nothing
to the body's text, so the body reads as it did. A note no reference cites,
or cited only inside a tracked deletion, is not shown by Word and is not
read; Word's separator lines are no notes. Comments come last, after the
footer, since Word does not print them: in the order the body anchors them,
then any anchored elsewhere, each `evidence` `comment` and opening with its
author (`Sam Example: Source?`). A heading inside a note is read as a
paragraph, and a picture in one is left out. Footnote reference marks set
as custom symbols, and comment replies' threading, are not read.

**Line breaks.** A line break (Shift+Enter) ends a line, and the line stays
one: `4.1 Equipment` and `4.2 Materials` are two lines of one paragraph. Two in a row leave a blank line, which parts the
paragraph as an empty paragraph would, so a numbered heading, a blank line and
a list of contents under it are a heading and the paragraphs after it.

**Symbol fonts.** A character from Symbol or Wingdings is stored as the font's
own code for it, which in any other font is nothing. It is read as the
character it shows: `°`, `±` and `μ` from Symbol, arrows, check marks and
boxes (`→`, `✔`, `☐`, `☑`) from Wingdings and Wingdings 2. A PDF made from
Word is read by the same table.

**Pictures and embedded objects** — an image, a chart, an object embedded
from another program — are each a `doco:Figure` where they stand, its text
the alternative text the document gives it, less the notice Word appends to
text it wrote itself. One with none is an empty figure: it marks where the
picture is, in the graph and in `-f json`, and adds nothing to the text. Word
also keeps a fallback copy of a drawing for readers that cannot show it,
which is not read a second time.

**The text in a picture** — a screenshot pasted into the report — is read by
the rules a [bare image](images.md) is read by. `fdoc dev render-routed`
writes the image as `<stem>_m<n>.png`, reason `embedded_image` (`small_image`
under the floor a model is sent, below), for the
figure `figure-0-<n>`, and each reader takes it under that name.

First, the pixel probe a PDF's raster regions are routed by
([the router](../design/router.md#the-pixel-probe)) looks at the picture.
One it does not take for text (a photograph, a chart, a logo, a flat
illustration), or too thin for a line of text (under 10 pixels on its
shorter side, the floor a PDF's region is held to, under which an engine
misreads what it reads), is read by nothing: neither OCR, which reads noise in such
pixels, nor a model, whose call is billed and sends the picture off the
machine. It is listed in
[`doc:unreadImages`](../formats/doco.md#images-nothing-read) as
`NotTexty`, not counted in the note on stderr, and `render-routed` writes no
crop of it. A capture, a scanned table or a labelled chart is taken for
text, a capture with a few lines on a wide, light screen included. A
picture that does not decode is not judged. The others go on:

1. **OCR**, when `convert` has an engine (`--ocr`) or the words one wrote
   (`--ocr-words`, `<stem>_m<n>.json`). What it reads is held to a noise
   rule, a net behind the probe for a photograph or a gradient it took for
   text, as an OCR reading of a PDF's region is: words whose mean
   confidence is under 0.5, or under 0.8 when most of them are fragments of
   one or two letters or figures, or one fragment, or one word read under
   0.8, boxed over nearly all of the picture. A line of text cropped close,
   read surely, is kept. The picture is then not read: the configured model
   may read it, and otherwise it is listed in
   [`doc:unreadImages`](../formats/doco.md#images-nothing-read) as
   `NotTexty`, not counted in the note on stderr.
2. **A model's reading supplied** (`--tier-results`, `<stem>_m<n>.json`),
   which replaces what OCR read, as a scanned page's reading does, unless it
   lost most of OCR's letters (a truncated reply). With `--tier-results`,
   no model is called for any picture, even one it holds no reading of.
3. **The configured model** (`--escalate`, or a reader configured), for a
   picture OCR read no text in, or with no OCR at all; at most
   `escalation.max_crops` pictures a document (0 for no limit), the others
   noted on stderr and left unread. Only a PNG, JPEG or TIFF is sent. A
   document's pictures go through one reader, up to
   `escalation.concurrency` at a time, as a PDF's crops do.

   A picture is sent when it has at least 400 pixels on its longer side and
   48 on its shorter, as an e-mail's image needs, whatever size the
   document shows it at: a small logo or an icon is under that floor.
   With the probe, nothing else is judged: a sparse screenshot or a
   labelled chart over the floor is sent, a request the provider bills,
   even one the document shows an inch wide. One under the floor is listed in
   [`doc:unreadImages`](../formats/doco.md#images-nothing-read) as `Small`
   and, as a logo is, not counted in the note on stderr (`--verbose` names
   it). OCR and a reading supplied are not held to the floor: they read
   every picture with a line's pixels (see above), whatever size the
   document shows it at.

A group of pictures is one figure holding several images:
the second and later are `m<n>_1`, `m<n>_2`, … and their text follows the
first's. An image several figures draw (a logo, a capture pasted twice) is
read once, by the name of the first figure drawing it, and its text follows
each of them. What it reads follows the figure in the flow, marked
`provenance: "ocr"` (`"vlm"` for a model's reading): its paragraphs and tables, with no `bbox` (the image's
pixels are no position in the document), and a heading of the screen read
as a paragraph so the document's outline stays its own. A picture deleted
under tracked changes is no figure and is not read, and a shape's image fill
(a texture) is not one of its pictures. A picture whose image is not in the
package (a link to an outside file, a missing part) is counted among those
not read; a part name is matched ignoring case, with backslashes read as
slashes. With nothing to read them (no OCR, no `--tier-results`, no model
configured), a note on stderr counts the pictures not read, and only the
first bytes of each image are decompressed; a configured model is sent each
picture's bytes as they are. Either way each picture not read is listed in
[`doc:unreadImages`](../formats/doco.md#images-nothing-read), with its figure,
its part and why. An
image Word keeps as a vector drawing (EMF, WMF) has no pixels to read: told
by its first bytes, whatever is configured, it is listed there as
`UnsupportedFormat`, not counted in the note and not warned about. Images
are read one at a time, each at most 64 MiB decompressed and 128 million pixels
decoded; one past either is reported and left as its figure. A document as a
whole has at most 256 images read, holding 512 million pixels together, as
their headers state them: a few kilobytes of archive can hold hundreds of
large images, and past that allowance the rest are counted among those not
read, with a note saying why. In a library,
`fluree_doc_docx::parse_with_ocr` (feature `ocr`) does it with an engine;
`parse_with_media` says where each figure's images are, `read_pictures`
reads each image once within that allowance (`read_media` reads one part of
an opened package), and `fluree_doc_image::place_after_figures` puts what
was read where it belongs.

**A list set side by side with itself.** A long list is often set two-up to
fill the page: `Group | Code | | Group | Code`, the list running down
the left pair and on down the right. Read as one table, each row joins two
entries that have nothing to do with each other, and the right one takes the
left one's first cell as its row's name. A table whose header repeats its
labels after a column left empty all the way down (or repeats at least two
labels) is read as one table per group, one after the other.

`page` is `0` throughout — a `.docx` stores a flow, not a layout, and page
boundaries exist only once something lays it out.

What the document makes, its body, page header and footer, notes and
comments, is spent from one budget
([limits on what one document may make](../reference/limits.md)) as it is
made: each paragraph with its text, a table's grid before it is made (a
`w:gridSpan` of a billion asks for a billion cells), its cells and their
text, and the copies DoCO writes of them: a value merged down (`w:vMerge`)
in each row it covers, the fragments of a merged cell joined first, and each
cell's labels. A document past it fails as too large.

Returns `DocxError` for a malformed or non-archive file, and
`DocxError::TooLarge` for one past its budget.

### Word 97-2003 (`.doc`)

```bash
fdoc convert old-memo.doc -f doco
```

A `.doc` (or `.dot`) is read for its text, through its piece table: the
`WordDocument` stream holds the text in pieces, anywhere and in any order
after a fast save, one byte a character (Windows-1252) or two (UTF-16), and
the piece table says which run of the document each holds. What is read:

- The main text, as paragraphs at Word's paragraph marks; a line break keeps
  its line, and a page break parts lines.
- **Tables**, by each paragraph's own properties (in a table; the end of a
  row), as `doco:Table`s of their cells, a cell's paragraphs joined.
- A field shows its result (`PAGE` its last page number), not its code;
  a footnote reference in the body adds nothing to it.
- **Footnotes, endnotes and comments** after the body, marked by `evidence`
  `footnote`, `endnote` and `comment` as in a DOCX; a note opens with its
  number (`1`, `2`, … and `i`, `ii`, … for endnotes).

What this round leaves out, and says so here rather than guessing: styles
(so no heading, title or list is told apart from a paragraph, and list
numbers Word computes are absent), headers and footers, text boxes, comment
authors, a table's header row (`header_rows` is left undetected), merged
cells, pictures, and the summary information. Word 95 and earlier and
encrypted files are refused. Provenance and evidence are `doc`.

A file is recognised as a Word document by its content, a compound file
holding a `WordDocument` stream, whatever its name. As for any compound
file, the container is opened by the `cfb` crate, which refuses a sector
chain that loops or runs past the file; the `WordDocument` stream is read
to at most 256 MiB and the table stream to 64 MiB, and at most a million
pieces and 64 million characters, a piece table whose positions go
backwards (pieces that overlap, reading the same characters again) failing
as malformed. Each page of paragraph properties is read once however many
times the bin table names it, at most about four million runs of them, and
a paragraph finds its own by a binary search. The characters are read one
at a time, never all held; every character kept, element made and table
cell (a row padded to the widest included) is spent from the document's
budget ([limits on what one document may make](../reference/limits.md)) first, so pieces that all point at the
same text cannot make millions of paragraphs of it. Returns `DocError`
(`fluree_doc_doc::parse`) otherwise.

## PPTX

```bash
fdoc convert deck.pptx -f doco
```

The one declared format with a real page concept: **each slide is a page**, so
`page` carries the slide index and the graph keeps the deck's pagination.
Slides are read in the deck's order, from its slide list in
`ppt/presentation.xml`, not in the order of their parts' names: a slide moved
in PowerPoint keeps its part's name, so `slide2.xml` can be the first slide.

Geometry is still absent. Shapes do have positions in EMUs, but those describe
a canvas layout rather than a text flow, and reporting them as `bbox` would
invite consumers to treat a deck like a scanned page.

A shape whose placeholder type is `title` or `ctrTitle` becomes the slide
heading, except the `ctrTitle` on the first slide — the title slide's title —
which is the deck's `doco:Title`; `a:tbl` is a real table with `gridSpan` / `rowSpan` merges;
paragraphs with a bullet character or a non-zero outline level are list items.

**Charts become tables.** Values come from the cached `c:strCache` /
`c:numCache` blocks — what the chart actually plots — because the `c:f`
formula beside them points into a workbook that may not travel with the deck.
Each value goes to the category its point's index names: a cache leaves out a
point with no value, and a gap stays a blank cell rather than moving the
values after it onto earlier categories. Of a multi-level category axis, the
level next to the plot is read.

What the deck makes is spent from one budget
([limits on what one document may make](../reference/limits.md)) as it is
made, slide by slide: each paragraph with its text, a table's or a chart's
grid before it is made, its cells and their text, and the copies DoCO
writes of them (a value merged down in each row it covers, each cell's
labels). A span past the table's edge is cut there. A deck past its budget
fails as too large.

Returns `PptxError` for a malformed or non-archive file, and
`PptxError::TooLarge` for one past its budget.

### PowerPoint 97-2003 (`.ppt`)

```bash
fdoc convert old-deck.ppt -f doco
```

A `.ppt` (or `.pps`, `.pot`) is read for its slides' text. Its `PowerPoint
Document` stream is a tree of records saved edit after edit: the `Current
User` stream names the last edit, each edit the one before it and a persist
directory of where each object is, the newest saying where an object is
now. Through it, the slide list gives the slides in order, each a page; each
slide's shapes hold its text in text atoms (UTF-16 or one byte a
character). A title placeholder's text, by the shape's placeholder or by
the text's own header, is a level-1 heading; every other text is a
paragraph a paragraph mark, a line break kept as a line. A paragraph whose
properties (`StyleTextPropAtom`) give it a bullet or an indent level past the
first is a list item, as a PPTX paragraph is; where they say nothing, a body
placeholder's paragraph, which the master bullets, is one. The level itself
is not kept: the element model gives a level to headings only. A slide whose shapes hold no text, as an older writer
keeps it, is read from the slide list. A slide's **speaker notes** (the
notes slide its own atom names) follow its content on its page, as
paragraphs whose `evidence` is `notes`: what the presenter said, kept apart
from what the audience saw.

**Tables** are drawn in a `.ppt` as a group of shapes whose first shape says
it is a table: each other shape is a cell, placed in the group, holding its
text. The grid is rebuilt from the cells' edges, a cell spanning several
columns or rows making the cells it covers continuations (`merged_left`,
`merged_down`); border lines are no cells. PowerPoint declares no header
row, so `header_rows` is left undetected. A table of more than 10,000 cells,
or whose grid would pass a million, or whose cells overlap to cover more
than a million, is read as its cells' text; which it is is decided from
the cells' positions before any text is read, so a group is read once.

**Pictures** on a slide are each a `doco:Figure` where they stand, its text
the picture's alternative text where it has one, numbered across the deck
(`figure-<slide>-<n>`). Their bytes are in the deck's picture store (the
`Pictures` stream, or the store entry itself), and are read by the rules a
[DOCX picture](#docx) is read by: OCR (`--ocr`, `--ocr-words`
`<stem>_m<n>.json`), a reading supplied (`--tier-results`), or the
configured model, the text placed after the figure; `render-routed` writes
`<stem>_m<n>.png`; one nothing read is listed in `doc:unreadImages`. A
drawing (EMF, WMF, PICT) is listed but not read, as in a DOCX. In a
library, `fluree_doc_ppt::parse_with_media`, and `MediaReader` to read
their bytes, the deck opened once for all of them (`read_media` opens it
for one).

Not read: the masters' placeholder prompts, charts, embedded objects and summary information.
An encrypted file is refused. A file is recognised by its content, a
compound file holding a `PowerPoint Document` stream, whatever its name and
on stdin. The container is opened by the `cfb` crate, which refuses a
looping sector chain; the stream is read to at most 256 MiB, records nest
at most 64 deep, at most 4,096 edits are followed (each once, so an edit
chain that loops ends), each persist directory read once and about four
million of their entries in all, and 10,000 slides read. A slide or a notes
slide is read once however many ids the persist directory sends to its
place in the stream, and every text read, element made and table cell, and
the copies DoCO writes of a table's cells (a merged value in each cell it
covers, each cell's labels), is spent from the document's budget ([limits on what one document may make](../reference/limits.md)) first. A
picture several shapes show is read once by OCR or a model, its text placed
after each of its figures, as a DOCX's is.
Provenance and evidence are `ppt`. Returns `PptError`
(`fluree_doc_ppt::parse`) otherwise.

## XLSX

```bash
fdoc convert book.xlsx -f doco
```

A workbook is the odd one out: it declares its geometry and nothing else.
Every cell has an exact row and column — the one thing the PDF engine has to
measure — and no cell says what it is. A sheet is a grid on which someone
laid out a title, a few labelled fields, a table and a note, and reading it
as one table per sheet returns a mostly empty rectangle with the structure
flattened out of it. So this reader measures after all, but only shape.

- **Each sheet is a page**, in the workbook's own order, and its name opens
  it as a level-1 heading.
- **Blocks of cells are tables.** Occupied cells that touch along an edge
  form a block; an empty row or column between two blocks separates them,
  which is how a sheet's author separates them. A cell alone on its row at
  the top of a block is the block's title — a level-2 heading when it is
  bold or set larger than the sheet's body text, a paragraph otherwise —
  and one alone at the bottom is its note. A lone cell is a heading or a
  paragraph by the same test.
- **Header rows** come from the sheet where it says: rows frozen by a pane,
  or the first row of an autofilter range. Otherwise the first row is the
  header when it names every column and is bold, or names every column in
  text while later rows carry numbers. A block of labels and values has
  none, and says so: `header_rows` is `0`, not absent, because absent means
  undetected and a consumer then presumes one.
- **Merges** read as Excel shows them: a merged range displays its top-left
  cell and hides the rest, stale values included. The hidden cells become
  `merged_left` / `merged_down` continuations, and a merged cell across the
  full width inside a table is a `sub_headers` band. A merge is laid out
  only as far as the sheet's cells reach, and a sheet's merges cover at most
  about a million cells, those past it read unmerged, so a range declared
  over the whole grid costs no more than the cells under it. A block whose
  grid would pass about four million cells in all (cells set down a
  diagonal, touching each other, span the square of their number) is read
  as its cells' text, a paragraph each.
- **Bounds.** No cell past Excel's grid (row 1,048,576, column 16,384) is
  read, and a merge is cut at its edge. Every cell read, its text (a shared
  string once for each cell naming it), each copy the layout makes, each
  cell a table's grid holds, each element, and the copies DoCO writes of a
  table's cells (a merged value in each cell it covers, each cell's labels)
  are spent from the workbook's
  budget ([limits on what one document may make](../reference/limits.md)) before it is made, and the passes that
  mend islands into blocks spend its work; past either the workbook fails
  as too large. Past 2,048 islands a sheet's islands are not mended. The
  `.xls`, `.xlsb` and `.ods` readers lay their sheets out the same way,
  within the same budget.
- **Values are the cached ones.** A formula's last computed value is what
  the file holds and what Excel shows; nothing is recalculated.
- **A cell's text is what Excel shows.** A number is rendered through its
  format: fixed decimals (`14.50`), grouping (`1,234.50`), a currency sign
  or code beside it, negatives in parentheses, thousands scaled out,
  percentages as percentages. The built-in currency formats show grouping
  and decimals but no sign, because which sign is the reader's locale and
  not the file's. Dates and times are ISO where the cell's format says the
  number is one. A format this does not read — conditions, exponents,
  fractions — falls back to the shortest decimal that reads back to the
  value. Colours are not rendered.
- **What the cell stores comes with it, typed.** A table's `datums`, and
  `doc:cellDatum` in [DoCO](../formats/doco.md#table-cells-are-nodes), hold
  each number at full precision whatever its format rounds away (`0.12345`
  under a `12%`, `1234.5678` under `1,234.57`) as an `xsd:decimal`; a date
  or time as the moment its serial counts to, keeping a time of day the
  format hides, as an `xsd:date`, `xsd:time` or `xsd:dateTime`; a boolean as
  an `xsd:boolean`. A cell the file stores as text has none, digits or not:
  `00123` stored as text is a code, and stays one.

Hidden sheets, rows and columns are read like any other; a consumer that
wants what the author showed can drop them by name. `bbox` is absent: a cell
address is not a position on a page, and the address is the table's row and
column.

**A picture anchored to a sheet's cells** — a screenshot pasted into a
workbook — is a `doco:Figure` on its sheet's page, its text its alternative
text, placed among the sheet's elements by the cell its top-left corner is
anchored to, as a table is placed by its first cell: after what starts
above it or on its row to its left, before what starts below. A picture
placed by an absolute position, which names no cell, comes last on its
sheet. Figures are numbered across the workbook, `figure-<sheet>-<n>`, and
the picture's text is read and placed after its figure exactly as a DOCX
picture's is (see [DOCX](#docx)): `--ocr`, `--ocr-words` (`<stem>_m<n>.json`),
`render-routed` (`<stem>_m<n>.png`, with its package `part`), and
[`doc:unreadImages`](../formats/doco.md#images-nothing-read) for one nothing
read. Only pictures (`xdr:pic`) are read: a chart has its data in the sheet,
and a shape's image fill is a texture. A group of pictures is one figure
holding several images. Pictures in a header or footer, comments' shapes,
and Excel's pictures placed *in* a cell (`IMAGE()`, Place in Cell) are not
read. A workbook without pictures converts as it did. In a library,
`fluree_doc_xlsx::parse_with_ocr` (feature `ocr`) does it with an engine, and
`parse_with_media` and `read_media` list and read the pictures as DOCX's do.

Returns `XlsxError` for a malformed or non-archive file.

### Excel 97-2003 (`.xls`)

```bash
fdoc convert old-book.xls -f doco
```

A BIFF8 workbook (`.xls`, `.xlt`) is read by
[calamine](https://crates.io/crates/calamine) and laid out by the same code
as an XLSX one: each sheet a page opened by its name, blocks of cells tables
with their titles and notes, merges as continuations, rows frozen by a pane
a table's header, a bold first row a header. One thing reads differently:
calamine types a number as a date by its format and gives no other format,
so a number reads as Excel's General format shows it (`0.125`, not `12.5%`;
`1234.5`, not `1,234.50`), while a date reads as an ISO date. What each cell
stores rides beside it typed (`datums`, `doc:cellDatum`) as an XLSX cell's
does, and the workbook's summary information gives its title, author and
dates. A file is recognised as a workbook by its content, a compound file
holding a `Workbook` stream, whatever its name. Provenance and evidence are
`xls`. Pictures are not read.

calamine is not given the file. It reads a compound file with a reader of
its own, which follows a looping sector chain; it reserves what a sheet's
`DIMENSIONS` record declares, lays each sheet out as a grid from its first
used cell to its last, copies a shared string into every cell naming it,
and indexes record data without checking its length. So the file is read
first: its compound file opened by the `cfb` crate, which refuses a sector
chain that loops or runs past the file; its workbook stream at most
256 MiB; and its records walked as calamine would walk them, each one
calamine reads checked against what calamine indexes in it. A workbook
whose cells, sheets' grids or merged ranges pass about four million in all
(a sheet named twice counting twice), or whose cells would copy more than
64 million characters of text, fails as too large, as does one whose cells
pass the document's budget once read ([limits on what one document may make](../reference/limits.md)); one whose records are
cut short, or whose sheet offsets point at no sheet, fails as malformed.
calamine then reads a compound file written for it, holding a workbook
stream of only the records it needs, each sheet once and its formulas
emptied to their cached values, and the summary information where it is
under 32 KiB.

### Excel binary workbooks (`.xlsb`)

A `.xlsb` is a workbook saved as binary records in a zip. Its records are
read here, each checked against its length, rather than by calamine, which
indexes them unchecked; a sheet's cells are laid out the same way as an
`.xls`'s, and provenance and evidence are `xlsb`. Numbers read as General,
dates as ISO dates, as for `.xls`; bold cells, frozen rows and merges are
not read. Every part is first inflated to nowhere against 64 MiB each and
256 MiB in all (and at most 10,000 parts), and lists at most 10,000 sheets;
a sheet part is read once however many times it is listed, at most about
four million cells are read, a cell past Excel's grid is none, and each
cell and its text (a shared string for each cell naming it) are spent from
the document's budget ([limits on what one document may make](../reference/limits.md)) before they are copied. A file is
recognised by its content, a zip holding `xl/workbook.bin`, whatever its
name and on stdin. `.ods` is read by its own reader
([OpenDocument](opendocument.md)).

Returns `XlsError` (from `fluree_doc_xls::parse` or `parse_xlsb`) for a file
that is not a workbook, is malformed, or is past a bound.

## HTML

```bash
fdoc convert page.html -f doco
```

HTML declares structure, but unlike Markdown or OOXML it also carries a great
deal that is not document content — navigation, scripts, styling wrappers —
and real-world markup is frequently malformed. So the parse is spec-compliant
(Servo's html5ever) and the walk is **selective**: non-content subtrees are
dropped whole, and only elements naming a document role are emitted.

What is left out, and why:

- **Never content:** scripts, styles, `<nav>`, `<iframe>`, `<svg>`,
  `<dialog>` and form controls (`<button>`, `<select>`, `<textarea>`). A
  `<form>` itself is read, because older pages and every ASP.NET page wrap
  the whole body in one.
- **Undrawn:** the `hidden` attribute (but not `hidden="until-found"`, which
  find-in-page reveals), inline `display:none` or `visibility:hidden` — how
  email preheaders are set — and the screen-reader-only classes of the common
  style sheets (`sr-only`, `visually-hidden`, `visuallyhidden`,
  `screen-reader-text`), whose text sits inside a visible phrase and would
  join its words ("the report(opens in a new window)"). `aria-hidden` text is
  drawn, so it stays.
- **Furniture:** the `navigation`, `search`, `dialog` and `alertdialog`
  roles, and an `<aside>` that belongs to the page rather than to an
  `<article>` or `<section>` — browsers make only such an aside a
  complementary landmark, so a sidebar of other stories goes and a factbox
  inside the story stays. The `banner` and `contentinfo` roles are kept:
  mail templates put the sender's name and address in a visible
  `contentinfo` footer.
- **Outside `<main>`:** a page that marks its main content (`<main>`, or
  `role="main"`) is read for that alone; its site header, menus and footer
  lie outside it. A `<main>` with no text is a marking error, and the page is
  read whole.

Text in the C1 control range (U+0080–U+009F) is read as the Windows-1252
character it stands for: it appears when a page in Windows-1252 was decoded as
Latin-1, which is how `l’art` becomes `l\u0092art`. A file is decoded by its
byte-order mark, then as UTF-8 when its bytes are valid UTF-8 (a saved page is
often re-encoded while its `<meta>` still names the old charset), then by the
charset it declares, resolved as browsers resolve labels (`iso-8859-1` reads as
Windows-1252), then as Windows-1252.

Nesting resolves **innermost wins**. A `<p>` inside a `<td>` inside a
`<table>` is table content, not a paragraph — emitting both would duplicate
the text, the same double-emission the PDF engine guards against when a grid's
glyphs would also become prose.

Text set straight in a container, with no `<p>` around it, is read the way a
browser lays it out: each run of it between blocks is a paragraph, and two
`<br>` in a row end one. That is how pages built from `<div>`s, and nearly
all email, set their text. A `<blockquote>` that holds paragraphs of its own
is their container rather than one paragraph run together.

A `<pre>` is read as written: one paragraph whose text keeps its line breaks,
blank lines and runs of spaces, so offsets index into the text as it was set.
Government publishers serve whole documents as one `<pre>` (every hearing and
public law on govinfo.gov, many SEC exhibits), with the letterhead centred and
the lists set in columns by spaces alone. Only what is never drawn is dropped:
the whitespace before the first character and after the last, and the spaces
a line ends in.

A page that declares no headings but shows them, as statutes and contracts do,
has them read by their shape. The Official Journal sets a regulation's
outline in classes of its own, and a filer sets a contract's in centred bold,
with no `<h1>`–`<h6>` anywhere. A paragraph that is nothing but a structural
keyword and its number becomes a section title: `CHAPTER IV`, `Section 1`,
`Article 30`, `ARTICLE II`, `ANNEX III`, `§ 7002`. So does one that adds its
own short title after a number closed by a full stop (`§ 7001.
Definitions.`, `ARTICLE 1. GENERAL PROVISIONS`), and a code's section number
alone (`1798.100.`). The keywords are Book, Part, Title, Division, Chapter,
Article, Section, Annex, Appendix, Schedule and Exhibit, and their sub-
forms. A short paragraph after a heading with no title of its own that
reads as a title is joined to it (`Article 30 Records of processing
activities`), and so is a title in capitals broken mid-phrase over two
paragraphs. A title on the heading's line that stops short of its full stop
takes the next paragraph when that one ends it. Such a heading carries the
evidence `keyword`.

The levels come from how the document nests its keywords, not from a fixed
table. A regulation sets articles inside sections, and a contract sets
sections inside articles. In each case the outer keyword is the rarer, so
keywords rank by how often they occur, divisions (part, title, chapter) above
units (article, section). An annex or schedule sits beside the outermost
division, or above the articles where there are no divisions, so an
agreement attached to another keeps its own articles. A label that comes
before any of the body, such as the `Exhibit 10.1` a filing is made under,
names the document and is left as it is.

A heading set at the start of its own text (`Section 2.1 Closing. Unless this
Agreement…`) stays a paragraph. A page that declares any heading keeps its
headings as declared, and an email body is never read this way.

A table used for **layout** is read as the containers its cells are, not as
data. Email, and many older pages, are laid out in tables: a column of
boxes, a logo beside a banner, tables nested to centre a column. Read as
data, a whole message would become one cell. A table is laid out when:

- it is marked `role="presentation"` or `role="none"`,
- it holds another table,
- it has one row, or
- it has one column and no header (`<th>`, `<thead>`, `<tfoot>`, `<caption>`).

Every other table is data. The test follows the one browsers use to decide
whether to announce a table to a screen reader. A cell marked `<th>` does
not make a table data when it holds a table or sits in a single row,
because email frameworks set their columns in `<th>`. A data table inside a
layout table is still read as a table.

A data table's **header** is the rows at its top that label the columns:

- With no `<thead>`, the leading rows whose text is all in `<th>` cells. An
  empty `<td>` before them, the corner over the row labels, does not make
  such a row data; a `<th>` label beside empty `<td>` values does.
- A `<thead>` ends the header, except that rows of several `<th>` at the top
  of the body go on with it: a statistical table sets its units there
  (`thousands of units`, `% change`) under the periods. A single `<th>`
  across the table there is a band over the rows beneath it.
- A filer's table, as an SEC filing sets it, writes its header in `<td>`
  cells, bold or ruled beneath (`font-weight: bold`, `border-bottom` of
  any width above zero, `0.5pt` included), and nothing else so. Leading
  rows of two such cells or more are the header when rows of plain values
  follow. The units caption in the label column, `(in millions)`, may stand
  in plain text beside the heads.
- One such head over the value columns is a header row too, with the label
  cell blank or holding the units: `Annual Output` over a pair of columns,
  a contents page's `Page`. It needs figures under it, and the row after it
  must not be a styled row with a label of its own: a signature over a
  name, or a contents page's first bold entry, is not a header. A lone
  figure ruled beneath is the line before a subtotal, not a head.
- A single bold label in the first column (`Revenues`) is a band, not a
  header row, and bold or a rule on every row is emphasis or a grid.

Under a header of several rows, a column's `doc:columnHeader` is its labels
from the top down, joined with ` / ` (`Three months ended September 30, /
2018`). A label spanning columns names each of them, so a value set beside
its currency sign is under the period above both.

A raised note mark is kept apart from the word it marks: `April 2026<sup>r</sup>`
reads `April 2026 r`, and `Expenses<sup>1</sup>` reads `Expenses 1`. So is a
number raised after a quantity (`26TB<sup>1</sup>` reads `26TB 1`) or after a
bracket that holds words (`(49 N)<sup>5</sup>` reads `(49 N) 5`). Raised
text that belongs to the word stays in it: a unit's power
(`m<sup>2</sup>` reads `m2`, `PIES<sup>2</sup>` reads `PIES2`), an exponent (`10<sup>6</sup>`, `106`), an
ordinal's ending (`1<sup>st</sup>`, `1st`) and a formula's index or power
(`2<sup>p</sup>`, a raised letter after a single letter, a term of a formula
set after `=`). PDFs follow the same rule for the raised glyphs they draw.
A mark written as a link to a note on the same page, rather than in a
`<sup>`, is read the same way: `governments<a href="#fn01"
class="sd-tablefnref">1</a>` reads `governments 1`. The link has to say it
is a note's, by `role="doc-noteref"` or a class, id, target or label naming
a note (`fn`, `note`, `foot`); any other link's text reads as written.

In Rust, `fluree_doc_html::parse_with_quote_depth` returns beside the
elements how many cited quotations enclose each one: a `<blockquote>` with
`type="cite"` or a `cite` address, Gmail's `gmail_quote`, and the containers
Yahoo and Proton put around a quote. A plain `<blockquote>`, as an indent
button writes one, does not count. The [email](email.md) reader splits a
thread by it.

Every byte sequence parses, as HTML defines it; what a page makes is spent
from one budget ([limits on what one document may make](../reference/limits.md))
as it is made: each element with its text, a table's grid before it is made
(a `colspan` of a billion asks for a billion cells), the cells each span
marks, the table's cells and their text, and the copies DoCO writes of them
(a value spanning rows in each row it covers, each cell's labels). A page
past it is refused: `parse` and `parse_with_quote_depth` return its
`TooLarge` error, and an email whose HTML body is refused fails as too
large.

## Markdown

```bash
fdoc convert notes.md -f doco
```

The most direct mapping. A heading states its level, a list states its items,
a table states where its header ends. Where the PDF engine reports a
*measurement* — `header_rows` inferred from shading and value types — this
reports a *fact*.

Infallible, for the same reason as HTML.

## Plain text

```bash
fdoc convert statute.txt -f doco
```

A text file (`.txt`, `.text`) declares nothing but its lines. A blank line
ends a paragraph, and the line breaks inside one are kept. A line that starts
with a list marker (`-`, `*`, `•`, `1.`, `2)`) is a list item, and the lines
after it continue it until the next marker or a blank line. A bullet is
dropped and a number kept, because a clause's number is how it is cited.

It is not read as Markdown. An indented line is not code, a line underlined
with `=` or `-` is not a heading, and `*`, `_` and `<…>` are characters.
Plain text underlines labels, signatures and column headers, and Markdown
would make a heading of each.

Its headings are the ones it shows, read as for an [HTML](#html) page
without heading markup: a line that is a structural keyword and its number,
with or without its title (`CHAPTER IV`, `§ 7001. Definitions.`,
`1798.100.`). Such a line at the top of a paragraph is set apart from the
lines under it, so a heading with its title on the next line reads as a
heading and its title rather than as one sentence wrapped over two lines.
Further down a paragraph, such a line stays where it is: there it is more
likely a reference wrapped onto a line of its own.

Markdown is often saved as `.txt`, and a file that writes Markdown's markup
is read as Markdown: two headings marked with `#`, the rule under a table's
header (`|---|---|`), a fenced block, two phrases in `**bold**`, or a link
written `[label](address)`.

A file is decoded by its byte-order mark, then as UTF-8 when it is valid
UTF-8, then as Windows-1252. Windows (CR LF) and classic Mac (CR) line ends
are read as line ends.

Infallible: any sequence of bytes is some text.

## Why convert Markdown at all

It looks circular until you want the graph. `fdoc convert notes.md -f doco`
gives you DoCO typing, explicit section containment, addressable table cells
and char offsets over a Markdown file — the same graph shape a PDF produces,
so a mixed corpus lands in one ledger with one schema and one query surface.
