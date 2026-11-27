//! Synthetic Word 97 documents, written structure by structure, and what
//! the reader makes of them.

use super::*;
use fluree_doc_model::budget::{CELL_BYTES, ELEMENT_BYTES, TEXT_BYTES};
use std::io::Write;

/// How a paragraph's mark is set: plain, a cell's, or a row's end.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Mark {
    Plain,
    Cell,
    Row,
}

/// The stories of a document, each a list of paragraphs: their text (with
/// Word's control characters) and how each ends.
pub(crate) struct Stories<'a> {
    pub main: Vec<(&'a str, Mark)>,
    pub footnotes: Vec<&'a str>,
    pub comments: Vec<&'a str>,
    pub endnotes: Vec<&'a str>,
    /// The headers' and footers' story, the body's text boxes' and the
    /// headers' text boxes', read where the table stream lists their
    /// stories ([`with_tables`]).
    pub headers: Vec<&'a str>,
    pub boxes: Vec<&'a str>,
    pub header_boxes: Vec<&'a str>,
}

/// A `.doc`: the main text in a one-byte piece, the notes in a two-byte
/// piece stored before it in the stream (as a fast save leaves text out of
/// order), paragraph properties in one FKP, `nfib` and `flags` in the FIB.
pub(crate) fn doc_with(s: &Stories<'_>, nfib: u16, flags: u16) -> Vec<u8> {
    let story = |paras: &[&str]| -> String { paras.iter().map(|p| format!("{p}\r")).collect() };
    let mut main = String::new();
    let mut marks: Vec<(usize, Mark)> = Vec::new();
    for (text, mark) in &s.main {
        main.push_str(text);
        marks.push((main.chars().count(), *mark));
        main.push(if *mark == Mark::Plain { '\r' } else { '\u{7}' });
    }
    let (ftn, atn, edn) = (story(&s.footnotes), story(&s.comments), story(&s.endnotes));
    let (hdd, txbx) = (story(&s.headers), story(&s.boxes));
    let hdr_txbx = story(&s.header_boxes);
    let notes = format!("{ftn}{hdd}{atn}{edn}{txbx}{hdr_txbx}");
    // Layout: FIB (to 0x382 where it holds every pair a Word 97 file
    // does), then the notes (UTF-16) at 0x400, then the main text
    // (Windows-1252) after them, then the FKP on its own 512-byte page.
    let mut wd = vec![0u8; 0x400];
    let notes_fc = wd.len() as u32;
    for u in notes.encode_utf16() {
        wd.extend(u.to_le_bytes());
    }
    let main_fc = wd.len() as u32;
    let (cp1252, _, _) = encoding_rs::WINDOWS_1252.encode(&main);
    wd.extend(cp1252.iter());
    // The FKP: one run per main-text paragraph that is in a table.
    while !wd.len().is_multiple_of(512) {
        wd.push(0);
    }
    let fkp_page = (wd.len() / 512) as u32;
    let mut page = vec![0u8; 512];
    let mut runs: Vec<(u32, u32, Mark)> = Vec::new();
    let mut start = 0usize;
    for (end, mark) in &marks {
        runs.push((main_fc + start as u32, main_fc + *end as u32 + 1, *mark));
        start = end + 1;
    }
    let crun = runs.len();
    for (i, (f0, _, _)) in runs.iter().enumerate() {
        page[4 * i..4 * i + 4].copy_from_slice(&f0.to_le_bytes());
    }
    let last = runs.last().map_or(main_fc, |r| r.1);
    page[4 * crun..4 * crun + 4].copy_from_slice(&last.to_le_bytes());
    // Two PAPXs: in a table, and a row's end.
    let papx = |at: usize, sprms: &[(u16, u8)], page: &mut Vec<u8>| {
        page[at] = 5; // 2 * 5 - 1 = 9 bytes: style index and sprms.
        let mut d = vec![0u8, 0];
        for (op, v) in sprms {
            d.extend(op.to_le_bytes());
            d.push(*v);
        }
        page[at + 1..at + 1 + d.len()].copy_from_slice(&d);
    };
    papx(400, &[(0x2416, 1)], &mut page);
    papx(430, &[(0x2416, 1), (0x2417, 1)], &mut page);
    for (i, (_, _, mark)) in runs.iter().enumerate() {
        page[4 * (crun + 1) + 13 * i] = match mark {
            Mark::Plain => 0,
            Mark::Cell => 200,
            Mark::Row => 215,
        };
    }
    page[511] = crun as u8;
    wd.extend(page);
    // The table stream: the piece table, then the FKP's bin table.
    let main_len = main.chars().count() as u32;
    let mut table = Vec::new();
    let clx_fc = table.len() as u32;
    let mut plc = Vec::new();
    for cp in [0, main_len, main_len + notes.encode_utf16().count() as u32] {
        plc.extend(cp.to_le_bytes());
    }
    for (fc, compressed) in [(main_fc, true), (notes_fc, false)] {
        plc.extend(0u16.to_le_bytes());
        let raw = if compressed {
            (fc * 2) | 0x4000_0000
        } else {
            fc
        };
        plc.extend(raw.to_le_bytes());
        plc.extend(0u16.to_le_bytes());
    }
    table.push(0x02);
    table.extend((plc.len() as u32).to_le_bytes());
    table.extend(&plc);
    let clx_len = table.len() as u32 - clx_fc;
    let bte_fc = table.len() as u32;
    for v in [main_fc, last, fkp_page] {
        table.extend(v.to_le_bytes());
    }
    let bte_len = table.len() as u32 - bte_fc;
    // The FIB.
    let put16 =
        |wd: &mut Vec<u8>, at: usize, v: u16| wd[at..at + 2].copy_from_slice(&v.to_le_bytes());
    let put32 =
        |wd: &mut Vec<u8>, at: usize, v: u32| wd[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put16(&mut wd, 0, 0xA5EC);
    put16(&mut wd, 2, nfib);
    put16(&mut wd, 0x0A, flags);
    let count = |s: &str| s.encode_utf16().count() as u32;
    for (i, v) in [
        main_len,
        count(&ftn),
        count(&hdd),
        0,
        count(&atn),
        count(&edn),
        count(&txbx),
        count(&hdr_txbx),
    ]
    .into_iter()
    .enumerate()
    {
        put32(&mut wd, 0x40 + 4 * (3 + i), v);
    }
    put32(&mut wd, 0x9A + 8 * 33, clx_fc);
    put32(&mut wd, 0x9A + 8 * 33 + 4, clx_len);
    put32(&mut wd, 0x9A + 8 * 13, bte_fc);
    put32(&mut wd, 0x9A + 8 * 13 + 4, bte_len);
    let table_name = if flags & 0x0200 != 0 {
        "/1Table"
    } else {
        "/0Table"
    };
    compound(&[("/WordDocument", &wd), (table_name, &table)])
}

pub(crate) fn compound(streams: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cf =
        cfb::CompoundFile::create_with_version(cfb::Version::V3, Cursor::new(Vec::new())).unwrap();
    for (path, data) in streams {
        cf.create_stream(path).unwrap().write_all(data).unwrap();
    }
    cf.flush().unwrap();
    cf.into_inner().into_inner()
}

pub(crate) fn memo() -> Vec<u8> {
    doc_with(
        &Stories {
            main: vec![
                ("Quarterly report", Mark::Plain),
                (
                    "Ren\u{e9} signed\u{2} on \u{13} PAGE \u{14}3\u{15} Monday.\u{b}Second line",
                    Mark::Plain,
                ),
                ("Name", Mark::Cell),
                ("Amount", Mark::Cell),
                ("", Mark::Row),
                ("Alex Sample", Mark::Cell),
                ("12.50", Mark::Cell),
                ("", Mark::Row),
                ("After the table.", Mark::Plain),
            ],
            footnotes: vec!["\u{2} Survey of Alex Sample."],
            comments: vec!["\u{5}Source?"],
            endnotes: vec!["\u{2} Method in the annex."],
            headers: Vec::new(),
            boxes: Vec::new(),
            header_boxes: vec![],
        },
        0x00C1,
        0x0200,
    )
}

#[test]
fn the_text_reads_through_the_piece_table() {
    let els = parse(&memo()).unwrap();
    let got: Vec<(&str, &str, &str)> = els
        .iter()
        .map(|e| (e.kind.as_str(), e.text.as_str(), e.evidence))
        .collect();
    assert_eq!(
        got,
        [
            ("doco:Paragraph", "Quarterly report", "doc"),
            // The field shows its result; the note reference adds nothing;
            // a line break keeps its line; `é` is one byte, Windows-1252.
            (
                "doco:Paragraph",
                "René signed on 3 Monday.\nSecond line",
                "doc"
            ),
            ("doco:Table", "Name | Amount\nAlex Sample | 12.50", "doc"),
            ("doco:Paragraph", "After the table.", "doc"),
            ("doco:Paragraph", "1 Survey of Alex Sample.", "footnote"),
            ("doco:Paragraph", "i Method in the annex.", "endnote"),
            ("doco:Paragraph", "Source?", "comment"),
        ]
    );
    let t = &els[2];
    assert_eq!(
        t.cells.as_ref().unwrap(),
        &[vec!["Name", "Amount"], vec!["Alex Sample", "12.50"]]
    );
    assert_eq!(t.header_rows, None);
    assert!(els
        .iter()
        .all(|e| e.provenance == "doc" && e.bbox.is_none()));
    assert_eq!(els[6].id, "elem-00007");
}

#[test]
fn the_table_stream_is_the_one_the_fib_names() {
    let zero = doc_with(
        &Stories {
            main: vec![("Plain text in 0Table", Mark::Plain)],
            footnotes: vec![],
            comments: vec![],
            endnotes: vec![],
            headers: vec![],
            boxes: vec![],
            header_boxes: vec![],
        },
        0x00C1,
        0,
    );
    assert_eq!(parse(&zero).unwrap()[0].text, "Plain text in 0Table");
}

#[test]
fn old_encrypted_and_foreign_files_are_refused() {
    let s = Stories {
        main: vec![("x", Mark::Plain)],
        footnotes: vec![],
        comments: vec![],
        endnotes: vec![],
        headers: vec![],
        boxes: vec![],
        header_boxes: vec![],
    };
    assert!(matches!(
        parse(&doc_with(&s, 0x0065, 0x0200)),
        Err(DocError::Unsupported(_))
    ));
    assert!(matches!(
        parse(&doc_with(&s, 0x00C1, 0x0300)),
        Err(DocError::Encrypted)
    ));
    let workbook = compound(&[("/Workbook", b"not a document")]);
    assert!(!is_doc(&workbook));
    assert!(matches!(parse(&workbook), Err(DocError::NotWord)));
    assert!(is_doc(&memo()));
    assert!(matches!(parse(b"PK\x03\x04"), Err(DocError::Container(_))));
}

#[test]
fn a_piece_table_pointing_outside_its_stream_is_malformed() {
    let mut bytes = memo();
    // Find the FIB in the compound file and push `fcClx` far away.
    let at = bytes
        .windows(4)
        .position(|w| w == [0xEC, 0xA5, 0xC1, 0x00])
        .unwrap();
    let clx = at + 0x9A + 8 * 33;
    bytes[clx..clx + 4].copy_from_slice(&0x7FFF_0000u32.to_le_bytes());
    assert!(matches!(parse(&bytes), Err(DocError::Malformed(_))));
}

#[test]
fn papx_reads_table_marks_past_other_sprms() {
    // A two-byte sprm, a variable one, then in-table and row end.
    let mut d = vec![0u8; 0];
    d.extend([0u8, 0]); // style index
    d.extend(0x4600u16.to_le_bytes());
    d.extend([1, 0]);
    d.extend(0xC615u16.to_le_bytes());
    d.extend([2, 9, 9]);
    d.extend(0x2416u16.to_le_bytes());
    d.push(1);
    d.extend(0x2417u16.to_le_bytes());
    d.push(1);
    let mut px = vec![0u8, (d.len() / 2) as u8 + 1];
    px.extend(&d);
    px.resize(px.len() + 4, 0);
    // cb = 0: the length is in the next byte, in words.
    px[1] = d.len().div_ceil(2) as u8;
    assert_eq!(
        papx(&px),
        ParaProps {
            in_table: true,
            row_end: true
        }
    );
}

/// A `.doc` of one-byte `text` at 0x400 in the stream, `ccp` characters of
/// main text, the piece table `plc` (positions, then descriptors), the bin
/// table `bte`, and `pages` after the text, on 512-byte pages; in `bte`,
/// `u32::MAX` stands for the first of those pages.
fn raw_doc(text: &[u8], ccp: u32, plc: &[u8], bte: &[u32], pages: &[u8]) -> Vec<u8> {
    let mut wd = vec![0u8; 0x400];
    wd[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
    wd[2..4].copy_from_slice(&0x00C1u16.to_le_bytes());
    wd[0x0A..0x0C].copy_from_slice(&0x0200u16.to_le_bytes());
    wd[0x4C..0x50].copy_from_slice(&ccp.to_le_bytes());
    wd.extend(text);
    wd.resize(wd.len().div_ceil(512) * 512, 0);
    let first_page = (wd.len() / 512) as u32;
    wd.extend(pages);
    let mut table = vec![0x02u8];
    table.extend((plc.len() as u32).to_le_bytes());
    table.extend(plc);
    let clx_len = table.len() as u32;
    let bte_fc = table.len() as u32;
    for &v in bte {
        table.extend(if v == u32::MAX { first_page } else { v }.to_le_bytes());
    }
    let put =
        |wd: &mut Vec<u8>, at: usize, v: u32| wd[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut wd, 0x9A + 8 * 33, 0);
    put(&mut wd, 0x9A + 8 * 33 + 4, clx_len);
    put(&mut wd, 0x9A + 8 * 13, bte_fc);
    put(&mut wd, 0x9A + 8 * 13 + 4, table.len() as u32 - bte_fc);
    compound(&[("/WordDocument", &wd), ("/1Table", &table)])
}

/// A piece table: `cps`, then one one-byte descriptor per piece, each
/// starting at 0x400.
fn plc(cps: &[u32]) -> Vec<u8> {
    let mut v: Vec<u8> = cps.iter().flat_map(|c| c.to_le_bytes()).collect();
    for _ in 1..cps.len() {
        v.extend(0u16.to_le_bytes());
        v.extend((0x800u32 | 0x4000_0000).to_le_bytes());
        v.extend(0u16.to_le_bytes());
    }
    v
}

#[test]
fn pieces_that_overlap_are_refused_not_read_again() {
    // Positions 0, 6, 0, 6: the second piece would read the first's
    // characters again, and fifty thousand of them three billion.
    let text = b"Hello\r";
    let fine = raw_doc(text, 6, &plc(&[0, 6]), &[], &[]);
    assert_eq!(parse(&fine).unwrap()[0].text, "Hello");
    let again = raw_doc(text, 6, &plc(&[0, 6, 0, 6]), &[], &[]);
    assert!(matches!(parse(&again), Err(DocError::Malformed(_))));
}

#[test]
fn an_fkp_page_named_many_times_is_read_once() {
    let mut page = vec![0u8; 512];
    for r in 0..=29u32 {
        page[4 * r as usize..4 * r as usize + 4].copy_from_slice(&r.to_le_bytes());
    }
    page[511] = 29;
    // A bin table of 1,000 entries, every one naming that page.
    let n = 1_000u32;
    let mut bte: Vec<u32> = (0..=n).collect();
    bte.extend(std::iter::repeat_n(u32::MAX, n as usize));
    let bytes = raw_doc(b"a\r", 2, &plc(&[0, 2]), &bte, &page);
    let mut cf = cfb::CompoundFile::open(Cursor::new(&bytes[..])).unwrap();
    let wd = stream(&mut cf, "/WordDocument", MAX_STREAM_BYTES).unwrap();
    let table = stream(&mut cf, "/1Table", MAX_TABLE_BYTES).unwrap();
    let props = para_props(&wd, &table, fib(&wd).unwrap().bte_papx).unwrap();
    assert_eq!(props.len(), 29);
}

/// A `.doc` whose `n` pieces all read the same `text`, one after another in
/// character positions: what fast save writes, and what a file may repeat.
fn repeated(text: &[u8], n: u32) -> Vec<u8> {
    let len = text.len() as u32;
    let cps: Vec<u32> = (0..=n).map(|i| i * len).collect();
    raw_doc(text, n * len, &plc(&cps), &[], &[])
}

#[test]
fn paragraphs_past_the_budget_are_refused_before_they_are_made() {
    // Pieces that all read the same two characters, a paragraph each.
    let bytes = repeated(b"a\r", 1_000);
    let budget = |n: u64| Budget::new(n * ELEMENT_BYTES + 1_000 * TEXT_BYTES, 0);
    assert_eq!(
        parse_within(&bytes, &mut budget(1_000)).unwrap().len(),
        1_000
    );
    assert!(matches!(
        parse_within(&bytes, &mut budget(999)),
        Err(DocError::TooLarge(ref m)) if m.contains("elements")
    ));
    // Empty paragraphs cost no element, and are not held.
    assert!(parse_within(&repeated(b"\r", 100_000), &mut budget(0)).is_ok());
}

#[test]
fn a_tables_padding_is_spent_before_it_is_made() {
    // A row of 6 cells over 7 rows of one: 48 cells once padded, of which
    // 13 read. A row of thousands over rows of one asks for millions.
    let mut main = vec![("x", Mark::Cell); 6];
    main.push(("", Mark::Row));
    for _ in 0..7 {
        main.extend([("x", Mark::Cell), ("", Mark::Row)]);
    }
    let bytes = doc_with(
        &Stories {
            main,
            footnotes: vec![],
            comments: vec![],
            endnotes: vec![],
            headers: vec![],
            boxes: vec![],
            header_boxes: vec![],
        },
        0x00C1,
        0x0200,
    );
    let table = parse(&bytes).unwrap().remove(0);
    assert_eq!(table.cells.as_ref().unwrap().len(), 8);
    // The table and its 13 cells that hold text, each a node of the output.
    let budget =
        |cells: u64| Budget::new(cells * CELL_BYTES + 14 * ELEMENT_BYTES + 13 * TEXT_BYTES, 0);
    assert!(parse_within(&bytes, &mut budget(48)).is_ok());
    assert!(matches!(
        parse_within(&bytes, &mut budget(47)),
        Err(DocError::TooLarge(_))
    ));
}

#[test]
fn a_paragraph_finds_its_properties_by_search() {
    // 200,000 runs and as many marks: a scan of the runs for each mark is
    // twenty billion steps.
    let n = 200_000u32;
    let runs: Vec<(u32, u32, ParaProps)> = (0..n)
        .map(|i| {
            let p = ParaProps {
                in_table: i % 2 == 1,
                row_end: false,
            };
            (2 * i, 2 * i + 2, p)
        })
        .collect();
    let chars: Vec<Char> = (0..n)
        .map(|i| Char {
            c: '\r',
            fc: 2 * i + 1,
            cp: 2 * i + 1,
        })
        .collect();
    let mut paras = Vec::new();
    paragraphs(chars, &runs, None, &mut Budget::default(), &mut |p, _| {
        paras.push(p);
        Ok(())
    })
    .unwrap();
    assert_eq!(paras.len(), n as usize);
    assert!(paras[1].props.in_table && !paras[2].props.in_table);
    // Outside every run: no properties.
    assert_eq!(props_at(&runs, 2 * n + 5), ParaProps::default());
    assert_eq!(props_at(&[(10, 20, runs[1].2)], 5), ParaProps::default());
}

/// The paragraphs `paragraphs` makes of `s`, as text.
fn para_texts(s: &str) -> Vec<String> {
    let mut got = Vec::new();
    paragraphs(
        s.chars().map(|c| Char { c, fc: 0, cp: 0 }),
        &[],
        None,
        &mut Budget::default(),
        &mut |p, _| {
            got.push(p.text);
            Ok(())
        },
    )
    .unwrap();
    got
}

#[test]
fn fields_show_their_results_however_they_nest_or_are_left_open() {
    // A field in a field's result shows its own result; one in a field's
    // code shows nothing; a separator past the result, or an end with no
    // field open, changes nothing.
    assert_eq!(
        para_texts("a\u{13}X\u{14}b\u{13}Y\u{14}c\u{15}d\u{15}e\r"),
        ["abcde"]
    );
    assert_eq!(
        para_texts("a\u{13}X\u{13}Y\u{14}y\u{15}Z\u{14}b\u{14}c\u{15}\u{15}d\r"),
        ["abcd"]
    );
    // Fields left open hide the rest of their paragraph only.
    assert_eq!(
        para_texts("a\u{13}\u{13}\u{13}hidden\rshown\r"),
        ["a", "shown"]
    );
    // A million fields opened, each showing its result, never closed,
    // then a million characters: read in one pass (looking at every open
    // field for each character, a million million steps), and all shown.
    let n = 1 << 20;
    let s = format!("{}{}\r", "\u{13}\u{14}".repeat(n), "x".repeat(n));
    assert_eq!(para_texts(&s)[0].len(), n);
}

/// The streams of a compound file, by name.
fn streams_of(bytes: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut cf = cfb::CompoundFile::open(Cursor::new(bytes)).unwrap();
    let names: Vec<String> = cf
        .walk()
        .filter(|e| e.is_stream())
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect();
    names
        .into_iter()
        .map(|n| {
            let mut v = Vec::new();
            cf.open_stream(&n).unwrap().read_to_end(&mut v).unwrap();
            (n, v)
        })
        .collect()
}

fn compound_of(streams: &std::collections::BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let s: Vec<(&str, &[u8])> = streams
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_slice()))
        .collect();
    compound(&s)
}

