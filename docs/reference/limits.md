# Limits on what one document may make

A reader bounds its input (bytes read, parts inflated, records nested), but
that does not bound what it makes of it. A cell repeated across a row, a
string shared by every cell, a slide listed a thousand times, pieces of a
Word file that all point at the same text, a table whose first row is
thousands of cells wide over rows of one: a file of a few kilobytes can
describe gigabytes. So every reader of the office formats (`.csv`, `.xls`,
`.xlsb`, `.xlsx`, `.ods`, `.doc`, `.ppt`) is handed one budget for the
document (`fluree_doc_model::budget::Budget`) and spends it *before* it
allocates. Past it, the document is refused with an error that names what
ran out (`workbook too large: cells past the 2560 MiB of memory a document
may take`); the process never aborts, and in a batch the other documents
still convert.

## Memory

Three kinds of spending share one estimate of memory, 2.5 GiB a document:

| Unit | Cost | What it is |
| --- | --- | --- |
| element | 4 KiB | an element made, or a table cell that holds text, which DoCO writes as a node of its own |
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

So a document that spends the whole budget, however it mixes them, peaks
under about 2.7 GB, and converts within a 4 GB address space with room for
its input and a library's own copies (calamine's, for an `.xls`). The
output a document is converted to is not known to the reader, so the costs
are those of the largest: a table of 600,000 numbers converted to Markdown
would fit, but is refused because its DoCO JSON-LD would not.

## Work

Layout passes whose cost the file chooses (islands of cells compared
pairwise to mend a block a missing cell split, a table's rows read across
its range) spend a count of steps of their own, at most 2^28 a document: a
few seconds.

## Coordinates

No cell past Excel's grid, row 1,048,576 or column 16,384, is read, in any
workbook format, and a merge is cut at the grid's edge: a sheet in 32-bit
coordinates could otherwise wrap its last column onto its first.
