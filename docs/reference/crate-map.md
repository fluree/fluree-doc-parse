# Crate map

```
fluree-doc-cli          the fdoc binary
      │
      ├── fluree-doc-pdf      PDF: extraction, layout, routing, arbitration
      ├── fluree-doc-markdown  Markdown reader
      ├── fluree-doc-html      HTML reader
      ├── fluree-doc-docx      DOCX reader
      └── fluree-doc-pptx      PPTX reader
                │
                └── fluree-doc-model   element model + emitters (source-agnostic)
```

Every reader depends on `fluree-doc-model` and on nothing else of ours. That
is the point of the split: **a Markdown or DOCX consumer never compiles a PDF
engine.**

## The crates

| crate | contains |
|---|---|
| `fluree-doc-model` | `Element`, `Link`, `Target`, `BBox`, `PageSize`, the Markdown/XHTML/DoCO/text emitters, merge denormalization |
| `fluree-doc-markdown` | `parse(&str) -> Vec<Element>` |
| `fluree-doc-html` | `parse(&str) -> Vec<Element>`, via html5ever |
| `fluree-doc-docx` | `parse(&[u8]) -> Result<Vec<Element>, DocxError>` |
| `fluree-doc-pptx` | `parse(&[u8]) -> Result<Vec<Element>, PptxError>`, incl. charts |
| `fluree-doc-pdf` | extraction, the layout pipeline, the router, the arbiter |
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
| html5ever | spec-compliant HTML parsing | MIT OR Apache-2.0 |
| pulldown-cmark | Markdown parsing | MIT |
| quick-xml, zip | OOXML containers | MIT |
| clap | CLI | MIT OR Apache-2.0 |
| serde, serde_json | serialization | MIT OR Apache-2.0 |
| unicode-normalization | NFKC | MIT OR Apache-2.0 |

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
