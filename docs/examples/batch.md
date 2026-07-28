# Batch conversion

```bash
fdoc convert ./corpus/ --out-dir ./out -f json -j 8
```

One output file per input, eight workers, `-j 0` for one per core. Output
names come from input stems, with an extension matching the format
(`.md`, `.xhtml`, `.json`, `.jsonld`, `.txt`), disambiguated when two inputs
share a stem.

## Failures do not stop the batch

A document that cannot be parsed is reported and skipped; everything else
still converts and still lands in the output directory.

```
error: ./corpus/broken.pdf: parse: Invalid
converted 1/2 files -> ./out
```

The exit code is **1 if any input failed** and 0 otherwise, so a batch is
"succeeded" only when every document did. This is the right default for a
pipeline — you want to know — but it means `set -e` will stop your script on a
single bad scan out of ten thousand.

```bash
fdoc convert ./corpus/ --out-dir ./out -f json -j 8 2> errors.log || true
awk -F': ' '/^error: /{print $2}' errors.log > failed.txt
```

Errors go to stderr, one `error: <path>: <reason>` line each, so the failure
list is recoverable without parsing the summary — which goes to stderr too.

## Sizing the run

Throughput is dominated by page count, not file count. The deterministic tier
runs at a few milliseconds per page on one core — no model, no GPU, no network
— so a large archive is usually bounded by your storage layer rather than by
the parser.

Ask before you commit:

```bash
fdoc triage ./corpus/
```

See [Pricing a deployment](triage-cost.md).

## Streaming from object storage

Stdin avoids a temp file, which matters when documents live in S3 or GCS and
the working set is larger than local disk:

```bash
aws s3 cp s3://bucket/report.pdf - | fdoc convert - -f doco > report.jsonld
```

**Stdin is PDF only.** The other readers identify a format from the file, so
DOCX, PPTX, HTML and Markdown need a real path. Write them to a temp file, or
route by extension:

```bash
case "$key" in
  *.pdf) aws s3 cp "s3://bucket/$key" - | fdoc convert - -f doco ;;
  *)     aws s3 cp "s3://bucket/$key" /tmp/in && fdoc convert /tmp/in -f doco ;;
esac
```

## Converting only part of a document

```bash
fdoc convert report.pdf --pages 1-5,12 -f json
```

Pages are **1-based** on the command line and the `page` field in the output
is **0-based**, because it indexes the physical page array. `--pages 1` gives
you elements with `page: 0`. This is deliberate — see [why `pageIndex` and not
`pageNumber`](../reference/vocabulary.md#why-pageindex-and-not-pagenumber) —
but it is worth an assertion in your loader the first time.

## Parallelism that is already there

`-j` parallelizes *across* documents. A single document is parsed on one
thread, so `-j 8` on one file does nothing. For a small number of very large
documents, split by page range across processes and merge:

```bash
for r in 1-50 51-100 101-150; do
  fdoc convert big.pdf --pages "$r" -f json > "part-$r.json" &
done
wait
jq -s 'add' part-*.json > all.json
```

Element `id`s are assigned from the whole document's numbering before the page
filter applies, so ranges produce disjoint ids and merge without renumbering.

Cover every page. Ranges that leave a gap produce a shorter document, not an
error.