fn stories<'a>(main: &[&'a str]) -> Stories<'a> {
    Stories {
        main: main.iter().map(|p| (*p, Mark::Plain)).collect(),
        footnotes: vec![],
        comments: vec![],
        endnotes: vec![],
        headers: vec![],
        boxes: vec![],
        header_boxes: vec![],
    }
}

#[test]
fn deleted_and_hidden_text_is_not_the_documents() {
    // `Kept deleted shown hidden end.`: a tracked deletion and hidden text,
    // marked in a CHPX FKP over the main text's bytes.
    let doc = doc_with(
        &stories(&["Kept deleted shown hidden end."]),
        0x00C1,
        0x0200,
    );
    let mut streams = streams_of(&doc);
    let wd = streams.get_mut("/WordDocument").unwrap();
    let at = |w: &[u8]| wd.windows(w.len()).position(|x| x == w).unwrap() as u32;
    let (del, hid) = (at(b"deleted "), at(b"hidden "));
    while !wd.len().is_multiple_of(512) {
        wd.push(0);
    }
    let pn = (wd.len() / 512) as u32;
    let mut page = vec![0u8; 512];
    let fcs = [del, del + 8, hid, hid + 7];
    for (i, fc) in fcs.iter().enumerate() {
        page[4 * i..4 * i + 4].copy_from_slice(&fc.to_le_bytes());
    }
    // Three runs: deleted (`sprmCFRMarkDel`), plain, hidden
    // (`sprmCFVanish`), their CHPXs at word offsets 200 and 210.
    page[16..19].copy_from_slice(&[200, 0, 210]);
    page[400..404].copy_from_slice(&[3, 0x00, 0x08, 1]);
    page[420..424].copy_from_slice(&[3, 0x3C, 0x08, 1]);
    page[511] = 3;
    wd.extend(page);
    let table = streams.get("/1Table").unwrap().len() as u32;
    let wd = streams.get_mut("/WordDocument").unwrap();
    wd[0x9A + 8 * 12..0x9A + 8 * 12 + 4].copy_from_slice(&table.to_le_bytes());
    wd[0x9A + 8 * 12 + 4..0x9A + 8 * 12 + 8].copy_from_slice(&12u32.to_le_bytes());
    let t = streams.get_mut("/1Table").unwrap();
    for v in [fcs[0], fcs[3], pn] {
        t.extend(v.to_le_bytes());
    }
    let els = parse(&compound_of(&streams)).unwrap();
    assert_eq!(els[0].text, "Kept shown end.");
}

