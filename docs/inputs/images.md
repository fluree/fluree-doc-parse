# Images

```bash
fdoc convert scan.png  -f doco
fdoc convert page.jpg  -f md
fdoc convert fax.tiff  -f text
```

PNG, JPEG and TIFF, read as **one page of pixels**.

## The one input with no deterministic reading

Every other source this library takes has a text layer or declared structure,
so there is always something to fall back on. An image has neither. It is the
case the deep reader exists for, and without one configured the honest output
is nothing:

```
note: image/png carries no text layer, so only a model can read it
      run `fdoc config gemini --credentials <key.json>` to enable one
```

That message matters more here than anywhere else, because an empty document
is otherwise indistinguishable from a blank image. See
[`fdoc config`](../cli/config.md).

## How it is handled

An image becomes a single page carrying itself and no glyphs, which is what
makes [the router](../design/router.md) return `Scanned` — the same verdict a
scanned PDF page gets, reached the same way. It then takes the same page-tier
path, with the same arbitration and the same output shape. No rule anywhere
downstream knows an image is different.

The bytes are sent as they arrived, with their real media type. A file named
`.png` holding a JPEG is common enough that the format is read from the
content; re-encoding to fit one code path would spend a decode and could only
lose detail the reader might have used.

## Size and coordinates

Dimensions come from the file header rather than from decoding — a page size
is four numbers, and a decoder is a large dependency and a large attack
surface for them.

**Pixels are treated as PDF units at 1:1.** A bare image declares no physical
size, and inventing one from an assumed DPI would put a wrong number in
[`doc:pages`](../formats/doco.md) for every consumer that scales by it. So a
1224 × 1584 PNG reports a 1224 × 1584 page, and a highlight drawn from
[`doc:bbox`](../integration/entity-overlay.md) needs no scaling at all.

## What you do not get

- **No `doc:bbox` on elements.** A page reading arrives as one block; there
  are no glyphs to place its parts by. The page's own box is all there is.
- **No tables as grids.** A reading may contain HTML table markup, but the
  cells were not measured, so `header_rows` is absent — treat it as 1.
- **No links.** Annotations live in a PDF's object graph; an image has none.
