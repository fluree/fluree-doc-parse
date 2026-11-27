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
from a separate call: open the `Pdf` once with `fluree_doc_pdf::open`, and
extract from it:

```rust
use fluree_doc_pdf::{document, extract_pdf, open, outline};

let pdf = open(std::fs::read("report.pdf")?)?;
let ol = outline::extract(&pdf);

let mut doc = extract_pdf(&pdf)?;
let analysis = document::analyze(&mut doc, &ol);
```

`open` measures the page tree before the parser walks it, and refuses one
that would overflow the stack or list more pages than it can hold
([Limits](../reference/limits.md#a-pdfs-page-tree)); `hayro_syntax::Pdf::new`
called directly walks it unguarded, and a crafted file aborts the process.

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
// read.first_page_only: multi-page images of which only the first page was read;
// read.ocr: each reading taken, with its confidence, as doc:ocrReadings lists it
// (a fluree_doc_model::OcrReading, new, as is the `ocr` field of each parse)
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
let els = fluree_doc_doc::parse(&bytes)?;         // &[u8], Word 97-2003
let els = fluree_doc_ppt::parse(&bytes)?;         // &[u8], PowerPoint 97-2003
let els = fluree_doc_xls::parse(&bytes)?;         // &[u8], Excel 97-2003
let els = fluree_doc_odf::parse(&bytes)?;         // &[u8], .odt, .ods or .odp
let els = fluree_doc_rtf::parse(&bytes)?;         // &[u8], RTF
let els = fluree_doc_csv::parse(&bytes)?;         // &[u8], delimiter sniffed
let els = fluree_doc_transcript::parse(&bytes)?;  // &[u8], WebVTT or SubRip
let email = fluree_doc_email::parse(&bytes)?;     // &[u8], .eml or .msg
let els = &email.elements;                        //   plus email.info, email.attachments
```

Those elements have `bbox: None` — see [Measured vs declared
structure](../concepts/geometry-vs-declared.md).

A reader counts what it makes against one budget per document
(`fluree_doc_model::budget`), at the costs of the largest output, DoCO,
which writes each table cell as a node of its own. A caller converting to
Markdown, XHTML, JSON or the text says so around the reading, and a table
too large for DoCO still reads:

```rust
use fluree_doc_model::budget::{converting_to, Output};
let els = converting_to(Output::Flat, || fluree_doc_xlsx::parse(&bytes))?;
```

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
and what replaces each call removed or changed. Only `ExtractOptions` is
`#[non_exhaustive]`; the other structs mostly have every field public, so each field one
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
  once faux bold counts once and an overlaid watermark is out (`None` until
  `analyze_with` or an OCR reading keeps it; `Page::routing()`
  decides it when absent, `Page::keep_routing()` keeps it), and `warnings`,
  what the PDF interpreter reported while reading it. A `Page` literal needs
  both. `ImagePlacement` gains `key`, what the image shows (`None` when not
  known, or when its box shows more or less than the image: a glyph, a path
  or another image overlapping it, a clip cutting it). Its `texty` is an
  `Option<bool>`: `None` when extraction did not probe the image
  (`ExtractOptions::probe_images` off, `extract_bytes_with`).
- `fluree_doc_model::DocoOptions` and `Notes` gain `unread_regions` and
  `warnings`. `Notes` is no longer `Eq`, only `PartialEq`: a region's box is
  floating-point. `==` still compares two of them; code that needs
  `Notes: Eq` (a generic bound `T: Eq`, or a struct of its own deriving
  `Eq` with a `Notes` field) breaks, and derives `PartialEq` only.
- `document::routing_is_consulted(&opts)` is gone: the analysis now routes
  every page, to number its regions once, whatever reads the verdict. What
  it answered, whether anything downstream acts on the routing, is
  `opts.emit_anchors || opts.layout_prefix.is_some()`, as `fdoc convert`
  now asks it when it decides whether to probe the images.
- `ExtractOptions` gains `key_images` (on by default): each placement's
  `key`, which an OCR engine reads an image drawn again once by. A caller
  that reads with no engine turns it off, so no image stream is hashed and
  no stencil decoded for it; `ExtractOptions` is `#[non_exhaustive]`, so
  this breaks no caller.
- New: `fluree_doc_pdf::unread_regions` and `parse_warnings`, which fill
  them; `route::Routing`, `route::Declined`, `route::declined_regions` (and
  `_for`, `_numbered`), `route::ocr_regions_for`; `ocr::copies`,
  `ocr::with_copies`, `ocr::MIN_PICTURE_CONFIDENCE` and
  `MIN_FRAGMENTS_CONFIDENCE`; `fluree_doc_vision::texty::lines_of_text`.
- Changed behaviour: `doc:unreadPages` calls a page that is one large
  image with next to no text layer `Scanned`, where it was `NearBlank`; the
  graph gains `doc:unreadRegions` (`Region`, `Small`, `NotTexty`,
  `Unprobed`) and `doc:parseWarnings`. `fdoc convert` decodes no image
  unless it escalates, reads with OCR, emits anchors, reads layout
  sidecars or is given `--probe-images`.
- New in this release, nothing to change from main: `ocr::ocr_jobs` and
  `ocr::read_pages` read the small texty regions after the routed ones, and
  an image drawn again at its size, with nothing else in its box, once; a
  reading of words from files should go through `ocr::with_copies` before
  `ocr::inject`, or the copies stay unread. `inject` sets aside a region's
  reading that is noise, and a reading with a score outside 0-1 has no
  confidence.

### Pictures a document places

- `fluree_doc_email::Email` gains `media`, and `AttachedFile` gains
  `content_id`, which an `AttachedFile` literal needs. An inline image is a
  `doco:Figure` holding its alternative text, except an image under the
  floor a model is sent (a signature's `LinkedIn` or `Logo` icon), whose
  figure holds none.
- `DocoOptions` and `Notes` gain `unread_images`, listed in the graph as
  `doc:unreadImages`: `NotRead`, `Missing`, `UnsupportedFormat`,
  `PastAllowance`, `Small`, `NotTexty` or `Failed`.
- `escalate::CropReason` gains `SmallImage`, an image a document places
  under the pixels a model is sent, which an OCR engine still reads. That
  floor is `fluree_doc_model::under_model_floor` (`MIN_SHORT_SIDE_PX`,
  `MIN_LONG_SIDE_PX`), and `fluree_doc_model::stored_size` reads an image's
  size from its header; `fluree_doc_pdf::image::dimensions` now calls it.
- New: `fluree_doc_xlsx::parse_with_ocr` and `parse_with_media`,
  `fluree_doc_image::read_placed`, `texty`, `looks_like_text` and
  `without_noise`, `ocr::is_noise`, `route::pasted` and `pasted_set`,
  `route::IMAGE_DOMINANT_COVERAGE` (public, where it was private),
  `fluree_doc_html::parse_message` and `cid_ids`, the probe's other rules
  in `fluree_doc_vision::texty`.
- New in this release, nothing to change from main; listed for a caller
  of this work's earlier pull requests:
  - The picture loop moves out of `fluree_doc_docx`, to be shared by a
    DOCX, an XLSX and an e-mail: its types (`MediaSource`, `Media`,
    `Picture`, `pictures`, `Allowance`, `Unread`, `PictureReader`,
    `PictureReads`) to `fluree_doc_model::media`, and `read_pictures`,
    `read_media` and `MAX_MEDIA_BYTES` to `fluree_doc_ooxml`.
    `fluree_doc_docx` re-exports them all and `fluree_doc_xlsx` most, so a
    path through either still resolves.
  - `MediaSource` names an image as any of these files does, not only as a
    package part: `Part` is also an e-mail's `cid:` URL or attachment
    name, `Missing` a reference the file does not resolve, and `name()`
    the name whatever it resolves to. `Unread::NotInPackage` says "not in
    the file".
  - `fluree_doc_docx::OcrParse::unread` is a list of
    `fluree_doc_model::UnreadImage`, the graph's own entries, where it held
    `(Picture, Unread)` pairs, and `OcrParse` gains `errors`.
    `fluree_doc_image::ImageElements` gains `not_texty` and `noise`.

### OCR readings and their confidence

- `DocoOptions` and `Notes` gain `ocr_readings` (`ocr`, on `Notes`), listed
  as `doc:ocrReadings`. A literal that names every field of either no
  longer compiles: add the field, or end it with `..Default::default()`,
  which both implement and which keeps a literal compiling through later
  additions.
- New in this release, nothing to change from main; listed for a caller
  of this work's earlier pull requests:
  - The OCR parses of `fluree_doc_docx`, `fluree_doc_xlsx` and
    `fluree_doc_image` (`ImageElements`, `PlacedRead`) gain `ocr`, and
    `ImageElements` gains `refused`.
  - `ocr::Injected` gains `min_conf`, `engine`, `regions` and `noise`
    (an `ocr::Noise`: the words a region's reading held when it was set
    aside as noise, and their confidence; `ocr::noise_of` gives it for a
    picture's reading); `ocr::readings` lists the readings an output holds,
    and `fluree_doc_model::OcrReading` is one of them, its `set_aside`
    saying why a reading's words are not in the output (`noise`);
    `WordReading::scores_out_of_range` and `words_with_text` count the
    scores outside 0-1 a warning names.
  - A reading none of whose words land on what it read, or a region's most
    of whose words land off its crop, is refused, where it was taken as a
    reading that came back blank; a placed picture whose reading is
    refused so is `Failed` in `doc:unreadImages`.

### Limits on what a document may take

The budget itself, what each reader spends and the calls it changed (the
readers that now return a `Result`, `converting_to`, `within`, the
`Package` reads), are listed in [Changes for library
callers](../reference/crate-map.md#changes-for-library-callers); what the
limits on it changed:

- Each reader's error gains `TooLarge(fluree_doc_model::budget::TooLarge)`:
  `ExtractError`, `DocxError`, `PptxError`, `XlsxError`, `CsvError`,
  `DocError`, `PptError`, `XlsError`, `OdfError`, `RtfError`, `XpsError`
  and `EmailError`. None of these enums is `#[non_exhaustive]`: a `match`
  over one needs an arm for it, or `_`. A caller of this work's earlier
  pull request, where the variant held a `String`, gets the same message
  from `e.to_string()`, and which limit was passed from `e.limit()`.
- `fluree_doc_pdf::Document` gains `pages_past_limit`, the pages after the
  last one read, and `cut_short`, where the budget stopped the reading
  before the page limit (`CutShort`): a `Document` literal needs both
  (`pages_past_limit: 0, cut_short: None` for one read whole).
- `annot::annotations(pdf, budget)`, of this work's earlier pull request,
  is `annotations(pdf, pages, budget)`: the notes of the first `pages`
  pages only, those read (`doc.pages.len()`).
- `UnreadPage::reason` gains `PastPageLimit`, `PastMemoryLimit` and
  `PastWorkLimit`: a page a limit left unread ([Limits](../reference/limits.md#pages)).
- Changed behaviour: a PDF is read to its 10,000th page, and one whose
  memory or work runs out on a page after the first is read to the page
  before, where it was refused; the pages not read are listed by
  `unread_pages`. `render::page` draws a page past the render pixels at the
  scale that fits, which `Raster::scale` says. A PDF opened by
  `fluree_doc_pdf::open`, as `extract_bytes` opens one, is refused when its
  page tree nests or lists past the bounds the parser can walk. Converted
  by `fdoc`, a PDF's, an XPS document's and an OCR reading's tables are
  spent the copies DoCO writes of their cells (`spend_table_copies`): a
  PDF or an XPS document past them is read shorter, an image refused, and
  `fluree_doc_image::read_texty` and `elements_from_words` return `Err`
  for a reading whose tables' copies are past one document's budget.
- New: `fluree_doc_model::budget::Limits`, `limited_to`, `limits`, `Limit`,
  `MAX_PAGES` and `MAX_RENDER_PIXELS`, `TooLarge::new`, `fixed`, `within`,
  `limit` and `max`; `fluree_doc_model::PAST_PAGE_LIMIT`,
  `PAST_MEMORY_LIMIT`, `PAST_WORK_LIMIT`, `past_limit_reason` and
  `is_past_limit`; `fluree_doc_pdf::open`, `extract_pdf`,
  `extract_pdf_with`, `extract_pdf_within`, `PageBudget`,
  `spend_table_copies`, `past_limit`, `CutShort` and the module `page_tree`;
  `render::fitted_scale`, `page_scale`, `shrunk_by`, `page_shrunk_by` and
  `MAX_RENDER_SIDE`.

## Errors

`extract_file` and `extract_bytes` return `Result<Document, ExtractError>`.
`ExtractError` and every reader's error implement `std::error::Error`, so
they compose with `?`, `Box<dyn Error>` and `anyhow`.

`fluree_doc_markdown::parse`, `fluree_doc_text::parse` and
`fluree_doc_html::parse` read every byte sequence; they fail only past a
limit, returning the `fluree_doc_model::budget::TooLarge` itself.

### Limits

A document past what it may take ([Limits](../reference/limits.md)) is
refused with a `TooLarge`: the readers' errors carry it in their
`TooLarge` variant, and as their `source()`. It says which limit was
passed, and its value:

```rust
use fluree_doc_model::budget::{limited_to, Limit, Limits, TooLarge};

let mut limits = Limits::default();
limits.memory = 512 << 20; // what one document may take, estimated bytes
limits.pages = 500;        // a PDF or XPS read to its 500th page
let read = limited_to(limits, || fluree_doc_docx::parse(&bytes));
if let Err(fluree_doc_docx::DocxError::TooLarge(e)) = &read {
    match e.limit() {
        Limit::Memory => eprintln!("past {} bytes: {e}", e.max()),
        Limit::Fixed => eprintln!("past a bound of the reader's own: {e}"),
        _ => eprintln!("{e}"),
    }
}
```

A PDF past the page limit, or past its memory or work on a page after the
first, is not refused: it is read short, to the last page that fit, and
`doc.pages_past_limit` counts the pages after it. The copies DoCO writes of
its tables' cells are spent from the same budget once the elements are
final: read it with `extract_pdf_within`, keeping the `PageBudget`, and
pass that to `fluree_doc_pdf::spend_table_copies` before writing, which
reads the document shorter where they do not fit (an XPS document or an
OCR reading spends them from a `PageBudget::default()`):

```rust
let raw = fluree_doc_pdf::open(bytes)?;
let mut budget = fluree_doc_pdf::PageBudget::default();
let mut doc = fluree_doc_pdf::extract_pdf_within(&raw, &Default::default(), &mut budget)?;
let mut analysis = fluree_doc_pdf::document::analyze(&mut doc, &[]);
fluree_doc_pdf::spend_table_copies(&mut doc, &mut analysis.elements, &mut budget)?;
```
 Nothing in the elements
says so, and `to_markdown(&elements)` carries no notes: list the pages
(`fluree_doc_pdf::unread_pages`, each `PastPageLimit`, `PastMemoryLimit`
or `PastWorkLimit`, `fluree_doc_model::is_past_limit` telling them apart)
and pass them on, as `to_markdown_with` writes them in band:

```rust
let unread = fluree_doc_pdf::unread_pages(&doc, &analysis.elements);
if unread.iter().any(|u| fluree_doc_model::is_past_limit(&u.reason)) {
    eprintln!("read to page {}", doc.pages.len());
}
let notes = fluree_doc_model::Notes { unread, ..Default::default() };
let md = fluree_doc_model::to_markdown_with(&analysis.elements, &notes);
```

`limited_to` holds every reader called inside it, on its thread, to those
limits; outside it they are `Limits::default()`. A thread a caller spawns
starts at the defaults: call `limited_to` in it too. A reader's own bounds
(`Limit::Fixed`: a part read beside the document, the glyphs of an XPS
page) are not among the limits a caller sets. `Limit::Pages` is a document
listing more pages than a reader holds before it reads any (250,000, or
the page limit where that is more).

## A document's budget

Each reader's entry point makes the document's budget
(`fluree_doc_model::budget::Budget::default()`) from three settings of the
thread that calls it: the limits (`budget::limited_to`, above), the output
the document is converted to (`budget::converting_to`), whose costs it
counts at, and the share of memory `budget::within` leaves it, the least
of that share and the memory limit. All three are thread-local. A caller
that runs readers on a pool of its own (rayon, a blocking pool) sets them
inside each task, on the worker thread: set on the thread that hands the
work out, they do not reach the reader, which counts at DoCO's costs with
the default limits. See [Limits](../reference/limits.md).

Pages read as parts of one document can share its budget, as an e-mail's
HTML versions do: `fluree_doc_html::parse_within`, `parse_message_within`
and `cid_ids_within` take a `&mut Budget` and leave it with what the page
leaves; a page refused spends all its work, so the next is refused at once.
