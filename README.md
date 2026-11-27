# fluree-doc-parse

Adaptive document parsing: a deterministic Rust engine first, with OCR and a
vision-language model a document uses only as far as it needs. PDF, Markdown,
plain text, HTML, DOCX, PPTX, XLSX, a raster image, a meeting transcript or an
email in; Markdown, XHTML, JSON, plain text or a DoCO JSON-LD graph out.

| mode | adds | overall¹ | s/doc² |
|---|---|---|---|
| deterministic | the document's own text, layout, tables and headings | 0.922007 | **0.0067** (CPU) |
| + OCR | the built-in OCR engine, for text set as pixels | 0.947430 | 0.340 (CPU) |
| + VLM | a vision-language model, for the pages and regions OCR cannot settle | **0.951650** | 1.580 |

¹ 200-document public evaluation corpus, measured 2026-11-26: the standings
[below](#where-it-stands). The deterministic and OCR scores reproduce with no
GPU and no API key. The VLM score needs a model provider, and a run is billed
per call.

² One `fdoc convert` process over all 200 documents with `-j 1`, its wall
time divided by 200, on an Apple M4 Max. No process is started per document.
The median of five runs (deterministic) or three (+ OCR) after a warm-up run,
and one run (+ VLM). With `-j 8` the same machine reads 833, 11.0 and
6.4 documents a second. With + VLM, 35 of the 200 documents call the
model; the others read as they do with + OCR. The deterministic engine and
OCR are CPU-only, so the vision-language model is the sole reason to think
about an API key. The full timing accounting is on [the benchmarks
page](docs/benchmarks/README.md).

Two principles produced those numbers:

- **Models arbitrate structure; the page tier owns its page.** Where the
  deterministic pass produced a reading, a model's replaces it only when their
  *shapes* disagree, and two independent readers agreeing against the deeper
  model win. Where the deterministic pass is what failed — no usable glyph
  layer, or a hierarchy resting on nothing — the reading owns the page
  outright, because there is no good text to prefer to it.
- **Escalation is earned, not configured.** Each step's outputs are the
  sensors that decide whether the next one runs, per document, per region,
  per table.

## Quick start

```
cargo build --release
fdoc convert document.pdf                    # markdown (default)
fdoc convert notes.md   -f doco              # Markdown in, DoCO graph out
fdoc convert report.docx -f doco              # DOCX in, same graph out
fdoc convert page.html  -f doco              # HTML in, same graph out
fdoc convert deck.pptx  -f doco              # PPTX in, slides become pages
                                             #   (charts become tables)
fdoc convert book.xlsx  -f doco              # XLSX in, sheets become pages
                                             #   (blocks of cells become tables)
fdoc convert orders.csv -f doco              # CSV or TSV in, one table
fdoc convert call.vtt   -f doco              # WebVTT or SubRip in, one paragraph
                                             #   per speaker turn, with who and when
fdoc convert reply.eml  -f doco              # .eml or .msg in, the thread split
                                             #   into its messages
fdoc convert document.pdf -f xhtml           # XHTML
fdoc convert document.pdf -f json            # DoCO-typed elements + bboxes
fdoc convert document.pdf -f doco            # DoCO JSON-LD graph (sections,
                                             #   containment, table cells, char
                                             #   offsets, page/bbox provenance)
fdoc convert document.pdf -f text            # plain-text projection (the string
                                             #   doco char offsets index into)
fdoc convert ./docs/ --out-dir out -j 8      # parallel batch
fdoc forms   document.pdf                    # AcroForm fields + filled values
fdoc triage  document.pdf                    # what would escalate, and why

fdoc config gemini --credentials key.json    # set a deep reader up, once
fdoc convert document.pdf -f doco            # …and any format escalates
fdoc convert document.pdf --no-escalate      # deterministic for this run
```

Escalation is off until a provider is configured, and with none configured the
binary opens no network connection. Readings can also be supplied as
[JSON sidecars](docs/integration/escalation-tiers.md) from any model you run
yourself — see [`fdoc config`](docs/cli/config.md).

## Where it stands

Measured 2026-11-26 on
[opendataloader-bench](https://github.com/opendataloader-project/opendataloader-bench):
200 public PDFs with hand-checked ground truth, scored by a harness neither
written nor tuned by this project. NID scores reading order and text, TEDS
table structure, MHS heading structure; `overall` is their per-document mean.
The reference writes a link as its words alone, so our scores are taken with
[`--md-links text`](docs/formats/markdown.md#links-as-plain-text), which does the same.

![Overall score against seconds per page for 16 engines on opendataloader-bench: fluree-doc-parse scores 0.922 deterministic at 6.7 ms per page, 0.947 with OCR at 0.34 s and 0.952 with a vision-language model at 1.58 s; markitdown and liteparse, below 0.65, are listed in a note](docs/assets/accuracy-vs-speed.svg)

All 16 engines scored, fluree-doc-parse in each of its three modes:

| # | engine | overall | NID | TEDS | MHS | s/doc³ |
|---|---|---|---|---|---|---|
| 1 | **fluree-doc-parse** (+ VLM) | **0.951650** | 0.9584 | 0.9678 | 0.9217 | 1.580 |
| 2 | **fluree-doc-parse** (+ OCR) | **0.947430** | 0.9582 | 0.9619 | 0.9060 | 0.340 |
| 3 | **fluree-doc-parse** (deterministic) | **0.922007** | 0.9343 | 0.9166 | 0.8866 | **0.0067** |
| 4 | opendataloader-hybrid | 0.906572 | 0.9337 | 0.9276 | 0.8208 | 0.463 |
| 5 | nutrient | 0.885067 | 0.9250 | 0.7081 | 0.8190 | 0.008 |
| 6 | docling | 0.881679 | 0.8984 | 0.8871 | 0.8240 | 0.762 |
| 7 | opendataloader-hybrid-hydrogen | 0.876816 | 0.9260 | 0.7958 | 0.7685 | 5.068 |
| 8 | pdf-inspector | 0.875348 | 0.9147 | 0.8141 | 0.7879 | 0.006 |
| 9 | marker | 0.860836 | 0.8897 | 0.8076 | 0.7956 | 53.932 |
| 10 | opendataloader-hybrid-helium | 0.845058 | 0.8789 | 0.8068 | 0.7549 | 8.620 |
| 11 | unstructured-hires | 0.841377 | 0.9038 | 0.5883 | 0.7486 | 3.008 |
| 12 | edgeparse | 0.836959 | 0.8938 | 0.7174 | 0.7061 | 0.036 |
| 13 | opendataloader | 0.831209 | 0.9023 | 0.4887 | 0.7395 | 0.015 |
| 14 | mineru | 0.831135 | 0.8574 | 0.8730 | 0.7430 | 5.962 |
| 15 | pymupdf4llm | 0.731621 | 0.8851 | 0.4010 | 0.4122 | 0.091 |
| 16 | unstructured | 0.685777 | 0.8818 | 0.0000 | 0.3877 | 0.077 |
| 17 | markitdown | 0.588504 | 0.8437 | 0.2729 | 0.0000 | 0.114 |
| 18 | liteparse | 0.575604 | 0.8660 | 0.0000 | 0.0000 | 1.061 |

³ Ours: one `fdoc convert` process over all 200 documents, its wall time
divided by 200, as in the table at the top. The other engines' times are the
harness's own: one process per document, each process's start-up included.

All three modes place above every other engine. The deterministic engine,
which uses no model, GPU or API key, is 0.015 ahead of the next engine on its
own. The built-in OCR engine, still on CPU with no key, adds 0.025. A
vision-language model, called only for what OCR cannot settle, adds
0.004 more.

Notes on how our `s/doc` figures were timed, and the caveats that apply to
these scores, are on [the benchmarks page](docs/benchmarks/README.md). That
page also lists [where our output differs from the
reference](docs/benchmarks/where-we-differ.md) and is scored lower as a
result. Per-document results, the steps to reproduce them, and the approaches
measured and rejected are in [the scoreboard](docs/contributing/scoreboard.md).

## One element model, five outputs

PDF, Markdown, plain text, HTML, DOCX, PPTX, XLSX, transcripts, email and raster images all converge on one element
model, so every source produces the same DoCO graph. PDF is the geometric
path — structure inferred from glyph and rule positions, with escalation to
model tiers where the inference is weak. The others declare their structure,
so those readers map rather than measure, and carry no geometry: their
elements have no `bbox` rather than a zeroed one.

The `doco` output is insertable into a [Fluree](https://flur.ee) ledger as-is:
the JSON-LD context carries the DoCO/NIF/pattern ontologies, containment edges
are IRI-coerced, and `--doc-iri` stamps every element for retract-on-rerun.
`fdoc dev` exposes pipeline internals (glyphs, lines, blocks, table geometry)
for debugging; its output is not a compatibility surface.

## Documentation

Full documentation is in [`docs/`](docs/README.md) — an mdBook, so
`mdbook serve docs` renders it locally.

- [Output formats](docs/formats/README.md) — what each of the five guarantees
- [CLI reference](docs/cli/README.md) — every command, flag by flag
- [Integration](docs/integration/README.md) — wiring the model tiers, ledger
  ingest, entity overlay
- [Rust library](docs/getting-started/rust-library.md) — the supported API
- [Design](docs/design/README.md) — how the deterministic engine works
- [Benchmarks](docs/benchmarks/README.md) — the standings, the method, the
  caveats

## Layout

- `crates/fluree-doc-model` — element model and emitters, source-agnostic
- `crates/fluree-doc-markdown` — Markdown reader
- `crates/fluree-doc-text` — plain-text reader
- `crates/fluree-doc-docx` — DOCX (OOXML) reader
- `crates/fluree-doc-html` — HTML reader
- `crates/fluree-doc-pptx` — PPTX (OOXML) reader
- `crates/fluree-doc-xlsx` — XLSX (OOXML) reader
- `crates/fluree-doc-ppt` — PowerPoint 97-2003 (`.ppt`) reader
- `crates/fluree-doc-doc` — Word 97-2003 (`.doc`) reader
- `crates/fluree-doc-xls` — Excel 97-2003 (`.xls`) and binary (`.xlsb`) reader
- `crates/fluree-doc-xps` — XPS and OpenXPS reader (pages for the PDF layout)
- `crates/fluree-doc-odf` — OpenDocument reader (`.odt`, `.ods`, `.odp`)
- `crates/fluree-doc-rtf` — RTF reader
- `crates/fluree-doc-csv` — CSV and TSV reader
- `crates/fluree-doc-transcript` — WebVTT and SubRip transcript reader
- `crates/fluree-doc-vision` — reading pixels: the OCR word format shared by the readers of images
- `crates/fluree-doc-image` — bare images (PNG, JPEG, TIFF) read by OCR as one-page documents
- `crates/fluree-doc-email` — email reader (`.eml`, `.msg`)
- `crates/fluree-doc-pdf` — extraction, layout, routing, arbitration
- `crates/fluree-doc-cli` — the `fdoc` binary
- `bench-adapter/` — opendataloader-bench engine adapters (incl. hybrid tiers)
- `docs/` — the documentation book
- `eval/` — test plan, corpus notes, reproduction caches, analyses and evidence

## License and attribution

Copyright 2026 Fluree PBC. Licensed under the Apache License, Version 2.0;
see [LICENSE](LICENSE).

PDF interpretation and rendering use the [hayro](https://github.com/LaurenzV/hayro)
crates (Apache-2.0 OR MIT). HTML parsing uses [html5ever](https://github.com/servo/html5ever)
(MIT OR Apache-2.0) directly — Servo's spec-compliant parser, without a CSS
selector layer the reader does not need. Markdown parsing
uses [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) (MIT),
and the OOXML readers use [quick-xml](https://github.com/tafia/quick-xml) and
[zip](https://github.com/zip-rs/zip2) (both MIT).

No weights ship here. Model readings enter through three doors: a deep
reader configured over HTTPS, JSON sidecars from any layout detector or
reader you run yourself, and the built-in OCR engine. That engine links
[oar-ocr](https://github.com/GreatV/oar-ocr) (Apache-2.0), with the
[ort](https://github.com/pykeio/ort) bindings and
[image](https://github.com/image-rs/image) (both MIT OR Apache-2.0); it can
be left out by building `fdoc` without its `ocr` feature. ONNX Runtime (MIT)
and the PP-OCRv6 models (Apache-2.0) it runs on are neither linked nor
redistributed: they are loaded at run time from an assets directory, checked
against the hashes `fdoc` is built with, which `tools/ocr-assets/build.py` builds from the
publishers' own files, with their licences and a CycloneDX fragment declaring
them for an SBOM. The committed `eval/`
caches — kept so every published score reproduces without a GPU or an API
key — include outputs produced during evaluation with PaddleX and PaddleOCR
models (Apache-2.0), whose code and weights are not redistributed here.

A configured deep reader ([`fdoc config`](docs/cli/config.md)) speaks HTTPS
through [ureq](https://github.com/algesten/ureq) and
[rustls](https://github.com/rustls/rustls) (both MIT OR Apache-2.0), with the
Mozilla CA set from [webpki-roots](https://github.com/rustls/webpki-roots)
(CDLA-Permissive-2.0, a permissive data licence). Service-account assertions
are signed with the pure-Rust [rsa](https://github.com/RustCrypto/RSA) crate
(MIT OR Apache-2.0), so no system crypto library is required. None of this is
reached with no provider configured.

Run `cargo tree` for the full dependency set. Every crate in it is under a
permissive licence — MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0 or
CDLA-Permissive-2.0 — with no copyleft anywhere in the tree. The per-user
config directory is resolved directly rather than through the usual crate,
whose transitive `option-ext` is MPL-2.0 and would be the only exception.
