# Input formats

Ten sources, one [element model](../concepts/element-model.md). The format is
detected from the file extension, and a transcript, an email or a media
asset from its content as well. An XML or JSON record is read when a
[source format](records.md) is declared for it. `-` reads a PDF, a
transcript, an email, a media asset or a declared record from stdin.

```bash
fdoc convert report.pdf
fdoc convert notes.md
fdoc convert page.html
fdoc convert report.docx
fdoc convert deck.pptx
fdoc convert book.xlsx
fdoc convert call.vtt
fdoc convert reply.eml
fdoc convert episode.axf
fdoc convert article.xml --source-format news-article.json
```

## What each reader knows

| | [PDF](pdf.md) | [DOCX](office-and-web.md) | [PPTX](office-and-web.md) | [XLSX](office-and-web.md) | [HTML](office-and-web.md) | [Markdown](office-and-web.md) | [WebVTT, SubRip](transcripts.md) | [Email](email.md) | [AXF](media-assets.md) | [XML, JSON records](records.md) |
|---|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|
| structure is | measured | declared | declared | measured from the grid | declared | declared | declared, turns decided | declared, thread split | declared by a source format, turns decided | declared by a source format |
| bounding boxes | ✅ | — | — | — | — | — | — | — | — | — |
| pages | ✅ | — | slides | sheets | — | — | — | — | — | — |
| headings | inferred | ✅ | ✅ | sheet names, titled blocks | ✅ | ✅ | — | HTML bodies | the title, section tracks | fields declared as title or heading |
| tables | inferred | ✅ | ✅ + charts | ✅ | ✅ | ✅ | — | HTML bodies | — | — |
| lists | inferred | ✅ | ✅ | — | ✅ | ✅ | — | ✅ | — | — |
| forms | ✅ AcroForm | — | — | — | — | — | — | — | — | — |
| speakers and times | — | — | — | — | — | — | ✅ | senders and dates | times, changes of voice | — |
| source field | — | — | — | — | — | — | — | — | ✅ | ✅ |
| can escalate | ✅ | — | — | — | — | — | — | — | — | — |
| can fail | ✅ | ✅ | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ |

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

A media asset is recognised by its content alone, its root element: `.axf`
is only what a directory is scanned for. See
[Media assets](media-assets.md#detection).

XML and JSON records are read only when `--source-format` is given, and
then by their content: a file that opens with `<` or `{` and that one of
the declarations recognises is a record, whatever its name. A file named
`.xml` or `.json` that none recognises is an error; one with another name
is left to the other readers. With the option given, a directory is
scanned for `.xml` and `.json` as well. See
[Records](records.md#detection).

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
- **Media assets** — no pagination. A turn and a section are placed in
  time, by `start_ms` and `end_ms`, and every element by the track or field
  it was read from, in its `sourcePath`.
- **Records** — no pagination. An element is placed by the field it was
  read from, in its `sourcePath`.
