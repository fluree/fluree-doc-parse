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
    texts(
        &inner,
        &r,
        0,
        (&mut 4, false),
        &mut Budget::default(),
        &mut out,
    )
    .unwrap();
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
    texts(
        &slide,
        &r,
        0,
        (&mut 4, false),
        &mut Budget::default(),
        &mut out,
    )
    .unwrap();
    let kinds: Vec<(u32, &str)> = out
        .iter()
        .filter_map(|i| match i {
            Item::Text(t) => Some((t.kind, t.text.as_str())),
            Item::Table(_) | Item::Picture(..) => None,
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
        paragraphs(&t, &mut Budget::default()).unwrap(),
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
        paragraphs(&body, &mut Budget::default()).unwrap(),
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
    assert_eq!(
        paragraphs(&other, &mut Budget::default()).unwrap(),
        [("Caption".to_string(), false)]
    );
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
    texts(
        &slide,
        &r,
        0,
        (&mut 4, false),
        &mut Budget::default(),
        &mut out,
    )
    .unwrap();
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
    texts(
        &plain,
        &r,
        0,
        (&mut 4, false),
        &mut Budget::default(),
        &mut out,
    )
    .unwrap();
    assert!(matches!(out.as_slice(), [Item::Text(t)] if t.text == "Loose"));
}

/// A one-slide deck: the document holding the slide list and `extra`
/// records, the slide holding `slide`; the `Pictures` stream where given.
fn deck_of(extra: &[u8], slide: &[u8], pictures: Option<&[u8]>) -> Vec<u8> {
    let list = rec(true, 0, SLIDE_LIST, &slide_persist(2));
    let document = rec(true, 0, DOCUMENT, &[list, extra.to_vec()].concat());
    let slide = rec(true, 0, SLIDE, slide);
    let mut s = document.clone();
    let slide_at = s.len() as u32;
    s.extend(&slide);
    let dir = s.len() as u32;
    s.extend(persist_dir(&[(1, 0), (2, slide_at)]));
    let edit = s.len() as u32;
    s.extend(user_edit(0, dir, 1));
    let mut user = vec![0u8; 16];
    user.extend(edit.to_le_bytes());
    let mut streams: Vec<(&str, &[u8])> =
        vec![("/PowerPoint Document", &s), ("/Current User", &user)];
    if let Some(p) = pictures {
        streams.push(("/Pictures", p));
    }
    compound(&streams)
}

/// A PNG picture record, as the `Pictures` stream keeps one.
fn png_blip(png: &[u8]) -> Vec<u8> {
    rec(
        false,
        0x6E0,
        0xF01E,
        &[vec![7u8; 16], vec![0xFF], png.to_vec()].concat(),
    )
}

/// A store entry: its picture at `delay` in the `Pictures` stream, or held
/// in the entry itself.
fn fbse(delay: u32, inline: Option<&[u8]>) -> Vec<u8> {
    let mut d = vec![6u8, 6];
    d.extend([0u8; 16]);
    d.extend([0u8; 2]);
    d.extend(0u32.to_le_bytes());
    d.extend(1u32.to_le_bytes());
    d.extend(delay.to_le_bytes());
    d.extend([0u8; 4]);
    if let Some(b) = inline {
        d.extend(b);
    }
    rec(false, 6, 0xF007, &d)
}

/// A picture shape showing store entry `pib`, its alternative text `alt`
/// (a complex property, UTF-16 with its terminating zero).
fn picture_shape(pib: u32, alt: Option<&str>) -> Vec<u8> {
    let mut props = vec![(PIB | 0x4000, pib, Vec::new())];
    if let Some(a) = alt {
        let b: Vec<u8> = a
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        props.push((DESCRIPTION | 0x8000, b.len() as u32, b));
    }
    let mut d = Vec::new();
    for (id, v, _) in &props {
        d.extend(id.to_le_bytes());
        d.extend(v.to_le_bytes());
    }
    for (_, _, b) in &props {
        d.extend(b);
    }
    rec(
        true,
        0,
        SHAPE,
        &[
            rec(false, 75, SHAPE_HEADER, &[0; 8]),
            rec(false, props.len() as u16, OPT, &d),
        ]
        .concat(),
    )
}

#[test]
fn pictures_are_figures_whose_bytes_the_store_gives() {
    let png = b"\x89PNG\r\n\x1a\n first picture";
    let other = b"\x89PNG\r\n\x1a\n second picture";
    let pictures = png_blip(png);
    let store = rec(
        true,
        0,
        0x040B,
        &rec(
            true,
            0,
            0xF000,
            &rec(
                true,
                0,
                0xF001,
                &[fbse(0, None), fbse(0, Some(&png_blip(other)))].concat(),
            ),
        ),
    );
    let slide = rec(
        true,
        0,
        0x040C,
        &[
            textbox(&[header(0), chars("Screens")]),
            picture_shape(1, Some("Order screen  for Alex Sample")),
            picture_shape(2, None),
            // An entry the store does not have.
            picture_shape(9, None),
        ]
        .concat(),
    );
    let bytes = deck_of(&store, &slide, Some(&pictures));
    let (els, media) = parse_with_media(&bytes).unwrap();
    let figures: Vec<(&str, Option<&str>)> = els
        .iter()
        .filter(|e| e.kind == "doco:Figure")
        .map(|e| (e.text.as_str(), e.figure.as_deref()))
        .collect();
    assert_eq!(
        figures,
        [
            ("Order screen for Alex Sample", Some("figure-0-1")),
            ("", Some("figure-0-2")),
            ("", Some("figure-0-3")),
        ]
    );
    let parts: Vec<(Option<&str>, &str, String)> = media
        .iter()
        .map(|m| (m.source.part(), m.source.name(), m.tag()))
        .collect();
    assert_eq!(
        parts,
        [
            (Some("Pictures#1"), "Pictures#1", "m1".to_string()),
            (Some("Pictures#2"), "Pictures#2", "m2".to_string()),
            (None, "Pictures#9", "m3".to_string()),
        ]
    );
    // The bytes: from the Pictures stream, and from the entry itself.
    assert_eq!(read_media(&bytes, "Pictures#1").unwrap(), png);
    assert_eq!(read_media(&bytes, "Pictures#2").unwrap(), other);
    assert!(read_media(&bytes, "Pictures#9").is_err());
    assert!(read_media(&bytes, "word/media/x.png").is_err());
}

/// A deck whose slide list is `list`, with slides `slides`, each object id
/// in `ids` mapped to where a slide is (by its index in `slides`).
fn deck_listing(list: &[u8], slides: &[Vec<u8>], ids: &[(u32, usize)]) -> Vec<u8> {
    let document = rec(true, 0, DOCUMENT, &rec(true, 0, SLIDE_LIST, list));
    let mut s = document;
    let mut at = Vec::new();
    for slide in slides {
        at.push(s.len() as u32);
        s.extend(rec(true, 0, SLIDE, slide));
    }
    let mut entries = vec![(1, 0)];
    entries.extend(ids.iter().map(|&(id, k)| (id, at[k])));
    let dir = s.len() as u32;
    s.extend(persist_dir(&entries));
    let edit = s.len() as u32;
    s.extend(user_edit(0, dir, 1));
    let mut user = vec![0u8; 16];
    user.extend(edit.to_le_bytes());
    compound(&[("/PowerPoint Document", &s), ("/Current User", &user)])
}

#[test]
fn a_slide_listed_again_is_not_read_again() {
    let list = [slide_persist(2), slide_persist(2), slide_persist(2)].concat();
    let slide = textbox(&[header(1), bytes("Once")]);
    let els = parse(&deck_listing(&list, &[slide], &[(2, 0)])).unwrap();
    let texts: Vec<(usize, &str)> = els.iter().map(|e| (e.page, e.text.as_str())).collect();
    assert_eq!(texts, [(0, "Once")]);
}

#[test]
fn slides_that_are_one_slide_are_read_once() {
    // 300 slide ids the persist directory maps to one slide of a million
    // characters: read for each, 300 million from a deck of one million.
    let ids: Vec<(u32, usize)> = (2..302).map(|id| (id, 0)).collect();
    let list: Vec<u8> = ids.iter().flat_map(|&(id, _)| slide_persist(id)).collect();
    let big = "x".repeat(1 << 20);
    let slide = textbox(&[header(1), bytes(&big)]);
    let els = parse(&deck_listing(&list, std::slice::from_ref(&slide), &ids)).unwrap();
    assert_eq!(els.len(), 1);
    assert_eq!(els[0].text.len(), 1 << 20);
}

#[test]
fn notes_slides_that_are_one_are_read_once() {
    // Two slides naming two notes ids the persist directory sends to one
    // notes slide.
    let list = rec(
        true,
        0,
        SLIDE_LIST,
        &[slide_persist(2), slide_persist(3)].concat(),
    );
    let notes_list = rec(
        true,
        2,
        SLIDE_LIST,
        &[notes_persist(4, 256), notes_persist(5, 257)].concat(),
    );
    let mut s = rec(true, 0, DOCUMENT, &[list, notes_list].concat());
    let slide = |n: u32, t: &str| {
        rec(
            true,
            0,
            SLIDE,
            &[slide_atom(n), textbox(&[header(1), chars(t)])].concat(),
        )
    };
    let s1 = s.len() as u32;
    s.extend(slide(256, "One"));
    let s2 = s.len() as u32;
    s.extend(slide(257, "Two"));
    let n = s.len() as u32;
    s.extend(rec(
        true,
        0,
        NOTES,
        &textbox(&[header(2), chars("Say it once.")]),
    ));
    let dir = s.len() as u32;
    s.extend(persist_dir(&[(1, 0), (2, s1), (3, s2), (4, n), (5, n)]));
    let edit = s.len() as u32;
    s.extend(user_edit(0, dir, 1));
    let mut user = vec![0u8; 16];
    user.extend(edit.to_le_bytes());
    let els = parse(&compound(&[
        ("/PowerPoint Document", &s),
        ("/Current User", &user),
    ]))
    .unwrap();
    let said = els.iter().filter(|e| e.text == "Say it once.").count();
    assert_eq!(said, 1, "{els:#?}");
}

#[test]
fn paragraphs_past_the_budget_are_refused_before_they_are_made() {
    use fluree_doc_model::budget::{ELEMENT_BYTES, TEXT_BYTES};
    // A thousand paragraphs, a mark each: an element each.
    let slide = textbox(&[header(4), bytes(&"a\r".repeat(1_000))]);
    let bytes = deck_of(&[], &slide, None);
    // The text, its marks counted at six bytes each, then the elements.
    let budget = |n: u64| Budget::new(n * ELEMENT_BYTES + 7_000 * TEXT_BYTES, 0);
    assert_eq!(
        parse_within(&bytes, &mut budget(1_000)).unwrap().0.len(),
        1_000
    );
    assert!(matches!(
        parse_within(&bytes, &mut budget(999)),
        Err(PptError::TooLarge(ref m)) if m.contains("elements")
    ));
}

#[test]
fn a_picture_shown_again_is_listed_at_each_shape_and_read_once() {
    let slide = [
        picture_shape(1, Some("Logo")),
        picture_shape(1, Some("Logo")),
        picture_shape(2, None),
    ]
    .concat();
    let (els, media) = parse_with_media(&deck_of(&[], &slide, None)).unwrap();
    assert_eq!(els.iter().filter(|e| e.kind == "doco:Figure").count(), 3);
    let parts: Vec<&str> = media.iter().map(|m| m.source.name()).collect();
    assert_eq!(parts, ["Pictures#1", "Pictures#1", "Pictures#2"]);
    // The reader groups them by their image: two pictures, the logo shown
    // by two figures.
    let pictures = fluree_doc_model::pictures(media);
    let figures: Vec<Vec<&str>> = pictures.iter().map(|p| p.figures()).collect();
    assert_eq!(
        figures,
        [vec!["figure-0-1", "figure-0-2"], vec!["figure-0-3"]]
    );
}

#[test]
fn a_text_of_many_paragraphs_and_runs_finds_each_run_once() {
    // 200,000 paragraphs over 100,000 runs: a search of the runs from the
    // first for each paragraph is ten billion steps.
    let n = 200_000;
    let t = Text {
        kind: 4,
        text: "a\r".repeat(n),
        paras: (0..100_000)
            .map(|i| ParaRun {
                count: 2,
                indent: u16::from(i % 2 == 1),
                bullet: None,
            })
            .collect(),
    };
    let p = paragraphs(&t, &mut Budget::default()).unwrap();
    assert_eq!(p.len(), n);
    assert!(!p[0].1 && p[1].1 && !p[2].1);
    assert!(!p[n - 1].1);
}

/// A table group: its first shape saying it is one, then `cells`.
fn table_group(cells: &[Vec<u8>]) -> Vec<u8> {
    rec(
        true,
        0,
        GROUP,
        &[
            rec(
                true,
                0,
                SHAPE,
                &opt(TERTIARY_OPT, &[(TABLE_PROPERTIES, 0xE)]),
            ),
            cells.concat(),
        ]
        .concat(),
    )
}

#[test]
fn cells_that_overlap_across_the_grid_are_read_as_text() {
    // 1,000 cells, each from its own corner to the far one: a grid of a
    // million, each cell marking most of it.
    let cells: Vec<Vec<u8>> = (0..1_000).map(|i| cell(i, i, 2_000, 2_000, "c")).collect();
    let group = table_group(&cells);
    let r = record(&group, 0).unwrap();
    assert!(table(&group, &r, 0, &mut Budget::default())
        .unwrap()
        .is_none());
}

#[test]
fn a_tables_cell_costs_what_a_sheets_does() {
    use fluree_doc_model::budget::{Output, CELL_BYTES, FLAT_CELL_BYTES, TEXT_BYTES};
    // A row of 100 cells of one digit: each written once in its row, at a
    // sheet's cost for Markdown, its slot and text apart, where an element
    // each (2 KiB) would not fit; a node of its own each (4 KiB) for DoCO.
    let cells: Vec<Vec<u8>> = (0..100)
        .map(|c| cell(c * 10, 0, c * 10 + 10, 10, "1"))
        .collect();
    let group = table_group(&cells);
    let r = record(&group, 0).unwrap();
    let memory = 100 * (CELL_BYTES + FLAT_CELL_BYTES + 16 * TEXT_BYTES);
    let read = |output| {
        table(
            &group,
            &r,
            0,
            &mut Budget::new(memory, 0).for_output(output),
        )
    };
    let t = read(Output::Flat).unwrap().unwrap();
    assert_eq!(t.rows[0].len(), 100);
    assert!(read(Output::Doco).is_err());
}

#[test]
fn an_empty_cell_costs_its_slot_only() {
    use fluree_doc_model::budget::{Output, CELL_BYTES, FLAT_CELL_BYTES, TEXT_BYTES};
    // A row of 100 cells, every other one empty: a cell that holds text
    // each of the 50 others, as a `.doc` table's.
    let cells: Vec<Vec<u8>> = (0..100)
        .map(|c| {
            cell(
                c * 10,
                0,
                c * 10 + 10,
                10,
                if c % 2 == 0 { "1" } else { "" },
            )
        })
        .collect();
    let group = table_group(&cells);
    let r = record(&group, 0).unwrap();
    let memory = 100 * CELL_BYTES + 50 * (FLAT_CELL_BYTES + 16 * TEXT_BYTES);
    let t = table(
        &group,
        &r,
        0,
        &mut Budget::new(memory, 0).for_output(Output::Flat),
    )
    .unwrap()
    .unwrap();
    assert_eq!(t.rows[0].iter().filter(|c| c.is_empty()).count(), 50);
}

#[test]
fn a_group_that_is_no_table_is_read_once_however_deep() {
    // Sixteen groups, each inside a cell of the one above, each claiming to
    // be a table of 1,025 cells down a diagonal, a grid past the bound: a
    // group read once as a table and again as text, at each level, is read
    // 65,536 times at the deepest.
    let diagonal: Vec<Vec<u8>> = (0..1_025).map(|i| cell(i, i, i + 1, i + 1, "")).collect();
    let mut inner = table_group(&[cell(0, 0, 1, 1, "deepest")]);
    for _ in 0..16 {
        let holder = rec(
            true,
            0,
            SHAPE,
            &[
                rec(false, 1, SHAPE_HEADER, &[0; 8]),
                rec(
                    false,
                    0,
                    CHILD_ANCHOR,
                    &[5_000i32, 5_000, 5_001, 5_001]
                        .iter()
                        .flat_map(|v| v.to_le_bytes())
                        .collect::<Vec<u8>>(),
                ),
                inner,
            ]
            .concat(),
        );
        let mut cells = diagonal.clone();
        cells.push(holder);
        inner = table_group(&cells);
    }
    let slide = rec(true, 0, SLIDE, &inner);
    let r = record(&slide, 0).unwrap();
    let mut out = Vec::new();
    texts(
        &slide,
        &r,
        0,
        (&mut 4, false),
        &mut Budget::default(),
        &mut out,
    )
    .unwrap();
    assert!(out.iter().any(|i| matches!(i, Item::Table(_))));
}

#[test]
fn what_doco_copies_of_a_table_is_spent_before_it_is_kept() {
    // A long first cell labels each other cell of its row as its row
    // header: a hundred copies of it that the file states once.
    let long = "w".repeat(100_000);
    let mut cells: Vec<Vec<u8>> = (0..101)
        .map(|c| cell(c * 10, 0, c * 10 + 10, 10, "H"))
        .collect();
    cells.push(cell(0, 10, 10, 20, &long));
    cells.extend((1..101).map(|c| cell(c * 10, 10, c * 10 + 10, 20, "1")));
    let slide = rec(true, 0, 0x040C, &table_group(&cells));
    let bytes = deck_of(&[], &slide, None);
    // Room for the table and its text, which a budget of 20 MiB is many
    // times over, but not for 10 MB of copies at 12 bytes a byte.
    let err = parse_within(&bytes, &mut Budget::new(20 << 20, 1 << 20)).unwrap_err();
    assert!(err.to_string().contains("copies"), "{err}");
    let (els, _) = parse_within(&bytes, &mut Budget::default()).unwrap();
    assert!(els.iter().any(|e| e.kind == "doco:Table"));
}
