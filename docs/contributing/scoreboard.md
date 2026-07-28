# Scoreboard, and where the next points are

Start here if you are picking up accuracy work. It says where the engine
stands, how to reproduce that in about ten minutes, and which documents carry
the deficit. [`eval/TEST_PLAN.md`](https://github.com/fluree/fluree-doc-parse/blob/main/eval/TEST_PLAN.md)
remains the full record; this is the working face of it.

Measured 2026-07-28 on opendataloader-bench, 200 documents.

## Where we stand

| # | engine | overall | NID | TEDS | MHS | s/doc |
|---|---|---|---|---|---|---|
| 1 | **fluree-doc-parse-cascade** | **0.929711** | 0.9440 | 0.9411 | 0.8734 | ~1.5¹ |
| 2 | opendataloader-hybrid | 0.906572 | 0.9337 | 0.9276 | 0.8208 | 0.463 |
| 3 | **fluree-doc-parse** (deterministic) | **0.889638** | 0.9195 | 0.8441 | 0.8131 | **~0.009**² |
| 4 | nutrient | 0.885067 | 0.9250 | 0.7081 | 0.8190 | 0.008 |
| 5 | docling | 0.881679 | 0.8984 | 0.8871 | 0.8240 | 0.762 |
| 6 | opendataloader-hybrid-hydrogen | 0.876816 | 0.9260 | 0.7958 | 0.7685 | 5.068 |
| 7 | pdf-inspector | 0.875348 | 0.9147 | 0.8141 | 0.7879 | 0.006 |
| 8 | marker | 0.860836 | 0.8897 | 0.8076 | 0.7956 | 53.932 |

² Warm, median of five. **The harness spawns 200 separate `fdoc` processes**,
so this number is substantially process startup and page cache, not parsing —
treat it as an upper bound on the engine and never as a profile of it. A cold
run costs ~10.7 ms/doc against 8.0–8.9 ms warm across two independent
measurements. A single timing is not a measurement: warm up, then take the
median of five. This directory's own history contains a latency "regression"
that was only ever a first run.

¹ The benchmark reports 0.010 s/doc for the cascade, which is only the Rust
pass reading cached model output. Honestly: 87 of 200 documents escalate,
113 never leave the Rust pass at 8 ms, the median escalated document costs
1.7 s and the worst 18.9 s. Quote ~1.5 s/document, or 51 s for the corpus at
the 6-way concurrency the runner uses.

**The deterministic engine is 0.0046 ahead of nutrient**, after weak
typography inside deterministic figure regions was demoted, split heading
fragments were coalesced, and false numbered sentences, page labels, formulas,
and chart metrics were rejected. Display labels governing lettered content
sequences are also recovered without relying on font metadata. It remains
roughly 90× faster than docling.

**0.0019 of the deterministic deficit is deliberate.** PDF link annotations
are read and emitted, and the ground truth — transcribed from the visible
page — carries no link markup anywhere in the corpus, so every address
recovered scores as an insertion error. 21 documents change, all downward,
summing to 0.385. The full accounting, with the readings side by side, is in
[Where our output differs from the
reference](../benchmarks/where-we-differ.md). Do not "fix" this; it is the
engine being right.

The aggregate gain still has four score-noise regressions against the preceding
commit: deterministic 015 (−0.0002) and 183 (−0.0009), and cascade 015
(−0.0002) and 183 (−0.0005). The former material inversion on 199 is resolved:
deterministic improves +0.0266 and cascade MHS now rises from 0.322 to 0.499.
Layout arbitration distinguishes explicit `Figure`/`Table`/`Chart` captions
from descriptive float titles instead of demoting both.

Survey-chart readings retain every percentage, category and legend item, but
promote an opening label followed by `N responses` into the chart's heading.
This changes only 148: cascade overall rises from 0.606 to 0.886 and MHS from
0.252 to 0.810.

## Reproduce it in ten minutes

```bash
git clone https://github.com/opendataloader-project/opendataloader-bench
cd opendataloader-bench && uv sync
cp <repo>/bench-adapter/pdf_parser_fluree.py src/
```

Add to `src/engine_registry.py`, in both `ENGINES` and `_ENGINE_MODULES`:

```python
ENGINES          = { ..., "fluree-doc-parse": "0.1.0", "fluree-doc-parse-cascade": "0.1.0" }
_ENGINE_MODULES  = { ..., "fluree-doc-parse": "pdf_parser_fluree",
                          "fluree-doc-parse-cascade": "pdf_parser_fluree" }
```

Then, from the bench directory:

```bash
cargo build --release --manifest-path <repo>/Cargo.toml

# deterministic — this is the one to work on
FLUREE_DOC_BINARY=<repo>/target/release/fdoc \
  uv run python src/run.py --engine fluree-doc-parse --force

# the full cascade, from committed caches, no GPU and no API key
FLUREE_DOC_BINARY=<repo>/target/release/fdoc \
FDOC_TITLE_BOXES=<repo>/eval/layout-cache \
FDOC_TIER_RESULTS=<repo>/eval/cascade-cache \
  uv run python src/run.py --engine fluree-doc-parse-cascade --force
```

**`--force` is not optional.** Without it `run.py` re-scores the markdown
already on disk instead of re-parsing, so a code change appears to do
nothing. A whole session's worth of numbers was once wrong this way.

Per-document scores land in
`prediction/<engine>/evaluation.json` under `documents[]`; the headline is
`metrics.score.overall_mean`.

## The next points are in headings

The deterministic engine beats nutrient on tables by a distance (TEDS 0.8441
vs 0.7081) and narrowly loses on headings (MHS 0.8131 vs 0.8190). Summed per
document against nutrient: TEDS **+5.71**, NID −0.63, MHS **−0.47**. Heading
work remains the largest quality opportunity, but no longer blocks the
deterministic lead.

Twenty documents carry it. If their MHS matched the better of nutrient and
docling on each:

| | overall | places |
|---|---|---|
| today | 0.889638 | 3rd |
| top-20 closed **halfway** | ~0.900 | 3rd, clear of nutrient |
| top-20 closed fully | ~0.907 | 2nd, just above hybrid |
| every document's MHS at best-of-three | ~0.912 | 2nd |

Closing the list would now edge hybrid. The list, worst first:

```
163  058  185  032  199  019  086  184  188  079
035  190  178  146  187  036  037  147  103  153
```

**Eleven of those twenty still over-promote.** We emit headings that are not
headings:

| doc | ours | gold | matched |
|---|---|---|---|
| 199 | 9 | 3 | 3 |
| 185 | 5 | 3 | 2 |

069's two display-word headings are now both recovered through their relation
to the lettered content below them. 032 still emits one of two. 163 and 058
emit the right number of the wrong things; 188 gets two of three right.

`heading::doubt` already measures exactly this — headings as a fraction of
elements — and fires on six documents at a 0.4 threshold. It is a good enough
signal to *route* on (see below), but blanket weak-heading demotion on those
six documents was not safe: it improved 103 and 150 while regressing 163, 180,
181, and 184. Deterministic figure containment supplied the missing
corroboration. Weak `font-size`/`bold` candidates now yield inside a figure;
outline, title, and numbering evidence survive.

Three documents (019, 062, 199) split one gold heading across two of ours —
a two-line heading emitted as two headings. A guarded same-style, aligned,
lowercase-continuation merge fixed 199 without changing any other document;
019 and 062 need different evidence.

## Dead ends — please don't re-run these

Each was implemented, measured, and reverted. Numbers are on the record so
nobody pays for them twice.

- **Column-boundary word truncation.** Real defect: `column::partition`
  assigns per glyph by centre x, so a word straddling a boundary tears in
  half. Three fixes (word-atomic, touching-only, alphanumeric-runs-only) all
  net-negative: −0.0017, −0.0017, −0.0013. The last fixed both target
  documents and still lost 18 documents to 3. `partition` sits upstream of
  lines, blocks and headings, so its blast radius exceeds the defect. A safe
  fix belongs in column *detection*, not in splitting.
- **Acting on `column::doubt`.** Detecting columns a page-global projection
  cannot see is easy and splitting there is wrong: ten documents change, six
  worse, none better, −0.0040. A band short enough to hide a gutter is short
  enough for a chart's axis tick labels to look like one. See the function's
  doc comment.
- **The heading "inversion" signal.** A label-above-statement pattern that
  reasoning said should predict bad hierarchies. Measured 0.022 against 0.028
  — no separation at all.
- **Escalating on column doubt rather than heading doubt.** Flags 22
  documents instead of 6, gains less in total, and makes five worse. Band
  coverage, missed-gutter count and line-fusion counts were all tested as
  discriminators between the documents escalation helps and hurts; all three
  overlap completely.
- **A separate table-structure model, in either role.** As the reading source
  it scores 0.892130, below the layout boxes alone. As a corroboration veto
  over the deep reader it scores 0.927005, below not having it. It earned its
  keep against a coarser table detector; agreeing with a grid stopped being
  evidence once the grid got better. The arbiter's second-opinion slot is
  still generic, and an alignment-derived shape is the obvious thing to try in
  it — but as a new question, not as a restoration.
- **Per-face ink-weight inference in mixed-metadata documents.** Letting an
  undeclared face infer weight even when another face declares it gained
  +0.001069 deterministically, almost entirely on 069. It failed the dual
  gate: cached-cascade MHS fell from 0.8614 to 0.8582 because the new base
  heading changed arbitration. The experiment was reverted.
- **Two-dimensional heading-fragment joining.** Joining compatible uppercase
  blocks by baseline and spatial adjacency repaired 163 but no other document:
  +0.000096 deterministically, no cascade movement. The geometric decision
  surface was too large for one document's gain, so it was reverted.
- **Letting numbering evidence yield inside figures.** Numbering often labels
  chart fragments, but figure containment also covers genuine numbered
  headings in 037 and 039. Treating it like weak typography dropped
  deterministic MHS from 0.8055 to 0.7915 and was reverted.
- **Splitting every multi-line block after a numbered first line.** This
  recovered 4.3.1 in 188, but damaged 039, 058, 156, and 169; deterministic
  overall fell from 0.889103 to 0.887786. The independently measurable
  display-before-lettered-item half of the experiment was retained.
- **Treating every float-title box as neutral.** This repaired 199 but exposed
  explicit captions as headings, dropping cascade overall to 0.928974 and MHS
  to 0.8641; 058 (−0.2495) and 178 (−0.1258) carried the loss. The semantic
  split was retained with explicit `Figure`/`Table`/`Chart` captions demoted.
- **Treating every layout furniture box as a heading veto.** This improved 148
  again, but `footer`/`header` labels also overlap genuine headings. Cascade
  overall fell from 0.932297 to 0.928067 and MHS from 0.8775 to 0.8636, with
  major regressions on 107 and 156, so the veto was reverted.

## Rules

**Pass `--force`, and re-measure *both* engines on every change.** A change to
the deterministic pass can move the cascade in the other direction: improving
the table detector's row banding gained 0.00124 deterministically and cost
0.00061 on the cascade, because the arbiter's shape rule was tuned against a
coarser grid and began keeping our table over a better reading. Neither
number alone would have shown that.

**The aggregate dual gate is necessary but not sufficient.** Compare
per-document deltas across both engines: a large deterministic gain can hide
an equal cascade loss when the improved base changes the arbiter's comparison.

**A change that moves a number updates it in the same commit, with a reason.**

**Check the cache still matches the crop set.** The splice reads a page-tier
entry unconditionally, so a cache entry the pipeline no longer generates is
still used — silently, and it inflates the score. That has now happened twice.
The check is two commands and a diff:

```bash
FDOC_TITLE_BOXES=eval/layout-cache \
  fdoc dev render-routed <bench>/pdfs /tmp/crops
diff <(ls /tmp/crops/*.png | xargs -n1 basename | sed 's/\.png$//' | sort) \
     <(ls eval/cascade-cache/*.json | xargs -n1 basename | sed 's/\.json$//' \
       | grep -v '^_' | sort)
```

Empty output means every reading in the cache is one the binary asks for, and
every crop it asks for has a reading. A trigger change is what breaks this:
when a document stops escalating, its old reading stays behind and keeps
scoring.

**A cache re-read is worth ±0.13 per document.** Re-running the deep reader
over an unchanged crop set with an unchanged prompt moved 45 documents and
left the aggregate within 0.0003 — but individual documents swung by up to
0.13 in both directions. Never attribute a single document's movement to a
code change without holding the cache fixed, and never read a per-document
delta across a cache refresh as a regression.

**Negative results stay on the record**, with their numbers, here or in
TEST_PLAN. The list above is worth more than most of the code.

**Read the caveats** in [evaluation.md](evaluation.md) before quoting any of
this: TEDS covers only 42 of 200 documents and MHS 107, the corpus is 199/200
Latin script, and there is no scanned PDF in it at all.
