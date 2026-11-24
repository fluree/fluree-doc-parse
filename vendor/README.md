# Vendored crates

Crates patched in through `[patch.crates-io]` in the workspace `Cargo.toml`,
each until its change is available upstream. Each is the published crate as
crates.io serves it, under its own license, with the change below and nothing
else.

## `oar-ocr-core` 0.11.0

License: Apache-2.0 (<https://github.com/greatv/oar-ocr>).

One change, in `src/core/inference/ort_infer_builders.rs`
(`arena_shrinkage_run_options`): `OrtSessionConfig::with_arena_shrinkage(true)`
with no CUDA execution provider shrinks the CPU arena after each run
(`memory.enable_memory_arena_shrinkage = cpu:0`), where upstream ignores it.

Why: the OCR engine (`fluree-doc-vision`, `oar` feature) shares one CPU arena
among its pipelines. ONNX Runtime's arena grows to the largest image read and
keeps that memory; without shrinkage, a process that read four 12 Mpx photos
at once holds about 9 GB for the rest of its life, against about 1 GB with
it, at the same speed.

To drop this directory: once an `oar-ocr-core` release shrinks the CPU arena
(or takes run options), move to it and remove the `[patch.crates-io]` entry.
