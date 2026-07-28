# MHS gap analysis

Why heading structure scores what it does, what has been tried against it, and
what the metric does **not** measure. The rejected experiments here are the
point of the document: several are plausible enough to be re-proposed, and §5(4)
is one that was ranked most promising and then measured as actively harmful.

**Where it stands** (see `eval/TEST_PLAN.md` §4 for the full run):

| | MHS |
|---|---|
| deterministic tier | 0.771 |
| full cascade | 0.821 |
| pdf-inspector · docling · opendataloader | 0.788 · 0.824 · 0.739 |

The cascade closed most of the original gap — the deterministic tier alone is
still behind pdf-inspector and docling, and the analysis below is written
against that tier, which is where heading detection actually happens.

Everything below is reproducible. Commands assume `opendataloader-bench` cloned alongside and
`fdoc` built (`cargo build --release`).

---

## 1. Where MHS is calculated

`opendataloader-bench/src/evaluator_heading_level.py`. It is **not** our code — it is the
benchmark's, and we score against it unmodified.

```bash
FLUREE_PDF_BINARY=/path/to/fdoc uv run python src/pdf_parser.py --engine fluree-doc-parse
uv run python src/evaluator.py --engine fluree-doc-parse
# -> prediction/fluree-doc-parse/evaluation.json
```

Our adapter is `bench-adapter/pdf_parser_fluree.py` (copy into the bench's `src/`, and add a
`fluree-doc-parse` entry to `ENGINES` and `_ENGINE_MODULES` in its `engine_registry.py`).

## 2. What MHS actually measures — read this before optimising

The name is misleading. Three things are non-obvious and all of them shaped what worked:

**(a) It does not measure heading depth.** From the module docstring:

> builds a flat section tree that treats all heading levels as equivalent

`_parse_markdown_structure` appends every heading to `root.children` — headings never nest. And
the corpus agrees: **all 193 ground-truth headings across 107 documents are `#`**, not one file
uses `##`.

Consequence: our level assignment is *unmeasurable here*. Unifying it by style rank moved
internal level-correctness 38.7% → 57.6% and MHS 0.535 → 0.530 — noise. It was kept because
DoCO `doco:SectionTitle` carries a real level and the previous per-detector constants were
incoherent, but that is a product judgement, not a benchmark result. **Do not tune levels
against this metric.**

**(b) Content text is scored, not just headings.** Prose between headings becomes a `content`
child of the preceding heading node, and `rename()` charges normalised Levenshtein distance on
node text. So MHS partly measures paragraph text fidelity and how content partitions between
headings — not only whether a heading was found.

**(c) It is asymmetric, and favours recall.** A *missed* heading merges two sections, so one
content node holds the wrong text and the tree shape is wrong. A *spurious* heading only splits
one section. Empirically: adding pdf-inspector's full `title_like` gate raised precision
41.3% → 48.3%, cut recall 61.1% → 51.6%, and MHS **fell** 0.530 → 0.510.

Scoring is `1 - edit_distance / max(gt_nodes, pred_nodes)`; the denominator uses the larger
tree, which softens the penalty for over-emitting and is part of why recall dominates.

Also note: GT with no headings returns `None` (excluded, 93 of 200 documents); GT with headings
and prediction with none returns **0.0** outright.

## 3. Detection decomposition

The precision/recall figures below predate the document-title detector. The
current zero-score count is shown separately because it was remeasured.

```
ground-truth headings : 193 across 107 scored documents
we emit               : ~205
recall                : ~61%
precision             : ~48%
current zero documents:  5 of 107  (14 → 6 after title detection → 5 after table ordering)
```

Precision by evidence source — reproduce with
`FDOC_HEADING_SOURCES=1 fdoc headings <pdf>` joined against ground truth:

| source | emitted | correct | precision |
|---|---|---|---|
| Bold | 80 | 43 | 53.8% |
| Numbering | 69 | 29 | 42.0% |
| FontSize | 136 | 52 | 38.2% |
| ~~Caps~~ | ~~34~~ | ~~2~~ | ~~5.9%~~ — removed, worth +0.020 |

## 4. What has been tried

