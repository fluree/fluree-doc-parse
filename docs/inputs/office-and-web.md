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

In Rust, `fluree_doc_html::parse_with_quote_depth` returns beside the
elements how many cited quotations enclose each one: a `<blockquote>` with
`type="cite"` or a `cite` address, Gmail's `gmail_quote`, and the containers
Yahoo and Proton put around a quote. A plain `<blockquote>`, as an indent
button writes one, does not count. The [email](email.md) reader splits a
thread by it.

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
