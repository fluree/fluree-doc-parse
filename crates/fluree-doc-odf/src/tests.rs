//! Synthetic OpenDocument packages and what the readers make of them.

use super::*;
use fluree_doc_model::budget::{
    converting_to, Output, CELL_BYTES, ELEMENT_BYTES, FLAT_CELL_BYTES, MAX_MEMORY, TEXT_BYTES,
};
use std::io::Write;

const NS: &str = "xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" \
    xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\" \
    xmlns:table=\"urn:oasis:names:tc:opendocument:xmlns:table:1.0\" \
    xmlns:draw=\"urn:oasis:names:tc:opendocument:xmlns:drawing:1.0\" \
    xmlns:presentation=\"urn:oasis:names:tc:opendocument:xmlns:presentation:1.0\" \
    xmlns:style=\"urn:oasis:names:tc:opendocument:xmlns:style:1.0\" \
    xmlns:fo=\"urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0\" \
    xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
    xmlns:meta=\"urn:oasis:names:tc:opendocument:xmlns:meta:1.0\"";

/// A package of `kind` (`text`, `spreadsheet`, `presentation`) holding
/// `body` in its content, and the other parts given.
pub(crate) fn package(kind: &str, body: &str, extra: &[(&str, String)]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file("mimetype", stored).unwrap();
    write!(z, "application/vnd.oasis.opendocument.{kind}").unwrap();
    let content = format!(
        "<office:document-content {NS}>{}<office:body>{body}</office:body></office:document-content>",
        extra
            .iter()
            .find(|(n, _)| *n == "auto")
            .map_or(String::new(), |(_, s)| s.clone())
    );
    for (name, xml) in [("content.xml", content)].into_iter().chain(
        extra
            .iter()
            .filter(|(n, _)| *n != "auto")
            .map(|(n, s)| (*n, s.clone())),
    ) {
        z.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(xml.as_bytes()).unwrap();
    }
    z.finish().unwrap();
    buf.into_inner()
}

pub(crate) fn report() -> Vec<u8> {
    let body = "<office:text>\
        <text:p text:style-name=\"Title\">Quarterly report</text:p>\
        <text:h text:outline-level=\"1\">Orders</text:h>\
        <text:p>Alex<text:s text:c=\"2\"/>Sample signed<text:note text:note-class=\"footnote\">\
          <text:note-citation>1</text:note-citation><text:note-body><text:p>Survey of 2026.</text:p></text:note-body>\
        </text:note> the orders.<office:annotation><dc:creator>Sam Example</dc:creator><text:p>Source?</text:p></office:annotation><text:line-break/>Second line</text:p>\
        <text:h text:outline-level=\"2\">Items</text:h>\
        <text:list><text:list-item><text:p>Bolts</text:p></text:list-item>\
          <text:list-item><text:p>Nuts</text:p><text:list><text:list-item><text:p>Hex nuts</text:p></text:list-item></text:list></text:list-item></text:list>\
        <table:table table:name=\"T\"><table:table-header-rows><table:table-row>\
          <table:table-cell table:number-columns-spanned=\"2\"><text:p>Customer</text:p></table:table-cell><table:covered-table-cell/>\
          <table:table-cell><text:p>Amount</text:p></table:table-cell></table:table-row></table:table-header-rows>\
          <table:table-row><table:table-cell><text:p>Alex</text:p></table:table-cell><table:table-cell><text:p>Sample</text:p></table:table-cell>\
          <table:table-cell><text:p>12.50</text:p></table:table-cell></table:table-row></table:table>\
        <text:p>Ends here<text:note text:note-class=\"endnote\"><text:note-citation>i</text:note-citation>\
          <text:note-body><text:p>Method.</text:p></text:note-body></text:note></text:p>\
        </office:text>";
    let styles = format!(
        "<office:document-styles {NS}><office:master-styles><style:master-page style:name=\"Standard\">\
         <style:header><text:p>Ref: QA-7</text:p></style:header>\
         <style:footer><text:p>Uncontrolled when printed</text:p></style:footer>\
         </style:master-page></office:master-styles></office:document-styles>"
    );
    let meta = format!(
        "<office:document-meta {NS}><office:meta><dc:title>Q3</dc:title>\
         <meta:initial-creator>Alex Sample</meta:initial-creator>\
         <meta:creation-date>2026-03-14T09:30:00</meta:creation-date></office:meta></office:document-meta>"
    );
    package("text", body, &[("styles.xml", styles), ("meta.xml", meta)])
}

