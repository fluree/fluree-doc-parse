# Tests

```bash
cargo test --workspace          # ~194 unit tests
./eval/run.sh                   # the full harness, incl. corpus checks
```

## Unit tests

Colocated with the code they test, in `#[cfg(test)] mod tests`. They lean
heavily on **small synthetic geometry** — a handful of glyphs or rules
positioned to reproduce one situation — rather than on whole PDFs, so a
failure names the behavior that broke rather than "some document changed".

Where a test encodes a measured finding, it says so. A test asserting that a
per-cell-segment grid resolves to four columns is documenting the discovery in
[Tables](../design/tables.md), not just guarding a function.

`cargo test --workspace` also compiles
`crates/fluree-doc-pdf/examples/library_usage.rs`, which is what keeps the
[library documentation](../getting-started/rust-library.md) honest.

## The eval harness

```bash
./eval/run.sh
```

Runs the unit tests, then the corpus checks: the T0/T1 probe (parse rate,
Unicode resolution, panics), span resolution, and the expectations in
`eval/expectations/corpus.json`.

`eval/TEST_PLAN.md` defines the tiers:

| tier | question | gate |
|---|---|---|
| T0 | does the parse layer hold up? | hard fail |
| T1 | is the text correct and normalized? | hard fail |
| T2 | are paragraphs/headings/tables/order right? | scored |
| T3 | do we route the right pages? | scored |
| T4 | are renders correct and fast? | hard fail on panic/blank |
| T5 | is the output contract intact? | hard fail |

**T0.2 — no panics on any page — is a release blocker, not a bug report.**
hayro is pre-1.0 and this engine runs on documents nobody vetted.

## Tough cases

```bash
eval/tough/check.py --fetch     # first run: download the public documents
eval/tough/check.py             # every case
eval/tough/check.py nist-sec    # one case
```

Real documents the engine once read wrong, each with the facts a fix made
true: a table row read whole under the right column names, a column name that
must not appear. Every document is also held to the offset contract. The
benchmark scores Markdown over 200 documents from one mix; a fix to a table
the bench never shows moves no number there, and a later change can undo it
without moving one either. These checks are where such fixes stay fixed.

Public documents are fetched from their publisher and checked against the
SHA-256 their checks were written on; they are not redistributed.
`eval/tough/private/` holds cases whose documents cannot be published, in the
same format, and is not tracked. A check marked `"open"` records a known
failure without failing the run, and the run says when it starts passing.
[`eval/tough/README.md`](https://github.com/fluree/fluree-doc-parse/blob/main/eval/tough/README.md)
has the case format.

## Scanned PDFs

```bash
uv run eval/scanned/make_scans.py               # generate the set (not tracked)
eval/scanned/check.py --models <ocr-assets>     # recall of gold facts, with and without OCR
eval/scanned/check.py --no-ocr                  # routing only, no assets
```

Synthetic image-only pages under nine scan profiles, and born-digital pages
with a pasted screenshot, each with the facts drawn on it. The run without OCR
fails if an image-only page is not in `doc:unreadPages` as `Scanned`, a
born-digital page is, a pasted screenshot is not in `doc:unreadRegions`, a
pasted page's headings are not its gold's word for word, or a fact only pixels
hold reaches the text; `--baseline eval/scanned/baseline.json` fails on a
fact the OCR run used to find, a heading of the gold a page used to read as
one, a heading other than the gold's a page did not read, a page it does not
list or one it lists that the set does not hold, or a list it lacks.
[`eval/scanned/README.md`](https://github.com/fluree/fluree-doc-parse/blob/main/eval/scanned/README.md)
has the details, and [Scanned PDFs](../benchmarks/scanned.md) the numbers.

## Fuzzing

Every reader that takes a file's bytes has a cargo-fuzz target under
`fuzz/`, a workspace of its own that needs a nightly toolchain. A target
fails when a reader panics, hangs, runs out of memory or makes more than
one document's budget allows (see [Limits](../reference/limits.md)).

```bash
cd fuzz
cargo +nightly fuzz run xls corpus/xls seeds/xls -O -a -s none -- \
    -rss_limit_mb=2048 -timeout=10 -max_len=262144 -max_total_time=600
```

[`fuzz/README.md`](https://github.com/fluree/fluree-doc-parse/blob/main/fuzz/README.md)
lists the targets, how their seeds are made, and how a crash becomes a
regression test.

## Adding tests

- **A bug fix** gets a test reproducing the geometry that caused it, not the
  PDF that contained it. Ship the minimal case; the document may not be
  redistributable, and the geometry is the actual finding.
- **A fix found on a real document** also gets a [tough case](#tough-cases)
  for that document: the unit test pins the geometry, the case pins the
  document. Write the check against the output the fix produces, and confirm
  it fails on the binary before the fix.
- **A new detector** needs both a positive case and the false positive you
  expect it to avoid. Most rejected ideas in the ablation table failed on
  false positives, not on recall.
- **A format change** needs a check that the [offset
  contract](../concepts/text-projection.md) still holds — every
  `nif:beginIndex`/`nif:endIndex` must resolve to exactly its `nif:isString`
  (a table cell's `doc:cellValue`) in the text projection.

## Benchmark scores

The 200-document scores require `opendataloader-bench` cloned separately; the
adapters are in `bench-adapter/`. Model-tier scores reproduce **without a
GPU** from the committed caches in `eval/*-cache/`. See
[Evaluation](evaluation.md).
