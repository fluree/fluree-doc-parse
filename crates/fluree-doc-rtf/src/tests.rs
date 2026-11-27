//! RTF written by hand, as Word and LibreOffice write it, and what the
//! reader makes of it.

use super::*;

fn kinds(els: &[Element]) -> Vec<(&str, Option<usize>, &str, &str)> {
    els.iter()
        .map(|e| (e.kind.as_str(), e.level, e.text.as_str(), e.evidence))
        .collect()
}

const DOC: &str = r#"{\rtf1\ansi\ansicpg1252\deff0
{\fonttbl{\f0\froman Times New Roman;}{\f1 Arial;}}
{\colortbl;\red0\green0\blue0;}
{\stylesheet{\s0 Normal;}{\s1\outlinelevel0 heading 1;}{\s2 heading 2;}{\s15 Title;}{\*\cs10 Default Paragraph Font;}}
{\info{\title Quarterly report}{\author Alex Sample}}
{\header\pard\plain Ref: QA-7\par}
{\footer\pard\plain Uncontrolled when printed\par}
\pard\plain\s15 Quarterly report\par
\pard\plain\s1 Orders\par
\pard\plain Alex Sample signed{\super\chftn}{\footnote\pard\plain{\super\chftn} Survey of 2026.} the orders for Ren\'e9\line {\b Second} line\tab end.\par
{\*\atnid AS}{\*\atnauthor Sam Example}\chatn{\*\annotation{\*\atnref 0}\pard\plain Source?}
\pard\plain\s2 Items\par
{\listtext\pard\plain 1.\tab}\pard\plain\ls1\ilvl0 Bolts\par
{\listtext\pard\plain 2.\tab}\pard\plain\ls1\ilvl0 Nuts\par
\trowd\cellx3000\cellx6000\pard\plain\intbl Customer\cell Amount\cell\row
\trowd\cellx3000\cellx6000\pard\plain\intbl Alex\par Sample\cell 12.50\cell\row
\pard\plain {\field{\*\fldinst HYPERLINK "https://example.org"}{\fldrslt Example}} link{\footnote\ftnalt\pard\plain{\super\chftn} Method.}\par
{\*\shppict{\pict\pngblip\picw10\pich10 89504e47}}{\nonshppict{\pict\wmetafile8 0100}}
\pard\plain \u8364?\uc0\u8212 done\par
}"#;

#[test]
fn a_document_reads_as_a_word_file_does() {
    let (els, info) = parse_with_info(DOC.as_bytes()).unwrap();
    assert_eq!(
        kinds(&els),
        [
            ("doco:Paragraph", None, "Ref: QA-7", "header"),
            ("doco:Title", None, "Quarterly report", "rtf"),
            ("doco:SectionTitle", Some(1), "Orders", "rtf"),
            (
                "doco:Paragraph",
                None,
                "Alex Sample signed the orders for René\nSecond line end.",
                "rtf"
            ),
            ("doco:SectionTitle", Some(2), "Items", "rtf"),
            // The number the list draws, as a Word file's is.
            ("doc:ListItem", None, "1. Bolts", "rtf"),
            ("doc:ListItem", None, "2. Nuts", "rtf"),
            (
                "doco:Table",
                None,
                "Customer | Amount\nAlex Sample | 12.50",
                "rtf"
            ),
            ("doco:Paragraph", None, "Example link", "rtf"),
            ("doco:Figure", None, "", "rtf"),
            // The space after a control word only ends it.
            ("doco:Paragraph", None, "€—done", "rtf"),
            ("doco:Paragraph", None, "1 Survey of 2026.", "footnote"),
            ("doco:Paragraph", None, "i Method.", "endnote"),
            (
                "doco:Paragraph",
                None,
                "Uncontrolled when printed",
                "footer"
            ),
            ("doco:Paragraph", None, "Sam Example: Source?", "comment"),
        ]
    );
    assert_eq!(info.title.as_deref(), Some("Quarterly report"));
    assert_eq!(info.creators, ["Alex Sample"]);
    assert!(els
        .iter()
        .all(|e| e.provenance == "rtf" && e.bbox.is_none()));
}

