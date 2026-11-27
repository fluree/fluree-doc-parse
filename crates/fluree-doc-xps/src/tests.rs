//! Synthetic XPS packages and what the reader makes of them.

use super::*;
use std::io::Write;

const FDSEQ: &str = "http://schemas.microsoft.com/xps/2005/06/fixedrepresentation";

/// A package of `parts`, with the root relationship naming the sequence.
pub(crate) fn package(parts: &[(&str, String)]) -> Vec<u8> {
    package_as(FDSEQ, parts)
}

/// [`package`], the root relationship of type `kind`.
fn package_as(kind: &str, parts: &[(&str, String)]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let opts = zip::write::SimpleFileOptions::default();
    let rels = format!(
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"R0\" Type=\"{kind}\" Target=\"/FixedDocSeq.fdseq\"/></Relationships>"
    );
    for (name, body) in [("_rels/.rels", rels)]
        .into_iter()
        .chain(parts.iter().map(|(n, b)| (*n, b.clone())))
    {
        z.start_file(name, opts).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    }
    z.finish().unwrap();
    buf.into_inner()
}

/// A run of text at `(x, y)`, `em` high, its characters' advances given
/// in hundredths of an em.
pub(crate) fn glyphs(x: f64, y: f64, em: f64, text: &str) -> String {
    let indices: Vec<String> = text
        .chars()
        .map(|c| format!(",{}", if c == ' ' { 25 } else { 55 }))
        .collect();
    format!(
        "<Glyphs OriginX=\"{x}\" OriginY=\"{y}\" FontRenderingEmSize=\"{em}\" \
         FontUri=\"/Resources/f.odttf\" Fill=\"#ff000000\" UnicodeString=\"{text}\" Indices=\"{}\"/>",
        indices.join(";")
    )
}

/// A page of these elements, US Letter.
pub(crate) fn fixed_page(body: &str) -> String {
    format!(
        "<FixedPage Width=\"816\" Height=\"1056\" xmlns=\"http://schemas.microsoft.com/xps/2005/06\" xml:lang=\"en-US\">{body}</FixedPage>"
    )
}

/// A picture filling the whole page: a screen printed to XPS.
pub(crate) fn screenshot() -> String {
    fixed_page(
        "<Path Data=\"M 0,0 L 816,0 816,1056 0,1056 Z\"><Path.Fill>\
         <ImageBrush ImageSource=\"/Documents/1/Resources/Images/1.png\" Viewbox=\"0,0,1632,2112\" \
         TileMode=\"None\" ViewboxUnits=\"Absolute\" ViewportUnits=\"Absolute\" Viewport=\"0,0,816,1056\"/>\
         </Path.Fill></Path>",
    )
}

/// A two-page document: a page of text, set in a canvas moved down an inch,
/// then a page that is only a picture.
pub(crate) fn report() -> Vec<u8> {
    let text = fixed_page(&format!(
        "<Canvas RenderTransform=\"1,0,0,1,0,96\">{}{}{}</Canvas>",
        glyphs(96.0, 0.0, 32.0, "Quarterly report"),
        glyphs(96.0, 60.0, 16.0, "Alex Sample signed the orders on Monday."),
        glyphs(96.0, 80.0, 16.0, "Sam Example shipped them the same week."),
    ));
    package(&[
        (
            "FixedDocSeq.fdseq",
            "<FixedDocumentSequence xmlns=\"http://schemas.microsoft.com/xps/2005/06\">\
             <DocumentReference Source=\"Documents/1/FixedDoc.fdoc\"/></FixedDocumentSequence>"
                .into(),
        ),
        (
            "Documents/1/FixedDoc.fdoc",
            "<FixedDocument xmlns=\"http://schemas.microsoft.com/xps/2005/06\">\
             <PageContent Source=\"Pages/1.fpage\"/><PageContent Source=\"Pages/2.fpage\"/></FixedDocument>"
                .into(),
        ),
        ("Documents/1/Pages/1.fpage", text),
        ("Documents/1/Pages/2.fpage", screenshot()),
        ("Documents/1/Resources/Images/1.png", "\u{89}PNG".into()),
    ])
}

