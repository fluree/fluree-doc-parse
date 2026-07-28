# The text projection and char offsets

A contract between two outputs: `-f text` produces a string, and `-f doco`
produces a graph whose elements carry character intervals into **exactly that
string**.

```bash
fdoc convert report.pdf -f text > report.txt
fdoc convert report.pdf -f doco > report.jsonld
```

```jsonc
{ "@id": "urn:fluree-doc-parse:report/element/2",
  "@type": "doco:Paragraph",
  "nif:beginIndex": 0,
  "nif:endIndex": 5,
  "nif:isString": "小田切 亘" }
```

`report.txt[0..5]` is that paragraph. Offsets are in **characters**, not
bytes — which matters the moment a document is not ASCII.

## Why this exists

Named-entity recognition runs over text. Highlighting a mention runs over
pixels. The offset contract is the bridge: find a mention at `[start, end)` in
the text, look up the element whose interval contains it, and you have its
page and bounding box.

```
NER finds "Fluree PBC" at chars 4210..4220
  → element whose [beginIndex, endIndex) contains 4210
  → that element's doc:pageIndex and doc:bbox
  → a rectangle to draw
```

Because the text and the coordinates come from the same parse, they agree by
construction. That is the decisive advantage over pairing an extractor with a
separate PDF viewer, which requires reconciling two coordinate systems.

## The projection

`to_text` emits each text-bearing element's trimmed text in reading order,
separated by blank lines. Nothing else — no headers, no page markers, no
decoration. It is a projection *of the elements*, so anything the layout pass
dropped (page [furniture](../design/furniture.md), watermarks) is absent from
both outputs consistently.

## Normalization

Element text is **NFKC-normalized**. This is not cosmetic. PDFs emit ligatures
as single codepoints, and searching un-normalized output for `profile` finds
one occurrence in six:

```
raw  : reﬂect ﬁnd reﬁne proﬁle ofﬁcer signiﬁcant
NFKC : reflect find refine profile officer significant
```

NFKC can change string length, so raw glyph offsets and normalized text
offsets are not the same coordinate. The engine keeps a bijective map between
them; the offsets in `doco` are in **normalized** space, matching the text
`-f text` gives you.

## The one seam

`fdoc dev find` reports offsets in **raw glyph space** — no synthetic spaces,
because glyphs are what carry bounding boxes. Element text has spaces inserted
where the layout pass measured word gaps. A consumer mapping *element-text*
offsets down to glyphs must account for that insertion.

This matters only if you are resolving offsets to rectangles yourself. If you
consume `overlay::rects_for_glyph_range` or the
[entity-overlay](../integration/entity-overlay.md) path, it is handled.

## Stability

Offsets are stable for a given input and engine version. They are **not**
stable across versions: a layout improvement that merges two paragraphs
differently shifts every offset after it. Store the text alongside the
offsets, or re-derive both together.
