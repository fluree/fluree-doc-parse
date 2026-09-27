# DOCX, PPTX, XLSX, HTML and Markdown

Four of these **declare** their structure. A `w:pStyle` names the heading level;
`<h1>` is a heading because it says so. So these readers map rather than
measure, and nothing they produce is a hypothesis a model tier could improve.

Two consequences apply to all of them:

- **No geometry.** `bbox` is absent — not zeroed. See [Measured vs declared
  structure](../concepts/geometry-vs-declared.md).
- **No escalation.** There is nothing to arbitrate.

## DOCX

```bash
fdoc convert report.docx -f doco
```

Word states outright what a PDF makes us infer: `w:pStyle` gives the heading
level, `w:numPr` marks a list item, `w:tbl` bounds a real table, and
`w:gridSpan` / `w:vMerge` state the cell merges the PDF engine has to read
back out of ruling geometry.

`page` is `0` throughout — a `.docx` stores a flow, not a layout, and page
boundaries exist only once something lays it out.

Returns `DocxError` for a malformed or non-archive file.

## PPTX

```bash
fdoc convert deck.pptx -f doco
```

The one declared format with a real page concept: **each slide is a page**, so
`page` carries the slide index and the graph keeps the deck's pagination.

Geometry is still absent. Shapes do have positions in EMUs, but those describe
a canvas layout rather than a text flow, and reporting them as `bbox` would
invite consumers to treat a deck like a scanned page.

A shape whose placeholder type is `title` or `ctrTitle` becomes the slide
heading; `a:tbl` is a real table with `gridSpan` / `rowSpan` merges;
paragraphs with a bullet character or a non-zero outline level are list items.

**Charts become tables.** Values come from the cached `c:strCache` /
`c:numCache` blocks — what the chart actually plots — because the `c:f`
formula beside them points into a workbook that may not travel with the deck.

Returns `PptxError` for a malformed or non-archive file.

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
  full width inside a table is a `sub_headers` band.
- **Values are the cached ones.** A formula's last computed value is what
  the file holds and what Excel shows; nothing is recalculated. Numbers
  render as the shortest decimal that reads back to the same value, dates
  and times as ISO where the cell's number format says the number is one,
  percentages as percentages. Other format details — thousands separators,
  currency signs, colours — are not rendered.

Hidden sheets, rows and columns are read like any other; a consumer that
wants what the author showed can drop them by name. Pictures anchored to
cells are not read. `bbox` is absent: a cell address is not a position on a
page, and the address is the table's row and column.

Returns `XlsxError` for a malformed or non-archive file.

## HTML

```bash
fdoc convert page.html -f doco
```

HTML declares structure, but unlike Markdown or OOXML it also carries a great
deal that is not document content — navigation, scripts, styling wrappers —
and real-world markup is frequently malformed. So the parse is spec-compliant
(Servo's html5ever) and the walk is **selective**: non-content subtrees are
dropped whole, and only elements naming a document role are emitted.

Nesting resolves **innermost wins**. A `<p>` inside a `<td>` inside a
`<table>` is table content, not a paragraph — emitting both would duplicate
the text, the same double-emission the PDF engine guards against when a grid's
glyphs would also become prose.

Text set straight in a container, with no `<p>` around it, is read the way a
browser lays it out: each run of it between blocks is a paragraph, and two
`<br>` in a row end one. That is how pages built from `<div>`s, and nearly
all email, set their text. A `<blockquote>` that holds paragraphs of its own
is their container rather than one paragraph run together.

Infallible: HTML is defined so that every byte sequence parses.

## Markdown

```bash
fdoc convert notes.md -f doco
```

The most direct mapping. A heading states its level, a list states its items,
a table states where its header ends. Where the PDF engine reports a
*measurement* — `header_rows` inferred from shading and value types — this
reports a *fact*.

Infallible, for the same reason as HTML.

## Why convert Markdown at all

It looks circular until you want the graph. `fdoc convert notes.md -f doco`
gives you DoCO typing, explicit section containment, addressable table cells
and char offsets over a Markdown file — the same graph shape a PDF produces,
so a mixed corpus lands in one ledger with one schema and one query surface.
