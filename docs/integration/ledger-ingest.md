# Loading into a Fluree ledger

The [`doco`](../formats/doco.md) output is insertable as-is. The JSON-LD
context carries the DoCO, NIF and pattern ontologies, and `po:contains` is
IRI-coerced so containment edges are real references rather than strings.

```bash
fdoc convert report.pdf -f doco \
  --base-iri https://example.org/docs/report \
  --doc-iri  https://example.org/docs/report \
  -o report.jsonld
```

## Choosing IRIs

Two flags, two different jobs — and mixing them up is the common mistake.

| flag | names | changes per extraction? |
|---|---|---|
| `--base-iri` | the **elements** minted by this run | yes, if you want history |
| `--doc-iri` | the **document** they came from | no, ever |

`--base-iri` defaults to `urn:fluree-doc-parse:<stem>`, which is fine for a single
corpus and wrong the moment two documents share a filename. Give it a real
namespace.

`--doc-iri` stamps every element with `doc:sourceDocument → <iri>`. That is
the tag re-extraction retracts by.

## Re-extraction without a diff

The problem: you re-run extraction after an engine upgrade, and the new graph
overlaps the old one. Elements shift, offsets move, and computing what changed
is expensive and error-prone.

The answer is not to diff. Retract everything tagged with the document IRI,
then insert the new graph:

```
delete { ?s ?p ?o }
where  { ?s doc:sourceDocument <https://example.org/docs/report> .
         ?s ?p ?o }
```

Then insert `report.jsonld`. The ledger holds exactly one extraction of that
document, and because Fluree is immutable the previous one remains queryable
at its commit — you get history without maintaining it.

Keep `--doc-iri` **stable across runs** for this to work. Vary `--base-iri`
per run if you want the two extractions' elements to have distinct identities
within that history.

## What you can query

The graph is shaped for the questions people actually ask of documents:

- **Section-scoped search** — `po:contains` chains from `doco:Document` down
  through `doco:Section`, so "entities mentioned under this heading" is a
  traversal rather than a coordinate comparison.
- **Cell-addressed tables** — every cell is a `doc:TableCell` with
  `doc:rowHeader` and `doc:columnHeader` denormalized onto it, so
  "the Supply voltage row of the LM358B column" needs no grid reconstruction.
- **Filtering by how it was known** — `doc:evidence` records which signal
  classified each element, so a query can exclude everything that came from a
  weak heading detector, or review everything an escalated region contributed.
- **Locating a mention on a page** — `nif:beginIndex` / `nif:endIndex` plus
  `doc:pageIndex` and `doc:bbox`. See [Entity overlay](entity-overlay.md).

## Mixed corpora

All five input formats produce the same graph shape, so a corpus of PDFs,
Word documents and Markdown notes lands in one ledger with one schema. The
only structural difference is that non-PDF elements have no `doc:bbox` — see
[Measured vs declared
structure](../concepts/geometry-vs-declared.md).

## Scale

`-f doco` is verbose by design: a table becomes one node per cell. A
40-page datasheet produced ~5,300 `doc:TableCell` nodes in testing. That
is the price of addressability; if you do not need cell-level queries,
[`-f json`](../formats/json.md) carries the same tables in a fraction of the
bytes.
