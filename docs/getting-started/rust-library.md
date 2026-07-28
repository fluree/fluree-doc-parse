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
use fluree_doc_model::{to_markdown, to_xhtml};
use fluree_doc_pdf::doco::{to_doco, to_text, DocoOptions};

let md    = to_markdown(&analysis.elements);
let xhtml = to_xhtml(&analysis.elements);
let text  = to_text(&analysis.elements);
let graph = to_doco(&analysis.elements, &DocoOptions {
    base_iri: "urn:fluree-doc-parse:report".into(),
    doc_iri: None,
});
```

All four return serialized `String`s. `DocoOptions` has no `Default` on
purpose: `base_iri` is the namespace your minted element IRIs live under, and
guessing it for you would put someone else's documents in your namespace.

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

## Read a non-PDF source

Each reader returns the same `Vec<Element>`:

```rust
let els = fluree_doc_markdown::parse(&src);       // &str
let els = fluree_doc_html::parse(&src);           // &str
let els = fluree_doc_docx::parse(&bytes)?;        // &[u8]
let els = fluree_doc_pptx::parse(&bytes)?;        // &[u8]
```

Those elements have `bbox: None` — see [Measured vs declared
structure](../concepts/geometry-vs-declared.md).

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

`fluree_doc_markdown::parse` and `fluree_doc_html::parse` are infallible —
both formats are defined so that every byte sequence is a valid parse.
