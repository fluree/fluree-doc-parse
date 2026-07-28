# Headings

The least certain part of the pipeline, and the one the [model
tiers](../concepts/escalation.md) most often improve.

## A priority cascade

Five detectors, tried in order. Each runs only where the one before it
declined, so the [`evidence`](../concepts/provenance.md) value on a heading
tells you how far down the ladder it came from.

| order | detector | evidence | basis |
|---|---|---|---|
| 1 | document title | `title` | a one-off style in the first blocks of page 1 |
| 2 | outline match | `outline` | the author named this heading |
| 3 | numbering | `numbering` | `5.3.1` states its own depth |
| 4 | weight | `bold` | bolder than body text |
| 5 | font size | `font-size` | larger than body, in a repeated size tier |

`font-size` is the weakest claim in the set — it is last because it is the one
most easily fooled by emphasis, captions and pull quotes.

## The outline tree

**The signal no benchmarked engine uses.** A PDF's outline (bookmark) tree is
author-provided and explicitly hierarchical: where a block matches an outline
title, its level is taken directly rather than guessed.

This was verified against the alternatives: a source-level check of the other
benchmarked engines found no outline-tree usage at all — one never greps for
the outline dictionary, and another has its own outline-derived
table-of-contents processor commented out of its own pipeline. It is
near-ground-truth hierarchy, free, and sitting unused in the file.

```bash
fdoc dev outline report.pdf     # what the document declares
fdoc dev headings report.pdf    # what we detected, and on what evidence
```

## The document title

A title is often a one-off style, which means it cannot form a repeated
font-size tier and the generic detectors miss it. It is recovered first, from
a bounded look at the first few short blocks on page 1, stopping once prose
begins and requiring prominence, vertical isolation or a contents label.

Worth **+0.048 MHS**, recovering eight zero-or-low-scoring documents with no
regressions.

## Table of contents suppression

Headings after a contents marker on the same page are suppressed — the entries
below it are a table of contents, not sections. Without this a TOC produces a
full set of phantom headings that also happen to match the real ones.

## What was tried and rejected

Recorded because each is plausible enough to be proposed again. Full detail in
`eval/MHS_ANALYSIS.md`.

**Isolation as a signal** — the idea that a heading carries more whitespace
above it than the modal leading. It was ranked the strongest untried idea:
`block.rs` already computes the gap, and the cue is genuinely orthogonal to
size, weight and case.

Implemented as a last-resort detector, gated to documents whose typography
offers nothing else, requiring the gap above to exceed 1.6× the median and the
gap below by 1.4×. It fired on 10 documents, 13 blocks. **None of the 13 was a
heading** — they were figure captions, source notes and running heads. MHS
0.704 → **0.621**.

**All-caps detection** — rejected twice, on measurement both times. The second
attempt added a `title_like` gate on the hypothesis that the gate was why it
failed the first time. It was not: MHS 0.704 → 0.678.

## What MHS does not measure

Worth knowing before optimising against the benchmark: its evaluator builds a
*flat* section tree, and all 193 ground-truth headings across 107 documents
are `#`. Heading **level** is therefore unmeasurable there, and prose text
between headings is scored alongside the headings themselves.

Do not tune levels against that metric. The levels exist because
`doco:SectionTitle` carries a real one that consumers need — a product
judgement, not a benchmark result.
