# Limits on what one document may make

A reader bounds its input (bytes read, parts inflated, records nested), but
that does not bound what it makes of it. A cell repeated across a row, a
string shared by every cell, a slide listed a thousand times, pieces of a
Word file that all point at the same text, a table whose first row is
thousands of cells wide over rows of one, a cell merged down over thousands
of rows: a file of a few kilobytes can describe gigabytes. So the readers
of the office formats (`.csv`, `.xls`, `.xlsb`, `.xlsx`, `.ods`, `.doc`,
`.docx`, `.odt`, `.ppt`, `.pptx`, `.odp`, `.rtf`), of Markdown, of plain text and of HTML
(a page, or an email's HTML body), their parsers' trees included, are each
handed one budget for the document
(`fluree_doc_model::budget::Budget`) and spend it *before* they allocate. Past it, the document is refused with an
error that names what ran out (`workbook too large: cells past the 2560 MiB
of memory a document may take`) rather than taking the process down with
it, and in a batch the other documents still convert.

The other readers are not on this budget, and bound what they make by
bounds of their own, where they have any: XPS the pages it lists (250,000,
read or past the page limit, or the page limit where that is more) and the
glyphs it reads
([XPS](../inputs/xps.md#bounds)), an image its pixels
([Images](../inputs/images.md#size-and-coordinates)). A PDF spends the
budget on what its pages draw, as they are interpreted ([A PDF's
content](#a-pdfs-content)), and on its annotations; the transcript readers
have no bound beyond their input's size. Of an e-mail, only an HTML body
is on it: its MIME tree or compound file, and a plain-text body, are
bounded by the message's size ([An e-mail's own
layer](#an-e-mails-own-layer)).

A budget is one document's. A message's attachments are converted within
what the message leaves of its budget, one at a time
([Files an e-mail carries](#files-an-e-mail-carries)); a batch runs no more
documents at once than free memory holds budgets for
([Batches](#batches)).

## Setting the limits

The bounds a caller may set are gathered in one value,
`fluree_doc_model::budget::Limits`, whose default is the constants below:

| Limit | Default | Bounds | Flag, variable, config key |
| --- | --- | --- | --- |
| `memory` | 2.5 GiB | what one document's elements, cells, text and parts inflated are estimated to take ([Memory](#memory)) | `--max-memory` (MiB), `FDOC_MAX_MEMORY`, `max_memory` |
| `work` | 2^28 steps | the layout passes and parsers' loops whose cost the file chooses ([Work](#work)) | `--max-work`, `FDOC_MAX_WORK`, `max_work` |
| `pages` | 10,000 | the pages a PDF or an XPS document is read to ([Pages](#pages)) | `--max-pages`, `FDOC_MAX_PAGES`, `max_pages` |
| `render_pixels` | 64 million | the pixels a page is rendered with ([Renders](#renders)) | `--max-render-pixels`, `FDOC_MAX_RENDER_PIXELS`, `max_render_pixels` |

A library caller sets them for the readers it calls with
`budget::limited_to(limits, || ...)`, on its thread, as `within` and
`converting_to` are set; every budget made inside
(`Budget::default()`) and every page cap and render follows them. `fdoc`
resolves them once per command, each `--max-*` flag over its `FDOC_MAX_*`
variable over the `[limits]` table of `.fdoc/config.toml`, and sets them
for every document it reads, a batch's workers included; `-j` is sized
from the memory limit in effect, or from `--job-memory` where it is set
([Batches](#batches)). Under the fuzz
targets the defaults are the fuzz build's ([Under the fuzz
targets](#under-the-fuzz-targets)).

A reader's own bounds on what the budget does not count (a part read
beside the document, an XPS page's glyphs, an image's pixels, the cells
calamine allocates) are not among them: each is sized for the shipped
budget and fixed, and stays so whatever memory limit is set.

A limit passed is a `budget::TooLarge`, which says which limit
(`TooLarge::limit()`: `Memory`, `Work`, `Pages`, or `Fixed` for a reader's
own) and its value (`TooLarge::max()`). The render pixels never refuse a
document: past them, a page is drawn smaller ([Renders](#renders)). Nor
does the page limit, but for a document listing more pages than a reader
can hold before it reads any: past it, a document is read to it and the
pages after listed unread ([Pages](#pages)), and a PDF whose memory or
work runs out on a page after the first is read to the page before. Each
reader's error carries it, in its `TooLarge` variant and as its
`source()`, so a caller can tell a document past a limit it may raise from
a broken one. `fdoc` names the flag that raises it: `table too large:
cells past the 1 MiB of memory a document may take (raise it with
--max-memory)`.

## Memory

Three kinds of spending share one estimate of memory, 2.5 GiB a document:

| Unit | Cost | What it is |
| --- | --- | --- |
| element | 4 KiB for `-f doco`, 2 KiB for the others | an element made, or a table cell that holds text, which DoCO writes as a node of its own; for the others, a table's cell that holds text costs 512 B, whatever the format, its slot and text counted apart |
| cell | 128 B | a cell slot allocated: read (a `.docx` or `.pptx` table's cells and rows as each is read, its grid within them), each copy of a repeated one, each a merge covers, each a table is padded with, and each copy a layout keeps |
| byte of text | 12 B | each byte the text may take in the worst output: a control character counts six (`\u0001` in JSON), `"` and `&` six, `<` and `>` four, a character Markdown escapes two |
| link | an element and a cell (4.1 KiB) for `-f doco`, four cells (512 B) for the others, with its target's text | a hyperlink kept on an element (Markdown, HTML): DoCO writes each as a node of its own |

Each cost is above the peak memory one unit was measured to take, from the
reader through the largest output (`-f doco`), with the unit alone filling
95% of the budget:

| Measured at the budget | md | json | doco |
| --- | --- | --- | --- |
| 551,000 numeric table cells (`.xlsb`) | 394 MB | 394 MB | 2,465 MB (4.5 KiB a cell) |
| 536,000 cells apart, each an element (`.xlsb`) | 753 MB | 753 MB | 1,879 MB |
| 590,000 paragraphs (`.doc`) | 321 MB | 400 MB | 2,062 MB (3.5 KiB each) |
| 211 M bytes of text in a table (`.xlsb`) | 1,219 MB | 1,421 MB | 2,029 MB (9.6 B a byte) |
| 35 M control characters in a table (`.xlsb`) | 211 MB | 582 MB | 769 MB |
| 19.8 M cells of padding (`.csv`) | 574 MB | 799 MB | 1,428 MB |

So a document that spends the whole budget, however it mixes them, copies
of its tables' cells included, peaks under about 2.7 GB, and converts
within a 4 GB address space with room for its input and a library's own
copies (calamine's, for an `.xls`).

## The output converted to

DoCO writes each element and each table cell that holds text as a node of
its own, and copies a table's cells (below); Markdown, XHTML and the flat
JSON write each once, and the text projection only repeats a value merged
down. So the budget counts what the requested output makes:
`fdoc convert` tells the readers the format once, for every input it
reads (`fluree_doc_model::budget::converting_to`), and a reader that is not
told, a library caller's, counts at DoCO's costs, the largest. An element
costs 2 KiB for Markdown, XHTML, JSON and the text: the reader's own copy is
most of it, and the most measured is 1.25 KB a paragraph (`.odt`, its XML
tree) and 750 B a cell (`.xlsx`). A table's cell that holds text costs the
same in every format but CSV, a sheet's (`.xls`, `.xlsb`, `.xlsx`, `.ods`)
as a Markdown, HTML, Word, PowerPoint, RTF or OpenDocument
table's: its slots, each copy the reader and the layout keep, and its text
are spent apart, and the cell 512 B more for these outputs
(`fluree_doc_model::budget::filled_cell_bytes`, which the fuzz targets'
check counts with too), not 2 KiB: a one-digit cell of an `.xlsx`, 750 B in
all, counts 980 B. A `.csv` record's cell costs 128 B more, not 512
(`filled_csv_cell_bytes`): the CSV reader keeps nothing of a cell but its
text, and counted as a sheet's, an ordinary file of a million records was
refused at four times what it takes ([A CSV's records](#a-csvs-records)).
The slots are not the same in every format: a sheet's
layout copies its cells (into islands, then a table's grid), and a
Markdown table's parser builds a tree, each spent, so a table may convert
from one format and be refused from another. 50,000 rows by 12 columns
convert to DoCO from a `.csv`, at 1.77 GB, and are refused as an `.xlsx`,
`.xls`, `.ods` or Markdown table; as Markdown, JSON or the text, all five
convert. Counted at an element's 2 KiB, a `.csv` of 100,000 rows
by 12 columns (13 MB) was refused as Markdown where the same `.xlsx`
converted; it now converts, at 144 MB, and 100,000 rows by 15 columns (1.5
million cells) at 162 MB, about 110 B a cell, 290 MB from a Markdown table,
whose parser's tree is counted too ([A Markdown document's
tree](#a-markdown-documents-tree)). As DoCO, 2.9 KB a cell were measured
(20,000 rows by 15 columns, 883 MB), so both are refused there. Counted
at 2 KiB on top of its slots, each of an `.ods` sheet's cells spent twice,
and its content bound to 64 MiB, a LibreOffice `.ods` of 50,000 rows by 12
columns (2.2 MB, its content 77 MB) was refused as Markdown; it now
converts, and peaks at 0.9 GB, its XML tree most of it.

A table of 50,000 rows by 15 columns, every cell filled (750,000 cells),
peaks at:

| Read from | md | xhtml | json | text | doco (unbounded) |
| --- | --- | --- | --- | --- | --- |
| `.csv` (4.4 MB) | 73 MB | 75 MB | 84 MB | 160 MB | 2,229 MB |
| `.xlsx` (5.3 MB) | 541 MB | 541 MB | 541 MB | 552 MB | 3,034 MB |
| `.docx` (2.1 MB) | 218 MB | 218 MB | 217 MB | 223 MB | 2,247 MB |
| `.html` (13 MB) | 403 MB | 403 MB | 409 MB | 416 MB | 2,249 MB |

It converts to every output but DoCO, whose 3 GB from the `.xlsx` is past
what the budget allows; as DoCO it is refused before anything is written.
At the other outputs' costs a budget holds about 1.2 million elements, or
about 4 million table cells of one digit, 3 million of 20 bytes (10 million
and 5.4 million from a `.csv`); the most such a document was measured to
take is 1.5 GB (1.2 million paragraphs of an `.odt`), against 0.25 GB for
1.2 million cells of a `.csv` or a `.doc`, and 1.2 GB for as many
paragraphs of a `.docx`.

### A CSV's records

A `.csv` record's cell takes less than any other table's: the reader holds
each as its string and nothing else, so from the reader through the output
a cell was measured at about 100 B as Markdown and 216 B as the text, its
slot and its text included. Counted at 128 B, its slot at 128 more and its
text at 12 B a byte, a one-digit cell counts 268 B and a six-byte one
328 B. An ordinary file of five columns (`id,name,date,amount,code`, 39
bytes a record) converts by default to Markdown, JSON and the text up to
about 1.6 million records; it was refused from 0.8 million:

| Records of five (`fdoc convert`, by default) | md | json | text | doco |
| --- | --- | --- | --- | --- |
| 1 million (36 MB) | 604 MB | 648 MB | 1,183 MB | refused |
| 1.5 million (53 MB) | 904 MB | 945 MB | 1,771 MB | refused |
| 1.7 million (60 MB) | refused | refused | refused | refused |
| 10 million one-digit cells (2 M records, 20 MB) | 989 MB | 1,071 MB | 2,155 MB | refused |

The last is about the most cells a budget holds. As DoCO, a cell is a node of its own at an element's cost (2.9 KB measured), so
about 100,000 such records convert. What a hostile file makes is still
refused before it is allocated: a header of 100,000 columns over 200,000
one-field records (20 billion cells padded), and a record of 30 million
empty fields before one that holds something, are refused in 0.01 and 0.15
s; a quoted field of 50 MB converts, at 0.3 GB, 0.65 GB as DoCO.

An HTML table's cells cost the same, beside the page's tree, which is
counted on its own ([A web page's tree](#a-web-pages-tree)). 50,000 rows of
30 cells (1.5 million, 27 MB) take 760 MB as Markdown and convert; 50,000
of 60 (54 MB) take 1.5 GB, about 500 B a cell, and are refused, counted at
about 1.3 KB a cell, its tree's two nodes and the cell's own.

## Copies of a table's cells

A table's cells are not all DoCO writes of it. Each `doc:TableCell` must
describe itself, so a value merged down is written in every row of its span
(the texts of the cells it covers joined first, by a space), the text
projection shows it on each of those rows too, and each cell carries its
column's header, its row's first cell and its section's band
(`doc:columnHeader`, `doc:rowHeader`, `doc:sectionLabel`). None of these is
in the file more than once: a 2 KB `.docx` whose two-column table holds a
value of 200,000 characters merged down its 3,000 rows made 1.8 GB of DoCO
and peaked at 5.3 GB, the value written twice a row (its cell's, and the
next cell's row label); and 8,000 cells of one letter each merged into one,
whose joined value is 16,000 characters on each of their rows, made 260 MB
from 5 KB.

So a reader spends them too, once a table is made and before it is kept
(`Budget::spend_copies`, sized by `fluree_doc_model::doco::table_copies`
walking the table as the writer does, without making a copy): each cell a
merge fills, a node of its own, at the element's cost, and each byte copied,
merged value or label, at the text's. A cell's labels are counted on every
table, merged or not: a long row label over many columns is the same copy.
A part of a table a page break carried on (`continues`) is written with
what it takes from the part before: the column names in force at that
part's end, on a part with no header of its own, the band open at its foot,
on each row until the part's own first band or break, and the value it ends
on in a cell the break cut. None is in the part's own cells, so the budget
counts them on every cell they land on, with the part before
(`fluree_doc_model::doco::TableCopies`, which the budget keeps across a
document's tables): a band of 6,000 characters at the foot of one page,
over 10,000 rows on the next, is 120 MB of labels from 6 KB, and is refused.
The 5.3 GB above is 4.4 B a byte copied, under the 12 the budget counts;
both files are refused as DoCO now, before a copy is made.

A PDF's tables, an XPS document's and those an OCR reading makes are laid
out from what the page draws, by a layout that keeps no budget: their
copies are spent once the document's elements are final (after the model
tiers' readings, the notes, and the pages not asked for left out), from the
budget the pages were read within. Past it, a PDF or an XPS document is
read short, as it is past the budget while its pages are read: to the most
pages whose glyphs and tables' copies fit together, the pages dropped
giving back what their glyphs spent, the rest listed `PastMemoryLimit`. A
table on a document's first page that does not fit alone, and an image
(one page), leave nothing to read short and are refused. 24 pages of a
ruled table of 40 rows, each a header of three 46-character names, read in
2 MiB as Markdown and the text, and to page 15 as DoCO. Only DoCO makes
these copies: converted to text, a value merged down is counted once a row,
as the projection shows it, and converted to Markdown, XHTML or JSON,
nothing is copied, so both files convert.

## Work

Layout passes whose cost the file chooses (islands of cells compared
pairwise to mend a block a missing cell split, a table's rows read across
its range), and a PDF's interpreter for each thing a page draws ([A PDF's
content](#a-pdfs-content)), spend a count of steps of their own, at most
2^28 a document. A step is not a fixed time: a layout pass's takes
nanoseconds, but a glyph the PDF interpreter draws, even off the page, 2
to 3.5 µs (4 million off the page took 14 s), so a PDF may take 8 to 15
minutes before its steps run out. The work bounds what a file can make a
reader do, not how long it takes ([Time](#time)).

A package's parts name each other by relationship id (a `.docx`'s
headers, footers and images, an `.xlsx`'s sheets and the pictures of its
drawings, a `.pptx`'s slides and charts), and each id is found among the part's relationships in one
lookup, not a scan of them all: a scan for each id costs the product of
the two, and an id no relationship has costs the whole scan and spends
nothing. A 338 KB `.docx` of 100,000 header references and 10,000
pictures naming no relationship among 30,000 took 17 s; it takes 0.27 s.
A 554 KB `.pptx` listing 20,000 slides by an id none of 200,000
relationships has took 24 s, and one whose slide frames 20,000 charts
that way 24 s; each takes about 0.1 s. A slide's relationships are read
once, however many times the deck lists it, and the charts they name
spent once; the slide's part is read, and spent, at each listing. A
slide of 300,000 chart relationships listed 100 times took 25 s, its
relationships read again at each listing; listed 1,000 times, it takes
0.3 s.

## A web page's tree

An HTML page, or an e-mail's HTML body, is parsed by html5ever into a tree
held while its elements are made. The tree is spent as the parser makes
it, not from the page's tags: the parser makes elements the page does not
write. A formatting element left open (`<b>`) is made again at each
paragraph after it, so a 124 KB page of 400 of them over 30,000
paragraphs made 12 million elements and peaked at 2.8 GB, and a 91 KB one
aborted at 4 GB. Each node, an element's, a text's or a comment's, costs
two cell slots (a node takes 144 bytes, with its place among its parent's
children), and one more for each 128 bytes of its text; an attribute,
one, and one for each 128 bytes of its value. 4.5 million `<i>x</i>`, 2.3
GB counted, peak at 1.7 GB.

The parser's own time is spent as steps of work, where it can be counted.
Before it starts, for each tag, the square of its attributes, halved,
counted from the tags, since its tokenizer looks for each among those
before it (two million on one tag ran past five minutes); the scan passes
over a comment, or the text of a `<script>` or a `<title>`, no further
than the tokenizer does, so as never to count less. As it parses, where
it asks the reader:

- a step for each open element it looks at and each pair of nodes it
  compares, which is how it walks the elements open to know what is in
  scope or what an end tag closes (40,000 nested `<div>` took 3 s, 200,000
  over two minutes; 20,000 `<span>` then 200,000 `</zz>` ran past a
  minute);
- a step for each 32 siblings among which it finds a node, to set a
  table's misplaced content before it or take a node out (500,000 nodes
  set before a table, among 200,000, took 105 s), and one for each child it
  moves to another parent.

On each formatting element's tag (`<a>`, `<b>`, `<font>`, `<i>` and the
like), which it handles in loops that do not ask the reader, a step for
each element it holds, open or among those it would make again, and for a
start tag the comparison of its attributes with those of each element of
its name, copied and sorted (5,000 `<b>` of 100 attributes each, 2 MB,
ran past two minutes). An element of its name it holds open is counted
although the parser would not compare it, which only counts more. Once its
budget is spent, the parser is given nothing more, is stopped within 256
bytes of the page, and the page is refused.

What the parser does inside its own loops between two calls to the reader
is bounded only by what those calls and the formatting tags count, and its
memory beyond the tree is not counted. There is no deadline on a page: work
inside a library's loop cannot be stopped from the thread running it, only
a process can be (issue #23).

What nests more than 512 deep, where a browser stops nesting, is set
beside its ancestor at that depth, as a browser sets it, and read: the
walk that reads the tree recurses down it, and 1,000 tables nested, a 30
KB page, overflowed a 2 MiB stack.

The walk is linear in the tree. What it asks of each element, whether a
block or a table is under it and whether it is left out (an `<aside>` is,
unless it is in an article or a section), is worked out once for the
whole tree before the walk, in one pass down it and back up. Asked as the
walk went, each was a walk of its own, down the element's subtree at each
level above it, or up its ancestors, and nothing counted it: 500 `<span>`
around a million `<br>x` (5 MB) took 54 s, around 2.5 million `<br>` (10
MB) 111 s, and a million `<aside>` 500 elements deep (15 MB) 18 s; they
now take 0.7, 1.2 and 1.0 s, as the same pages do without the depth. An
element with a fact to keep (a block under it, or left out) takes about
70 bytes more at the peak, within what its node is counted at: the page
of a million `<aside>` peaks 68 MB higher, at 265 MB.

A block's text is spent before it is made: gathered once to count it,
copying nothing, then gathered into one string, its whitespace collapsed
and its links placed as it is gathered. It was gathered whole, collapsed
with a map from each of its bytes to the character it lands on (16 bytes
a byte), then spent: about 20 bytes of memory for each byte of a
paragraph before the budget saw it, so a page of one paragraph of 50 MB
peaked at 1.0 GB, one of 185 MB converted at 3.7 GB, and one of 193 MB,
which the budget refuses, aborted the process under a 4 GB address space,
as did a table whose cell held as much. The 50 MB one now converts at 0.2
GB, the 185 MB one at 0.71 GB (0.89 GB as DoCO), and the 193 MB one is
refused at 0.56 GB, the table at 1.1 GB. A table's cells are each
gathered once, then spent with the table, as a sheet's are; a page's
`<main>` is tested for text by counting it, without a copy.

## A Markdown document's tree

A Markdown document (and a `.txt` that writes Markdown's markup, read as
one) is parsed by pulldown-cmark,
which builds the tree of the whole document, its blocks and the marks of
their inline markup, before the reader sees its first event, and holds it
while the elements are made. So the tree is spent first, from the
document's text, without parsing it: each byte the parser looks for as it
builds the tree (`` ` ``, `\`, `&`, `*`, `_`, `~`, `!`, `<`, `[`, `]`, `|`),
each line end, and each container a line opens (a quote's `>`, a list
item's marker, nested ones each) is a mark, a node of the tree and at most
the text before it, and costs a cell slot and a half, 192 bytes. Text
between marks, however long, is one node. About 100 bytes a mark were
measured at the peak, from `*a* ` to `[a`, nested `- ` and lines of one
letter, and up to 210 bytes in address space, the tree's vector reserving
up to twice what it holds: 60 MB of `*a* ` on one line, 30 million marks,
aborted at 4 GB, and 19 million `[a` too; both are refused now, before the
parse, and no such run near the budget aborts. A 108 MB log of 720,000
lines, six marks a line, takes 522 MB and converts to every output.

## Parts inflated

A part of a `.docx`, `.pptx` or `.xlsx` package that is read whole (the
main part, its styles and numbering, each slide, sheet, chart, header and
footer), or of an `.odt`, `.ods` or `.odp` (its content, styles and
metadata, with no smaller bound of their own), is read within what is left
of the document's budget, and spent
from it at two bytes a byte: the part, and the copy a reader makes of a run
of text as long as it. An OpenDocument part's XML tree, held while the
elements are made from it, is spent too as it is built, each node before it
is copied out of the part, whose runs of text are read in place: a cell
slot a node and one for each 128 bytes of its XML, and one an attribute and
one more for a node's list of them (a node 140 bytes measured, its first
attribute 260, each other 90). A million `<a b=""/>` took 370 bytes a node,
counted at 512. Counted at two slots a node and two an attribute, an `.odt`
of 40 tables of 2,500 rows by 10 columns (a million cells, 3 MB), each cell
with the three attributes LibreOffice writes, was refused as Markdown; it
takes 1.03 GB, now counted at 1.98 GB, and converts. A 1.2 MB `.odt` whose
one paragraph is 1.2 GB of text is refused at 1.25 GB, the part alone. A
part that is not UTF-8 is read with each byte that is not replaced by
U+FFFD, three bytes, in a copy beside the part, spent at its length
before it is made: 1.2 GiB of `0xFF`, a megabyte zipped, peaked near 5
GB. And
a 202 KB `.ods` whose content inflates to 65 MB, two million cells, made a
tree of about 600 MB the budget did not see, and peaked at 1.7 GB before
its cells were refused, at 800 MB under the fuzz targets' budget of 160
MiB; with its tree spent, it peaks at 1.5 GB. A 2 MB `.docx` whose main part inflates to 1.9 GB of
spaces peaked at 4.0 GB; it is refused now from the size its archive
declares, before inflating, and a part whose declared size lies is cut as
it inflates. The declared size is reserved only where memory is found for
it, and what it claims past what the part holds is let go once the part is
read: a 9 KB `.docx` whose three main parts each declared 1.3 GB for 1 MB
was refused under a 3 GB address space as though it were that large. The parts read beside the document (relationships, core
properties, settings, notes, comments) have a bound of their own, 64 MiB.
A document whose relationships pass it is refused, naming the part: read
as none, its headers, notes and pictures were left out without a word.
The core properties past it are read as saying nothing.

What an `.xlsx` keeps of its parts while its sheets are read is spent as
it is kept: each shared string, each cell style and each sheet the
workbook lists a cell slot, a number format's literals and a sheet's name
as text. A cell style holds its number format shared, so a format's
literals are kept once however many styles name it. The shared strings'
text is no more than their part's, spent as it is read. A 1.5 MB `.xlsx`
of 200 million empty shared strings (`<si/>`, five bytes of the part and a
string in the table) aborted the process at 3 GB; it is refused now, at
1.1 GB. A 3 KB one whose 5,000 cell styles each copied one format's
megabyte of literals aborted at 3.9 GB; it takes 18 MB now. A workbook's
million shared strings count 128 MB. A sheet's part is read once however
many times the workbook lists it, as an `.xlsb`'s is.

An `.xlsb`'s parts are not spent: the package is first inflated to nowhere
against bounds of its own, 64 MiB a part and 256 MiB in all, and then its
parts are read one at a time, each sheet's freed before the next is read,
so it holds at most a sheet part and its shared strings' text beside the
budget, within the margin a full budget leaves in a 4 GB address space;
each shared string is spent as a cell slot as it is read (9.5 million
empty ones, seven bytes each, take 290 MB). Spending
each sheet's part as it is read would count, for a workbook of many
sheets, parts that are no longer held. The stream an `.xls`, a `.doc` or a
`.ppt` is read from is bounded in the same way, 256 MiB: these files are
not compressed, so it is about the file's own size.

## An e-mail's own layer

An e-mail's HTML body is read within the budget, as a page is, and so is
every other HTML version of the message, one budget for them all: an
alternative read to tell whether the plain text was rendered from it, and
one set aside, read for the images it shows. Twenty alternatives of 150 KB,
each read on a budget of its own, took 12 s. An alternative too large to
read gives way to the plain text, rather than the message being refused;
where one set aside is, each image the message names by a Content-ID is
taken for one it shows, and is not converted as a file. The rest of the
message is not spent from the budget, and is bounded by the message's
size and by bounds of its own:

- an `.eml`'s MIME tree is followed 16 levels deep, each part decoded
  whole from its transfer encoding (base64, quoted-printable) and its
  charset, which takes about the message's size again;
- a `.msg` is a compound file whose streams are each read whole; a
  message attached in it, down to 8 levels, is written back as an `.eml`
  of its own, its body and files in base64 at 76 characters a line, so
  that it reaches its reader as a forwarded message does: what a message
  nested 8 levels deep carries is written about twelve times its size
  into the outermost one, (4/3 × 78/76)^8;
- a plain-text body is read into its elements, its text, at its part's
  size, with no budget.

What this layer takes grows with the message's size, and for a `.msg`
with how deep the messages in it nest, and the CLI sets no bound on an
input's size.

## Files an e-mail carries

`--parse-attachments` converts the files an e-mail carries while the
message is held, so each is read within what the message leaves of the
budget (`fluree_doc_model::budget::within`, which bounds every budget a
reader makes on the thread meanwhile): its elements and their text at the
costs above, and its files' bytes at a part's two bytes a byte (the
message's copy and the walk's). Each output is written as soon as it is
made, so no more than one file's conversion is held at a time. A 31 KB
e-mail carrying 14 small `.docx`, each making 300 MB of DoCO, aborted
holding all 14 outputs; it now converts all 14, peaking at 0.9 GB.

A message attached is converted within that share like any file, and the
walk then opens it again for its own files, within the same share, and
without making its body: only its MIME tree or compound file is read, to
take the files out, and its HTML parsed for the images it shows, which are
not files to convert. An e-mail whose HTML body peaks at 0.9 GB, carrying
one whose body peaks at 1.3 GB on its own, was refused converting the
attached one, and then made its body again on a whole budget of its own
to follow its files, holding both at once: 1.73 GB. It now peaks at 1.4
GB, the attached message refused within the share. The files taken out of
an attached message wait in the walk until their turn, not spent from the
budget: they are taken out all at once, and the walk's bounds (64 MiB a
file, 256 MiB in all) are checked as each one's turn comes. What waits is
about the attached message's own size, which is within 64 MiB for it to
be followed, and more for a `.msg` whose messages nest
([An e-mail's own layer](#an-e-mails-own-layer)).

## Batches

The budget is a document's, and a batch (`fdoc convert DIR --out-dir OUT
-j N`) converts N documents at once in one process, so N documents each
within their budget could together pass what the machine has, and one
allocation refused would abort the whole batch, or the kernel kill it. So
each document is admitted only once the memory free to the process has
room for it beside those already running: a whole budget, its input, held
whole, and 1.25 GiB for what the readers' own bounds let a library take
beside the budget (an image decoded within 1 GiB), 3.75 GiB and the input
by default. A document that needs more than there is runs alone. The
input and the 1.25 GiB do not shrink with `--max-memory`. A page limit
past 250,000 adds what the PDF parser keeps for the pages past that, 1.5
KiB each ([A PDF's page tree](#a-pdfs-page-tree)).

`--job-memory <MiB>` (or `FDOC_JOB_MEMORY`) says what a document takes in
place of that estimate, its budget and the 1.25 GiB, where the caller has
measured what its documents take on its machine: each document is then
admitted at that and its input, and read within two thirds of it
(`fluree_doc_model::budget::within`), as the default's 2.5 GiB budget is
two thirds of its 3.75 GiB; `--max-memory`, where it is lower, still bounds
it. An 8 GB container that runs one document at a time runs four small
ones with `--job-memory 1536`, each refused past 1 GiB. Where the memory
free is not known, setting it lifts the one-per-core bound below: a batch
that waits on the network, not the processor, runs as many as `-j` asks.

`-j` is lowered, with a note on stderr, to as many documents as the
memory free holds at the batch's smallest input, and at least one (`note:
-j 4 lowered to 1: 2860 MiB of memory free, 3840 MiB a document`); a large
input among small ones waits for room rather than lowering `-j` for the
others. The memory free is, on Linux, the least of what the system has
available (`MemAvailable`), what the process's control groups leave it
(a container's limit, counted as docker and the kubelet count a working
set) and what its address space may grow to (`ulimit -v`); on macOS, the
pages `vm_stat` counts free, inactive or speculative, a container's limit
or `ulimit -v` not looked at. Where it is not known (on any other
system), no more than one document per core runs at once.

A batch reading images with the OCR engine (`--ocr`) sets aside, before it
counts documents, the memory the engine may take: its own budget
(`ocr.max_memory`; unset, a third of the memory free, at most 8 GiB; or one
pass at `ocr.max_megapixels` where that is more, as it is read alone) and
0.3 GB for each pipeline it may build, one per image read at once, four at
most. The engine is shared by every document, so it is set aside once, not
per document: with the defaults, a batch runs one document at once with 12
GiB free, two with 16 GiB (`note: -j 4 lowered to 2: 16384 MiB of memory
free, 6661 MiB of it for OCR, 3840 MiB a document`). Set `ocr.max_memory`
lower to run more documents at once; the engine then reads fewer images at
a time ([Memory and speed](../cli/ocr.md#memory-and-speed)).

What these figures were measured on is in [How the limits were
sized](../design/limits.md#batches).

## Pages

A PDF or an XPS document is read to its 10,000th page, the page limit
(`Limits::pages`, `--max-pages`), and no further: the pages past it are
neither interpreted nor laid out. A PDF whose memory or work runs out on a
page after the first is read to the page before, the last whole one
([A PDF's content](#a-pdfs-content)). Never silently: each page not read
is listed in `doc:unreadPages` with the limit that left it, one entry a
page:

| Reason | The page is | Raised by |
| --- | --- | --- |
| `PastMemoryLimit` | on or after the one a PDF's memory ran out on, before the page limit | `--max-memory` |
| `PastWorkLimit` | on or after the one a PDF's work ran out on, before the page limit | `--max-work` |
| `PastPageLimit` | past the page limit (past both, it is this one: raising the budget alone leaves it unread) | `--max-pages` |

`fluree_doc_pdf::unread_pages` lists them, from `Document::pages_past_limit`
and `Document::cut_short`; `fluree_doc_model::is_past_limit` tells these
reasons from the others. `fdoc` warns once for each run (`pages
10001-12000 are past the page limit, not read (2000 pages)`), then names
the flag that raises the limit that stopped it (`read to page 10000
(raise it with --max-pages)`). Under `-q` it still says the second where
the output carries no notes (`-f json`, `-f text`): nothing else would
tell a document read short from a whole one. `--strict` does not fail such
a document: it fails one that yields nothing, and the pages read are
output. The notes left on the pages past those read are not read either.

A document listing more than 250,000 pages, or more than the page limit
where that is higher, is refused before any page is read
(`Limit::Pages`, raised by `--max-pages`): the PDF parser keeps each page
of its tree, about 1.4 KB, before the page limit can leave any unread, and
an XPS document lists each page past the limit (`XPS document too large:
it lists more than 250000 pages`). A caller that raises the page limit past
250,000 asks for that many pages read, and accepts what the parser keeps
for them. A multi-page TIFF, of which only the first page is read, counts
its pages to one past the limit: the pages after the first are listed
`LaterPage`, and one past the limit `PastPageLimit`, after which the file
is not followed.

The page limit bounds the pages read, not what each page holds: a page's
glyphs, paths and pictures are [A PDF's content](#a-pdfs-content). Slides
(`.pptx`, `.ppt`, `.odp`) and sheets are not cut at the page limit: each
slide's part, and each element made of it, is spent from the memory
budget, which bounds them already, and a deck has no page a reader routes.

## A PDF's content

A page's content stream is interpreted by hayro, which calls back into the
reader for each thing the page draws, and has no bound of its own: a page
of 31 KB drawing 20 million glyphs took 4.1 GB. So each call spends the
document's budget (`Budget::default`, one for all its pages) before what it
drew is kept:

| What the page draws | Work | Memory |
| --- | --- | --- |
| a glyph | a step | 224 bytes, its text where a mapping gives it more than four bytes, 48 more where it starts a line, and 192 more for each glyph of the page that holds the most, once |
| a path | a step a segment | a cell slot for each rule or fill kept |
| an image | a step, and 4,096 where its probe decodes it (not for an image whose probe is off, nor one drawn again whose result is reused) | a cell slot for its placement |

Past the budget the page is stopped where it is (where the build unwinds;
built to abort on a panic, the interpreter runs the page to its end with
nothing more kept), and dropped: the document is read to the page before,
the pages from there listed unread ([Pages](#pages)). By the costs
above, a text PDF of 5,000 pages of fifty lines is read to about its
3,500th within the default budget, rather than refused. Where the first page alone passes the budget, nothing would be
read, and the document is refused: `extract: PDF too large: glyphs past
the 2560 MiB of memory a document may take (raise it with --max-memory)`
(`ExtractError::TooLarge`).

What the interpreter does between two calls is not counted: decoding a
content stream, the operators that draw nothing (a million `q` or `cm`),
building a path before handing it over, a glyph's outline, a shading. Its
memory is bounded by the content stream's own size, its time by none:
there is no deadline ([Time](#time)). The layout passes after extraction
spend the work budget where their cost is the file's choice; their memory
follows the glyphs kept. The notes left on the pages read are read on a
budget of their own, after the pages.

The measurements behind each cost are in [How the limits were
sized](../design/limits.md#a-pdfs-content).

## A PDF's page tree

hayro-syntax walks a PDF's page tree when the document is opened, by
recursion, with no bound on its depth, and keeps every page it walks: a
1.7 MB file of 20,000 nested nodes overflowed the stack and aborted the
process, and a 12 KB file whose two levels each list one kid a thousand
times made a million pages, 1.4 GB. The parser has no hook into that walk,
so before a document is opened (`fluree_doc_pdf::open`, which
`extract_bytes` and every `fdoc` command use; `extract_pdf` reads a
document opened so) its tree is measured from the file's bytes
(`fluree_doc_pdf::page_tree`), and refused past:

- 256 levels (`PDF too large: its page tree nests more than 256 deep`), a
  reader's own bound (`Limit::Fixed`); a real tree is a handful deep;
- 250,000 pages, or the page limit where that is more (`its page tree
  lists more than 250000 pages`, `Limit::Pages`), each path to a page
  counted, a page listed twice counted twice. The longest real documents
  run to tens of thousands.

The measure reads every object the file holds as the parser reads one, in
every version an incremental update left and inside object streams, and
counts from the objects a catalog names by `/Pages`, where the parser's
walk starts: a structure tree's array of references, or a name tree's
`/Kids`, are not pages. An object nesting more than 2,048 deep is refused
(`an object nests more than 2048 deep`), as are object streams decoding
past 64 MiB each or 256 MiB in all, cross-reference streams listing more
than four million objects, and objects overlapping so that reading them
would scan the file and what its object streams decode to more than 16
times over.

The measure does not see everything the parser may walk: an encrypted
document's object streams, for one, are not decrypted. What it misses,
and how each bound was found, are in [How the limits were
sized](../design/limits.md#a-pdfs-page-tree). The parser bounding its own
walk is the complete answer, and is to come from hayro-syntax.

## Renders

A page is rendered whole, for `fdoc render` and for the crops a model or
an OCR engine reads, which are cut from a render of their page. No page is
drawn with more than 64 million pixels (256 MB of RGBA,
`--max-render-pixels`), nor with a side longer than 32,767 pixels, which
the renderer, sizing its canvas in 16 bits, cannot draw: a page the scale
asked for would draw larger is drawn at the scale that fits
(`fluree_doc_pdf::render::fitted_scale`), never cut, and the render, and
each crop cut from it, records the scale it was drawn at (`Raster::scale`,
`RawCrop::scale`). A page within both is drawn as asked; the largest page
the format allows, 14,400 points a side, is drawn whole at 2x. `fdoc
render` says which bound drew a page smaller (`drawn at 0.33x (1000x1000
px), the most 1000000 pixels allow (--max-render-pixels)`, or `..., the
longest side a render may take, 32767 pixels`, which no flag raises), and a
page whose short side comes to less than a pixel at that scale is not
drawn, and said to be. What a page's content costs to draw, beyond its
pixels, is not bounded here ([A PDF's content](#a-pdfs-content)).
Measured: [How the limits were sized](../design/limits.md#renders).

## Time

There is no deadline on a document, nor a time limit among the
[limits](#setting-the-limits): work inside a library's loop (hayro's
interpreter between two calls, html5ever's tree builder, a decoder) cannot
be stopped from the thread running it, and a deadline checked only where
the reader gets control back bounds nothing in between. What the limits
bound is what a document may make and the steps whose cost the file
chooses; a caller that needs a wall-clock bound runs the reader in a
process it can kill (issue #23).

## Coordinates

No cell past Excel's grid, row 1,048,576 or column 16,384, is read, in any
workbook format, and a merge is cut at the grid's edge: a sheet in 32-bit
coordinates could otherwise wrap its last column onto its first.

## Under the fuzz targets

The fuzz targets (`fuzz/`) build the readers with a sixteenth of both
bounds, 160 MiB and 2^24 steps, which `Limits::default()` then holds (the
page limit and the render pixels are the shipped ones): an input that
spends the whole budget is then quick to run. CI runs each target for 15
seconds over its seeds (`fuzz/README.md`). Each input is read for one output, DoCO, the text, or
the flat outputs, which its bytes pick, and checked at that output's costs,
so the costs of each are fuzzed; an e-mail's files are converted within
what the message leaves, as `--parse-attachments` converts them.

A reader's own bounds on memory the budget does not count scale with it
(`fluree_doc_model::budget::scaled`): the cells and text calamine
allocates for an `.xls` or `.xlsb` (4 million cells, 250 MB, as shipped);
an XPS document's parts and glyphs (64 MiB a part, 250,000 glyphs a page,
4 million in all); an `.xlsb`'s parts, and the streams of an `.xls`, a
`.doc` or a `.ppt`; the pictures read from a `.ppt` or an
Office package and the parts read beside an Office document's main one
(64 MiB each); the files a PDF embeds (64 MiB each, 256 MiB in all, 512
MiB decoded); the object and cross-reference streams a PDF's page tree
is measured through (64 MiB each, 256 MiB in all, 4 million entries); and the pixels an image is decoded to (128
million, 1 GiB).
Otherwise an input within them would take as much under the fuzzer as in
production, past its memory limit.

The targets' memory limit (`-rss_limit_mb=320`) is twice that budget, near
the margin a full budget has in production (2.5 GiB in a 4 GB address
space), so a reader whose memory runs past its estimate by as much
as it would need to abort there fails under the fuzzer too.
