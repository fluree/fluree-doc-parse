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
doubt. An `r{i}` reading of a region under half its page, a screen pasted
into a page of text, has its headings (`doc_title`, `paragraph_title`, a
Markdown `#` line) read as paragraphs: they are the screen's, not the
document's. A region that, with the images it abuts, covers half its page
or more keeps them, as a page read whole does: a scan stored in strips is
one picture. So does the `full` reading of a page near blank but for
such a screen (one alone on its page with its caption), where OCR's reading
demotes the headings whose words land in the screen: a model's reading has
no positions, and its text cannot tell the screen's title from the page's
own, a cover's title drawn in outlines (no text layer) or a caption that
repeats the screen's name.

An image a DOCX, an XLSX or an e-mail places in its flow has no page: its
crop name is `m{n}` (`m{n}_{k}` for a figure's later images), the name
`render-routed` gives its crop, `{stem}_m{n}.png`. Its reading is read as a
whole page's is, its headings read as paragraphs as a pasted screen's are,
and placed after the image's figure.

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
question about structure, and an OCR engine reads text. A bare image also
takes `{stem}.json`, the name `fdoc ocr` gives the words of an image file;
of the two, it reads the one sized as it is. A DOCX's pictures are read from
`{stem}_m{n}.json`, for the figure `figure-0-{n}`, and `{stem}_m{n}_{k}.json`
for the figure's `k`-th image after the first; their text follows the figure.
An image several figures draw is read once, from the name of the first figure
drawing it, and its text follows each of them (see [Office files](../inputs/office-and-web.md)).
So are the pictures anchored to an XLSX's cells, for the figure
`figure-{sheet}-{n}`, and the images an e-mail shows in its body (see
[E-mail](../inputs/email.md#images-in-the-body)). Readings are looked up by
stem, so two inputs of one stem in a batch (`report.pdf` and `report.png`)
are warned about, and `render-routed` crops only the first.

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
| `words[].conf` | no | the engine's confidence, 0-1; its mean and minimum over the words taken are reported in [`doc:ocrReadings`](../formats/doco.md#ocr-readings). A score outside 0-1 says the reading is scored on another scale, where its other scores mean something else too (a Tesseract 1 is 1 in 100): the reading then reports no confidence at all, with a warning on stderr, and a reading of a region (`r{i}`) or of a placed picture is judged noise by its boxes alone, as a reading with no `conf` is. A word with no text is not counted, whatever its score: it is never placed, and Tesseract scores its rows of blocks and lines -1. Divide Tesseract's 0-100 scores by 100 |
| `words[].ink` | no | the word's ink measured in the image read, in its pixels: heights above the baseline, `first_cap`, a capital opening the word from its foot; `top`, its highest line, which `top_line` names (`cap` or `ascender`; another name is read as none); `x_height`; `stem`, its stems' width, and `density`, how much of the band they are measured in is ink (0-1), which say its weight. The built-in engine writes it for Latin text. A reading whose words carry it is sized by their ink rather than their boxes, at the scale its boxes give: see below. Each field is optional, and a word without it is sized by its box. It sizes detector boxes only: a reading whose boxes hug the ink (Tesseract, hOCR, ALTO) is sized by them |
| `rules` | no | the straight rules the image draws, in the same frame as the words. Absent means none |
| `rules[].box` | with `rules` | `[x0, y0, x1, y1]`, y down: the rule's extent, a thin band |
| `rules[].axis` | with `rules` | `horizontal` or `vertical` |

**Rules.** A table pasted into a document as a picture keeps its ruling only
as pixels, so the words alone do not say where its rows and columns are.
`rules` says it. `fdoc ocr`, and `convert --ocr`, write the rules each `r{i}`
crop draws: thin straight bands of ink darker than the image's ground, long
against the image and its text. Where a picture's own rules grid most of its
words into mostly filled cells across its text, the picture is read as a
table ruled by them; any other picture's rules are not used. A `full`
reading's rules are not used. A sidecar without `rules` reads as it always
has, and an engine that supplies its own may draw them by any method.

```json
"rules": [
  { "box": [35, 61, 903, 64], "axis": "horizontal" },
  { "box": [35, 61, 38, 249], "axis": "vertical" }
]
```

The format is stable: `--ocr-words` (`FDOC_OCR_WORDS`), the file names, the
fields above and their units. A release changes or removes one only as a
breaking change; a new optional field may be added.

**The crop, in pixels.** A `px` reading of `full` covers the whole page; one
of `r{i}` covers the region plus a 6-unit margin, clipped to the page —
what `render-routed` draws. Any resolution works: `width` and `height` scale
the reading onto the crop, so an engine may read the same crop rendered
finer, as `render-routed --stored-resolution` renders it. The image embedded
in the page is not that crop: it may cover only part of the page, and a
region crop includes its margin. For a bare image (PNG, JPEG, TIFF) the page
is the image, so its `full` crop is the image itself, turned upright by its
EXIF orientation, one pixel per unit. On a page turned to read upright
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
them (`r0`, `r1`, …), then the small ones after them: a model's `r1` and
an engine's `r1` are the same region. `render-routed --ocr-regions` crops
the small ones too, with the reason `small_region`. Only a run that reads
with an engine (`--ocr`, `--ocr-words`, `--ocr-regions`) decodes the small
images to tell which hold text: without one nothing would read them, and
they are most of what probing a document's images costs. An image the pixel
probe does not take for text is no unread region: nothing reads it, and an
`r{i}` reading names none.

A whole-page reading for any other page is refused with a warning, so the
text layer of a born-digital page is never replaced by a reading of it — a
page asked for on a doubt about its headings included. Region readings are
looked up only for the page's unread regions. A word over a glyph the
page already has is dropped, so a stamp the text layer carries is not read
twice.

Only those names are looked up — `full` on every page, `r{i}` for each unread region — so a batch can share one directory. The elements these words make up are marked `provenance: "ocr"`. A word read in a region `r{i}` of a page with text of its own is the picture's text, read apart from the page's and never a heading; a word of a `full` reading is the page's text.

**What to read.** The `render-routed` manifest says why each crop exists
([escalation tiers](escalation-tiers.md#producing-the-crops)). An OCR engine
needs to read only the crops whose `reason` is `scanned`, `near_blank`,
`broken_text`, `region`, `small_region`, `embedded_image` (a DOCX's or an
XLSX's picture, an e-mail's inline image) or `small_image` (one under a model's floor): those are the ones whose readings the document takes. A
`table_missing` crop it reads only where the page's text does not cover it
(see [escalation tiers](escalation-tiers.md#producing-the-crops)). [`fdoc ocr`](../cli/ocr.md) reads a crops directory that way.
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
still read apart. The run's size comes from its boxes and the letters they
hold. A detector box (PP-OCR) is the ink and a margin, and grows with the
ink by about half as much: the built-in engine boxes a line of prose with
capitals and descenders about 1.09 em tall, one with no descenders about
0.98, one of x-height letters only about 0.87, the dots over `i` and `j`
counting as tall letters. A box less than five times as long as it is
thick, over a word or two, is a little taller for its type than one over a
line, by a number of the image's pixels rather than a share of its type
(about 4 px at twice as long as thick; 0.15 em in a reading in page
units, which has no pixels), so `Total` or `Reçu` alone on a line reads at
the size of the paragraph around it at any resolution. That margin was
measured in paragraphs set 1.3 em apart: with more room above and below, a
word alone on its line boxes taller still (about 0.24 em at double
spacing), and at body size can read as a heading. A box around the
ink (Tesseract, hOCR, ALTO) is as tall as its letters reach, so `our` and
`Payments` read at one size. Either way the marks around the letters
count: an accent over a small letter reaches a capital's height and one
over a capital about 0.9 em (on a detector's box over a line, where it
stands over one letter of many, half as far above the capitals), a cedilla
or an ogonek a descender's depth;
brackets and `|` reach from a capital's height to a descender's depth, a
comma, a semicolon and `$` a little below the baseline, quotes and `/` a
capital's height. This was measured on Latin script; other scripts are
sized by their letters' case alone. Which kind a
document's readings hold is decided once, over all of them: neighbouring
ink boxes differ in height as their letters do. What still sets one line's
detector box apart from another's is noise, a pixel or two and the font: a
run within 10% of the median size of its page's boxes, each box weighed by
the letters it holds, is set at that size, so it does not read as a
heading set larger. A page of fewer than eight lines is sized by the
median of all the document's boxes instead. A heading on a scan is one by
its size when it is set more than 15% larger than the body (a 12 pt heading
over 10 pt text, 20% larger, can come out under that), with or without
descenders: a 14 pt `Exclusions` over 11.5 pt text reads as a heading.

A box says a line's size to about 5% either way, and a heading set a fifth
larger than its body has 4% to spare. So the built-in engine also measures
each word's ink, in the image it read (`words[].ink`): column by column of
the box set level by its angle, the background taken from the box's border
and ink as a distance from it in colour, specks and rules (an underline, a
strike, a box drawn round the word) taken out, the text saying which lines
its letters should reach. A reading of five words or more with their ink is
sized by it: each word in capitals' heights (a capital opening it, or its
highest line by the reading's own ratio of ascenders to capitals, or its
x-height by the ratio of capitals to it), at the scale the reading's boxes
give in the median. A word whose ink stands more than 40% off its box is
sized by its box. On born-digital pages rasterised at 150 to 300 dpi and
read by the built-in engine (Helvetica, Times and Courier, headings a fifth
larger and not bold), 581 of 582 headings read as headings, against 322
by their boxes, and one line of body size of 519 does; on the same pages
in colour, light on dark, over a photo or a gradient, noisy, blurred,
re-saved as a JPEG, through a fax's threshold, turned 1.5 degrees,
underlined, struck through, boxed or in italic, 91% or more (by their
boxes, 12% to 85%; see [Twins](../benchmarks/twins.md)).

The same measure gives a word's weight, which OCR otherwise does not see: a
heading set in bold at body size, the most common cue after size, is plain
text on a scan without it. A word is weighed against its reading's lines of
three words or more measured in the same band (its x-height's, or its
capitals' for a word of capitals): one whose stems, over its type size,
stand more than 1.25 times their median, and whose band holds more than 1.3
times their ink, is bold (weight 700), and any other regular (400). A word
whose band spans fewer than 12 pixels, of fewer than three letters and
figures, with a rule taken out of it (an underline, a strike, a box's edge),
or that its ink does not size, has no weight, nor has a word whose band has
fewer than three such lines in its reading.
Punctuation alone in a box takes the size of its run. The sheet's
skew, stated by `angle` or measured between neighbouring words, is taken out
first, and the runs of one visual line share one baseline. Lines are set
level, so on a skewed sheet text sits off where it is drawn by its distance
from the middle of the reading times the skew's tangent. A word 7.5° or more off the sheet's angle is laid on its own: along its angle when it runs sideways (about ±90°), level about its center otherwise. An engine that boxes
whole lines square, with no `angle`, on a skewed sheet gives nothing to
measure the skew by: give the angle. A sheet fed sideways, two thirds of
whose text, counted in characters of words of three or more, the engine
states at about one of ±90°, with less than a quarter of it level, or fed
upside down, two thirds of its text at about 180° or more of it at 180°
than level with the two together a quarter of it and 20 characters or more
at 180° (an upright fax header over a sheet upside down, or a table set
sideways on it, not a stamp or a word), is read in the frame where it runs upright, as a page
drawn turned is when it is extracted, its boxes still on the page as displayed. Over a page that has
its own text, a reading upside down is refused, and a sideways one has its
words laid along their angle where they are.

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
an engine that gave two words for a dense scan has not read it; not an image
given on its own, read once a word in it was taken. A reading that took no
word reads nothing: the page or region stays as it was, routed
and reported unread (in
[`doc:unreadPages`](../formats/doco.md#pages-nothing-read) or
[`doc:unreadRegions`](../formats/doco.md#regions-nothing-read)). So does a whole-page reading
most of whose words land nowhere, or a region's most of whose words land off
its crop (its margin included), or any reading all of whose words do: it is
refused, and not listed in `doc:ocrReadings` as one that came back blank. When a page has both a
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
jq -r 'select(.reason | IN("scanned", "near_blank", "broken_text", "region", "small_region", "embedded_image", "small_image")) | "./crops/" + .png' \
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
