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
| `ocr` | assembled by the deterministic engine from [OCR words](../integration/sidecar-formats.md#4-ocr-words----ocr-words) |
| `markdown` / `html` / `docx` / `pptx` / `xlsx` / `xls` / `xlsb` / `doc` / `ppt` / `odt` / `ods` / `odp` / `rtf` / `csv` / `vtt` / `srt` / `eml` / `msg` | the corresponding reader |

So `provenance` answers "which reader", and for PDF specifically it
distinguishes deterministic output from escalated output. An `ocr` element
was laid out like any other — its `evidence` is the geometric signal that
classified it — but its text was read from pixels.

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
| `metadata` | a PDF's `doco:Title` that the file's own title (`/Title`) names |
| `type` | a PDF's `doco:Title` set apart as the first page's title by its type, nothing doubting it |
| `subtitle` | a PDF's `doco:Subtitle`: a line set as the sure title's subtitle, directly under it |
| `kicker` | a PDF's `doco:Paragraph` set over its `doco:Title`, smaller than it: a category, series name or tagline |
| `header`, `footer` | a line repeated at the top or bottom of every page, kept once; see [furniture](../design/furniture.md#removed-from-the-body-kept-about-the-document) |

A PDF's `doco:Title` carries `metadata`, `outline` or `type`: how sure it
is. A line the first page sets as its title, but that a date, a section's
name or number, a company's name or a mark makes doubtful, is a level-1
`doco:SectionTitle` instead, and the document has no `doco:Title`.

The heading detectors form a ladder, tried in this order — the title,
`outline`, `numbering`, `bold`, `font-size` — each consulted only where the
one before it declined. So the value tells you how far down that ladder a heading came from,
and `font-size` is the weakest claim in the set.

From the escalation path:

| value | meaning |
|---|---|
| `route` | a routed region supplied this |
| `page-tier` | the whole page escalated |
| `table-confidence` | a table escalated because its reading is in doubt: structure that disagrees with itself, or a header other than one plain row |
| `table-missing` | a detector found a table where the grid pass found none |
| `layout-demoted` | a detector corroborated demoting this heading to prose |

And from the declared formats: `markdown`, `text`, `html`, `docx`, `pptx`, `xlsx`, `xls`, `xlsb`, `doc`, `ppt`, `odt`, `ods`, `odp`, `rtf`, `csv`, and
`header` and `footer` for what a Word file's page header and footer hold,
and `footnote`, `endnote` and `comment` for its notes and comments.
A PDF's annotations carry `annotation`, and a `.ppt` slide's speaker notes
`notes`. These are
the honest ones — an element marked `docx` was not inferred at all, and no
amount of escalation would improve it. `vtt` and `srt` are nearly so: the
file declares the words, the times and usually the speaker, and what the
reader decides is where one turn ends and the next begins — see
[Transcripts](../inputs/transcripts.md#turns). `eml` and `msg` likewise: the
reader decides where a quoted message begins, from the line that introduces
it. An email body sent only as HTML keeps the HTML reader's evidence,
`html`, under the email reader's provenance.

One value in a declared format is inferred: `keyword`, a heading an HTML page
or a text file shows without declaring it, read from a paragraph that is a
structural keyword and its number, with at most a short title (`CHAPTER IV`,
`Article 30`, `§ 7001. Definitions.`). Its level is inferred too, from how
the document nests those keywords; see [HTML](../inputs/office-and-web.md#html).

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

Elements with `provenance: "ocr"` have text derived from pixels too, but
each character keeps a position, spread evenly over the box the engine
drew: on a square sheet, within a character for an engine that boxes words,
within a few for one that boxes whole lines. On a skewed sheet lines are set
level, so highlights drift with the distance from the middle of the page.
An element is `ocr` as soon as one of its characters was read from pixels:
a line OCR read never joins a paragraph of the page's own text, so that
paragraph stays `rust`, and an element that still holds both (a table whose
rows mix them) is `ocr` whole.

Elements with `provenance: "vlm"` have text derived from pixels. Where a model
supplies element-level boxes rather than character-level ones, an
[overlay](../integration/entity-overlay.md) can highlight the containing
element but not the exact span. Degrade to the element box and indicate the
difference rather than drawing a rectangle you cannot justify.
