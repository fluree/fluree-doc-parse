# Modes

A run reads in one of three modes. Each builds on the one before, and a
document only uses what it needs: most pages carry their own text, and never
reach OCR or a model in any mode.

| mode | adds | needs | nothing leaves the machine |
|---|---|---|---|
| `deterministic` | the document's own text, layout, tables and headings | nothing | yes |
| `ocr` | the built-in OCR engine, on pages and regions whose text is pixels | the OCR assets | yes |
| `vlm` | a model reader, for what OCR cannot do: a scan's structure, a table in a picture, lettering OCR is unsure of | the OCR assets and a model reader | no |

How each mode scores on a public corpus is on [the benchmarks
page](../benchmarks/README.md).

## Choosing the mode

```sh
fdoc convert report.pdf                       # deterministic
fdoc convert scan.pdf --mode ocr --models ~/assets/ocr
fdoc convert scan.pdf --mode vlm              # with mode's needs in the config
```

The mode comes from the first of these that is set:

1. `--mode deterministic|ocr|vlm`
2. the `FDOC_MODE` environment variable
3. `mode` in the [config](../cli/config.md)
4. `deterministic`

`fdoc config show` prints the mode a run with no flags would use and where it
came from; `fdoc convert -v` prints the run's.

A mode whose needs are not met is an error that says what to set. It never
quietly runs in a lower mode, so a batch run under a wrong assumption cannot
look like a working one.
- `ocr` needs the OCR assets: `--models DIR`, `FDOC_OCR_MODELS` or `ocr.models`
  (see [OCR](../cli/convert.md#ocr)).
- `vlm` needs those and a model reader: `fdoc config gemini --credentials
  <key.json>` sets one up, and writes `mode = "vlm"`.

**`--no-escalate`** keeps its meaning: no model is called in this run. A `vlm`
the environment or the config asks for reads as `ocr`. Given with `--mode
vlm`, the two contradict each other, and the run stops.

Readings you already have stand in for what produces them: OCR words from
files (`--ocr-words`) for the engine, and model readings from files
(`--tier-results`) for the model. Giving them asks for the mode they belong
to, so they are never ignored. See [sidecar formats](../integration/sidecar-formats.md).

## The two principles

**Models arbitrate structure; the page tier owns its page.** Where the
deterministic pass produced a reading, a model's replaces it only when their
shapes disagree. Where the deterministic pass is what failed, the reading owns
the page: there is no good text to prefer to it.

**What is read is earned, not configured.** The mode says what may be used.
Within it, each page's own output decides what is used: a page with no glyphs
earns OCR, a reading OCR is unsure of earns the model, a grid whose detected
structure disagrees with itself earns a table reading. Nothing is sent
because it might be read better.

## What this costs

A deterministic page costs milliseconds. In `ocr`, most pages cost nothing
more: they carry their own text. A page OCR reads costs from a fraction of a
second to a few seconds, with the number of lines it reads. A drawing whose
callouts make many small pictures can take longer. `ocr.max_small_regions_per_page`
bounds a page's time where latency matters (see [config](../cli/config.md#ocr)).
In `vlm`, a page sent to the model costs a call, seconds each, and only what
OCR cannot read well is sent.

Your rate depends entirely on your document mix. Measure it with [`fdoc
triage`](../cli/triage.md) before sizing anything.

## Where drawings point the other way

On a mechanical package drawing, a model returned the whole drawing as one
opaque image block, and every dimension callout inside it as an image. The
deterministic path extracts that same text with per-glyph rotation. For
engineering drawings, a model loses information; their text is read where
it lies.
