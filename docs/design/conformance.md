# Projection conformance

Every reader produces the same element model, and every writer projects it into
a format: DoCO, Markdown, XHTML, the text projection. A fix in the model reaches
the user only if each writer carries it. The writers are where a format's limits
meet the model — a GFM pipe table has to have a header row, a Markdown cell has
no line breaks — and nothing checked them. The conformance check does: it reads
each projection back with our own readers and compares what survives with the
model, under a contract written down per format.

A mismatch is a writer bug (the format could carry it and we didn't), a reader
bug (we wrote it and can't read it back), or a loss the format forces. The first
two are defects. The third goes on the format's named allowlist, so a loss is
always a decision and never silence.

## The reference

The reference is the element list `E` a reader produced, normalised the way
every writer reads it:

- A table continued from the page before (`continues`) is part of the table it
  continues: its rows are appended, its repeated header rows dropped.
- An element with no text and no cells is no element.
- A list item's depth is its nesting: the number of items before it, in its
  run of items, that are shallower and still open. An item one deeper than
  the one before opens a list inside it and a shallower one closes the lists
  deeper than it, so a list that opens deeper than any item before it (a
  procedure's `8.1.` items under a heading `8.`) is nested in nothing, and
  no format sets it deeper.
- A table whose header the reader did not measure (`header_rows` absent)
  makes no claim about its header, and none is held.

DoCO is `E` serialised, and its offset contract (each element's string is
exactly what its offsets cut from the text projection) is already enforced by
`eval/tough`. The text projection is checked through it. The contract is on
strings, not order. A slide's speaker notes are contained by the slide but
follow every slide in the text, the one class where containment order and
text order differ.

## What is compared

`E` is written to a format, the output is read back with the matching reader
(`fluree-doc-markdown`, `fluree-doc-html`), and the two element lists are
aligned in order by their text. Each aligned pair is held to the format's
contract; an element of `E` with no partner is lost, one of the read-back with
no partner is extra.

| Class | Markdown | XHTML |
|---|---|---|
| **text** — an element's letters, as a multiset; order matters between elements, not within | must survive | must survive |
| **order** — elements in the order of `E` | must survive | must survive |
| **heading** — a heading stays a heading, at its level | must survive (levels 1–6) | must survive |
| **list** — a list item stays an item, at its depth | must survive | must survive |
| **table.grid** — the cell grid's shape and every cell's text | must survive | must survive |
| **table.header_rows** — the count, 0 included | must survive (0: an empty header row) | must survive |
| **table.spans** — `merged_down` / `merged_left` | must survive (as HTML) | must survive |
| **table.sub_headers** — section band rows | must survive (as HTML) | must survive |
| **table.restated_headers** — a header row set again inside the body (a gradient's `Time (min) \| A% \| B%` under its own rows) | must survive (as HTML: a `<th>` row mid-table) | must survive |

Text is compared after the normalisation every format imposes: whitespace runs
are one space, and a cell's line breaks are spaces.

A table with no header has an empty header row in Markdown. A pipe table must
open with a header row, so the writer sets an empty one (`| | |` over
`|---|---|`) above a table measured to have none, and every row of the table is
a body row; the reader reads a header row with nothing in it as no header. HTML
says the same without help: a table with no `<th>` row.

## Allowed losses

Per format, by name, each with its reason. Anything not listed is a violation.

Both formats:

- `bbox`, `page`: neither format has geometry or pages; DoCO carries them.
- `provenance`, `evidence`: how an element was found, not what it says; DoCO
  carries them.
- `datums`: a cell shows its formatted value in every format; the stored,
  typed value is DoCO's.
- `figure` ids: a figure's fragments are written in their page order; which
  drawing they belong to is DoCO's.
- `continues`: a table carried over a page break is written as one table, its
  repeated header rows once; the reference joins the parts the same way.
- `section_breaks`: the blank row a table sets between its groups has no
  markup of its own.
- `spread_left`: a value set across columns is written once, as a span; which
  of the spanned columns it belongs to is DoCO's.
- A message's header fields (`message`) beyond what its text says, `turn`
  metadata beyond the speaker label, `signature`: written as text or not at
  all; DoCO carries them as properties.
- `notes_for`: a slide's speaker notes are written as a section of their own
  after the slides, under `Speaker notes` and each slide's name (`Slide 4:
  Results`); the reference sets the same two headings over them. The slide
  they belong to is DoCO's, which puts them inside it.

Kinds:

- A paragraph-like kind (`doco:Paragraph`, `doco:Caption`, `doco:Figure` text,
  `doco:Footnote`, `doco:Subtitle`, a message part) reads back as a paragraph:
  neither format types its paragraphs.
- `doco:Title` reads back as a level-1 heading: the document's title is
  written as its top heading.

Markdown only:

- A link's target where the writer was asked for anchor text only
  (`--md-links text`).
- A list item's own ordinal is part of its text (`3. FAQ`).
- A header row with nothing in it, over a pipe table: an empty header row is
  how a pipe table says it has none, so it reads back as no header and no row.
  It held nothing to lose.

## Tooling

- `fluree-doc-conform` (a dev crate: the readers depend on the model, so the
  check cannot live in it): `conform(&elements, &notes)`, a list of
  violations, each with its format, class, element index and a short excerpt.
- `fdoc dev conform <files>`: reads each file as `convert` does, runs the
  check, and reports violations by format and class (`--json`: one line per
  file). `fdoc convert -f conform` is the same check as an output format, so
  a document converted with options (`--ocr-words`) is checked as converted.
- `eval/tough`: every case document is held to the contract, as it is to the
  offset contract: a class may fail no more often than the case's known list
  records (`eval/tough/conform-known.json`; a private case's beside it, under
  `conform/`). A fix lowers the counts and `--conform-update` writes them; the
  list only shrinks, to nothing.

## First findings

The first run (2026-11-27) held about 11,400 documents in every format to the
contract. Each class below is one cause, fixed in a change of its own.

Markdown:

- **Headerless tables.** A table with no header row (`header_rows` 0) is
  written as a pipe table, and GFM makes its first row the header:
  `table.header_rows`. The writer sets an empty header row over it instead
  (above, under *What is compared*).
- **HTML tables unread.** The Markdown reader does not read the raw HTML
  tables the Markdown writer emits for span and band tables, so each is lost:
  `table.lost`. The reader reads each raw HTML block with the HTML reader,
  which also serves anyone feeding us Markdown with HTML in it.
- **Text read as markup.** The writer escapes almost nothing, so text that
  Markdown reads as syntax becomes syntax: a paragraph opening `1. ` or `* `
  reads as a list item, `# ` as a heading, `> ` as a quote, a lone `-` or
  `4.` as an empty item, a row of `___` as a rule, `~~~` as a code fence
  that swallows what follows, `<name>` and `<a@b.c>` as markup, `*`, `_` and
  `` ` `` as emphasis and code, `\` as an escape, and a blank line inside an
  element ends it: `list.extra`, `heading.extra`, `text.lost`,
  `text.changed`, `block.split`, `table.grid`.

XHTML:

- **Figure fragments run together.** A drawing's fragments are written as
  `<span>`s in a `<figure>`, which HTML reads as one run of text:
  `block.merged`, and the `text.lost`, `text.extra` and `order` that follow
  from it.
- **Bands read as header rows.** A band row under the header, or a table
  without one, is written as a `<th>` row, and with no `<thead>` to end it
  the header runs on into it: `table.header_rows`, `table.sub_headers`.
- **Tables read as layout.** A table of one row, or of one column with no
  header, is read by the HTML reader as a page's layout grid: `table.lost`.
- **Rows dropped as a record's second line.** A row holding nothing but
  cells spanned down into it is dropped by the HTML reader: `table.shape`.
- Smaller: a paragraph read as a heading by the HTML reader's keyword rule
  (`heading.extra`); a C1 control character read as Windows-1252.

Both:

- **Restated header rows** are written as plain cells, so they read as data:
  `table.restated_headers`. A table with restated headers is written as
  HTML, its restated rows as `<th>`, as a table with spans is.
