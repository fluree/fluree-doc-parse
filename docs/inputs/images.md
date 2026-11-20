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
by an OCR engine (`--ocr`, or the words one wrote, `--ocr-words`), by a
model's reading supplied (`--tier-results`) or by the configured model, and
without any of them the honest output is nothing. It is warned about as a
PDF's page nothing read is, and listed in `doc:unreadPages`:

```
warning: scan: fluree-doc-parse: page 1 carries content no reader transcribed (Scanned). This output is missing it.
note: scan: read it with --ocr, --ocr-words, --tier-results or a configured model (`fdoc config gemini --credentials <key.json>`)
```

That warning matters more here than anywhere else, because an empty document
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
`scanned`. [`fdoc ocr`](../cli/ocr.md) reads it, or the image file itself.
Either way the words land where `convert` reads them.

An image given on its own is given to be read, and is read as a scanned
page is: the pixel probe that keeps a document's photographs and charts from
any reader does not judge it, nor does the noise rule an OCR reading of a
picture or of a PDF region is held to. What an engine reads in it is in the
output, however unsure, as the words of a scan read poorly are. However few
they are, the image is read: a stamp's `PAID` may be all it holds. The floor
a scanned PDF's page is held to (`OcrSparse`, a page read whole under 40
characters) judges a page among pages of text, and is not applied to an
image given alone; only an image OCR took no word in is listed unread. One
it took only words it doubts in, their mean confidence under 0.5 (the rule
a placed picture's reading is held to), is sent to the configured model
where one is, as one it took nothing in: a photograph's one "word" at 0.21,
a whiteboard's stray letters. A stamp's `PAID` at 0.97 is not. Without a
model, OCR's words are the output.

Where OCR reads nothing, and a model is configured, the model reads the image
as it would without OCR: words for other files only, a reading with no word,
or an image too large to decode. Not with `--tier-results`: the readings
given there are the model's, spliced over what OCR made of the image. Should
the model fail (a key it cannot sign in with, the network), what OCR made of
the image is kept, with a warning, and its reading, which took no word or
only words it doubts, is listed in `doc:ocrReadings`: the conversion fails
only where no OCR reading of the image was there to keep (no words written
for it). Should the model find no text, a reading of words it doubts is not
listed: a reading is never listed whose words are not in the output.

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
`Scanned`, the reason an unread image is listed with). It then takes the
same page-tier path, with the same arbitration and the same output shape. No
rule anywhere downstream knows an image is different.

The bytes are sent as they arrived, with their real media type. A file named
`.png` holding a JPEG is common enough that the format is read from the
content; re-encoding to fit one code path would spend a decode and could only
lose detail the reader might have used.

## Images inside a document

The images a document places in its flow — the pictures of a DOCX, the
pictures anchored to an XLSX's cells, the images an e-mail shows in its
body — are read by these same rules, each as a one-page image named
`<stem>_m<n>` (its crop `<stem>_m<n>.png`, its words and its reading
`<stem>_m<n>.json`), and what is read in it is placed after its figure. See
[Office files](office-and-web.md#docx) and
[E-mail](email.md#images-in-the-body).

## Size and coordinates

**Pixels are treated as PDF units at 1:1.** A bare image declares no physical
size, and inventing one from an assumed DPI would put a wrong number in
[`doc:pages`](../formats/doco.md) for every consumer that scales by it. So a
1224 × 1584 PNG reports a 1224 × 1584 page, and a highlight drawn from
[`doc:bbox`](../integration/entity-overlay.md) needs no scaling at all. A photo
turned upright by its EXIF orientation reports the upright size.

For the model, and for words already read (`--ocr-words`), dimensions come
from the file header and its EXIF orientation, without decoding (a PNG's
colour profile and text chunks are inflated within 16 MiB, past which they
are dropped). An engine decodes the image, since it reads its pixels: an
image of more than 128 million pixels, longer than 65,535 on a side, or of
more than 512 MiB as stored (a 16-bit RGBA image of more than 64 Mi pixels)
is refused before anything is allocated, as a file of a few kilobytes can
declare a size that takes gigabytes to decode.

Grey stays grey, one byte a pixel, and colour three; only an image with
transparency is held as RGBA. Decoding peaks at about **1 GB** in the worst
case, an 8-bit RGBA image at the cap turned a quarter by its EXIF
orientation (the turn holds two copies); unturned, or grey, it takes half of
that or less. The OCR engine then adds an RGB copy of the image (384 MB at
the cap) and its own working memory. Size a worker for the largest images it
takes.

## What a model reading does not give

- **No `doc:bbox` on elements.** A page reading arrives as one block; there
  are no glyphs to place its parts by. The page's own box is all there is.
  An OCR reading has boxes.
- **No tables as grids.** A reading may contain HTML table markup, but the
  cells were not measured, so `header_rows` is absent — treat it as 1.
- **No links.** Annotations live in a PDF's object graph; an image has none.
