# Input formats

Many sources, one [element model](../concepts/element-model.md). The format is
detected from the file extension, and a transcript, an email, an Office
97-2003 file, an OpenDocument file, an Excel binary workbook, RTF, an
Office package, an XPS document or a web page from its content as well;
`-` reads any of those, or a PDF, from stdin.

```bash
fdoc convert report.pdf
fdoc convert notes.md
fdoc convert statute.txt
fdoc convert page.html
fdoc convert report.docx
fdoc convert deck.pptx
fdoc convert book.xlsx
fdoc convert orders.csv
fdoc convert memo.odt
fdoc convert old-deck.ppt
fdoc convert printed.xps
fdoc convert call.vtt
fdoc convert reply.eml
```

## What each reader knows

|  | [PDF, XPS](pdf.md) | [DOCX](office-and-web.md#docx) | [DOC](office-and-web.md#word-97-2003-doc) | [PPTX](office-and-web.md#pptx) | [PPT](office-and-web.md#powerpoint-97-2003-ppt) | [XLSX, XLS](office-and-web.md#xlsx) | [ODT, ODS, ODP](opendocument.md) | [CSV, TSV](csv.md) | [RTF](rtf.md) | [HTML](office-and-web.md) | [Markdown](office-and-web.md) | [WebVTT, SubRip](transcripts.md) | [Email](email.md) | [Plain text](office-and-web.md#plain-text) |
| --- | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | — |
| structure is | measured | declared | declared | declared | declared | measured from the grid | declared; sheets measured from the grid | declared, one table | declared | declared | declared | declared, turns decided | declared, thread split | lines, headings shown |
| bounding boxes | ✅ | — | — | — | — | — | — | — | — | — | — | — | — | — |
| pages | ✅ | — | — | slides | slides | sheets | slides, sheets | — | — | — | — | — | — | — |
| headings | inferred | ✅ | — | ✅ | slide titles | sheet names, titled blocks | ✅ | — | ✅ | ✅ | ✅ | — | HTML bodies | by keyword |
| tables | inferred | ✅ | ✅ | ✅ + charts | ✅ rebuilt | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | — | HTML bodies | — |
| lists | inferred | ✅ | — | ✅ | ✅ | — | ✅ | — | ✅ | ✅ | ✅ | — | ✅ | ✅ |
| forms | ✅ AcroForm | — | — | — | — | — | — | — | — | — | — | — | — | — |
| speakers and times | — | — | — | — | — | — | — | — | — | — | — | ✅ | senders and dates | — |
| can escalate | ✅ | — | — | — | — | — | — | — | — | — | — | — | — | — |
| can fail | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | — | — | ✅ | ✅ | — |

The two axes that matter are **geometry** and **certainty**, and they trade
against each other. PDF is the only source with coordinates, and the only one
where structure is a guess. The others know their structure exactly and have
no idea where anything sits. See [Measured vs declared
structure](../concepts/geometry-vs-declared.md).

## Detection

By extension: `.pdf`, `.md`/`.markdown`, `.html`/`.htm`, `.docx`, `.doc`, `.pptx`, `.ppt`, `.xlsx`,
`.xls`, `.xlsb`, `.odt`/`.ods`/`.odp`, `.rtf`, `.csv`, `.tsv`, `.xps`/`.oxps`, `.vtt`, `.srt`, `.eml`, `.msg`, and plain text, `.txt`/`.text`. A
text file that writes Markdown's markup is read as Markdown. A transcript or an email is
also recognised by its content whatever its name, because both often arrive
renamed or untyped; see [Transcripts](transcripts.md#detection) and
[Email](email.md#detection). A PDF-only command given another format fails cleanly rather
than producing empty output.

A format with no reader yet is refused by name: `convert` fails on it with
`unsupported format (.key, Apple Keynote presentation): no reader for it
yet`, rather than reading it as a PDF and failing as a corrupt one. That holds
for WordPerfect's `.wpd` and Apple's
`.pages`, `.numbers` and `.key`, and for an OLE compound file that is no
document a reader here reads nor an Outlook message, whatever its name, on
stdin too. Word, Excel and PowerPoint 97-2003 files, and [OpenDocument](opendocument.md)
files by their `mimetype` and [RTF](rtf.md) by its opening, are recognised by their content and read. A
directory's files of these formats are listed with the rest, so a batch
counts them among its failures.

Content wins over the name. A file whose first bytes say what it is is read
as that, whatever its extension: a PDF (`%PDF-`), an image, an OLE compound
file by the stream that makes it an Outlook message or a Word, Excel or
PowerPoint 97-2003 file, a zip by the part that makes it an XPS document,
an OpenDocument file, an Excel binary workbook or an Office package, and
RTF (`{\rtf`). So a PDF named `.doc` or `.csv` is read as a PDF, a
screenshot named `.doc` as an image. The extension only decides between
formats the content cannot tell apart, and the CSV reader refuses binary
content (a NUL in its first 64 KiB of text) rather than read it as a table.

A file named as a Word, Excel or PowerPoint 97-2003 one that is no compound
file is read as what it holds: a web page or comma-, semicolon- or
tab-separated text (an export named `.xls`, as many systems write one), or
an Office package or XPS document renamed. An Office package, an XPS
document or a web page is recognised on stdin too. Any other file named as a
format a reader takes goes to that reader, which says what is wrong with it:
never to the PDF reader, whose `parse: Invalid` would read as a corrupt
PDF.

## Pages

- **PDF, XPS** — real pages, 0-based. An [XPS](xps.md) document is read by
  the PDF pipeline, with its geometry.
- **PPTX, PPT, ODP** — slides are pages, so `page` is the slide index.
- **XLSX, XLS, ODS** — sheets are pages, so `page` is the sheet's index in the workbook.
- **DOCX, DOC, ODT, HTML, Markdown, CSV**, plain text — no pagination; `page` is `0` throughout. A DOCX
  has page breaks only once something lays it out, and that something is not
  this.
- **Transcripts** — no pagination either. A turn is placed in time instead,
  by its `start_ms` and `end_ms`.
- **Email** — no pagination. A message is placed by its sender and date,
  in its `message` header.