#[test]
fn code_pages_and_unicode_escapes_decode() {
    // Cyrillic in its code page, and a character a Unicode escape gives
    // with a two-byte fallback skipped.
    let doc =
        b"{\\rtf1\\ansi\\ansicpg1251 \\'cf\\'f0\\'e8\\'e2\\'e5\\'f2 \\uc2\\u20320\\'c4\\'e3!\\par}";
    let els = parse(doc).unwrap();
    assert_eq!(els[0].text, "Привет 你!");
}

#[test]
fn nesting_past_the_bound_fails_and_binary_data_is_skipped() {
    let deep = format!(
        "{{\\rtf1 {}x{}}}",
        "{".repeat(MAX_DEPTH + 1),
        "}".repeat(MAX_DEPTH + 1)
    );
    assert_eq!(parse(deep.as_bytes()).err(), Some(RtfError::TooDeep));
    // `\bin` data is skipped, whatever it holds, and never past the end.
    let mut doc = b"{\\rtf1 Before {\\*\\blob\\bin4 {}\\}x} after\\par".to_vec();
    doc.extend(b"{\\bin999999 ");
    let els = parse(&doc).unwrap();
    assert_eq!(els[0].text, "Before after");
    assert!(!is_rtf(b"PK\x03\x04"));
    assert_eq!(parse(b"plain text").err(), Some(RtfError::NotRtf));
    assert!(is_rtf(b"\r\n {\\rtf1}"));
}

#[test]
fn a_numbered_heading_is_a_heading() {
    // As Word and LibreOffice write a heading numbered by a list.
    let doc = br"{\rtf1{\stylesheet{\s1\outlinelevel0 heading 1;}}
\pard\s1\outlinelevel0\ls1\ilvl0 {\listtext 1.\tab}Introduction\par
\pard\ls1\ilvl0 {\listtext 2.\tab}An item\par}";
    assert_eq!(
        kinds(&parse(doc).unwrap()),
        [
            ("doco:SectionTitle", Some(1), "1. Introduction", "rtf"),
            ("doc:ListItem", None, "2. An item", "rtf"),
        ]
    );
}

#[test]
fn a_cell_as_wide_as_two_spans_them() {
    // The second row's first cell ends where the first row's second does;
    // the third row's first cell continues the one above it.
    let doc = br"{\rtf1
\trowd\cellx3000\cellx6000\cellx9000\pard\intbl Region\cell Q1\cell Q2\cell\row
\trowd\cellx6000\cellx9000\pard\intbl North and East\cell 7\cell\row
\trowd\clvmgf\cellx3000\cellx6000\cellx9000\pard\intbl South\cell 1\cell 2\cell\row
\trowd\clvmrg\cellx3000\cellx6000\cellx9000\pard\intbl\cell 3\cell 4\cell\row
\pard After\par}";
    let els = parse(doc).unwrap();
    let t = &els[0];
    assert_eq!(
        *t.cells.as_ref().unwrap(),
        [
            ["Region", "Q1", "Q2"],
            ["North and East", "", "7"],
            ["South", "1", "2"],
            ["", "3", "4"],
        ]
    );
    let left = t.merged_left.as_ref().unwrap();
    assert_eq!(left.iter().positions(), [4]);
    assert_eq!(t.merged_down.as_ref().unwrap().iter().positions(), [9]);
    // A table whose rows state no edges is read as before.
    let plain = parse(br"{\rtf1\pard\intbl a\cell b\cell\row\pard\intbl c\cell\row}").unwrap();
    assert_eq!(*plain[0].cells.as_ref().unwrap(), [["a", "b"], ["c", ""]]);
    assert!(plain[0].merged_left.is_none());
}

#[test]
fn surrogate_pairs_and_font_character_sets_decode() {
    let doc =
        br"{\rtf1\ansi\ansicpg1252\uc1{\fonttbl{\f0\fcharset0 Arial;}{\f1\fcharset204 Arial;}}
Emoji \u-10179?\u-8704? and caf\'e9 {\f1 \'cf\'f0\'e8\'e2\'e5\'f2}\par}";
    assert_eq!(
        parse(doc).unwrap()[0].text,
        "Emoji \u{1F600} and café Привет"
    );
}

