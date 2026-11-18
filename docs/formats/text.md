# Text

```bash
fdoc convert report.pdf -f text
```

The plain-text projection. Each text-bearing element's trimmed text in reading
order, separated by blank lines. Nothing else — no page markers, no headers,
no decoration.

## Why it is not just "the text"

This is **the string [`doco` char offsets](doco.md) index into**. That is its
job. `nif:beginIndex` / `nif:endIndex` are character positions in exactly this
output, so the pair forms a contract:

```bash
fdoc convert report.pdf -f text > report.txt
fdoc convert report.pdf -f doco > report.jsonld
```

Find a mention at `[start, end)` in `report.txt`, look up the element whose
interval contains it, and you have its page and bounding box — which is how an
entity found by NER becomes a highlight on a rendered page. See [The text
projection](../concepts/text-projection.md) and [Entity
overlay](../integration/entity-overlay.md).

## Do not substitute another format

Markdown adds `#` and `|` characters. XHTML adds tags. Both shift every offset
after them. If you run NER over `-f md` and resolve the offsets against a
`doco` graph, the highlights will be subtly and increasingly wrong down the
page — the failure mode that looks like a rendering bug and is not.

## Normalization

Text is NFKC-normalized, so ligatures are decomposed:

```
raw  : reﬂect ﬁnd reﬁne proﬁle ofﬁcer signiﬁcant
NFKC : reflect find refine profile officer significant
```

Superscripts and subscripts are kept as printed: `m³/h` stays `m³/h` rather
than folding to `m3/h`.

Without this, searching for `profile` finds one occurrence in six. Offsets are
in **characters** of the normalized string, not bytes.

A character set for a degree sign that is not one reads as `°`: the ordinal
`º`, the ring `˚`, a superscript `⁰`, a ring operator `∘` or a white bullet
`◦` before a temperature's unit (`25 ºC` → `25 °C`), and the ring or ring operator after a
number (`45˚` → `45°`). NFKC would turn `º` into the letter `o` and `˚` into a
space and a combining mark. An ordinal stays an ordinal: `1º` and `Nº` fold as
NFKC folds them.

## Tables

A table contributes its cells joined by tabs, rows by newlines — the same
string that appears in `nif:isString` for the table element:

```
a	b
1	2
```

An empty cell keeps its field, so every row puts its values at the same tab
positions. When the first cell of the first row is empty (the corner above a
column of row labels), the table's text starts with a tab, and so does its
`nif:isString`. Compare an element's string with its slice untrimmed.

A value merged down over several rows is on each of them. A line is read on
its own, and a row whose merged cell is left blank has lost what it is
about:

```
Grain	G-1
Grain	G-2
```

The table's cells in the [DoCO graph](doco.md) carry the same value on every
row, and a repeated one's offsets point at its copy on its own line. A value
merged across columns is not repeated: a label over two columns names them
both once. [Markdown](markdown.md) keeps a merged value on its first row
only, as a reference rendering of the table does.

## What is absent

Anything the layout pass dropped: a PDF's page [furniture](../design/furniture.md),
headers, footers, watermarks. A Word file's header and footer are read once,
as the file stores them, and are text like the rest:
see [DOCX](../inputs/office-and-web.md#docx). This is consistent with the graph — an element
that does not exist has no text and no offsets — so the two outputs never
disagree about what the document contains.
