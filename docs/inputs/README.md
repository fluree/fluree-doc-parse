# Input formats

Nine sources, one [element model](../concepts/element-model.md). The format is
detected from the file extension, and a transcript or an email from its
content as well; `-` reads a PDF, a transcript or an email from stdin.

```bash
fdoc convert report.pdf
fdoc convert notes.md
fdoc convert statute.txt
fdoc convert page.html
fdoc convert report.docx
fdoc convert deck.pptx
fdoc convert book.xlsx
fdoc convert orders.csv
fdoc convert call.vtt
fdoc convert reply.eml
```

## What each reader knows

| | [PDF, XPS](pdf.md) | [DOCX](office-and-web.md) | [PPTX](office-and-web.md) | [XLSX, XLS](office-and-web.md#xlsx) | [CSV, TSV](csv.md) | [HTML](office-and-web.md) | [Markdown](office-and-web.md) | [WebVTT, SubRip](transcripts.md) | [Email](email.md) | [Plain text](office-and-web.md#plain-text) |
|---|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|
| structure is | measured | declared | declared | measured from the grid | declared, one table | declared | declared | declared, turns decided | declared, thread split | lines, headings shown |
| bounding boxes | ✅ | — | — | — | — | — | — | — | — | — |
| pages | ✅ | — | slides | sheets | — | — | — | — | — | — |
| headings | inferred | ✅ | ✅ | sheet names, titled blocks | — | ✅ | ✅ | — | HTML bodies | by keyword |
| tables | inferred | ✅ | ✅ + charts | ✅ | ✅ | ✅ | ✅ | — | HTML bodies | — |
| lists | inferred | ✅ | ✅ | — | — | ✅ | ✅ | — | ✅ | ✅ |
| forms | ✅ AcroForm | — | — | — | — | — | — | — | — | — |
| speakers and times | — | — | — | — | — | — | — | ✅ | senders and dates | — |
| can escalate | ✅ | — | — | — | — | — | — | — | — | — |
| can fail | ✅ | ✅ | ✅ | ✅ | ✅ | — | — | ✅ | ✅ | — |

The two axes that matter are **geometry** and **certainty**, and they trade
against each other. PDF is the only source with coordinates, and the only one
where structure is a guess. The others know their structure exactly and have
no idea where anything sits. See [Measured vs declared
structure](../concepts/geometry-vs-declared.md).

## Detection

By extension: `.pdf`, `.md`/`.markdown`, `.html`/`.htm`, `.docx`, `.pptx`, `.xlsx`,
`.xls`, `.csv`, `.tsv`, `.xps`/`.oxps`, `.vtt`, `.srt`, `.eml`, `.msg`, and plain text, `.txt`/`.text`. A
text file that writes Markdown's markup is read as Markdown. A transcript or an email is
also recognised by its content whatever its name, because both often arrive
renamed or untyped; see [Transcripts](transcripts.md#detection) and
[Email](email.md#detection). A PDF-only command given another format fails cleanly rather
than producing empty output.

A format with no reader yet is refused by name: `convert` fails on it with
`unsupported format (.ppt, PowerPoint 97-2003 presentation): no reader for it
yet`, rather than reading it as a PDF and failing as a corrupt one. That holds
for `.doc`, `.dot`, `.xlsb`, `.ods`, `.ppt`, `.pps`, `.pot`, `.rtf`, `.odt` and `.odp`, and for an OLE compound
file that is not an Outlook message (a Word, Excel or PowerPoint 97-2003 file)
whatever its name, on stdin too; an Excel 97-2003 workbook is recognised by
its content and read. A directory's files of these formats are
listed with the rest, so a batch counts them among its failures. A file whose
content is a PDF or an image is read as one, whatever its extension.

## Pages

- **PDF, XPS** — real pages, 0-based. An [XPS](xps.md) document is read by
  the PDF pipeline, with its geometry.
- **PPTX** — slides are pages, so `page` is the slide index.
- **XLSX, XLS** — sheets are pages, so `page` is the sheet's index in the workbook.
- **DOCX, HTML, Markdown, CSV, plain text** — no pagination; `page` is `0` throughout. A DOCX
  has page breaks only once something lays it out, and that something is not
  this.
- **Transcripts** — no pagination either. A turn is placed in time instead,
  by its `start_ms` and `end_ms`.
- **Email** — no pagination. A message is placed by its sender and date,
  in its `message` header.
