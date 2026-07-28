# render

```bash
fdoc render report.pdf ./pages              # 2x, ~144 dpi
fdoc render report.pdf ./pages --scale 3
fdoc render report.pdf ./pages --pages 1-5
```

Page images in the coordinate space the output's bounding boxes use. Written
as `<stem>_p<N>.png`, with **N 0-based** so the filename matches
`doc:pageIndex`.

## Why this is not "use any renderer"

A highlight is a rectangle in PDF user units; an image is pixels. They line up
only if the same code produced both. Rendering with a second PDF
implementation means reconciling two that were never guaranteed to agree, and
the failure mode is a highlight that drifts further down the page the further
you scroll — which reads as a CSS bug rather than a rendering one.

## Placing a box on the image

Coordinates are PDF user units with a **top-left origin**, so they are CSS
directly, with no flip. Multiply by the scale:

```html
<div style="position:absolute;
            left:  calc(var(--x) * 2px);
            top:   calc(var(--y) * 2px);
            width: calc(var(--w) * 2px);
            height:calc(var(--h) * 2px);"></div>
```

The page's own size is in [`doc:pages`](../formats/doco.md), so a viewer that
scales the image to fit can derive the same factor rather than hard-coding it.

## Options

| flag | means |
|---|---|
| `--scale <N>` | oversampling; 2 is ~144 dpi |
| `--pages <RANGES>` | 1-based, e.g. `3`, `1-5`, `1,4,9-12` |

## From Rust

Behind the `render` feature, so a consumer that only parses does not build a
rasteriser:

```toml
fluree-doc-pdf = { version = "0.1", features = ["render"] }
```

```rust
let raster = fluree_doc_pdf::render::page(&pdf, page_index, render::SCALE)?;
let png = raster.to_png()?;                       // whole page
let crop = raster.crop_to_png(x0, y0, x1, y1)?;   // pixels, not PDF units
```