#[test]
fn a_header_said_on_each_kind_of_page_is_read_once_and_its_own_lines_kept() {
    let doc = br"{\rtf1{\header\pard Ref: QA-7\par\pard Rev 2\par\pard Rev 2\par}
{\headerf\pard Ref: QA-7\par\pard Draft\par}\pard Body\par}";
    let els = parse(doc).unwrap();
    let texts: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(texts, ["Ref: QA-7", "Rev 2", "Rev 2", "Draft", "Body"]);
}

#[test]
fn notes_and_table_padding_past_their_bounds_are_refused() {
    let notes = format!("{{\\rtf1 {}}}", "{\\footnote x}".repeat(MAX_FLOWS + 1));
    assert!(matches!(
        parse(notes.as_bytes()),
        Err(RtfError::TooLarge(_))
    ));
    let notes = format!("{{\\rtf1 {}}}", "{\\footnote x}".repeat(MAX_FLOWS - 1));
    assert!(parse(notes.as_bytes()).is_ok());
    // One row of 5,000 cells, then enough rows of one that the grid they
    // pad to holds more cells than a document's memory allows: a few
    // kilobytes asking for tens of millions of cells.
    use fluree_doc_model::budget::{CELL_BYTES, MAX_MEMORY};
    let rows = (MAX_MEMORY / CELL_BYTES / 5000 + 1) as usize;
    let doc = format!(
        "{{\\rtf1\\intbl {}\\row{}}}",
        "\\cell".repeat(5000),
        "\\cell\\row".repeat(rows)
    );
    assert!(matches!(parse(doc.as_bytes()), Err(RtfError::TooLarge(_))));
}

#[test]
fn a_rows_definitions_are_spent_at_each_row_before_they_are_copied() {
    use fluree_doc_model::budget::CELL_BYTES;
    // A row defined with a thousand cells, then a hundred rows of one: a
    // grid of a hundred cells, but each row keeps a copy of the thousand
    // definitions.
    let doc = format!(
        "{{\\rtf1\\pard\\intbl\\trowd{} {}\\pard\\par}}",
        "\\cellx1".repeat(1000),
        "x\\cell\\row ".repeat(100)
    );
    let budget = |cells: u64| Budget::new(cells * CELL_BYTES + (1 << 20), 0);
    assert!(parse_within(doc.as_bytes(), budget(100 * 1001 + 100)).is_ok());
    assert!(matches!(
        parse_within(doc.as_bytes(), budget(50 * 1001)),
        Err(RtfError::TooLarge(_))
    ));
    // Paragraphs spend it too, each with its text.
    let doc = format!("{{\\rtf1 {}}}", "word\\par ".repeat(1000));
    let need =
        1000 * (fluree_doc_model::budget::ELEMENT_BYTES + 4 * fluree_doc_model::budget::TEXT_BYTES);
    assert_eq!(
        parse_within(doc.as_bytes(), Budget::new(need, 0))
            .unwrap()
            .0
            .len(),
        1000
    );
    assert!(parse_within(doc.as_bytes(), Budget::new(need - 1, 0)).is_err());
}

#[test]
fn a_tables_cell_costs_what_a_sheets_does() {
    use fluree_doc_model::budget::{
        Output, CELL_BYTES, ELEMENT_BYTES, FLAT_CELL_BYTES, TEXT_BYTES,
    };
    // Two rows of two cells, three that hold text.
    let doc = "{\\rtf1\\trowd\\cellx1\\cellx2\\intbl ab\\cell c\\cell\\row\\trowd\\cellx1\\cellx2\\intbl d\\cell\\cell\\row\\pard\\par}";
    let read = |output, memory: u64| {
        let (e, _) = parse_within(doc.as_bytes(), Budget::new(memory, 0).for_output(output))?;
        Ok::<_, RtfError>(e[0].cells.clone())
    };
    // Each row keeps its two cells and a copy of their definitions, then
    // the grid of four: the table, and each cell that holds text written
    // once in its row, or as a node of its own in DoCO, a node at a time,
    // at the same cost. DoCO spends the labels it copies into each cell
    // too.
    let slots = 2 * 4 + 4;
    let flat = slots * CELL_BYTES + ELEMENT_BYTES + 3 * FLAT_CELL_BYTES + 4 * TEXT_BYTES;
    assert_eq!(read(Output::Flat, flat).unwrap().unwrap()[0], ["ab", "c"]);
    assert!(read(Output::Flat, flat - 1).is_err());
    assert!(read(Output::Doco, flat + (1 << 20)).is_ok());
    assert!(read(Output::Doco, flat).is_err());
}