#[test]
fn a_page_of_glyphs_is_a_page_of_positioned_text() {
    let xps = parse(&report()).unwrap();
    assert_eq!(xps.doc.pages.len(), 2);
    let p = &xps.doc.pages[0];
    assert_eq!((p.width, p.height), (612.0, 792.0));
    let q = &p.glyphs[0];
    assert_eq!(q.text, "Q");
    // (96, 0) in a canvas moved 96 down, in points: (72, 72).
    assert_eq!(q.origin, (72.0, 72.0));
    assert_eq!(q.font_size, 24.0);
    // 0.55 em of a 32-unit em, in points.
    assert!((q.advance.unwrap() - 0.55 * 32.0 * 0.75).abs() < 1e-9);
    let b = q.bbox.unwrap();
    assert!((b.y0 - (72.0 - 0.8 * 24.0)).abs() < 1e-9 && (b.y1 - (72.0 + 0.2 * 24.0)).abs() < 1e-9);
    // A space has no outline.
    assert!(p.glyphs.iter().any(|g| g.text == " " && g.bbox.is_none()));
    assert!(p.images.is_empty());
    assert!(is_xps(&report()));
}

#[test]
fn the_layout_pipeline_reads_it_as_it_reads_a_pdf_page() {
    let mut xps = parse(&report()).unwrap();
    let a = fluree_doc_pdf::document::analyze_with(&mut xps.doc, &[], &Default::default());
    let texts: Vec<(&str, &str)> = a
        .elements
        .iter()
        .map(|e| (e.kind.as_str(), e.text.as_str()))
        .collect();
    assert!(
        texts.contains(&("doco:Title", "Quarterly report")),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(
            |(_, t)| t.contains("Alex Sample signed the orders on Monday.")
                && t.contains("Sam Example shipped")
        ),
        "{texts:?}"
    );
    assert!(a.elements.iter().all(|e| e.page == 0 && e.bbox.is_some()));
}

#[test]
fn a_page_that_is_only_a_picture_routes_as_a_scan() {
    let mut xps = parse(&report()).unwrap();
    let img = xps.page_image(1).unwrap();
    assert_eq!(img.part, "Documents/1/Resources/Images/1.png");
    assert_eq!((img.bbox.x1, img.bbox.y1), (612.0, 792.0));
    assert_eq!(xps.doc.pages[1].images.len(), 1);
    assert_eq!(
        read_image(&report(), &img.part).unwrap(),
        "\u{89}PNG".as_bytes()
    );
    let a = fluree_doc_pdf::document::analyze_with(&mut xps.doc, &[], &Default::default());
    let unread = fluree_doc_pdf::unread_pages(&xps.doc, &a.elements);
    assert_eq!(unread.len(), 1, "{unread:?}");
    assert_eq!((unread[0].index, unread[0].reason.as_str()), (1, "Scanned"));
}

#[test]
fn transforms_compose_and_property_elements_set_their_owners() {
    // A glyph run turned a quarter by its own property element, inside a
    // canvas scaled by two.
    let page = fixed_page(
        "<Canvas RenderTransform=\"2,0,0,2,0,0\"><Glyphs OriginX=\"10\" OriginY=\"20\" \
         FontRenderingEmSize=\"10\" UnicodeString=\"Ab\" Indices=\",50;,50\">\
         <Glyphs.RenderTransform><MatrixTransform Matrix=\"0,-1,1,0,0,100\"/></Glyphs.RenderTransform>\
         </Glyphs></Canvas>",
    );
    let (p, _) = super::page(&page, "Documents/1/Pages/1.fpage", 0, MAX_GLYPHS).unwrap();
    assert_eq!(p.glyphs.len(), 2);
    let a = &p.glyphs[0];
    // (10, 20) turned: (20, 90), scaled by two, in points: (30, 135).
    assert_eq!(a.origin, (30.0, 135.0));
    assert_eq!(a.rotation_deg, 90.0);
    assert_eq!(a.font_size, 15.0);
    // The next glyph runs up the page.
    assert!(p.glyphs[1].origin.1 < a.origin.1);
}

#[test]
fn an_engines_words_land_where_the_picture_is_drawn() {
    use fluree_doc_pdf::ocr::{Coords, WordReading};
    let reading = WordReading::from_json(
        r#"{"coords": "px", "width": 200, "height": 100,
            "words": [{"text": "Invoice", "box": [20, 10, 60, 30]}]}"#,
    )
    .unwrap();
    let image = PageImage {
        page: 1,
        part: "p.png".into(),
        bbox: BBox {
            x0: 100.0,
            y0: 50.0,
            x1: 500.0,
            y1: 250.0,
        },
    };
    let placed = reading_on_page(reading, &image.bbox, 200, 100);
    assert_eq!(placed.coords, Coords::Page);
    assert_eq!(placed.words[0].bbox, [140.0, 70.0, 220.0, 110.0]);
    assert_eq!((placed.width, placed.height), (None, None));
}

