# Wiring the escalation tiers

There are two ways to run the tiers, and they produce the same output.

**Configure a provider and it is one command.** `fdoc config gemini
--credentials <key.json>` once, and `fdoc convert report.pdf -f doco`
escalates the pages that ask for it — rendering the crops, reading them and
splicing the results, in process. See [`fdoc config`](../cli/config.md).

**Or supply the readings yourself.** Tiers 2–5 read model output from
**directories of JSON files**, so any model you can run can feed them: your
own GPU service, a provider this build has no client for, a batch that ran
last week. `fdoc` tells you what to send and arbitrates what comes back.

Reach for the second when the reader is not Google's, when the readings have
to be auditable, or when nothing may leave the machine — with no provider
configured, `fdoc` opens no connection at all. The rest of this page is that
path.

## The loop

```bash
# 1. What needs escalation, and what would it cost?
fdoc triage ./corpus/

# 2. Render the crops the tiers should read.
fdoc dev render-routed report.pdf ./crops

# 3. Run your models over ./crops/*.png, writing one JSON per crop.
#    (your GPU service — see "The reading format" below)

# 4. Convert, splicing the readings back in.
fdoc convert report.pdf --tier-results ./readings -f doco
```

Step 4 applies the [arbitration
rules](../concepts/escalation.md): shapes are compared, text is not replaced
where the shapes agree, and with three readers the majority wins.

## The three inputs

| flag | env | supplies |
|---|---|---|
| `--layout-boxes <DIR>` | `FDOC_TITLE_BOXES` | tier 2: layout-detector boxes |
| `--tier-results <DIR>` | `FDOC_TIER_RESULTS` | tier 4: VLM readings of crops |
| `--structure-results <DIR>` | `FDOC_STRUCTURE_RESULTS` | tier 3/5: table-structure readings |
| `--ocr-words <DIR>` | `FDOC_OCR_WORDS` | OCR words, read as glyphs where the text is pixels |

