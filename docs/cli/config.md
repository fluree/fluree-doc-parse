# config

```bash
fdoc config gemini --credentials ~/sa-key.json   # set the deep reader up
fdoc config show                                 # what is set, and is it ready
fdoc config path                                 # which file is in effect
fdoc config init [--global]                      # write a commented template
fdoc config set <key> <value>                    # one key at a time
```

The config says which [mode](../getting-started/modes.md) a run reads in, and
sets up what the modes past `deterministic` need: the OCR assets and a model
reader. Everything else about a parse is deterministic and takes no settings;
that is what makes a parse reproducible on someone else's machine.

**With no `mode` set, `fdoc` reads deterministic and never reaches the
network.** `mode = "vlm"` is what sends pages to a model.

## Setting up Google Vertex AI

You need a service-account key with the **Vertex AI User** role, from
`IAM & Admin > Service Accounts > Keys > Add key > JSON` in the Google Cloud
console.

```bash
fdoc config gemini --credentials ~/Downloads/sa-key.json
```

```
escalation configured in /Users/you/.config/fluree-doc-parse/config.toml
  provider gemini, model gemini-3-flash-preview, project my-project
  mode = "vlm": `fdoc convert <pdf>` sends a model what OCR cannot read
```

It sets `mode = "vlm"`. `vlm` builds on `ocr`, so it also needs the OCR assets
(`ocr.models`, below); the command says so when they are not set.

The key is read and checked here rather than at the first request, so a wrong
file is caught now and not halfway through a batch. The path is stored
absolute — a relative one would resolve against whatever directory the next
run started in.

`--project` overrides the project in the key; `--model` names a different
model.

## Where the config lives

Nearest first:

1. `./.fdoc/config.toml`, walking up from the working directory — so a project
   can pin its own settings
2. `$FDOC_HOME/config.toml`
3. the per-user config directory: `~/.config/fluree-doc-parse/config.toml` on
   Linux, `~/Library/Application Support/fluree-doc-parse/` on macOS

`fdoc config path` prints the one in effect. Written files are owner-only
(`0600`), because the file names the path to a private key.

## The file

```toml
mode = "vlm"

[escalation]
provider = "gemini"
model = "gemini-3-flash-preview"
concurrency = 6

[escalation.gemini]
credentials = "/Users/you/sa-key.json"
project = "my-project"          # optional; read from the key when absent
```

| key | means |
|---|---|
| `mode` | `deterministic` (the default), `ocr` or `vlm`: see [modes](../getting-started/modes.md). `--mode` and `FDOC_MODE` override it |
| `escalation.provider` | `gemini` is the one this build implements |
| `model` | passed to the provider unchanged |
| `concurrency` | crops read at once |
| `on_column_doubt` | also escalate pages that read across their panels |
| `max_crops` | most crops one document sends; past it the most valuable are read and the rest keep their deterministic reading (default 200, `0` for no limit) |
| `gemini.credentials` | path to the service-account JSON key |
| `gemini.project` | overrides the project named in the key |

## OCR

```toml
[ocr]
engine = "oar-ocr"
models = "/opt/fluree/assets/ocr"
threads = 1
max_megapixels = 12.5
max_memory = 8192
max_small_regions_per_page = 0
```