#[test]
fn an_emoji_is_one_character_not_two_unknown_ones() {
    // The notes are stored as UTF-16, where an emoji is a surrogate pair.
    let mut s = stories(&["Party"]);
    s.footnotes = vec!["\u{2} Cake \u{1F382} and \u{1F600}."];
    let els = parse(&doc_with(&s, 0x00C1, 0x0200)).unwrap();
    assert_eq!(els[1].text, "1 Cake \u{1F382} and \u{1F600}.");
}

#[test]
fn the_summary_gives_title_and_author_and_what_is_not_read_is_said() {
    let mut s = stories(&["Body"]);
    s.headers = vec!["\u{3}", "Ref: QA-7"];
    s.boxes = vec!["Boxed"];
    let doc = doc_with(&s, 0x00C1, 0x0200);
    // A property set: its header, one section, a code page (1252), the
    // title as bytes and the author as UTF-16.
    let title = b"Quarterly  report\0";
    let author: Vec<u8> = "Alex Sample\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let props: Vec<(u32, Vec<u8>)> = vec![
        (
            1,
            [2u32.to_le_bytes().to_vec(), 1252u32.to_le_bytes().to_vec()].concat(),
        ),
        (
            2,
            [
                0x1Eu32.to_le_bytes().to_vec(),
                (title.len() as u32).to_le_bytes().to_vec(),
                title.to_vec(),
            ]
            .concat(),
        ),
        (
            4,
            [
                0x1Fu32.to_le_bytes().to_vec(),
                (author.len() as u32 / 2).to_le_bytes().to_vec(),
                author,
            ]
            .concat(),
        ),
    ];
    let mut section = Vec::new();
    let mut body: Vec<u8> = Vec::new();
    let head = 8 + 8 * props.len();
    for (id, p) in &props {
        section.extend(id.to_le_bytes());
        section.extend(((head + body.len()) as u32).to_le_bytes());
        body.extend(p);
    }
    let mut set = vec![0xFE, 0xFF, 0, 0];
    set.extend([0u8; 20]);
    set.extend(1u32.to_le_bytes());
    set.extend([0u8; 16]);
    set.extend(48u32.to_le_bytes());
    set.extend(((head + body.len()) as u32).to_le_bytes());
    set.extend((props.len() as u32).to_le_bytes());
    set.extend(section);
    set.extend(body);
    let mut streams = streams_of(&doc);
    streams.insert("/\u{5}SummaryInformation".into(), set);
    let read = parse_with_info(&compound_of(&streams)).unwrap();
    assert_eq!(read.info.title.as_deref(), Some("Quarterly report"));
    assert_eq!(read.info.creators, ["Alex Sample"]);
    assert_eq!(read.left_out, ["headers and footers", "text boxes"]);
    let texts: Vec<&str> = read.elements.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(texts, ["Body"]);
    // Separators alone in the headers' story are no headers.
    let mut s = stories(&["Body"]);
    s.headers = vec!["\u{3}", "\u{4}"];
    assert!(parse_with_info(&doc_with(&s, 0x00C1, 0x0200))
        .unwrap()
        .left_out
        .is_empty());
}

