# Environment variables

## Tier wiring

Each has an equivalent flag on [`fdoc convert`](../cli/convert.md); the flag
wins where both are set. See [Wiring the escalation
tiers](../integration/escalation-tiers.md).

| variable | flag | supplies |
|---|---|---|
| `FDOC_TITLE_BOXES` | `--layout-boxes` | layout-detector sidecars (tier 2) |
| `FDOC_TIER_RESULTS` | `--tier-results` | model readings of crops (tier 4) |
| `FDOC_ESCALATE_COLUMNS` | `escalation.on_column_doubt` | also escalate pages whose columns the segmentation cannot represent — see below |
| `FDOC_STRUCTURE_RESULTS` | `--structure-results` | table-structure readings (tier 3/5) |
| `FDOC_VLM_ANCHORS` | `--emit-anchors` | emit `[[VLM:…]]` tokens |
| `FDOC_HOME` | — | where the per-user [config](../cli/config.md) lives, overriding the platform default |

Presence is what counts for the boolean ones — any value, including an empty
string, enables them.

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

Off by default. When set, a page that [`column::doubt`] flags — columns that do
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
| `FDOC_PROBE_DEBUG` | print per-glyph decode detail during extraction |
| `FDOC_NO_REJOIN` | leave lines split across a column boundary unjoined |
| `GAPS_DIGITS_ONLY` | `dev gaps`: count only gaps between two digits |
| `GAPS_NO_DIGITS` | `dev gaps`: count only gaps that are not between two digits |

The last two are how the word-space threshold was set from data: digit pairs
are kerned differently from letter pairs, and pooling them hides the boundary
that matters.