#[test]
fn a_text_document_reads_as_a_word_file_does() {
    let (els, info, format) = parse_with_info(&report()).unwrap();
    assert_eq!(format, Format::Text);
    let got: Vec<(&str, Option<usize>, &str, &str)> = els
        .iter()
        .map(|e| (e.kind.as_str(), e.level, e.text.as_str(), e.evidence))
        .collect();
    assert_eq!(
        got,
        [
            ("doco:Paragraph", None, "Ref: QA-7", "header"),
            ("doco:Title", None, "Quarterly report", "odt"),
            ("doco:SectionTitle", Some(1), "Orders", "odt"),
            (
                "doco:Paragraph",
                None,
                "Alex Sample signed the orders.\nSecond line",
                "odt"
            ),
            ("doco:SectionTitle", Some(2), "Items", "odt"),
            ("doc:ListItem", None, "Bolts", "odt"),
            ("doc:ListItem", None, "Nuts", "odt"),
            ("doc:ListItem", None, "Hex nuts", "odt"),
            (
                "doco:Table",
                None,
                "Customer |  | Amount\nAlex | Sample | 12.50",
                "odt"
            ),
            ("doco:Paragraph", None, "Ends here", "odt"),
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
    // A list in a list's item is a level deeper.
    let depths: Vec<usize> = els[5..8].iter().map(|e| e.depth).collect();
    assert_eq!(depths, [0, 0, 1]);
    let t = &els[8];
    assert_eq!(t.header_rows, Some(1));
    assert!(t.merged_left.as_ref().unwrap()[1]);
    assert!(els
        .iter()
        .all(|e| e.provenance == "odt" && e.bbox.is_none()));
    assert_eq!(info.title.as_deref(), Some("Q3"));
    assert_eq!(info.creators, ["Alex Sample"]);
    assert_eq!(info.created.as_deref(), Some("2026-03-14T09:30:00"));
}

#[test]
fn a_spreadsheet_reads_as_a_workbook_and_repeats_cost_nothing_while_empty() {
    let auto =
        "<office:automatic-styles><style:style style:name=\"ce1\" style:family=\"table-cell\">\
        <style:text-properties fo:font-weight=\"bold\"/></style:style></office:automatic-styles>";
    let body = "<office:spreadsheet><table:table table:name=\"Orders\">\
        <table:table-column table:number-columns-repeated=\"16384\"/>\
        <table:table-row><table:table-cell table:style-name=\"ce1\" office:value-type=\"string\"><text:p>Customer</text:p></table:table-cell>\
          <table:table-cell table:style-name=\"ce1\" office:value-type=\"string\"><text:p>Amount</text:p></table:table-cell>\
          <table:table-cell table:style-name=\"ce1\" office:value-type=\"string\"><text:p>Shipped</text:p></table:table-cell>\
          <table:table-cell table:number-columns-repeated=\"16381\"/></table:table-row>\
        <table:table-row><table:table-cell office:value-type=\"string\"><text:p>Alex Sample</text:p></table:table-cell>\
          <table:table-cell office:value-type=\"percentage\" office:value=\"0.125\"><text:p>12.5%</text:p></table:table-cell>\
          <table:table-cell office:value-type=\"date\" office:date-value=\"2026-03-14\"><text:p>14/03/2026</text:p></table:table-cell></table:table-row>\
        <table:table-row><table:table-cell office:value-type=\"string\"><text:p>Sam Example</text:p></table:table-cell>\
          <table:table-cell office:value-type=\"float\" office:value=\"7\"><text:p>7.00</text:p></table:table-cell>\
          <table:table-cell office:value-type=\"boolean\" office:boolean-value=\"true\"><text:p>TRUE</text:p></table:table-cell></table:table-row>\
        <table:table-row table:number-rows-repeated=\"1048573\"><table:table-cell table:number-columns-repeated=\"16384\"/></table:table-row>\
        </table:table><table:table table:name=\"Notes\"><table:table-row><table:table-cell office:value-type=\"time\" office:time-value=\"PT13H05M30S\"><text:p>13:05</text:p></table:table-cell></table:table-row></table:table>\
        </office:spreadsheet>";
    let bytes = package("spreadsheet", body, &[("auto", auto.into())]);
    let els = parse(&bytes).unwrap();
    assert_eq!(els[0].text, "Orders");
    let t = els.iter().find(|e| e.kind == "doco:Table").unwrap();
    let cells = t.cells.as_ref().unwrap();
    // What the cell shows, with what it stores beside it.
    assert_eq!(cells[1], ["Alex Sample", "12.5%", "14/03/2026"]);
    assert_eq!(t.header_rows, Some(1));
    let d = t.datums.as_ref().unwrap();
    assert_eq!(d[1][1].as_ref().unwrap().value, "0.125");
    assert_eq!(d[1][2].as_ref().unwrap().datatype, "xsd:date");
    assert_eq!(d[2][2].as_ref().unwrap().value, "true");
    assert!(els.iter().all(|e| e.provenance == "ods"));
    let notes = els
        .iter()
        .find(|e| e.page == 1 && e.text == "13:05")
        .unwrap();
    assert_eq!(notes.kind, "doco:Paragraph");
}

#[test]
fn a_repeated_cell_that_holds_something_is_bounded() {
    // One cell repeated across the row, the row repeated a million times:
    // sixteen billion cells, asked for in a few hundred bytes.
    let body = "<office:spreadsheet><table:table table:name=\"Bomb\">\
        <table:table-row table:number-rows-repeated=\"1048576\">\
        <table:table-cell table:number-columns-repeated=\"16384\" office:value-type=\"float\" office:value=\"1\"><text:p>1</text:p></table:table-cell>\
        </table:table-row></table:table></office:spreadsheet>";
    let bytes = package("spreadsheet", body, &[]);
    assert!(matches!(parse(&bytes), Err(OdfError::TooLarge(_))));
}

#[test]
fn a_presentation_reads_as_a_deck_does() {
    let body = "<office:presentation>\
        <draw:page draw:name=\"p1\"><draw:frame presentation:class=\"title\"><draw:text-box><text:p>Quarterly review</text:p></draw:text-box></draw:frame>\
          <draw:frame presentation:class=\"outline\"><draw:text-box><text:list><text:list-item><text:p>Orders up</text:p></text:list-item></text:list>\
          <text:p>Costs down</text:p></draw:text-box></draw:frame>\
          <presentation:notes><draw:frame><draw:text-box><text:p>Speaker only</text:p></draw:text-box></draw:frame></presentation:notes></draw:page>\
        <draw:page draw:name=\"p2\"><draw:frame presentation:class=\"title\"><draw:text-box><text:p>Detail</text:p></draw:text-box></draw:frame>\
          <draw:frame><table:table><table:table-row><table:table-cell><text:p>Region</text:p></table:table-cell><table:table-cell><text:p>West</text:p></table:table-cell></table:table-row>\
          <table:table-row><table:table-cell><text:p>Owner</text:p></table:table-cell><table:table-cell><text:p>Alex Sample</text:p></table:table-cell></table:table-row></table:table></draw:frame>\
          <draw:custom-shape><text:p>Shape text</text:p></draw:custom-shape></draw:page>\
        </office:presentation>";
    let els = parse(&package("presentation", body, &[])).unwrap();
    let got: Vec<(usize, &str, &str)> = els
        .iter()
        .map(|e| (e.page, e.kind.as_str(), e.text.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (0, "doco:SectionTitle", "Quarterly review"),
            (0, "doc:ListItem", "Orders up"),
            (0, "doc:ListItem", "Costs down"),
            (1, "doco:SectionTitle", "Detail"),
            (1, "doco:Table", "Region | West\nOwner | Alex Sample"),
            (1, "doco:Paragraph", "Shape text"),
        ]
    );
    assert!(els.iter().all(|e| e.provenance == "odp"));
}

#[test]
fn the_kind_is_the_mimetype_and_parts_are_bounded() {
    assert_eq!(Format::sniff(&report()), Some(Format::Text));
    assert_eq!(Format::sniff(&package("graphics", "", &[])), None);
    assert!(matches!(
        parse(&package("graphics", "", &[])),
        Err(OdfError::NotOdf)
    ));
    assert_eq!(Format::sniff(b"PK\x03\x04"), None);
    // A content part past 64 MiB, a bound of its own it no longer has:
    // read, within the budget, which bounds it
    // ([`a_part_past_what_the_budget_has_left_is_refused_before_it_is_read`]).
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let deflate = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    z.start_file("mimetype", deflate).unwrap();
    z.write_all(b"application/vnd.oasis.opendocument.text")
        .unwrap();
    z.start_file("content.xml", deflate).unwrap();
    let chunk = vec![b' '; 1 << 20];
    for _ in 0..65 {
        z.write_all(&chunk).unwrap();
    }
    z.finish().unwrap();
    assert!(parse(&buf.into_inner()).unwrap().is_empty());
    // Nesting past the tree's bound.
    let deep = format!(
        "<office:text>{}{}</office:text>",
        "<text:section>".repeat(300),
        "</text:section>".repeat(300)
    );
    assert!(matches!(
        parse(&package("text", &deep, &[])),
        Err(OdfError::TooLarge(_))
    ));
}

#[test]
fn a_paragraph_in_a_style_based_on_a_heading_is_one() {
    let auto = "<office:automatic-styles>\
        <style:style style:name=\"P1\" style:family=\"paragraph\" style:parent-style-name=\"Heading_20_1\"/>\
        <style:style style:name=\"P2\" style:family=\"paragraph\" style:parent-style-name=\"Mine\"/>\
        <style:style style:name=\"P3\" style:family=\"paragraph\" style:parent-style-name=\"Text_20_body\"/>\
        </office:automatic-styles>";
    let styles = format!(
        "<office:document-styles {NS}><office:styles>\
         <style:style style:name=\"Heading_20_1\" style:display-name=\"Heading 1\" style:family=\"paragraph\"/>\
         <style:style style:name=\"Mine\" style:family=\"paragraph\" style:default-outline-level=\"3\"/>\
         <style:style style:name=\"Text_20_body\" style:display-name=\"Text body\" style:family=\"paragraph\"/>\
         </office:styles></office:document-styles>"
    );
    let body = "<office:text><text:p text:style-name=\"P1\">Orders</text:p>\
        <text:p text:style-name=\"P2\">Detail</text:p><text:p text:style-name=\"P3\">Body</text:p></office:text>";
    let els = parse(&package(
        "text",
        body,
        &[("auto", auto.into()), ("styles.xml", styles)],
    ))
    .unwrap();
    let got: Vec<(&str, Option<usize>)> = els.iter().map(|e| (e.kind.as_str(), e.level)).collect();
    assert_eq!(
        got,
        [
            ("doco:SectionTitle", Some(1)),
            ("doco:SectionTitle", Some(3)),
            ("doco:Paragraph", None)
        ]
    );
}

/// A text document whose body is `body`.
fn text(body: &str) -> Vec<u8> {
    package("text", &format!("<office:text>{body}</office:text>"), &[])
}

fn texts(els: &[Element]) -> Vec<&str> {
    els.iter().map(|e| e.text.as_str()).collect()
}

/// What reading `pkg`'s content and styles spends, their text and their
/// trees, before any element is made from them.
fn reading(pkg: &[u8]) -> u64 {
    let mut b = Budget::new(MAX_MEMORY, 0);
    let mut p = Package::open(pkg).unwrap();
    for name in ["content.xml", "styles.xml"] {
        tree(&mut p, name, &mut b).unwrap();
    }
    MAX_MEMORY - b.memory_left()
}

#[test]
fn a_repeated_cell_of_text_is_bounded_by_its_bytes() {
    // 7 KiB of text in a cell repeated 1024 times: 7 MiB of text asked for
    // in a few hundred bytes, each copy spent before it is made.
    let word = "word ".repeat(1400);
    let cell = |repeat: usize| {
        text(&format!(
            "<table:table><table:table-row><table:table-cell table:number-columns-repeated=\"{repeat}\">\
             <text:p>{}</text:p></table:table-cell></table:table-row></table:table>",
            word.trim()
        ))
    };
    // For Markdown, which copies nothing: the table, and each copy a cell
    // that holds the text, written once in its row.
    let need = |n: u64| {
        ELEMENT_BYTES
            + n * (FLAT_CELL_BYTES + CELL_BYTES)
            + n * (word.trim().len() as u64) * TEXT_BYTES
    };
    let need = |n: u64| need(n) + reading(&cell(n as usize));
    let within = |repeat: usize, memory: u64| {
        parse_within(
            &cell(repeat),
            &mut Budget::new(memory, 0).for_output(Output::Flat),
        )
    };
    let els = within(1024, need(1024)).unwrap().0;
    assert_eq!(els[0].cells.as_ref().unwrap()[0].len(), 1024);
    let refused = within(1024, need(1024) - 1).unwrap_err();
    assert!(matches!(refused, OdfError::TooLarge(_)), "{refused}");
}

#[test]
fn paragraphs_past_the_budget_are_refused_before_they_are_made() {
    // A paragraph of one letter is six bytes of XML, and a node of the
    // output: 1.5 million of them, 66 KB zipped, are 6 GB of DoCO.
    let body = "<text:p>w</text:p>".repeat(1000);
    let within = |memory: u64, output: Output| {
        parse_within(&text(&body), &mut Budget::new(memory, 0).for_output(output))
    };
    let need = 1000 * (ELEMENT_BYTES + TEXT_BYTES) + reading(&text(&body));
    assert_eq!(within(need, Output::Doco).unwrap().0.len(), 1000);
    let refused = within(need - 1, Output::Doco).unwrap_err();
    assert!(matches!(refused, OdfError::TooLarge(_)), "{refused}");
    // Markdown writes a paragraph as a line: it costs less.
    let flat = 1000 * (ELEMENT_BYTES + TEXT_BYTES) + reading(&text(&body));
    assert!(within(flat, Output::Flat).is_ok());
    assert!(within(flat - 1, Output::Flat).is_err());
    // A presentation's, the same.
    let slides = package(
        "presentation",
        &format!(
            "<office:presentation><draw:page><draw:frame><draw:text-box>{body}</draw:text-box></draw:frame></draw:page></office:presentation>"
        ),
        &[],
    );
    let need = 1000 * (ELEMENT_BYTES + TEXT_BYTES) + reading(&slides);
    assert!(parse_within(&slides, &mut Budget::new(need, 0)).is_ok());
    assert!(parse_within(&slides, &mut Budget::new(need - 1, 0)).is_err());
}

#[test]
fn a_table_padded_to_its_widest_row_counts_the_padding() {
    // One row of 4096 cells, one holding text, then 1100 rows of none: a
    // grid of 4.5 million cells, every one spent.
    let rows = "<table:table-row/>".repeat(1100);
    let body = format!(
        "<table:table><table:table-row><table:table-cell table:number-columns-repeated=\"1024\"/>\
         <table:table-cell table:number-columns-repeated=\"1024\"/><table:table-cell table:number-columns-repeated=\"1024\"/>\
         <table:table-cell table:number-columns-repeated=\"1024\"><text:p>x</text:p></table:table-cell></table:table-row>{rows}</table:table>"
    );
    // The table, and the 1024 cells that hold `x`, at Markdown's costs.
    let need = 1101 * 4096 * CELL_BYTES
        + ELEMENT_BYTES
        + 1024 * (FLAT_CELL_BYTES + TEXT_BYTES)
        + reading(&text(&body));
    let within = |memory: u64| {
        parse_within(
            &text(&body),
            &mut Budget::new(memory, 0).for_output(Output::Flat),
        )
    };
    assert_eq!(
        within(need).unwrap().0[0].cells.as_ref().unwrap().len(),
        1101
    );
    assert!(matches!(within(need - 1), Err(OdfError::TooLarge(_))));
}

#[test]
fn overlapping_merges_mark_no_more_than_the_table() {
    // Every row's cell merged down past the table's end: each merge is cut
    // at the table, and together they mark at most its cells.
    let rows = "<table:table-row><table:table-cell table:number-rows-spanned=\"1000000\" \
                table:number-columns-spanned=\"1000000\"><text:p>v</text:p></table:table-cell>\
                <table:table-cell><text:p>w</text:p></table:table-cell></table:table-row>"
        .repeat(3);
    let els = parse(&text(&format!("<table:table>{rows}</table:table>"))).unwrap();
    let down = els[0].merged_down.as_ref().unwrap();
    let left = els[0].merged_left.as_ref().unwrap();
    assert_eq!(*down, [false, false, true, false, true, false]);
    assert_eq!(*left, [false, true, false, true, false, true]);
}

#[test]
fn a_text_box_anchored_to_a_paragraph_is_read_after_it() {
    let frame = |text: &str| {
        format!(
            "<draw:frame text:anchor-type=\"paragraph\"><draw:text-box><text:p>{text}</text:p></draw:text-box></draw:frame>"
        )
    };
    let body = format!(
        "<text:p>Before<text:span>{}</text:span> the box.</text:p><text:p>{}</text:p><text:p>After.</text:p>",
        frame("Callout inside a text box."),
        frame("A box alone on its line.")
    );
    let els = parse(&text(&body)).unwrap();
    assert_eq!(
        texts(&els),
        [
            "Before the box.",
            "Callout inside a text box.",
            "A box alone on its line.",
            "After."
        ]
    );
}

#[test]
fn a_list_in_a_cell_reads_its_items_apart() {
    let body = "<table:table><table:table-row><table:table-cell><text:list>\
        <text:list-item><text:p>Alpha</text:p></text:list-item>\
        <text:list-item><text:p>Beta</text:p></text:list-item></text:list></table:table-cell>\
        <table:table-cell><text:p>One</text:p><text:p>Two</text:p></table:table-cell></table:table-row></table:table>";
    let els = parse(&text(body)).unwrap();
    assert_eq!(els[0].cells.as_ref().unwrap()[0], ["Alpha Beta", "One Two"]);
}

#[test]
fn a_header_repeated_on_master_pages_is_read_once_and_its_own_lines_kept() {
    let master = |name: &str| {
        format!(
            "<style:master-page style:name=\"{name}\"><style:header><text:p>Ref: QA-7</text:p>\
             <text:p>Rev 2</text:p></style:header></style:master-page>"
        )
    };
    let styles = format!(
        "<office:document-styles {NS}><office:master-styles>{}{}</office:master-styles></office:document-styles>",
        master("Standard"),
        master("First")
    );
    let bytes = package(
        "text",
        "<office:text><text:p>Body</text:p></office:text>",
        &[("styles.xml", styles)],
    );
    assert_eq!(
        texts(&parse(&bytes).unwrap()),
        ["Ref: QA-7", "Rev 2", "Body"]
    );
}

#[test]
fn copies_of_merged_cells_are_spent() {
    // A cell spanning n rows over n covered cells that each hold text of
    // their own: DoCO joins the n texts and writes them on each row.
    let table = |n: usize, first: &str| {
        let row = |cell: String| format!("<table:table-row>{cell}</table:table-row>");
        format!(
            "<table:table>{}{}</table:table>",
            row(format!(
                "<table:table-cell table:number-rows-spanned=\"{n}\"><text:p>{first}</text:p></table:table-cell>"
            )),
            row("<table:covered-table-cell><text:p>x</text:p></table:covered-table-cell>".into())
                .repeat(n - 1)
        )
    };
    // 3,000 rows: 3,000 nodes read, 18 million bytes of copies, past 64 MiB
    // only with the copies counted.
    let within = |n: usize, first: &str, output: Output| {
        parse_within(
            &text(&table(n, first)),
            &mut Budget::new(64 << 20, 0).for_output(output),
        )
    };
    assert!(within(1_000, "x", Output::Doco).is_ok());
    let refused = within(3_000, "x", Output::Doco).unwrap_err();
    assert!(refused.to_string().contains("copies"), "{refused}");
    // The text shows the joined value on each row too; Markdown, once.
    assert!(within(3_000, "x", Output::Text).is_err());
    assert!(within(3_000, "x", Output::Flat).is_ok());
    // One long value over many rows, the same, within the whole budget: a
    // file of a few kilobytes whose copies are 7 GB.
    let long = "y".repeat(200_000);
    assert!(parse(&text(&table(3, &long))).is_ok());
    assert!(matches!(
        parse(&text(&table(3_000, &long))),
        Err(OdfError::TooLarge(_))
    ));
    assert!(converting_to(Output::Flat, || parse(&text(&table(3_000, &long)))).is_ok());
}

#[test]
fn a_parts_tree_is_spent_before_its_elements_are_made() {
    // 200,000 empty elements, 4 bytes of XML each, the most a part packs
    // into nodes: a tree no element is made of, 136 bytes a node measured,
    // refused by a budget that holds the part and 10,000 nodes.
    let pkg = text(&format!("<text:p>w</text:p>{}", "<a/>".repeat(200_000)));
    let part = 800_000 * PART_BYTES;
    let refused = parse_within(&pkg, &mut Budget::new(part + 10_000 * CELL_BYTES, 0)).unwrap_err();
    assert!(refused.to_string().contains("cells past the"), "{refused}");
    // Each node, each attribute and the list of them, and the text they
    // hold.
    let mut b = Budget::new(MAX_MEMORY, 0);
    let t = Tree::parse("<a b=\"\" c=\"v\">x</a>", &mut b).unwrap();
    assert_eq!(t.nodes.len(), 3);
    let node = tree::NODE_CELLS * CELL_BYTES;
    let attr = tree::ATTR_CELLS * CELL_BYTES;
    assert_eq!(
        MAX_MEMORY - b.memory_left(),
        2 * node + 3 * attr + 2 * CELL_BYTES
    );
    // A node of one attribute, 370 bytes measured, counts four slots.
    let mut b = Budget::new(MAX_MEMORY, 0);
    Tree::parse("<a b=\"\"/>", &mut b).unwrap();
    assert_eq!(MAX_MEMORY - b.memory_left(), 4 * CELL_BYTES);
    // Within what the reading costs, read; a byte less, refused.
    let need = reading(&pkg) + ELEMENT_BYTES + TEXT_BYTES;
    assert!(parse_within(&pkg, &mut Budget::new(need, 0)).is_ok());
    assert!(parse_within(&pkg, &mut Budget::new(need - 1, 0)).is_err());
}

#[test]
fn a_part_past_what_the_budget_has_left_is_refused_before_it_is_read() {
    // A content part of 3 MiB of spaces, past a budget of 4 MiB at two
    // bytes a byte.
    let pkg = text(&" ".repeat(3 << 20));
    let refused = parse_within(&pkg, &mut Budget::new(4 << 20, 0)).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "OpenDocument file too large: bytes of parts inflated past the 4 MiB of memory a document may take"
    );
}

#[test]
fn a_corrupt_part_is_a_broken_file_not_one_too_large() {
    // A content part whose compressed bytes are damaged: no bound is
    // passed, so nothing a larger limit would read.
    let mut pkg = text(&"<text:p>Some words to compress.</text:p>".repeat(200));
    let name = b"content.xml";
    let header = pkg.windows(name.len()).position(|w| w == name).unwrap() - 30;
    let le16 = |at: usize| u16::from_le_bytes([pkg[at], pkg[at + 1]]) as usize;
    let data = header + 30 + le16(header + 26) + le16(header + 28);
    for b in &mut pkg[data + 8..data + 24] {
        *b ^= 0x5a;
    }
    let err = parse(&pkg).unwrap_err();
    assert!(matches!(err, OdfError::Zip(_)), "{err}");
}

/// A text document whose content part is `prefix`, `bytes`, then the end of
/// a paragraph: bytes that need not be UTF-8.
fn text_with_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file("mimetype", stored).unwrap();
    write!(z, "application/vnd.oasis.opendocument.text").unwrap();
    z.start_file("content.xml", zip::write::SimpleFileOptions::default())
        .unwrap();
    write!(
        z,
        "<office:document-content {NS}><office:body><office:text><text:p>a"
    )
    .unwrap();
    z.write_all(bytes).unwrap();
    write!(
        z,
        "b</text:p></office:text></office:body></office:document-content>"
    )
    .unwrap();
    z.finish().unwrap();
    buf.into_inner()
}

