# fdoc convert

Convert documents to Markdown, XHTML, JSON, DoCO JSON-LD or plain text.

```bash
fdoc convert <FILE|DIR|->... [options]
```

Reads PDF, Markdown, HTML, DOCX and PPTX. PDF structure is inferred from
layout; the others declare theirs and carry no geometry. See [Input
formats](../inputs/README.md).

```bash
fdoc convert report.pdf                       # Markdown to stdout
fdoc convert report.pdf -f doco -o out.jsonld
fdoc convert ./corpus/ --out-dir ./out -j 8
cat report.pdf | fdoc convert -
```

## Options

| Option | Description |
|--------|-------------|
| `-f`, `--format <FMT>` | `md` (default), `xhtml`, `json`, `doco`, `text`. See [Output formats](../formats/README.md) |
| `-o`, `--output <FILE>` | write to a file; single input only. Conflicts with `--out-dir` |
| `--out-dir <DIR>` | write one output file per input into this directory |
| `--pages <RANGES>` | restrict to 1-based pages: `3`, `1-5`, `1,4,9-12` |
| `-j`, `--jobs <N>` | parallel workers for batch (`0` = one per core) |
| `--base-iri <IRI>` | base for minted element IRIs in `-f doco`. Default `urn:fluree-doc-parse:<stem>` |
| `--doc-iri <IRI>` | stamp every `-f doco` element with `doc:sourceDocument` |
| `--layout-boxes <DIR>` | layout-detector sidecars. Env: `FDOC_TITLE_BOXES` |
| `--tier-results <DIR>` | model-tier readings to splice. Env: `FDOC_TIER_RESULTS` |
| `--structure-results <DIR>` | table-structure readings. Env: `FDOC_STRUCTURE_RESULTS` |
| `--emit-anchors` | emit `[[VLM:…]]` tokens where escalated crops belong. Env: `FDOC_VLM_ANCHORS` |
| `--escalate` | read escalated pages with the configured model in this run |
| `--no-escalate` | never call a model, whatever the config says |

The last four wire the [escalation
tiers](../integration/escalation-tiers.md); without them you get tier 1.

## Escalation

With a provider configured, `convert` escalates the pages that ask for it and
splices the readings back — the whole loop, in this command. See
[`fdoc config`](config.md) for the setup and the on/off rules.

```bash
fdoc convert report.pdf -f doco          # escalates once a provider is set
fdoc convert report.pdf --no-escalate    # deterministic for this run
fdoc convert report.pdf --escalate       # force it; warns if nothing is set up
```

With nothing configured this never happens and no connection is opened.
`--tier-results` takes precedence: it supplies readings you already have, so
producing them again would be surprising.

`--pages` narrows what is *read*, not only what is printed, so inspecting one
page of a long document costs one crop rather than all of them.

## Inputs

Files, directories, or `-` for stdin. **Stdin is PDF only** — the other
readers identify the format by extension.

Directories are scanned for supported documents. Multiple inputs are allowed;
with more than one you need `--out-dir` rather than `--output`.

## Output naming

With `--out-dir`, each output is the input stem plus the format's extension
(`.md`, `.xhtml`, `.json`, `.jsonld`, `.txt`). Where two inputs share a stem —
`a/report.pdf` and `b/report.docx` — the names are disambiguated rather than
one silently overwriting the other.

## Batch and parallelism

```bash
fdoc convert ./corpus/ --out-dir ./out -j 8
fdoc convert ./corpus/ --out-dir ./out -j 0    # one worker per core
```

A document that fails does not stop the batch: the rest are written and the
exit code is non-zero. Use `-v` to see which failed and how long each took.

## Page ranges

```bash
fdoc convert report.pdf --pages 1-5
fdoc convert report.pdf --pages 1,4,9-12
```

Ranges are **1-based** — matching what a PDF viewer shows — while the `page`
field in the output is **0-based**. The whole document is still parsed and
analyzed; the filter applies to the emitted elements, so cross-page structure
(sections spanning a boundary) is resolved before the cut.

## Exit codes

`0` on success, non-zero if any input failed.

## Compatibility forms

`fdoc md`, `fdoc json` and `fdoc xhtml` are hidden single-file equivalents of
`convert <file> --format <fmt>`, kept because benchmark adapters shell them.
Prefer `convert`.
