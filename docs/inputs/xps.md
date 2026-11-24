# XPS and OpenXPS

```bash
fdoc convert printed.xps -f doco
fdoc convert printed.oxps --ocr --models ~/assets/ocr
```

An XPS file (`.xps`, or OpenXPS's `.oxps`) is what Windows' "Microsoft XPS
Document Writer" prints to: a package of pages, each placing its text and
pictures at absolute positions. That is a PDF page's content, stated rather
than drawn, so it is read **as a PDF is**: each page's text runs become
positioned glyphs, and the PDF layout pipeline makes lines, paragraphs,
columns, headings, tables and furniture of them, with a `bbox` on every
element, page sizes in `doc:pages`, and running headers kept once. See
[PDF](pdf.md).

- **Pages** come from the package's fixed document sequence, in order, its
  documents one after another; `page` is the page's index across them.
- **Units.** XPS measures in 1/96 inch; boxes and page sizes are in PDF
  units (1/72 inch), y down, like a PDF's.
- **Glyph geometry is approximate.** A run states its origin, size and each
  character's advance; a glyph's height is taken as 0.8 em above and 0.2 em
  below its baseline, since the fonts' own metrics are in embedded (usually
  obfuscated) font parts that are not read. A character whose advance the
  run does not state is half an em wide, a space a quarter. Transforms
  (`RenderTransform`, nested canvases, `MatrixTransform`) are applied, so
  scaled and turned text is placed where it is drawn. A run drawn larger
  than its page, and a glyph drawn wholly off it, are no text anyone reads
  and are left out, and no advance is taken as more than ten ems.
- **Not read:** vector drawing (so a table drawn with ruled lines is found
  as an unruled one is), right-to-left runs as such, links, and content
  split into interleaved pieces.

## Pages that are pictures, and pictures on pages of text

A screen printed to XPS is one picture filling the page (a path filled with
an `ImageBrush`) and no text. Such a page routes as a scanned PDF page does
(`Scanned`, or `NearBlank` for a picture covering less of the page, in
[`doc:unreadPages`](../formats/doco.md) when nothing reads it). A picture
pasted into a page of text (a screenshot under a paragraph) is a raster
region of it, routed as a PDF page's is (`doc:unreadRegions` when nothing
reads it). Both are read the same ways as a PDF's:

1. **OCR**: `--ocr` reads the picture's own pixels, at their own resolution
   (the whole picture for a page, the part of it under the region for a
   region), and lays its words out where they are drawn; `--ocr-words`
   takes the words an engine already wrote, as `<stem>_p<N>_full.json` or
   `<stem>_p<N>_r<i>.json`, of those same pixels (its `width` and `height`
   theirs), and lays them out the same way. The pictures are decoded a
   page at a time, each read as it is cut, and at most 512 million pixels
   of them for a document, each picture's size checked before it is
   decoded; past that, the rest are named in a warning and left unread.
2. **A reading someone already has**: `--tier-results`, `<stem>_p<N>_full.json`
   for a page.
3. **The configured model**, sent the picture's bytes as they are (PNG, JPEG
   or TIFF), for a page OCR did not read, within `escalation.max_crops`. A
   region is read by OCR only.

`fdoc dev render-routed` writes each crop, decoded upright, as
`<stem>_p<N>_full.png` or `<stem>_p<N>_r<i>.png`, the names its words and
readings are looked up by, its manifest record giving the box on the page
it shows (`bbox`, whole picture or part: a picture rarely fills its page),
so the three steps (`render-routed
--ocr-regions --stored-resolution`, `fdoc ocr`, `convert --ocr-words`) read
what `convert --ocr` reads, to the character.

## Bounds

As for any archive: each part at most 64 MiB uncompressed and all parts
read 512 MiB, checked as they inflate (the package's relationships and
core properties at most 64 MiB each); at most 100,000 parts, 10,000 pages,
250,000 glyphs and pictures on a page and 4 million in all, counted before
a run is laid out, and elements nested 256 deep on a page. The pages are
laid out by the PDF pipeline, all held at once, at 370 to 530 bytes a
glyph: 4 million glyphs keep a document within about 2 GB, and a page of
250,000 takes a third of a second. A page side is at most 200 inches.
Past any, the file fails as too large.

In a library, `fluree_doc_xps::parse` returns the pages as the PDF engine's
`Document`, for `fluree_doc_pdf::document::analyze_with`, with the pictures
each page places (`page_image`, `read_image`) and the package's core
properties.
