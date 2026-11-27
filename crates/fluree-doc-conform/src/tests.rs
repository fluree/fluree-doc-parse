use super::*;

fn el(kind: &str, text: &str) -> Element {
    Element::new(kind, text, "rust", "layout")
}

fn heading(text: &str, level: usize) -> Element {
    Element {
        level: Some(level),
        ..el("doco:SectionTitle", text)
    }
}

fn item(text: &str, depth: usize) -> Element {
    Element {
        depth,
        ..el("doc:ListItem", text)
    }
}

fn table(id: &str, rows: &[&[&str]], header: usize) -> Element {
    Element {
        id: id.into(),
        cells: Some(
            rows.iter()
                .map(|r| r.iter().map(|c| c.to_string()).collect())
                .collect(),
        ),
        header_rows: Some(header),
        ..el("doco:Table", "")
    }
}

fn classes(els: &[Element], format: Format) -> Vec<&'static str> {
    conform(els, &Notes::default())
        .into_iter()
        .filter(|v| v.format == format)
        .map(|v| v.class)
        .collect()
}

#[test]
fn a_plain_document_conforms() {
    let els = [
        el("doco:Title", "Annual report"),
        heading("Results", 2),
        el("doco:Paragraph", "Revenue rose by 4% (see table)."),
        item("First", 0),
        item("Nested", 1),
        item("3. Third", 0),
        table("t1", &[&["Year", "Sales"], &["2024", "10"]], 1),
        el("doco:Paragraph", "After the table."),
    ];
    assert_eq!(conform(&els, &Notes::default()), []);
}

#[test]
fn a_table_continued_over_a_page_is_one_table() {
    let mut second = table("t2", &[&["Year", "Sales"], &["2025", "12"]], 1);
    second.continues = Some("t1".into());
    let els = [
        table("t1", &[&["Year", "Sales"], &["2024", "10"]], 1),
        second,
    ];
    assert_eq!(conform(&els, &Notes::default()), []);
}

#[test]
fn each_class_is_caught() {
    // A heading read back at another level.
    let a = classes(&[heading("Deep", 7), heading("Results", 2)], Format::Md);
    assert_eq!(a, Vec::<&str>::new(), "levels past six are written as six");
    // A table with no header row is written under an empty one, and read
    // back without a header.
    let t = [table("t", &[&["a", "1"], &["b", "2"]], 0)];
    assert_eq!(classes(&t, Format::Md), Vec::<&str>::new());
    assert_eq!(classes(&t, Format::Xhtml), Vec::<&str>::new());
    // So a header row with nothing in it reads back as no row at all, which
    // loses nothing.
    let t = [table("t", &[&["", ""], &["b", "2"]], 1)];
    assert_eq!(classes(&t, Format::Md), Vec::<&str>::new());
    // Restated header rows have no markup of their own yet.
    let mut r = table(
        "t",
        &[&["Time", "A%"], &["0", "95"], &["Time", "B%"], &["1", "5"]],
        1,
    );
    r.restated_headers = Some(vec![2]);
    assert!(classes(&[r.clone()], Format::Xhtml).contains(&"table.restated_headers"));
}

#[test]
fn alignment_tells_lost_split_merged_and_moved_apart() {
    let a = reference(
        &[
            el("doco:Paragraph", "One two three"),
            el("doco:Paragraph", "Four"),
            el("doco:Paragraph", "Five six"),
            el("doco:Paragraph", "Seven"),
            el("doco:Paragraph", "Eight"),
            el("doco:Paragraph", "Nine"),
        ],
        Format::Md,
    );
    let b = read_back(&[
        el("doco:Paragraph", "One two"),
        el("doco:Paragraph", "three"),
        el("doco:Paragraph", "Five sixSeven"),
        el("doco:Paragraph", "Eight"),
        el("doco:Paragraph", "Nine"),
        el("doco:Paragraph", "Four"),
    ]);
    let mut c = Checker {
        format: Format::Md,
        a: &a,
        b: &b,
        out: Vec::new(),
        lost: Vec::new(),
        extra: Vec::new(),
    };
    c.run();
    let got: Vec<(&str, Option<usize>)> = c.out.iter().map(|v| (v.class, v.element)).collect();
    assert_eq!(
        got,
        [
            ("block.split", Some(0)),
            ("block.merged", Some(2)),
            ("order", Some(1)),
        ]
    );
}

