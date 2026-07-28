# fluree-doc-parse

Adaptive document parsing: a deterministic Rust engine first, with
model-arbitrated upgrade tiers a document walks through only as far as it
needs. PDF, Markdown, HTML, DOCX, PPTX or a raster image in; Markdown, XHTML,
JSON, plain text or a DoCO JSON-LD graph out.

| tier | adds | overall¹ | typical cost/document |
|---|---|---|---|
| 1 | deterministic extraction + layout | 0.889638 | **8 ms** (CPU) |
| 2 | layout-detector arbitration (headings, table regions) | 0.896694 | ~0.2 s (CPU) |
| 3 | deep reading of pixels-only content and doubted structure | **0.929711** | ~1.5 s² |

¹ 200-document public evaluation corpus, measured 2026-07-28 — the standings
[below](#where-it-stands). Every rung reproduces from the committed
model-output caches with no GPU and no API key.

² Averaged across the corpus: 87 of the 200 documents escalate (median
1.7 s), the other 113 never leave tier 1 at 8 ms. Tiers 1 and 2 are CPU-only,
so the deep reader is the sole reason to think about a GPU or an API key. The
full timing accounting is on [the benchmarks page](docs/benchmarks/README.md).

Two principles produced those numbers:

- **Models arbitrate structure; the page tier owns its page.** Where the
  deterministic pass produced a reading, a model's replaces it only when their
  *shapes* disagree, and two independent readers agreeing against the deeper
  model win. Where the deterministic pass is what failed — no usable glyph
  layer, or a hierarchy resting on nothing — the reading owns the page
  outright, because there is no good text to prefer to it.
- **Escalation is earned, not configured.** Each tier's outputs are the
  sensors that decide whether the next tier runs, per document, per region,
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

Measured 2026-07-28 on
[opendataloader-bench](https://github.com/opendataloader-project/opendataloader-bench):
200 public PDFs with hand-checked ground truth, scored by a harness neither
written nor tuned by this project. NID scores reading order and text, TEDS
table structure, MHS heading structure; `overall` is their per-document mean.
The top 8 of the 17 engines scored:

| # | engine | overall | NID | TEDS | MHS | s/doc |
|---|---|---|---|---|---|---|
| 1 | **fluree-doc-parse** (cascade) | **0.929711** | 0.9440 | 0.9411 | 0.8734 | ~1.5 |
| 2 | opendataloader-hybrid | 0.906572 | 0.9337 | 0.9276 | 0.8208 | 0.463 |
| 3 | **fluree-doc-parse** (deterministic) | **0.889638** | 0.9195 | 0.8441 | 0.8131 | **~0.009** |
| 4 | nutrient | 0.885067 | 0.9250 | 0.7081 | 0.8190 | 0.008 |
| 5 | docling | 0.881679 | 0.8984 | 0.8871 | 0.8240 | 0.762 |
| 6 | opendataloader-hybrid-hydrogen | 0.876816 | 0.9260 | 0.7958 | 0.7685 | 5.068 |
| 7 | pdf-inspector | 0.875348 | 0.9147 | 0.8141 | 0.7879 | 0.006 |
| 8 | marker | 0.860836 | 0.8897 | 0.8076 | 0.7956 | 53.932 |

The deterministic engine — no model, no GPU, no API key — places third on its
own. The timing footnotes (both `s/doc` figures deserve them) and the caveats
that belong with these numbers are on
[the benchmarks page](docs/benchmarks/README.md) — including
[where our output is better than the reference](docs/benchmarks/where-we-differ.md)
and scores lower for it. The per-document accounting, the reproduce recipe,
and the negative results kept on record are in
[the scoreboard](docs/contributing/scoreboard.md).

## One element model, five outputs

PDF, Markdown, HTML, DOCX, PPTX and raster images all converge on one element
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
- `crates/fluree-doc-docx` — DOCX (OOXML) reader
- `crates/fluree-doc-html` — HTML reader
- `crates/fluree-doc-pptx` — PPTX (OOXML) reader
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

No model code is linked and no weights ship here. Model readings enter
through two doors: a deep reader configured over HTTPS, or JSON sidecars
from any layout detector or reader you run yourself. The committed `eval/`
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
