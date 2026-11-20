//! Synthetic BIFF8 workbooks, written record by record into a compound
//! file, and what the reader makes of them.

use super::*;
use std::io::Write;

/// One BIFF record: its type, its length, its data.
pub(crate) fn rec(typ: u16, data: &[u8]) -> Vec<u8> {
    let mut v = typ.to_le_bytes().to_vec();
    v.extend((data.len() as u16).to_le_bytes());
    v.extend(data);
    v
}

fn cell_head(row: u16, col: u16, xf: u16) -> Vec<u8> {
    [row.to_le_bytes(), col.to_le_bytes(), xf.to_le_bytes()].concat()
}

/// A NUMBER record.
pub(crate) fn number(row: u16, col: u16, xf: u16, v: f64) -> Vec<u8> {
    rec(
        0x0203,
        &[cell_head(row, col, xf), v.to_le_bytes().to_vec()].concat(),
    )
}

/// A LABELSST record: the `i`-th string of the shared string table.
pub(crate) fn label(row: u16, col: u16, xf: u16, i: u32) -> Vec<u8> {
    rec(
        0x00FD,
        &[cell_head(row, col, xf), i.to_le_bytes().to_vec()].concat(),
    )
}

fn boolean(row: u16, col: u16, v: bool) -> Vec<u8> {
    rec(
        0x0205,
        &[cell_head(row, col, 0), vec![u8::from(v), 0]].concat(),
    )
}

fn merge(r0: u16, r1: u16, c0: u16, c1: u16) -> Vec<u8> {
    let mut d = 1u16.to_le_bytes().to_vec();
    for v in [r0, r1, c0, c1] {
        d.extend(v.to_le_bytes());
    }
    rec(0x00E5, &d)
}

fn dimensions(rows: u32, cols: u16) -> Vec<u8> {
    let mut d = 0u32.to_le_bytes().to_vec();
    d.extend(rows.to_le_bytes());
    d.extend(0u16.to_le_bytes());
    d.extend(cols.to_le_bytes());
    d.extend(0u16.to_le_bytes());
    rec(0x0200, &d)
}

/// The rows above `rows` frozen: WINDOW2 marking the pane frozen, then the
/// PANE splitting it.
fn frozen(rows: u16) -> Vec<Vec<u8>> {
    let mut w = 0x0008u16.to_le_bytes().to_vec();
    w.extend([0u8; 16]);
    let pane = [0u16, rows, rows, 0, 2]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<u8>>();
    vec![rec(0x023E, &w), rec(0x0041, &pane)]
}

/// XF indices the workbook below declares.
pub(crate) const PLAIN: u16 = 0;
pub(crate) const BOLD: u16 = 1;
pub(crate) const DATE: u16 = 2;

