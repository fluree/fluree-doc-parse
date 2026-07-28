# Output formats

Five projections of the same [element list](../concepts/element-model.md).
Pick by what you need to survive the trip.

```bash
fdoc convert report.pdf -f md      # default
fdoc convert report.pdf -f xhtml
fdoc convert report.pdf -f json
fdoc convert report.pdf -f doco
fdoc convert report.pdf -f text
```

## Capability matrix

| | [md](markdown.md) | [xhtml](xhtml.md) | [json](json.md) | [doco](doco.md) | [text](text.md) |
|---|:--:|:--:|:--:|:--:|:--:|
| human-readable | ✅ | — | — | — | ✅ |
| DoCO element types | — | partial¹ | ✅ | ✅ | — |
| page numbers | — | — | ✅ | ✅ | — |
| bounding boxes² | — | — | ✅ | ✅ | — |
| char offsets | — | — | — | ✅ | is the baseline |
| section containment | implicit | implicit | — | ✅ explicit | — |
| table cells addressable | — | ✅ | ✅ | ✅ | — |
| header/merge metadata | applied | applied | ✅ raw | applied | — |
| [evidence](../concepts/provenance.md) | — | — | ✅ | ✅ | — |
| provenance (engine) | — | — | ✅ | — | — |
| ledger-insertable | — | — | — | ✅ | — |

¹ as HTML tags (`h1`–`h6`, `p`, `ul`, `table`), which is a lossy encoding of
the same typing — see [XHTML](xhtml.md).
² PDF sources only. See [Measured vs declared
structure](../concepts/geometry-vs-declared.md).

## Choosing

- **Reading it yourself, or feeding an LLM** → [`md`](markdown.md)
- **Replacing an HTML-consuming extraction worker** → [`xhtml`](xhtml.md)
- **You want elements, boxes and the raw table grid** → [`json`](json.md)
- **You want a graph: containment, offsets, ledger ingest** →
  [`doco`](doco.md)
- **You are running NER and need the string the offsets index into** →
  [`text`](text.md), paired with `doco`

## Stability

`md`, `xhtml`, `json`, `doco` and `text` are compatibility surfaces: their
shape follows semver.

[`fdoc dev`](../cli/dev.md) is not. It exposes pipeline internals — glyphs,
lines, blocks, table geometry — and its output may change in any release.