#[test]
fn advances_follow_indices_and_clusters() {
    assert_eq!(
        advances(Some(",50;;(2:1)12,80;3"), 5),
        [Some(0.5), None, Some(0.4), Some(0.4), None]
    );
    assert_eq!(advances(None, 2), [None, None]);
    assert_eq!(advances(Some(",50"), 3), [Some(0.5), None, None]);
}

#[test]
fn image_sources_and_open_xps() {
    assert_eq!(
        image_source("{ColorConvertedBitmap /Resources/a.tif /Resources/p.icc}").as_deref(),
        Some("/Resources/a.tif")
    );
    assert_eq!(image_source("../img.png").as_deref(), Some("../img.png"));
    // OpenXPS names the sequence in its own namespace, ending alike.
    let oxps = package_as(
        "http://schemas.openxps.org/oxps/v1.0/fixedrepresentation",
        &[],
    );
    assert!(is_xps(&oxps));
    assert!(!is_xps(&package_as("http://example.org/other", &[])));
    assert!(!is_xps(b"PK\x03\x04"));
    assert!(matches!(parse(b"%PDF-1.4"), Err(XpsError::Zip(_))));
}

#[test]
fn a_part_past_its_bound_is_refused_in_bounded_memory() {
    // A page that inflates to more than a part may hold.
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let rels = format!(
        "<Relationships><Relationship Id=\"R0\" Type=\"{FDSEQ}\" Target=\"/s.fdseq\"/></Relationships>"
    );
    for (name, body) in [
        ("_rels/.rels", rels),
        (
            "s.fdseq",
            "<FixedDocumentSequence><DocumentReference Source=\"d.fdoc\"/></FixedDocumentSequence>"
                .into(),
        ),
        (
            "d.fdoc",
            "<FixedDocument><PageContent Source=\"p.fpage\"/></FixedDocument>".into(),
        ),
    ] {
        z.start_file(name, opts).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    }
    z.start_file("p.fpage", opts).unwrap();
    let chunk = vec![b' '; 1 << 20];
    for _ in 0..(MAX_PART_BYTES >> 20) + 1 {
        z.write_all(&chunk).unwrap();
    }
    z.finish().unwrap();
    assert!(matches!(
        parse(&buf.into_inner()),
        Err(XpsError::TooLarge(_))
    ));
}

#[test]
fn a_cluster_counts_no_more_than_the_characters_and_entries_left() {
    // A cluster claiming a million million characters for one, and one
    // claiming more glyph entries than there are: one advance each, made
    // and walked at once.
    assert_eq!(
        advances(Some("(1000000000000:1)0,50"), 1),
        [Some(0.5 / 1e12)]
    );
    assert_eq!(advances(Some("(1:1000000000000000000)0,50"), 1), [None]);
    assert_eq!(
        advances(Some("(4:2)0,50;0,30;0,99"), 2),
        [Some(0.2), Some(0.2)]
    );
}

#[test]
fn a_run_past_the_glyphs_left_is_refused_before_it_is_laid_out() {
    let page = fixed_page(&glyphs(10.0, 20.0, 12.0, "Eleven char"));
    let read = |room| super::page(&page, "p.fpage", 0, room);
    assert_eq!(read(11).unwrap().0.glyphs.len(), 11);
    assert!(matches!(read(10), Err(XpsError::TooLarge(_))));
    // A picture counts as one.
    let pic = screenshot();
    assert!(matches!(
        super::page(&pic, "p.fpage", 0, 0),
        Err(XpsError::TooLarge(_))
    ));
    assert_eq!(super::page(&pic, "p.fpage", 0, 1).unwrap().1.len(), 1);
}

#[test]
fn a_page_past_its_own_glyphs_is_refused_whatever_the_document_has_left() {
    // One run, a millionth of a point high, of one glyph more than a page
    // holds: the pile the layout's passes would compare glyph by glyph.
    let run = format!(
        "<Glyphs OriginX=\"100\" OriginY=\"100\" FontRenderingEmSize=\"0.000001\" UnicodeString=\"{}\"/>",
        "AB".repeat(MAX_PAGE_GLYPHS / 2 + 1)
    );
    let xps = package(&[
        (
            "FixedDocSeq.fdseq",
            "<FixedDocumentSequence><DocumentReference Source=\"/Doc.fdoc\"/></FixedDocumentSequence>"
                .into(),
        ),
        (
            "Doc.fdoc",
            "<FixedDocument><PageContent Source=\"/p.fpage\"/></FixedDocument>".into(),
        ),
        ("p.fpage", fixed_page(&run)),
    ]);
    let err = parse(&xps).map(|_| ()).unwrap_err();
    assert!(matches!(err, XpsError::TooLarge(_)), "{err}");
}