/// A BIFF8 workbook stream: the globals (two fonts, the formats above, the
/// shared strings, a sheet record per sheet), then each sheet's records.
pub(crate) fn stream(strings: &[&str], sheets: &[(&str, Vec<Vec<u8>>)]) -> Vec<u8> {
    let bof = |dt: u16| {
        rec(
            0x0809,
            &[0x0600u16, dt, 0, 0, 0, 0, 0, 0]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<u8>>(),
        )
    };
    let font = |weight: u16| {
        let mut d = [200u16, 0, 0x7FFF, weight, 0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<u8>>();
        d.extend([0, 0, 0, 0, 5, 0]);
        d.extend(b"Arial");
        rec(0x0031, &d)
    };
    let xf = |font: u16, fmt: u16| {
        let mut d = [font, fmt]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<u8>>();
        d.extend([0u8; 16]);
        rec(0x00E0, &d)
    };
    let mut sst = (strings.len() as u32).to_le_bytes().to_vec();
    sst.extend((strings.len() as u32).to_le_bytes());
    for s in strings {
        sst.extend((s.len() as u16).to_le_bytes());
        sst.push(0);
        sst.extend(s.as_bytes());
    }
    let sheet_streams: Vec<Vec<u8>> = sheets
        .iter()
        .map(|(_, recs)| [bof(0x0010), recs.concat(), rec(0x000A, &[])].concat())
        .collect();
    let globals = |offsets: &[u32]| {
        let mut g = vec![
            bof(0x0005),
            rec(0x0042, &1200u16.to_le_bytes()),
            font(400),
            font(700),
            xf(0, 0),
            xf(1, 0),
            xf(0, 14),
        ];
        for ((name, _), off) in sheets.iter().zip(offsets) {
            let mut d = off.to_le_bytes().to_vec();
            d.extend([0, 0, name.len() as u8, 0]);
            d.extend(name.as_bytes());
            g.push(rec(0x0085, &d));
        }
        g.push(rec(0x00FC, &sst));
        g.push(rec(0x000A, &[]));
        g.concat()
    };
    let head = globals(&vec![0; sheets.len()]).len() as u32;
    let mut offsets = Vec::new();
    let mut at = head;
    for s in &sheet_streams {
        offsets.push(at);
        at += s.len() as u32;
    }
    [globals(&offsets), sheet_streams.concat()].concat()
}

/// A compound file holding `streams`, by path: version 3, with 512-byte
/// sectors, as Excel writes one.
pub(crate) fn compound(streams: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cf =
        cfb::CompoundFile::create_with_version(cfb::Version::V3, std::io::Cursor::new(Vec::new()))
            .unwrap();
    for (path, data) in streams {
        cf.create_stream(path).unwrap().write_all(data).unwrap();
    }
    cf.flush().unwrap();
    cf.into_inner().into_inner()
}

pub(crate) fn workbook(strings: &[&str], sheets: &[(&str, Vec<Vec<u8>>)]) -> Vec<u8> {
    compound(&[("/Workbook", &stream(strings, sheets))])
}

/// An order book: a bold title, a header frozen above a table of names,
/// amounts and dates, a merged band, and a second sheet.
pub(crate) fn orders() -> Vec<u8> {
    let strings = [
        "Quarterly orders",
        "Customer",
        "Amount",
        "Shipped",
        "Alex Sample",
        "Sam Example",
        "Region West",
        "Paid",
        "Notes",
    ];
    let mut first = frozen(3);
    first.extend([
        label(0, 0, BOLD, 0),
        label(2, 0, BOLD, 1),
        label(2, 1, BOLD, 2),
        label(2, 2, BOLD, 3),
        label(3, 0, PLAIN, 4),
        number(3, 1, PLAIN, 12.5),
        // 2026-03-14 in the 1900 system.
        number(3, 2, DATE, 46095.0),
        label(4, 0, PLAIN, 6),
        label(5, 0, PLAIN, 5),
        number(5, 1, PLAIN, 0.125),
        number(5, 2, DATE, 46096.0),
        merge(4, 4, 0, 2),
    ]);
    let second = vec![
        label(0, 0, BOLD, 7),
        boolean(0, 1, true),
        label(1, 0, PLAIN, 8),
    ];
    // Listed `Zeta` first: sheets go in the workbook's order, not by name.
    workbook(&strings, &[("Zeta orders", first), ("Alpha", second)])
}

#[test]
fn a_workbook_reads_as_an_xlsx_sheet_reads() {
    let (els, info) = parse_with_info(&orders()).unwrap();
    assert_eq!(info, DocumentInfo::default());
    let at = |kind: &str, text: &str| els.iter().position(|e| e.kind == kind && e.text == text);
    assert_eq!(els[0].text, "Zeta orders");
    assert_eq!(els[0].page, 0);
    // Bold, alone on its row above the block: the block's title.
    assert!(
        at("doco:SectionTitle", "Quarterly orders").is_some(),
        "{els:#?}"
    );
    let t = els.iter().find(|e| e.kind == "doco:Table").unwrap();
    let cells = t.cells.as_ref().unwrap();
    assert_eq!(cells[0], ["Customer", "Amount", "Shipped"]);
    // General format: the number as stored; a date as an ISO date.
    assert_eq!(cells[1], ["Alex Sample", "12.5", "2026-03-14"]);
    assert_eq!(cells[3], ["Sam Example", "0.125", "2026-03-15"]);
    assert_eq!(t.header_rows, Some(1));
    // The merged band across the table is a sub-header.
    assert_eq!(cells[2][0], "Region West");
    assert_eq!(t.sub_headers, Some(vec![2]));
    let datums = t.datums.as_ref().unwrap();
    assert_eq!(datums[1][1].as_ref().unwrap().value, "12.5");
    assert_eq!(datums[1][2].as_ref().unwrap().datatype, "xsd:date");
    assert_eq!(datums[1][2].as_ref().unwrap().value, "2026-03-14");
    assert!(els
        .iter()
        .all(|e| e.provenance == "xls" && e.bbox.is_none()));
    // The second sheet is the second page.
    let alpha = els.iter().find(|e| e.text == "Alpha").unwrap();
    assert_eq!(alpha.page, 1);
    assert!(
        els.iter().any(|e| e.page == 1 && e.text.contains("TRUE")),
        "{els:#?}"
    );
}

#[test]
fn bold_and_frozen_rows_come_from_the_walk() {
    let sheets = scan::scan(&orders()).unwrap().0.sheets;
    assert_eq!(sheets.len(), 2);
    assert_eq!(sheets[0].frozen_rows, 3);
    assert!(sheets[0].bold.contains(&(2, 1)));
    assert!(!sheets[0].bold.contains(&(3, 1)));
    assert_eq!(sheets[1].frozen_rows, 0);
}

#[test]
fn a_sheet_spanning_too_many_cells_is_refused_before_calamine() {
    // Two cells at opposite corners of BIFF8's grid: calamine would lay out
    // sixteen million.
    let far = workbook(
        &["a"],
        &[(
            "S",
            vec![number(0, 0, PLAIN, 1.0), number(65535, 255, PLAIN, 2.0)],
        )],
    );
    assert!(matches!(parse(&far), Err(XlsError::TooLarge(_))));
    // A DIMENSIONS record declaring a grid calamine would reserve is not
    // passed on: the sheet is the one cell it holds.
    let declared = workbook(
        &["a"],
        &[(
            "S",
            vec![dimensions(1 << 20, 256), number(0, 0, PLAIN, 1.0)],
        )],
    );
    assert!(parse(&declared).unwrap().iter().any(|e| e.text == "1"));
    // Columns past BIFF8's 256, which a hostile file can name.
    let wide = workbook(
        &["a"],
        &[(
            "S",
            vec![number(0, 0, PLAIN, 1.0), number(100, 60000, PLAIN, 2.0)],
        )],
    );
    assert!(matches!(parse(&wide), Err(XlsError::TooLarge(_))));
    // Within bounds, the same shape reads.
    let near = workbook(
        &["a"],
        &[(
            "S",
            vec![number(0, 0, PLAIN, 1.0), number(9, 9, PLAIN, 2.0)],
        )],
    );
    assert!(parse(&near).is_ok());
}

#[test]
fn a_sheet_offset_that_is_no_sheet_is_refused() {
    let mut s = stream(&["a"], &[("S", vec![number(0, 0, PLAIN, 1.0)])]);
    // Point the sheet record one byte past its BOF.
    let at = s.windows(2).position(|w| w == [0x85, 0x00]).unwrap() + 4;
    let off = u32::from_le_bytes(s[at..at + 4].try_into().unwrap()) + 1;
    s[at..at + 4].copy_from_slice(&off.to_le_bytes());
    let bytes = compound(&[("/Workbook", &s)]);
    // Refused by the walk, before calamine reads anything from there.
    assert!(
        matches!(parse(&bytes), Err(XlsError::Biff(ref m)) if m.contains("offset")),
        "{:?}",
        parse(&bytes)
    );
}

#[test]
fn only_a_compound_file_with_a_workbook_is_one() {
    assert!(is_xls(&orders()));
    let word = compound(&[("/WordDocument", b"not a workbook")]);
    assert!(!is_xls(&word));
    assert!(matches!(parse(&word), Err(XlsError::NoWorkbook)));
    assert!(!is_xls(b"PK\x03\x04"));
    assert!(matches!(parse(b"%PDF-1.4"), Err(XlsError::Container(_))));
}

#[test]
fn a_large_stream_outside_the_mini_stream_reads_too() {
    // Over 4096 bytes, the stream lives in regular sectors.
    let strings: Vec<String> = (0..400).map(|i| format!("Item number {i}")).collect();
    let refs: Vec<&str> = strings.iter().map(String::as_str).collect();
    let recs: Vec<Vec<u8>> = (0..400u16)
        .flat_map(|i| {
            [
                label(i, 0, PLAIN, u32::from(i)),
                number(i, 1, PLAIN, f64::from(i)),
            ]
        })
        .collect();
    let bytes = workbook(&refs, &[("Items", recs)]);
    let els = parse(&bytes).unwrap();
    let t = els.iter().find(|e| e.kind == "doco:Table").unwrap();
    assert_eq!(t.cells.as_ref().unwrap().len(), 400);
    assert_eq!(t.cells.as_ref().unwrap()[399], ["Item number 399", "399"]);
}

fn bof(dt: u16) -> Vec<u8> {
    let mut d = 0x0600u16.to_le_bytes().to_vec();
    d.extend(dt.to_le_bytes());
    d.extend([0u8; 12]);
    rec(0x0809, &d)
}

fn eof() -> Vec<u8> {
    rec(0x000A, &[])
}

fn sheet_record(pos: u32, name: &str) -> Vec<u8> {
    let mut d = pos.to_le_bytes().to_vec();
    d.extend([0, 0, name.len() as u8, 0]);
    d.extend(name.as_bytes());
    rec(0x0085, &d)
}

/// A workbook stream of `globals` (after its BOF), sheet records named
/// `S0`, `S1`… each at the offset `sheets` gives by index into `bodies`,
/// then `bodies`, each wrapped in its BOF and EOF.
fn raw_stream(globals: &[Vec<u8>], sheets: &[usize], bodies: &[Vec<Vec<u8>>]) -> Vec<u8> {
    let bodies: Vec<Vec<u8>> = bodies
        .iter()
        .map(|b| [bof(0x10), b.concat(), eof()].concat())
        .collect();
    let head = |offsets: &[u32]| {
        let mut g = vec![bof(5)];
        g.extend(globals.iter().cloned());
        for (i, &k) in sheets.iter().enumerate() {
            g.push(sheet_record(offsets[k], &format!("S{i}")));
        }
        g.push(eof());
        g.concat()
    };
    let mut at = head(&vec![0; bodies.len()]).len() as u32;
    let mut offsets = Vec::new();
    for b in &bodies {
        offsets.push(at);
        at += b.len() as u32;
    }
    [head(&offsets), bodies.concat()].concat()
}

fn dims(rf: u32, rl: u32, cf: u16, cl: u16) -> Vec<u8> {
    let mut d = rf.to_le_bytes().to_vec();
    d.extend(rl.to_le_bytes());
    d.extend(cf.to_le_bytes());
    d.extend(cl.to_le_bytes());
    d.extend([0u8; 2]);
    rec(0x0200, &d)
}

#[test]
fn a_dimensions_record_whose_rows_wrap_is_not_passed_on() {
    // calamine computes `rl - 1 - rf + 1` rows in u32: 10 > 0 wraps to four
    // billion, which it reserves.
    let s = raw_stream(
        &[],
        &[0],
        &[vec![dims(10, 1, 0, 1), number(10, 0, PLAIN, 7.0)]],
    );
    let els = parse(&compound(&[("/Workbook", &s)])).unwrap();
    assert!(els.iter().any(|e| e.text == "7"), "{els:#?}");
}

#[test]
fn calamine_reads_only_the_root_workbook_the_walk_read() {
    // A storage listed before the root's workbook holds a `workbook` of its
    // own: calamine's compound-file reader would take it, whatever its path.
    let hostile = raw_stream(
        &[],
        &[0],
        &[vec![dims(10, 1, 0, 1), number(0, 0, PLAIN, 1.0)]],
    );
    let fine = raw_stream(&[], &[0], &[vec![number(0, 0, PLAIN, 42.0)]]);
    let mut cf =
        cfb::CompoundFile::create_with_version(cfb::Version::V3, std::io::Cursor::new(Vec::new()))
            .unwrap();
    cf.create_storage("/A").unwrap();
    cf.create_stream("/A/workbook")
        .unwrap()
        .write_all(&hostile)
        .unwrap();
    cf.create_stream("/Workbook")
        .unwrap()
        .write_all(&fine)
        .unwrap();
    cf.flush().unwrap();
    let bytes = cf.into_inner().into_inner();
    let els = parse(&bytes).unwrap();
    assert!(els.iter().any(|e| e.text == "42"), "{els:#?}");
}

#[test]
fn a_sheet_record_nested_in_the_globals_is_counted() {
    // calamine reads the globals to their first EOF, a BOF inside them
    // notwithstanding: a sheet record there is a sheet.
    let far = [number(0, 0, PLAIN, 1.0), number(65535, 255, PLAIN, 2.0)];
    let body = [bof(0x10), far.concat(), eof()].concat();
    let mut g = [bof(5), bof(5)].concat();
    let at = g.len() + 4;
    g.extend(sheet_record(0, "S"));
    g.extend([eof(), eof()].concat());
    let pos = g.len() as u32;
    g[at..at + 4].copy_from_slice(&pos.to_le_bytes());
    g.extend(body);
    let bytes = compound(&[("/Workbook", &g)]);
    assert!(
        matches!(parse(&bytes), Err(XlsError::TooLarge(_))),
        "{:?}",
        parse(&bytes)
    );
}

#[test]
fn a_sheet_named_twice_counts_twice() {
    // Two corners 2,097,408 cells apart: one sheet record reads, two ask
    // calamine for two grids.
    let body = vec![number(0, 0, PLAIN, 1.0), number(8192, 255, PLAIN, 2.0)];
    let once = raw_stream(&[], &[0], std::slice::from_ref(&body));
    let r = parse(&compound(&[("/Workbook", &once)]));
    assert!(r.is_ok(), "{r:?}");
    let twice = raw_stream(&[], &[0, 0], &[body]);
    assert!(matches!(
        parse(&compound(&[("/Workbook", &twice)])),
        Err(XlsError::TooLarge(_))
    ));
}

/// A shared string table of one string of `n` characters.
fn sst_of(n: usize) -> Vec<u8> {
    let mut sst = 1u32.to_le_bytes().to_vec();
    sst.extend(1u32.to_le_bytes());
    sst.extend((n as u16).to_le_bytes());
    sst.push(0);
    sst.extend(std::iter::repeat_n(b'x', n));
    rec(0x00FC, &sst)
}

#[test]
fn a_shared_string_named_by_many_cells_counts_for_each() {
    // 9,000 cells naming one string of 8,000 characters: 72 million
    // characters calamine would copy, one string each.
    let cells: Vec<Vec<u8>> = (0..9_000u32)
        .map(|i| label((i / 64) as u16, (i % 64) as u16, PLAIN, 0))
        .collect();
    let s = raw_stream(&[sst_of(8_000)], &[0], &[cells]);
    assert!(
        matches!(parse(&compound(&[("/Workbook", &s)])), Err(XlsError::TooLarge(ref m)) if m.contains("characters")),
    );
    // The same string named by a few cells reads.
    let cells: Vec<Vec<u8>> = (0..10u16).map(|i| label(i, 0, PLAIN, 0)).collect();
    let s = raw_stream(&[sst_of(8_000)], &[0], &[cells]);
    assert!(parse(&compound(&[("/Workbook", &s)])).is_ok());
}

#[test]
fn records_calamine_would_index_past_are_refused_or_not_passed_on() {
    let book = |cells: Vec<Vec<u8>>| compound(&[("/Workbook", &raw_stream(&[], &[0], &[cells]))]);
    // A formula whose tokens claim more bytes than there are: calamine
    // slices past them. Its tokens are not passed on; its value reads.
    let mut f = cell_head(0, 0, PLAIN);
    f.extend(5.0f64.to_le_bytes());
    f.extend([0u8; 6]);
    f.extend(200u16.to_le_bytes());
    f.push(0x24);
    let els = parse(&book(vec![rec(0x0006, &f)])).unwrap();
    assert!(els.iter().any(|e| e.text == "5"), "{els:#?}");
    // A merge record counting more ranges than it holds.
    let mut m = 50u16.to_le_bytes().to_vec();
    m.extend([0u8; 8]);
    assert!(matches!(
        parse(&book(vec![number(0, 0, PLAIN, 1.0), rec(0x00E5, &m)])),
        Err(XlsError::Biff(_))
    ));
    // A multiple-number record whose last column is before its first.
    let mut mr = cell_head(0, 5, PLAIN);
    mr.extend([0u8; 4]);
    mr.extend(2u16.to_le_bytes());
    assert!(matches!(
        parse(&book(vec![rec(0x00BD, &mr)])),
        Err(XlsError::Biff(_))
    ));
    // A shared string whose continuation is empty where its characters go
    // on: calamine reads the continuation's first byte.
    let mut sst = 1u32.to_le_bytes().to_vec();
    sst.extend(1u32.to_le_bytes());
    sst.extend(10u16.to_le_bytes());
    sst.push(0);
    sst.extend(b"abc");
    let s = raw_stream(
        &[rec(0x00FC, &sst), rec(0x003C, &[])],
        &[0],
        &[vec![label(0, 0, PLAIN, 0)]],
    );
    assert!(matches!(
        parse(&compound(&[("/Workbook", &s)])),
        Err(XlsError::Biff(_))
    ));
    // A shared string flagged rich with no run count after it.
    let mut sst = 1u32.to_le_bytes().to_vec();
    sst.extend(1u32.to_le_bytes());
    sst.extend(1u16.to_le_bytes());
    sst.push(0x08);
    let s = raw_stream(&[rec(0x00FC, &sst)], &[0], &[vec![label(0, 0, PLAIN, 0)]]);
    assert!(matches!(
        parse(&compound(&[("/Workbook", &s)])),
        Err(XlsError::Biff(_))
    ));
    // A sheet record too short to hold its state and type.
    let s = raw_stream(&[rec(0x0085, &[0, 0, 0, 0])], &[0], &[vec![]]);
    assert!(matches!(
        parse(&compound(&[("/Workbook", &s)])),
        Err(XlsError::Biff(_))
    ));
}

/// Make the regular-sector stream `name` of a version 3 compound file loop
/// on its first sector, and declare it 4 GiB long.
fn loop_stream(f: &mut [u8], name: &str) {
    let u32_at = |f: &[u8], at: usize| u32::from_le_bytes(f[at..at + 4].try_into().unwrap());
    let sector = |n: u32| 512 + n as usize * 512;
    let fat = u32_at(f, 0x4C);
    let want: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut s = u32_at(f, 0x30);
    loop {
        for k in 0..4 {
            let at = sector(s) + 128 * k;
            if f[at..at + want.len()] == want[..] {
                let start = u32_at(f, at + 116);
                f[at + 120..at + 124].copy_from_slice(&0xFFFF_F000u32.to_le_bytes());
                let link = sector(fat) + 4 * start as usize;
                f[link..link + 4].copy_from_slice(&start.to_le_bytes());
                return;
            }
        }
        s = u32_at(f, sector(fat) + 4 * s as usize);
    }
}

#[test]
fn a_summary_whose_chain_loops_is_left_out() {
    // calamine's own compound-file reader follows the loop, allocating the
    // 4 GiB the stream declares.
    let s = raw_stream(&[], &[0], &[vec![number(0, 0, PLAIN, 3.0)]]);
    let mut summary = vec![0u8; 5_000];
    summary[..2].copy_from_slice(&[0xFE, 0xFF]);
    let mut bytes = compound(&[("/Workbook", &s), ("/\u{5}SummaryInformation", &summary)]);
    loop_stream(&mut bytes, "\u{5}SummaryInformation");
    let (els, info) = parse_with_info(&bytes).unwrap();
    assert!(els.iter().any(|e| e.text == "3"));
    assert_eq!(info, DocumentInfo::default());
}
