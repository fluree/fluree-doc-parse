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

## Adding tests

- **A bug fix** gets a test reproducing the geometry that caused it, not the
  PDF that contained it. Ship the minimal case; the document may not be
  redistributable, and the geometry is the actual finding.
- **A new detector** needs both a positive case and the false positive you
  expect it to avoid. Most rejected ideas in the ablation table failed on
  false positives, not on recall.
- **A format change** needs a check that the [offset
  contract](../concepts/text-projection.md) still holds — every
  `nif:beginIndex`/`nif:endIndex` must resolve to exactly its `nif:isString`
  in the text projection.

## Benchmark scores

The 200-document scores require `opendataloader-bench` cloned separately; the
adapters are in `bench-adapter/`. Model-tier scores reproduce **without a
GPU** from the committed caches in `eval/*-cache/`. See
[Evaluation](evaluation.md).
