# Evaluation Plan

The single place where we record **what we test, what we expect, and what we currently get.**
Every accuracy or latency claim made about this engine — in the README or anywhere else —
should trace to a check here.

Run everything: `./eval/run.sh`

> **Current standing (2026-07-28).** Deterministic **0.889638**, full cascade
> **0.929711** — first of 17 on the public evaluation corpus. The rung-by-rung table,
> the reproduce recipe, the twenty documents carrying the remaining deficit,
> and the running list of measured-and-reverted ideas now live in
> [`docs/contributing/scoreboard.md`](../docs/contributing/scoreboard.md).
> Numbers below this line predate that measurement unless dated otherwise.

---

## 1. How this is organised

| tier | question | gate |
|---|---|---|
| **T0 Foundation invariants** | does the parse layer hold up? | hard fail — blocks everything |
| **T1 Text fidelity** | is the text correct and normalized? | hard fail |
| **T2 Layout quality** | are paragraphs/headings/tables/order right? | scored, thresholded |
| **T3 Routing** | do we send the right pages to the VLM? | scored |
| **T4 Render** | are pages correct and fast? | hard fail on panic/blank |
| **T5 Output contract** | do Rust and VLM emit identical shapes? | hard fail |

**Baseline discipline:** every number in §4 was measured on Apple M4 Max, 2026-07-25. When a
number changes, update it here in the same commit and say why. A silently drifting baseline is
worse than no baseline.

---

## 2. Corpus

### 2.1 In `eval/corpus/`

| file | pages | why |
|---|---|---|
| `ti_tps5430.pdf`¹ | 41 | datasheet; **mechanical package drawings** (GD&T), dense tables, rotated axis labels |
| `ti_ne555.pdf`¹ | 33 | datasheet; many rotated chart labels, older typesetting |
| `ti_lm358.pdf`¹ | 65 | datasheet; large, many packages |
| `jp_stat.pdf` | 1 | Japanese CID fonts, **faux-bold overprint** |
| `cn_gov.pdf` | ~40 | Simplified Chinese, 11.5k CJK chars, TOC with dot leaders |
| `cn_arxiv.pdf`¹ | ~43 | mixed CJK/Latin, heavy math, two-column academic |

¹ Not redistributable (TI's terms; arXiv), so not in the tree —
`./eval/fetch-corpus.sh` downloads them from their publishers and checks
each against the sha256 of the copy the expectations were measured on. The
two government documents are official texts excluded from copyright and
ship in the tree.

### 2.2 External (not redistributed — fetch separately)

| corpus | how | why |
|---|---|---|
| opendataloader-bench | `git clone github.com/opendataloader-project/opendataloader-bench` | 200 scored PDFs with ground truth; the only source of NID/TEDS/MHS numbers |
| (internal licensed report) | not distributable | primary regression doc for rule-less tables + watermark leakage |

### 2.3 Corpus gaps — **acquire before Phase 3**

These are not "nice to have"; the router cannot be validated without the first one.

- [ ] **Scanned / image-only PDF** — zero in the corpus. Blocks T3.1, the most important routing signal.
- [ ] **Broken CID fonts** (text extracts as garbage) — blocks T3.2.
- [ ] **Korean** — 1 doc across everything we have.
- [ ] **Form** (AcroForm fields, checkboxes).
- [ ] **Multi-column newspaper** — hardest reading-order case.
- [ ] **Financial statement** with nested/spanning tables.
- [ ] **Vector-outline text** (text drawn as paths, no glyphs) — blocks T3.3.

---

## 3. Tests and expectations

### T0 — Foundation invariants

Run on all of `eval/corpus/`. **Any failure blocks the build.**

| id | check | expected |
|---|---|---|
| T0.1 | every PDF parses | 6/6, 0 errors |
| T0.2 | no panics on any page | 0 |
| T0.3 | glyph count > 0 on text-bearing pages | 0 zero-glyph docs in this corpus |
| T0.4 | every glyph has page + transform | 100% |
| T0.5 | parse throughput | ≥ 300 pages/s (measured 465) |

Rationale for T0.2: hayro is pre-1.0. A panic on real input is a release blocker, not a bug report.

### T1 — Text fidelity

| id | check | expected | currently |
|---|---|---|---|
| T1.1 | Unicode resolution rate, Latin corpus | ≥ 99.9% | 99.970% (bench 200) |
| T1.2 | Unicode resolution rate, CJK corpus | ≥ 99.5% | 99.732% |
| T1.3 | replacement chars, `jp_stat` / `cn_gov` | **0** | 0 / 0 |
| T1.4 | **NFKC applied** — no ligature codepoints U+FB00-06 in output | **0** | ✅ 0 (glyph + line text) |
| T1.5 | faux-bold dedup — no doubled CJK runs (`検検討討`) | 0 | ✅ 3518 removed on corpus |
| T1.6 | offset→bbox round-trip: every char offset resolves to a rect or a known-empty (whitespace) | 100% | ✅ prototyped |
| T1.7 | raw↔normalized offset map is bijective over non-normalized chars | 100% | ✅ unit-tested |

T1.4/T1.5 were the two known-required cleanup passes; both now implemented and passing.
Layout-pass defects found since are tracked in §7.

### T2 — Layout quality (scored against opendataloader-bench)

Metric definitions from the bench evaluator: NID = reading order, TEDS = table structure,
MHS = heading structure. TEDS averages over only **42** of 200 docs; MHS over **107**.

| id | metric | floor | target | reference points |
|---|---|---|---|---|
| T2.1 | overall | 0.80 | **0.90** | pdf-inspector 0.875 · docling 0.882 · oracle 0.918 |
| T2.2 | NID | 0.89 | **0.92** | pdf-inspector 0.915 · oracle 0.938 |
| T2.3 | TEDS | 0.70 | **0.85** | pdf-inspector 0.814 · docling 0.887 · oracle 0.904 |
| T2.4 | MHS | 0.75 | **0.85** | pdf-inspector 0.788 · docling 0.824 · oracle 0.881 |
| T2.5 | missing predictions | 0 | 0 | — |

**Floor** = do not merge a regression below this. **Target** = the goal that justifies the project.

Targets are set above pdf-inspector because we expect to gain from: the PDF outline/bookmark tree
(verified unused by every engine tested), rotation-bucketed reading order, and combining row
segmentation with column inference — measured as complementary failure modes on the internal
regression report, where one engine recovered the rows and another the columns, neither both.

#### T2.6 — rule-less table regression (internal corpus document)

Tracked against a licensed research report that cannot ship with this
repository, so the finding is recorded here rather than by reference: a 4×6 table
whose grid is drawn as one short rule per cell edge defeats every engine
tested unless the per-cell segments are clustered by axis position — their
endpoints are the column boundaries. This engine recovers it exactly.

#### T2.7 — header/footer leakage (internal corpus document)

Furniture-stripping checks against the same licensed document, likewise
recorded here rather than by reference. All checks pass — including the long-pending "no furniture
inside table cells" case, closed by scrubbing known furniture texts from cell
and prose emission (exact for constant footers, digit-insensitive for page
numbers, prefix-matched for watermarks that wrap differently across table
regions). An independent VLM labeling of the same page agrees with the
deterministic furniture decisions line-for-line.


Totals: 22 pages, 738 lines → 679 body lines, 59 removed.

Stripping must happen **before** table assembly — a leaked `|` breaks column counts.

### T3 status (2026-07-25)

Router implemented in `route.rs`: page tier (Scanned / NearBlank / BrokenText) and region
tier (raster regions that read as text/table structure via a pixel probe, gated on a glyph
void — regions the text layer already covers never route). On the bench corpus: 1 page-tier
route (doc 141), 44 region-tier routes (22.5%), 0 false page-tier routes. Two calibration
facts recorded for whoever tunes this next: image *coverage* alone misroutes decorative
full-page backgrounds (docs 198-200 score 0.87-0.97 deterministically); and pixel statistics
cannot predict whether the bench GT transcribes an image's content (texty-row fractions of
helped and unhelped documents overlap completely) — that is a property of the GT, not the
PDF. The region tier is therefore recall-oriented and relies on splice asymmetry: our text
is never replaced, so a routed region can only add.