/// `doc` with `parts` appended to its table stream, each where the FIB
/// pair it is names it (`fcPlcfHdd` is 11, `fcDop` 31, …), the FIB saying
/// it holds every pair a Word 97 file does, and `tail` appended to the
/// `WordDocument` stream.
fn with_tables(doc: &[u8], parts: &[(usize, Vec<u8>)], tail: &[u8]) -> Vec<u8> {
    let mut streams = streams_of(doc);
    let mut table = streams.remove("/1Table").unwrap();
    let wd = streams.get_mut("/WordDocument").unwrap();
    wd[0x98..0x9A].copy_from_slice(&93u16.to_le_bytes());
    for (pair, data) in parts {
        let at = 0x9A + 8 * pair;
        wd[at..at + 4].copy_from_slice(&(table.len() as u32).to_le_bytes());
        wd[at + 4..at + 8].copy_from_slice(&(data.len() as u32).to_le_bytes());
        table.extend(data);
    }
    wd.extend(tail);
    streams.insert("/1Table".into(), table);
    compound_of(&streams)
}

/// The length of a `.doc`'s `WordDocument` stream: where [`with_tables`]
/// puts its `tail`.
fn wd_len(doc: &[u8]) -> u32 {
    streams_of(doc)["/WordDocument"].len() as u32
}

