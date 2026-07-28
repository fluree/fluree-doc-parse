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

Measured 2026-07-28 on the full 200 documents.

| # | engine | overall | NID | TEDS | MHS | s/doc |
|---|---|---|---|---|---|---|
| 1 | **fluree-doc-parse** (cascade) | **0.929711** | 0.9440 | 0.9411 | 0.8734 | ~1.5¹ |
| 2 | opendataloader-hybrid | 0.906572 | 0.9337 | 0.9276 | 0.8208 | 0.463 |
| 3 | **fluree-doc-parse** (deterministic) | **0.889638** | 0.9195 | 0.8441 | 0.8131 | **~0.009**² |
| 4 | nutrient | 0.885067 | 0.9250 | 0.7081 | 0.8190 | 0.008 |
| 5 | docling | 0.881679 | 0.8984 | 0.8871 | 0.8240 | 0.762 |
| 6 | opendataloader-hybrid-hydrogen | 0.876816 | 0.9260 | 0.7958 | 0.7685 | 5.068 |
| 7 | pdf-inspector | 0.875348 | 0.9147 | 0.8141 | 0.7879 | 0.006 |
| 8 | marker | 0.860836 | 0.8897 | 0.8076 | 0.7956 | 53.932 |

The deterministic engine — no model, no GPU, no API key — places third, and
the cascade places first by 0.023.

By [tier](../getting-started/tiers.md):

| tier | adds | overall | typical cost/document |
|---|---|---|---|
| 1 | deterministic extraction and layout | 0.889638 | 8 ms, CPU |
| 2 | layout-detector arbitration | 0.896694 | ~0.2 s, CPU |
| 3 | deep reading of pixels-only content | 0.929711 | ~1.5 s across the corpus¹ |

¹ The harness prints 0.010 s/document for the cascade, which is only the Rust
pass reading cached model output. Honestly: 87 of 200 documents escalate and
113 never leave tier 1; the median escalated document costs 1.7 s and the
worst 18.9 s.

² Warm, median of five. The harness spawns one process per document, so this
is substantially process startup rather than parsing — an upper bound on the
engine, not a profile of it. A single timing is not a measurement.

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

Then:

```bash
cargo build --release --manifest-path <repo>/Cargo.toml

# tier 1 — deterministic
FLUREE_DOC_BINARY=<repo>/target/release/fdoc \
  uv run python src/run.py --engine fluree-doc-parse --force

# tier 3 — the full cascade, from the committed caches
FLUREE_DOC_BINARY=<repo>/target/release/fdoc \
FDOC_TITLE_BOXES=<repo>/eval/layout-cache \
FDOC_TIER_RESULTS=<repo>/eval/cascade-cache \
  uv run python src/run.py --engine fluree-doc-parse-cascade --force
```

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
  reference](where-we-differ.md) — that accounts for 0.0019 of tier 1's
  overall today.

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