### T3 — Routing

| id | signal | expected precision | corpus needed |
|---|---|---|---|
| T3.1 | zero glyphs / image-only → VLM | 1.00 | ❌ **missing scanned docs** |
| T3.2 | low Unicode resolution → VLM | 1.00 | ❌ **missing broken-CID docs** |
| T3.3 | vector-outline text → VLM | 1.00 | ❌ **missing** |
| T3.4 | table detector disagreement → VLM | — | bench corpus |
| T3.5 | clean page stays local | ≥ 0.95 | ✅ current corpus |
| T3.6 | **CAD drawings stay local** — routing them to the VLM loses the dimension text | 1.00 | ✅ `eval/corpus/ti_*.pdf` |

**Reference for T3.4:** using three engines as a proxy for multi-strategy detection, on the 42
table docs — agreement (25 docs, 59.5%) → best-of-3 TEDS 0.987, 0 failures; disagreement
(17 docs, 40.5%) → 0.781, 1 failure. Target: match or beat that separation with detectors
*inside one library*.

**Routing rate target:** ≤ 15% of pages to the VLM on a text-layer corpus. Track cost per
document as a first-class metric — the whole point is that most pages never leave Lambda.

### T4 — Render

| id | check | expected | currently |
|---|---|---|---|
| T4.1 | no panics | 0 | 0 across 423 pages |
| T4.2 | no blank pages | 0 | 0 |
| T4.3 | throughput @2× (~144 DPI) | ≥ 60 pages/s | 105-141 pages/s |
| T4.4 | CAD fidelity — GD&T symbols, dash-dot, hatching | visual pass | ✅ `eval/evidence/hayro_cad.png` |
| T4.5 | CJK fidelity — kanji/kana/full-width punctuation | visual pass | ✅ `eval/evidence/hayro_jp.png` |
| T4.6 | overlay alignment: rendered rect matches glyph bbox × scale | ±1 px | ✅ verified manually, **needs automating** |

#### T2.4a — what MHS actually measures

**Despite the name, MHS does not measure hierarchy.** From the evaluator's own docstring
(`evaluator_heading_level.py:3-5`): it *"builds a flat section tree that treats all heading
levels as equivalent"*. The ground truth agrees — **all 193 headings across 107 documents are
`#`**; not one file uses `##`.

So MHS scores *which blocks are headings* and *how content partitions between them*, not depth.
Two consequences:

* Level assignment is **unmeasurable here**. Unifying it (style-rank normalization) moved
  internal level-correctness 38.7% → 57.6% and MHS 0.535 → 0.530, i.e. nothing. It was kept
  because DoCO `doco:SectionTitle` carries a real `level` and the previous per-detector
  constants (caps→2, bold→3) were incoherent — a product judgement, not a benchmark win.
* The metric **rewards recall over precision**: a missed heading merges two sections, a
  spurious one merely splits one. Adding pdf-inspector's full `title_like` gate raised
  precision 41.3% → 48.3% but cut recall 61.1% → 51.6% and MHS *fell* 0.530 → 0.510.

Measured precision by source (after the relaxed gate):

| source | emitted | correct | precision |
|---|---|---|---|
| Bold | 80 | 43 | 53.8% |
| Numbering | 69 | 29 | 42.0% |
| FontSize | 136 | 52 | 38.2% |
| ~~Caps~~ | ~~34~~ | ~~2~~ | ~~5.9%~~ — **removed** |

Reproduce with `FDOC_HEADING_SOURCES=1 fdoc headings <pdf>` joined against ground truth.

