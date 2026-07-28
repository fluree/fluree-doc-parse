# The tier model

A document walks up the tiers only as far as it needs. Most never leave
tier 1.

| tier | adds | overall¹ | typical cost/document |
|---|---|---|---|
| 1 | deterministic extraction + layout | 0.889638 | **8 ms** (CPU) |
| 2 | layout-detector arbitration (headings, table regions) | 0.896694 | ~0.2 s (CPU) |
| 3 | deep reading of pixels-only content and doubted structure | **0.929711** | ~1.5 s/doc² |

¹ 200-document public evaluation corpus, measured 2026-07-28. Tier 3 places
first among the 17 engines scored, including ML and AI-routed systems; tier 1
alone would place third. See [the benchmarks](../benchmarks/README.md).

² Averaged over the corpus. 87 of 200 documents escalate; the other 113 stay
at tier 1's 8 ms. Median escalated document 1.7 s, worst 18.9 s. Tiers 1–2
are CPU-only.

The arbiter takes a generic second opinion on a table, and nothing is wired
into that slot by default. Supply one through
[`--structure-results`](../integration/sidecar-formats.md) if you have a
table-structure model worth consulting.

## The two principles

**Models arbitrate structure; the page tier owns its page.** Where the
deterministic pass produced a reading, a model's replaces it only when their
shapes disagree. Where the deterministic pass is what failed, the reading owns
the page — there is no good text to prefer to it.

**Escalation is earned, not configured.** There is no "quality" knob. Each
tier's own output is the sensor for the next: a page with no glyphs earns the
VLM, a grid whose detected structure disagrees with itself earns the
table-structure pass, a block a layout detector calls a title earns
promotion. Nothing escalates because a config file said so.

## What this costs

The asymmetry is the whole design. A deterministic page costs milliseconds; a
model page costs seconds of GPU. So the question is never "might the model do
better?" — on ordinary born-digital pages it does not — but "is the
deterministic output *unusable*?"

That happens for exactly two reasons, both measured rather than guessed:

- **The text is pixels.** A scanned page carries one large image and few or no
  glyphs. No analysis of absent glyphs can recover it.
- **The text is garbage.** Broken CID fonts produce glyphs whose Unicode is
  unknown or wrong. The layout may be perfect and the text still worthless.

On the (adversarial) bench corpus, 22% of pages carry GPU work. Your rate
depends entirely on your document mix — measure it with
[`fdoc triage`](../cli/triage.md) before sizing anything.

## Where CAD points the other way

Measured on a mechanical package drawing, the VLM returned the whole drawing
as one opaque image block and every dimension callout inside it as `<img>`.
The deterministic path extracts that same text *with per-glyph rotation*. For
engineering drawings, escalation **loses** information, so they stay local.

## Running the tiers

Two ways, same output. Configure a provider and `fdoc convert` does the whole
loop in one command — see [`fdoc config`](../cli/config.md). Or supply the
readings yourself as JSON sidecars, which is what keeps the binary usable
where nothing may leave the machine — see [Wiring the escalation
tiers](../integration/escalation-tiers.md).


Tier 1 is what you get by default and needs nothing.
