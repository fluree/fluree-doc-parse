//! `fdoc convert` on the formats read beside PDF and the Office packages:
//! each reaches every output format alike (Markdown, JSON, DoCO, text), so
//! a reader is only as wired as its weakest output.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fdoc-formats-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `fdoc convert <input> -f <format> <args>` with no reader configured.
fn convert(dir: &Path, input: &Path, format: &str, args: &[&str]) -> Output {
    let home = dir.join("home");
    Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .arg("convert")
        .arg(input)
        .args(["-f", format, "--no-escalate"])
        .args(args)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("FDOC_HOME", &home)
        .env_remove("FDOC_OCR_WORDS")
        .env_remove("FDOC_TIER_RESULTS")
        .output()
        .unwrap()
}

/// The input converted to each output format, which must all succeed.
fn every_format(dir: &Path, input: &Path) -> [(String, String); 4] {
    ["md", "json", "doco", "text"].map(|f| {
        let o = convert(dir, input, f, &[]);
        assert!(
            o.status.success(),
            "{f}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        (f.to_string(), String::from_utf8(o.stdout).unwrap())
    })
}

/// Every output holds each of `facts`; the JSON and the graph type the
/// element `kind`.
fn assert_parity(outputs: &[(String, String)], kind: &str, facts: &[&str]) {
    for (format, out) in outputs {
        for fact in facts {
            assert!(out.contains(fact), "{format} lacks {fact:?}:\n{out}");
        }
        if matches!(format.as_str(), "json" | "doco") {
            assert!(out.contains(kind), "{format} lacks {kind}:\n{out}");
        }
    }
}

#[test]
fn a_csv_and_a_tsv_are_tables_in_every_output() {
    let dir = scratch("csv");
    let csv = dir.join("orders.csv");
    std::fs::write(
        &csv,
        "Customer;Amount\r\nAlex Sample;12,50\r\nSam Example;7,00\r\n",
    )
    .unwrap();
    let out = every_format(&dir, &csv);
    assert_parity(&out, "doco:Table", &["Alex Sample", "12,50", "Sam Example"]);
    let doco = &out[2].1;
    assert!(doc_has_cell(doco, "Alex Sample"), "{doco}");
    // A `.tsv` is split on tabs, whatever else its fields hold: here as
    // many commas on each line, which a sniff would take.
    let tsv = dir.join("codes.tsv");
    std::fs::write(
        &tsv,
        "Code, short\tLabel, long\nA,1\tFirst, then\nB,2\tSecond, too\n",
    )
    .unwrap();
    let json = &every_format(&dir, &tsv)[1].1;
    assert!(json.contains("\"A,1\""), "{json}");
    assert!(json.contains("\"First, then\""), "{json}");
    let _ = std::fs::remove_dir_all(dir);
}

/// The graph has a cell node whose value is `text`.
fn doc_has_cell(doco: &str, text: &str) -> bool {
    let v: serde_json::Value = serde_json::from_str(doco).unwrap();
    let mut found = false;
    walk(&v, &mut |n| {
        if n.get("doc:cellValue").and_then(|c| c.as_str()) == Some(text) {
            found = true;
        }
    });
    found
}

fn walk(v: &serde_json::Value, f: &mut dyn FnMut(&serde_json::Value)) {
    f(v);
    match v {
        serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, f)),
        serde_json::Value::Object(o) => o.values().for_each(|x| walk(x, f)),
        _ => {}
    }
}

/// One BIFF record.
fn rec(typ: u16, data: &[u8]) -> Vec<u8> {
    [
        &typ.to_le_bytes()[..],
        &(data.len() as u16).to_le_bytes(),
        data,
    ]
    .concat()
}

/// A LABEL record: a string in a cell, written in the record itself.
fn biff_label(row: u16, col: u16, text: &str) -> Vec<u8> {
    let mut d: Vec<u8> = [row, col, 0, text.len() as u16]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    d.push(0);
    d.extend(text.as_bytes());
    rec(0x0204, &d)
}

fn biff_number(row: u16, col: u16, v: f64) -> Vec<u8> {
    let mut d: Vec<u8> = [row, col, 0].iter().flat_map(|x| x.to_le_bytes()).collect();
    d.extend(v.to_le_bytes());
    rec(0x0203, &d)
}

