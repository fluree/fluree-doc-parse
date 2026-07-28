# fdoc triage

Per-page routing verdicts: which pages need model escalation, and why.

```bash
fdoc triage <FILE|DIR>
fdoc route  <FILE|DIR>     # alias
```

Every page is measured — glyph counts, Unicode resolution, image coverage —
and reported as deterministic or as needing escalation. Nothing is converted;
this is the cheap look before you spend.

## Over a directory

```
$ fdoc triage ./corpus/
datasheet-a  TABLE   p1 Fragmented cols=[3, 9]; p3 MergedRows cols=[5]
datasheet-a  ROUTE   179.1ms  p25 Regions(1) glyphs=1144 img=0.36 [(56, 251, 555, 550)]
datasheet-c  ROUTE    86.9ms  p18 BrokenText glyphs=670 unicode=0.85 img=0.00
datasheet-b  ROUTE   149.9ms  p34 Regions(1) glyphs=196 img=1.00 [(75, 255, 536, 489)]

268 tables, 10 with disagreeing structure (3.7%) — model-tier candidates

6 files, 223 pages, 5 routed (2.2%), deterministic parse 733.5ms total
```

**The last two lines are the point.** The escalation rate is the number that
prices a deployment: it is what fraction of your pages will cost GPU seconds
instead of CPU milliseconds. The bench corpus is adversarial at 22%; this
corpus runs 2.2%. Your document mix decides, and it is the single number most
worth measuring before sizing anything.

## Verdicts

| verdict | meaning |
|---|---|
| `Deterministic` | the text layer is readable; no escalation |
| `Scanned` | the page is pixels — few or no glyphs against high image coverage |
| `NearBlank` | too little content to judge from the text layer |
| `BrokenText` | glyphs resolve to unusable Unicode (broken CID fonts) |
| `Regions(n)` | `n` raster regions the text layer does not cover |
| `Fragmented` | a table's detected columns disagree with themselves |
| `MergedRows` | a table's rows do not segment cleanly |

Four line kinds, and they answer different questions:

| line | says |
|---|---|
| `ROUTE` | a page or region the text layer cannot read |
| `TABLE` | a table whose detected structure disagrees with itself |
| `HEADING` | a page whose hierarchy rests on nothing but font size |
| `COLUMN` | a page laid out in panels a page-wide projection cannot see |

`ROUTE`, `TABLE` and `HEADING` escalate on their own evidence once a reader is
configured. **`COLUMN` does not**, and the report says so where it fires:

```
report  COLUMN  p8 3 column(s) found, 2 gutter(s) visible only in a band covering 44% of rows

COLUMN pages do not escalate by default — `fdoc config set escalation.on_column_doubt true`
```

That default is a measurement rather than caution — see
[`fdoc config`](config.md#pages-that-read-across-their-panels). A `COLUMN`
page still parses; it reads across its panels instead of down them.

See [the router](../design/router.md) and [Escalation and
arbitration](../concepts/escalation.md).

## Single file

For one file every page prints, including the ones that did not route,
because threshold tuning needs to see the near-misses:

```
$ fdoc triage report.pdf
report  deterministic  p1 glyphs=333 unicode=1.00 img=0.00 n_img=0 boxes=[] page=595x842
```

Set `FDOC_ROUTE_VERBOSE=1` to get the same per-page detail over a directory.

## Page numbers are the printed ones

`p1` is the first page, because this report exists to be handed to
[`--pages`](convert.md), which is 1-based. The 0-based index is what
`doc:pageIndex` carries in the output — an index into a sequence is a
different thing from a page's printed number.

## Reading the signals

| signal | what it means |
|---|---|
| `glyphs` | glyph count on the page |
| `unicode` | fraction of glyphs resolving to a Unicode value |
| `img` | image coverage as a fraction of page area |
| `n_img` | number of images |
| `boxes` | routed region rectangles, in PDF units |

A page with `img=1.00` and a low glyph count is a scan. A page with plenty of
glyphs and `unicode=0.85` has font problems — the layout may be perfect and
the text still worthless.

## What it does not do

It does not run any model and opens no network connection, whatever is
configured. `triage` reports what *would* escalate, which is what makes it the
cheap look before you spend — `fdoc convert` is where the reading happens, in
the same command, once a provider is set up. See
[`fdoc config`](config.md) and [Wiring the escalation
tiers](../integration/escalation-tiers.md).
