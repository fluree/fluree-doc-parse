# Loading into a Fluree ledger

The [`doco`](../formats/doco.md) output is insertable as-is. The JSON-LD
context carries the DoCO, NIF and pattern ontologies, and `po:contains` is
IRI-coerced so containment edges are real references rather than strings.

```bash
fdoc convert report.pdf -f doco --doc-iri urn:doc:finance-q3-report -o report.jsonld
```

## Choosing IRIs

Two flags, two different jobs — and mixing them up is the common mistake.

| flag | names | changes per extraction? |
|---|---|---|
| `--doc-iri` | the **document** the nodes came from | no, ever |
| `--base-iri` | the **nodes** minted by this run | only if you want each run's nodes named apart |

`--doc-iri` stamps nodes with `doc:sourceDocument → <iri>`. That is the tag
re-extraction retracts by. Without `--base-iri`, the nodes are named after it
too: `urn:doc:finance-q3-report-element-12`.

With neither flag, nodes are named after the file:
`urn:fluree-doc-parse:report-661511bb2b30-element-12`, the stem and the first
twelve hex digits of the file's SHA-256. The hash keeps `report.pdf` apart
from `report.docx`, and from every other `report.pdf` on a shared drive.

### Keep the namespaces few

Fluree stores an IRI as a namespace and a name, split at the last `/`, `#` or
`:`, and encodes each distinct namespace once. Minted IRIs never add one of
those to the base, so a document's nodes cost exactly the namespace its
document IRI sits in. What decides the count for a corpus is how document IRIs
are shaped:

| document IRIs | namespaces |
|---|---|
| `urn:doc:<id>`, `https://example.org/doc/<id>` | one, for the whole corpus |
| `https://example.org/doc/<id>/` | one per document |
| `file:///Volumes/Share/Finance/2024/report.pdf` | one per directory crawled |

Give documents flat identifiers and keep a path or a location as data about
them. For the same reason, put a run's version inside the last segment of a
`--base-iri` (`https://example.org/doc/report-v2`), never as a segment of its
own (`https://example.org/doc/report/v2`).

## Re-extraction without a diff

The problem: you re-run extraction after an engine upgrade, and the new graph
overlaps the old one. Elements shift, offsets move, and computing what changed
is expensive and error-prone.

The answer is not to diff. Retract everything tagged with the document IRI,
and the table cells one `po:contains` below it, then insert the new graph:

```sparql
DELETE { ?s ?p ?o }
WHERE  { ?x doc:sourceDocument <urn:doc:finance-q3-report> .
         ?x po:contains? ?s .
         ?s ?p ?o }
```

Cells are the one kind of node not stamped: a cell is always directly inside
its table, which is, and cells are most of a table-heavy graph.

Then insert `report.jsonld`. The ledger holds exactly one extraction of that
document, and because Fluree is immutable the previous one remains queryable
at its commit — you get history without maintaining it.

Keep `--doc-iri` **stable across runs** for this to work. Pass a `--base-iri`
per run if you want the two extractions' nodes to have distinct identities
within that history.

## Finding duplicates

Every document node carries `doc:sha256`, the hash of the bytes it was read
from, and `doc:sourceName`, the file name it was given. Copies of one file
across a crawl are the hashes that repeat:

```sparql
SELECT ?hash (COUNT(?d) AS ?copies) (GROUP_CONCAT(?name; separator=", ") AS ?names)
WHERE { ?d a doc:Document ; doc:sha256 ?hash ; doc:sourceName ?name }
GROUP BY ?hash
HAVING (COUNT(?d) > 1)
```

A crawler can also hash a file before converting it, and skip the conversion
when that hash is already in the ledger. An email's attachments carry their
hashes too, in `doc:attachments`, so the same file forwarded in fifty threads
is one value.

## What you can query

The graph is shaped for the questions people actually ask of documents:

- **Section-scoped search** — `po:contains` chains from `doc:Document` down
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
