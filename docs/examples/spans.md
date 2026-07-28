# Finding text on the page

You have a string — a search hit, a regex match, an entity your NER model
found — and you want to point at it in the original document. This is the
thing structure-free extractors cannot do, and it is the reason the [text
projection](../formats/text.md) and the [DoCO graph](../formats/doco.md) are
two views of one parse rather than two unrelated outputs.

The contract: `nif:beginIndex` and `nif:endIndex` are character positions in
exactly the string `-f text` writes. Find your match in that string, look up
the element whose interval contains it, and you have the page and the box.

## The index

```python
import bisect
import json
import re
import subprocess


def run(path, fmt):
    return subprocess.run(
        ["fdoc", "convert", path, "-f", fmt],
        capture_output=True, check=True,
    ).stdout.decode("utf-8")


class Located:
    """The text projection, indexed so a char offset resolves to an element."""

    def __init__(self, path):
        self.text = run(path, "text")
        graph = json.loads(run(path, "doco"))["@graph"]
        self.spans = sorted(
            (n["nif:beginIndex"], n["nif:endIndex"], n)
            for n in graph
            if "nif:beginIndex" in n
        )
        self.starts = [s[0] for s in self.spans]

    def at(self, offset):
        i = bisect.bisect_right(self.starts, offset) - 1
        if i < 0:
            return None
        begin, end, node = self.spans[i]
        return node if begin <= offset < end else None

    def find(self, pattern):
        for m in re.finditer(pattern, self.text):
            node = self.at(m.start())
            if node is None:
                continue
            box = node.get("doc:bbox")          # absent for non-PDF sources
            yield {
                "match": m.group(0),
                "page": node["doc:pageIndex"],
                "bbox": tuple(float(v) for v in box.split(",")) if box else None,
                "type": node["@type"],
                "element": node["@id"],
            }
```

```python
doc = Located("report.pdf")
for hit in doc.find(r"\d+\.\d+\s?MHz"):
    x0, y0, x1, y1 = hit["bbox"]
    print(f"p{hit['page']}  {hit['type']:<20} "
          f"({x0:.0f},{y0:.0f})-({x1:.0f},{y1:.0f})  {hit['match']!r}")
```

```
p0   doco:Figure          (57,128)-(287,173)   '1.2MHz'
p0   doco:Table           (57,569)-(555,697)   '0.7\tMHz'
p30  doco:Paragraph       (57,522)-(555,711)   '0.7 MHz'
```

The interval list is sorted and disjoint, so `bisect` gives you O(log n)
lookup over the whole document, and every interval slices back to its own
`nif:isString` exactly.

The same index works on a DOCX or an HTML file — offsets are emitted for every
source. Only `bbox` is PDF-only, which is why `find` reads it with `.get()`:
those formats [declare structure rather than placing
it](../concepts/geometry-vs-declared.md), so there is no rectangle to return
and the key is absent rather than zeroed.

## Drawing the box

`bbox` is in PDF user units with a **top-left origin**, so it maps to CSS
without a flip. At the default 2× render scale, multiply by two:

```python
scale = 2
style = (f"left:{x0 * scale}px; top:{y0 * scale}px; "
         f"width:{(x1 - x0) * scale}px; height:{(y1 - y0) * scale}px")
```

To render the page underneath it:

```bash
fdoc render report.pdf ./pages              # PNGs at 2x
```

## What the box actually is

**It is the element's box, not the match's box.** The rectangle above covers
the whole paragraph, not the four characters that matched. For most
highlighting that is the better answer anyway — a reader looking for context
wants the paragraph lit up.

For a rectangle around the phrase itself, `overlay::highlight` takes the same
offsets and returns per-line rects:

```rust
use fluree_doc_pdf::overlay::highlight;

let hit = highlight(&analysis.elements, &doc.pages, begin, end)?;
// hit.page, hit.rects, and hit.text — what was actually covered
```

Check `hit.text` against the string you searched for. The offset spaces in
play are not interchangeable, and a mismatch shows up as a rectangle in the
wrong place rather than as an error. See [Entity
overlay](../integration/entity-overlay.md#the-three-offset-spaces).

To see it without writing code, `fdoc dev find` resolves a string to the same
rectangles — but it lives under [`fdoc dev`](../cli/dev.md), which is
explicitly unstable and not something to build a product surface on.

Where the text came from a model tier (`provenance: "vlm"`), the per-character
option does not exist at all — the model returns element boxes. Degrade to the
element and show it as an element, rather than drawing a tight rectangle you
cannot justify. [Entity overlay](../integration/entity-overlay.md) covers the
full degradation ladder.

## Two ways to get this wrong

**Matching against the wrong string.** Every offset belongs to `-f text`. Run
your regex over `-f md` and the `#` and `|` characters push every subsequent
offset out of alignment — progressively, so the first page looks right and
page thirty is nonsense. This failure reads like a rendering bug and is not.

**Assuming table cells are separated the way `-f json` shows them.** In the
text projection a table's cells are joined with **tabs**; in `-f json` the
same table's `text` joins them with ` | `. Note the `'0.7\tMHz'` hit above — a
pattern with a literal space between the number and the unit finds the
paragraph on page 30 and misses the table on page 0. Use `\s` when a pattern
may cross a cell boundary.

## Normalization

The text is NFKC-normalized, so ligatures are already decomposed — `ﬁnd`
became `find` before your regex ran. This is what makes a search for `profile`
find all six occurrences rather than one. Normalize your query the same way if
it might contain composed forms:

```python
import unicodedata
pattern = unicodedata.normalize("NFKC", user_query)
```