#[test]
fn a_span_is_compared_by_the_cell_that_holds_it() {
    // `Region` spans two rows; its text is the span's wherever a reader
    // left it in the grid.
    let mut t = table("t", &[&["Region", "Q1"], &["", "4"], &["South", "5"]], 0);
    t.header_rows = Some(1);
    t.merged_down = Some(vec![false, false, true, false, false, false]);
    let c = canonical(
        t.cells.clone().unwrap(),
        t.merged_down.clone().unwrap(),
        vec![false; 6],
        Some(1),
        BTreeSet::new(),
        BTreeSet::new(),
    );
    assert_eq!(c.spans, BTreeMap::from([((0, 0), (2, 1))]));
    assert_eq!(c.cells[0], "Region");
    // A flagged cell holding text no span takes in is a cell of its own.
    let c = canonical(
        vec![vec!["a".into(), "b".into()]],
        vec![true, false],
        vec![false, false],
        Some(0),
        BTreeSet::new(),
        BTreeSet::new(),
    );
    assert_eq!(c.cells, ["a", "b"]);
    assert!(c.spans.is_empty());
}

#[test]
fn a_depth_is_read_as_nesting() {
    // A procedure's items numbered `8.1.` and `8.1.1.` under a heading `8.`
    // open at depth 1: nested in no item, they are a list of their own.
    let els = [
        heading("8. Responsibilities", 1),
        item("8.1. Owners", 1),
        item("8.1.1. Train", 2),
        item("8.2. Approvers", 1),
    ];
    assert_eq!(conform(&els, &Notes::default()), []);
    let depths = |els: &[Element]| -> Vec<usize> {
        reference(els, Format::Xhtml)
            .iter()
            .map(|u| match u.kind {
                Kind::Item(d) => d,
                _ => usize::MAX,
            })
            .collect()
    };
    assert_eq!(
        depths(&[item("a", 2), item("b", 1), item("c", 0)]),
        [0, 0, 0]
    );
    assert_eq!(
        depths(&[item("a", 0), item("b", 2), item("c", 1)]),
        [0, 1, 1]
    );
    // Anything between two items ends the list.
    assert_eq!(
        depths(&[item("a", 0), el("doco:Paragraph", "p"), item("b", 1)]),
        [0, usize::MAX, 0]
    );
}

#[test]
fn text_markdown_reads_as_markup_is_written_as_text() {
    // Each would read back as something else were it written as it is.
    let texts = [
        "1. The purpose of this Regulation",   // a list
        "* Sections omitted are not listed",   // a list
        "-",                                   // an empty item
        "4.",                                  // an empty item
        "_______________________",             // a rule
        "~~~~ ~",                              // a fence, swallowing what follows
        "> +300%",                             // a quote
        "# of Users: 50",                      // a heading
        "From: Ann Lee <ann@example.org>",     // an autolink
        "SPARQL only, as FROM <iri> would",    // an HTML tag
        "stats.classes[*].properties[*].refs", // emphasis
        "Fresh`N`Fruity and `more`",           // code
        "x &amp; y &#65;",                     // entities
        "L\\/2 and \\* and a\\",               // escapes
        "Exclusive\n---------",                // a setext heading
        "line one\n1. two",                    // a list interrupting
        "a | b\n--- | ---",                    // a table
        "[^1]: a note",                        // a footnote definition
        "code\n\n    indented",                // a blank line, then code
        "-_z and later\n_______",              // a rule's rest closing emphasis
        "said, ``I don't'' and `Hey ``ok''",   // code span closed by an escape
    ];
    let mut els: Vec<Element> = texts.iter().map(|t| el("doco:Paragraph", t)).collect();
    els.push(heading("Section #", 2));
    els.push(item("- nested?", 0));
    els.push(table("t", &[&["a\\", "*b*"], &["<c>", "d"]], 1));
    // An item whose link takes in its own number keeps the number its
    // marker, outside the link.
    let mut linked = item("1. INCITE", 0);
    linked.links = Some(vec![
        fluree_doc_model::Link::uri("https://e.example").spanning(0, 9)
    ]);
    els.push(linked);
    let md: Vec<_> = conform(&els, &Notes::default())
        .into_iter()
        .filter(|v| v.format == Format::Md)
        .collect();
    assert_eq!(md, []);
    // A lone star after a value, and words joined by underscores, open
    // nothing and are written as they are.
    let plain = to_md(&[el("doco:Paragraph", "p < 0.05* and snake_case_name, C#")]);
    assert_eq!(plain, "p < 0.05* and snake_case_name, C#\n\n");
}

fn to_md(els: &[Element]) -> String {
    fluree_doc_model::to_markdown(els)
}
