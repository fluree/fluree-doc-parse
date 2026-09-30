# Provenance and evidence

Every element records **which engine produced it** and **which signal
classified it**. Routing is meant to be invisible in the sense that you do not
configure it — not in the sense that you cannot audit it.

```jsonc
{ "id": "elem-00001", "type": "doco:Paragraph",
  "provenance": "rust", "evidence": "layout" }
```

## provenance — which engine

| value | meaning |
|---|---|
| `rust` | the deterministic PDF engine |
| `vlm` | spliced in from a model tier |
| `markdown` / `html` / `docx` / `pptx` / `xlsx` / `vtt` / `srt` / `eml` / `msg` | the corresponding reader |
| `xml` / `json` | a [record](../inputs/records.md) in that notation, read by a source format |
| `axf` | a [media asset](../inputs/media-assets.md) |

So `provenance` answers "which reader", and for PDF specifically it
distinguishes deterministic output from escalated output.

**Where it appears:** `-f json` only. The [DoCO graph](../formats/doco.md)
carries `doc:evidence` but not the engine, so a ledger query can ask *which
signal* classified an element and not *which engine* produced it. Where you
need engine-level provenance in a graph, the `evidence` values `route` and
`page-tier` are the model-tier markers — every element bearing one came from
an escalated region.

## evidence — which signal

`evidence` names the detector that produced the classification. It is the
useful replacement for a confidence scalar: instead of `0.78`, you learn
*which* reasoning applied and can decide whether you trust that reasoning for
your documents.

Roughly in order of how often you will see them:

| value | meaning |
|---|---|
| `layout` | inferred from geometry — the default deterministic path |
| `fills` | the element sits inside a drawn chart or diagram |
| `rules` | drawn ruling lines defined the structure |
| `marker` | a list marker (bullet, number, drawn checkbox) |
| `bold` | heading detected by weight |
| `font-size` | heading detected by size rarity |
| `numbering` | heading detected by a section-numbering pattern |
| `outline` | the PDF bookmark tree named this heading |
| `title` | the document-title heuristic on page 1 |

The heading detectors form a ladder, tried in this order — `title`, `outline`,
`numbering`, `bold`, `font-size` — each consulted only where the one before it
declined. So the value tells you how far down that ladder a heading came from,
and `font-size` is the weakest claim in the set.

From the escalation path:

| value | meaning |
|---|---|
| `route` | a routed region supplied this |
| `page-tier` | the whole page escalated |
| `table-confidence` | a table escalated on self-disagreeing structure |
| `table-missing` | a detector found a table where the grid pass found none |
| `layout-demoted` | a detector corroborated demoting this heading to prose |

And from the declared formats: `markdown`, `html`, `docx`, `pptx`, `xlsx`. These are
the honest ones — an element marked `docx` was not inferred at all, and no
amount of escalation would improve it. `vtt` and `srt` are nearly so: the
file declares the words, the times and usually the speaker, and what the
reader decides is where one turn ends and the next begins — see
[Transcripts](../inputs/transcripts.md#turns). `eml` and `msg` likewise: the
reader decides where a quoted message begins, from the line that introduces
it. An email body sent only as HTML keeps the HTML reader's evidence,
`html`, under the email reader's provenance.

And from records and media assets: `declared`. The class of every element
is what a [source format](../inputs/records.md) declares its field to be: a
title because the format names the field as the title, a paragraph because
it names it as text. Nothing about it was inferred from the record. The
notation is in `provenance`, `xml`, `json` or `axf`, and the evidence is
the same for all three, because the same declaration decided. An asset read
with no declaration carries `declared` too: the reader then declares what
every asset has. As with a transcript, where a turn of a media asset ends
is the reader's decision; see
[Media assets](../inputs/media-assets.md#sections-and-turns).

## Reading it

```bash
# Which elements did a model contribute?
fdoc convert report.pdf -f json | jq '[.[] | select(.provenance=="vlm")] | length'

# What classified the headings?
fdoc convert report.pdf -f json \
  | jq -r '.[] | select(.type=="doco:SectionTitle") | "\(.evidence)\t\(.text)"'

# The same question against the graph
fdoc convert report.pdf -f doco \
  | jq -r '."@graph"[] | select(."@type"=="doco:SectionTitle")
           | "\(."doc:evidence")\t\(."rdfs:label")"'
```

`outline`-evidenced headings are the ones to trust most: they came from the
document's own bookmark tree rather than from a guess about font size. The
[headings design note](../design/headings.md) covers why that signal is
near-ground-truth, and why no other engine tested uses it.

## Asymmetry to plan for

Elements with `provenance: "vlm"` have text derived from pixels. Where a model
supplies element-level boxes rather than character-level ones, an
[overlay](../integration/entity-overlay.md) can highlight the containing
element but not the exact span. Degrade to the element box and indicate the
difference rather than drawing a rectangle you cannot justify.
