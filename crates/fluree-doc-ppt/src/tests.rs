//! Synthetic PowerPoint 97 files, record by record, and what the reader
//! makes of them.

use super::*;
use std::io::Write;

/// A record: version and instance, type, length, data.
fn rec(container: bool, instance: u16, typ: u16, data: &[u8]) -> Vec<u8> {
    let ver = if container { 0x0F } else { 0 };
    let mut v = ((instance << 4) | ver).to_le_bytes().to_vec();
    v.extend(typ.to_le_bytes());
    v.extend((data.len() as u32).to_le_bytes());
    v.extend(data);
    v
}

fn header(kind: u32) -> Vec<u8> {
    rec(false, 0, TEXT_HEADER, &kind.to_le_bytes())
}

fn chars(s: &str) -> Vec<u8> {
    let d: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
    rec(false, 0, TEXT_CHARS, &d)
}

fn bytes(s: &str) -> Vec<u8> {
    let d: Vec<u8> = s.chars().map(|c| c as u8).collect();
    rec(false, 0, TEXT_BYTES, &d)
}

/// A shape's text box (`ClientTextbox`) inside a drawing container.
fn textbox(parts: &[Vec<u8>]) -> Vec<u8> {
    rec(true, 0, 0x040C, &rec(true, 0, 0xF00D, &parts.concat()))
}

fn slide_persist(id: u32) -> Vec<u8> {
    let mut d = id.to_le_bytes().to_vec();
    d.extend([0u8; 16]);
    rec(false, 0, SLIDE_PERSIST, &d)
}

/// A notes slide's place in the notes list: where it is, and its id.
fn notes_persist(persist: u32, id: u32) -> Vec<u8> {
    let mut d = persist.to_le_bytes().to_vec();
    d.extend([0u8; 8]);
    d.extend(id.to_le_bytes());
    d.extend([0u8; 4]);
    rec(false, 0, SLIDE_PERSIST, &d)
}

/// A slide's own atom, naming its notes slide.
fn slide_atom(notes: u32) -> Vec<u8> {
    let mut d = vec![0u8; 16];
    d.extend(notes.to_le_bytes());
    d.extend([0u8; 4]);
    rec(false, 0, SLIDE_ATOM, &d)
}

fn persist_dir(entries: &[(u32, u32)]) -> Vec<u8> {
    let mut d = Vec::new();
    for (id, off) in entries {
        d.extend(((1u32 << 20) | id).to_le_bytes());
        d.extend(off.to_le_bytes());
    }
    rec(false, 0, PERSIST_DIRECTORY, &d)
}