trait Positions {
    fn positions(self) -> Vec<usize>;
}

impl<'a, I: Iterator<Item = &'a bool>> Positions for I {
    fn positions(self) -> Vec<usize> {
        self.enumerate()
            .filter(|(_, b)| **b)
            .map(|(i, _)| i)
            .collect()
    }
}

#[test]
fn a_long_value_merged_down_is_refused_rather_than_copied_into_each_row() {
    let doc = |rows: usize| {
        format!(
            "{{\\rtf1\\trowd\\clvmgf\\cellx3000\\cellx6000\\pard\\intbl {}\\cell a\\cell\\row {}\\pard After\\par}}",
            "y".repeat(200_000),
            "\\trowd\\clvmrg\\cellx3000\\cellx6000\\pard\\intbl\\cell b\\cell\\row ".repeat(rows - 1)
        )
    };
    assert!(parse(doc(3).as_bytes()).is_ok());
    let err = parse(doc(3_000).as_bytes()).unwrap_err();
    assert!(matches!(err, RtfError::TooLarge(_)), "{err}");
}

#[test]
fn pictures_spend_the_budget_before_they_are_made() {
    use fluree_doc_model::budget::ELEMENT_BYTES;
    // A thousand pictures of seven bytes each: a figure each.
    let doc = format!("{{\\rtf1 {}}}", "{\\pict}".repeat(1_000));
    let (els, _) = parse_within(doc.as_bytes(), Budget::new(1_000 * ELEMENT_BYTES, 0)).unwrap();
    assert_eq!(els.len(), 1_000);
    assert!(els.iter().all(|e| e.kind == "doco:Figure"));
    assert!(parse_within(doc.as_bytes(), Budget::new(999 * ELEMENT_BYTES, 0)).is_err());
}

#[test]
fn a_lists_numbers_and_levels_are_read_and_its_bullets_add_nothing() {
    let doc = br"{\rtf1
\pard\ls1\ilvl0 {\listtext a)\tab}First\par
\pard\ls1\ilvl1 {\listtext\f2 \'b7\tab}Nested\par
\pard\ls1\ilvl0 {\listtext b)\tab}Second\par
\pard After\par}";
    let els = parse(doc).unwrap();
    let got: Vec<(&str, &str, usize)> = els
        .iter()
        .map(|e| (e.kind.as_str(), e.text.as_str(), e.depth))
        .collect();
    assert_eq!(
        got,
        [
            ("doc:ListItem", "a) First", 0),
            ("doc:ListItem", "Nested", 1),
            ("doc:ListItem", "b) Second", 0),
            ("doco:Paragraph", "After", 0),
        ]
    );
}

#[test]
fn header_rows_text_boxes_and_a_note_left_open_are_read() {
    // A table whose first row repeats as its header (`\trhdr`); a text box
    // anchored in a paragraph, read after it; a footnote the file ends
    // inside of.
    let doc = br"{\rtf1
\trowd\trhdr\cellx3000\cellx6000\pard\intbl Customer\cell Amount\cell\row
\trowd\cellx3000\cellx6000\pard\intbl Alex\cell 12.50\cell\row
\pard Before {\shp{\*\shpinst{\sp{\sn fFilled}{\sv 0}}{\shptxt \pard Boxed text\par}}} after\par
\pard Cited{\footnote\pard{\super\chftn} Left open";
    let els = parse(doc).unwrap();
    let got: Vec<(&str, &str, &str)> = els
        .iter()
        .map(|e| (e.kind.as_str(), e.text.as_str(), e.evidence))
        .collect();
    assert_eq!(
        got,
        [
            ("doco:Table", "Customer | Amount\nAlex | 12.50", "rtf"),
            ("doco:Paragraph", "Before after", "rtf"),
            ("doco:Paragraph", "Boxed text", "rtf"),
            ("doco:Paragraph", "Cited", "rtf"),
            ("doco:Paragraph", "1 Left open", "footnote"),
        ]
    );
    assert_eq!(els[0].header_rows, Some(1));
}
