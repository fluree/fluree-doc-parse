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
// At most 1,000 crops a document, as `ocr.max_crops` defaults to; 0 for no
// limit.
let pass = ocr::read_pages(&mut doc, &pdf, &engine, None, 1000);
let mut analysis = document::analyze(&mut doc, &ol);
ocr::mark_provenance(&mut analysis.elements, &pass.injected);
```

`read_pages` reads the same crops `--ocr-words` expects (`ocr::ocr_jobs`):
pages whose text is pixels, whole, and the unread raster regions of the
others, small ones included. It renders one crop at a time, so a long scan
holds one page's pixels, and a crop the engine cannot read is reported in
`pass.failed` while the rest is read. The engine is `Send + Sync`: share one
across threads. Any other engine plugs in by implementing
`fluree_doc_vision::OcrEngine`.

`OarOptions` sets how images are sized (`sizing`: at most 12.5 Mpx a pass by
default, `OcrSizing::megapixels` for another cap), the memory the passes read
at once may take (`memory`, 8 GiB by default, counted at
`oar::BYTES_PER_PIXEL`), and whom the engine tells of an image it shrank
(`notices`; nobody by default). `OarEngine::memory_held` says what the passes
being read hold now.

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
| `image::as_document` | a raster image as a one-page document |
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

## Errors

`extract_file` and `extract_bytes` return `Result<Document, ExtractError>`.
`ExtractError`, `DocxError` and `PptxError` all implement
`std::error::Error`, so they compose with `?`, `Box<dyn Error>` and `anyhow`.

`fluree_doc_markdown::parse`, `fluree_doc_text::parse` and
`fluree_doc_html::parse` are infallible — each format is defined so that every
byte sequence is a valid parse.
