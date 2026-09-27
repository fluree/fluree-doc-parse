# Input formats

Eight sources, one [element model](../concepts/element-model.md). The format is
detected from the file extension, and a transcript or an email from its
content as well; `-` reads a PDF, a transcript or an email from stdin.

```bash
fdoc convert report.pdf
fdoc convert notes.md
fdoc convert page.html
fdoc convert report.docx
fdoc convert deck.pptx
fdoc convert book.xlsx
fdoc convert call.vtt
fdoc convert reply.eml
```

## What each reader knows

| | [PDF](pdf.md) | [DOCX](office-and-web.md) | [PPTX](office-and-web.md) | [XLSX](office-and-web.md) | [HTML](office-and-web.md) | [Markdown](office-and-web.md) | [WebVTT, SubRip](transcripts.md) | [Email](email.md) |
|---|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|
| structure is | measured | declared | declared | measured from the grid | declared | declared | declared, turns decided | declared, thread split |
| bounding boxes | ✅ | — | — | — | — | — | — | — |
| pages | ✅ | — | slides | sheets | — | — | — | — |
| headings | inferred | ✅ | ✅ | sheet names, titled blocks | ✅ | ✅ | — | HTML bodies |
| tables | inferred | ✅ | ✅ + charts | ✅ | ✅ | ✅ | — | HTML bodies |
| lists | inferred | ✅ | ✅ | — | ✅ | ✅ | — | ✅ |
| forms | ✅ AcroForm | — | — | — | — | — | — | — |
| speakers and times | — | — | — | — | — | — | ✅ | senders and dates |
| can escalate | ✅ | — | — | — | — | — | — | — |
| can fail | ✅ | ✅ | ✅ | ✅ | — | — | ✅ | ✅ |

The two axes that matter are **geometry** and **certainty**, and they trade
against each other. PDF is the only source with coordinates, and the only one
where structure is a guess. The others know their structure exactly and have
no idea where anything sits. See [Measured vs declared
structure](../concepts/geometry-vs-declared.md).

## Detection

By extension: `.pdf`, `.md`/`.markdown`, `.html`/`.htm`, `.docx`, `.pptx`, `.xlsx`,
`.vtt`, `.srt`, `.eml`, `.msg`, and plain text. A transcript or an email is
also recognised by its content whatever its name, because both often arrive
renamed or untyped; see [Transcripts](transcripts.md#detection) and
[Email](email.md#detection). A PDF-only command given another format fails cleanly rather
than producing empty output.

## Pages

- **PDF** — real pages, 0-based.
- **PPTX** — slides are pages, so `page` is the slide index.
- **XLSX** — sheets are pages, so `page` is the sheet's index in the workbook.
- **DOCX, HTML, Markdown** — no pagination; `page` is `0` throughout. A DOCX
  has page breaks only once something lays it out, and that something is not
  this.
- **Transcripts** — no pagination either. A turn is placed in time instead,
  by its `start_ms` and `end_ms`.
- **Email** — no pagination. A message is placed by its sender and date,
  in its `message` header.
