# Entity overlay

Drawing a highlight on a rendered page where an entity was found in the text.

The whole feature rests on one property: **the text and the coordinates come
from the same parse**, so they agree by construction. Pairing an extractor
with a separate PDF viewer instead means reconciling two coordinate systems
that were never guaranteed to match.

## The chain

```
NER finds "Fluree PBC" at chars 4210..4220 of the text projection
  → the element whose [nif:beginIndex, nif:endIndex) contains 4210
  → that element's glyph range
  → merged per-line rectangles
  → doc:pageIndex + a 2× page render
  → a positioned <div>
```

## Step 1 — text and graph from one run

```bash
fdoc convert report.pdf -f text > report.txt
fdoc convert report.pdf -f doco > report.jsonld
```

Run NER over `report.txt` — **not** over the Markdown. Markdown's `#` and `|`
characters shift every offset after them, and the resulting highlights drift
progressively down the page in a way that looks like a rendering bug. See
[The text projection](../concepts/text-projection.md).

## Step 2 — offset to element

Find the element whose `[nif:beginIndex, nif:endIndex)` contains the mention's
start. That gives you `doc:pageIndex` and `doc:bbox` immediately, which is
enough for **element-level** highlighting — good enough for "show me the
paragraph this came from".

## Step 3 — exact spans

For a rectangle around the mention itself rather than its paragraph, one call
takes the projection offsets straight to rectangles:

```rust
use fluree_doc_pdf::overlay::highlight;

let hit = highlight(&analysis.elements, &doc.pages, begin, end)?;
// hit.page   0-based, the same space as doc:pageIndex
// hit.rects  one per visual line, PDF units, top-left origin
// hit.text   what was actually covered — check it against what you asked for
```

`hit.text` is there because the three offset spaces in play are not
interchangeable, and a mismatch is otherwise invisible: you would get a
rectangle in the wrong place rather than an error. Compare it with the
mention's own string before drawing.

`None` means the span fell outside every element, or landed in one with no
geometry, or could not be found among the page's glyphs. The element-level box
from step 2 is still there to fall back on.

The lower-level `rects_for_glyph_range(&page.glyphs, start, end_inclusive)`
remains available for callers that already hold glyph indices.

One rect per visual line, so a span wrapping two lines yields two rectangles.
Whitespace glyphs carry no box and are skipped rather than breaking the run,
so `"Hype Cycle"` yields a single rect rather than two.

To see it working without writing code:

```bash
$ fdoc dev find report.pdf "Features"
page 0   norm_off 47    [{"x":67.59,"y":108.07,"w":48.55,"h":8.73}]
page 1   norm_off 19    [{"x":64.86,"y":104.95,"w":36.41,"h":6.55}]
4 match(es) for "Features"
```

## Step 4 — render and position

```bash
fdoc render report.pdf ./pages              # 2× (~144 dpi), <stem>_p<N>.png
```

Or from Rust, behind the `render` feature:

```rust
let raster = fluree_doc_pdf::render::page(&pdf, hit.page, render::SCALE)?;
```

Page sizes travel in the graph as [`doc:pages`](../formats/doco.md), so a
viewer scaling the image to fit can derive the factor rather than assuming it.

Coordinates are PDF user units with a **top-left origin**, so they are
directly usable as CSS with no flip. At 2×, multiply by 2 for pixels:

```html
<div style="position:absolute;
            left:   calc(var(--x) * 2px);
            top:    calc(var(--y) * 2px);
            width:  calc(var(--w) * 2px);
            height: calc(var(--h) * 2px);"></div>
```

## The three offset spaces

They are not interchangeable, and `highlight` exists because crossing between
them by hand is where this goes wrong.

| space | what it counts | who uses it |
|---|---|---|
| the text projection | characters of element text, elements joined by a blank line | `nif:beginIndex`, NER mentions |
| element text | characters of one element | nothing on its own |
| page glyphs | drawn glyphs, no synthetic spaces, page order not reading order | `rects_for_glyph_range` |

A table's contribution to the projection is its cells joined with tabs, not
`Element::text` — so walking the projection by re-deriving element text drifts
by exactly that difference, silently, from the first table onward.
`highlight` walks it with the same function that built it.

Going offset → element → element bbox touches none of this. It only matters
for a span *inside* an element.

## Text selection

Rendered pages are images: there is no selectable text over them. If
copy/paste matters, emit an invisible positioned text layer from the same
extraction pass — the standard approach for overlaying selectable text on a
page image, but guaranteed consistent with your offsets because it comes
from the same index rather than a second parse.

## Where it degrades

Elements with `provenance: "vlm"` have text derived from pixels, and a model
that supplies element-level boxes cannot support character-level highlighting.
Highlight the containing element and indicate the difference rather than
drawing a rectangle you cannot justify. See [Provenance and
evidence](../concepts/provenance.md).
