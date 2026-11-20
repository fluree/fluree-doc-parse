# The router

Decides which pages and regions the deterministic engine cannot read. See
[`fdoc triage`](../cli/triage.md) to run it, and [Escalation and
arbitration](../concepts/escalation.md) for what happens afterward.

## The objective is asymmetric

A deterministic page costs milliseconds; a model page costs seconds of GPU. So
the question is never *might the model do better?* — on ordinary born-digital
pages it does not — but **is the deterministic output unusable?**

That has exactly two causes, and both are measured rather than predicted:

| cause | measured by |
|---|---|
| the text is pixels | glyph count against image coverage |
| the text is garbage | Unicode resolution rate |

No analysis of absent glyphs can parse a scan, and broken CID fonts produce
glyphs whose Unicode is unknown or wrong — the layout can be perfect and the
text worthless.

## Page verdicts

| verdict | condition |
|---|---|
| `Deterministic` | the text layer is readable |
| `Scanned` | few or no glyphs against high image coverage |
| `NearBlank` | next to no glyphs and an image under half the page |
| `BrokenText` | glyphs resolve to unusable Unicode |
| `Regions(n)` | *n* raster regions the text layer does not cover |

## Region routing and the glyph void

Below the page level, a raster region routes when a pixel probe reads it as
text or table structure — **gated on a glyph void**. A region the text layer
already covers never routes, however picture-like it looks.

The void is measured on the page as drawn, before analysis cleans its glyphs
(a word drawn twice to look bold, an underline struck over a word) and before
OCR words are laid on it, and the page keeps that routing. The routed regions
are numbered on it (`r0`, `r1`, …), and the number names a region's crop, its
OCR word file, a model's reading of it and its splice anchor alike. Routed again after cleaning, a
caption drawn twice over a capture would count half its glyphs, the capture
would become one more region, and every region after it would be renumbered.

This gate is what makes region routing safe: because our text is never
replaced, a routed region can only *add*. The tier is therefore
recall-oriented, biased toward routing when uncertain, since the cost of a
false positive is money and the cost of a false negative is missing content.

## Two calibration facts

Recorded for whoever tunes this next, because both cost real time to discover:

**Image coverage alone misroutes.** Decorative full-page backgrounds score
0.87–0.97 deterministically while presenting as near-total image coverage.
Coverage is a necessary signal, not a sufficient one.

**Pixel statistics cannot predict whether a benchmark's ground truth
transcribes an image's content.** The texty-row fractions of documents where
routing helped and documents where it did not overlap completely. That is a
property of the ground truth, not of the PDF — so it is not learnable from the
file, and chasing it is wasted effort.

## Not a triage processor

A deliberate non-goal: the router does **not** replicate the large
constant-tuned triage stages some engines carry — 1,100 lines of thresholds
fitted to one corpus, with several signals commented out after they backfired
(one documented in its own source as having caused 19 false positives, 28.4%).

The signals here are few, measured, and each has a stated failure mode.

## Rates

| corpus | routed |
|---|---|
| opendataloader-bench (adversarial) | 22% of pages |
| a born-digital sample (datasheets, CJK) | 2.2% of pages |
| a harder sample (scans, forms, newsprint) | 2.4% of pages |

An order of magnitude apart, which is the point: **the rate is a property of
your documents, not of the router.** Measure it with `fdoc triage` before
sizing any deployment, because it is the number that prices everything.

## Image coverage alone never routes a page

The two scanned-page verdicts both require an absence of readable text, not
the presence of an image:

| verdict | condition |
|---|---|
| `Scanned` | an image over half the page, and under 100 glyphs — none at all, or a stamped header, a page number |
| `NearBlank` | under 20 glyphs and an image, but less than half the page of it — a logo on a separator sheet, a signature |

The scan rule is checked first. Both verdicts send the page to the same
reader, so the order only names the reason, and the reason is what a caller
sorts by: `fdoc triage`, [`doc:unreadPages`](../formats/doco.md) and the
crop manifest's `scanned` / `near_blank` all carry it, and a caller sends
scans to an OCR engine and may skip a sheet with a logo on it. Checked the
other way round, as it once was, every image-only scan came out `NearBlank`.

Requiring the absence of text matters because a page can be entirely image
and entirely readable. Digitized newsprint is typically a full-page photograph with a
complete text layer laid over it: `img=1.00`, tens of thousands of glyphs, and
nothing to gain from a model. It reports `deterministic` and routes nowhere,
because the glyph void — not the image — is what the router is looking for.

Route on image coverage instead and that page costs a VLM call per page, for
output worse than the text layer already sitting there.