#### T2.4b — heading evidence sources

| source | what it gives | availability in `eval/corpus` |
|---|---|---|
| PDF outline tree | text + explicit level, author-provided | 4 of 6 documents (24-79 items each) |
| Section numbering | depth from `5.3.1` | datasheets, academic papers |
| Font size vs body | fallback | always |

The outline is the signal **no benchmarked engine uses** (`grep -rin "Outlines"` over
pdf-inspector returns nothing; opendataloader's `TableOfContentsProcessor` is commented out of
its pipeline). `ti_tps5430.pdf` carries 79 items with correct nesting —
`5 Specifications` → `5.1 Absolute Maximum Ratings` → `6.3.1 Oscillator Frequency`.

It cannot be the only source: `Use_internal-report…pdf` and `jp_stat.pdf` have no outline at all.

### T4b — VLM tier (measured 2026-07-25, EC2 g6.xlarge / NVIDIA L4)

| id | check | expected | currently |
|---|---|---|---|
| T4b.1 | emits per-element bounding boxes | required | ✅ `block_bbox` + `block_polygon_points` |
| T4b.2 | internal-report Table 3 | 4 cols, header, 6 rows | ✅ all correct |
| T4b.3 | page furniture labelled, not in body | required | ✅ `footer`, `number`, watermark separate |
| T4b.4 | latency per page | < 5 s desired | ⚠️ **6.2-11.3 s** (init 13.0 s cold) |
| T4b.5 | CAD drawing dimension text | — | ❌ returns one opaque `image` block |
| T4b.6 | CJK fidelity | no substitutions | ⚠️ emitted `五十音顺` for `五十音順` |
| T4b.7 | **input rendered on opaque background** | required | ⚠️ transparent bg gives 0-1 blocks |

T4b.4 is ~10× the original assumption and drives the deployment choice (scale-to-zero async
over an always-on endpoint). T4b.5 reverses the CAD routing decision: the VLM *loses* dimension
text the deterministic path extracts with per-glyph rotation, so mechanical drawings stay local.

T4b.7 is a trap worth stating outright, because it costs a full GPU run to diagnose. `hayro`'s
`RenderSettings::default()` uses `bg_color: TRANSPARENT`; an RGBA page with a transparent
background flattens to black in any RGB consumer, so black text becomes invisible and the model
sees a blank page. Symptom: a near-empty `parsing_res_list` on a page you know carries text.

### T5 — Output contract

| id | check | expected |
|---|---|---|
| T5.1 | output validates against the DoCO schema | 100% |
| T5.2 | every element carries `id`, `type`, `page`, `text` | 100% |
| T5.3 | `nif:beginIndex`/`nif:endIndex` resolve into the `-f text` projection | 100% |
| T5.4 | `bbox`, where present, is within page bounds | 100% |
| T5.5 | emitted element order is reading order | 100% |
| T5.6 | **VLM output passes T5.1-T5.5 identically** | 100% |

T5.6 is the contract that makes routing invisible. Both engines run the same conformance suite.

---

## 4. Current baseline (2026-07-25, Apple M4 Max)

Measured with `spike/hayro-spike` before the layout layer exists.

```
Parse — eval/corpus (6 PDFs, 223 pages)
  parsed 6/6 · 0 errors · 0 panics · 0 zero-glyph
  99.732% unicode · 2.15 ms/page · 465 pages/s

Parse — opendataloader-bench (200 PDFs, 200 pages)
  parsed 200/200 · 0 errors · 0 panics
  99.970% unicode · 2.38 ms/page · 420 pages/s

Render @2×
  bench corpus   200 pages · 9.6 ms/page · 104.6 pages/s · 0 panics · 0 blank
  eval/corpus    223 pages · 7.1 ms/page · 141.3 pages/s · 0 panics · 0 blank

Layout quality
  overall 0.87630 · NID 0.91853 · TEDS 0.80892 · MHS 0.77052
  200/200 predictions · ~1.4 s for 200 documents · identical across repeat runs
```

**Deterministic engine: strongest non-AI engine on this benchmark** — overall
0.87642 vs pdf-inspector's 0.87535, NID 0.91867 ahead of everything except the
AI hybrid and nutrient.

**Hybrid tier (2026-07-25, measured end-to-end on an EC2 g6.xlarge / L4):**

```
fluree-pdf-hybrid  overall 0.90695 · NID 0.93340 · TEDS 0.92485 · MHS 0.82036
```

**#1 of 18 engines** — above opendataloader-hybrid (0.90657), with NID, TEDS
and MHS each within 0.003 of the best posted component score. The final pass
extended layout arbitration to table *regions*: an inferred grid the detector
sees no table under is demoted — to a paragraph carrying the grid's row-major
reading, because returning its glyphs to prose assembly re-ordered them worse
(doc 184 measured it) — while a detector table no grid covers becomes a VLM
insert anchor, deduplicated against router regions (doc 110's raster table
was briefly emitted twice), and a grid whose bounds substantially disagree
with the detector's box is cropped at the union (doc 150: TEDS 0.383→0.994,
the wrong-region failure class). Residual known deficits: doc 116
(list-vs-table dispute, 0.530) and doc 121 (nested table the flat crop
cannot express, 0.601).

Gap to opendataloader-hybrid (0.90657): **0.0020**, with MHS statistically tied
(0.82008 vs 0.82078) and NID within 0.001. The stack, in the order it was
measured in:

1. **Structure-arbitrated table splicing** for *all* detected tables, ruled
   included. Replace only when the VLM's shape disagrees (rows ≥2 apart or
   columns differing), except the VLM's own signature hallucination — one
   extra, ≥20%-empty column — which defers to us. Naive replacement measured
   catastrophic (TEDS 0.850→0.761); arbitrated it is worth +0.030 TEDS.
