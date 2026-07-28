# Sidecar formats

The exact file contract the [escalation tiers](escalation-tiers.md) read.
Three directories, three shapes.

## Crop names

Model readings are keyed by a **crop name**: `p{page}_{tag}`, where the tag is

| tag | meaning |
|---|---|
| `r{i}` | routed region *i* on that page |
| `t{i}` | table *i* on that page |
| `n{i}` | layout-found insert *i* (a table the grid pass missed) |
| `full` | the whole page |

`page` is 0-based. `fdoc dev render-routed` emits PNGs named
`{stem}_{crop}.png` and a manifest tying each back to its source.

## 1. Layout boxes — `--layout-boxes`

**Filename:** `{stem}_p{page}_page.json`

One file per page. An array of detector boxes over the **2× page render**, so
coordinates are in pixels at twice PDF units (`fdoc` halves them on read).

```json
[
  { "label": "paragraph_title", "score": 0.94, "box": [136, 370, 890, 402] },
  { "label": "table",           "score": 0.99, "box": [120, 500, 1050, 1400] }
]
```

| field | meaning |
|---|---|
| `label` | detector class (see below) |
| `score` | confidence; **boxes below 0.6 are ignored** |
| `box` | `[x0, y0, x1, y1]` in 2×-render pixels |

Labels that do something:

- **title family** — `paragraph_title`, `doc_title`, `title`. A title box
  covering one of our short prose blocks promotes it to a
  `doco:SectionTitle`.
- **caption family** — `figure_title`, `chart_title`, `table_title`,
  `figure_caption`, `chart`, `image`, `figure`. A heading of ours sitting
  inside one of these is demoted to a `doco:Paragraph`.

Everything else is ignored. Promotion is one-way except for that corroborated
demotion — see [Escalation and
arbitration](../concepts/escalation.md#promotion-only-for-headings).

## 2. Model readings — `--tier-results`

**Filename:** `{stem}_{crop}.json`

An array of pages, each carrying a `parsing_res_list`.

```jsonc
[{
  "input_path": "…/01030000000005_p0_r0.png",
  "width": 648,
  "height": 453,
  "parsing_res_list": [
    { "block_label": "table",
      "block_content": "<table><tr><td>…</td></tr></table>",
      "block_bbox": [133, 160, 1056, 1290],
      "block_polygon_points": [[133,160],[1056,160],[1056,1290],[133,1290]],
      "block_order": 2 }
  ]
}]
```

`fdoc` reads only three fields per block:

| field | required | use |
|---|---|---|
| `block_label` | yes | `table`, `text`, `image`, `footer`, `number`, … |
| `block_content` | yes | the reading; HTML for tables |
| `block_order` | no | sort key; blocks without it sort last |

Everything else may be present and is ignored, so a richer producer needs no
trimming.

**Table content is HTML.** Its `<tr>`/`<td>` structure is what gets compared
against the deterministic grid's shape — row and column counts — to decide
whether the model's reading replaces ours.

## 3. Table-structure readings — `--structure-results`

**Filename and shape:** identical to `--tier-results`.

It is a separate directory because it is a separate *opinion*. Supplying both
gives three independent readings of a table — drawn grid geometry, the
structure model, and the VLM — which is what enables the [three-way
veto](../concepts/escalation.md#the-three-way-veto). Two agreeing readers
overrule the deepest model.

## Worked example

```bash
stem=report

# crops + manifest
fdoc dev render-routed $stem.pdf ./crops

# your service writes:
#   ./layout/report_p12_page.json     (detector boxes for page 12)
#   ./vlm/report_p12_t0.json          (VLM reading of table 0 on page 12)
#   ./structure/report_p12_t0.json    (structure model, same crop)

fdoc convert $stem.pdf -f doco \
  --layout-boxes ./layout \
  --tier-results ./vlm \
  --structure-results ./structure
```

Any file that is missing, unreadable or not valid JSON is skipped silently —
that tier simply does not fire for that crop. This is deliberate: a partial
GPU batch degrades to a lower tier rather than failing the document.

## Reference caches

`eval/vlm-cache/`, `eval/layout-cache/`, `eval/structure-cache/` and
`eval/cascade-cache/` hold real files in exactly these formats, committed so
every published score reproduces without a GPU. They are the best available
specification — when in doubt, look at one.
