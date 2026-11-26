# Model readings

In mode [`vlm`](../getting-started/modes.md), a model reads what OCR cannot
read well. There are two ways to give it one, and they produce the same
output.

**Configure a provider and it is one command.** `fdoc config gemini
--credentials <key.json>` once (it sets `mode = "vlm"`), and `fdoc convert
report.pdf -f doco` sends the pages and regions that need it, reads them and
splices the results, in process. See [`fdoc config`](../cli/config.md).

**Or supply the readings yourself.** Model readings and table-structure
readings are read from **directories of JSON files**, so any model you can
run can feed them: your own GPU service, a provider this build has no client
for, a batch that ran last week. `fdoc` tells you what to send and arbitrates
what comes back.

Reach for the second when the reader is not Google's, when the readings have
to be auditable, or when nothing may leave the machine: with no provider
configured, `fdoc` opens no connection at all. The rest of this page is that
path.

`vlm` builds on `ocr`, readings from files included: give the OCR assets
(`--models`) or OCR words from files (`--ocr-words`) too.

## The loop

```bash
# 1. What needs escalation, and what would it cost?
fdoc triage ./corpus/

# 2. Render the crops the tiers should read.
fdoc dev render-routed report.pdf ./crops

# 3. Run your models over ./crops/*.png, writing one JSON per crop.
#    (your GPU service — see "The reading format" below)

# 4. Convert, splicing the readings back in.
fdoc convert report.pdf --tier-results ./readings --models ~/assets/ocr -f doco
```

Step 4 applies the [arbitration
rules](../concepts/escalation.md): shapes are compared, text is not replaced
where the shapes agree, and with three readers the majority wins.

## The inputs

| flag | env | supplies | asks for |
|---|---|---|---|
| `--tier-results <DIR>` | `FDOC_TIER_RESULTS` | model readings of crops | `vlm` |
| `--structure-results <DIR>` | `FDOC_STRUCTURE_RESULTS` | table-structure readings | |
| `--ocr-words <DIR>` | `FDOC_OCR_WORDS` | OCR words, read as glyphs where the text is pixels | `ocr` |

