# JSON

```bash
fdoc convert report.pdf -f json
```

A flat array of [elements](../concepts/element-model.md) in reading order.
**Not JSON-LD** — for a graph, use [`doco`](doco.md).

```json
[
  {
    "id": "elem-00001",
    "type": "doco:Paragraph",
    "page": 0,
    "bbox": { "x0": 91.16856, "y0": 185.6449, "x1": 144.9965, "y1": 195.0893 },
    "text": "小田切 亘",
    "provenance": "rust",
    "evidence": "layout"
  }
]
```

## Fields

| field | always? | notes |
|---|---|---|
| `id` | yes | `elem-NNNNN`, emission order |
| `type` | yes | DoCO class |
| `page` | yes | 0-based; `0` for formats without pages |
| `text` | yes | NFKC-normalized, super- and subscripts kept |
| `provenance` | yes | which reader: `rust`, `vlm`, `markdown`, `html`, `docx`, `pptx`, `xlsx`, `vtt`, `srt`, `eml`, `msg` |
| `evidence` | yes | which signal classified it |
| `bbox` | **PDF only** | omitted entirely otherwise |
| `level` | headings | 1–6 |
| `figure` | figures | shared id for fragments of one drawing |
| `cells` | tables | row-major, **as detected** |
| `header_rows` | tables | `None` → treat as 1 |
| `sub_headers` | tables | full-width banner row indices |
| `merged_down` | tables | row-major continuation flags |
| `merged_left` | tables | row-major continuation flags |
| `datums` | workbook tables | same shape as `cells`: each cell's stored value as `{type, value}` (`xsd:decimal`, `xsd:date`, …), or `null` for text |
| `links` | where the source has any | hyperlinks over this element's text |
| `turn` | [transcripts](../inputs/transcripts.md) | `{speaker?, start_ms, end_ms}`: who spoke and when |
| `message` | [email](../inputs/email.md), on the element opening each message | the message's header: `from`, `to`, `cc`, `date`, `subject`, … |
| `resumes` | email, where a message goes on after a quote nested in it | the `id` of the element that opened that message |
| `signature` | email | `true` on each element of a message's signature |

Absent fields are **omitted**, not null. `bbox` in particular: an element with
no geometry has no `bbox` key, and this is load-bearing — see [Measured vs
declared structure](../concepts/geometry-vs-declared.md).

`merged_down` and `merged_left` are **flat** arrays of `rows × cols` booleans;
the flag for row `r`, column `c` is at index `r * cols + c`.

A table's `text` joins cells with ` | `. This is the one format where that is
true — [`text`](text.md) and [`doco`](doco.md) join with tabs, and those are
the ones char offsets index into, so a pattern written against `text` here
will not match there.

## Links carry their anchor

`links` is the one place every link survives, including the ones the markup
formats cannot express.

```json
"links": [
  { "uri": "https://www.sec.gov/…", "begin": 4, "end": 14 },
  { "page": 11, "begin": 25, "end": 34 }
]
```

`uri` is an address outside the document; `page` is a 0-based index into this
one, the same space as the element's own `page` field. Exactly one of the two
is present.

`begin`/`end` are **char offsets into this element's `text`** — not into
[`text`](text.md), which is what [`doco`](doco.md) offsets index. They are
absent together when the annotation covers something with no text of its own:
an image, or a whole table cell. The link still belongs to the element; only
its extent inside it is unknown.

The array arrives sorted by `begin` and non-overlapping, so it can be spliced
into the text in one pass.

## Transcript turns carry who and when

```json
{
  "id": "elem-00004",
  "type": "doco:Paragraph",
  "page": 0,
  "text": "Ada Park: I want to say this is my last review.",
  "turn": { "speaker": "Ada Park", "start_ms": 272404, "end_ms": 279512 },
  "provenance": "vtt",
  "evidence": "vtt"
}
```

`speaker` is absent for captions that name no one. When present, it also
opens `text`, followed by `": "`. Times are milliseconds from the start of
the recording. See [Transcripts](../inputs/transcripts.md).

## Email messages carry their header

```json
{
  "id": "elem-00005",
  "type": "doco:Paragraph",
  "page": 0,
  "text": "On Fri, Jul 17, 2026 at 10:05 AM Kai Moreno <kai@example.com> wrote:",
  "message": {
    "from": [ { "name": "Kai Moreno", "address": "kai@example.com" } ],
    "date": "2026-07-17T10:05:00",
    "quoted": true
  },
  "provenance": "eml",
  "evidence": "eml"
}
```

`message` is on the first element of each message in an email; the elements
after it, up to the next that carries `message` or `resumes`, are that
message's body. `resumes` marks where a message goes on after a quote nested
in it, a signature below the quoted thread for one, by the `id` of the
element that opened it:

```json
{
  "id": "elem-00009",
  "type": "doco:Paragraph",
  "page": 0,
  "text": "--\nKai Moreno, Senior Director\n1 Main Street, Springfield",
  "resumes": "elem-00004",
  "signature": true,
  "provenance": "eml",
  "evidence": "eml"
}
```

So the sender of any element is one walk back: to the last element with
`message`, or to the element a `resumes` names. `signature` marks the
elements of a message's signature, which is its sender's. The document's
attachments and its title, creator and date are not in this output; they
are in [`doco`](doco.md#emails-are-threads-of-messages). See
[Email](../inputs/email.md).

## Figures come in groups

A chart or diagram is rarely one element. Its labels arrive as separate text
runs, and reading them in sequence pairs the wrong label with the wrong value
— a donut chart prints `20.0% 34.5% Latin America North America`. Rather than
guess the pairing, the fragments are left in the page's own order and marked:
every element from one drawing shares a `figure` id, and each carries the
drawing's box.

So `figure` says *these belong together and their order is not a reading of
them*. A consumer that needs the pairing has the box and can look at the
drawing itself.

## Coordinates

`bbox` is in **PDF user units with a top-left origin**, directly usable as CSS
without a flip. `x0`/`y0` is the top-left corner, `x1`/`y1` the bottom-right.

At the default 2× render scale, multiply by 2 to get pixel coordinates.

## Tables are raw here

Spanning cells follow the rowspan convention: the value sits in the position
where the text was laid out, the rest are empty, and the continuation flags
mark which is which. [`md`](markdown.md) prints those same rows.

Use this format when you are reasoning about the grid yourself. Otherwise pick
the projection that has already done the work:

| you want | use |
|---|---|
| rows that stand on their own | [`doco`](doco.md) — `doc:TableCell` with headers attached |
| real `rowspan` / `colspan` | [`xhtml`](xhtml.md), but see [where a span cannot be drawn](xhtml.md#where-a-span-cannot-be-drawn) |
| the grid plus the flags | this format |

This format and `doco` are the two that cannot lose a cell: the flags here
describe every position, and `doco` resolves every position into its own node.
`xhtml` is the one that can, on merge shapes HTML has no way to tile.

The [element model](../concepts/element-model.md) page has the denormalization
snippet, and [Tables to rows and spans](../examples/tables.md) works each path
end to end.

## Working with it

```bash
# headings only
fdoc convert report.pdf -f json | jq -r '.[] | select(.type=="doco:SectionTitle") | .text'

# everything on page 12
fdoc convert report.pdf -f json | jq '[.[] | select(.page==12)]'

# elements with no geometry (should be empty for a PDF)
fdoc convert report.pdf -f json | jq '[.[] | select(has("bbox")|not)] | length'
```
