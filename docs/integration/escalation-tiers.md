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

## Producing the crops

```bash
fdoc dev render-routed <file|dir> [out-dir]
```

Renders every routed page and region to PNG at **2×** (≈144 dpi) with a small
margin, and writes `manifest.jsonl` alongside:

```jsonl
{"bbox":[75.6,255.6,536.4,489.6],"doc":"datasheet-b","kind":"region","page":33,"png":"datasheet-b_p33_r0.png","reason":"region","table":true}
{"doc":"form","kind":"page","page":0,"png":"form_p0_full.png","reason":"heading_doubt"}
```

The manifest maps every PNG back to its document, page and PDF-unit box, which
is what lets a reading be spliced into the right place.

`reason` says why the crop exists:

| reason | crop | the text layer |
|---|---|---|
| `scanned`, `near_blank`, `broken_text` | whole page | has nothing usable there |
| `region` | a raster region, or a whole page that also carries one | lacks what the pixels hold |
| `table` | a table the grid does not trust | has the text, not the structure |
| `table_missing` | a table a layout detector boxed and no grid covered | usually has the text, not the structure; a raster table too small to route as a region has neither |
| `heading_doubt`, `figure_doubt`, `column_doubt` | whole page | has all of it; the doubt is about structure |

A reader that sees only characters (an OCR engine) has nothing to add to a
`table` crop or a doubt page and should skip them. A `table_missing` crop it
may skip only where the page's text covers it: a raster table too small for
the router to send a reader is boxed by the layout detector alone. A table on
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
- **Latency is ~6–11 s/page** for a VLM, against ~7 ms for the deterministic
  tier. Table content is generated autoregressively, so dense pages cost most.
  This is why escalation is per-region rather than per-document.
