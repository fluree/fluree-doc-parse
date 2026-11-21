# OpenDocument: .odt, .ods, .odp

```bash
fdoc convert memo.odt -f doco
fdoc convert book.ods
fdoc convert deck.odp
```

LibreOffice's formats, and the ISO standard's: a zip whose `mimetype` entry
says which kind it is (text, spreadsheet, presentation, or their templates
`.ott`, `.ots`, `.otp`), whose `content.xml` holds the body, `styles.xml` the
page header and footer and `meta.xml` the title, author and dates
(`dcterms:*` on the document node). The kind is read from the `mimetype`,
whatever the file's name, on stdin too. Like the Office formats, these
declare their structure: no geometry, nothing to escalate. Provenance and
evidence are `odt`, `ods` or `odp`.

## Text (`.odt`)

Read as a [Word file](office-and-web.md#docx) is:

- a heading's outline level is its level (`text:h`), and so is a paragraph's
  whose style, or a style it is based on, states an outline level or is a
  built-in heading (`Heading 2`): a heading LibreOffice imported from HTML is
  often such a paragraph. The built-in `Title` style is the `doco:Title`;
- list items are list items, nested lists included; their numbers, which a
  list style computes, are not written;
- tables are tables, their declared header rows (`table-header-rows`) their
  `header_rows`, a cell spanning columns or rows making the cells it covers
  continuations (`merged_left`, `merged_down`). What DoCO copies of a merge
  (its value in each row it covers, the texts of the cells it covers joined
  first) and the labels each cell carries count against the tables' bounds
  as cells holding that text, so a span over many rows of text fails as too
  large rather than being written once a row;
- footnotes and endnotes are read after the body, each opening with its
  citation (`1`, `i`), marked `evidence` `footnote` or `endnote`; comments
  last, each opening with its author, `evidence` `comment`; the page header
  and footer of the master pages once each, before and after, as `header`
  and `footer`;
- a text box is read where it is anchored: one anchored to a paragraph or
  to a character in it right after that paragraph, one anchored to the
  page where it stands in the body;
- `text:s` is a space, a line break a line, a tab a space; the paragraphs
  and list items of a table cell are its text, one after the other.

Not read: pictures and drawings, a text box inside a table cell, tracked
changes as such (the text as it stands is read), indexes' own generated
entries beyond their text.

## Spreadsheets (`.ods`)

Read as a [workbook](office-and-web.md#xlsx) is, by the same layout: each
sheet a page opened by its name, its blocks of cells tables, a bold first
row or the sheet's header rows a header, spans as merges. A cell's text is
what it shows (its `text:p`, `12.5%`), and what it stores rides beside it
typed (`xsd:decimal` 0.125, `xsd:date`, `xsd:time`, `xsd:boolean`).

A sheet writes its trailing empty rows and columns as one row repeated to
the end of the grid (`number-rows-repeated="1048573"`). Repeats are
counted, not copied, while they hold nothing; a repeated cell that holds
something is a cell each time it repeats, its text copied each time, and a
workbook of more than about four million such cells, or whose cells and
copies of text pass the document's budget
([limits on what one document may make](../reference/limits.md)), fails as
too large. A cell spanning rows and columns is
a merge, laid out as far as the sheet's cells reach. So a few hundred bytes
cannot ask for the sixteen billion cells a repeated row of repeated cells
describes, nor for the copies of one long cell repeated across them. Rows
past 1,048,576 and columns past 16,384 are not read.
Frozen panes (in `settings.xml`) are not read.

## Presentations (`.odp`)

Read as a [deck](office-and-web.md#pptx) is: each slide a page, its title
frame (`presentation:class="title"`) a level-1 heading, an outline frame's
paragraphs list items, other frames' and shapes' text paragraphs, tables
tables, in the slide's order. Speaker notes, pictures and charts are not
read.

## Bounds

Each part at most 64 MiB inflated, checked as it inflates, and its XML at
most four million nodes nested 256 deep. The tables of a text or a
presentation hold at most about four million cells in all, a repeated cell
counted each time and a short row's padding to the table's width counted
too, and at most 64 MiB of cell text, a repeated cell's text counted each
time it is copied: a cell of a few kilobytes repeated a thousand times asks
for gigabytes. Both are counted before the cells are made, and a document
past either fails as too large. In a library,
`fluree_doc_odf::parse_with_info` returns the elements, the declared info
and the kind; `Format::sniff` tells the kind from the bytes.
