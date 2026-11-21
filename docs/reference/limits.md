# Limits on what one document may make

A reader bounds its input (bytes read, parts inflated, records nested), but
that does not bound what it makes of it. A cell repeated across a row, a
string shared by every cell, a slide listed a thousand times, pieces of a
Word file that all point at the same text, a table whose first row is
thousands of cells wide over rows of one, a cell merged down over thousands
of rows: a file of a few kilobytes can describe gigabytes. So every reader
of the office formats (`.csv`, `.xls`, `.xlsb`, `.xlsx`, `.ods`, `.doc`,
`.docx`, `.odt`, `.ppt`, `.pptx`, `.odp`, `.rtf`), of Markdown and of HTML
(a page, or an email's HTML body) is handed one budget for the document
(`fluree_doc_model::budget::Budget`) and spends it *before* it allocates.
Past it, the document is refused with an error that names what ran out
(`workbook too large: cells past the 2560 MiB of memory a document may
take`); the process never aborts, and in a batch the other documents still
convert.

## Memory

Three kinds of spending share one estimate of memory, 2.5 GiB a document:

| Unit | Cost | What it is |
| --- | --- | --- |
| element | 4 KiB for `-f doco`, 2 KiB for the others | an element made, or a table cell that holds text, which DoCO writes as a node of its own |
| cell | 128 B | a cell slot allocated: read, each copy of a repeated one, each a merge covers, each a table is padded with, and each copy a layout keeps |
| byte of text | 12 B | each byte the text may take in the worst output: a control character counts six (`\u0001` in JSON), `"` and `&` six, `<` and `>` four, a character Markdown escapes two |

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
tree) and 750 B a cell (`.xlsx`).

A table of 50,000 rows by 15 columns, every cell filled (750,000 cells),
peaks at:

| Read from | md | xhtml | json | text | doco (unbounded) |
| --- | --- | --- | --- | --- | --- |
| `.csv` (4.4 MB) | 73 MB | 75 MB | 84 MB | 160 MB | 2,229 MB |
| `.xlsx` (5.3 MB) | 541 MB | 541 MB | 541 MB | 552 MB | 3,034 MB |
| `.docx` (2.1 MB) | 218 MB | 218 MB | 217 MB | 223 MB | 2,247 MB |
| `.html` (13 MB) | 454 MB | 454 MB | 454 MB | 458 MB | 2,249 MB |

It converts to every output but DoCO, whose 3 GB from the `.xlsx` is past
what the budget allows; as DoCO it is refused before anything is written.
At the other outputs' costs a budget holds about 1.2 million elements or
cells that hold text; the most such a document was measured to take is
1.5 GB (1.2 million paragraphs of an `.odt`), against 0.25 GB for 1.2
million cells of a `.csv` or a `.doc`, and 1.2 GB for as many paragraphs
of a `.docx`.

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
The 5.3 GB above is 4.4 B a byte copied, under the 12 the budget counts;
both files are refused as DoCO now, before a copy is made. Only DoCO makes
these copies: converted to text, a value merged down is counted once a row,
as the projection shows it, and converted to Markdown, XHTML or JSON,
nothing is copied, so both files convert.

## Work

Layout passes whose cost the file chooses (islands of cells compared
pairwise to mend a block a missing cell split, a table's rows read across
its range) spend a count of steps of their own, at most 2^28 a document: a
few seconds.

## Coordinates

No cell past Excel's grid, row 1,048,576 or column 16,384, is read, in any
workbook format, and a merge is cut at the grid's edge: a sheet in 32-bit
coordinates could otherwise wrap its last column onto its first.

## Under the fuzz targets

The fuzz targets (`fuzz/`) build the readers with a sixteenth of both
bounds, 160 MiB and 2^24 steps: an input that spends the whole budget is
then quick to run, and one that makes more than its budget accounts for
shows under the targets' memory limit (2 GB) rather than within the room a
full budget needs.
