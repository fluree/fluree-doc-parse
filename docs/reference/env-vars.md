# Environment variables

## Tier wiring

Each has an equivalent flag on [`fdoc convert`](../cli/convert.md); the flag
wins where both are set. See [Wiring the escalation
tiers](../integration/escalation-tiers.md).

| variable | flag | supplies |
|---|---|---|
| `FDOC_TITLE_BOXES` | `--layout-boxes` | layout-detector sidecars (tier 2) |
| `FDOC_TIER_RESULTS` | `--tier-results` | model readings of crops (tier 4) |
| `FDOC_ESCALATE_COLUMNS` | `escalation.on_column_doubt` | also escalate pages whose columns the segmentation cannot represent, or with `0` do not — see below |
| `FDOC_STRUCTURE_RESULTS` | `--structure-results` | table-structure readings (tier 3/5) |
| `FDOC_VLM_ANCHORS` | `--emit-anchors` | emit `[[VLM:…]]` tokens; with a reader, keep the ones it did not answer — see [Anchors](../integration/anchors.md#interaction-with---tier-results) |
| `FDOC_OCR_WORDS` | `--ocr-words` | OCR word readings, read as glyphs before analysis |
| `FDOC_OCR` | `--ocr` | the built-in OCR engine to read pixels with (`oar-ocr`) |
| `FDOC_OCR_MODELS` | `--models` | the OCR assets directory |
| `FDOC_HOME` | — | where the per-user [config](../cli/config.md) lives, overriding the platform default |

## Limits

What one document may take ([Limits](limits.md#setting-the-limits)). Each
has a flag, which wins over it, and a key in the config's `[limits]`
table, over which it wins. A flag or variable that is not a number, or a
zero anywhere, stops the command before it reads anything, naming the
flag, the variable or the config key it was set by; a variable its flag
overrides is not read, and one set empty (`FDOC_MAX_MEMORY=`) is unset. A
config value that is not a whole number is warned about and ignored
([config](../cli/config.md#limits)).

| variable | flag | config key | bounds | default |
|---|---|---|---|---|
| `FDOC_MAX_MEMORY` | `--max-memory` | `max_memory` | the memory one document's elements, cells, text and parts may take, in MiB | 2560 |
| `FDOC_MAX_WORK` | `--max-work` | `max_work` | the steps of layout and parsing one document may take | 268435456 |
| `FDOC_MAX_PAGES` | `--max-pages` | `max_pages` | the pages a PDF or XPS document is read to | 10000 |
| `FDOC_MAX_RENDER_PIXELS` | `--max-render-pixels` | `max_render_pixels` | the pixels a page is rendered with | 64000000 |

Presence is what counts for the boolean ones — any value, including an empty
string, enables them. `FDOC_ESCALATE_COLUMNS` is the one exception: `0`,
`false`, `off` or `no`, trimmed and in any case, turn it off.

```bash
FDOC_TITLE_BOXES=./layout \
FDOC_STRUCTURE_RESULTS=./structure \
FDOC_TIER_RESULTS=./vlm \
fdoc convert report.pdf -f doco
```

## Behavior

| variable | effect |
|---|---|
| `FDOC_INSERT_TABLES` | emit tables a detector found where the grid pass found none. Measured net-negative against benchmark ground truth, which rarely transcribes them — but a completeness-first deployment may want them |

## Diagnostics

Unstable, like [`fdoc dev`](../cli/dev.md) itself.

| variable | effect |
|---|---|
| `FDOC_ROUTE_VERBOSE` | per-page routing signals over a directory, not just for a single file |
| `FDOC_TABLE_CONF` | table-confidence signals to stderr |
| `FDOC_HEADING_SOURCES` | `fdoc dev headings` prints per-detector attribution instead of headings |

## Standard

| variable | effect |
|---|---|
| `NO_COLOR` | disable colored output, same as `--no-color` |


## `FDOC_ESCALATE_COLUMNS`

The same switch as `escalation.on_column_doubt` in
[the config file](../cli/config.md), for one run rather than a deployment.
Set, it wins over the file either way: `FDOC_ESCALATE_COLUMNS=0` (or
`false`, `off`, `no`) keeps column doubt off whatever the file says, any other
value turns it on; one that is not a usual yes (`1`, `true`, `on`, `yes`, or
empty) is warned about.

Off by default. When on, a page that [`column::doubt`] flags — columns that do
not run the page's full height, which the whitespace projection cannot see — is
escalated as a whole page.

Whether this helps depends on the corpus, and nothing on the page says which
kind you have. On the evaluation corpus it is **net negative**: fourteen
documents better, seven worse, −0.0016 overall, and the ones it hurts had sound
hierarchies that escalation then disturbed. On layout-heavy material it marks
exactly the pages that read across their panels — a marketing deck of
twenty-eight pages gains six escalations that are all real failures, including
two the default leaves broken.

Four discriminators were tried and none separates the two populations: how much
of the page the doubtful band covers, how many gutters were missed, whether our
lines are concatenations of the reading's, and whether the document carries a
PDF outline. Until one is found this is a decision about your documents, so it
is a switch rather than a heuristic.

## Diagnostic switches

Not a compatibility surface. These exist to make one measurement reproducible
and are read by `fdoc dev`, or by a stage while it is being tuned; they are
listed so that finding one in the source does not require reading the code
around it.

| variable | effect |
|---|---|
| `FDOC_PROBE_DEBUG` | print the pixel probe's verdict on each raster image a PDF page draws, with its size |
| `FDOC_NO_REJOIN` | leave lines split across a column boundary unjoined |
| `GAPS_DIGITS_ONLY` | `dev gaps`: count only gaps between two digits |
| `GAPS_NO_DIGITS` | `dev gaps`: count only gaps that are not between two digits |

The last two are how the word-space threshold was set from data: digit pairs
are kerned differently from letter pairs, and pooling them hides the boundary
that matters.
