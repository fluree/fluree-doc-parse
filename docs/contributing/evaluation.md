# Evaluation

Every accuracy and latency claim about this engine traces to
[`eval/TEST_PLAN.md`](https://github.com/fluree/fluree-doc-parse/blob/main/eval/TEST_PLAN.md).
That file is the source of truth; this page explains how to use it.

## What is measured against

The corpus, the standings, the reproduce recipe and the caveats are in
[Benchmarks](../benchmarks/README.md), which is the page to send anyone
quoting a number. What follows is the part that only matters if you are
changing the engine.

The caches every rung reproduces from:

```
eval/layout-cache/      layout-detector boxes
eval/structure-cache/   table-structure readings
eval/vlm-cache/         VLM readings
eval/cascade-cache/     the full cascade
```

```bash
FDOC_TITLE_BOXES=eval/layout-cache \
FDOC_STRUCTURE_RESULTS=eval/structure-cache \
FDOC_TIER_RESULTS=eval/vlm-cache \
fdoc convert document.pdf
```

The deep reader's cache is produced by `eval/llm-tier/run_tier.py` over the
crops `fdoc dev render-routed` writes. **A change to those prompts invalidates
the cache**, in the sense that matters: the committed readings are no longer
what the committed prompt produces. Refresh it in the same commit, and expect
the aggregate to hold while individual documents move — see the variance note
in [the scoreboard](scoreboard.md#rules).

`eval/MHS_ANALYSIS.md` has the heading-metric analysis behind the MHS caveat.

## Corpus gaps

Tracked in TEST_PLAN §2.3 and worth knowing before trusting a routing number.
The most important: **there is no scanned / image-only PDF in the corpus**,
which is the router's single most important signal. Also missing: broken CID
fonts, Korean, forms, multi-column newspapers, financial statements with
nested tables.

## Baseline discipline

Two rules, both in place because the alternative is worse:

**A change that moves a number updates it in the same commit, with a reason.**
A silently drifting baseline turns every later measurement into an argument
about which number was real.

**Negative results stay on the record.** The ablation table keeps ideas that
were tried and measured worse, with their numbers. The
[isolation-signal](../design/headings.md#what-was-tried-and-rejected) entry is
the archetype: ranked the most promising untried idea, implemented carefully,
and it fired on 13 blocks of which zero were headings.

## Evidence

`eval/evidence/` holds the render-fidelity PNGs behind T4.4/T4.5 and the raw
VLM block dumps behind T4b — including the bounding-box output that made the
model tier viable at all, since a reading without coordinates cannot be
spliced back into a page.