/// A PLC: its positions, then its data items.
fn plc_of(cps: &[u32], data: &[Vec<u8>]) -> Vec<u8> {
    let mut v: Vec<u8> = cps.iter().flat_map(|c| c.to_le_bytes()).collect();
    v.extend(data.iter().flatten());
    v
}

/// The header stories `slots` (six for the notes' separators, then six a
/// section), each `None` where it is empty: the paragraphs of the headers'
/// story [`doc_with`] writes, and the `PlcfHdd` bounding them, with the
/// guard Word ends it with.
fn hdd<'a>(slots: &[Option<&'a str>]) -> (Vec<&'a str>, Vec<u8>) {
    let mut cps = vec![0u32];
    let mut paras = Vec::new();
    for slot in slots {
        let len = slot.map_or(0, |t| t.encode_utf16().count() as u32 + 1);
        cps.push(cps.last().unwrap() + len);
        paras.extend(*slot);
    }
    cps.push(cps.last().unwrap() + 1);
    (paras, plc_of(&cps, &[]))
}

/// A PLC of one section, its properties (SEPX) at `sepx` in the
/// `WordDocument` stream.
fn sed(sepx: u32) -> Vec<u8> {
    let mut d = vec![0u8; 12];
    d[2..6].copy_from_slice(&sepx.to_le_bytes());
    plc_of(&[0, 1], &[d])
}