| change | MHS | note |
|---|---|---|
| baseline (font size only) | 0.471 | 29 documents scored 0.000 |
| + all-caps detector | 0.522 | later measured at 5.9% precision |
| + font weight (bold) | 0.541 | I predicted this would be the biggest win. It was not. |
| + level normalisation | 0.530 | unmeasurable — see §2(a) |
| + full `title_like` gate | 0.510 | precision up, recall down, net worse |
| + relaxed `title_like` | 0.560 | dropped the title-case and trailing-dot tests |
| − caps detector | 0.580 | |
| + bare-numbered headings | **0.615** | `7 Variants of SJ Observer Models` |
| + bounded document-title detector | **0.658** | current benchmark main: 0.610 → 0.658; zero documents 14 → 6 |
| + interleave tables with prose | **0.679** | fixes section content order; zero documents 6 → 5 |
| + guarded three-row aligned tables | **0.686** | recovers short tables without false positives; zero documents unchanged |
| + region-first table recovery | **0.689** | local rule components, header fills, captions, and one-column tables |
| + competing table hypotheses | **0.700** | prevents coarse grids from swallowing headings and prose |
| + ink-density weight inference | 0.729 | *alone, a regression* — see §4a |
| + run-in and bold-density guards | **0.762** | the two together; neither works without the other |
| + two-column gutter under a header | 0.760 | NID +0.004; costs a little MHS, net positive |
| + whitespace isolation detector | 0.755 | **reverted** — 0 of 13 emissions were headings, see §5(4) |

### 4a. Weight was silently unavailable on 40% of the corpus

`hayro`'s `font_data()` **returns `None` for Type 1 fonts**, and many embedded
subsets omit `/FontWeight` besides. Measured across the benchmark corpus:

```bash
for f in pdfs/*.pdf; do fdoc weights "$f" | grep -qv "weight=None" || echo "$f"; done | wc -l
# -> 81 of 200 documents expose no weight at all
```

So the Bold detector — the *highest-precision* of the three at 53.8% — was dark
on exactly the classic LaTeX papers where a bold section title is the only cue.
Two separate causes, both fixed:

- **URW naming.** Ghostscript's substitute families abbreviate their styles to
  `Regu`/`Medi`/`Ital`/`Bold`, and there `Medi` *is* the bold face —
  `NimbusRomNo9L-Medi` sets every bold word in a pdfTeX paper. The name test
  looked only for `bold`/`black`/`heavy`.
- **Type 1 fonts expose nothing at all**, so no name test can work. `extract.rs`
  now infers weight from **ink density**: the fraction of each glyph's box its
  outline actually fills, averaged per face and compared against the document's
  body face. Bold carries ~25-30% more ink; italics differ by a few percent, so
  a 1.15 margin separates them. Runs only when the document declares no weights.

**The inference alone is a regression** (MHS 0.745 → 0.729) even though it is
correct — it *restored* a weak signal, and the Bold detector then fired on
everything bold. Two guards were needed, and only the three together win:

| configuration | MHS |
|---|---|
| baseline | 0.745 |
| ink weight only | 0.729 |
| guards only | 0.729 |
| **ink weight + guards** | **0.762** |

The guards, both in `heading.rs`:

- `runs_into_prose()` — LaTeX `\paragraph{}` sets a bold lead-in on the same
  line as the text it introduces (`**Filtered task names.** We present…`).
  Length and weight cannot separate that from a title; a sentence boundary with
  real text after it can. Worth +0.013 alone, and the abbreviation carve-out
  (`vs.`, `Fig.`, `et al.`) a further +0.014 — do not skip it.
- `MAX_BOLD_HEADINGS_PER_PAGE` — where a document sets chart legends or table
  headers bold, one page proposes a dozen headings (doc 38: `October 2020`,
  `Oct 2020`, `Don't know`). Past a plausible density, bold is measuring
  something else and is dropped for the document. Benchmarked at 6/10/14/∞;
  10 is the best and ∞ costs 0.007.

### 4b. Display-typography experiments (2026-07-25, all measured)

| idea | result | why |
|---|---|---|
| tracked-text repair (`H O W C A N` → `HOW CAN`) | **kept**, +0.0001 | pure text correctness; also feeds NID |
| caption guard (`Figure 2.1:` not a heading) | rejected, −0.0004 both scoped and unscoped | GT blesses prominent captions as headings often enough that recall wins; `is_caption()` kept for the DoCO emitter only |
| colon-lead headings (`As a boater:`) | rejected in three forms | ungated fires on every "For example:"; gated recovers nothing because poster block order is not adjacent |
| prose-weighted body font (slide docs) | rejected, −0.007 MHS | body-font shifts ripple through every size-tier document |

The plateau holds: docs 199 (slide titles), 069 (single-word headings at body
size) and the ≤2-word/ALL-CAPS class resist every corpus-safe signal tried.
These are the clearest candidates for the VLM tier or a learned classifier,
not more geometry.

Two lessons for whoever picks this up: **measure before predicting** (the font-weight prediction
was wrong), and **A/B behind an env var is unreliable** — toggling the caps detector that way
showed no change, while deleting it outright gained 0.020.

## 5. Ideas, ranked

