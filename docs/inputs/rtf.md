# RTF

```bash
fdoc convert memo.rtf -f doco
```

Rich Text Format is text interleaved with control words (`\par`, `\b`,
`\cell`) in nested groups, some of which are destinations holding something
other than text: the font table, the style sheet, a picture, a footnote.
It declares its structure, so it is read as a [Word file](office-and-web.md#docx)
is, with no geometry and nothing to escalate. Provenance and evidence are
`rtf`; the `\info` group's title and author go on the document node.

- **Paragraphs** end at `\par`; `\line` is a line break, `\tab` a space,
  and `\pard` resets what the paragraph is.
- **Headings**: a paragraph with an outline level (`\outlinelevelN`, set on
  it or on its style `\sN` in the style sheet), or in a style named
  `heading N`, is a heading at that level, numbered or not (Word and
  LibreOffice number a heading by putting it in a list too); one in the
  style named `Title` is the document's `doco:Title`.
- **Lists**: a paragraph in a list (`\lsN`) that is no heading is a list
  item. The number or bullet a list draws (`\listtext`, `\pntext`) is not
  written, as for an OpenDocument list.
- **Tables**: paragraphs in a table (`\intbl`) fill its cells, `\cell` ends
  a cell and `\row` a row. Each row states its cells' right edges
  (`\cellxN`); the table's columns are the edges all its rows state, so a
  cell as wide as two is one cell and the column it covers a continuation
  (`merged_left`), as is a cell merged into the one before it (`\clmrg`);
  one merged into the cell above (`\clvmrg`) is a `merged_down`
  continuation. A row that states no edges is read cell by cell from the
  left. Nested tables are read flat; no header row is declared, so
  `header_rows` is left undetected.
- **Footnotes and endnotes** (`\footnote`, `\ftnalt`) are read after the
  body, each opening with its number (`1`, `i`), marked `footnote` or
  `endnote`; the reference mark adds nothing to the body. **Comments**
  (`\annotation`) come last, opening with their author (`\atnauthor`). The
  page **header and footer** (`\header`, `\footer` and their first-page and
  left/right variants) are read once each, before and after the body: a
  line the first page's header says again is read once, a line one header
  repeats as often as it does.
- **Encodings**: `\ansicpg` names the code page of the text's bytes and of
  `\'hh` escapes (Windows-1252 by default, so `\'e9` is `é`), and a font's
  character set (`\fcharsetN` in the font table) that of text set in that
  font (`\f1`, Cyrillic under `\fcharset204`); `\uN` is a Unicode
  character, two of them a surrogate pair (an emoji), after which the
  `\ucN` fallback characters are skipped.
- **Pictures** (`\pict`) are each an empty `doco:Figure` where they stand;
  a picture Word also writes for older readers (`\nonshppict`) is not a
  second one. A field shows its result, not its instruction.
- **Skipped**: the colour, list and revision tables, the font table's and
  the style sheet's own text, document variables, shapes (their drawing
  data, and the text of a text box, `\shptxt`, which is lost), embedded
  objects, and any destination marked ignorable (`\*`) not named above.

A file is recognised by its opening, `{\rtf`, whatever its name: Word and
Outlook save RTF as `.doc` often enough. Groups nest at most 1,024 deep;
at most 100,000 notes, comments, headers and footers are read, each made
as it opens; a row definition states at most about four million cells.
What the document makes is counted against the memory one document may
take before it is made: each paragraph with its text, each row with the
copy of its definitions it keeps, and each table's cells, a short row's
padding to its table's width counted too. A file past any of these fails
as too large. `\binN` data is skipped, never past
the end of the file. In a library, `fluree_doc_rtf::parse_with_info`.
