//! CSV and TSV to DoCO-typed document elements.
//!
//! A delimited file is one table, and its first row names the columns. That
//! is all the format says: nothing is typed, nothing is merged, nothing is
//! a title. So the reader is deliberately simple, and the two things it has
//! to decide, which character separates the fields and how the bytes are
//! encoded, are decided by rules short enough to state here.
//!
//! * **Encoding.** A byte-order mark says (UTF-8, UTF-16 LE or BE); else
//!   bytes whose every other byte is mostly zero, and the others never, are
//!   UTF-16 without a mark, in the byte order the zeros give; else bytes
//!   that are valid UTF-8 are UTF-8; else they are Windows-1252, what Excel
//!   writes a "CSV" in on most Western systems.
//! * **Delimiter.** Given by the caller (a `.tsv` is tab-separated), or
//!   named by Excel's `sep=;` first line, or else sniffed: of comma,
//!   semicolon, tab and pipe, the one found outside quotes on the most of
//!   the first [`SNIFF_LINES`] records the same number of times as on the
//!   first, ties going in that order. A file in which none appears is one
//!   column.
//! * **Quoting** is RFC 4180's: a field in double quotes may hold the
//!   delimiter, line breaks and `""` for a quote. A quote inside an unquoted
//!   field is a character like any other, and a quote never closed runs to
//!   the end of the file.
//!
//! The table's `header_rows` is 1 (0 for a file of one row, which names
//! columns nothing fills). Rows shorter than the longest are padded with
//! empty cells; a record with nothing in it at all (a blank line) is no
//! row. A cell's line breaks become spaces, as an XLSX cell's do: the text
//! projection reads a row a line. No `datums`: a CSV stores text, and
//! `00123` is a code as often as a number.
//!
//! No geometry, no pages, nothing to escalate: `bbox` is `None` and `page`
//! is 0.

use fluree_doc_model::Element;

/// How many records the delimiter is sniffed from.
pub const SNIFF_LINES: usize = 20;

/// The most cells a table may hold, its rows padded to its widest: 16
/// million. A file of one very wide row over a million short ones would
/// otherwise ask for a trillion.
pub const MAX_CELLS: usize = 16 << 20;

/// How much of a file's text is looked at for a NUL, which says it is no
/// text: 64 KiB.
pub const SNIFF_BYTES: usize = 64 << 10;

/// The delimiters sniffed, in the order a tie goes.
const CANDIDATES: [char; 4] = [',', ';', '\t', '|'];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CsvError {
    /// The table, padded, would hold more than [`MAX_CELLS`] cells.
    TooLarge { rows: usize, columns: usize },
    /// A NUL among the first [`SNIFF_BYTES`] of its text: binary content,
    /// which no delimited file holds, under a delimited file's name.
    NotText,
}

impl std::fmt::Display for CsvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { rows, columns } => write!(
                f,
                "a table of {rows} rows by {columns} columns is more than the {MAX_CELLS} cells read"
            ),
            Self::NotText => write!(f, "not delimited text: binary content"),
        }
    }
}

impl std::error::Error for CsvError {}

/// Parse a delimited file, its delimiter sniffed.
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, CsvError> {
    parse_with(bytes, None)
}

/// Parse a delimited file whose fields `delimiter` separates; `None` sniffs
/// it (see the crate documentation).
pub fn parse_with(bytes: &[u8], delimiter: Option<char>) -> Result<Vec<Element>, CsvError> {
    let text = decode(bytes);
    // Decoded first: UTF-16 text is half NULs as bytes, and none as text.
    let head = text.char_indices().take_while(|(i, _)| *i < SNIFF_BYTES);
    if head.map(|(_, c)| c).any(|c| c == '\0') {
        return Err(CsvError::NotText);
    }
    let (text, named) = sep_line(&text);
    let delimiter = delimiter.or(named).unwrap_or_else(|| sniff(text));
    let rows = records(text, delimiter)?;
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .map(|mut r| {
            r.resize(width, String::new());
            r
        })
        .collect();
    let text = rows
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = Element::new("doco:Table", text, "csv", "csv");
    e.id = "elem-00001".into();
    e.header_rows = Some(usize::from(rows.len() > 1));
    e.cells = Some(rows);
    Ok(vec![e])
}