Supplying all three enables the [three-way
veto](../concepts/escalation.md#the-three-way-veto) — tier 5, the
first-place configuration.

```bash
FDOC_TITLE_BOXES=./layout \
FDOC_STRUCTURE_RESULTS=./structure \
FDOC_TIER_RESULTS=./vlm \
fdoc convert report.pdf -f doco
```

Each is independent: any subset works, and any missing sidecar simply means
that tier does not run for that document.

## Producing layout boxes

Tier 2 reads boxes from a layout detector run over every page. `fdoc` does
not run the detector itself and no provider supplies it: you run it, and
pass its boxes in. The published scores use PaddleOCR's PP-DocLayoutV3, run
alone, without OCR. Any detector that writes the [box
format](sidecar-formats.md#1-layout-boxes----layout-boxes) with the same
label names will do.

```bash
# 1. Every page to PNG at 2×, named {stem}_p{page}_page.png
fdoc dev render-pages report.pdf ./pages

# 2. Boxes for each page, as {stem}_p{page}_page.json
python3 -m venv .venv          # tested with Python 3.12
.venv/bin/pip install paddlepaddle==3.3.1 paddleocr==3.7.0
.venv/bin/python layout_boxes.py ./pages ./layout

# 3. Convert with them
fdoc convert report.pdf -f doco --layout-boxes ./layout
```

`render-pages` takes a directory as well as a file, and names each image
after its document and page, so one `./layout` serves a whole corpus.
`layout_boxes.py`:

```python
"""Write fdoc layout-box sidecars: PP-DocLayoutV3 over each page PNG."""
import json, pathlib, sys
from paddleocr import LayoutDetection

pages, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
out.mkdir(parents=True, exist_ok=True)
model = LayoutDetection(model_name="PP-DocLayoutV3")
for png in sorted(pages.glob("*_page.png")):
    for res in model.predict(str(png), batch_size=1):
        boxes = [{"label": b["label"], "score": round(float(b["score"]), 4),
                  "box": [round(float(v), 1) for v in b["coordinate"]]}
                 for b in res["boxes"]]
    (out / f"{png.stem}.json").write_text(json.dumps(boxes))
```

The model downloads on first use. On a GPU it reads a page in about 0.2 s.
`paddlepaddle` has no GPU support on a Mac, where it runs on CPU at about
0.8 s a page. Write every box the detector returns: `fdoc` ignores those
scored below 0.6 and the labels it has no use for. The boxes are in the
render's pixels, which is why the pages must come from `render-pages`: `fdoc`
halves the coordinates to reach PDF units.

## Producing the crops

```bash
fdoc dev render-routed <file|dir> [out-dir]
```

Renders every routed page and region to PNG at **2×** (≈144 dpi) with a small
margin, and writes `manifest.jsonl` alongside:

```jsonl
{"bbox":[75.6,255.6,536.4,489.6],"doc":"datasheet-b","kind":"region","page":33,"png":"datasheet-b_p33_r0.png","reason":"region","scale":2.0,"table":true}
{"doc":"form","kind":"page","page":0,"png":"form_p0_full.png","reason":"heading_doubt","scale":2.0}
```

`scale` is the crop's pixels per PDF unit. 2× is what a model reads best and
what it bills for, measured. An OCR engine wants more where the file has it:
a 3,840-px screen capture pasted 6.5 inches wide comes out 960 px wide at 2×,
its 22-px text at 5.5 px. `--stored-resolution` renders each crop at the
resolution of the image it shows (the one mostly inside it, the largest of
those; failing any, the one covering most of it), measured along the
image's coarser axis, between 2× and 300 dpi. A one-bit image (a fax, an
image mask, a CCITT or JBIG2 stream) stays at 2×: smoothed, it reads better
than its raw pixels.

What is bounded, for an engine:

- A crop is held back toward 2× (never below it) to stay under 4,000 px on
  its long edge.
- A crop is cut from a render of its whole page, and that render is bounded
  outright: at most 64 million pixels (256 MB of RGBA) and 65,535 on a side,
  for every crop of the page, the 2× ones included. A page past what 2× draws
  within those (about 4,000 points square) is drawn below 2×, as large as
  they allow.
- A page is rendered at most four times for its crops, plus once per crop
  held to its long-edge cap that no shared scale fits: past four scales, a
  crop shares the next one up, never a coarser one, nor one past its cap.
  Such a crop is over about 960 points long, so it is rare.

A model's crops are drawn at 2× whatever the page's size.

On a synthetic corpus of 500 attachments this lifted the facts an OCR engine
found only in pixels, counted as whole words, from 0.861 to 0.871;
screenshots pasted into PDFs gained most. The finer crops cost time and
memory in proportion to their pixels: on a page carrying a 288-ppi capture
drawn 20 times, `convert --ocr` took 28.75 s, against 12.27 s at 2×, and a
corpus of screenshots costs about that much more to read. One limit comes from the built-in
engine's recogniser, not from the rendering: it decides a line brought to
48 px high in steps of about 8 px, and the word space of a narrow UI font,
between groups of digits, spans about a step and a half. There the space and
"no character" can come out nearly even, and the space is dropped
(`25409834791723/02/2017`); any small change to the crop tips it one way or
the other, at either scale.

The manifest maps every PNG back to its document, page and PDF-unit box, which
is what lets a reading be spliced into the right place. The box is in the
page's reading frame; the PNG is cut from the page as displayed. On a page
read in a turned frame (`/Rotate`, or text drawn sideways) the two differ by
the turn, so a consumer mapping pixels to the box goes through the page's
rotation, as `--ocr-words` does, not through `bbox × scale` alone.

`reason` says why the crop exists:

| reason | crop | the text layer |
|---|---|---|
| `scanned`, `near_blank`, `broken_text` | whole page | has nothing usable there |
| `region` | a raster region, or a whole page that also carries one | lacks what the pixels hold |
| `small_region` | a raster region under a tenth of the page, cropped only with `--ocr-regions`: not worth a model's call, read by an OCR engine | lacks what the pixels hold |
| `embedded_image` | an image placed in a DOCX's flow, once however many figures draw it (`kind: "media"`, with its first `figure`, every one of its `figures` and its package `part`), `<stem>_m<n>.png`; for an OCR engine only for now (see below) | has the text around it, not the image's |
| `table` | a table the grid does not trust | has the text, not the structure |
| `table_missing` | a table a layout detector boxed and no grid covered | usually has the text, not the structure; a raster table too small to route as a region has neither |
| `heading_doubt`, `figure_doubt`, `column_doubt` | whole page | has all of it; the doubt is about structure |

The names are stable: a producer may filter on them, and a release renames
or removes one only as a breaking change. A new reason may be added.

An `embedded_image` crop is read by OCR only, for now: `convert` takes its
words (`--ocr-words`, `<stem>_m<n>.json`) or runs its engine on it, but has
no way back for a model's reading of it, and a configured model never reads
one. A model reading a DOCX picture is still open in #10. A pipeline that
sends every crop to its model should leave these to its OCR engine.

A reader that sees only characters (an OCR engine) has nothing to add to a
`table` crop, a `table_missing` crop or a doubt page and should skip them.
Skipping `table_missing` has one known gap: a raster table too small for the
router to send a reader is boxed by the layout detector alone, so its text
stays unread until a reader that sees structure takes the crop. A table on
a page read whole for a doubt has no crop of its own: it goes with its page,
under the doubt's reason. A `full` reading replaces its page whatever the
reason, so on a doubt page any character it gets wrong replaces text the PDF
had right.

The `table` field (not the reason) says the layout detector boxed a table
inside this crop, so a reader can be told what the image holds instead of
deciding for itself. It needs `FDOC_TITLE_BOXES`, and is `false` without it.
The distinction is worth passing on: a routing trigger knows a region is
unreadable but not what it is, and a model left to judge transcribes some
grids as one value per line — which reads as a plausible answer and scores
zero for structure.

**Render on an opaque background.** If you rasterize pages yourself rather
than using `render-routed`, note that a transparent background flattens to
black in any RGB consumer, so black text becomes invisible and the model sees
a blank page. The symptom is a near-empty result on a page you know has text,
and it costs a full GPU run to diagnose.

## The reading format

`--tier-results` and `--structure-results` point at directories of
`{stem}_{crop}.json`, matching the crop names `render-routed` produced.

The JSON is an array of pages, each with a `parsing_res_list` of blocks:

```jsonc
[{
  "parsing_res_list": [
    { "block_label": "table",
      "block_content": "<table>…</table>",
      "block_bbox": [133, 160, 1056, 1290],
      "block_order": 2 }
  ]
}]
```

Only `block_label`, `block_content` and `block_order` are required; blocks are
sorted by `block_order` when present. See [Sidecar
formats](sidecar-formats.md) for the full contract including the layout-box
shape.

## Implementing a different backend

In Rust, implement `TierBackend`:

```rust
use fluree_doc_pdf::arbiter::{Block, TierBackend};

struct MyBackend { /* … */ }

impl TierBackend for MyBackend {
    fn read(&self, stem: &str, crop: &str) -> Option<Vec<Block>> {
        // crop is "p{page}_{tag}", tag ∈ r{i} | t{i} | n{i} | full
        todo!()
    }
}
```

`FixtureBackend` is the file-reading implementation the CLI uses. Because the
whole tier stack runs against files, **the complete cascade is testable with
no model in reach** — which is how every score in `eval/` reproduces without a
GPU.

## Operational notes

- **Per-crop timeouts are mandatory.** A VLM under load can fall into a
  repetition loop and run several times its normal latency. A crop that will
  not terminate must be abandoned, not waited on.
- **Latency is ~6–11 s/page** for a VLM, against ~8 ms for the deterministic
  tier. Table content is generated autoregressively, so dense pages cost most.
  This is why escalation is per-region rather than per-document.
