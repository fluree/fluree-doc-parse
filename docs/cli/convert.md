# fdoc convert

Convert documents to Markdown, XHTML, JSON, DoCO JSON-LD or plain text.

```bash
fdoc convert <FILE|DIR|->... [options]
```

Reads PDF, Markdown, HTML, DOCX, PPTX, XLSX, WebVTT and SubRip transcripts,
email (`.eml`, `.msg`), media assets (AXF), and XML and JSON records a
source format is declared for. PDF structure is inferred from layout; the others declare theirs
and carry no geometry. See [Input formats](../inputs/README.md).

```bash
fdoc convert report.pdf                       # Markdown to stdout
fdoc convert report.pdf -f doco -o out.jsonld
fdoc convert ./corpus/ --out-dir ./out -j 8
cat report.pdf | fdoc convert -
fdoc convert call.vtt -f doco                 # a transcript: one paragraph per turn
fdoc convert reply.eml --attachments ./att    # an email, its attachments saved
fdoc convert episode.axf -f doco              # a media asset: its title and what was said
fdoc convert article.xml -f doco \
     --source-format news-article.json        # a record, read as its format declares
fdoc convert episode.axf -f md \
     --source-format formats.json             # an asset, read as the archive's format declares
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
| `--attachments <DIR>` | save each email's attachments under `DIR/<email name>/`, to convert on their own. See [Email](../inputs/email.md#attachments) |
| `--source-format <FILE>` | read XML and JSON records, and media assets, as these source formats declare. A JSON file holding one declaration or a list of them; repeatable. See [Records](../inputs/records.md#the-declaration) |
| `--layout-boxes <DIR>` | layout-detector sidecars. Env: `FDOC_TITLE_BOXES` |
| `--tier-results <DIR>` | model-tier readings to splice. Env: `FDOC_TIER_RESULTS` |
| `--structure-results <DIR>` | table-structure readings. Env: `FDOC_STRUCTURE_RESULTS` |
| `--emit-anchors` | emit `[[VLM:…]]` tokens where escalated crops belong. Env: `FDOC_VLM_ANCHORS` |
| `--escalate` | read escalated pages with the configured model in this run |
| `--no-escalate` | never call a model, whatever the config says |

`--layout-boxes`, `--tier-results`, `--structure-results` and
`--emit-anchors` wire the [escalation
tiers](../integration/escalation-tiers.md); without them you get tier 1.

## Source formats

```bash
fdoc convert article.xml --source-format news-article.json -f doco
fdoc convert ./records/ --out-dir out \
     --source-format formats/articles.json --source-format formats/stories.json
```

A [source format](../inputs/records.md) declares what the fields of a kind
of record are. `--source-format` names a JSON file that holds one
declaration or a list of them, and may be given several times. Every
declaration is checked when it is read, and one that is wrong is an error
for every record it was passed for.

| input | without `--source-format` | with it |
|---|---|---|
| a media asset (AXF) | read as its title, its dates and its captions | read as the declaration that recognises it says; as its title, dates and captions when none does |
| an XML or JSON record | not read: `fdoc` has no reader for it | read as the declaration that recognises it says; when none does, an error for a file named `.xml` or `.json` |
| a directory | scanned for supported documents, `.axf` among them | scanned for `.xml` and `.json` as well |

A file named `.xml` or `.json`, or read from stdin, was passed as a record.
One that no declaration recognises, or that is not well-formed, is an error
and not a fallback, because the declarations were passed to say what the
inputs are. So is a file named `.xml` or `.json` that opens with neither
`<` nor `{`, a JSON list most often: `not a record: a record is one XML
element or one JSON object, and this file opens with neither`.

A file with any other name that opens with `<` or `{` is read as a record
when a declaration recognises it, and is left to the other readers when
none does. So an HTML page in the same run is still read as an HTML page.

A record or an asset that more than one declaration recognises is an error
whatever its name, and the message names the declarations that claim it.

What in a record does not fit its declaration, such as a value a list does
not hold or a date that is not one, is stated as written and reported on
stderr as a `note:`. The record is still read, and the exit code is `0`.
`-q` silences the notes.

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

Files, directories, or `-` for stdin. **Stdin reads what is recognised by
its content**: a PDF, a transcript, an email, a media asset, or a record
when `--source-format` is given. The other readers identify the format by
extension.

Directories are scanned for supported documents, `.axf` among them. With
`--source-format` given they are scanned for `.xml` and `.json` as well,
and without it they are not. Keep the declarations outside the directories
you convert: a declaration is a `.json` file, and one found there is taken
for a record that no declaration recognises. Multiple inputs are allowed;
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