**(1) Attack the 5 remaining zero-scoring documents.** Worth the most per unit effort: each is a full
1.0 lost. The document-title detector recovered eight of the previous 14 without regressing any
scored document. Table/prose interleaving recovered one more. Enumerate the remaining five and characterise each missed heading — font size vs
body, bold, isolation, position on page.

```bash
python3 - <<'EOF'
import json,re
d=json.load(open('prediction/fluree-doc-parse/evaluation.json'))
o={x['document_id']:x['scores'] for x in d['documents']}
for k in o:
    if o[k]['mhs'] is not None and o[k]['mhs']<0.01:
        gt=[m.group(1) for m in (re.match(r'^#{1,6}\s+(.*)',l.rstrip())
            for l in open(f'ground-truth/markdown/{k}.md',errors='replace')) if m]
        print(k, gt)
EOF
```

**(2) Document-title heuristic — implemented.** It examines only the first four short blocks on
page 1, stops once prose begins, and requires prominence, vertical isolation, or a Contents
label. This recovered eight zero/low documents with no MHS regressions and was worth +0.048.

**(3) Improve content-node text, not heading detection.** §2(b) means paragraph fidelity is
scored. Our NID is 0.865 so text is broadly fine, but check whether zero/low MHS documents
correlate with poor content text rather than missed headings — that would redirect the whole
effort. Cheap to test: compare per-document NID against per-document MHS.

**(4) ~~Isolation as a signal.~~ Tried and rejected — do not retry as a standalone detector.**

The reasoning was that a heading carries more whitespace above it than the modal leading, that
`block.rs` already computes the gap, and that the cue is orthogonal to size/weight/case. It was
ranked here as the strongest untried idea. It is wrong, and the way it fails is worth keeping.

Implemented as a last-resort detector — tried only after outline, numbering, weight and size had
all declined, and only on documents whose typography offers nothing else (one size, one weight,
no outline). It required the gap above a block to exceed 1.6× the document's median block gap
**and** to exceed the gap below by 1.4×, on the theory that a heading is pushed away from the
section it ends and pulled toward the one it opens, while a paragraph sits evenly between its
neighbours.

It fired on 10 documents, 13 blocks. **None of the 13 was a heading:**

```
Figure 8.7a–c A gazelle horn used in al-Sadu weaving.   figure caption
Diagram 4 Distribution of @komnas.ham Instagram ...     figure caption
Source: World Bank (2022a)                              source note
13ASEAN Migration Outlook                               running head
298 | Ch. 13. Homogeneous Investment Types              running head
The Law Library of Congress 2                           running head
ence optimization on intel gaudi2.                      column fragment
```

MHS 0.760 → 0.755. Two reasons it cannot be tuned into shape:

- Whitespace isolation does not identify headings, it identifies *anything set apart from the
  text flow* — and captions, source notes and running heads dominate that set. They are more
  isolated than headings, not less.
- **7 of the 10 documents it fired on have no ground-truth headings at all.** The signal is
  anti-correlated with the thing it is meant to find: a page whose only isolated block is a
  figure caption is exactly a page with no sections.

If isolation is revisited it can only be as *corroboration* for a candidate some other signal
already proposed — never as a source of candidates. Note that this buys precision, and §2(c)
shows MHS pays for recall, so the upside is small either way.

**(5) Re-examine `FontSize` precision (38.2%, 136 emitted).** It is the largest source and the
weakest. Its guards are ad hoc (≤2 lines, ≤90 chars). Worth checking whether requiring a
*distinct size tier used by ≥2 blocks* helps — pdf-inspector has a "sequence" concept along
these lines (`sequence_level`, `singleton_bold_label_does_not_form_sequence` in
`src/markdown/heading.rs`) that we have not adopted.

## 6. Where the code is

| file | role |
|---|---|
| `crates/fluree-doc-pdf/src/heading.rs` | detection, `title_like` gate, level normalisation |
| `crates/fluree-doc-pdf/src/block.rs` | blocks; `bold`, `font_size`, modal leading |
| `crates/fluree-doc-pdf/src/outline.rs` | PDF bookmark tree — strongest signal where present, 4 of 6 local corpus docs |
| `crates/fluree-doc-pdf/src/document.rs` | pipeline order; heading → `doco:SectionTitle` |
| `eval/TEST_PLAN.md` | §T2.4a records the metric findings above |

## 7. Caveat

193 headings over 107 documents is a small sample — one document swings MHS by roughly 1%.
Treat single-document wins with suspicion, and prefer changes that move several documents.
The corpus is also 199/200 Latin-script and curated by one project; nothing here has been
validated against a representative customer mix.