#[test]
fn a_part_not_utf8_is_spent_at_its_replacements_before_they_are_made() {
    // Each byte that is not UTF-8 is read as U+FFFD, three bytes of it.
    let els = parse(&text_with_bytes(b"\xff")).unwrap();
    assert_eq!(els[0].text, "a\u{fffd}b");
    // A megabyte of them makes three of replacements beside it: past a
    // budget that holds the part at two bytes a byte, and more, refused
    // as bytes of the part, before they are made.
    let n = 1 << 20;
    let pkg = text_with_bytes(&vec![0xff; n]);
    let budget = (n as u64 + 1_000) * PART_BYTES + (n as u64);
    let refused = parse_within(&pkg, &mut Budget::new(budget, u64::MAX)).unwrap_err();
    assert!(
        refused.to_string().contains("bytes of parts inflated"),
        "{refused}"
    );
}

#[test]
fn frames_nested_in_paragraphs_are_read_in_linear_time() {
    // A paragraph holding a text box holding a paragraph, `n` deep, each
    // with text of its own: each paragraph checked the room for the text
    // of every frame under it, again at each level.
    let nested = |n: usize| {
        let words = "word ".repeat(20_000);
        let mut body = String::new();
        for _ in 0..n {
            body.push_str(&format!("<text:p>{words}<draw:frame><draw:text-box>"));
        }
        for _ in 0..n {
            body.push_str("</draw:text-box></draw:frame></text:p>");
        }
        text(&body)
    };
    let els = parse(&nested(3)).unwrap();
    assert_eq!(els.len(), 3);
    fluree_doc_model::scaling::assert_linear("frames nested", 20, nested, |pkg| {
        assert!(parse(pkg).is_ok())
    });
}