/// Section properties: their size, then `sprmSFTitlePage` set as `on`.
fn title_sepx(on: bool) -> Vec<u8> {
    let mut v = 3u16.to_le_bytes().to_vec();
    v.extend(0x300Au16.to_le_bytes());
    v.push(u8::from(on));
    v
}

fn kinds_texts(els: &[Element]) -> Vec<(&str, &str, &str)> {
    els.iter()
        .map(|e| (e.kind.as_str(), e.text.as_str(), e.evidence))
        .collect()
}

#[test]
fn headers_and_footers_are_read_once_where_word_shows_them() {
    // A section's stories: even pages' header and footer, odd pages' (the
    // only ones a document that does not set even pages apart shows), and
    // the first page's, shown by a section with a title page.
    let (headers, plcf_hdd) = hdd(&[
        Some("\u{3}"),
        Some("\u{4}"),
        None,
        None,
        None,
        None,
        Some("Even head"),
        Some("Ref: QA-7"),
        Some("Even foot"),
        Some("Uncontrolled copy"),
        Some("Cover only\rRef: QA-7"),
        None,
    ]);
    let mut s = stories(&["Body text"]);
    s.headers = headers;
    s.footnotes = vec!["\u{2} A note."];
    s.comments = vec!["Why?"];
    let doc = doc_with(&s, 0x00C1, 0x0200);
    let read = |title: bool, facing: bool| {
        let sepx = wd_len(&doc);
        let mut dop = vec![0u8; 8];
        dop[0] = u8::from(facing);
        let bytes = with_tables(
            &doc,
            &[(11, plcf_hdd.clone()), (6, sed(sepx)), (31, dop)],
            &title_sepx(title),
        );
        parse_with_info(&bytes).unwrap()
    };
    let titled = read(true, false);
    assert!(titled.left_out.is_empty());
    assert_eq!(
        kinds_texts(&titled.elements),
        [
            // The first page's header, then the rest's, a line they share
            // once; the notes' separators nothing.
            ("doco:Paragraph", "Cover only", "header"),
            ("doco:Paragraph", "Ref: QA-7", "header"),
            ("doco:Paragraph", "Body text", "doc"),
            ("doco:Paragraph", "1 A note.", "footnote"),
            // The footer after the notes, as a DOCX's; comments last.
            ("doco:Paragraph", "Uncontrolled copy", "footer"),
            ("doco:Paragraph", "Why?", "comment"),
        ]
    );
    assert_eq!(titled.elements[5].id, "elem-00006");
    // No title page: the first page's header is not shown. Even pages set
    // apart: theirs are, after the odd pages'.
    let texts = |p: Parsed| -> Vec<String> { p.elements.into_iter().map(|e| e.text).collect() };
    assert_eq!(
        texts(read(false, true)),
        [
            "Ref: QA-7",
            "Even head",
            "Body text",
            "1 A note.",
            "Uncontrolled copy",
            "Even foot",
            "Why?"
        ]
    );
}

