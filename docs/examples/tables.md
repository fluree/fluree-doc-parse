# Tables to rows and spans

Two different jobs hide behind "extract the tables", and they want opposite
things from a merged cell.

- **Analysis.** You want a DataFrame. Every row must stand on its own, so a
  cell spanning four rows should appear in all four.
- **Fidelity.** You want to reproduce the table. A cell spanning four rows is
  *one* cell, and repeating it four times is wrong.

Pick the output that matches the job, because two of them have already done
the work:

| you want | use |
|---|---|
| rows that stand on their own | `-f doco` |
| real `rowspan` / `colspan` | `-f xhtml` |
| the grid as detected, plus flags | `-f json` |

`-f md` prints the same grid as `-f json`, which makes it the quickest way to
eyeball a table and the wrong input for analysis that needs self-contained
rows.

One asymmetry decides it when both would do. `doco` cannot lose a cell — its
cells are independent nodes, so there is no shape it fails to represent.
`xhtml` can: merge flags describe neighbours, neighbours can describe an L,
and HTML cannot tile one. Where that happens the position is dropped, text and
all. Rare, but silent — see [where a span cannot be
drawn](../formats/xhtml.md#where-a-span-cannot-be-drawn).

## A quick look at what is there

```python
import re
import subprocess


def md_tables(path):
    """Yield each pipe table as a list of rows, header first."""
    md = subprocess.run(["fdoc", "convert", path, "-f", "md"],
                        capture_output=True, check=True).stdout.decode()
    block = []
    for line in md.split("\n") + [""]:
        if line.startswith("|"):
            if not re.fullmatch(r"\|(\s*-+\s*\|)+", line):     # separator row
                block.append([c.strip() for c in re.split(r"(?<!\\)\|", line)[1:-1]])
        elif block:
            yield block
            block = []
```

```python
import pandas as pd

for rows in md_tables("report.pdf"):
    df = pd.DataFrame(rows[1:], columns=rows[0])
```

Split on `(?<!\\)\|` rather than plain `|`. A cell whose text contains a pipe
carries it escaped as `\|`, and splitting naively turns that cell into two.

Remember that these rows are the grid as detected: a merged cell's value
appears once, and the rows it spans are empty. For a DataFrame you can
actually aggregate over, take the next section instead.

## Rows that stand on their own

Where you want each cell to describe itself — for a DataFrame, for entity
extraction, or to load into a graph — `-f doco` emits one `doc:TableCell` per
data cell carrying the merged value already resolved, along with
`doc:rowHeader`, `doc:columnHeader` and `doc:sectionLabel`.

```python
def grids(path):
    graph = json.loads(run(["fdoc", "convert", path, "-f", "doco"]))["@graph"]
    by_id = {n["@id"]: n for n in graph}
    for table in (n for n in graph if n.get("@type") == "doco:Table"):
        cells = [by_id[i] for i in table.get("po:contains", [])
                 if by_id[i].get("@type") == "doc:TableCell"]
        if not cells:
            continue
        n_rows = max(c["doc:rowIndex"] for c in cells) + 1
        n_cols = max(c["doc:columnIndex"] for c in cells) + 1
        grid = [["" for _ in range(n_cols)] for _ in range(n_rows)]
        for c in cells:
            grid[c["doc:rowIndex"]][c["doc:columnIndex"]] = c["doc:cellValue"]
        yield grid
```

Two properties of the graph shape the code above:

- **Empty cells are not emitted.** Size the grid from the highest row and
  column index and fill the gaps, rather than taking each row's own width —
  otherwise rows come out ragged.
- **Header rows are not cells.** `doc:rowIndex` counts from the first data
  row, and the header travels on each cell as `doc:columnHeader`. Row 0 here
  is not row 0 in `cells`.

## Spans for fidelity

If HTML is your destination, `-f xhtml` already emits real `rowspan` and
`colspan` and there is nothing to compute:

```xml
<tr><th colspan="3">PIN</th><th rowspan="2"></th></tr>
<tr><td rowspan="2">Differential input voltage</td><td>–32</td><td>V</td></tr>
```

Compute them yourself only when the spans have to land in some other
structure. An origin cell is one that continues neither the cell above it nor
the cell to its left; its rowspan is how far the continuation flags run down,
its colspan how far they run right.

```python
import html
import json
import subprocess


def tables(path):
    out = subprocess.run(["fdoc", "convert", path, "-f", "json"],
                         capture_output=True, check=True).stdout
    return [e for e in json.loads(out) if e["type"] == "doco:Table"]


def spans(el):
    """Yield (row, col, rowspan, colspan, value) for every origin cell."""
    rows = el["cells"]
    if not rows:
        return
    cols = len(rows[0])
    n = len(rows) * cols
    down = el.get("merged_down") or [False] * n
    left = el.get("merged_left") or [False] * n

    for r, row in enumerate(rows):
        for c in range(cols):
            i = r * cols + c
            if down[i] or left[i]:
                continue
            rowspan = 1
            while (r + rowspan) < len(rows) and down[(r + rowspan) * cols + c]:
                rowspan += 1
            colspan = 1
            while (c + colspan) < cols and left[r * cols + c + colspan]:
                colspan += 1
            yield r, c, rowspan, colspan, row[c]
```

Both flag arrays are **flat**, `rows × cols` booleans, indexed `r * cols + c`.

### Check the tiling before you trust it

Not every merge decomposes into rectangles. A cell that merges downward whose
continuation row also merges left describes an L, and no HTML table can tile
one — the spans end up leaving a position uncovered or claiming it twice.
Check first, and fall back to a flat grid when the check fails:

```python
def tiles(el):
    """True when the spans cover every grid position exactly once."""
    rows = el["cells"]
    if not rows:
        return True
    cols = len(rows[0])
    seen = {}
    for r, c, rs, cs, _ in spans(el):
        for i in range(r, r + rs):
            for j in range(c, c + cs):
                seen[(i, j)] = seen.get((i, j), 0) + 1
    return len(seen) == len(rows) * cols and all(v == 1 for v in seen.values())
```

```python
def to_html(el):
    rows = el["cells"]
    header = el.get("header_rows") or 1
    cells = (spans(el) if tiles(el) else
             ((r, c, 1, 1, v) for r, row in enumerate(rows)
              for c, v in enumerate(row)))
    by_row = {}
    for r, c, rs, cs, v in cells:
        by_row.setdefault(r, []).append((c, rs, cs, v))

    out = ["<table>"]
    for r in range(len(rows)):
        out.append("<tr>")
        for c, rs, cs, v in sorted(by_row.get(r, [])):
            tag = "th" if r < header else "td"
            attr = f' rowspan="{rs}"' if rs > 1 else ""
            attr += f' colspan="{cs}"' if cs > 1 else ""
            out.append(f"<{tag}{attr}>{html.escape(v)}</{tag}>")
        out.append("</tr>")
    out.append("</table>")
    return "\n".join(out)
```

Escape the cell text. It is raw document text, and technical documents are
full of `<`, `>` and `&`.

## Fields that matter

| field | notes |
|---|---|
| `header_rows` | how many leading rows are headers; **absent means 1**, not zero |
| `sub_headers` | row indices that are one full-width banner splitting the body into sections |
| `merged_down` | flat `rows × cols`: this cell continues the one above |
| `merged_left` | flat `rows × cols`: this cell continues the one to its left |

`header_rows` is absent on model-supplied tables, where the header count was
never measured. Treat absent as 1.

Do not skip `sub_headers`. A banner row reads as a data row with one populated
column, which silently corrupts a DataFrame — the row `Europe` followed by
three empty columns is not a measurement.

## Finding the tables worth checking

Not every detected table is equally certain. `fdoc triage` reports the ones
whose structure disagrees with itself, which are the candidates for a
[model tier](../integration/escalation-tiers.md):

```bash
fdoc triage ./corpus/ | grep TABLE
```

```
report-a   TABLE   p1 Fragmented cols=[3, 4]; p7 Fragmented cols=[4, 4, 5]; p12 MergedRows cols=[3]
report-b   TABLE   p0 Fragmented cols=[3, 9]; p2 MergedRows cols=[5]; p15 MergedRows cols=[2]
```

One line per document, listing the pages whose grids disagree with themselves.
Feed those pages to a table-structure model and leave the rest alone — see
[Wiring the escalation tiers](../integration/escalation-tiers.md).