| key | means |
|---|---|
| `engine` | the built-in engine that reads pixels in mode `ocr` and `vlm`; `oar-ocr`, the one this build has, is the default |
| `models` | the OCR assets directory (models and ONNX Runtime) |
| `threads` | ONNX Runtime threads per crop read (default 1: a batch runs one per worker) |
| `max_megapixels` | most megapixels the engine reads in one pass (default 12.5, from 1 to 128): a page scanned at 300 dpi or a 12 Mpx photo is read whole, a larger image in overlapping strips, one too wide for them shrunk with a warning. A pass costs about 230 bytes a pixel while it is read ([how an image is sized](ocr.md#how-an-image-is-sized)) |
| `max_memory` | the memory, in MiB, the passes the engine reads at once may take, each counted at about 230 bytes a pixel (default 8192); a pass that does not fit waits for others to end, one larger than all of it is read alone ([memory and speed](ocr.md#memory-and-speed)). Env: `FDOC_OCR_MAX_MEMORY` |
| `max_crops` | most crops the engine reads in one document; past it pages read whole come first, then routed regions, then small ones, and a note names how many were left (default 1000, `0` for no limit). Separate from `escalation.max_crops`: a scanned page is one crop, and a model's 200 would leave page 201 of a long scan unread |
| `max_small_regions_per_page` | most small regions (pictures too small for a model's call) the engine reads on one page, the largest first, and a note names how many were left (default `0`: every one is read). Routed regions and pages read whole are always read. Set it where a page's time must be bounded: a drawing's callouts or a paper's figure labels can make a hundred crops of one page, each read on its own. At 8, such a page reads in about a tenth of the time, and a corpus loses about 2.6% of its OCR'd lines, mostly callouts and labels, but some of them a drawing's notes or problems set as images |

`--models` and `--ocr-max-megapixels` override these per run, and
`FDOC_OCR_MODELS` and `FDOC_OCR_MAX_MEGAPIXELS` override the file. Naming an
engine does not turn OCR on; the mode does (`mode = "ocr"`). Unlike the model reader, OCR runs on the
machine and calls nothing outside it.

## Pages that read across their panels

A page laid out as panels side by side under a full-width heading has columns
a page-global whitespace projection cannot see — the heading spans the gutters,
so the projection finds no gap and reading order runs across the panels:

```
The charities I support are extremely important to
What happens to our
me — how do I maximize
child's inheritance
```

`fdoc triage <file>` reports those pages as `COLUMN`, and they do **not**
escalate by default:

```
report  COLUMN  p7 2 column(s) found, 3 gutter(s) visible only in a band covering 57% of rows

COLUMN pages do not escalate by default — `fdoc config set escalation.on_column_doubt true`
```

The default is a measurement, not caution. Over a 200-document corpus of
reports and papers this flags 22 documents, gains less in total than the
heading signal alone, and makes five worse. On layout-heavy material — decks,
brochures, one-pagers — it marks exactly the pages that need it. Nothing on
the page separates the two populations, so it is a fact about a corpus and
lives in a corpus's configuration.

```bash
fdoc config set escalation.on_column_doubt true
```

`fdoc triage` and `fdoc dev render-routed` follow it too, so that they count
and write the crops `convert` in mode `vlm` sends; each says on stderr when the
file made it read pages whole. `FDOC_ESCALATE_COLUMNS=0` turns it off for one
run, as a benchmark run that must not depend on the machine does.

## How much one document may send

`fdoc convert` says what it is about to pay for before the first call:

```
note: chart.pdf: 21 crop(s) to read with gemini-3-flash-preview
```

and `fdoc triage <file>` prints the same count as a `CROPS` line without
sending anything, given the same structure readings
(`FDOC_STRUCTURE_RESULTS`): a table they veto is not read. Past `max_crops` the most valuable crops are read — a
scanned page before a doubted table — and the rest keep their deterministic
reading, with a note saying how many were left. The deterministic pass is
complete on its own, so the limit truncates rather than fails; see [how many
readings a document asks for](../concepts/escalation.md#how-many-readings-a-document-asks-for).

```bash
fdoc config set escalation.max_crops 500
```

Editing through `fdoc config set` preserves comments and any key this build
does not recognise, so a file can be hand-written and machine-edited in turn.

## Choosing the mode

| you run | what happens |
|---|---|
| no `mode`, no flag | `deterministic`: silent, no network |
| `mode = "ocr"` | the OCR engine reads pages and regions whose text is pixels; an error if `ocr.models` is not set |
| `mode = "vlm"` | and the model reads what OCR cannot; an error if the OCR assets or the provider are not set up |
| `--mode <mode>` | that mode for this run, whatever the config says |
| `--no-escalate` | no model is called: a `vlm` from the config reads as `ocr` |

A mode whose needs are not met stops the run and says what to set. It never
reads in a lower mode instead, because a batch run under a wrong assumption
otherwise looks like a working one:

```
error: mode vlm (from the config's `mode`) builds on ocr, which needs the OCR assets: set --models DIR (or FDOC_OCR_MODELS, or ocr.models in the config), or read words from files with --ocr-words
```

A config that sets up a provider or an OCR engine but no `mode` no longer
turns them on by itself. It reads `deterministic`, or the mode an input such
as `--ocr` asks for, and says once what it leaves out:

```
note: the config sets up a model reader but no mode; reading deterministic. Set mode = "vlm" in the config, or give --mode vlm
```

`fdoc config show` prints the mode a run with no flags would read in, and
where it came from.

Two paths never call a model: `--tier-results` supplies readings someone
already has, so paying to produce them again would be surprising; and the
hidden `fdoc md` / `json` / `xhtml` forms, which scripts shell, so a
configured key on the machine that runs one cannot change its output. The
benchmark adapter passes `--no-escalate` for the same reason.

## What it costs

`fdoc triage <file>` prices a run before you make it — it reports what would
escalate and why, without sending anything. Over the 200-document evaluation
corpus, 113 documents ask for nothing at all.

`--pages` narrows what is *read*, not only what is printed, so inspecting one
page of a long deck costs one crop.