/// The text of a file: by its byte-order mark, else UTF-16 by its zero
/// bytes ([`unmarked_utf16`]), else UTF-8 where it is valid, else
/// Windows-1252.
pub fn decode(bytes: &[u8]) -> String {
    if let Some((enc, bom)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc
            .decode_without_bom_handling(&bytes[bom..])
            .0
            .into_owned();
    }
    if let Some(enc) = unmarked_utf16(bytes) {
        return enc.decode_without_bom_handling(bytes).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => encoding_rs::WINDOWS_1252
            .decode_without_bom_handling(bytes)
            .0
            .into_owned(),
    }
}

/// UTF-16 with no byte-order mark, which is how a program may write text it
/// calls Unicode: in its first 4 KiB, the high byte of at least half the
/// characters zero (a Latin script's, a digit's, a delimiter's), and the low
/// byte of almost none (no character of the first block is NUL). Binary
/// content has zeros in both.
fn unmarked_utf16(bytes: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let head = &bytes[..bytes.len().min(4096) & !1];
    let pairs = head.len() / 2;
    if pairs < 2 {
        return None;
    }
    let zeros = |at: usize| head.iter().skip(at).step_by(2).filter(|&&b| b == 0).count();
    let (even, odd) = (zeros(0), zeros(1));
    let text = |high: usize, low: usize| 2 * high >= pairs && 16 * low <= high;
    if text(odd, even) {
        Some(encoding_rs::UTF_16LE)
    } else if text(even, odd) {
        Some(encoding_rs::UTF_16BE)
    } else {
        None
    }
}

/// Excel's `sep=;` first line, which names the delimiter and is no record:
/// the text after it, and the delimiter it names.
fn sep_line(text: &str) -> (&str, Option<char>) {
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let first = first.trim_end_matches('\r');
    match first.strip_prefix("sep=").map(|d| {
        let mut c = d.chars();
        (c.next(), c.next())
    }) {
        Some((Some(d), None)) => (rest, Some(d)),
        _ => (text, None),
    }
}

/// The delimiter of `text` by the rule in the crate documentation.
pub fn sniff(text: &str) -> char {
    let mut best = (0usize, ',');
    for d in CANDIDATES {
        let counts: Vec<usize> = records_counting(text, d, SNIFF_LINES);
        let Some(&first) = counts.first() else {
            continue;
        };
        if first == 0 {
            continue;
        }
        let agree = counts.iter().filter(|&&n| n == first).count();
        if agree > best.0 {
            best = (agree, d);
        }
    }
    best.1
}

/// For each of the first `limit` records of `text`, how many times `d`
/// separates its fields.
fn records_counting(text: &str, d: char, limit: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut n = 0usize;
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == d && !quoted => n += 1,
            '\n' | '\r' if !quoted => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(n);
                n = 0;
                any = false;
                if out.len() == limit {
                    return out;
                }
            }
            _ => {}
        }
    }
    if any {
        out.push(n);
    }
    out
}

/// The records of `text` that hold something, each its fields on one line
/// ([`one_line`]), as RFC 4180 reads them; refused as soon as the table they
/// make, padded to its widest row, would pass [`MAX_CELLS`], before the
/// cells past it are allocated.
fn records(text: &str, d: char) -> Result<Vec<Vec<String>>, CsvError> {
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut width = 0usize;
    let mut row = Row::default();
    let mut field = String::new();
    // Inside a quoted field; and whether the field began with its quote,
    // which is what makes a quote in it special.
    let mut quoted = false;
    let mut at_start = true;
    let mut chars = text.chars().peekable();
    let mut pending = false;
    while let Some(c) = chars.next() {
        pending = true;
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' if at_start => {
                quoted = true;
                at_start = false;
            }
            c if c == d => {
                row.push(&mut field, out.len(), width)?;
                at_start = true;
            }
            '\n' | '\r' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row.push(&mut field, out.len(), width)?;
                row.end(&mut out, &mut width)?;
                at_start = true;
                pending = false;
            }
            c => {
                field.push(c);
                at_start = false;
            }
        }
    }
    if pending {
        row.push(&mut field, out.len(), width)?;
        row.end(&mut out, &mut width)?;
    }
    Ok(out)
}

/// The record being read: its fields up to its last that holds something,
/// and how many empty ones follow, counted rather than held, so a record of
/// nothing but delimiters, which is no row, costs nothing.
#[derive(Default)]
struct Row {
    fields: Vec<String>,
    empty: usize,
}

impl Row {
    /// Refused where a table of `rows` rows, then this one `n` fields wide,
    /// padded to `width`, would pass [`MAX_CELLS`].
    fn check(rows: usize, width: usize, n: usize) -> Result<(), CsvError> {
        let columns = width.max(n);
        if (rows + 1).saturating_mul(columns) > MAX_CELLS {
            return Err(CsvError::TooLarge {
                rows: rows + 1,
                columns,
            });
        }
        Ok(())
    }

