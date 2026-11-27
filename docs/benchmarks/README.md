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

Measured 2026-11-26 on the full 200 documents. The reference writes a link as
its words alone, so our scores are taken with
[`--md-links text`](../formats/markdown.md#links-as-plain-text), which does
the same.

| # | engine | overall | NID | TEDS | MHS | s/doc |
|---|---|---|---|---|---|---|
| 1 | **fluree-doc-parse** (VLM) | **0.951650** | 0.9584 | 0.9678 | 0.9217 | 1.580¹ |
| 2 | **fluree-doc-parse** (OCR) | **0.947430** | 0.9582 | 0.9619 | 0.9060 | 0.340² |
| 3 | **fluree-doc-parse** (deterministic) | **0.922007** | 0.9343 | 0.9166 | 0.8866 | **0.0067**² |
| 4 | opendataloader-hybrid | 0.906572 | 0.9337 | 0.9276 | 0.8208 | 0.463³ |
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

All three configurations place above every other engine. The deterministic
one needs no model, no GPU and no API key, and is ahead of every other engine
on its own. Reading the text set as pixels with the built-in OCR engine, still
on CPU and with no key, adds 0.025. A vision-language model, called only for
what OCR cannot settle, adds 0.004 more.

By configuration:

| configuration | adds | overall | s/doc, one process² | documents/s, 8 workers² |
|---|---|---|---|---|
| deterministic (`--no-escalate`) | the text layer, its geometry and its drawing | 0.922007 | 0.0067 | 833 |
| OCR (`--ocr oar-ocr --models <dir> --no-escalate`) | the [built-in OCR engine](../cli/ocr.md), on CPU, for text set as pixels | 0.947430 | 0.340 | 11.0 |
| VLM (OCR, with a [model provider](../cli/config.md#setting-up-google-vertex-ai) configured) | a vision-language model for the pages and regions OCR cannot settle | 0.951650 | 1.580¹ | 6.4 |

Boxes from a layout detector, supplied as a sidecar, add nothing measurable
on this corpus in any of the three configurations.

¹ The model is Gemini 3 Flash (`gemini-3-flash-preview`) on Google Vertex AI.
35 of the 200 documents call it, with 38 crops between them. Each of those
documents takes about 7 s longer than in the OCR configuration; the other 165
read as they do there. A model's reading varies from run to run: two runs of
this configuration scored 0.951650 and 0.952186, and the lower is quoted.

² One `fdoc convert <dir> -j 1` process over the 200 documents, its wall time
divided by 200, on an Apple M4 Max. No process is started per document. The
median of five runs (deterministic) or three (OCR) after a warm-up run, and
one run (VLM, which is billed per crop). The last column is the same run with
`-j 8`.

³ The other engines' times are the harness's own: one process per document,
each process's start-up included.

## Reproducing it

The deterministic and OCR scores need no GPU and no API key. The VLM score
needs a model provider configured, and a run is billed per crop.

```bash
git clone https://github.com/opendataloader-project/opendataloader-bench
cd opendataloader-bench && uv sync
cp <repo>/bench-adapter/pdf_parser_fluree.py src/
```

Register the engine in `src/engine_registry.py`, under `ENGINES` and
`_ENGINE_MODULES`:

```python
ENGINES         = { ..., "fluree-doc-parse": "0.1.0" }
_ENGINE_MODULES = { ..., "fluree-doc-parse": "pdf_parser_fluree" }
```

The adapter runs `fdoc convert <pdf> --format md --md-links text
--no-escalate` on each document, so links are written as their words, as the
reference writes them, and no model is called. Set
`FLUREE_DOC_MD_LINKS=inline` to score the default Markdown, links and all.
The adapter takes out the notes `fdoc` leaves in HTML comments
(`<!-- fluree-doc-parse: … -->`, such as raster regions no reader
transcribed): they report on the conversion, are no part of the document's
text, and a rendered view does not show them. Set `FLUREE_DOC_KEEP_NOTES=1`
to score them as written.

Then, for the deterministic and OCR scores:

```bash
cargo build --release --manifest-path <repo>/Cargo.toml

# deterministic
FLUREE_DOC_BINARY=<repo>/target/release/fdoc \
  uv run python src/run.py --engine fluree-doc-parse --force

# OCR, with the OCR engine's assets directory
FLUREE_DOC_BINARY=<repo>/target/release/fdoc \
FDOC_OCR=oar-ocr FDOC_OCR_MODELS=<assets> \
  uv run python src/run.py --engine fluree-doc-parse --force
```

The [assets directory](../cli/ocr.md#the-assets-directory) holds the OCR
models and their runtime.

`--force` re-parses; without it the harness re-scores the Markdown already on
disk. Per-document scores land in `prediction/<engine>/evaluation.json` under
`documents[]`, and the headline is `metrics.score.overall_mean`.

The adapter never calls a model, so the VLM score is taken from one
`fdoc convert` run over the corpus, scored by the harness's evaluator. With a
provider configured:

```bash
<repo>/target/release/fdoc convert pdfs --format md --md-links text \
  --ocr oar-ocr --models <assets> \
  --out-dir prediction/fluree-doc-parse-vlm/markdown
uv run python src/evaluator.py --engine fluree-doc-parse-vlm
```

The times in the tables are the same `fdoc convert pdfs … --out-dir <dir>`
run with `-j 1` and with `-j 8`, its wall time divided by 200.

## Caveats that belong with the numbers

State these whenever the scores are quoted.

- **TEDS averages over 42 of 200 documents**, so a single document moves it by
  about 2.4%. MHS covers 107.
- **The corpus is 199/200 Latin script.** There is CJK evidence, but not
  against this ground truth.
- **There is no scanned or image-only PDF in the corpus** — which is the
  router's single most important signal, and therefore the least evidenced
  part of what the OCR and VLM configurations decide. A generated set of our
  own covers that and the OCR tier: see [Scanned PDFs](scanned.md). Also
  absent: broken CID fonts, Korean, filled forms, multi-column newspapers,
  and financial statements with nested tables.
- **MHS does not measure heading depth.** Its evaluator flattens the tree, so
  every ground-truth heading is a top-level one.
- **It is one project's curation.** "Every customer throws different documents
  at us" is the standing risk; measure per document class, not in aggregate.
- **Some of the deficit is the engine being right.** Where our reading carries
  something the ground truth does not, the metric scores the difference as
  error. See [Where our output differs from the
  reference](where-we-differ.md). The largest such difference, links, is
  left out of these scores by `--md-links text`; the default Markdown scores
  0.0019 lower in the deterministic configuration.

## Two rungs that were retired, with their numbers

A separate table-structure model was measured in both roles it could hold. As
the reading source it scored 0.892130, *below* the layout boxes alone; as a
corroboration veto over the model's readings it scored 0.927005, *below* those
readings without it.
It earned its place under an earlier, coarser table detector — once the
deterministic grid improved, agreeing with that grid stopped being evidence.
The arbiter still accepts an independent second opinion; nothing is wired
into it.

Negative results are kept on the record with their numbers, including ideas
that reasoning said should work. That record lives in
[the scoreboard](../contributing/scoreboard.md).
