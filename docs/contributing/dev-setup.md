# Dev setup

```bash
git clone https://github.com/fluree/fluree-doc-parse
cd fluree-doc-parse
cargo build --release
cargo test --workspace
```

A Rust toolchain is the only requirement. No models, no Python, no system PDF
library.

## Layout

See the [crate map](../reference/crate-map.md). The short version:
`fluree-doc-model` holds the element model and emitters, one crate per input
format sits on top of it, and `fluree-doc-pdf` holds everything specific to
PDF.

## Debugging extraction

[`fdoc dev`](../cli/dev.md) exposes every intermediate stage, and it is
usually faster than adding print statements:

```bash
fdoc dev glyphs   report.pdf 0        # raw glyphs, draw order
fdoc dev lines    report.pdf 0        # after line assembly
fdoc dev blocks   report.pdf 0        # after furniture + paragraphs
fdoc dev headings report.pdf          # what was detected, on what evidence
fdoc dev rules    report.pdf 12       # table geometry
fdoc dev tables   report.pdf 12       # the grid derived from it
```

Work down the [pipeline](../design/pipeline.md) in order — a wrong table is
often a wrong line, and a wrong line is often a wrong gap threshold.

## Rendering

```bash
fdoc render            report.pdf ./pages     # every page, 2×
fdoc dev render-routed report.pdf ./crops     # routed regions + manifest
```

Looking at the render answers questions the geometry dump cannot — whether
text really is bold, whether a "rule-less" table has per-cell segments, whether
a region is genuinely a picture.

## Documentation

```bash
cargo doc --no-deps --open           # API docs
mdbook serve docs                    # this book, at localhost:3000
```

`mdbook` is not a build dependency; install it with `cargo install mdbook` if
you want to preview.

## Before you push

```bash
cargo fmt --check
cargo build --release --workspace
cargo test --workspace
./eval/run.sh                        # if you touched extraction or layout
```

The first three are what CI runs.
