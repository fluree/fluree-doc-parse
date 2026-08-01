# XHTML

```bash
fdoc convert report.pdf -f xhtml
```

XHTML fragments — `h1`–`h6`, `p`, `ul`/`li`, `table` — inside a minimal
document wrapper.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><body>
<h1>1 Features</h1>
<p>Wide supply range of 3V to 36V</p>
<ul>
<li>For all available packages, see the datasheet</li>
</ul>
<table>
<tr><th>PART NUMBER (1)</th><th>PACKAGE</th><th>PACKAGE SIZE (2)</th></tr>
<tr><td>LM358B, LM358BA</td><td>D (SOIC,8)</td><td>4.9mm × 6mm</td></tr>
</table>
</body></html>
```

## Headings that admit they are guesses

```html
<h1>Sustainability Impact Review</h1>
<h6 class="doco-level-uncertain">From waste to new beginnings</h6>
```

A level from the bookmark outline, a numbering scheme, or the document's own
title is something the author declared. A level from font size or weight is
this library's reading of how the page looks — often right, never a fact.
Those carry `class="doco-level-uncertain"`.

The point is to make a downstream pass affordable. A model adjudicating a
bounded set of doubtful headings costs a fraction of one re-reading the
document, and where the file declares its structure the set is nearly empty:

| document | outline entries | headings | uncertain |
|---|---|---|---|
| a benefits handbook | 447 | 491 | 81 |
| a report with bookmarks | 9 | 14 | 4 |
| a magazine with none | 0 | 33 | 30 |

The same fact is in [`json`](json.md) and [`doco`](doco.md) as
`evidence` — `outline`, `numbering` and `title` are declared, `font-size` and
`bold` are inferred.

## Links

A PDF's link annotations are read and emitted as `<a href>` around the words
they cover:

```xml
<p>See <a href="https://www.sec.gov/…">the filing</a> for detail.</p>
```

A jump to elsewhere in the document becomes `href="#page=12"` — 1-based, the
fragment convention PDF viewers use. Anchors on a picture, or inside a table
cell, have nowhere to go here and appear only in [`json`](json.md) and
[`doco`](doco.md). See [Markdown's account](markdown.md#links) for the rest.

## Pages nothing read

A page carrying content the output does not hold gets a comment:

```html
<!-- fluree-doc-parse: page 1 carries content no reader transcribed
     (NearBlank). This output is missing it. -->
```

A comment rather than text, so extracting text from this file does not read a
sentence this library wrote as though the document had said it. The
machine-readable form is [`doc:unreadPages`](doco.md). Escalating the page
clears it.

## Why this exists

It is a drop-in for pipelines built around an HTML-producing extractor —
notably a docling-based extraction worker, whose consumer parses exactly this
shape. Adopting `fdoc` in such a pipeline needs no downstream change.

## What it loses

The same as [Markdown](markdown.md): no pages, no boxes, no offsets, no
evidence. DoCO typing survives only as far as HTML tags can carry it, which is
lossy in a specific way worth knowing:

| DoCO | tag | what is lost |
|---|---|---|
| `doco:SectionTitle` | `<h1>`–`<h6>` | nothing |
| `doco:Paragraph` | `<p>` | nothing |
| `doco:ListItem` | `<li>` | nothing |
| figure fragment | `<span>` | that it is prose at all |
| `doco:Table` | `<table>` | sub-header bands; merges survive as `rowspan`/`colspan` |
| `doco:Figure` | `<figure data-figure="…">` | **that it is an escalation anchor** |
| `doco:Section` | — | containment is implicit in heading order |
| — | — | page, bbox, evidence: nowhere to put them |

Consecutive figure fragments sharing a `figure` id are wrapped in one
`<figure>` element, which is the one piece of grouping this format keeps that
Markdown does not:

```xml
<figure data-figure="figure-0-0">
<span>• 2mV input offset voltage maximum at 25°C (BA</span>
<span>version)</span>
</figure>
```

Their order inside the wrapper is the page's, not a reading of the drawing —
see [Figures come in groups](json.md#figures-come-in-groups).

Recovering DoCO types by reading tag names back — the round-trip a separate
structuring service would perform — cannot restore what the tags never
carried. That is the argument for emitting [`doco`](doco.md) directly where
the consumer can accept it.

## Tables keep their spans

This is the one format that expresses merged cells directly. `merged_down` and
`merged_left` become real attributes, so a table round-trips into a browser or
an HTML-aware consumer without any work on your part:

```xml
<tr><th colspan="3">PIN</th><th rowspan="2"></th></tr>
<tr><td rowspan="2">Differential input voltage</td><td>–32</td><td>V</td></tr>
```

If you need the spans in some other structure, [`json`](json.md) exposes the
same information as flags — see [Tables to rows and
spans](../examples/tables.md).

### Where a span cannot be drawn

The continuation flags describe a cell's neighbours, and neighbours can
describe a region no table can tile — an L, where a cell merged downward has a
neighbour in the row below that merges left. HTML has no way to express that
shape, and this emitter currently drops those positions rather than degrading
them: measured across the evaluation corpora, 53 grid positions fall outside
any span and 33 of them carry text that then appears nowhere in the output.

It is rare — those 33 cells sit in 86 documents' worth of tables — but it is
silent, which is the part worth knowing. **If losing no text matters more than
keeping the spans, use [`doco`](doco.md).** Its cells are independent nodes
with no spans to tile, so the same tables lose nothing: 53 positions, 0
absent. That is the difference between a format that represents merges and one
that resolves them.

## Header rows

The first `header_rows` rows become `<th>`; the rest are `<td>`. Where
`header_rows` is `None` (a model-supplied table), one header row is assumed.

## Escaping

Text is XML-escaped. The output is well-formed XML, so a strict parser will
accept it — which is not true of the HTML that some extractors emit.