#[test]
fn text_no_page_could_show_is_left_out() {
    let page = fixed_page(&format!(
        "{}<Glyphs OriginX=\"10\" OriginY=\"80\" FontRenderingEmSize=\"1e200\" UnicodeString=\"huge\"/>\
         <Glyphs OriginX=\"1e200\" OriginY=\"80\" FontRenderingEmSize=\"12\" UnicodeString=\"far\"/>\
         <Canvas RenderTransform=\"1e300,0,0,1e300,0,0\"><Glyphs OriginX=\"10\" OriginY=\"20\" \
         FontRenderingEmSize=\"12\" UnicodeString=\"vast\"/></Canvas>\
         <Glyphs OriginX=\"10\" OriginY=\"100\" FontRenderingEmSize=\"12\" UnicodeString=\"ab\" Indices=\",1e300;,50\"/>",
        glyphs(10.0, 60.0, 12.0, "kept")
    ));
    let (p, _) = super::page(&page, "p.fpage", 0, MAX_GLYPHS).unwrap();
    let text: String = p.glyphs.iter().map(|g| g.text.as_str()).collect();
    assert_eq!(text, "keptab");
    // An advance is at most ten ems, so no box reaches far past the page.
    let a = p.glyphs.iter().find(|g| g.text == "a").unwrap();
    assert!(a.bbox.unwrap().width() <= MAX_ADVANCE_EMS * 12.0 * UNIT + 1e-9);
    // A page side is at most a PDF's.
    let wide = "<FixedPage Width=\"1e300\" Height=\"1e300\"></FixedPage>";
    let (p, _) = super::page(wide, "p.fpage", 0, MAX_GLYPHS).unwrap();
    assert_eq!((p.width, p.height), (MAX_PAGE_SIZE, MAX_PAGE_SIZE));
}

#[test]
fn a_page_of_huge_text_goes_through_the_layout_pipeline() {
    // Enough lines for the column pass to build its grid, and one run whose
    // size no page has, as a hostile file sets it.
    let mut body: String = (0..30)
        .map(|i| glyphs(10.0, 20.0 + 14.0 * i as f64, 12.0, "normal text here"))
        .collect();
    body.push_str(
        "<Glyphs OriginX=\"10\" OriginY=\"80\" FontRenderingEmSize=\"1e200\" UnicodeString=\"huge em\"/>",
    );
    let doc = package(&[
        (
            "FixedDocSeq.fdseq",
            "<FixedDocumentSequence><DocumentReference Source=\"d.fdoc\"/></FixedDocumentSequence>"
                .into(),
        ),
        (
            "d.fdoc",
            "<FixedDocument><PageContent Source=\"p.fpage\"/></FixedDocument>".into(),
        ),
        ("p.fpage", fixed_page(&body)),
    ]);
    let mut xps = parse(&doc).unwrap();
    let a = fluree_doc_pdf::document::analyze_with(&mut xps.doc, &[], &Default::default());
    assert!(a
        .elements
        .iter()
        .any(|e| e.text.contains("normal text here")));
}

#[test]
fn pages_past_the_page_limit_are_counted_and_listed_not_read() {
    use fluree_doc_model::budget::{limited_to, Limits};
    let whole = parse(&report()).unwrap();
    assert_eq!((whole.doc.pages.len(), whole.doc.pages_past_limit), (2, 0));
    let mut one = Limits::default();
    one.pages = 1;
    let cut = limited_to(one, || parse(&report())).unwrap();
    assert_eq!((cut.doc.pages.len(), cut.doc.pages_past_limit), (1, 1));
    // The picture on the page not read is not offered to a reader.
    assert!(cut.images.is_empty(), "{:?}", cut.images);
    let unread: Vec<(usize, String)> = fluree_doc_pdf::unread_pages(&cut.doc, &[])
        .into_iter()
        .map(|u| (u.index, u.reason))
        .collect();
    assert_eq!(unread, [(1, "PastPageLimit".to_string())]);
}

