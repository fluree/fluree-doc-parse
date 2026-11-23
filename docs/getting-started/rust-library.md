# Using fluree-doc-parse as a Rust library

Two crates matter to most consumers:

- **`fluree-doc-model`** — the element model and the emitters. Depend on this
  alone if you consume elements and never parse a PDF; it pulls in no source
  format, so a Markdown or DOCX consumer never compiles a PDF engine.
- **`fluree-doc-pdf`** — PDF extraction, layout inference, routing and
  arbitration.

A complete, runnable version of everything below is
`crates/fluree-doc-pdf/examples/library_usage.rs`. CI compiles it as part of
`cargo test --workspace`, so if this page drifts from the API, the build
fails:

```bash
cargo run --release --example library_usage -- path/to/document.pdf
```

## Parse a PDF

```rust
use fluree_doc_pdf::{document, extract_file};

let mut doc = extract_file(std::path::Path::new("report.pdf"))?;
let analysis = document::analyze(&mut doc, &[]);

for e in &analysis.elements {
    println!("{} p{} {}", e.kind, e.page, e.text);
}
```

That second argument is the PDF **outline** (bookmark tree) — a
near-ground-truth heading signal, and one no other engine tested uses. It is
read from the PDF object graph rather than the content stream, so it comes
from a separate call and you construct the `Pdf` yourself:

```rust
use fluree_doc_pdf::{document, extract_bytes, outline};

let data = std::fs::read("report.pdf")?;
let pdf = hayro_syntax::Pdf::new(std::sync::Arc::new(data.clone()))?;
let ol = outline::extract(&pdf);

let mut doc = extract_bytes(data)?;
let analysis = document::analyze(&mut doc, &ol);
```

Passing `&[]` costs you only the outline signal; the other heading detectors
still run.

## Emit

```rust
use fluree_doc_model::{doco::default_base_iri, sha256_hex, to_markdown, to_xhtml};
use fluree_doc_pdf::doco::{to_doco, to_text, DocoOptions};

let md    = to_markdown(&analysis.elements);
let xhtml = to_xhtml(&analysis.elements);
let text  = to_text(&analysis.elements);
let graph = to_doco(&analysis.elements, &DocoOptions {
    base_iri: default_base_iri(Some("report"), &sha256_hex(&std::fs::read("report.pdf")?)),
    doc_iri: None,
    ..Default::default()
});
```

