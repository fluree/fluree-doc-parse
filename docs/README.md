# fluree-doc-parse

Adaptive document parsing: a deterministic Rust engine first, with
model-arbitrated upgrade tiers a document walks through only as far as it
needs.

PDF, Markdown, HTML, DOCX and PPTX all converge on **one element model**, so
every source produces the same output — the same Markdown, the same DoCO
graph, the same character offsets. PDF is the geometric path, where structure
is inferred from where glyphs and ruling lines actually sit. The others
declare their structure, so those readers map rather than measure.

## What you get

- **Deterministic first.** The CPU tier extracts text and structure in about
  7 ms per document and outscores every non-ML engine on
  [opendataloader-bench](benchmarks/README.md) by itself. No model, no
  GPU, no network.
- **Escalation that is earned, not configured.** Each tier's own output is the
  sensor that decides whether the next tier runs — per document, per region,
  per table. See [the tier model](getting-started/tiers.md).
- **Models arbitrate structure; the page tier owns its page.** Where the
  deterministic pass produced a reading, a model's replaces it only when their
  *shapes* disagree. Where the deterministic pass is what failed, the reading
  owns the page — there is no good text to prefer to it. See
  [escalation and arbitration](concepts/escalation.md).
- **Coordinates that survive to the consumer.** Every PDF element carries its
  page and bounding box, and every character in the text projection maps back
  to a rectangle on a rendered page — which is what makes
  [entity overlay](integration/entity-overlay.md) possible.
- **A graph, not a blob.** The [DoCO output](formats/doco.md) is JSON-LD with
  the DoCO, NIF and pattern ontologies, insertable into a
  [Fluree](https://flur.ee) ledger as-is.
- **Honest about what it doesn't know.** [`fdoc triage`](cli/triage.md) tells
  you which pages would escalate and why, before you spend anything on them.

## Start here

- **Install it** → [Install](getting-started/install.md)
- **Convert your first document** → [Quickstart](getting-started/quickstart.md)
- **Call it from Python or Node** → [Examples](examples/README.md)
- **Understand the tiers** → [The tier model](getting-started/tiers.md)
- **Pick an output format** → [Output formats](formats/README.md)
- **Embed the crates** → [Using fluree-doc-parse as a Rust library](getting-started/rust-library.md)

## Explore the docs

- [Concepts](concepts/README.md) — the element model, measured vs declared
  structure, escalation, char offsets, provenance
- [Output formats](formats/README.md) — Markdown, XHTML, JSON, DoCO JSON-LD,
  text; what each one guarantees
- [Input formats](inputs/README.md) — PDF, DOCX, PPTX, HTML, Markdown, and
  what each reader can and cannot know
- [Examples](examples/README.md) — chunking for retrieval, locating text on
  the page, tables, batch runs, sizing a deployment, form fields
- [CLI reference](cli/README.md) — every `fdoc` command, flag by flag
- [Integration](integration/README.md) — wiring the model tiers, the sidecar
  contract, ledger ingest, entity overlay
- [Design](design/README.md) — internals: the pipeline, reading order,
  headings, tables, furniture, the router
- [Reference](reference/README.md) — vocabulary, environment variables, crate map
- [Contributing](contributing/README.md) — dev setup, tests, evaluation

## A note on measurement

Every accuracy and latency number in these docs traces to a check in
[`eval/TEST_PLAN.md`](contributing/evaluation.md), which also records the
ablations and the **negative results** — the ideas that looked good and
measured worse. Several of them are documented precisely because they are
plausible enough to be proposed again.