/// A text box's FTXBXS: the shape holding it, and whether it is kept for
/// reuse (deleted).
fn ftxbxs(lid: u32, reusable: bool) -> Vec<u8> {
    let mut d = vec![0u8; 22];
    d[0..4].copy_from_slice(&1u32.to_le_bytes());
    d[8..10].copy_from_slice(&u16::from(reusable).to_le_bytes());
    d[14..18].copy_from_slice(&lid.to_le_bytes());
    d
}

/// A shape's FSPA: its id, and the rest zeros.
fn fspa(spid: u32) -> Vec<u8> {
    let mut d = vec![0u8; 26];
    d[0..4].copy_from_slice(&spid.to_le_bytes());
    d
}

#[test]
fn text_boxes_are_read_after_the_paragraph_holding_their_anchor() {
    // The body anchors shape 1026 at its `\x08` in the second paragraph;
    // the text boxes' story holds its box, a deleted box kept for reuse,
    // and a box whose shape no anchor names. The header anchors 2049.
    let main = ["Before.", "\u{8}Anchored here.", "After."];
    let (headers, plcf_hdd) = hdd(&[
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("Head\u{8} two"),
        None,
        None,
        None,
        None,
    ]);
    let mut s = stories(&main);
    s.headers = headers;
    s.boxes = vec!["Boxed words", "Second box line", "Deleted box", "Loose box"];
    s.header_boxes = vec!["Header box"];
    let doc = doc_with(&s, 0x00C1, 0x0200);
    // Box 1 is its two paragraphs (positions 0..28), then 28..40, 40..50.
    let txbx = plc_of(
        &[0, 28, 40, 50, 51],
        &[
            ftxbxs(1026, false),
            ftxbxs(7, true),
            ftxbxs(99, false),
            ftxbxs(0, false),
        ],
    );
    let spa_mom = plc_of(&[8, 32], &[fspa(1026)]);
    let hdr_txbx = plc_of(&[0, 11, 12], &[ftxbxs(2049, false), ftxbxs(0, false)]);
    let spa_hdr = plc_of(&[4, 11], &[fspa(2049)]);
    let bytes = with_tables(
        &doc,
        &[
            (11, plcf_hdd),
            (56, txbx),
            (40, spa_mom),
            (58, hdr_txbx),
            (41, spa_hdr),
        ],
        &[],
    );
    let read = parse_with_info(&bytes).unwrap();
    assert!(read.left_out.is_empty(), "{:?}", read.left_out);
    assert_eq!(
        kinds_texts(&read.elements),
        [
            ("doco:Paragraph", "Head two", "header"),
            ("doco:Paragraph", "Header box", "header"),
            ("doco:Paragraph", "Before.", "doc"),
            ("doco:Paragraph", "Anchored here.", "doc"),
            ("doco:Paragraph", "Boxed words", "doc"),
            ("doco:Paragraph", "Second box line", "doc"),
            ("doco:Paragraph", "After.", "doc"),
            // No anchor found: after the body.
            ("doco:Paragraph", "Loose box", "doc"),
        ]
    );
}

