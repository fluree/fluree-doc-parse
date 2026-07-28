# Quickstart

## Convert a document

```bash
fdoc convert report.pdf
```

Markdown to stdout. That is the default because it is the format you can read
without tooling; every other format is a different projection of the same
elements.

## Ask for a different shape

```bash
fdoc convert report.pdf -f json     # elements with bounding boxes
fdoc convert report.pdf -f doco     # JSON-LD graph, ledger-ready
fdoc convert report.pdf -f xhtml    # h1-h6 / p / ul / table fragments
fdoc convert report.pdf -f text     # plain text, the offset baseline
```

[Output formats](../formats/README.md) explains what each one guarantees.
The short version: `md` and `xhtml` are for humans and HTML consumers, `json`
is elements plus geometry, `doco` is a graph with containment and char
offsets, and `text` is the exact string those offsets index into.

## Other sources, same output

```bash
fdoc convert notes.md    -f doco
fdoc convert report.docx -f doco
fdoc convert page.html   -f doco
fdoc convert deck.pptx   -f doco
```

The graph has the same shape from all five sources. One difference matters:
non-PDF elements have **no `bbox` field at all**, because those formats
declare structure rather than placing it. See [Measured vs declared
structure](../concepts/geometry-vs-declared.md).

## Batch

```bash
fdoc convert ./corpus/ --out-dir ./out -j 8
```

One output file per input, eight workers. `-j 0` uses one per core. Output
names are derived from input stems and disambiguated when stems collide.

## Read from stdin

```bash
cat report.pdf | fdoc convert -
```

Stdin is PDF only — the other readers need a file to identify the format.

## Look before you spend

```bash
fdoc triage report.pdf
```

Per-page verdicts: which pages the deterministic tier cannot read, which
regions are pixels, and — over a directory — the escalation rate, which is the
number that prices a deployment. See [`fdoc triage`](../cli/triage.md).

## What next

- [Examples](../examples/README.md) — calling it from Python or Node, and
  recipes for chunking, highlighting, tables and batch runs
- [The tier model](tiers.md) — what escalation buys and what it costs
- [Output formats](../formats/README.md) — the capability matrix
- [`fdoc convert`](../cli/convert.md) — every flag
