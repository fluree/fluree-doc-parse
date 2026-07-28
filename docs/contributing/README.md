# Contributing

- [Dev setup](dev-setup.md) — build, run, debug
- [Tests](tests.md) — what runs, and what to add
- [Evaluation](evaluation.md) — the benchmark harness and the discipline
  around its numbers
- [Scoreboard](scoreboard.md) — **start here for accuracy work**: where the
  engine stands, how to reproduce it in ten minutes, which twenty documents
  carry the deficit, and what has already been tried and measured worse

## The one rule worth stating up front

**A change that moves a measured number updates that number in the same
commit, with a note on why.** A silently drifting baseline is worse than no
baseline — it converts every future measurement into an argument about which
number was real.

`eval/TEST_PLAN.md` is where those numbers live.

## The second rule

**Negative results are kept.** When an idea is tried and measured worse, it
goes into the ablation table with its number rather than being quietly
deleted. Several ideas in there are plausible enough to be proposed again, and
the record is what stops that costing a second afternoon.

## Documentation

If your change alters behavior these docs describe, update them in the same
changeset. In particular:

| you changed | update |
|---|---|
| an output format | [`docs/formats/`](../formats/README.md) |
| a CLI flag | [`docs/cli/`](../cli/README.md) |
| the sidecar contract | [`docs/integration/sidecar-formats.md`](../integration/sidecar-formats.md) |
| the supported Rust API | [`rust-library.md`](../getting-started/rust-library.md) **and** `examples/library_usage.rs` |
| a measured number | `eval/TEST_PLAN.md` |

New top-level pages go in `docs/SUMMARY.md`, or mdBook will not link them.