2. **Layout-detector arbitration** (PP-DocLayoutV3 alone, 178 ms/page, no
   content decode) over every page: title boxes *promote* our short prose
   blocks to headings and *split* blocks that merged a bare heading into the
   paragraph below (doc 69: 0.000→0.982); float boxes *demote* our headings
   that sit inside charts and figure captions (doc 36: 0.171→0.777). The
   blind caption guard measured negative; the same rule with an independent
   second reading measured +0.012 MHS. Nothing ever alters text or order.
3. Remaining TEDS deficit concentrates in three deep detection failures
   (116 list-vs-table, 150 wrong region, 121 nested) worth ~0.0015 overall.

**The tier ladder, each point measured** (this is the differentiation story —
every other engine is one fixed accuracy/latency point; this is one engine
with three deployable tiers):

| tier | overall | NID | TEDS | MHS | per-page cost |
|---|---|---|---|---|---|
| deterministic | 0.87649 | 0.919 | 0.809 | 0.771 | ~7 ms CPU |
| + layout arbitration (no VLM) | 0.88611 | 0.919 | 0.809 | 0.808 | ~185 ms (layout model; CPU-viable) |
| + VLM arbitration (full) | 0.90456 | 0.933 | 0.904 | 0.820 | p50 185 ms, mean 2.2 s |

The middle tier is notable: it outscores every ML engine and the commercial
engine on this benchmark using no autoregressive model at all — the layout
detector arbitrates headings and the deterministic engine does everything
else.

**Same-hardware head-to-head (2026-07-26, two identical g6.2xlarge, 200 docs
each, full end-to-end):** the reference hybrid measured mean 1.21 s / median
0.87 s / max 27.6 s / corpus 242 s (its published 0.463 s mean was Apple-M4
silicon). Ours, honest full stack with two queue-fed VLM workers: **median
0.19 s / geo-mean 0.50 s** (both ~2-4.6× better) but mean 2.16 s / p90 6.4 s /
corpus 432 s (worse — all three trace to autoregressive decode on table
crops). Accuracy on that same run: **0.90699, matching the cached reference
to four decimals** — the production-shaped deployment reproduces #1 exactly.

**Rung 3b measured (PP-StructureV3 table pipeline replacing the VLM on table
crops; VLM retained for scans/raster regions only):** overall 0.89776, TEDS
0.85629, at 0.67 s mean / 4.5 s max per table crop and no repetition-loop
hazard (nothing autoregressive). Table-quality ordering, same crops, same
arbiter: VLM 0.925 > PP pipeline 0.856 > deterministic 0.809. A naive
structure-only marry (SLANeXt + home-grown cell-text matching) measured
0.690 — *worse than deterministic*; cell matching is the engineered part of
these pipelines, do not reimplement it casually. The obvious next design is a
cascade — PP first, escalate a crop to the VLM only when PP's shape disagrees
with the deterministic grid — projected to hold ~0.90+ while cutting the tail;
unbuilt, unmeasured.

**Cascade measurements (2026-07-26, all simulated locally from the three
caches):** the cascade family arbitrates each table crop three ways — our
grid, PP-StructureV3's reading, the VLM's reading.

| variant | rule | overall | table cost |
|---|---|---|---|
| cascade-v1 | PP agrees with grid → ours; else VLM | 0.90466 | PP + ~70% VLM |
| **cascade-v2** | as v1, but PP seeing *no* table escalates | **0.90755** | PP + ~98% VLM |
| (rung 3b) | PP always, no VLM on tables | 0.89776 | PP only |

v2 is the new accuracy ceiling — above the always-VLM rung — because true
three-way corroboration, though it fired on only 1 of 51 crops, was exactly
right: two independent readers agreeing on a structure outvoted a harmful VLM
replacement. As latency relief the cascade failed (real corroboration is too
rare to skip much VLM work; v1's apparent savings came from a bug conflating
"PP saw no table" with agreement, and cost 0.003 accuracy). The cost-relief
rung remains 3b; the accuracy ceiling is now v2. Cache: eval/cascade-cache.

**Cache regenerated 2026-07-26 (g6.xlarge, NVIDIA L4).** Grid geometry moved
this session (decoration-rule trimming, run splitting, grouping choice), so
every table crop was re-rendered from current geometry and re-read: 51 crops
through PaddleOCR-VL (50 succeeded, 1 repetition-loop casualty, same as the
original run) and through PP-StructureV3 for the structure arbiter. The
cascade measures **0.90759** (NID 0.93398, TEDS 0.92562, MHS 0.82121) against
opendataloader-hybrid's 0.90657.

Note the readings are a *fresh sample*, not an isolated correction: VL
decoding is stochastic, and all 50 re-read crops differ textually from the
previous cache. The 0.90653 measured beforehand was against crops rendered
from superseded geometry, where two documents' cached readings still
contained prose the deterministic pass had since learned to keep out of the
table — so that number understated the configuration rather than the
configuration having regressed.

Latency with the full stack: p50 185 ms/doc (layout stage on every page),
mean 2.2 s (dense-table crops decode autoregressively). Both stages are
per-deployment optional: without them the deterministic engine stands at
0.876 / 7.6 ms. Reproduce without a GPU: FLUREE_VLM_RESULTS=eval/vlm-cache
FDOC_TITLE_BOXES=eval/layout-cache.

**Batching measurements (2026-07-25, g6.xlarge, all negative — recorded so
nobody repeats them):** accuracy survives batching exactly (VL outputs
110/111 content-identical, the exception being the near-blank scan whose
output is unstable on any run; layout box jitter under batch_size=16 lands
below every promotion threshold — score identical to five decimals). But
in-process batching buys almost nothing in paddlex 3.7: list-predict over
111 crops ran 448 s against 472 s sequential (~5%); an explicit
batch_size=8 measured 1.27× on small crops; layout with batch_size=16 was
flat (189 vs 178 ms/page) because the stage is CPU-preprocess-bound, not
GPU-bound. Two process replicas on one L4 degraded ≥4× — both pegged the
4-vCPU host while the GPU sat idle. Conclusion: the time levers are
deployment-level, in order — (1) vLLM/SGLang serving for the VL stage
(paddleocr 3.7 supports a genai-server backend; continuous batching is the
only real fix for decode-bound table crops), (2) more vCPU or async image
preprocessing for the layout stage, (3) running a document's crops
concurrently at the orchestration layer.

