# Table cells: fixtures and check

Each fixture is one document rendered from one model into every format it
exists in: HTML, Markdown, DOCX, XLSX, and a PDF printed by headless Chrome
from the HTML (the `.html` beside a `.pdf` shows what the PDF means). They
are synthetic receipts, invoices, price lists, rosters and schedules, with
fictional merchants and people, laid out in the shapes that trouble table
readers:

| family | shape |
|---|---|
| `line-table` | one table of items under a header |
| `totals-block` | a line table, then label-and-amount totals beneath it |
| `two-tables` | a key/value block and a line table, ruled or not |
| `field-value` | one item as a two-column list of fields |
| `kv-lines` | fields as plain monospace lines, no table at all |
| `sparse` | line tables with empty cells |
| `transposed` | items across the header, properties down the first column |
| `merged-header` | a stacked header: a banner spanning the columns it names |
| `section-rows` | full-width bands labelling the rows beneath them |
| `plain` | generic entity tables under their own header words |

`probe-*` files are hand-written cases for a single behaviour (a `<caption>`,
a `<th colspan>` band). The PDF probes for tables drawn without column rules
(`probe-statement`, `probe-statement-far`, `probe-booktabs`: dot leaders,
underlines under figures, banners, centred unit lines, wrapped labels) are
drawn by `make_probes.py` (`uv run eval/tables/make_probes.py`) in the
geometry of the published pages that a real-document audit found unread, so
no third-party page is kept here.

## What is checked

`<stem>.expect.json` holds the document's blocks in reading order (text, and
tables with each column's expected header path) and the parts that must be
found:

* a body cell's value in a `doc:TableCell` with the `doc:columnHeader` its
  column's header gives it (under a stacked header the labels from the top
  down, joined with `" / "`), `doc:rowHeader` the row's first cell,
  `doc:sectionLabel` the band above it, and offsets that slice `fdoc convert
  -f text` to the value;
* a text span inside its block, the blocks found in reading order;
* a header label on its header line of the table's text;
* a section band on a line of its own inside the table's text.

A file is **kept** when every part marked `needed` is found. A table marked
`all_cells` needs every non-empty body cell.

## Running it

```
cargo build --release
eval/tables/check.py                                   # all fixtures, by family and format
eval/tables/check.py eval/tables/fixtures/w1-d029.pdf  # one file, every gap shown
eval/tables/check.py --baseline eval/tables/baseline.json
```

With `--baseline` the check fails when a file kept in the baseline is dropped
now, and lists the files newly kept. When a change keeps more, record it with
`--write-baseline eval/tables/baseline.json` in the same commit. `--fdoc` (or
`FDOC_BIN`) checks another binary.

Not every fixture passes: `baseline.json` records the PDFs still dropped,
which are the open work.

The full corpus these fixtures were drawn from, about 1,900 documents and
8,600 files, is kept outside the repository. Where it is present in
`eval/tables-full/`, `eval/tables/check.py eval/tables-full --baseline
eval/tables-full/baseline.json` sweeps it the same way in about a minute.