All four return serialized `String`s. `base_iri` is what every node IRI the
graph mints starts with, and it is yours to choose: your document's own IRI,
or, as here, the one `fdoc convert` uses when it is given none — the file's
stem and the first twelve hex digits of its SHA-256, so two files named
`report` do not mint the same nodes. See [IRIs and
re-extraction](../formats/doco.md#iris-and-re-extraction).

`to_text` and `to_doco` are a pair: the `nif:beginIndex` / `nif:endIndex`
values in the graph are character offsets into exactly the string `to_text`
returns. See [The text projection](../concepts/text-projection.md).

## Check what would escalate

```rust
use fluree_doc_pdf::route;

for page in &doc.pages {
    let (verdict, signals) = route::decide(page);
    println!("{verdict:?} glyphs={} unicode={:.3}",
             signals.glyphs, signals.unicode_rate);
}
```

Call this *before* `analyze` if you want to skip structure work on pages you
are going to escalate anyway.

## Read a scan with your own OCR engine

A scanned page has no text layer to analyse. Give it one: an engine's words,
added as glyphs before `analyze`, go through the same assembly as any text.

```rust
use fluree_doc_pdf::ocr::{self, Crop, WordReading};

let reading = WordReading::from_json(&std::fs::read_to_string("scan_p0_full.json")?)?;
let injected = ocr::inject(&mut doc, &[(0, Crop::Page, reading)]);
let mut analysis = document::analyze(&mut doc, &ol);
ocr::mark_provenance(&mut analysis.elements, &injected);
```

`inject` takes a reading only for a page or region the router sends a reader,
and reports what it took and refused per reading. The reading format and the
rules are under [OCR words](../integration/sidecar-formats.md#4-ocr-words----ocr-words).

## Read a scan with the built-in OCR engine

With `fluree-doc-pdf`'s `render` feature and `fluree-doc-vision`'s `oar`
feature, the engine renders and reads the pages itself, in memory:

```rust
use fluree_doc_vision::oar::{OarEngine, OarOptions};

// Once per process: checks the assets, loads ONNX Runtime and the models.
let engine = OarEngine::open(Path::new("/opt/assets/ocr"), &OarOptions::default())?;

// Per document, before `analyze`.
// Each crop at the resolution of the image under it: what an engine reads.
// At most 1,000 crops a document, as `ocr.max_crops` defaults to; 0 for no
// limit.
let stored = fluree_doc_pdf::escalate::CropScale::Stored;
let pass = ocr::read_pages(&mut doc, &pdf, &engine, None, stored, 1000);
let mut analysis = document::analyze(&mut doc, &ol);
ocr::mark_provenance(&mut analysis.elements, &pass.injected);
```

`read_pages` reads the same crops `--ocr-words` expects (`ocr::ocr_jobs`):
pages whose text is pixels, whole, and the unread raster regions of the
others, small ones included. It renders one page at a time, at most four
times for its crops, and reads
each crop as it is cut, so a long scan holds one render of one page and one
crop at a time, and a crop the engine cannot read is reported in
`pass.failed` while the rest is read. The engine is `Send + Sync`: share one
across threads. Any other engine plugs in by implementing
`fluree_doc_vision::OcrEngine`.

`OarOptions` sets how images are sized (`sizing`: at most 12.5 Mpx a pass by
default, `OcrSizing::megapixels` for another cap), the memory the passes read
at once may take (`memory`, 8 GiB by default, counted at
`oar::BYTES_PER_PIXEL`), and whom the engine tells of an image it shrank
(`notices`; nobody by default). `OarEngine::memory_held` says what the passes
being read hold now.

A bare image (PNG, JPEG, TIFF) is one call, decoded upright and laid out as a
scanned page:

```rust
let read = fluree_doc_image::elements(&bytes, &engine)?;
// read.elements, read.size (the page, in the image's pixels), read.more_pages
```

The pictures of a DOCX, their text after each figure (`fluree-doc-docx`'s
`ocr` feature):

```rust
let read = fluree_doc_docx::parse_with_ocr(&bytes, &engine)?;
// read.elements, read.info; read.unread: each picture not read, with why, as
// doc:unreadImages lists it (read.errors: what went wrong, by picture);
// read.first_page_only: multi-page images of which only the first page was read
```

Each image is read once however many figures draw it, and a document has at
most `Allowance::DOCUMENT` read (256 images, 512 million pixels). Without the
feature, `parse_with_media` lists each figure's images (figure, and a
`MediaSource`: a package part, a file outside the package, or a missing
relationship), `fluree_doc_docx::read_pictures` reads each image once with
your `PictureReader`, within an `Allowance`, and
`fluree_doc_image::place_after_figures` puts what you read where it belongs
(`Picture::after_each_figure` gives it once per figure). `read_media` reads one
part of a package opened once (`fluree_doc_docx::Package::open`).
`fluree-doc-xlsx` does the same for the pictures anchored to a workbook's
cells (`parse_with_ocr`, `parse_with_media`, `read_pictures`, `read_media`),
and `fluree-doc-email` for the images a message shows in its body
(`email.media`, `email.media_bytes`), through the same loop
(`fluree_doc_model::read_pictures`). `fluree_doc_image::read_placed` is the
reading `parse_with_ocr` runs: each image read by an engine, once.

## Read a non-PDF source

Each reader returns the same `Vec<Element>`:

```rust
let els = fluree_doc_markdown::parse(&src);       // &str
let els = fluree_doc_text::parse(&src);           // &str; decode(&bytes) gives one
let els = fluree_doc_html::parse(&src);           // &str
let els = fluree_doc_docx::parse(&bytes)?;        // &[u8]
let els = fluree_doc_pptx::parse(&bytes)?;        // &[u8]
let els = fluree_doc_xlsx::parse(&bytes)?;        // &[u8]
let els = fluree_doc_transcript::parse(&bytes)?;  // &[u8], WebVTT or SubRip
let email = fluree_doc_email::parse(&bytes)?;     // &[u8], .eml or .msg
let els = &email.elements;                        //   plus email.info, email.attachments
```

Those elements have `bbox: None` — see [Measured vs declared
structure](../concepts/geometry-vs-declared.md).

A text file may be Markdown under another name; `fluree_doc_text::is_markdown`
tells, as `fdoc` asks before choosing a reader. The HTML and plain-text
readers read the headings a statute or contract shows without declaring them
(`CHAPTER IV`, `Article 30`, `§ 7001. Definitions.`). The same reading is
`fluree_doc_model::outline::infer_headings`, for elements from anywhere else;
see [HTML](../inputs/office-and-web.md#html).

## What is a compatibility surface

Most of `fluree-doc-pdf` is `pub` for a mechanical reason: `fdoc` is a
separate crate, so everything `fdoc dev` inspects has to be reachable.
Reachability is not an offer of stability.

**Supported** — follows semver:

| item | purpose |
|---|---|
| `extract_file` / `extract_bytes` | PDF → `Document` |
| `document::analyze` / `analyze_with` | `Document` → elements |
| `document::to_markdown` / `to_xhtml` | emitters |
| `doco::to_doco` / `to_text` / `DocoOptions` | graph and text projection |
| `route::decide` / `route::signals` | escalation verdicts |
| `forms::fields` | AcroForm values |
| `outline::extract` | PDF bookmark tree |
| `link::extract` / `link::attach` | link annotations and their anchors |
| `image::page_of` | a raster image of a given size as a one-page document; `fluree_doc_image::header` reads the size upright |
| `arbiter::TierBackend` / `arbiter::splice` | the model-tier contract |
| `arbiter::scrub_furniture` | strip running headers from a model reading |
| `ocr::inject` / `ocr::mark_provenance` / `ocr::WordReading` | OCR words as glyphs, before `analyze_with` |
| `ocr::read_pages` / `ocr::ocr_jobs` | read a document's pixels with an `OcrEngine`, in memory (feature `render`) |
| `fluree_doc_vision::OcrEngine` / `oar::OarEngine` | the engine interface, and PP-OCRv6 through oar-ocr (feature `oar`) |
| `overlay::highlight` | a text-projection span → its page and rectangles |
| `overlay::rects_for_glyph_range` | glyph span → rectangles |
| `render::page` (feature `render`) | rasterising a page |
| all of `fluree-doc-model` | the element model and emitters |

**Internal** — `block`, `column`, `dedup`, `fidelity`, `figure`, `furniture`,
`geom`, `heading`, `line`, `rule`, `table`, `text`. This is layout machinery whose shape follows
whatever the measurements demand, and it may change in any release. It stays
documented because the reasoning in it is worth reading, not because it is
stable — the same promise [`fdoc dev`](../cli/dev.md) makes about its output.

`extract` and `glyph` are private; their types (`Document`, `Page`,
`ExtractError`, `Glyph`) are re-exported at the crate root.

## Upgrading

What a caller of these crates has to change, by the work that changed it,
and what replaces each call removed or changed. No public struct is
`#[non_exhaustive]`, and most have every field public, so each field one
gains breaks a caller building it with a struct literal: build
`DocoOptions`, `Notes` and `escalate::CropHints` with
`..Default::default()`, as the example above does, rather than field by
field. `Page` and `ImagePlacement` have no `Default`: a literal of either,
a test's page, names every field. Read the other structs below rather than
build them.

### The OCR engine, its crops and bare images (#17 to #21)

- `escalate::render_crops(pdf, jobs)` is now
  `render_crops(pdf, doc, jobs)`. It renders at the model scale as before,
  and cuts each region where it shows on the page as displayed, mapped from
  the frame `doc` reads the page in. That is the frame of the jobs
  `escalate::crops_for` and `crops_for_with` return, so a caller of either
  passes the document it analysed: `render_crops(&pdf, &doc, &jobs)`. The
  old call cut a region of a page read turned (`/Rotate`, or text drawn
  sideways) in the wrong frame, and it came out blank or not at all.
  `render_crops_displayed(pdf, jobs)` cuts each region where it says, as
  the old call did, for regions already in the display frame, as a
  `doc:bbox` in the graph is. On an upright page the two cut the same
  crops.
- `image::as_document(bytes)` is gone: `fluree_doc_image::header(bytes)`
  reads the image's size, upright, and whether it is one bit deep, and
  `image::page_of(width, height, bitonal)` builds the one-page document.
  `as_document` sized an image as stored, its EXIF orientation ignored;
  `image::dimensions` still gives that size. `fluree_doc_image::read` and
  `elements` decode and read an image with an engine in one call.
- `Page` gains `ocr`, what an OCR reading added to the page (`None` for a
  page none was injected into); `ImagePlacement` gains `ppi`, `bitonal` and
  `grid`; `escalate::CropHints` gains `regions_apart`.
- `escalate::CROP_MARGIN` is defined in `route` and re-exported where it
  was. `UnreadPage::reason` gains `OcrSparse`, a page an OCR reading read
  whole that still holds next to no text, and `LaterPage`, a page after
  the first of a multi-page image file.
- `escalate::CropReason` gains `SmallRegion`, a raster region too small
  for a model's call that an OCR engine still reads, and `EmbeddedImage`,
  an image a document places in its flow. The enum is not
  `#[non_exhaustive]`: a `match` over it needs an arm for each, or `_`.
- New: `ocr` (`inject`, `mark_provenance`, `read_pages`, `ocr_jobs`, the
  word format), `route::ocr_regions`, `escalate::CropScale`, `crop_scale`
  and `for_each_crop_of`, the crate `fluree-doc-vision` (`OcrEngine`,
  `oar::OarEngine`), the crate `fluree-doc-image`, and
  `fluree_doc_docx::parse_with_ocr` and `parse_with_media`.

### Regions nothing read

- `fluree_doc_pdf::Page` gains `routing`, what the router made of the page
  as it was drawn (`None` until `analyze_with` keeps it; `Page::routing()`
  decides it when absent, `Page::keep_routing()` keeps it), and `warnings`,
  what the PDF interpreter reported while reading it. A `Page` literal needs
  both. `ImagePlacement` gains `key`, what the image shows (`None` when not
  known, or when its box shows more or less than the image: a glyph, a path
  or another image overlapping it, a clip cutting it).
- `fluree_doc_model::DocoOptions` and `Notes` gain `unread_regions` and
  `warnings`. `Notes` is no longer `Eq`, only `PartialEq`: a region's box is
  floating-point.
- New: `fluree_doc_pdf::unread_regions` and `parse_warnings`, which fill
  them; `route::Routing`, `route::Declined`, `route::declined_regions` (and
  `_for`, `_numbered`), `route::ocr_regions_for`; `ocr::copies`,
  `ocr::with_copies`, `ocr::MIN_PICTURE_CONFIDENCE` and
  `MIN_FRAGMENTS_CONFIDENCE`; `fluree_doc_vision::texty::lines_of_text`.
- Changed behaviour: `doc:unreadPages` calls a page that is one large
  image with next to no text layer `Scanned`, where it was `NearBlank`; the
  graph gains `doc:unreadRegions` (`Region`, `Small`, `NotTexty`) and
  `doc:parseWarnings`.
- New in this release, nothing to change from main: `ocr::ocr_jobs` and
  `ocr::read_pages` read the small texty regions after the routed ones, and
  an image drawn again at its size, with nothing else in its box, once; a
  reading of words from files should go through `ocr::with_copies` before
  `ocr::inject`, or the copies stay unread. `inject` sets aside a region's
  reading that is noise, and a reading with a score outside 0-1 has no
  confidence.

## Errors

`extract_file` and `extract_bytes` return `Result<Document, ExtractError>`.
`ExtractError`, `DocxError` and `PptxError` all implement
`std::error::Error`, so they compose with `?`, `Box<dyn Error>` and `anyhow`.

`fluree_doc_markdown::parse`, `fluree_doc_text::parse` and
`fluree_doc_html::parse` are infallible — each format is defined so that every
byte sequence is a valid parse.
