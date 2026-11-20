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
  scaled and turned text is placed where it is drawn.
- **Not read:** vector drawing (so a table drawn with ruled lines is found
  as an unruled one is), right-to-left runs as such, links, and content
  split into interleaved pieces.

## Pages that are pictures

A screen printed to XPS is one picture filling the page (a path filled with
an `ImageBrush`) and no text. Such a page routes as a scanned PDF page does
(`Scanned` in [`doc:unreadPages`](../formats/doco.md) when nothing reads it),
and is read the same ways:

1. **OCR**: `--ocr` reads the picture's own pixels, at their own resolution,
   and its words are laid out where the picture is drawn; `--ocr-words`
   takes the words an engine already wrote, as `<stem>_p<N>_full.json`.
2. **A reading someone already has**: `--tier-results`, `<stem>_p<N>_full.json`.
3. **The configured model**, sent the picture's bytes as they are (PNG, JPEG
   or TIFF), for a page OCR did not read, within `escalation.max_crops`.

`fdoc dev render-routed` writes each such page's picture, decoded upright,
as `<stem>_p<N>_full.png`, the name its reading is looked up by.

## Bounds

As for any archive: each part at most 64 MiB uncompressed and all parts
read 512 MiB, checked as they inflate; at most 100,000 parts, 10,000 pages,
5 million glyphs, and elements nested 256 deep on a page. Past any, the file
fails as too large.

In a library, `fluree_doc_xps::parse` returns the pages as the PDF engine's
`Document`, for `fluree_doc_pdf::document::analyze_with`, with the pictures
each page places (`page_image`, `read_image`) and the package's core
properties.
