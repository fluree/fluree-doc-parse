# Where our output differs from the reference

A benchmark scores agreement with an answer key, not correctness. Usually
those are the same thing. Sometimes they are not, and where the engine reads a
document better than the reference records it, the metric charges us for the
difference.

This page is the list. It exists because the alternative — quietly matching
the answer key — would make the engine worse at the job it is actually for.

## Links the reference does not carry

**Cost: 0.0019 of the deterministic overall, across 21 of 200 documents.**

A PDF carries its hyperlinks as annotations beside the content stream, not as
glyphs. A parser that ignores them sees the anchor's words and nothing else,
so a linked citation extracts identically to an unlinked one. We
[read them](../formats/markdown.md#links) and emit them; the ground truth,
transcribed from the visible page, has no link markup anywhere in the corpus.

Every character of markup we add is therefore counted as an insertion error.

Document 094 asks a question about a news event:

```
reference   After reading this account of what happened at the march…
ours        After reading [this](https://www.nytimes.com/2019/01/20/us/…) account…
```

The word *this* is a live link to the article the question is about. Losing it
loses the question's referent. The reference is not wrong about what the page
*says* — it is silent about what the page *does*, and NID scores the
difference as noise. That document costs 0.0507.

Document 100 is the expensive case, because the address is long:

```
reference   (Yoeli et al. 2013)
ours        [Yoeli et al. 2013](https://www.jstor.org/stable/42706676?refreqid=…)
```

A citation with a resolvable source is a different fact from a citation
without one, and for anything that loads a document into a graph it is one of
the more valuable facts on the page. It costs 0.0421 there.

The trade in full: 21 documents change, every one of them downward, summing to
0.385 — which over 200 documents is 0.0019 on the overall. Tier 1 goes from
0.891562 to 0.889638 and stays third, ahead of the next engine by 0.0046. Tier
3 goes from 0.931645 to 0.929711 and stays first by 0.023.

We would make the same trade again. A link is content.

## Chart labels paired with their values

**Not currently counted, because it appears where a page escalates.**

A bar chart's labels and values are separate text runs on the page, and their
draw order is not a reading of the chart. Transcribed in page order they come
out detached:

```
reference   Upstage / 0.4048 / Graph-RecSys
ours        UpStage  Graph-RecSys 0.4048
```

The reference records the order the page draws them in. A reader that can see
the drawing pairs each label with the value beside it, which is the only
reading of a chart anybody can use. On document 183 that pairing scores
−0.2472 against the key.

This is why the engine
[marks figure fragments rather than reordering them](../formats/json.md#figures-come-in-groups)
in the deterministic path: their sequence is real information about the page,
and only a reader that can see the drawing may assert a different one.

## Composed pages, where the metric cannot see the gain

**Cost: none. And that is the finding.**

A page whose text sits inside its drawings is a designed layout, and it now
earns a reading. Six documents in the corpus qualify; escalating all six moved
the overall score by nothing at all — not a rounding difference, the same
number to sixteen digits.

The readings are plainly better. One page's chart extracts deterministically
as draw-order rubble with a fabricated value in it:

```
reference   100  22  3780  32  20  60  17  30  40  57
ours        July 2020: No Challenge 32, Small Challenge 30, Big Challenge 38
```

`3780` is not on the page. It is the segment label `37` fused with the axis
tick `80` — two numbers that happen to be drawn near each other. The reading
instead pairs every value with its category and series, which is what the
chart says.

Another page's bar chart has no glyph layer at all: fifteen data values that
the deterministic pass cannot see and the reading recovers in full.

The metric registers none of this, because its answer key was itself
transcribed in page order — so a reading that pairs a label with its value
agrees with the key no more often than one that does not. The score is
neither evidence for this change nor against it, which is worth stating
plainly rather than quoting "no regression" as though it were a result.

## How to read this list

It is short on purpose, and it is not a general excuse. Two rules keep it
honest:

- **An entry needs the document, the two readings, and the cost.** "The
  benchmark is unfair" is not an entry.
- **A regression is a regression until it is on this page with evidence.**
  Every other movement in a score is treated as a defect in the engine.
