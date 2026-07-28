# Pricing a deployment

The question that decides an architecture is not "how accurate is it" but
"how much of my corpus needs a GPU". `fdoc triage` answers that from your
documents, before you provision anything.

```bash
fdoc triage ./corpus/
```

```
6 files, 223 pages, 5 routed (2.2%), deterministic parse 741.2ms total
```

Two numbers matter. The **parse time** is what the CPU tier costs — no model,
no network. The **routed percentage** is the share of pages a model tier would
have to look at, and therefore the share of your corpus that carries GPU cost
at all.

At 2%, a corpus of a million pages sends twenty thousand to a model. At 40%,
the arithmetic is different enough to change the design. Measure it rather
than assume it: the rate is a property of your documents, not of the parser.

## What is being measured

Every page is checked for two failures, neither of which is a guess about
whether a model *might* do better:

| verdict | cause |
|---|---|
| `Scanned` / `NearBlank` | the text is pixels — glyph count against image coverage |
| `BrokenText` | glyphs whose Unicode cannot be trusted (broken CID fonts) |
| `Regions` | a raster region the text layer does not cover |

```
ti_tps5430   ROUTE   148.2ms   p33 Regions(1) glyphs=196 img=1.00 [(75, 255, 536, 489)]
jp_stat      deterministic   2.1ms   p1 glyphs=333 unicode=1.00 img=0.00 n_img=0 boxes=[] page=595x841
```

`unicode=1.00` means every glyph resolved; `img=1.00` on that region means it
is fully covered by an image with no text underneath it. Those are
measurements, which is why escalation can be earned rather than configured —
see [Escalation and arbitration](../concepts/escalation.md).

## Tables are priced separately

The same run reports grids that disagree with themselves:

```
267 tables, 10 with disagreeing structure (3.7%) — model-tier candidates
0 of 6 documents have a doubtful heading hierarchy (0.0%)
```

Table escalation is per-table, not per-page, and it is usually the larger line
item. A few percent of uncertain tables costs almost nothing; a financial
corpus where a third of them are uncertain prices very differently from a
corpus of datasheets.

## Turning it into a number

```bash
fdoc triage ./sample/ 2>&1 | tail -1 |
  sed -E 's/.* ([0-9]+) pages, ([0-9]+) routed.*/pages=\1 routed=\2/'
```

Sample a few hundred representative documents rather than the whole archive —
the rate converges quickly, and triage itself is not free on a million files.

Then:

```
GPU pages   = corpus_pages × routed_rate
GPU tables  = corpus_tables × uncertain_rate
```

The [tier model](../getting-started/tiers.md) gives the per-page costs to
multiply through: roughly 0.2 s for layout detection, 0.7 s per table for the
structure pipeline, and 2–8 s per table for a VLM.

## What the rate does not tell you

A 0% escalation rate does not mean the output is perfect — it means nothing
was *unreadable*. What the deterministic tier still gets wrong is structure
decisions: heading levels, table boundaries. Escalation buys structural
arbitration, not characters. See [the tier
model](../getting-started/tiers.md) for what each tier adds.

The reverse also holds. A high rate on mechanical drawings is a signal to
route *less*, not more: the VLM returns a CAD drawing as one opaque image with
its dimension callouts as `<img>`, while the deterministic path extracts that
text with per-glyph rotation. See [where escalation is
wrong](../concepts/escalation.md#where-escalation-is-wrong).

## Before you provision

```bash
# pages needing a model — one ROUTE line can list many, so count the verdicts
fdoc triage ./corpus/ | grep -oE 'p[0-9]+ (Scanned|NearBlank|BrokenText|Regions)' | wc -l

# tables needing one
fdoc triage ./corpus/ | grep TABLE
```

If both are near zero, you do not need the escalation stack at all — ship the
binary and skip [the entire integration
chapter](../integration/README.md). That is a legitimate outcome and the most
common one for born-digital corpora.
