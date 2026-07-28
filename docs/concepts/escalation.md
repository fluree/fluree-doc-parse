# Escalation and arbitration

Two separate questions, often confused:

- **Escalation** — should a more expensive reader look at this?
- **Arbitration** — when two readers disagree, who wins?

## Escalation is earned

There is no quality setting. Each tier's own output is the sensor for the
next, and the decision is made from measurements rather than predictions.

The [router](../design/router.md) asks one question per page: *is the
deterministic output unusable?* Not "might a model do better" — on ordinary
born-digital pages it does not, and routing them would multiply latency for
nothing. Unusable has exactly two causes:

| cause | measured as |
|---|---|
| the text is pixels | glyph count against image coverage |
| the text is garbage | Unicode resolution rate (broken CID fonts) |

Below the page level, regions escalate on the same logic: a raster region that
reads as text or table structure under a pixel probe, gated on a **glyph
void** — a region the text layer already covers never routes.

Tables escalate on a third signal: a grid whose detected structure disagrees
with itself (fragmented columns, merged rows). `fdoc triage` reports these as
`TABLE` verdicts.

## Arbitration: what a reading is allowed to replace

The answer differs by tier, and the difference is the point.

**Over a table or region, a model arbitrates shape.** Its reading replaces the
deterministic one only when their shapes disagree — a different row or column
count, a table where we found none. Where the shapes agree, deterministic text
wins, because it came from the glyph stream rather than from pixels, and a
reader that must *generate* text can substitute plausible characters. This was
observed directly: on a Japanese page the reading emitted `五十音顺` where the
source reads `五十音順` — a simplified-Chinese variant for the Japanese kanji.
Small, and exactly the class of error that poisons a knowledge graph.

**Over a whole page, the reading owns the page.** A page escalates because the
deterministic reading of it is the thing that failed — no usable glyph layer,
or a hierarchy resting on nothing but font size — so there is no good text to
prefer to it. Withholding the substitution would keep the reading that earned
the escalation.

That asymmetry is deliberate, and it is not a statement that models are
untrustworthy in general. Measured over the flagged-table population, the
reader now configured recalls **98.8%** of printed values and fabricates
**0.0%**, against the deterministic pass's 95.9% and 0.1% — better on both
axes. The shape rule exists because a *class* of reader is not like that:
the same measurement puts two candidates at 14.5% and 16.7% fabrication, and
a deployment may still point at one.

What no tier extends is trust in the transport. A truncated response or a
blocked candidate produces a short reading that looks complete, which has
nothing to do with the model's quality — see [the completeness
floor](#the-completeness-floor).

## The three-way veto

Tier 5 adds a third independent reader, and with three opinions the rule
becomes majority rather than deference:

```
grid geometry  ×  table-structure model  ×  VLM
```

When two readers agree against the deepest model, **they win**. The deepest
model is not the most trusted one; it is the one most likely to have
hallucinated. This is what took the cascade to first place among the 17 engines
evaluated. Its marginal value is now small — +0.000319 over the same stack
without the veto — because the readings it arbitrates got better. See
[the scoreboard](../contributing/scoreboard.md) for the current rung-by-rung
numbers.

## Promotion-only for headings

Layout-detector arbitration (tier 2) can promote one of our short prose blocks
to a `doco:SectionTitle` where the detector saw a title box, but it never
demotes on the detector's word alone. The asymmetry is a cost calculation: a
detector false positive costs one split section, while doing nothing costs the
status quo.

The one demotion allowed is *corroborated*: a heading of ours sitting inside a
figure, chart or table title box is caption-like, and becomes a
`doco:Paragraph` with `evidence: "layout-demoted"`. Demoting on our own
judgement alone costs more than it gains; requiring a second independent
reading confines it to blocks both readers call float furniture.

## Where escalation is wrong

For mechanical drawings it loses information. On a package outline, a VLM
returns the drawing as one opaque image block and every dimension callout
inside it as `<img>`, while the deterministic path extracts that text with
per-glyph rotation. The arrow points the other way, and CAD stays local.

## Seeing it

```bash
fdoc triage report.pdf     # per-page verdicts and the signals behind them
fdoc triage ./corpus/      # the escalation rate over a directory
```

See [`fdoc triage`](../cli/triage.md) and, to actually run the tiers,
[Wiring the escalation tiers](../integration/escalation-tiers.md).

## The completeness floor

A page reading replaces a whole page, so the one thing it must be is a reading
of *that* page. Two checks stand between the wire and the substitution, and
neither is about how good the model is.

The reader refuses a response whose `finishReason` is anything but a normal
stop. `MAX_TOKENS` means the output budget ran out mid-page, and the result is
half a reading that looks exactly like a complete reading of a shorter page.
A blocked or absent candidate is refused the same way.

The arbiter then measures how much of the page's letter mass the reading
carries, and keeps the deterministic elements below **0.5**. Letters rather
than words, because the page text this compares against sets word boundaries
from advance gaps and fuses prose into runs like `SemanticSearchPack:Value` —
a word comparison scores a perfect reading near zero. Counting letters is
indifferent to spacing, reflowing and reordering, which is what a good reading
does on purpose. Every committed page reading measures at the cap; a page with
no glyph layer cannot be judged at all and is never rejected for it.
