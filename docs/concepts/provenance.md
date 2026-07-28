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
| `markdown` / `html` / `docx` / `pptx` | the corresponding reader |

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

And from the declared formats: `markdown`, `html`, `docx`, `pptx`. These are
the honest ones — an element marked `docx` was not inferred at all, and no
amount of escalation would improve it.

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
