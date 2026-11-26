# Benchmarks

Every accuracy claim here comes from one public corpus, scored by a harness
neither written nor tuned by this project. This page says what is measured,
what the numbers are, how to reproduce them, and — because it matters when
reading any of them — [where the engine's output is better than the reference
it is scored against](where-we-differ.md).

## What is measured

**[opendataloader-bench](https://github.com/opendataloader-project/opendataloader-bench)**:
200 PDFs with hand-checked ground truth, and three metrics.

| metric | what it scores | documents |
|---|---|---|
| **NID** | reading order and text, as normalized edit distance | 200 |
| **TEDS** | table structure, as tree edit distance | 42 |
| **MHS** | heading structure | 107 |

The headline `overall` is the mean of those three per document.

## Where we stand

Measured 2026-11-23 on the full 200 documents. The reference writes a link as
its words alone, so our scores are taken with
[`--md-links text`](../formats/markdown.md#links-as-plain-text), which does
the same.

| # | engine | overall | NID | TEDS | MHS | s/doc |
|---|---|---|---|---|---|---|
| 1 | **fluree-doc-parse** (cascade) | **0.940703** | 0.9512 | 0.9420 | 0.9012 | ~1.5¹ |
| 2 | opendataloader-hybrid | 0.906572 | 0.9337 | 0.9276 | 0.8208 | 0.463 |
| 3 | **fluree-doc-parse** (deterministic) | **0.901319** | 0.9264 | 0.8616 | 0.8402 | **~0.011**² |
| 4 | nutrient | 0.885067 | 0.9250 | 0.7081 | 0.8190 | 0.008 |
| 5 | docling | 0.881679 | 0.8984 | 0.8871 | 0.8240 | 0.762 |
| 6 | opendataloader-hybrid-hydrogen | 0.876816 | 0.9260 | 0.7958 | 0.7685 | 5.068 |
| 7 | pdf-inspector | 0.875348 | 0.9147 | 0.8141 | 0.7879 | 0.006 |
| 8 | marker | 0.860836 | 0.8897 | 0.8076 | 0.7956 | 53.932 |
| 9 | opendataloader-hybrid-helium | 0.845058 | 0.8789 | 0.8068 | 0.7549 | 8.620 |
| 10 | unstructured-hires | 0.841377 | 0.9038 | 0.5883 | 0.7486 | 3.008 |
| 11 | edgeparse | 0.836959 | 0.8938 | 0.7174 | 0.7061 | 0.036 |
| 12 | opendataloader | 0.831209 | 0.9023 | 0.4887 | 0.7395 | 0.015 |
| 13 | mineru | 0.831135 | 0.8574 | 0.8730 | 0.7430 | 5.962 |
| 14 | pymupdf4llm | 0.731621 | 0.8851 | 0.4010 | 0.4122 | 0.091 |
| 15 | unstructured | 0.685777 | 0.8818 | 0.0000 | 0.3877 | 0.077 |
| 16 | markitdown | 0.588504 | 0.8437 | 0.2729 | 0.0000 | 0.114 |
| 17 | liteparse | 0.575604 | 0.8660 | 0.0000 | 0.0000 | 1.061 |

The deterministic engine — no model, no GPU, no API key — places third. The
layout detector, still CPU only, would lift it to second, and the cascade
places first by 0.034.

By [mode](../getting-started/modes.md):

| tier | adds | overall | typical cost/document |
|---|---|---|---|
| 1 | deterministic extraction and layout | 0.901319 | 8 ms, CPU |
| 2 | layout-detector arbitration | 0.908607 | ~0.2 s³ |
| 3 | deep reading of pixels-only content | 0.940703 | ~1.5 s across the corpus¹ |

¹ The harness prints 0.012 s/document for the cascade, which is only the Rust
pass reading cached model output. Honestly: 87 of 200 documents escalate and
113 never leave tier 1; the median escalated document costs 1.7 s and the
worst 18.9 s.

² Warm, median of five, which ranged from 11.2 to 11.8 ms. The harness spawns
one process per document, so this is substantially process startup rather
than parsing — an upper bound on the engine, not a profile of it. In one
process, `fdoc convert <dir> -j 1` reads the same 200 documents at 7.7 ms
each: the 8 ms in the tier table. A single timing is not a measurement.

³ PP-DocLayoutV3 on a GPU host, 178 ms a page. It runs on CPU too, at
0.82 s a page on an Apple M4 Max, whose GPU it cannot use. Boxes made afresh
for all 200 pages score tier 2 the same as the committed layout cache,
0.908607.

## Reproducing it

The model-tier scores reproduce from committed caches, so every rung runs
without a GPU and without an API key.

```bash
git clone https://github.com/opendataloader-project/opendataloader-bench
cd opendataloader-bench && uv sync
cp <repo>/bench-adapter/pdf_parser_fluree.py src/
```

Register both engines in `src/engine_registry.py`, under `ENGINES` and
`_ENGINE_MODULES`:

```python
ENGINES         = { ..., "fluree-doc-parse": "0.1.0",
                         "fluree-doc-parse-cascade": "0.1.0" }
_ENGINE_MODULES = { ..., "fluree-doc-parse": "pdf_parser_fluree",
                         "fluree-doc-parse-cascade": "pdf_parser_fluree" }
```

The adapter runs `fdoc convert <pdf> --format md --md-links text
--no-escalate` on each document, so links are written as their words, as the
reference writes them, and no model is called: tiers 2 and 3 read the
committed caches named below.
Set `FLUREE_DOC_MD_LINKS=inline` to score the default Markdown, links and
all.

Then:

```bash
cargo build --release --manifest-path <repo>/Cargo.toml

# tier 1 — deterministic
FLUREE_DOC_BINARY=<repo>/target/release/fdoc \
  uv run python src/run.py --engine fluree-doc-parse --force

# tier 2 — the layout detector, from the committed cache
FLUREE_DOC_BINARY=<repo>/target/release/fdoc \
FDOC_TITLE_BOXES=<repo>/eval/layout-cache \
  uv run python src/run.py --engine fluree-doc-parse-cascade --force

# tier 3 — the full cascade, from the committed caches: model readings are
# mode vlm, which builds on ocr and so needs the OCR assets
FLUREE_DOC_BINARY=<repo>/target/release/fdoc \
FDOC_TITLE_BOXES=<repo>/eval/layout-cache \
FDOC_TIER_RESULTS=<repo>/eval/cascade-cache \
FDOC_OCR_MODELS=<assets>/ocr \
  uv run python src/run.py --engine fluree-doc-parse-cascade --force
```

Tiers 2 and 3 share an engine name and so an output directory: run them one
after the other, and keep tier 2's `evaluation.json` before running tier 3.

`--force` re-parses; without it the harness re-scores the Markdown already on
disk. Per-document scores land in `prediction/<engine>/evaluation.json` under
`documents[]`, and the headline is `metrics.score.overall_mean`.

That the caches are committed is a deliberate property rather than a
convenience: a result nobody else can reproduce is not a result. It also gives
the [sidecar formats](../integration/sidecar-formats.md) a worked reference,
since those caches are real files in exactly the documented shape.

## Caveats that belong with the numbers

State these whenever the scores are quoted.

- **TEDS averages over 42 of 200 documents**, so a single document moves it by
  about 2.4%. MHS covers 107.
- **The corpus is 199/200 Latin script.** There is CJK evidence, but not
  against this ground truth.
- **There is no scanned or image-only PDF in the corpus** — which is the
  router's single most important signal, and therefore the least evidenced
  part of the tier model. Also absent: broken CID fonts, Korean, filled forms,
  multi-column newspapers, and financial statements with nested tables.
- **MHS does not measure heading depth.** Its evaluator flattens the tree, so
  every ground-truth heading is a top-level one.
- **It is one project's curation.** "Every customer throws different documents
  at us" is the standing risk; measure per document class, not in aggregate.
- **Some of the deficit is the engine being right.** Where our reading carries
  something the ground truth does not, the metric scores the difference as
  error. See [Where our output differs from the
  reference](where-we-differ.md). The largest such difference, links, is
  left out of these scores by `--md-links text`; the default Markdown scores
  0.0020 lower on tier 1.

## Two rungs that were retired, with their numbers

A separate table-structure model was measured in both roles it could hold. As
the reading source it scores 0.892130, *below* the layout boxes alone; as a
corroboration veto over tier 3 it scores 0.927005, *below* tier 3 without it.
It earned its place under an earlier, coarser table detector — once the
deterministic grid improved, agreeing with that grid stopped being evidence.
The arbiter still accepts an independent second opinion; nothing is wired
into it.

Negative results are kept on the record with their numbers, including ideas
that reasoning said should work. That record lives in
[the scoreboard](../contributing/scoreboard.md).