Model readings and structure readings together enable the [three-way
veto](../concepts/escalation.md#the-three-way-veto). The model reading may
also come from a model called in the run (`--mode vlm` with a provider
configured) in place of `--tier-results`: `--structure-results` vetoes its
table readings the same way, and no table crop it vetoes is sent to the
model.

```bash
FDOC_STRUCTURE_RESULTS=./structure \
FDOC_TIER_RESULTS=./vlm \
FDOC_OCR_MODELS=~/assets/ocr \
fdoc convert report.pdf -f doco
```

Each is independent: any subset works, and a missing sidecar means that
reader adds nothing for that document.

## Producing the crops

```bash
fdoc dev render-routed <file|dir> [out-dir]
```

Renders every routed page and region to PNG at **2×** (≈144 dpi) with a small
margin, and writes `manifest.jsonl` alongside:

```jsonl
{"bbox":[75.6,255.6,536.4,489.6],"doc":"datasheet-b","kind":"region","page":33,"png":"datasheet-b_p33_r0.png","reason":"region","scale":2.0,"table":true,"vetoed":false}
{"bbox":[72.0,118.4,540.0,402.7],"doc":"datasheet-b","kind":"region","page":34,"png":"datasheet-b_p34_t0.png","reason":"table","scale":2.0,"table":true,"vetoed":true}
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

- A crop is not held to a size of its own: the OCR engine reads one larger
  than a pass holds (12.5 Mpx by default) in overlapping strips (see
  [How an image is sized](../cli/ocr.md#how-an-image-is-sized)). A cap of
  4,000 px on the long edge once read a tall capture with a tenth of its lines
  lost.
- A crop is cut from a render of its whole page, and that render is bounded
  outright: at most 64 million pixels (256 MB of RGBA) and 65,535 on a side,
  for every crop of the page, the 2× ones included. A page past what 2× draws
  within those (about 4,000 points square) is drawn below 2×, as large as
  they allow.
- A page is rendered at most four times for its crops: past four scales, a
  crop shares the next one up, never a coarser one.

A model's crops are drawn at 2× whatever the page's size.

On a synthetic corpus of 500 attachments this lifted the facts an OCR engine
found only in pixels, counted as whole words, from 0.861 to 0.871;
screenshots pasted into PDFs gained most. The finer crops cost time and
memory in proportion to their pixels: on a page carrying a 288-ppi capture
drawn 20 times, `convert` in mode `ocr` took 28.75 s, against 12.27 s at 2×, and a
corpus of screenshots costs about that much more to read. One limit comes from the built-in
engine's recogniser, not from the rendering: it decides a line brought to
48 px high in steps of about 8 px, and the word space of a narrow UI font,
between groups of digits, spans about a step and a half. There the space and
"no character" can come out nearly even, and the space is dropped
(`25409834791723/02/2017`); any small change to the crop tips it one way or
the other, at either scale.

The manifest maps every PNG back to its document, page and PDF-unit box, which
is what lets a reading be spliced into the right place. The box is in the
page's display frame, the frame the PNG is cut from, also on a page read in
a turned frame (`/Rotate`, or text drawn sideways): `bbox × scale`, less the
margin, gives the pixels.

`reason` says why the crop exists:

| reason | crop | the text layer |
|---|---|---|
| `scanned`, `near_blank`, `broken_text` | whole page | has nothing usable there |
| `region` | a raster region, or a whole page read for one: widened by a doubt, or read whole because it reads as a scan once its doubled glyphs are dropped | lacks what the pixels hold |
| `small_region` | a raster region under a tenth of the page, cropped with `--ocr-regions` or in mode `ocr` and over: not worth a model's call, read by an OCR engine | lacks what the pixels hold |
| `embedded_image` | an image placed in a DOCX's flow, once however many figures draw it (`kind: "media"`, with its first `figure`, every one of its `figures` and its package `part`), `<stem>_m<n>.png`; for an OCR engine only for now (see below) | has the text around it, not the image's |
| `table` | a table the grid does not trust | has the text, not the structure |
| `heading_doubt`, `figure_doubt`, `column_doubt` | whole page | has all of it; the doubt is about structure |

The names are stable: a producer may filter on them, and a release renames
or removes one only as a breaking change. A new reason may be added.

An `embedded_image` crop is read by OCR only, for now: `convert` takes its
words (`--ocr-words`, `<stem>_m<n>.json`) or runs its engine on it, but has
no way back for a model's reading of it, and a configured model never reads
one. A pipeline that sends every crop to its model should leave these to its
OCR engine.

A reader that sees only characters (an OCR engine) has nothing to add to a
`table` crop or a doubt page and should skip them. A table on
a page read whole for a doubt has no crop of its own: it goes with its page,
under the doubt's reason. A `full` reading replaces its page whatever the
reason, so on a doubt page any character it gets wrong replaces text the PDF
had right.

The `table` field (not the reason) says a table detector boxed a table
inside this crop, so a reader can be told what the image holds instead of
deciding for itself; with no detector's boxes it is `false`. The distinction
is worth passing on: a routing trigger knows a region is unreadable but not
what it is, and a model left to judge transcribes some grids as one value per
line, which reads as a plausible answer and scores zero for structure.

`vetoed` says the [three-way veto](../concepts/escalation.md#the-three-way-veto)
keeps this table's grid whatever a deep reader says: the structure readings
in `FDOC_STRUCTURE_RESULTS` corroborate it. `convert` in mode `vlm` does not
send such a crop and `fdoc triage` does not count it, so a deep reader need
not read it for runs that always pass these structure readings. A reading
cache shared with runs that do not (model readings alone, say) still needs it:
there the table is not vetoed and its reading is spliced. It is written all
the same, because the structure reader reads it: a crop left out on the
strength of a structure reading could never be read again to refresh that
reading. `render-routed` says on stderr how many it wrote. Without
`FDOC_STRUCTURE_RESULTS`, `vetoed` is `false`. A vetoed crop keeps its
`reason`, `table`: the veto says a deep reader need not read it, not why it
was cut. Only region records carry `vetoed`; a whole page is never vetoed.

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
formats](sidecar-formats.md) for the full contract.

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

`FixtureBackend` is the file-reading implementation the CLI uses. Because
every reader can run against files, **a whole `vlm` run is testable with no
model in reach**.

## Operational notes

- **Per-crop timeouts are mandatory.** A VLM under load can fall into a
  repetition loop and run several times its normal latency. A crop that will
  not terminate must be abandoned, not waited on.
- **Latency is ~6–11 s/page** for a model, against milliseconds for a
  deterministic page. Table content is generated autoregressively, so dense
  pages cost most. This is why what is sent is chosen per region rather than
  per document.
