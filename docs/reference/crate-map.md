# Crate map

```
fluree-doc-cli          the fdoc binary
      │
      ├── fluree-doc-pdf      PDF: extraction, layout, routing, arbitration
      │      └── fluree-doc-vision  reading pixels: the OCR word format
      ├── fluree-doc-xps      XPS and OpenXPS: pages as glyphs for the PDF layout
      ├── fluree-doc-image    bare images: decoded upright, read by OCR (uses the PDF layout)
      ├── fluree-doc-markdown  Markdown reader
      ├── fluree-doc-text      plain-text reader
      ├── fluree-doc-html      HTML reader
      ├── fluree-doc-docx      DOCX reader ─┐
      ├── fluree-doc-pptx      PPTX reader ─┼── fluree-doc-ooxml  the package layer they share
      ├── fluree-doc-xlsx      XLSX reader ─┘
      ├── fluree-doc-ppt       PowerPoint 97-2003 slide text
      ├── fluree-doc-doc       Word 97-2003 text, through the piece table
      ├── fluree-doc-xls       Excel 97-2003 reader (lays out with the XLSX reader)
      ├── fluree-doc-odf       OpenDocument text, sheets (with the XLSX layout), slides
      ├── fluree-doc-rtf       RTF reader
      ├── fluree-doc-csv       CSV and TSV reader
      ├── fluree-doc-transcript  WebVTT and SubRip reader
      └── fluree-doc-email     .eml and .msg reader (uses the HTML reader)
                │
                └── fluree-doc-model   element model + emitters (source-agnostic)
```

Every reader depends on `fluree-doc-model`, and on no other reader but two:
the email reader reads HTML bodies with the HTML reader, and the `.xls`
reader lays its sheets out with the XLSX reader. The three Office
readers share `fluree-doc-ooxml`, the package layer under all three formats.
That is the point of the split: **a Markdown or DOCX consumer never compiles
a PDF engine.** Only the DOCX and XLSX readers' `ocr` feature, which reads
their pictures with an engine, brings in `fluree-doc-image`, and with it the
PDF layout.

## The crates