#[test]
fn a_notes_citation_is_its_own() {
    // The citation is the note's child: one inside its body is text.
    let body = "<text:p>Cited<text:note><text:note-body><text:p>Body \
                <text:note-citation>9</text:note-citation></text:p></text:note-body>\
                <text:note-citation>1</text:note-citation></text:note></text:p>";
    let els = parse(&text(body)).unwrap();
    assert_eq!(els.last().unwrap().text, "1 Body 9");
}

#[test]
fn a_texts_tables_and_paragraphs_spend_the_documents_budget() {
    // A hundred rows of a cell repeated a hundred times: 10,000 cells that
    // hold text from a few kilobytes, each spent at 512 bytes and its slot,
    // whether Markdown writes it once in its row or DoCO as a node of its
    // own, a node at a time: about 6.5 MB, past a thousand elements' worth.
    let row = "<table:table-row><table:table-cell table:number-columns-repeated=\"100\">\
               <text:p>x</text:p></table:table-cell></table:table-row>";
    let table = text(&format!("<table:table>{}</table:table>", row.repeat(100)));
    for output in [Output::Doco, Output::Flat] {
        let budget = |n: u64| Budget::new(n * ELEMENT_BYTES, 0).for_output(output);
        let refused = parse_within(&table, &mut budget(1_000)).unwrap_err();
        assert!(refused.to_string().contains("past"), "{refused}");
        let els = parse_within(&table, &mut budget(5_000)).unwrap();
        assert_eq!(els.0[0].cells.as_ref().unwrap().len(), 100);
    }
    // Paragraphs, each an element and its text, are spent as well.
    let paras = text(&"<text:p>x</text:p>".repeat(100));
    let need = reading(&paras) + 100 * (ELEMENT_BYTES + TEXT_BYTES);
    assert!(parse_within(&paras, &mut Budget::new(need, 0)).is_ok());
    assert!(parse_within(&paras, &mut Budget::new(need - 1, 0)).is_err());
}

