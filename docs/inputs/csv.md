# CSV and TSV

```bash
fdoc convert orders.csv -f doco
fdoc convert codes.tsv
```

A delimited file is **one table, and its first row names the columns**. That
is all the format says: nothing is typed, merged or titled. So the table's
`header_rows` is `1` (`0` for a file of a single row, whose names head no
values), `page` is `0`, and there is no `bbox` and nothing to escalate.

What the reader has to decide, it decides by short rules:

- **Encoding.** A byte-order mark says which (UTF-8, UTF-16 LE or BE);
  otherwise bytes that are valid UTF-8 are read as UTF-8, and anything else
  as Windows-1252, which is what Excel saves a CSV in on most Western
  systems.
- **Delimiter.** A `.tsv` is split on tabs. A `.csv` names its delimiter in
  Excel's `sep=;` first line, which is no row, or else it is sniffed: of
  comma, semicolon, tab and pipe, the one found outside quotes the same
  number of times on most of the first 20 records as on the first, ties
  going in that order. A European Excel's `Name;Amount` over `Alex
  Sample;12,50` is semicolon-separated: the header holds no comma. A file
  in which none appears is one column.
- **Quoting** is RFC 4180's: a field in double quotes may hold the
  delimiter, line breaks, and `""` for a quote. A quote inside an unquoted
  field is an ordinary character, and a quote never closed runs to the end
  of the file rather than failing it.

Rows shorter than the longest are padded with empty cells, and a record
with nothing in it (a blank line, or only delimiters) is no row. A cell's
line breaks become spaces, as a workbook cell's do: the
[text projection](../concepts/text-projection.md) reads a table a row per
line.

Cells carry no `datums`. A CSV stores text: `00123` is a code as often as a
number, and `03/04/2026` a date only in one country's order, so a type would
be a guess about the file rather than a fact in it. A workbook declares its
types, and its tables carry them (see [XLSX](office-and-web.md#xlsx)).

The table's cells, its rows padded to the widest, and their text are
counted against the memory one document may take as they are read, before
they are allocated, and a table past it is refused (`CsvError::TooLarge`):
one very wide row over a million short ones would otherwise ask for a
trillion cells. A cell that holds text is counted at what the output makes
of it: a node of its own in DoCO (4 KiB), a cell of its row in Markdown,
XHTML, JSON and the text (128 bytes and its text). So 70,000 records of ten
fields (4 MB) convert to Markdown and are refused as DoCO. The
empty fields of a record are counted, not held, until one that holds
something follows them, so a line of nothing but delimiters, which is no
row, costs nothing however long it is.

In a library, `fluree_doc_csv::parse(&bytes)` sniffs the delimiter, and
`parse_with(&bytes, Some('\t'))` names it.