#[test]
fn a_document_listing_too_many_pages_is_refused() {
    // One fixed document listing `n` pages, named by the sequence `refs`
    // times: its pages counted each time.
    let listing = |n: usize, refs: usize| {
        package(&[
            (
                "FixedDocSeq.fdseq",
                format!(
                    "<FixedDocumentSequence>{}</FixedDocumentSequence>",
                    "<DocumentReference Source=\"d.fdoc\"/>".repeat(refs)
                ),
            ),
            (
                "d.fdoc",
                format!(
                    "<FixedDocument>{}</FixedDocument>",
                    "<PageContent Source=\"p.fpage\"/>".repeat(n)
                ),
            ),
            ("p.fpage", fixed_page(&glyphs(96.0, 96.0, 16.0, "Hi"))),
        ])
    };
    let at = parse(&listing(MAX_LISTED_PAGES / 2, 2)).unwrap();
    assert_eq!(at.doc.pages.len(), MAX_PAGES);
    assert_eq!(at.doc.pages_past_limit, MAX_LISTED_PAGES - MAX_PAGES);
    match parse(&listing(MAX_LISTED_PAGES / 2 + 1, 2)) {
        Err(XpsError::TooLarge(e)) => {
            assert_eq!(e.limit(), fluree_doc_model::budget::Limit::Pages);
            assert!(
                e.to_string().contains("lists more than 250000 pages"),
                "{e}"
            );
        }
        Err(e) => panic!("{e}"),
        Ok(x) => panic!("{} pages past the limit", x.doc.pages_past_limit),
    }
    // A page limit past it raises it: the caller asks for that many read.
    let mut limits = fluree_doc_model::budget::Limits::default();
    limits.pages = MAX_LISTED_PAGES + 2;
    let read = fluree_doc_model::budget::limited_to(limits, || {
        parse(&listing(MAX_LISTED_PAGES / 2 + 1, 2)).map(|x| x.doc.pages_past_limit)
    });
    assert_eq!(read.unwrap(), 0);
}

/// A document of `pages`, each part named `Pages/<n>.fpage`, those not in
/// `held` left out of the package.
fn pages_of(pages: &[String], held: &[usize]) -> Vec<u8> {
    let listed: String = (0..pages.len())
        .map(|n| format!("<PageContent Source=\"Pages/{n}.fpage\"/>"))
        .collect();
    let mut parts = vec![
        (
            "FixedDocSeq.fdseq".to_string(),
            "<FixedDocumentSequence><DocumentReference Source=\"Documents/1/FixedDoc.fdoc\"/></FixedDocumentSequence>"
                .to_string(),
        ),
        (
            "Documents/1/FixedDoc.fdoc".to_string(),
            format!("<FixedDocument>{listed}</FixedDocument>"),
        ),
    ];
    for &n in held {
        parts.push((format!("Documents/1/Pages/{n}.fpage"), pages[n].clone()));
    }
    let parts: Vec<(&str, String)> = parts.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
    package(&parts)
}

#[test]
fn a_missing_page_is_an_empty_page_that_says_so() {
    let page = |text: &str| fixed_page(&glyphs(96.0, 96.0, 16.0, text));
    let pages = [page("One"), page("Two"), page("Three")];
    let xps = parse(&pages_of(&pages, &[0, 2])).unwrap();
    assert_eq!(xps.doc.pages.len(), 3);
    assert!(xps.doc.pages[1].glyphs.is_empty());
    let warned: Vec<(usize, String)> = fluree_doc_pdf::parse_warnings(&xps.doc)
        .into_iter()
        .map(|w| (w.page, w.kind))
        .collect();
    assert_eq!(warned, [(1, MISSING_PAGE.to_string())]);
    // A page that is there, and empty, says nothing.
    let xps = parse(&pages_of(&[fixed_page(""), page("Two")], &[0, 1])).unwrap();
    assert!(fluree_doc_pdf::parse_warnings(&xps.doc).is_empty());
}

#[test]
fn pictures_count_against_the_documents_bound_as_glyphs_do() {
    // Three pages of a picture and two glyphs: three things each. Within
    // eight in all, the third page is refused: its picture counts, as it
    // does on its page.
    let page = fixed_page(&format!(
        "{}<Path Data=\"M 0,0 L 100,0 100,100 0,100 Z\"><Path.Fill>\
         <ImageBrush ImageSource=\"/i.png\" Viewport=\"0,0,100,100\"/></Path.Fill></Path>",
        glyphs(200.0, 200.0, 16.0, "ab")
    ));
    let bytes = pages_of(&[page.clone(), page.clone(), page], &[0, 1, 2]);
    assert!(parse_within(&bytes, 9).is_ok());
    assert!(matches!(
        parse_within(&bytes, 8),
        Err(XpsError::TooLarge(_))
    ));
}