#[test]
fn a_cells_tabs_spaces_and_line_breaks_part_its_words() {
    let body = "<office:spreadsheet><table:table table:name=\"S\"><table:table-row>\
        <table:table-cell office:value-type=\"string\"><text:p>Total<text:tab/>42</text:p></table:table-cell>\
        <table:table-cell office:value-type=\"string\"><text:p>A<text:s text:c=\"3\"/>B<text:line-break/>C</text:p></table:table-cell>\
        </table:table-row></table:table></office:spreadsheet>";
    let els = parse(&package("spreadsheet", body, &[])).unwrap();
    let t = els.iter().find(|e| e.kind == "doco:Table").unwrap();
    // A cell is one line in a table, as an .xlsx cell's is.
    assert_eq!(t.cells.as_ref().unwrap()[0], ["Total 42", "A B C"]);
}

#[test]
fn broken_or_deep_documents_read_as_far_as_they_go() {
    // No content part: refused as such.
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    z.start_file("mimetype", zip::write::SimpleFileOptions::default())
        .unwrap();
    z.write_all(b"application/vnd.oasis.opendocument.text")
        .unwrap();
    z.finish().unwrap();
    assert!(matches!(parse(&buf.into_inner()), Err(OdfError::NoContent)));
    // Not a zip at all, past its signature.
    assert!(parse(b"PK\x03\x04 cut short").is_err());
    // XML cut off mid-element keeps what was read before it, the
    // paragraph it stops in included.
    let cut = "<office:text><text:p>Kept</text:p><text:p>Cut off<text:span";
    let els = parse(&package("text", cut, &[])).unwrap();
    assert_eq!(texts(&els), ["Kept", "Cut off"]);
    // Lists nested as deep as the tree allows, 126 lists in 126 items in
    // the three elements around a text: each item read, its depth capped,
    // within a test thread's 2 MiB stack.
    let deep = format!(
        "<office:text>{}<text:p>Deepest</text:p>{}</office:text>",
        "<text:list><text:list-item>".repeat(126),
        "</text:list-item></text:list>".repeat(126)
    );
    let els = parse(&package("text", &deep, &[])).unwrap();
    assert_eq!(texts(&els), ["Deepest"]);
    assert_eq!(els[0].kind, "doc:ListItem");
    assert_eq!(els[0].depth, MAX_LIST_DEPTH);
}