/// A one-sheet BIFF8 workbook of `cells`, in a version 3 compound file as
/// Excel writes one.
fn xls(sheet: &str, cells: &[Vec<u8>]) -> Vec<u8> {
    use std::io::Write;
    let bof = |dt: u16| {
        rec(
            0x0809,
            &[0x0600u16, dt, 0, 0, 0, 0, 0, 0]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<u8>>(),
        )
    };
    let globals = |offset: u32| {
        let mut bs = offset.to_le_bytes().to_vec();
        bs.extend([0, 0, sheet.len() as u8, 0]);
        bs.extend(sheet.as_bytes());
        [bof(0x0005), rec(0x0085, &bs), rec(0x000A, &[])].concat()
    };
    let at = globals(0).len() as u32;
    let stream = [globals(at), bof(0x0010), cells.concat(), rec(0x000A, &[])].concat();
    let mut cf =
        cfb::CompoundFile::create_with_version(cfb::Version::V3, std::io::Cursor::new(Vec::new()))
            .unwrap();
    cf.create_stream("/Workbook")
        .unwrap()
        .write_all(&stream)
        .unwrap();
    cf.flush().unwrap();
    cf.into_inner().into_inner()
}

#[test]
fn an_xls_workbook_reads_in_every_output_whatever_its_name() {
    let dir = scratch("xls");
    let book = xls(
        "Orders",
        &[
            biff_label(0, 0, "Customer"),
            biff_label(0, 1, "Amount"),
            biff_label(1, 0, "Alex Sample"),
            biff_number(1, 1, 12.5),
            biff_label(2, 0, "Sam Example"),
            biff_number(2, 1, 7.0),
        ],
    );
    let input = dir.join("orders.xls");
    std::fs::write(&input, &book).unwrap();
    let out = every_format(&dir, &input);
    assert_parity(&out, "doco:Table", &["Orders", "Alex Sample", "12.5"]);
    assert!(doc_has_cell(&out[2].1, "Sam Example"));
    assert!(out[2].1.contains("doc:cellDatum"), "{}", out[2].1);
    // Renamed, it is still read as what it is.
    let renamed = dir.join("orders.dat");
    std::fs::write(&renamed, &book).unwrap();
    let o = convert(&dir, &renamed, "md", &[]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("Alex Sample"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_word_files_footnotes_and_comments_reach_every_output() {
    use std::io::Write;
    let dir = scratch("docx-notes");
    let ns = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
    let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let parts = [
        (
            "word/document.xml",
            format!(
                "<w:document {ns}><w:body><w:p><w:r><w:t>Yield rose</w:t></w:r>\
                 <w:r><w:footnoteReference w:id=\"1\"/></w:r>\
                 <w:r><w:commentReference w:id=\"0\"/></w:r></w:p></w:body></w:document>"
            ),
        ),
        (
            "word/_rels/document.xml.rels",
            format!(
                "<Relationships><Relationship Id=\"rId1\" Type=\"{rel}/footnotes\" Target=\"footnotes.xml\"/>\
                 <Relationship Id=\"rId2\" Type=\"{rel}/comments\" Target=\"comments.xml\"/></Relationships>"
            ),
        ),
        (
            "word/footnotes.xml",
            format!(
                "<w:footnotes {ns}><w:footnote w:id=\"1\"><w:p><w:r><w:t>Survey of Alex Sample.</w:t></w:r></w:p></w:footnote></w:footnotes>"
            ),
        ),
        (
            "word/comments.xml",
            format!(
                "<w:comments {ns}><w:comment w:id=\"0\" w:author=\"Sam Example\"><w:p><w:r><w:t>Source?</w:t></w:r></w:p></w:comment></w:comments>"
            ),
        ),
    ];
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let opts = zip::write::SimpleFileOptions::default();
    for (name, xml) in &parts {
        z.start_file(*name, opts).unwrap();
        z.write_all(xml.as_bytes()).unwrap();
    }
    z.finish().unwrap();
    let input = dir.join("memo.docx");
    std::fs::write(&input, buf.into_inner()).unwrap();
    let out = every_format(&dir, &input);
    assert_parity(
        &out,
        "doco:Paragraph",
        &[
            "Yield rose",
            "1 Survey of Alex Sample.",
            "Sam Example: Source?",
        ],
    );
    for evidence in ["\"footnote\"", "\"comment\""] {
        assert!(out[1].1.contains(evidence), "{}", out[1].1);
        assert!(out[2].1.contains(evidence), "{}", out[2].1);
    }
    let _ = std::fs::remove_dir_all(dir);
}