fn user_edit(last: u32, dir: u32, doc: u32) -> Vec<u8> {
    let mut d = vec![0u8; 8];
    d.extend(last.to_le_bytes());
    d.extend(dir.to_le_bytes());
    d.extend(doc.to_le_bytes());
    d.extend([0u8; 8]);
    rec(false, 0, USER_EDIT, &d)
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

/// A two-slide deck saved twice: the first save's second slide said
/// `Stale`, the second save's replaces it, and keeps its text in the slide
/// list as an older writer does.
pub(crate) fn deck() -> Vec<u8> {
    let list = rec(
        true,
        0,
        SLIDE_LIST,
        &[
            slide_persist(2),
            slide_persist(3),
            header(0),
            bytes("Detail"),
            header(1),
            bytes("Owner: Alex Sample"),
        ]
        .concat(),
    );
    let notes_list = rec(true, 2, SLIDE_LIST, &notes_persist(4, 256));
    let document = rec(true, 0, DOCUMENT, &[list, notes_list].concat());
    // The speaker's notes, as PowerPoint keeps them: a notes slide of its
    // own, with the slide's picture (no text) and the notes' text box.
    let notes = rec(
        true,
        0,
        NOTES,
        &[
            textbox(&[header(2)]),
            textbox(&[header(2), chars("Say the totals slowly.\rThen pause.")]),
        ]
        .concat(),
    );
    let slide1 = rec(
        true,
        0,
        SLIDE,
        &[
            slide_atom(256),
            textbox(&[header(0), chars("Quarterly review")]),
            textbox(&[header(1), chars("Orders up\rCosts down\u{b}by 3%")]),
        ]
        .concat(),
    );
    let stale = rec(true, 0, SLIDE, &textbox(&[header(1), bytes("Stale")]));
    let slide2 = rec(true, 0, SLIDE, &[]);
    let mut s = Vec::new();
    let doc_at = s.len() as u32;
    s.extend(&document);
    let s1 = s.len() as u32;
    s.extend(&slide1);
    let st = s.len() as u32;
    s.extend(&stale);
    let n1 = s.len() as u32;
    s.extend(&notes);
    let dir1 = s.len() as u32;
    s.extend(persist_dir(&[(1, doc_at), (2, s1), (3, st), (4, n1)]));
    let edit1 = s.len() as u32;
    s.extend(user_edit(0, dir1, 1));
    let s2 = s.len() as u32;
    s.extend(&slide2);
    let dir2 = s.len() as u32;
    s.extend(persist_dir(&[(3, s2)]));
    let edit2 = s.len() as u32;
    s.extend(user_edit(edit1, dir2, 1));
    let mut user = rec(false, 0, 0x0FF6, &[0u8; 0]);
    user.truncate(8);
    user.extend(20u32.to_le_bytes());
    user.extend(0xE391C05Fu32.to_le_bytes());
    user.extend(edit2.to_le_bytes());
    user.extend([0u8; 8]);
    compound(&[("/PowerPoint Document", &s), ("/Current User", &user)])
}

#[test]
fn slides_read_in_order_through_the_last_edit() {
    let els = parse(&deck()).unwrap();
    let got: Vec<(usize, &str, &str)> = els
        .iter()
        .map(|e| (e.page, e.kind.as_str(), e.text.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (0, "doco:SectionTitle", "Quarterly review"),
            // Body text, which the master bullets: list items, a line
            // break kept as a line.
            (0, "doc:ListItem", "Orders up"),
            (0, "doc:ListItem", "Costs down\nby 3%"),
            // The notes, after the slide.
            (0, "doco:Paragraph", "Say the totals slowly."),
            (0, "doco:Paragraph", "Then pause."),
            // The second save's slide holds no text: its list's is read,
            // and the first save's `Stale` is not.
            (1, "doco:SectionTitle", "Detail"),
            (1, "doc:ListItem", "Owner: Alex Sample"),
        ]
    );
    assert_eq!(els[3].evidence, "notes");
    assert_eq!(els[2].evidence, "ppt");
    assert!(els
        .iter()
        .all(|e| e.provenance == "ppt" && e.bbox.is_none()));
    assert_eq!(els[5].id, "elem-00006");
}

#[test]
fn an_edit_chain_that_loops_ends_and_broken_offsets_are_malformed() {
    // An edit naming itself as the one before.
    let mut s = Vec::new();
    let document = rec(true, 0, DOCUMENT, &rec(true, 0, SLIDE_LIST, &[]));
    s.extend(&document);
    let dir = s.len() as u32;
    s.extend(persist_dir(&[(1, 0)]));
    let edit = s.len() as u32;
    s.extend(user_edit(edit, dir, 1));
    let mut user = vec![0u8; 16];
    user.extend(edit.to_le_bytes());
    let looped = compound(&[("/PowerPoint Document", &s), ("/Current User", &user)]);
    assert!(parse(&looped).unwrap().is_empty());
    // The last edit is not where the Current User stream says.
    let mut user = vec![0u8; 16];
    user.extend(9999u32.to_le_bytes());
    let lost = compound(&[("/PowerPoint Document", &s), ("/Current User", &user)]);
    assert!(matches!(parse(&lost), Err(PptError::Malformed(_))));
}

/// A stream of `dirs` (each laid where it is) and a chain of edits, the
/// last first, the i-th naming the directory at `names[i]`.
fn edits_naming(dirs: &[u8], names: &[u32]) -> (Vec<u8>, usize) {
    let mut s = dirs.to_vec();
    let mut last = 0u32;
    for &dir in names {
        let at = s.len() as u32;
        s.extend(user_edit(last, dir, 1));
        last = at;
    }
    (s, last as usize)
}

#[test]
fn a_persist_directory_is_read_once_and_its_entries_counted() {
    // 4,096 edits all naming one directory of 2,000 entries: read once.
    let entries: Vec<(u32, u32)> = (1..=2_000).map(|i| (i, 0)).collect();
    let (s, last) = edits_naming(&persist_dir(&entries), &[0; MAX_EDITS]);
    let (map, _) = persist(&s, last).unwrap();
    assert_eq!(map.len(), 2_000);
    // 1,024 directories laid inside each other's data, each a record of its
    // own reaching to the end: as many readings of the same entries.
    let (n, filler) = (1_024usize, 20_000usize);
    let total = 8 * n + filler;
    let mut dirs = Vec::new();
    for i in 0..n {
        dirs.extend(0u16.to_le_bytes());
        dirs.extend(PERSIST_DIRECTORY.to_le_bytes());
        dirs.extend(((total - 8 * i - 8) as u32).to_le_bytes());
    }
    dirs.resize(total, 0);
    let names: Vec<u32> = (0..n as u32).map(|i| 8 * i).collect();
    let (s, last) = edits_naming(&dirs, &names);
    assert!(matches!(persist(&s, last), Err(PptError::TooLarge(_))));
}

#[test]
fn only_a_compound_file_with_a_powerpoint_stream_is_one() {
    assert!(is_ppt(&deck()));
    let word = compound(&[("/WordDocument", b"x")]);
    assert!(!is_ppt(&word));
    assert!(matches!(parse(&word), Err(PptError::NotPowerPoint)));
    assert!(matches!(parse(b"PK"), Err(PptError::Container(_))));
}

#[test]
fn nesting_past_the_bound_is_not_followed() {
    let mut inner = textbox(&[header(1), bytes("Deep")]);
    for _ in 0..(MAX_DEPTH + 4) {
        inner = rec(true, 0, 0x040C, &inner);
    }
    let r = record(&inner, 0).unwrap();
    let mut out = Vec::new();
    texts(&inner, &r, 0, &mut 4, false, &mut out);
    assert!(out.is_empty());
}

#[test]
fn a_title_placeholder_is_the_title_whatever_its_text_header_says() {
    // As LibreOffice writes a slide: every text `Other` (4), the title
    // known by the shape's placeholder (`PT_Title`, 0x0D).
    let placeholder = |kind: u8| {
        rec(
            true,
            0,
            0xF011,
            &rec(false, 0, PLACEHOLDER, &[0, 0, 0, 0, kind, 0, 0, 0]),
        )
    };
    let shape = |kind: u8, text: &str| {
        rec(
            true,
            0,
            SHAPE,
            &[
                placeholder(kind),
                rec(true, 0, 0xF00D, &[header(4), chars(text)].concat()),
            ]
            .concat(),
        )
    };
    let slide = rec(
        true,
        0,
        SLIDE,
        &rec(
            true,
            0,
            0x040C,
            &[shape(0x0D, "Review"), shape(0x0E, "Body text")].concat(),
        ),
    );
    let r = record(&slide, 0).unwrap();
    let mut out = Vec::new();
    texts(&slide, &r, 0, &mut 4, false, &mut out);
    let kinds: Vec<(u32, &str)> = out
        .iter()
        .filter_map(|i| match i {
            Item::Text(t) => Some((t.kind, t.text.as_str())),
            Item::Table(_) => None,
        })
        .collect();
    assert_eq!(kinds, [(0, "Review"), (4, "Body text")]);
}

/// The paragraph properties LibreOffice writes for an outline of three
/// bulleted paragraphs, the second indented one level.
const OUTLINE_PROPS: &str = "0a0000000000ff710a000f0095f004002d00000000fe5d00ffffffff880001000c0000000100ff750a000f0095f004002d00000000fe5d00ffffffff1001880001000b0000000000ff750a000f0095f004002d00000000fe5d00ffffffff8800000001000a00000000000400000000fe0c00000000000400000000fe0b00000000000400000000fe";

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn bullets_and_indent_levels_make_list_items() {
    let text = "Orders up\rWest region\rCosts down".to_string();
    let runs = para_runs(&unhex(OUTLINE_PROPS), text.encode_utf16().count());
    let shape: Vec<(u32, u16, Option<bool>)> =
        runs.iter().map(|r| (r.count, r.indent, r.bullet)).collect();
    assert_eq!(
        shape,
        [
            (10, 0, Some(true)),
            (12, 1, Some(true)),
            (11, 0, Some(true))
        ]
    );
    // Text LibreOffice marks `Other` (4): its bullets say it is a list.
    let t = Text {
        kind: 4,
        text,
        paras: runs,
    };
    assert_eq!(
        paragraphs(&t),
        [
            ("Orders up".to_string(), true),
            ("West region".to_string(), true),
            ("Costs down".to_string(), true)
        ]
    );
    // A body placeholder's paragraph that says it has no bullet is prose;
    // an indented one is a list item whatever its bullet; other text that
    // says nothing is prose.
    let run = |count, indent, bullet| ParaRun {
        count,
        indent,
        bullet,
    };
    let body = Text {
        kind: 1,
        text: "Intro\rPoint\rSub".into(),
        paras: vec![
            run(6, 0, Some(false)),
            run(6, 0, None),
            run(4, 2, Some(false)),
        ],
    };
    assert_eq!(
        paragraphs(&body),
        [
            ("Intro".to_string(), false),
            ("Point".to_string(), true),
            ("Sub".to_string(), true)
        ]
    );
    let other = Text {
        kind: 4,
        text: "Caption".into(),
        paras: Vec::new(),
    };
    assert_eq!(paragraphs(&other), [("Caption".to_string(), false)]);
}

/// A shape's properties: each id and value, none complex.
fn opt(typ: u16, props: &[(u16, u32)]) -> Vec<u8> {
    let d: Vec<u8> = props
        .iter()
        .flat_map(|(id, v)| [id.to_le_bytes().to_vec(), v.to_le_bytes().to_vec()].concat())
        .collect();
    rec(true, props.len() as u16, typ, &d)[..].to_vec()
}

/// A table cell: a shape placed at `l, t, r, b` in its group, holding
/// `text`.
fn cell(l: i32, t: i32, r: i32, b: i32, text: &str) -> Vec<u8> {
    let anchor: Vec<u8> = [l, t, r, b].iter().flat_map(|v| v.to_le_bytes()).collect();
    rec(
        true,
        0,
        SHAPE,
        &[
            rec(false, 1, SHAPE_HEADER, &[0; 8]),
            rec(false, 0, CHILD_ANCHOR, &anchor),
            rec(true, 0, 0xF00D, &[header(4), chars(text)].concat()),
        ]
        .concat(),
    )
}

#[test]
fn a_group_drawn_as_a_table_is_a_table() {
    // As LibreOffice writes one: the group's first shape says it is a
    // table; the cells are shapes placed in a grid, `Region` spanning both
    // columns of its row; a border line is no cell.
    let line = rec(
        true,
        0,
        SHAPE,
        &[
            rec(false, LINE_SHAPE, SHAPE_HEADER, &[0; 8]),
            rec(false, 0, CHILD_ANCHOR, &[0u8; 16]),
        ]
        .concat(),
    );
    let group = rec(
        true,
        0,
        GROUP,
        &[
            rec(
                true,
                0,
                SHAPE,
                &opt(TERTIARY_OPT, &[(0x039F, 1), (TABLE_PROPERTIES, 0xE)]),
            ),
            cell(0, 0, 200, 50, "Region West"),
            cell(0, 50, 100, 100, "Owner"),
            cell(100, 50, 200, 100, "Alex Sample"),
            line,
        ]
        .concat(),
    );
    let slide = rec(true, 0, SLIDE, &rec(true, 0, 0x040C, &group));
    let r = record(&slide, 0).unwrap();
    let mut out = Vec::new();
    texts(&slide, &r, 0, &mut 4, false, &mut out);
    let [Item::Table(t)] = out.as_slice() else {
        panic!("{out:?}");
    };
    assert_eq!(
        t.rows,
        [vec!["Region West", ""], vec!["Owner", "Alex Sample"]]
    );
    assert_eq!(t.left, [false, true, false, false]);
    let e = table_element(t.clone(), 3);
    assert_eq!(e.kind, "doco:Table");
    assert_eq!(e.page, 3);
    assert!(e.merged_down.is_none());
    // The same group without the table property is its shapes' text.
    let plain = rec(
        true,
        0,
        GROUP,
        &[rec(true, 0, SHAPE, &[]), cell(0, 0, 10, 10, "Loose")].concat(),
    );
    let r = record(&plain, 0).unwrap();
    let mut out = Vec::new();
    texts(&plain, &r, 0, &mut 4, false, &mut out);
    assert!(matches!(out.as_slice(), [Item::Text(t)] if t.text == "Loose"));
}
