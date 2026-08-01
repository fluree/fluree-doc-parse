# Tables

The hardest target, and the main reason the [model
tiers](../concepts/escalation.md) exist.

## Rule-less tables are usually not rule-less

The founding observation, found by dumping the geometry of a table that
*looks* to have no vertical rules rather than trusting its appearance: its
grid is drawn as **per-cell segments**, one short rule per cell edge.

```text
Horizontal axis=77.5  (68,77)-(183,78)
Horizontal axis=77.5  (183,77)-(297,78)
Horizontal axis=77.5  (297,77)-(412,78)
Horizontal axis=77.5  (412,77)-(528,78)
```

Read individually those are four short lines, and nothing looks like a table.
**Clustered by axis position** they are one horizontal grid line, and their
endpoints — 68, 183, 297, 412, 528 — are exactly the four column boundaries.

Every engine benchmarked failed on that table while working from a grid fully
present in the file: one recovered 3 of 4 columns, one lost the header row,
one collapsed it to a single column. Clustering per-cell segments by axis
recovers it exactly.

```bash
fdoc dev rules  report.pdf 12    # the raw geometry
fdoc dev tables report.pdf 12    # the grid that came out of it
```

## Detection strategies

Multiple passes over one parse, rather than multiple services:

- **Stroked rules** — drawn lines, clustered by axis as above.
- **Filled rects** — thin filled rectangles used as rules.
- **Alignment clustering** — column inference from the x-positions of text,
  for tables with genuinely no drawn geometry.

Alignment-based detection is **required to be corroborated** by drawn
geometry. Ungated it measured TEDS 0.488 → 0.526 but NID 0.868 → 0.839 and
MHS 0.528 → 0.506 — net worse, because it found tables in ordinary aligned
prose. `fdoc dev aligned` shows what corroboration rejected.

## Grouping and trimming

Where a ruled grid can be grouped more than one way, the grouping chosen is
**whichever survives trimming** — the interpretation that still looks like a
table after empty edge rows and columns are removed.

A page's ruling is not one table, and distance is a poor way to tell where one
ends. Two tables are split apart at a row band **no vertical rule crosses**:
the whitespace between two ruled boxes. The band has to sit directly between
two crossed bands, because in a table ruled with horizontals alone every band
is uncrossed, and splitting on those alone turns a ruled table of contents into
one table per entry.

## Banners

A letterhead is a single row of fields ruled side by side, and it is a table.
Treating one row as too few left its ruling to be grouped with the table below
it, whose columns then cut the letterhead's text mid-word — a document number
split across two cells, a company name losing its second line.

One row counts as a table only when a **closed box** encloses it: three or
more columns, and a vertical rule standing *inside* the box's own x-range. The
enclosure is the whole test. A caption's underlines — a short rule beneath each
word — produce the same two baselines and the same scatter of column edges, and
on boundaries alone are indistinguishable from a letterhead; nothing encloses
them. Nor does a decorative rule in a margin vouch for a row it never touched.

## Merges

Spanning cells are detected from which boundaries are *not* drawn across a
row, and reported as `merged_down` / `merged_left` flags with the value in the
position where its text was laid out.

`cells` is deliberately left in that rowspan convention. Repeating a spanned
value across the positions it covers contradicts how a reference encodes a
rowspan, and measured worse against the benchmark's ground truth when it was
tried — Markdown is the projection the harness scores, so it prints the grid
as detected. Consumers wanting self-contained rows
[denormalize](../concepts/element-model.md#tables); the DoCO emitter already
does, and the XHTML emitter expresses the merges as `rowspan`/`colspan`
instead.

## Escalation signals

A table earns a model tier when its detected structure **disagrees with
itself**:

| signal | meaning |
|---|---|
| `Fragmented` | column boundaries are inconsistent between rows |
| `MergedRows` | rows do not segment cleanly |

```bash
fdoc triage ./corpus/
# 268 tables, 10 with disagreeing structure (3.7%) — model-tier candidates
```

That self-disagreement is a genuinely corpus-independent confidence signal:
it does not depend on a threshold tuned to one document collection, only on
whether two readings of the same geometry agree.

## Why tables still escalate most

Even at tier 5, tables are where the remaining error lives. Table content is
also what makes escalation expensive — a VLM generates it autoregressively, so
a dense table page costs the most seconds of anything in the pipeline. The
combination is why escalation is per-table rather than per-document.

## A drawn lattice is not a table

A designer's baseline grid is drawn with the same primitive a table's ruling
is. One deck's page carries nine full-width rules at a 21.7pt pitch and five
full-height rules at 45pt, and the grid built from them shredded two columns
of prose into a twelve-by-twenty-three "table" whose cells tore words in half
— `ope | n-architect | ure` for *open-architecture*.

Rules that span the full page at a constant pitch **on both axes** are dropped
before anything reads the geometry.

Both axes is the whole safety of it. Evenly spaced rules on one axis are an
ordinary table with equal rows, which is common — one corpus document has six
of them. A lattice that repeats down the other axis too, with every rule
crossing the entire page, is not something a table does: a table's ruling
spans the table. No document in the evaluation corpus matches the two-axis
signature, and adding the rule moved neither score.

The pitch is established from the longest even run and then applied to every
full-bleed rule on it, so a stray rule drawn across the lattice does not hide
half of it. A page border sits off the pitch and survives — it is nobody's row
boundary, but removing it is not this rule's business.
