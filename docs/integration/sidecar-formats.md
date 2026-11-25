# Sidecar formats

The exact file contract the [escalation tiers](escalation-tiers.md) read.
Four directories, four shapes.

## Crop names

Model readings are keyed by a **crop name**: `p{page}_{tag}`, where the tag is

| tag | meaning |
|---|---|
| `r{i}` | routed region *i* on that page |
| `t{i}` | table *i* on that page |
| `n{i}` | layout-found insert *i* (a table the grid pass missed) |
| `full` | the whole page |

`page` is 0-based. `fdoc dev render-routed` emits PNGs named
`{stem}_{crop}.png` and a manifest tying each back to its source, with the
`reason` each crop was asked for
([escalation tiers](escalation-tiers.md#producing-the-crops)). A `full`
reading replaces its page whatever the reason, so a reader that does not see
structure should skip the crops asked for on a heading, figure or column
doubt.

## 1. Layout boxes — `--layout-boxes`

**Filename:** `{stem}_p{page}_page.json`

One file per page. An array of detector boxes over the **2× page render**, so
coordinates are in pixels at twice PDF units (`fdoc` halves them on read).
[Producing layout boxes](escalation-tiers.md#producing-layout-boxes) renders
the pages and runs a detector over them.

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

The VLM reading need not come from `--tier-results`: the veto applies the
same way to a model `fdoc convert --escalate` calls in the run. There it
rests on the structure reading and the grid alone, both known before the
model is called, so no table crop it vetoes is sent to the model. A page
read whole still shows the model its tables, and its reading replaces the
page, grids and all.

## 4. OCR words — `--ocr-words`

**Filename:** `{stem}_p{page}_full.json` for a whole page, `{stem}_p{page}_r{i}.json`
for routed region *i* — the names of the crops `fdoc dev render-routed`
writes, with `.json` for `.png`. Other tags are ignored: a table crop is a
question about structure, and an OCR engine reads text.

A page's regions are numbered once glyphs drawn twice at one spot (faux bold)
count once and a watermark laid over the text is taken out, as the analysis
and its anchors number them: a label in faux bold over a capture covers it as
drawn, not once cleaned, and `r0` is the same region for the crop, the
reading and the anchor.

An OCR engine's words, each with its box. Unlike a model reading, which
replaces elements, these become **glyphs** before analysis, so a scanned page
goes through the same line, column, table and heading assembly a born-digital
page does, and every character keeps a position for
[highlighting](entity-overlay.md).

```json
{
  "width": 1224, "height": 1584,
  "engine": "pp-ocrv6-small",
  "words": [
    { "text": "Account", "box": [144, 200, 284, 226], "conf": 0.98 },
    { "text": "Date of birth", "box": [144, 260, 404, 286], "angle": 0.0, "conf": 0.95 }
  ]
}
```

| field | required | use |
|---|---|---|
| `coords` | no | `px` (default): pixels of the image read. `page`: units of the page as displayed (after `/Rotate`), y down — not PDF user space, whose y runs up |
| `width`, `height` | with `px` | size of the image read, which shows the crop |
| `engine` | no | recorded for whoever compares two engines |
| `words[].text` | yes | a word, or a whole line: a space inside it is a space |
| `words[].box` | yes | `[x0, y0, x1, y1]`, y down: the word's own extent |
| `words[].angle` | no | baseline direction, degrees counter-clockwise; the word is `box` turned about its center. Absent means level, unless the box is much taller than wide and holds three characters or more: that is text set sideways |
| `words[].conf` | no | the engine's confidence, 0-1 |

The format is stable: `--ocr-words` (`FDOC_OCR_WORDS`), the file names, the
fields above and their units. A release changes or removes one only as a
breaking change; a new optional field may be added.

**The crop, in pixels.** A `px` reading of `full` covers the whole page; one
of `r{i}` covers the region plus a 6-unit margin, clipped to the page —
what `render-routed` draws. Any resolution works: `width` and `height` scale
the reading onto the crop, so an engine may read the same crop rendered
finer, as `render-routed --stored-resolution` renders it. The image embedded
in the page is not that crop: it may cover only part of the page, and a
region crop includes its margin. On a page turned to read upright
(`/Rotate`, or text drawn sideways), a region crop is cut where the region
shows on the page as displayed, which is where this mapping places it.

**What the page takes.** Only what the router sends a reader:

- a `full` reading for a page routed `Scanned`, `NearBlank` or `BrokenText`
  (whose garbled glyphs it replaces);
- a `full` reading for a page with unread regions, keeping only the words
  inside them — the crop `render-routed` writes when the page is also asked
  for on a doubt, which supersedes its region crops;
- an `r{i}` reading for one of the page's unread regions, keeping only the
  words whose center falls inside the region, not its margin.

A page's unread regions are its raster images that look like text, whose own
pixels can hold a line of it (10 px on the short side: a checkbox drawn from an
8 × 8 mask or a rule stretched from a 1 × 1 one is not a region), and that
the text layer does not cover; an image drawn twice in one place is one
region. The router sends a model only those over a
tenth of the page, a price rather than a property of the pixels: an OCR
engine is cheap per pixel, and a capture an inch high still holds a date. So
OCR reads them all, the routed ones first, numbered as the router numbers
them (`r0`, `r1`, …), then the small ones after them: a model's `r1` and an
engine's `r1` are the same region. `render-routed --ocr-regions` crops the
small ones too, with the reason `small_region`.

A whole-page reading for any other page is refused with a warning, so the
text layer of a born-digital page is never replaced by a reading of it — a
page asked for on a doubt about its headings included. Region readings are
looked up only for the page's unread regions. A word over a glyph the
page already has is dropped, so a stamp the text layer carries is not read
twice.

Only those names are looked up — `full` on every page, `r{i}` for each unread region — so a batch can share one directory. The elements these words make up are marked `provenance: "ocr"`.

**What to read.** The `render-routed` manifest says why each crop exists
([escalation tiers](escalation-tiers.md#producing-the-crops)). An OCR engine
needs to read only the crops whose `reason` is `scanned`, `near_blank`,
`broken_text`, `region` or `small_region`: those are the ones whose readings
the page takes. [`fdoc ocr`](../cli/ocr.md) reads a crops directory that way.
A `heading_doubt`, `figure_doubt` or `column_doubt` page has all its text,
and a `table` or `table_missing` crop asks about structure; read, they would
only be refused or ignored. Skipping `table_missing` has the one known gap
the escalation tiers name: a raster table too small for the router to send a
reader is boxed by the layout detector alone, and its text stays unread until
a reader that sees structure takes the crop.

**How words become text.** Each box is chained to its nearest neighbour on
the right — same line, comparable height, no wider apart than a word space —
and each chain is one run of words, separated by spaces (none between
characters of a script written without them, CJK or Thai). A run never crosses
a column gutter or the gap between two table cells, so columns and cells
still read apart. The run's size comes from its boxes: a detector box (PP-OCR)
is taken as 1.3 em tall whatever its letters; a box around the ink
(Tesseract, hOCR, ALTO) as tall as its letters reach, so `our` and
`Payments` read at one size. Which kind a document's readings hold is
decided once, over all of them: neighbouring ink boxes differ in height as
their letters do. Punctuation alone in a box takes the size of its run. The sheet's
skew, stated by `angle` or measured between neighbouring words, is taken out
first, and the runs of one visual line share one baseline. Lines are set
level, so on a skewed sheet text sits off where it is drawn by its distance
from the middle of the reading times the skew's tangent. A word 7.5° or more off the sheet's angle is laid on its own: along its angle when it runs sideways (about ±90°), level about its center otherwise. An engine that boxes
whole lines square, with no `angle`, on a skewed sheet gives nothing to
measure the skew by: give the angle. A sheet fed upside down, whose words
the engine states at about 180°, is read in the frame where they run
upright, as a page drawn upside down is when it is extracted; over a page
that has its own text, such a reading is refused.

Two runs of one line boxed into each other by up to 1 em, as an engine does
when it splits a long line, are set one after the other, the second squeezed
into what is left of its box. Two stacked lines whose boxes overlap, such as a
form's label set tight above its value, are set no larger than their spacing
allows: three quarters of the distance between their baselines. Otherwise line
assembly takes the smaller one for the larger one's superscript and interleaves
their characters. A run too large to be read as a script of the text it
passes over (over 2.25 times its size: a stamp, a display line) keeps its
size and moves its baseline instead, the least it must, to sit clear of
every line under it.

Each character takes an equal share of its box, so a highlight is as close
on a square sheet, as the engine's boxes are fine: within a character for word boxes, within a
few for line boxes.

**What OCR read is not escalated again.** A region a reading filled keeps
its number among the page's regions, so a model reading of another region
still lands where it belongs, and is not sent to a model. A page read whole
asks a model for nothing — neither its whole page nor its tables: escalating
past OCR is a decision of its own. Such a page is not reported unread unless
its text stays under the floor every page is held to (40 characters): then it
is listed in `doc:unreadPages` as `OcrSparse`, with a warning on stderr, since
an engine that gave two words for a dense scan has not read it. A reading
that took no word reads nothing: the page
stays as it was, routed and reported unread. So does a whole-page reading
most of whose words land nowhere: it is refused. When a page has both a
`full` reading and `r{i}` readings, the whole-page one is taken first, and a
region reading is refused where the whole-page one took words in its region
(or, on a page whose text is all pixels, anywhere); the others stand.

**OCR text is never taken for page furniture.** Furniture — running
headers, footers, watermarks — is found by repetition across pages, and on a
scan the same content repeats for its own reasons: a fax that sends one ID
card on every page, a form copied with the same note down its margin. OCR
lines stay in the body, page numbers included.

Right-to-left scripts are laid out left to right: their words come out in
reverse order.

A file that does not parse is reported on stderr (unless `--quiet`), not
skipped silently: it comes from your pipeline, not from a partial batch. A
`--ocr-words` directory that does not exist is an error.

```bash
fdoc dev render-routed scan.pdf ./crops --ocr-regions --stored-resolution
# the crops whose readings the page takes
jq -r 'select(.reason | IN("scanned", "near_blank", "broken_text", "region", "small_region")) | "./crops/" + .png' \
  ./crops/manifest.jsonl > ./crops/for-ocr.txt
your-ocr $(cat ./crops/for-ocr.txt) --out ./words   # one {stem}_p{N}_{tag}.json per crop
fdoc convert scan.pdf -f doco --ocr-words ./words
```

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

For the model-tier directories (1-3), any file that is missing, unreadable
or not valid JSON is skipped silently — that tier simply does not fire for
that crop. This is deliberate: a partial
GPU batch degrades to a lower tier rather than failing the document.

## Reference caches

`eval/vlm-cache/`, `eval/layout-cache/`, `eval/structure-cache/` and
`eval/cascade-cache/` hold real files in exactly these formats, committed so
every published score reproduces without a GPU. They are the best available
specification — when in doubt, look at one.