**Replica follow-up (2026-07-26, g6.2xlarge / 8 vCPU):** the 4-vCPU failure
was pure CPU starvation, confirmed. On 8 vCPU, two replicas processed the
same 56 table crops in 371 combined process-seconds against 373.7 s
single-process — **zero aggregate contention on one L4** (two 8 GB models in
24 GB VRAM). The measured wall was 249 s only because a static even/odd split
put the heavy tables in one half; with a shared work queue the wall is
work/2 ≈ 187 s ≈ 2.0×. So launch throughput doubles per GPU by running two
queue-fed workers on a g6.2xlarge (+23% instance cost), before vLLM enters
the picture. Also observed again: a crop that decoded fine single-process hit
a repetition loop in the replica run (decode is not run-to-run stable under
load) — the per-crop timeout is not optional in any production adapter. 44 of 55 table arbitrations fired
(the VLM's answer was used), so pre-filtering crops has little to cut — the
11 unused ones have no deterministic signature.

The second hybrid pass added **table-confidence routing with a structure
arbiter**: grids whose columns were inferred from alignment (never from drawn
rules) are re-read by the VLM, and the VLM's answer is used only when its
table *shape* disagrees with ours — a disagreement means our inference was
wrong; agreement means our deterministic cell text stands. The arbiter is what
made the tier safe: naive replacement of every unruled table destroyed
documents the deterministic path had at 1.000 (TEDS crashed to 0.761 in that
configuration, worse than no VLM at all), because unruled does not mean wrong.
Three residual doc-metric regressions remain (165/116 TEDS, 163 MHS, ≈−0.0008
total) against +0.034 TEDS gained.

#2 of 17 engines, above nutrient (0.885) and docling (0.882); only
opendataloader-hybrid (0.907, gap 0.0156) remains ahead. TEDS 0.884 is within
0.003 of docling's. Router: 45 of 200 docs routed
(1 page-tier, 44 region-tier, 55 VLM calls). 18 docs received spliced content;
on the other 27 the VLM returned empty image blocks (photographs the probe
over-fired on) and the splice layer dropped the anchor — probe false positives
cost GPU seconds, never accuracy. VLM cache in `eval/vlm-cache/` so this score
reproduces without a GPU (`FLUREE_VLM_RESULTS=eval/vlm-cache`).

Latency (fdoc timings + measured per-crop GPU seconds):

```
deterministic mean    7.6 ms/doc
region crop mean      1.64 s (vs 6-11 s measured for full pages)
fleet average         309 ms/doc  ·  p50 6.5 ms  ·  p90 29 ms  ·  max 19.5 s
```

90% of documents finish in under 30 ms. For scale: opendataloader-hybrid
publishes 0.463 s/doc measured on an Apple M4 (its AI component runs per
document, so mean ≈ median); our fleet mean of 309 ms is only ~33% below it,
but our median is 6.5 ms — the meaningful difference is the distribution, not
the mean. 78% of documents never leave the CPU parser, so GPU capacity sizes
to ~22% of traffic. The pure deterministic engine at 0.0076 s/page would rank
among the fastest engines on the bench's own speed column while outscoring
every non-AI engine. One repetition-loop hang was observed (a crop held the
autoregressive decoder for 15+ min at 96% CPU); the batch runner kills any
crop at 120 s and quarantines it on restart — the production adapter needs the
same guard.

Reference engines on opendataloader-bench, for context:

| engine | overall | NID | TEDS | MHS |
|---|---|---|---|---|
| opendataloader-hybrid (AI) | 0.907 | 0.934 | 0.928 | 0.821 |
| docling | 0.882 | 0.898 | 0.887 | 0.824 |
| pdf-inspector | 0.875 | 0.915 | 0.814 | 0.788 |
| opendataloader | 0.831 | 0.902 | 0.489 | 0.739 |
| *oracle best-of-3* | *0.918* | *0.938* | *0.904* | *0.881* |

---


## Table escalation: does deferring to a second engine pay?

**Measured 2026-07-26 on 82 pages from 7 real-world documents** (financial
statements, an annual report, tax and insurance forms), comparing the
deterministic engine against docling page-for-page.

The deterministic detector loses tables that are horizontally ruled with no
verticals, and tables whose rows merge. `table::suspect_tables` reports both
without acting on them: `Fragmented` (one contiguous ruled region reported as
fragments disagreeing on column count) and `MergedRows` (a column of pure
values holding cells of one value and cells of three or more).

Adjudicated by a rubric applied identically to both engines — no ground truth
exists for these documents, so the judge must be symmetric. Three defects
(fragmented / crammed / ragged) plus character recovery against the page's own
text, so an engine that drops a table cannot win by silence.

| | flagged (42 pages) | control (40 pages) |
|---|---|---|
| docling strictly better | **21 (50%)** | 4 (10%) |
| comparable | 9 | 19 |
| docling worse | 6 | 8 |
| docling emitted no table | 6 | 9 |

The trigger enriches 5x: escalation helps on half of flagged pages against a
10% background rate. Cost over the same 1156-page corpus: 42 pages escalated
(3.6%), +42s on a 4.3s baseline, **2.0s per improved page** against 8.7s for
running docling everywhere — 4.4x more efficient at 3.6% of the latency.

**Escalation must be arbitrated, not blind.** docling is *worse* on 12 of the
42 flagged pages, and produces no table at all on 6: on the Enact delinquency
form it returns zero table characters where the deterministic pass returns
543. Every such case is detectable at runtime by the same rubric — no table,
far fewer characters, or more defects — so the tier can only help if its
output must earn its place. Same principle as the existing cascade: models
arbitrate structure, they never own it.

Rates for the trigger itself: **1 of 56 tables on the benchmark corpus
(1.8%)**, 48 of 453 (10.6%) across the real-world set. It essentially never
fires on the benchmark, so a docling tier cannot move that score.

## 5. VLM tier — local development

**Problem:** PaddleOCR-VL targets CUDA. This dev machine is Apple Silicon (arm64) with no GPU
that CUDA can use.

**Do not** run an amd64 CUDA image under Docker on this machine. Measured with the docling
Lambda image: `qemu-x86_64` emulation was **>20× slower** — it failed to finish constructing a
`DocumentConverter` in 5 minutes versus 19.4 s natively. Any number measured that way is invalid.

Options, in preference order:

| option | for | against |
|---|---|---|
| **Remote GPU endpoint** (dev instance or hosted) | matches production exactly; no local heft | needs network + a running service |
| **MLX / Metal port** of a small VLM | native Apple Silicon, fast | PaddleOCR-VL may have no MLX port; a *different* model means dev ≠ prod |
| **CPU-only inference locally** | no GPU needed, correct output | slow; fine for correctness tests, useless for timing |
| **API VLM** (Gemini/GPT) behind the same adapter | trivial to run | different model; 92.91/86.59 vs 96.34 on OmniDocBench |

**Design consequence:** the VLM must sit behind a **trait/adapter** so the backend is swappable
per environment — local CPU, remote GPU, or hosted API — without touching the pipeline. Correctness
tests (T5.6) run against any backend; performance tests only count on production-equivalent hardware.

This needs a decision before Phase 3. It does not block Phases 1-2.

---

## 6. Adding a test

1. Put the document in `eval/corpus/` (tracked only if redistributable —
   otherwise add it to `eval/fetch-corpus.sh` with its source URL and sha256)
   or note it in §2.2.
2. Add expectations to `eval/expectations/<doc>.json`.
3. State the expected value **and where it came from** — measured, computed, or asserted by hand.
4. If it is a known-failing case, add it anyway with the current value and mark it ⚠️. Known
   failures that are written down get fixed; ones that aren't, don't.

---

## 7. Known defects (layout pass 1)

Recorded as they are found, per §6: written-down failures get fixed.

| id | defect | evidence | status |
|---|---|---|---|
| L1 | Lines sharing a baseline but separated by a wide horizontal gap merge into one line. | `ti_ne555.pdf` p7 | **mostly fixed** — `BLOCK_GAP_RATIO = 1.5`, derived from the measured gap distribution. Side-by-side chart labels and axis titles now split correctly. `www.ti.comSLFS022K` still merges: that gap is below threshold. |
| L2 | Missing space between word pairs: `buyertargeting`, `risktolerance`. | internal-report p2 | **fixed** — root cause was not the threshold. The PDF encodes an explicit space glyph; spaces have no outline, and `assemble` was dropping every outline-less glyph. Explicit spaces are now honoured as ground truth, with the geometric gap as fallback. |
| L4 | Bullet marks split from their text (`■` on its own line). | internal-report p2 | **fixed at the block level** — markers (≤4 non-space chars, and either smaller or outdented relative to their neighbour) are pulled out before blocks form, then attached by containment first, nearest-following second. A bullet's baseline sits between line 1 and line 2 of its own text and was cutting paragraphs in three. |
| L3 | Subscripts join the parent line: `V- Low-Level Output Voltage (V)OL`. | `ti_ne555.pdf` p7 | open — needs a baseline-offset + font-size test (the subscript is 4.7pt against 6.3pt). |
| L6 | Spurious spaces inside numbers: `Page 1 3 of 1 5`. | internal-report p13 | **fixed** — tabular figures give a narrow `1` a full digit advance, so the `1`→`3` gap measured 0.2515, just over the Latin threshold. Digit pairs now use their own threshold (0.32), set from the digit-pair distribution (intra-number gaps end at 0.25, valley 0.25-0.35). |
| L5 | Spurious spaces between CJK characters: `( 別 紙 2 )`, `エ フ エム`. | `jp_stat.pdf` | **fixed** — full-width glyphs advance wider than the Latin word threshold. Geometric space insertion is now suppressed between two spaceless-script characters. |

| L7 | Modal leading over-estimated on documents with no multi-line paragraphs, merging separate entries. `jp_stat.pdf` reported 3.43x and collapsed four people into one block. | `jp_stat.pdf`, `cn_gov.pdf` | **fixed** — only gaps ≤ 2.0x count toward the mode; beyond that is a paragraph break in any typography. jp_stat now 1.20x / 16 blocks, cn_gov 4.53x → 1.68x. |

| L9 | Hanging indents split list items: a wrapped bullet continuation became its own block, then a one-word "heading" (`count`, `size`, `range`). | `ti_tps5430.pdf` p1 | **fixed** — indentation alone is not a paragraph break. Both common styles change the left edge *within* a paragraph (first-line indent moves the opening line right, hanging indent moves continuations right). The break test now also requires the previous line to have ended short of its column's right edge. |
| L8 | **Multi-column pages merge columns into single lines.** `"that integrates a low-resistance, high-side N-channel – TPS5430: 5.5V to 36V"` is left-column prose concatenated with a right-column bullet. | `ti_tps5430.pdf` p1, `cn_arxiv.pdf` | **fixed** — column segmentation by vertical whitespace projection, run *before* line assembly. A gutter is identified not by width but by being empty down the page: an occupancy grid marks bins empty across ≥90% of live rows. Two guards were needed on real pages — a minimum column width (0.15 of text width), or the whitespace beside a bullet strip reads as a gutter and every marker becomes a column; and a rejoin pass, or a full-width title straddling the gutter is cut in half (`TPS543x 3A, Wide Input R` / `ange, Step-Down Converter`). |

**Heading status after L8/L9.** Typography-only false positives on `ti_tps5430.pdf` fell from
320 to 193 as column segmentation and hanging-indent handling landed. The remainder are table
cells and figure labels set larger than the document's 8pt modal size — they will be absorbed by
table detection, and tightening heading heuristics against them would be fitting to a symptom.
On single-column `Use_internal-report…pdf` (no outline at all) the same code yields 26 headings with
correct levels. **T2.4 should still not be measured until table detection lands.**

| L10 | Cell text interleaved across columns: `More than 10 years Less than 1 % of or five to 10 years`. | internal-report p13 | **fixed** — glyphs are routed into cells and lines assembled *within* each cell. Routing whole page-lines interleaves neighbours, because a page-level line spans the grid. Same ordering constraint as columns: segment first, assemble second. |
| L11 | Grid extended past the table, adding an empty row and the page footer as a final row. | internal-report p13 | **fixed** — a table ends at the first empty row after its content, not the last populated row in the grid. A footer separator rule far below still clusters as a grid line. |
| L13 | Chart gridlines formed a full-page 9x2 "table" that shredded a document into cells. | bench doc 78 | **fixed** — a column boundary must be supported by ≥50% of the horizontal grid lines' endpoints. A real segmented grid breaks at the same x on every line; a chart's gridlines only share the plot edges. |
| L14 | Tables built from segmented *vertical* rules were rejected entirely. | bench docs 45, 149 | **fixed** — the axes were asymmetric in my own logic: horizontal segment endpoints gave columns, but vertical endpoints gave nothing. Both directions occur in real files. Worth TEDS 0.392 → 0.488. |
| L15 | Headings at body font size were invisible (`CHAPTER 1.`, `COURSE MARKING DRIVERS` both 11.0pt, same as body) — 29 documents scored MHS 0.000. | bench doc 151 | **fixed** — added a short/all-caps/single-line signal. MHS 0.475 → 0.522. |
| L16 | No font-weight signal — bold is the most common heading cue after size. | corpus-wide | **fixed** — `OutlineFontData` carries `weight` and `postscript_name`; both are now read (name-sniffing for `bold`/`black`/`heavy` covers subsets that omit the weight class). Bold is ignored when most of the document is bold, since it then carries no information. MHS 0.528 → 0.541. Smaller than hoped: heading *recall* is not weight-limited on this corpus. |
| L17 | Alignment-based table detection was net-negative: it converted prose into tables, costing more in reading order than it gained in structure. | corpus-wide | **fixed, enabled.** Two changes. (a) It now works from **glyphs**, not assembled lines: table columns are routinely closer than the 1.5x line-split threshold, so `Saccharometer DI Water Glucose Solution Yeast Suspension` arrived as one line and the line-based version found *zero* candidates on every document that needed this strategy. (b) Candidates must be corroborated by drawn geometry. Three-row candidates receive a stricter gate: at least three columns plus a spanning rule or dense fill evidence. **TEDS 0.488 → 0.600, zeros 14 → 7**; the short-table extension also improved NID and MHS on every affected document. |
| L18 | Table evidence was evaluated page-wide and one signal had to describe the whole grid. This missed local partial grids, shaded headers, booktabs tables, and one-column ruled stacks. | bench docs 116, 117, 127, 149, 180 | **fixed.** Connected rule components localize incomplete grids; tiled header fills and numbered captions corroborate aligned candidates; sustained two-border stacks may form one-column tables. **TEDS 0.600 → 0.694, zeros 7 → 2**. The only remaining zeros (110, 122) are raster-only and require the VLM tier. |
| L19 | A page can have valid body glyphs but contain an image-only table, so the page-level `glyph_count == 0` router signal does not fire. | bench docs 110, 122 | **open — VLM plumbing.** Capture raster-image bounds in `Collector::draw_image`, identify large glyph-empty image bands, and route the crop rather than the whole page. The current extractor intentionally discards images and no VLM adapter is wired into this crate yet. |
| L20 | A coarse ruled grid claimed glyphs before stronger aligned/fill hypotheses could compete; sparse tables with blank cells had no repeated-column signal. | bench docs 165, 170, 187, 190, 197, 200 | **fixed.** A strongly wider or structurally richer corroborated candidate can replace one prose-heavy ruled grid. Repeated fill bands recover logical rows across wrapped cells; horizontal rule bands use the densest row for columns and first-column labels for sparse row starts. Occupancy and relative-structure gates rejected the observed false positives. **TEDS 0.694 → 0.787**, with all six affected documents improving in TEDS and NID. |
| L12 | Spacing artefacts around numbers: `1 %`, `1 .22V`. | internal-report, `ti_tps5430` | **fixed** — the digit threshold now covers digit-adjacent pairs generally (digit→`.`/`,`/`%` and `.`/`,`→digit), not just digit→digit. Tabular figures leave the same wide gap before a decimal point or unit sign. |

### L18 — the rejoin pass was silently undoing column segmentation

`line::rejoin_spanning_lines` exists to repair a full-width title that a column
cut splits in half. Its test was: a line ending flush against the cut plus a line
beginning flush past it, on the same baseline.

In **justified two-column body text every row matches that description** — the left
column ends at the gutter and the right begins just past it. So the rejoin fired on
every row and merged the columns straight back together, silently reversing correct
column detection. `…gratitude to the teams` came out joined to `Ethics Statement`.

This dominated the worst documents: the ten largest MHS deficits accounted for **84%**
of the entire MHS gap, and they are two-column academic papers.

Fixed by counting first: a genuine spanning line is rare, so if more than 25% of a
column's lines look like spanning fragments, they are not.

  overall  0.83676 → 0.85075
  NID      0.88691 → 0.89948
  MHS      0.70437 → 0.72494

A regression test now builds a justified two-column page and asserts the left column
does not absorb right-column text.

### Where the remaining MHS loss lives

Bucketing the 107 scored documents by whether we recover the exact heading set:

| bucket | docs | MHS mean |
|---|---|---|
| exact heading set | 52 | 0.934 |
| partial overlap | 39 | 0.535 |
| no overlap | 16 | 0.342 |

So the loss is **heading detection, not content text** — with a perfect heading set MHS would be
~0.93. The 39 partial-overlap documents are the largest pool, worth roughly +0.145 if recovered.

Of 59 missed headings, 80% are genuinely undetected (19% split across lines, 2% merged). The
undetected ones cluster by shape:

| property | count | status |
|---|---|---|
| ≤2 words (`Abstract`, `References`, `Trash`) | 18 | **open** — no signal reaches these; isolation was tried and rejected |
| ALL CAPS (`CREATING SLIDES`, `CHAPTER 1.`) | 10 | **open** — caps detection rejected twice on measurement |
| number + dash/paren prefix (`01 - Find…`) | 6 | fixed |
| lettered/roman sections (`B.1 …`, `II. …`) | 6 | fixed |
| >12 words | 2 | open — the word gate |

The ≤2-word and ALL-CAPS classes together are 28 of 47 and both resist the obvious signals.
They are the reason MHS has plateaued around 0.70.

### The deterministic table ceiling on this corpus

Two documents remain at TEDS 0.000 — `01030000000110` and `01030000000122` — and they are not
a missing heuristic. Their tables are raster: doc 110's ground truth has 26 table rows while the
page yields only 16 text lines in total. The content is not in the text layer.

Every non-ML engine scores 0.000 on both, and docling — with a layout model *and* TableFormer —
manages 0.000 and 0.115. Only the AI-routed hybrid recovers them:

| doc | ours | pdf-inspector | opendataloader | edgeparse | docling | ODL-hybrid |
|---|---|---|---|---|---|---|
| 110 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.930 |
| 122 | 0.000 | 0.000 | 0.000 | 0.000 | 0.115 | 0.716 |

**This is the deterministic ceiling for zero-scoring tables on this corpus.** These two are
precisely the case the VLM tier exists for, and they need crop-level VLM plumbing (the extractor
currently discards images and no VLM adapter is wired). Recorded so the remaining TEDS gap is
not mistaken for a heuristic that has yet to be found: the other 0.12 is distributed across the
40 non-zero table documents and is ordinary quality work.

### Tried and rejected

Recorded so they are not re-attempted. A heuristic that helps a handful of documents and hurts a
handful is indistinguishable from noise on a corpus of 42 table and 107 heading documents — but
one that regresses NID is hurting across all 200. The evidence is asymmetric and should be
weighted that way.

| experiment | outcome |
|---|---|
| Ungated alignment-based table detection | TEDS 0.488 → 0.526 but NID 0.868 → 0.839, MHS 0.528 → 0.506; net worse. Fixed later by requiring corroborating drawn geometry (L17). |
| pdf-inspector's full `title_like` gate | Precision 41.3% → 48.3% but recall 61.1% → 51.6%; MHS fell 0.530 → 0.510. Two of its tests reject real headings here — title-case (12%) and trailing full stop (9%). |
| All-caps heading detector | 34 emitted, 2 correct (5.9% precision). Removing it gained MHS 0.020. |
| General newspaper/tabular classifier | NID regressed. |
| Bounding-box spanning-band inference | Larger regressions than the classifier. |
| All-caps detector, **retried under the `title_like` gate** | Still net-harmful: MHS 0.704 → 0.678. The gate was the hypothesis for why it failed the first time; it was not the reason. Rejected twice, on measurement both times. |
| Heading "sequence" tiers, **second attempt keyed on size alone** | overall 0.85673 → 0.85623. Rejected on both keyings. The idea is sound in pdf-inspector but depends on machinery we lack: it keys on font *identity* and indent bucket and additionally requires a distinct bold face or a coherent numbered run. Prerequisite for a third attempt: `Line` must carry the font name. |
| Heading "sequence" tiers (style repeated but sparse, borrowed from pdf-inspector's `classify_heading_sequences`) | 64 emitted, 17 correct — 26.6% precision; overall 0.85075 → 0.85000. The idea is sound and it did catch the target case (`Limitations` at 12.0pt against 11.0pt body, under the 15% solo margin), but our style key — quantised size plus bold — is too coarse. pdf-inspector additionally keys on font *identity* and indent bucket, and requires a distinct bold face or a coherent numbered run. Worth retrying if `Line` ever carries the font name. |
| Isolation signal (gap above > 1.5× font size, ≤6 words, single line) | MHS 0.704 → **0.621**. Idea (4) in `eval/MHS_ANALYSIS.md`, proposed as "nearly free" and orthogonal to size/weight/case. It is orthogonal, and it is wrong: too much body text is set off by whitespace. |
| Heading level normalisation | Kept, but **not** for benchmark reasons: MHS 0.535 → 0.530. MHS ignores depth entirely (§T2.4a). Retained because DoCO carries a real level. |

**Method note.** L1 and L2 pull in opposite directions, so both thresholds were set from a
measured distribution rather than by eye. `fdoc gaps <dir>` histograms edge-to-edge horizontal
gaps between baseline-adjacent glyphs, normalized by font size. Over `eval/corpus` (305k pairs):

```
0.00-0.20   intra-word kerning         78.5% cumulative
0.20-0.30   valley                      2.9%   <- word-space threshold sits here (0.25)
0.30-0.45   word spaces (peak 0.35)    12.3%
0.45-1.00   wide/justified spacing       2.4%
1.00-5.00   nearly empty                       <- block threshold sits here (1.5)
>5.00       column gutters, cells        1.5%
```

Both thresholds sit in empty bands, so neither is sensitive to its exact value.
`fdoc pair <pdf> "<text>"` prints the measured gap around a specific character pair — that is
what showed L2 was a dropped space glyph rather than a threshold problem.