| crate | contains |
|---|---|
| `fluree-doc-model` | `Element`, `Link`, `Target`, `Turn`, `Message`, `Mailbox`, `DocumentInfo`, `Attachment`, `Media` (an image placed in the flow) and `read_pictures`, the loop every container's pictures are read through, each once within a document's `Allowance`, `UnreadPage` and `UnreadImage` (what nothing read), `BBox`, `PageSize`, the Markdown/XHTML/DoCO/text emitters, merge denormalization, `outline::infer_headings` (headings shown by keyword and number) |
| `fluree-doc-markdown` | `parse(&str) -> Vec<Element>` |
| `fluree-doc-text` | `parse(&str) -> Vec<Element>`; `decode(&[u8])`; `is_markdown` |
| `fluree-doc-html` | `parse(&str) -> Vec<Element>`, via html5ever; `parse_with_quote_depth`, a message's body, its own images (`cid:`) as figures; `cid_ids`, the content ids its HTML shows; `parse_message`, both from one parse |
| `fluree-doc-ooxml` | `Package`: a `.docx`, `.pptx` or `.xlsx` opened as parts, its relationships with targets resolved to part names, its main part and core properties as the package declares them; `PartNames` (a relationship's image, found as the format finds it), `read_media` (an image part of an opened `Package`, capped), `read_pictures` (each image once within a per-document `Allowance`), `alt_text` |
| `fluree-doc-docx` | `parse(&[u8]) -> Result<Vec<Element>, DocxError>`; `parse_with_info` adds the core properties from the same opening, `parse_with_media` where each figure's images are, `read_pictures` each image once within a per-document `Allowance`, `read_media` one image's bytes from an opened `Package`; feature `ocr`: `parse_with_ocr`, the pictures' text read and placed after their figures |
| `fluree-doc-pptx` | `parse(&[u8]) -> Result<Vec<Element>, PptxError>`, incl. charts; `parse_with_info` |
| `fluree-doc-xlsx` | `parse(&[u8]) -> Result<Vec<Element>, XlsxError>`; `parse_with_info`; `grid_elements`, sheets another workbook format's reader read, laid out as an XLSX sheet; `parse_with_media` the pictures anchored to its cells, `read_media` one image's bytes; feature `ocr`: `parse_with_ocr`, the pictures' text read and placed after their figures |
| `fluree-doc-ppt` | `parse(&[u8]) -> Result<Vec<Element>, PptError>`: a `.ppt`'s slides through its last edit's persist directory (text, bullets as list items, tables, notes); `parse_with_media` the pictures each slide shows, `MediaReader` their bytes (`read_media` one's); `is_ppt` |
| `fluree-doc-doc` | `parse(&[u8]) -> Result<Vec<Element>, DocError>`: a `.doc`'s text, tables and notes through its piece table; `is_doc` |
| `fluree-doc-xls` | `parse(&[u8]) -> Result<Vec<Element>, XlsError>`, BIFF8 through calamine, given a workbook rewritten from the records a walk of its own checked; `parse_with_info`; `is_xls`; `parse_xlsb` and `is_xlsb`, the binary workbook, its package inflated against bounds first and its records read here |
| `fluree-doc-odf` | `parse_with_info(&[u8]) -> Result<(Vec<Element>, DocumentInfo, Format), OdfError>`: `.odt`, `.ods`, `.odp`; `Format::sniff` by the `mimetype` |
| `fluree-doc-rtf` | `parse_with_info(&[u8]) -> Result<(Vec<Element>, DocumentInfo), RtfError>`: paragraphs, headings by outline level or style, lists, tables, notes, comments, header and footer; `is_rtf` |
| `fluree-doc-csv` | `parse(&[u8]) -> Result<Vec<Element>, CsvError>`: one table under its header row, the delimiter sniffed; `parse_with` names it; `decode`, `sniff` |
| `fluree-doc-transcript` | `parse(&[u8]) -> Result<Vec<Element>, TranscriptError>`, WebVTT or SubRip; `Format::sniff` |
| `fluree-doc-email` | `parse(&[u8]) -> Result<Email, EmailError>`, `.eml` or `.msg`: elements, document info, attachments, `media` (the images its body shows, drawn as figures), `media_bytes` and `media_attachment`; `Format::sniff`; `parse_attachments`, the attachments down through attached messages, within `AttachmentLimits`, read by a reader the caller supplies |
| `fluree-doc-xps` | `parse(&[u8]) -> Result<Xps, XpsError>`: the pages as the PDF engine's `Document` (glyphs, image placements), the pictures each places (`page_image`, `read_image`), core properties; `reading_on_page`, an engine's words on the page; `is_xps` |
| `fluree-doc-pdf` | extraction, the layout pipeline, the router, the arbiter |
| `fluree-doc-image` | `decode` (PNG, JPEG, TIFF, upright by EXIF, at most 128 million pixels), `header` and `upright_size` (size upright, one-bit, page count: header only), `document`, `read` and `elements`: a bare image read by an `OcrEngine` and laid out as a scanned page; `elements_from_words` the same from words already read; `read_placed` the images a document places in its flow, read one at a time; `place_after_figures` puts a document's image text after its figures |
| `fluree-doc-vision` | `WordReading`, `OcrWord`, `Coords`: the words an OCR engine read in one image, and the `--ocr-words` sidecar format (`from_json`, `to_json`); `OcrEngine` and `Pixels`, the engine interface. Feature `oar`: `oar::OarEngine` (PP-OCRv6 through oar-ocr, ONNX Runtime loaded at run time) and `assets`, the checked assets directory |
| `fluree-doc-cli` | argument parsing and the commands |

## Inside fluree-doc-pdf

Supported API and internals are marked; see [Using fluree-doc-parse as a Rust
library](../getting-started/rust-library.md#what-is-a-compatibility-surface).

| module | role | |
|---|---|---|
| `extract` | PDF → glyphs (private; types re-exported) | ✅ via `extract_file` |
| `document` | the layout pipeline, producing elements | ✅ |
| `doco` | graph and text projection | ✅ |
| `route` | escalation verdicts | ✅ |
| `arbiter` | model-tier contract and splicing | ✅ |
| `forms` | AcroForm fields | ✅ |
| `outline` | PDF bookmark tree | ✅ |
| `link` | link annotations and their anchors | ✅ |
| `annot` | notes left on the pages, embedded files | ✅ |
| `image` | a raster image as a one-page document | ✅ |
| `overlay` | a text-projection span → page and rectangles | ✅ |
| `render` | rasterising a page (feature `render`) | ✅ |
| `glyph` | the glyph type (private; re-exported) | internal |
| `dedup` | faux-bold removal | internal |
| `line`, `block`, `column` | lines, paragraphs, columns | internal |
| `heading`, `table`, `rule` | structure detection | internal |
| `furniture` | headers/footers/watermarks | internal |
| `text`, `geom` | text projection helpers, geometry | internal |

## Third-party

| dependency | role | licence |
|---|---|---|
| hayro | PDF parsing, interpretation, rendering | Apache-2.0 OR MIT |
| ureq, rustls | HTTPS for a configured reader | MIT OR Apache-2.0 |
| webpki-roots | the Mozilla CA set | CDLA-Permissive-2.0 |
| rsa, sha2, base64 | signing a service-account assertion | MIT OR Apache-2.0 |
| toml, toml_edit | reading and editing the config | MIT OR Apache-2.0 |
| png | encoding rendered pages | MIT OR Apache-2.0 |
| flate2 | inflating a PDF's embedded files against a bound | MIT OR Apache-2.0 |
| html5ever | spec-compliant HTML parsing | MIT OR Apache-2.0 |
| pulldown-cmark | Markdown parsing | MIT |
| quick-xml, zip | OOXML containers | MIT |
| cfb | Outlook `.msg`, Word, Excel and PowerPoint 97-2003 containers | MIT |
| calamine | Excel 97-2003 (BIFF8) cells, pinned | MIT |
| encoding_rs | email, CSV and RTF charsets | (MIT OR Apache-2.0) AND BSD-3-Clause |
| clap | CLI | MIT OR Apache-2.0 |
| serde, serde_json | serialization | MIT OR Apache-2.0 |
| unicode-normalization | NFC, reading PDF presentation forms | MIT OR Apache-2.0 |

No copyleft anywhere in the tree — run `cargo tree` to verify. This is a
release constraint rather than a preference: the engine is meant to be
embedded in customer environments. The per-user config directory is resolved
directly rather than through the usual crate, whose transitive `option-ext` is
MPL-2.0 and would otherwise be the only exception.

The HTTPS stack is only built into the binary; `fluree-doc-pdf` itself reaches
no network, and its `render` feature is off by default so a consumer that only
parses does not build a rasteriser.

## Not in the workspace

`spike/hayro-spike` is a standalone exploration probe with its own
dependencies, deliberately excluded from the workspace.
