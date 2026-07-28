# Input formats

Five sources, one [element model](../concepts/element-model.md). The format is
detected from the file extension; `-` reads a PDF from stdin.

```bash
fdoc convert report.pdf
fdoc convert notes.md
fdoc convert page.html
fdoc convert report.docx
fdoc convert deck.pptx
```

## What each reader knows

| | [PDF](pdf.md) | [DOCX](office-and-web.md) | [PPTX](office-and-web.md) | [HTML](office-and-web.md) | [Markdown](office-and-web.md) |
|---|:--:|:--:|:--:|:--:|:--:|
| structure is | measured | declared | declared | declared | declared |
| bounding boxes | ✅ | — | — | — | — |
| pages | ✅ | — | slides | — | — |
| headings | inferred | ✅ | ✅ | ✅ | ✅ |
| tables | inferred | ✅ | ✅ + charts | ✅ | ✅ |
| lists | inferred | ✅ | ✅ | ✅ | ✅ |
| forms | ✅ AcroForm | — | — | — | — |
| can escalate | ✅ | — | — | — | — |
| can fail | ✅ | ✅ | ✅ | — | — |

The two axes that matter are **geometry** and **certainty**, and they trade
against each other. PDF is the only source with coordinates, and the only one
where structure is a guess. The others know their structure exactly and have
no idea where anything sits. See [Measured vs declared
structure](../concepts/geometry-vs-declared.md).

## Detection

By extension: `.pdf`, `.md`/`.markdown`, `.html`/`.htm`, `.docx`, `.pptx`,
and plain text. A PDF-only command given another format fails cleanly rather
than producing empty output.

## Pages

- **PDF** — real pages, 0-based.
- **PPTX** — slides are pages, so `page` is the slide index.
- **DOCX, HTML, Markdown** — no pagination; `page` is `0` throughout. A DOCX
  has page breaks only once something lays it out, and that something is not
  this.
