# Images

```bash
fdoc convert scan.png  -f doco --ocr oar-ocr --models ~/assets/ocr
fdoc convert page.jpg  -f md   --ocr-words words/
fdoc convert fax.tiff  -f text                     # with a model configured
```

PNG, JPEG and TIFF (classic and BigTIFF), read as **one page of pixels**.

## The one input with no deterministic reading

Every other source this library takes has a text layer or declared structure,
so there is always something to fall back on. An image has neither. It is read
by an OCR engine, or by the deep reader, and without either the honest output
is nothing:

```
note: image/png carries no text layer, so only a model can read it
      run `fdoc config gemini --credentials <key.json>` to enable one
```

That message matters more here than anywhere else, because an empty document
is otherwise indistinguishable from a blank image. See
[`fdoc config`](../cli/config.md).

## Read by OCR

With the built-in engine (`--ocr`, see [`fdoc convert`](../cli/convert.md#ocr))
or the words an engine wrote (`--ocr-words`, the file `<stem>_p0_full.json`, or
`<stem>.json` as `fdoc ocr` names the words of an image file), the image's page
is **upright** — a phone photo's EXIF orientation applied — and the engine's
words are laid on it as glyphs, exactly as on a scanned PDF page. The same layout pass then gives paragraphs, tables and headings, each
element with its `doc:bbox`, marked `provenance: "ocr"`.

For an outside engine, [`fdoc dev render-routed`](../cli/dev.md) writes the
image's crop, `<stem>_p0_full.png`: the image upright, at its own resolution
(`scale` 1), drawn over white where it is transparent, with the reason
`near_blank`. [`fdoc ocr`](../cli/ocr.md) reads it, or the image file itself.
Either way the words land where `convert` reads them.

Where OCR reads nothing, and a model is configured, the model reads the image
as it would without OCR: words for other files only, a reading with no word,
or an image too large to decode. Not with `--tier-results`: the readings
given there are the model's, spliced over what OCR made of the image.

Only the first page of a multi-page TIFF is read, with a warning, and the
others are listed in [`doc:unreadPages`](../formats/doco.md) with the reason
`LaterPage`. Some TIFF codings are not decoded and stop the image with the
decoder's error: CCITT Group 3 fax (Group 4 is read), YCbCr and CIELab colour.
A JPEG that ends before its image does is read as far as it goes, with a
warning: the decoder fills in the rest, so part of the page may be blank.

In a library, `fluree_doc_image::elements(bytes, &engine)` does all of it, and
`fluree_doc_image::read` stops before the analysis.

## Read by a model

Without OCR, an image becomes a single page carrying itself and no glyphs,
which is what makes [the router](../design/router.md) send it to a reader, as
it sends a scanned PDF page (an image with no glyphs at all is the router's
`NearBlank`, the reason an unread image is listed with). It then takes the
same page-tier path, with the same arbitration and the same output shape. No
rule anywhere downstream knows an image is different.

The bytes are sent as they arrived, with their real media type. A file named
`.png` holding a JPEG is common enough that the format is read from the
content; re-encoding to fit one code path would spend a decode and could only
lose detail the reader might have used.

## Size and coordinates

**Pixels are treated as PDF units at 1:1.** A bare image declares no physical
size, and inventing one from an assumed DPI would put a wrong number in
[`doc:pages`](../formats/doco.md) for every consumer that scales by it. So a
1224 × 1584 PNG reports a 1224 × 1584 page, and a highlight drawn from
[`doc:bbox`](../integration/entity-overlay.md) needs no scaling at all. A photo
turned upright by its EXIF orientation reports the upright size.

For the model, and for words already read (`--ocr-words`), dimensions come
from the file header and its EXIF orientation, without decoding. An engine
decodes the image, since it reads its pixels: an image of more than 128
million pixels is refused before anything is allocated, as a file of a few
kilobytes can declare a size that takes gigabytes to decode.

## What a model reading does not give

- **No `doc:bbox` on elements.** A page reading arrives as one block; there
  are no glyphs to place its parts by. The page's own box is all there is.
  An OCR reading has boxes.
- **No tables as grids.** A reading may contain HTML table markup, but the
  cells were not measured, so `header_rows` is absent — treat it as 1.
- **No links.** Annotations live in a PDF's object graph; an image has none.
