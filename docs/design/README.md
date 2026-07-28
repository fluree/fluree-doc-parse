# Design

How the deterministic engine actually works. This section documents
**internals** — useful for understanding behavior or contributing, but the
Rust modules behind it are [not a compatibility
surface](../getting-started/rust-library.md#what-is-a-compatibility-surface).

- [The pipeline](pipeline.md) — the stages and why their order is load-bearing
- [Reading order and columns](reading-order.md) — rotation buckets and gutters
- [Headings](headings.md) — the detector cascade, and the signal nobody uses
- [Tables](tables.md) — grids from geometry, including grids that look absent
- [Page furniture](furniture.md) — headers, footers, watermarks
- [The router](router.md) — what earns escalation

## A note on how these decisions were made

Nearly every constant and threshold here came from dumping the geometry of
real documents rather than from reasoning about what a document should look
like. Where a page says a threshold is derived per document rather than fixed,
that is usually because a fixed one was tried first and measured worse.

The ideas that were tried and **rejected** are recorded in
`eval/TEST_PLAN.md` — several are plausible enough to be proposed again, and
the measurement is the only thing that settles them. See
[Evaluation](../contributing/evaluation.md).