#[test]
fn a_header_list_that_does_not_read_is_said_not_read() {
    let (headers, plcf_hdd) = hdd(&[
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("Ref: QA-7"),
        None,
        Some("Footer text"),
        None,
        None,
    ]);
    let mut s = stories(&["Body"]);
    s.headers = headers;
    s.boxes = vec!["Boxed"];
    let doc = doc_with(&s, 0x00C1, 0x0200);
    let read =
        |parts: &[(usize, Vec<u8>)]| parse_with_info(&with_tables(&doc, parts, &[])).unwrap();
    let texts = |p: &Parsed| -> Vec<String> { p.elements.iter().map(|e| e.text.clone()).collect() };
    // Positions that go backwards: the stories would overlap, and be read
    // again for each.
    let mut backwards = plcf_hdd.clone();
    backwards[4 * 10..4 * 11].copy_from_slice(&5u32.to_le_bytes());
    // A list cut short, its size running past the table stream.
    let mut streams = streams_of(&with_tables(&doc, &[(11, plcf_hdd.clone())], &[]));
    let wd = streams.get_mut("/WordDocument").unwrap();
    let at = 0x9A + 8 * 11 + 4;
    wd[at..at + 4].copy_from_slice(&0x7FFF_FFF0u32.to_le_bytes());
    let cut = parse_with_info(&compound_of(&streams)).unwrap();
    for p in [read(&[(11, backwards)]), read(&[]), cut] {
        assert_eq!(p.left_out, ["headers and footers", "text boxes"]);
        assert_eq!(texts(&p), ["Body"]);
    }
    // Positions past the story: held to it.
    let mut long = plcf_hdd.clone();
    let n = long.len() / 4;
    for i in 10..n {
        long[4 * i..4 * i + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    }
    let p = read(&[(11, long)]);
    assert_eq!(texts(&p), ["Ref: QA-7", "Body", "Footer text"]);
    // A text box list cut to a few bytes reads as none.
    let p = read(&[(11, plcf_hdd), (56, vec![1, 2, 3])]);
    assert_eq!(p.left_out, ["text boxes"]);
}

#[test]
fn section_properties_are_looked_through_within_a_bound() {
    // Five hundred sections, each with a first page's header, each naming
    // its own section properties two bytes past the last's, each running
    // sixty kilobytes over the next ones: thirty megabytes looked
    // through, past the bound.
    let n = 500usize;
    let mut slots = vec![None; 6];
    for _ in 0..n {
        slots.extend([None, None, None, None, Some("x"), None]);
    }
    let (headers, plcf_hdd) = hdd(&slots);
    let mut s = stories(&["Body"]);
    s.headers = headers;
    let doc = doc_with(&s, 0x00C1, 0x0200);
    let base = wd_len(&doc);
    // At every even byte a size of 0xFFF0, then sprms none of which is
    // the title page's.
    let sepx: Vec<u8> = [0xF0, 0xFF].repeat(0x8000 + n);
    let cps: Vec<u32> = (0..=n as u32).collect();
    let seds: Vec<Vec<u8>> = (0..n as u32)
        .map(|i| {
            let mut d = vec![0u8; 12];
            d[2..6].copy_from_slice(&(base + 2 * i).to_le_bytes());
            d
        })
        .collect();
    let bytes = with_tables(
        &doc,
        &[(11, plcf_hdd.clone()), (6, plc_of(&cps, &seds))],
        &sepx,
    );
    let read = parse_with_info(&bytes).unwrap();
    assert_eq!(read.left_out, ["headers and footers"]);
    // One section's properties, however many sections name them, are
    // looked through once.
    let same: Vec<Vec<u8>> = (0..n).map(|_| seds[0].clone()).collect();
    let bytes = with_tables(&doc, &[(11, plcf_hdd), (6, plc_of(&cps, &same))], &sepx);
    assert!(parse_with_info(&bytes).unwrap().left_out.is_empty());
}

#[test]
fn a_run_of_characters_finds_its_first_piece_by_search() {
    // 200,000 pieces of a character each, and as many runs of one: each
    // looking through every piece is forty billion steps.
    let n = 200_000u32;
    let wd: Vec<u8> = (0..n).map(|i| b'a' + (i % 26) as u8).collect();
    let pieces: Vec<Piece> = (0..n)
        .map(|i| Piece {
            cp: (i, i + 1),
            fc: i,
            compressed: true,
        })
        .collect();
    for i in 0..n {
        let got: Vec<Char> = chars(&wd, &pieces, i, i + 1).collect();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].c, got[0].cp), ((b'a' + (i % 26) as u8) as char, i));
    }
}
