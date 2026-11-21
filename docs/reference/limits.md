# Limits on what one document may make

A reader bounds its input (bytes read, parts inflated, records nested), but
that does not bound what it makes of it. A cell repeated across a row, a
string shared by every cell, a slide listed a thousand times, pieces of a
Word file that all point at the same text, a table whose first row is
thousands of cells wide over rows of one, a cell merged down over thousands
of rows: a file of a few kilobytes can describe gigabytes. So the readers
of the office formats (`.csv`, `.xls`, `.xlsb`, `.xlsx`, `.ods`, `.doc`,
`.docx`, `.odt`, `.ppt`, `.pptx`, `.odp`, `.rtf`), of Markdown and of HTML
(a page, or an email's HTML body, the parser's tree included, as the
parser builds it) are each handed one budget for the document
(`fluree_doc_model::budget::Budget`) and spend it *before* they allocate. Past it, the document is refused with an
error that names what ran out (`workbook too large: cells past the 2560 MiB
of memory a document may take`) rather than taking the process down with
it, and in a batch the other documents still convert.

The other readers are not on this budget, and bound what they make by
bounds of their own, where they have any: XPS its pages and glyphs
([XPS](../inputs/xps.md#bounds)), an image its pixels
([Images](../inputs/images.md#size-and-coordinates)). A PDF spends the
budget on its annotations alone; the transcript readers have no bound
beyond their input's size.

A budget is one document's. A message's attachments are converted within
what the message leaves of its budget, one at a time
([Files an e-mail carries](#files-an-e-mail-carries)); a batch runs no more
documents at once than free memory holds budgets for
([Batches](#batches)).

## Memory

Three kinds of spending share one estimate of memory, 2.5 GiB a document:

| Unit | Cost | What it is |
| --- | --- | --- |
| element | 4 KiB for `-f doco`, 2 KiB for the others | an element made, or a table cell that holds text, which DoCO writes as a node of its own |
| cell | 128 B | a cell slot allocated: read, each copy of a repeated one, each a merge covers, each a table is padded with, and each copy a layout keeps |
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
tree) and 750 B a cell (`.xlsx`).

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
At the other outputs' costs a budget holds about 1.2 million elements or
cells that hold text; the most such a document was measured to take is
1.5 GB (1.2 million paragraphs of an `.odt`), against 0.25 GB for 1.2
million cells of a `.csv` or a `.doc`, and 1.2 GB for as many paragraphs
of a `.docx`.

An HTML table's cell that holds text costs a cell more than its slot in
the grid, with its text, rather than an element, for the outputs that
write it once: the page's tree, the most of what it takes, is counted on
its own ([A web page's tree](#a-web-pages-tree)). 50,000 rows of 30 cells
(1.5 million), refused as Markdown before, take 775 MB, and 50,000 of 60
take 1.5 GB, about 510 B a cell, against about 880 B counted.

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

## Parts inflated

A part of a `.docx`, `.pptx` or `.xlsx` package that is read whole (the
main part, its styles and numbering, each slide, sheet, chart, header and
footer), or of an `.odt`, `.ods` or `.odp` (its content, styles and
metadata), is read within what is left of the document's budget, and spent
from it at two bytes a byte: the part, and the copy a reader makes of a run
of text as long as it. An OpenDocument part's XML tree, held while the
elements are made from it, is spent too as it is built, two cell slots a
node and two an attribute: a 202 KB `.ods` whose content inflates to 65 MB,
two million cells, made a tree of about 600 MB the budget did not see, and
peaked at 1.7 GB before its cells were refused, at 800 MB under the fuzz
targets' budget of 160 MiB; with its tree spent, it peaks at 1.4 GB, and at
115 MB under the fuzz targets. A 2 MB `.docx` whose main part inflates to 1.9 GB of
spaces peaked at 4.0 GB; it is refused now from the size its archive
declares, before inflating, and a part whose declared size lies is cut as
it inflates. The declared size is reserved only where memory is found for
it, and what it claims past what the part holds is let go once the part is
read: a 9 KB `.docx` whose three main parts each declared 1.3 GB for 1 MB
was refused under a 3 GB address space as though it were that large. The parts read beside the document (relationships, core
properties, settings, notes, comments) have a bound of their own, 64 MiB.

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

## Batches

The budget is a document's, and a batch (`fdoc convert DIR --out-dir OUT
-j N`) converts N documents at once in one process, so N documents each
within their budget could together pass what the machine has, and one
allocation refused would abort the whole batch, or the kernel kill it. So
no more documents run at once than the memory free to the process holds at
3.75 GiB each, a whole budget and half as much again for the document's
input and a library's own copies (the 4 GB a document at its budget was
measured to fit in), and at least one. The memory free is the least of
what the system has available (`MemAvailable` in `/proc/meminfo`), what
the process's control groups leave it (a container's limit: cgroup v2's
`memory.max` less `memory.current`, v1's `memory.limit_in_bytes` less
`memory.usage_in_bytes`, for its group and each above it, from the path
`/proc/self/cgroup` names under `/sys/fs/cgroup`; what a group uses is
counted less the file cache the kernel reclaims first, `inactive_file` in
its `memory.stat`, `total_inactive_file` in v1, as docker and the kubelet
count a working set), and what its address
space may grow to (`ulimit -v`). Where none is known (outside Linux), no
more than one per core. A lowered `-j` is said on stderr, where the batch
has more files than it runs at once (`note: -j 4 lowered to 1: 2860 MiB of
memory free, 3840 MiB a document`). Six `.docx` that each make 300 MB of
DoCO, converted with `-j 6` under a 4 GB address space, aborted the batch;
they now convert one at a time. Three HTML tables that each peak at 1.5 GB,
converted with `-j 3` in a group limited to 3 GB on a host with 23 GB
available, had the batch killed, silently; they now convert one at a time.
And a group limited to 9 GB, after 4 GB written to a file had left 3.1 GB
of cache counted in its use, no longer lowers `-j 2` to one.

## Coordinates

No cell past Excel's grid, row 1,048,576 or column 16,384, is read, in any
workbook format, and a merge is cut at the grid's edge: a sheet in 32-bit
coordinates could otherwise wrap its last column onto its first.

## Under the fuzz targets

The fuzz targets (`fuzz/`) build the readers with a sixteenth of both
bounds, 160 MiB and 2^24 steps: an input that spends the whole budget is
then quick to run. A reader's own bounds on memory the budget does not
count scale with it (`fluree_doc_model::budget::scaled`): the cells and
text calamine allocates for an `.xls` or `.xlsb` (4 million cells, 250 MB,
as shipped), and an XPS document's parts and glyphs (64 MiB a part, 250,000
glyphs a page, 4 million in all); otherwise an input within them would
take as much under the fuzzer as in production, past its memory limit. Their memory limit (`-rss_limit_mb=320`) is twice that
budget, near the margin a full budget has in production (2.5 GiB in a 4 GB
address space), so a reader whose memory runs past its estimate by as much
as it would need to abort there fails under the fuzzer too.
