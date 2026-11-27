# How the limits were sized

The [limits](../reference/limits.md) say what a document may take; this
page records what each bound was measured against, what failed before it,
and what it still does not see. It is for a contributor moving a bound,
not for a caller setting one.

## Batches

A document at its whole budget was measured to fit in 4 GB: its budget,
2.5 GiB, and 1.25 GiB for what the readers' own bounds let a library take
beside it, plus its input, held whole. That is what a batch admits each
document at ([Batches](../reference/limits.md#batches)).

What a group uses is counted less the file cache the kernel reclaims
first, `inactive_file` in its `memory.stat` (`total_inactive_file` in
cgroup v1), as docker and the kubelet count a working set: cgroup v2's
`memory.max` less `memory.current`, v1's `memory.limit_in_bytes` less
`memory.usage_in_bytes`, for the process's group and each above it, from
the path `/proc/self/cgroup` names under `/sys/fs/cgroup`.

Before the bound, six `.docx` that each make 300 MB of DoCO, converted with
`-j 6` under a 4 GB address space, aborted the batch; they now convert one
at a time. Three HTML tables that each peak at 1.5 GB, converted with `-j
3` in a group limited to 3 GB on a host with 23 GB available, had the
batch killed, silently; they now convert one at a time. And a group
limited to 9 GB, after 4 GB written to a file had left 3.1 GB of cache
counted in its use, no longer lowers `-j 2` to one.

Every worker was first counted at the batch's largest input: one 2 GB
file lowered `-j` for every other document. Each document is now admitted
by its own size, and the workers counted at the smallest input.

On macOS the memory free is what `vm_stat` counts free, inactive or
speculative: the pages the system hands out before it compresses or
swaps. Purgeable pages are left out, as they are counted among the active
and inactive ones.

With the OCR engine's defaults and four pipelines, a batch runs one
document at once with 12 GiB free, two with 16 GiB, three with 24 GiB and
six with 32 GiB.

## DoCO, written a node at a time

DoCO's graph was built whole, every node a map of JSON values, then
printed into one string: a table's cell took about 3 KB, so an element or
a cell was counted at 4 KiB for DoCO and 2 KiB or less for the others, and
a table of 570,000 filled cells was refused as DoCO where Markdown
converted it. It is now written a node at a time (`doco::write_doco`): the
elements are walked once to find what each node contains, which is held as
runs of node numbers (a table's cells are one run), then again to write
each node once it is whole. An element and a cell cost DoCO what they cost
the others; a `.csv` record's cell costs it 256 B rather than 128, for the
copies of the table the writer keeps while it writes it (its rows, merges
read, and each cell's place in the text), which a `.csv` reader's own
strings are smaller than. The labels DoCO copies into each cell are still
counted, though no longer held: they bound what a file of a few kilobytes
can make it write.

Peak memory, debug build, before and after:

| Input | md | doco, held whole | doco, written |
| --- | --- | --- | --- |
| 500,000 cells (`.csv`, 50,000 rows by 10, `r{row}c{col}`) | | 1,512 MB | 180 MB |
| 2,000,000 cells, the same (`.csv`) | 259 MB | 5,970 MB | 645 MB |
| 2,000,000 one-digit cells (`.csv`) | 200 MB | 5,863 MB | 603 MB |
| 500,000 one-digit cells (`.xlsx`) | 405 MB | 2,350 MB | 413 MB |
| 500,000 cells (Markdown table) | 127 MB | 1,507 MB | 179 MB |
| 1,000,000 paragraphs (`.txt`) | 731 MB | 3,808 MB | 712 MB |
| 600,000 links on one line (Markdown) | 280 MB | 2,530 MB | 280 MB |
| 2,000,000 links on one line (Markdown) | 886 MB | | 887 MB |

The output is the same, byte for byte: every document under the
repository that converts (1,423 of them, 74 MB of DoCO) was converted by
both and compared, and the tests hold each graph they build to the one
built whole. The second walk costs little time, as the first writes
nothing and skips a cell's labels: a table of two million cells took 32 s
in a debug build, 33 s held whole, and one of half a million 7.8 s, 6.9
s held whole.

## A PDF's content

Before the budget was spent on what a page draws, a page of 31 KB drawing
20 million glyphs peaked at 4.1 GB, took 37 s, and aborted under a 4 GB
address space.

What a glyph costs ([A PDF's
content](../reference/limits.md#a-pdfs-content)) was measured on text
PDFs of fifty lines of words a page, which peaked at 217 bytes a glyph:
1.47 GB for 2,100 pages, 3.64 GB for 5,000. Dense text costs a little more
than counted: fifty lines of 100 characters in 7 pt peaked at 233 bytes a
glyph (2.45 GB for 2,100 pages, 2.36 GB counted), 4% over: a document of
such text at the budget's edge takes about 100 MB more than it is counted.
The gap is left, small, rather than closed: counted at 240 bytes a glyph,
3,500 pages of ordinary text would be cut short.

A page is laid out with its glyphs all at hand, so a crowded page costs
more: two million glyphs in lines of words peaked at 374 bytes each on one
page, 278 on two, 211 on ten; a line each, at 412 on one page and 251 over
a thousand. Counted at 384 bytes a glyph, whatever the page, the 2,100
pages were refused; they convert now, as do 3,500 (2.4 GB), and 5,000,
past the budget by what they take, were refused at 2.5 GB in 32 s
(`--max-memory 4000` converts them); by the costs counted, they are now
read to about their 3,500th page, the rest listed `PastMemoryLimit` (an
estimate, not measured). Counted at 192 bytes, 12
million glyphs a line each on one page (an 88 KB file) were within the
budget and aborted under a 4 GB address space; they are refused now, at
1.2 GB. 20 million glyphs on lines of words on one page are refused at 1.3
GB, in 17 s, 3 s and 0.3 GB under `--max-memory 512`; 3 million, 1.1 GB,
convert as before, as do two million rules (0.14 GB).

A document past its budget was first refused whole: a text PDF of some
3,500 to 4,500 pages, which had converted before glyphs were counted, was
refused, and the page limit never applied to text PDFs. It is now read to
its last whole page.

An image's probe is decoded at most 256 pixels square, 4,096 steps at a
step for every sixteen pixels. Counted for every draw, whether the probe
ran or not, about 65,000 draws of one picture (picture bullets, a scan
tiled in strips) passed the shipped work; a draw now costs a step, and
the probe's steps only where it decodes.

## A PDF's page tree

hayro-syntax walks the tree by recursion, one call for each level of
`/Pages` nodes: a 1.7 MB file of 20,000 nested nodes overflowed the main
thread's stack, and 5,000 a batch worker's. A release build overflowed 8
MiB between 5,000 and 10,000 levels, about 1 KB a level; 256 levels are
walked on a 2 MiB thread in a debug build. The 12 KB file of a million
pages took 1.4 GB (2.8 GB as `fdoc convert` opened it, twice), and one of
three levels aborted. A tree of 250,000 pages took 360 MB more than one of
a single page as `fdoc convert` opened it: about 370 MB at the bound,
within the 1.25 GiB a batch keeps beside each document's budget, with the
cross-reference entries'.

The measure reads every object the file holds as the parser reads one:

- its header, `N G obj`, wherever the parser's lexer reads one, white
  space and comments between its parts, glued to what precedes it
  (`endobj3 0 obj`);
- its body, scanned first as the parser skips one, without recursion: an
  object nesting dictionaries and arrays more than 2,048 deep is refused,
  since the parser reads one by recursion too: 100,000 dictionaries deep
  overflowed the measure and the parser, and on a batch worker's 2 MiB
  stack about 22,000 overflow it;
- each version an incremental update left, and each object an object
  stream holds, the stream read at its `/Length` (resolved, where it is
  given by reference, to the last object written with its number) and
  decoded by the parser's own filters (any chain of Flate, with its
  predictors, LZW, ASCIIHex, ASCII85 and RunLength), each stage's output
  bounded before it runs, and its objects read in place, each up to the
  next. Listing an object at one offset 200 times over 60 MiB of padding,
  a 61 KB file, copied the rest of the stream for each and aborted the
  measure itself, as did a real stream of 20,000 objects (7.2 GB); the
  latter is now measured in 8 ms, and converts in 41 MB. Read up to the
  first `endstream` instead, as a stream whose length is given by
  reference once was, a stream stored uncompressed whose objects held
  that word was cut short, and its 20,000 levels aborted the process;
- each object of an object stream under every number the parser may find
  it by: the number a cross-reference stream's entry gives the index it
  sits at, which is how the parser finds it, whatever number the stream's
  header writes there; and the header's number, which is how the parser
  numbers it when it rebuilds a table it cannot read. A 196 KB file whose
  cross-reference stream placed a chain of 20,000 levels at indexes the
  header numbered otherwise was measured one deep and aborted the process.
  The cross-reference streams are read as the parser reads one, wherever
  a stream gives `/W` and `/Size`, and their entries, each one the parser
  keeps in its table, about 88 bytes, are refused past four million:
  thirty million, from a 219 KB file, took 2.6 GB as it was opened.

Over that graph, the kids of each `/Pages` node (or one whose `/Type` is a
reference) followed to every version of the objects they name, the depth
and the pages are no fewer than the parser's walk makes. The walk starts
at the object a catalog names by `/Pages`, so the measure counts from
every object a dictionary names so, each version of it; first counted over
every node, a tagged PDF whose structure tree's `/K` is an array of more
than 250,000 references was refused as a page tree of that many pages.
Where no dictionary naming `/Pages` is seen, every node is still measured.
A cycle the parser cuts (it does not walk again an object already on its
path) is counted as though every path through it were walked; a cycle
among objects of object streams, which the parser does not keep on its
path and walks forever, is past both bounds.

The scan is bounded at 16 passes over the file and what its object
streams decode to: counted against the file alone, a few kilobytes whose
object stream held 3 MB of objects, each read once, were refused as
overlapping.

The chain of 20,000 levels is refused in 0.01 s however its headers are
written or its nodes packed, 200,000 levels (17 MB) in 0.16 s, the 12 KB
file of a million pages in 0.01 s; on 19 real PDFs of 0.1 to 21 MB the
measure took at most 5 ms, and of 66 converted none was refused and the
total time was unchanged.

What the measure does not see, and the parser may still walk unbounded:

- an encrypted document's object streams: the measure does not decrypt,
  so a tree packed in them (as Word and Acrobat write an owner-password
  file) is not measured; three of 19 real PDFs tried were such. A catalog
  packed so is not seen either: where no other dictionary names `/Pages`,
  every node the measure sees is counted, and where one does, only what
  it names;
- an object stream coded by an image filter (CCITT, JBIG2, DCT, JPX) or
  `Crypt`, which the parser decodes too;
- an object stream itself packed in another, and an object stream's
  `/N`, `/First`, `/Filter` or `/DecodeParms` given by reference to an
  object in an object stream (one written in the file resolves to its
  last version), or a filter's parameters given by reference inside them;
- objects of an object stream that overlap the next one, read only up to
  it;
- the entries of a cross-reference table written as text, not counted:
  20 bytes of the file each, they are bounded by its size (four times it
  in the parser's table);
- a header the cross-reference table points to inside a token: into the
  digits of a number (`12 0 obj` read as object 2), or past a sign or a
  point within one; the measure reads a number from the start of its run
  of digits, signs and points;
- an object the measure's scan and the parser's lexer delimit
  differently, read short;
- a document whose page tree the parser cannot read at all, which it
  reads by brute force, every dictionary with `/Contents` a page: bounded
  by the file's size only;
- the parser's time, which nothing bounds.

## Renders

At the model's 2x, a 3,000 x 3,000 pt page is 36 million pixels, a 6,000
pt one 144 million (576 MB of RGBA). The renderer sizes its canvas in 16
bits and pads it: a page 32,764 points wide and three high, 393,000 pixels
at 2x, asked for 16 GiB and aborted the process; a canvas 65,528 pixels
wide aborted, 65,527 drew. Half of that, 32,767, leaves room for whatever
else in the renderer rounds a side up. The 32,764-point page is now drawn
32,767 pixels wide, in 12 MB. `fitted_scale` counts the pixels as the
renderer sizes its canvas, in `f32`, each side rounded down.

Rendered by `fdoc render` at 2x, a 3,000 pt page, 36 million pixels, is
within the bound and peaks at 300 MB, as before; under
`--max-render-pixels 16000000` it is drawn at 1.33x, 4,000 pixels a side,
and peaks at 140 MB. A 6,000 pt page peaked at 1.17 GB; drawn at 1.33x, 64
million pixels, it peaks at 530 MB.
