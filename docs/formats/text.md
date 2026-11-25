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

Text keeps the characters the document prints, in every input format: `µm`,
`½`, `™`, `（２）`, `m³/h` and `H₂O` come out as they are, so the same
document gives the same text whether it arrives as PDF, HTML or DOCX. A
PDF's text is put in NFC, so a letter and the accent a PDF draws after it
read as one character (`é`).

A PDF also draws some characters as glyph forms no other format carries, and
those are read as the letters they stand for:

```
raw  : reﬂect ﬁnd reﬁne proﬁle ofﬁcer signiﬁcant
text : reflect find refine profile officer significant
```

- ligatures and the other presentation forms (Latin and Hebrew ligatures,
  Arabic contextual forms, vertical and small CJK punctuation);
- Kangxi radicals a font's character map names for the ideograph they look
  like (`⾊` for `色`);
- a character set for a degree sign that is not one: the ordinal `º`, the
  ring `˚`, a superscript `⁰`, a ring operator `∘` or a white bullet `◦`
  before a temperature's unit (`25 ºC` → `25 °C`), and the ring or ring
  operator after a number (`45˚` → `45°`). An ordinal stays an ordinal: `1º`
  and `Nº` are kept.

Without the first, searching for `profile` finds one occurrence in six.
Offsets are in **characters** of this text, not bytes.

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
headers, footers, watermarks, on every page they repeat on. A running header
and footer are text once: a Word file's as the file stores them (see
[DOCX](../inputs/office-and-web.md#docx)), and a PDF's as its first page prints
the header and its last page the footer. A running head that names the section
the page is in is text once as well, where it is first printed, at the start of its
section (see [furniture](../design/furniture.md#a-running-head-that-names-its-section)).
This is consistent with the graph — an element
that does not exist has no text and no offsets — so the two outputs never
disagree about what the document contains.
