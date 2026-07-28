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

Without this, searching for `profile` finds one occurrence in six. Offsets are
in **characters** of the normalized string, not bytes.

## Tables

A table contributes its cells joined by tabs, rows by newlines — the same
string that appears in `nif:isString` for the table element:

```
a	b
1	2
```

## What is absent

Anything the layout pass dropped: page [furniture](../design/furniture.md),
headers, footers, watermarks. This is consistent with the graph — an element
that does not exist has no text and no offsets — so the two outputs never
disagree about what the document contains.
