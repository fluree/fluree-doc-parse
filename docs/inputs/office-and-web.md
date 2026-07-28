# DOCX, PPTX, HTML and Markdown

These four **declare** their structure. A `w:pStyle` names the heading level;
`<h1>` is a heading because it says so. So these readers map rather than
measure, and nothing they produce is a hypothesis a model tier could improve.

Two consequences apply to all four:

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
