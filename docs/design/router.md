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
text or table structure ([below](#the-pixel-probe)) — **gated on a glyph
void**. A region the text layer
already covers never routes, however picture-like it looks.

The void is measured once glyphs drawn twice at one spot (faux bold) count
once and a watermark laid over the text is taken out, the two cleanings that
take glyphs off an image, and before any other cleaning or OCR words are laid
on the page; the page keeps that routing (`document::region_routes`,
`document::routing_pages`). The routed regions are numbered on it (`r0`,
`r1`, …), and the number names a region's crop, its OCR word file, a model's
reading of it, its splice anchor and its report alike. A caption drawn twice
over a capture is one caption to all of them: the capture is a region for
each, under one number.

This gate is what makes region routing safe: because our text is never
replaced, a routed region can only *add*. The tier is therefore
recall-oriented, biased toward routing when uncertain, since the cost of a
false positive is money and the cost of a false negative is missing content.

## The pixel probe

Whether an image's pixels look like text is decided by two rules, either
of which is enough (`extract::looks_like_text`; the second is
`fluree_doc_vision::texty::lines_of_text`):

- **ink on paper**: at least 55% of the pixels outside the middle half of
  the luminance range, a range of 64 levels or more, and 18% of the rows
  crossing its midpoint six times or more. A page of text, a table, a dense
  screen;
- **lines of text**: on the image's ground (its most common luminance, give
  or take 16, holding a quarter of the pixels at least), within a band 320
  pixels wide, a run of 4 to 80 rows framed by rows of bare ground (under 2%
  ink) whose ink falls in four runs or more across it, mostly narrow,
  covering a quarter of its extent. Two such lines, or one covering 8% of
  the image's height, hold text. A screen capture whose text is sparse or
  small: a few lines at the top of a wide, light screen, which the first
  rule takes for no text.

Measured on 761 synthetic images (screens light and dark, sparse captures,
dialogs, charts, image tables, photographed documents, labels; against
landscapes, foliage, faces, gradients, textures, logos and 160 crops of
photographs and illustrations), the first rule alone missed 151 of the 361
holding text, every sparse capture among them, and took 79 of the 400
holding none for text; with the second it misses 29 and takes 96, the
photographs it takes being the same. The second rule alone misses 61 and
takes 33: it misses text set densely, which the first finds.

Both rules look at the image at its own size: the decoder honours a target
size only for JPEG 2000, so a Flate or DCT image is probed at the
resolution it is stored at. The cost grows with the pixels and stays under
the decode's: about 0.6 ms for a 1366 × 768 capture, 5 ms for a page
scanned at 300 dpi, 55 ms for 100 million pixels.

## What the router declines is still listed

Two kinds of region in a glyph void get no model: a texty one under a tenth
of the page, and one a tenth of the page or more whose pixels the probe does
not take for text. A texty region under a tenth is small for a price, not
for its pixels: a model call costs the same for a logo as for a page. An
OCR engine runs locally and bills nothing, so it reads the small ones
(`small_region`); a model call is billed and sends the image off the
machine, so a model keeps the tenth. A region the probe does not take for
text is read by neither: the probe is where the router tells a photograph
from a capture, and what an engine reads in a photograph is noise.

What an engine reads in a region, routed or small, is still held to a noise
rule, a net behind the probe for a photograph or a gradient it took for
text: a mean confidence under 0.5, or under 0.8 with most of its words
fragments of one or two letters or figures, or one fragment, or one word
read under 0.8, boxed over nearly all of the region
(`ocr::MIN_PICTURE_CONFIDENCE`, `MIN_FRAGMENTS_CONFIDENCE`,
`NOISE_BOX_SHARE`); the region stays unread. A reading of a whole page is
not judged: a scan's lines are words.

Neither kind is dropped silently: unread, each is listed in
[`doc:unreadRegions`](../formats/doco.md#regions-nothing-read) as `Small`
or `NotTexty`, with its box, without a warning.

All of this needs the images' pixels, and decoding them is most of what an
image costs: extraction probes them only when something acts on the verdict
(`ExtractOptions::probe_images`; in `fdoc convert`, escalation, OCR, the
anchors, the layout sidecars or `--probe-images`). Unprobed, an image's
`texty` is `None`: the router routes none, no engine is offered any, and
each one the text layer does not cover, half an inch or more on its longer
side, is listed as `Unprobed`, at any share of the page, with one warning
that says how many and on which pages. A texty one under half an
inch on its longer side (`MIN_SMALL_REGION_SIDE`, 36 units: a checkbox, an
icon) is not listed, though an OCR engine is still offered it when its own
pixels hold a line of text: a stencil reads as text whatever it draws, and a
form would list every box. No region, listed or read, is an image under
10 pixels on its short side (`MIN_TEXT_LINE_PX`), however large it is
drawn: a checkbox drawn from an 8 × 8 mask or a rule stretched from a 1 × 1
one holds no line of text. An image drawn twice in one place is one region.

An image drawn again elsewhere, on its page or another, at the same size and
the same way up, is one reading: its copies are numbered as regions of their
own, but an OCR engine does not read them again, and each takes the reading
of the first (`ocr::copies`), its words laid where the copy is drawn and
judged there.
The image is told by its own bytes (a raster image's stream, a stencil's
mask up to 512 × 512 pixels), not by where it is drawn. A page of prose
whose 200 checkboxes are one 24 × 24 stencil drawn 200 times took 200 calls
of the engine, 20 s where the page without them reads in 0.2 s, for the same
output: it takes one, 0.26 s. What a copy's crop shows in its margin, apart
from the image, is not read again.

The tenth (`MIN_REGION_FRACTION`) is a constant, not a setting. It also
numbers the routed regions (`r0`, `r1`, …) that crop names, OCR word files
and model readings are looked up by, across `render-routed`, `fdoc ocr` and
`convert`; a setting that one command ran with and another did not would
send a reading to the wrong region without a word. A caller who wants the
declined ones read has them listed with their boxes, and an OCR engine reads
the small ones already, numbered after the routed ones in the order the page
places them.

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