    /// Take `field` as the record's next.
    fn push(&mut self, field: &mut String, rows: usize, width: usize) -> Result<(), CsvError> {
        if field.is_empty() {
            self.empty += 1;
            return Ok(());
        }
        let n = self.fields.len() + self.empty + 1;
        Self::check(rows, width, n)?;
        self.fields
            .extend(std::iter::repeat_n(String::new(), self.empty));
        self.empty = 0;
        self.fields.push(one_line(field));
        field.clear();
        Ok(())
    }

    /// End the record: a row of `out` if anything in it holds something.
    fn end(&mut self, out: &mut Vec<Vec<String>>, width: &mut usize) -> Result<(), CsvError> {
        let empty = std::mem::take(&mut self.empty);
        if self.fields.is_empty() {
            return Ok(());
        }
        let n = self.fields.len() + empty;
        Self::check(out.len(), *width, n)?;
        self.fields
            .extend(std::iter::repeat_n(String::new(), empty));
        *width = (*width).max(n);
        out.push(std::mem::take(&mut self.fields));
        Ok(())
    }
}

/// A cell's text on one line, its runs of whitespace one space.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(bytes: &[u8]) -> Element {
        let mut v = parse(bytes).unwrap();
        assert_eq!(v.len(), 1);
        v.remove(0)
    }

    #[test]
    fn binary_content_is_refused_but_utf16_text_is_read() {
        let mut bin = b"PK\x03\x04,\0\0,x\n1,2\n".to_vec();
        bin.extend(std::iter::repeat_n(b'a', 100));
        assert_eq!(parse(&bin).unwrap_err(), CsvError::NotText);
        // A NUL past the bytes looked at is a character like another.
        let mut late = b"a,b\n".to_vec();
        late.extend(std::iter::repeat_n(b'x', SNIFF_BYTES));
        late.extend(b"\0\n");
        assert!(parse(&late).is_ok());
        let utf16: Vec<u8> = "\u{feff}Name,Amount\nAlex,12\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(cells(&table(&utf16))[1], ["Alex", "12"]);
    }

    fn cells(e: &Element) -> Vec<Vec<&str>> {
        e.cells
            .as_ref()
            .unwrap()
            .iter()
            .map(|r| r.iter().map(String::as_str).collect())
            .collect()
    }

    #[test]
    fn a_csv_is_one_table_under_its_header_row() {
        let e = table(b"Name,City,Amount\r\nAlex Sample,Lyon,12.50\r\nSam Example,Oslo,7\r\n");
        assert_eq!(e.kind, "doco:Table");
        assert_eq!(e.header_rows, Some(1));
        assert_eq!(
            cells(&e),
            [
                ["Name", "City", "Amount"],
                ["Alex Sample", "Lyon", "12.50"],
                ["Sam Example", "Oslo", "7"],
            ]
        );
        assert_eq!(
            e.text,
            "Name | City | Amount\nAlex Sample | Lyon | 12.50\nSam Example | Oslo | 7"
        );
        assert!(e.bbox.is_none() && e.datums.is_none());
        assert_eq!((e.provenance, e.evidence), ("csv", "csv"));
    }

    #[test]
    fn quoted_fields_hold_delimiters_line_breaks_and_quotes() {
        let e = table(
            b"Item,Note\n\"Bolt, M6\",\"Says \"\"keep dry\"\"\nsecond line\"\nNut,plain \"inch\" mark\n",
        );
        assert_eq!(
            cells(&e),
            [
                ["Item", "Note"],
                ["Bolt, M6", "Says \"keep dry\" second line"],
                ["Nut", "plain \"inch\" mark"],
            ]
        );
    }

    #[test]
    fn the_delimiter_is_the_one_each_record_has_alike() {
        // Semicolons, as a European Excel writes, with commas inside amounts.
        let semi = "Name;Amount\nAlex Sample;12,50\nSam Example;7,00\n";
        assert_eq!(sniff(semi), ';');
        assert_eq!(cells(&table(semi.as_bytes()))[1], ["Alex Sample", "12,50"]);
        assert_eq!(sniff("a\tb\tc\n1\t2\t3\n"), '\t');
        assert_eq!(sniff("a|b\n1|2\n"), '|');
        // A comma inside quotes is not counted.
        assert_eq!(sniff("\"x,y\";b\n\"1,2\";3\n"), ';');
        // None at all: one column.
        let one = table(b"Name\nAlex Sample\n");
        assert_eq!(cells(&one), [["Name"], ["Alex Sample"]]);
        // Excel's `sep=` line names it and is no row.
        let named = table(b"sep=|\nA|B\n1,5|2\n");
        assert_eq!(cells(&named), [["A", "B"], ["1,5", "2"]]);
        // A caller naming it wins over the sniff.
        let tsv = parse_with(b"a,b\tc\n1,2\t3\n", Some('\t')).unwrap();
        assert_eq!(cells(&tsv[0]), [["a,b", "c"], ["1,2", "3"]]);
    }

    #[test]
    fn short_rows_are_padded_and_blank_lines_dropped() {
        let e = table(b"A,B,C\n\n1,2\n,,\n4,5,6,7\n");
        assert_eq!(
            cells(&e),
            [
                ["A", "B", "C", ""],
                ["1", "2", "", ""],
                ["4", "5", "6", "7"]
            ]
        );
    }

    #[test]
    fn encodings_by_mark_then_utf8_then_windows_1252() {
        let utf8_bom = [
            b"\xEF\xBB\xBF".as_slice(),
            "Nom,Ville\nZoë,Genève\n".as_bytes(),
        ]
        .concat();
        assert_eq!(cells(&table(&utf8_bom))[1], ["Zoë", "Genève"]);
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(
                "Nom\tVille\nZoë\tGenève\n"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        assert_eq!(cells(&table(&utf16))[1], ["Zoë", "Genève"]);
        // Without a mark, as some programs write "Unicode text", in either
        // byte order.
        let tsv = "Nom\tVille\nZoë\tGenève\n";
        let le: Vec<u8> = tsv.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let be: Vec<u8> = tsv.encode_utf16().flat_map(u16::to_be_bytes).collect();
        assert_eq!(cells(&table(&le))[1], ["Zoë", "Genève"]);
        assert_eq!(cells(&table(&be))[1], ["Zoë", "Genève"]);
        // Zeros in both bytes of the pairs are no text.
        assert!(unmarked_utf16(b"PK\x03\x04\0\0\0\0a\0b\0").is_none());
        assert!(unmarked_utf16(b"plain,text\n1,2\n").is_none());
        // Excel's Windows-1252: `é` is one byte, 0xE9, invalid as UTF-8.
        assert_eq!(
            cells(&table(b"Nom,Ville\nRen\xE9,S\xE8te\n"))[1],
            ["René", "Sète"]
        );
    }

    #[test]
    fn one_row_names_columns_and_is_no_header_over_nothing() {
        assert_eq!(table(b"A,B\n").header_rows, Some(0));
        assert!(parse(b"").unwrap().is_empty());
        assert!(parse(b"\n\n,\n").unwrap().is_empty());
    }

    #[test]
    fn an_unclosed_quote_runs_to_the_end() {
        let e = table(b"A,B\n1,\"open\n2,3\n");
        assert_eq!(cells(&e), [["A", "B"], ["1", "open 2,3"]]);
    }

    #[test]
    fn a_table_too_large_is_refused_as_it_is_read() {
        // One record of more fields than the bound, nothing in them but the
        // last: refused before they are all allocated, as soon as the one
        // holding something comes.
        let mut f = ",".repeat(MAX_CELLS);
        f.push_str("x\n");
        assert!(matches!(
            parse(f.as_bytes()),
            Err(CsvError::TooLarge { rows: 1, .. })
        ));
        // Records of nothing but delimiters are no rows, however wide.
        let mut f = "a,b\n".to_string();
        f.push_str(&",".repeat(MAX_CELLS + 1));
        f.push_str("\n1,2\n");
        let e = table(f.as_bytes());
        assert_eq!(cells(&e), [["a", "b"], ["1", "2"]]);
    }

    #[test]
    fn empty_fields_are_counted_not_held_and_a_row_past_the_bound_never_grows() {
        let mut row = Row::default();
        for _ in 0..=MAX_CELLS {
            row.push(&mut String::new(), 0, 0).unwrap();
        }
        assert_eq!(row.fields.capacity(), 0);
        assert!(row.push(&mut "x".to_string(), 0, 0).is_err());
        assert_eq!(row.fields.capacity(), 0);
    }

    #[test]
    fn a_table_too_large_to_pad_is_refused() {
        let wide = ",".repeat(5000);
        let mut f = format!("{wide}x\n");
        for _ in 0..(MAX_CELLS / 5000 + 1) {
            f.push_str("1\n");
        }
        assert!(matches!(
            parse(f.as_bytes()),
            Err(CsvError::TooLarge { columns: 5001, .. })
        ));
    }
}
