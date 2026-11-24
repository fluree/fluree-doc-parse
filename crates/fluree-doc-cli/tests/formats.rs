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

/// A one-page PDF of these objects after its catalog (object 1), page tree
/// (2) and page (3), whose content (4) sets one line of text.
fn pdf_with(page_extra: &str, catalog_extra: &str, more: &[Vec<u8>]) -> Vec<u8> {
    let content = "BT /F1 12 Tf 72 700 Td (Quarterly report) Tj ET";
    let mut objects = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>").into_bytes(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 \
             << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> /Contents 4 0 R {page_extra} >>"
        )
        .into_bytes(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()).into_bytes(),
    ];
    objects.extend(more.iter().cloned());
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).into_bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).into_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").into_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .into_bytes(),
    );
    out
}

#[test]
fn a_pdfs_notes_and_embedded_files_reach_every_output() {
    let dir = scratch("pdf-annots");
    let csv = b"Customer,Amount\nAlex Sample,12.50\n";
    let pdf = pdf_with(
        "/Annots [<< /Type /Annot /Subtype /Text /Rect [500 700 520 720] /T (Sam Example) \
         /Contents (Check this total) >>]",
        "/Names << /EmbeddedFiles << /Names [(orders) 5 0 R] >> >>",
        &[
            b"<< /Type /Filespec /F (orders.csv) /EF << /F 6 0 R >> >>".to_vec(),
            [
                format!(
                    "<< /Type /EmbeddedFile /Subtype /text#2Fcsv /Length {} >>\nstream\n",
                    csv.len()
                )
                .into_bytes(),
                csv.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ],
    );
    let input = dir.join("report.pdf");
    std::fs::write(&input, pdf).unwrap();
    let out = every_format(&dir, &input);
    assert_parity(
        &out,
        "doco:Paragraph",
        &["Quarterly report", "Sam Example: Check this total"],
    );
    assert!(out[1].1.contains("\"annotation\""), "{}", out[1].1);
    // The embedded file is described on the document node.
    let doco: serde_json::Value = serde_json::from_str(&out[2].1).unwrap();
    let mut described = String::new();
    walk(&doco, &mut |n| {
        if let Some(a) = n.get("doc:attachments") {
            described = a.to_string();
        }
    });
    assert!(described.contains("orders.csv"), "{}", out[2].1);
    assert!(described.contains("text/csv"), "{described}");
    // Saved under `--attachments`, else named on stderr.
    let o = convert(&dir, &input, "md", &[]);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("1 embedded file not converted (orders.csv)"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let att = dir.join("att");
    let o = convert(
        &dir,
        &input,
        "md",
        &["--attachments", att.to_str().unwrap()],
    );
    assert!(o.status.success());
    assert_eq!(
        std::fs::read(att.join("report").join("orders.csv")).unwrap(),
        csv
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_emails_attachments_are_converted_on_request_once_each() {
    let dir = scratch("email-children");
    let part = |name: &str, ty: &str, body: &str| {
        format!(
            "--b\r\nContent-Type: {ty}\r\nContent-Disposition: attachment; filename=\"{name}\"\r\n\r\n{body}\r\n"
        )
    };
    let inner = "From: Sam Example <sam@example.com>\r\nTo: Alex Sample <alex@example.com>\r\n\
                 Subject: Codes\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"c\"\r\n\r\n\
                 --c\r\nContent-Type: text/plain\r\n\r\nThe codes.\r\n\
                 --c\r\nContent-Type: text/csv\r\nContent-Disposition: attachment; filename=\"codes.csv\"\r\n\r\n\
                 Code,Label\r\nA1,First\r\n--c--\r\n";
    let mail = format!(
        "From: Alex Sample <alex@example.com>\r\nTo: Sam Example <sam@example.com>\r\n\
         Subject: Orders\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
         --b\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n{}{}{}--b--\r\n",
        part("orders.csv", "text/csv", "Customer,Amount\r\nAlex Sample,12.50"),
        part("fwd.eml", "message/rfc822", inner),
        part("same.csv", "text/csv", "Customer,Amount\r\nAlex Sample,12.50"),
    );
    let input = dir.join("mail.eml");
    std::fs::write(&input, mail).unwrap();
    let att = dir.join("att");
    let o = convert(
        &dir,
        &input,
        "md",
        &[
            "--attachments",
            att.to_str().unwrap(),
            "--parse-attachments",
        ],
    );
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(err.contains("3 of 3 attached file(s) converted"), "{err}");
    // The attached message's own file was converted by this run: nothing
    // says it was left out, or asks for the directory the run was given.
    assert!(!err.contains("not converted"), "{err}");
    let mut outputs: Vec<String> = std::fs::read_dir(att.join("mail"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect();
    outputs.sort();
    assert_eq!(outputs.len(), 3, "{outputs:?}");
    assert!(outputs
        .iter()
        .any(|o| o.contains("Alex Sample") && o.contains("12.50")));
    assert!(outputs
        .iter()
        .any(|o| o.contains("A1") && o.contains("First")));
    assert!(outputs.iter().any(|o| o.contains("The codes.")));
    // Without --attachments it is refused.
    let o = convert(&dir, &input, "md", &["--parse-attachments"]);
    assert!(!o.status.success());
    let _ = std::fs::remove_dir_all(dir);
}

/// A two-page XPS document: a page of text, then a page that is only a
/// picture, as a screen printed to XPS is.
fn xps() -> Vec<u8> {
    use std::io::Write;
    let ns = "http://schemas.microsoft.com/xps/2005/06";
    let glyphs = |y: f64, em: f64, text: &str| {
        let indices: Vec<&str> = text
            .chars()
            .map(|c| if c == ' ' { ",25" } else { ",55" })
            .collect();
        format!(
            "<Glyphs OriginX=\"96\" OriginY=\"{y}\" FontRenderingEmSize=\"{em}\" UnicodeString=\"{text}\" Indices=\"{}\"/>",
            indices.join(";")
        )
    };
    let mut png = Vec::new();
    image::GrayImage::from_pixel(160, 200, image::Luma([255u8]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let parts: Vec<(&str, Vec<u8>)> = vec![
        (
            "_rels/.rels",
            format!(
                "<Relationships><Relationship Id=\"R0\" Type=\"{ns}/fixedrepresentation\" Target=\"/seq.fdseq\"/></Relationships>"
            )
            .into_bytes(),
        ),
        (
            "seq.fdseq",
            format!("<FixedDocumentSequence xmlns=\"{ns}\"><DocumentReference Source=\"/Documents/1/doc.fdoc\"/></FixedDocumentSequence>").into_bytes(),
        ),
        (
            "Documents/1/doc.fdoc",
            format!("<FixedDocument xmlns=\"{ns}\"><PageContent Source=\"Pages/1.fpage\"/><PageContent Source=\"Pages/2.fpage\"/></FixedDocument>").into_bytes(),
        ),
        (
            "Documents/1/Pages/1.fpage",
            format!(
                "<FixedPage Width=\"816\" Height=\"1056\" xmlns=\"{ns}\">{}{}</FixedPage>",
                glyphs(120.0, 32.0, "Quarterly report"),
                glyphs(180.0, 16.0, "Alex Sample signed the orders on Monday.")
            )
            .into_bytes(),
        ),
        (
            "Documents/1/Pages/2.fpage",
            format!(
                "<FixedPage Width=\"816\" Height=\"1056\" xmlns=\"{ns}\"><Path Data=\"M 0,0 L 816,0 816,1056 0,1056 Z\">\
                 <Path.Fill><ImageBrush ImageSource=\"../Resources/screen.png\" Viewport=\"0,0,816,1056\" ViewportUnits=\"Absolute\" \
                 Viewbox=\"0,0,160,200\" ViewboxUnits=\"Absolute\"/></Path.Fill></Path></FixedPage>"
            )
            .into_bytes(),
        ),
        ("Documents/1/Resources/screen.png", png),
    ];
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    for (name, body) in &parts {
        z.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(body).unwrap();
    }
    z.finish().unwrap();
    buf.into_inner()
}

/// The value of `key` on the graph's document node, as written.
fn document_value(doco: &str, key: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(doco).unwrap();
    let mut found = String::new();
    walk(&v, &mut |n| {
        if let Some(x) = n.get(key) {
            found = x.to_string();
        }
    });
    found
}

#[test]
fn an_xps_document_reads_as_a_pdf_does_and_its_pictured_page_as_a_scan() {
    let dir = scratch("xps");
    let input = dir.join("report.xps");
    std::fs::write(&input, xps()).unwrap();
    let out = every_format(&dir, &input);
    assert_parity(
        &out,
        "doco:SectionTitle",
        &[
            "Quarterly report",
            "Alex Sample signed the orders on Monday.",
        ],
    );
    let doco = &out[2].1;
    assert!(doco.contains("doc:bbox"), "{doco}");
    // The pictured page is unread, as a scan nothing read is.
    let unread = document_value(doco, "doc:unreadPages");
    assert!(
        unread.contains("\"pageIndex\":1") && unread.contains("Scanned"),
        "{unread}"
    );

    // Words read from it (`--ocr-words`) are its text, laid out there.
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    let reading = serde_json::json!({
        "coords": "page",
        "words": [
            {"text": "Invoice", "box": [72.0, 100.0, 130.0, 114.0], "conf": 0.98},
            {"text": "4417", "box": [136.0, 100.0, 170.0, 114.0], "conf": 0.97}
        ]
    });
    std::fs::write(words.join("report_p1_full.json"), reading.to_string()).unwrap();
    let o = convert(
        &dir,
        &input,
        "doco",
        &["--ocr-words", words.to_str().unwrap()],
    );
    let doco = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(doco.contains("Invoice 4417"), "{doco}");
    assert!(
        !document_value(&doco, "doc:unreadPages").contains("Scanned"),
        "{doco}"
    );

    // A model's reading someone already has (`--tier-results`) reads it too.
    let tiers = dir.join("tiers");
    std::fs::create_dir_all(&tiers).unwrap();
    let page = serde_json::json!([{ "parsing_res_list": [
        { "block_label": "text", "block_content": "Ticket 92 closed by Sam Example", "block_order": 0 }
    ]}]);
    std::fs::write(tiers.join("report_p1_full.json"), page.to_string()).unwrap();
    let o = convert(
        &dir,
        &input,
        "md",
        &["--tier-results", tiers.to_str().unwrap()],
    );
    let md = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(md.contains("Ticket 92 closed by Sam Example"), "{md}");
    assert!(md.contains("Quarterly report"), "{md}");

    // `render-routed` writes the picture under the name its reading takes.
    let crops = dir.join("crops");
    let o = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .args(["dev", "render-routed"])
        .arg(&input)
        .arg(&crops)
        .env("HOME", dir.join("home"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(crops.join("report_p1_full.png").is_file());
    let _ = std::fs::remove_dir_all(dir);
}

/// An XPS document of one portrait page holding only a picture of
/// `width` × `height` pixels, drawn in the box `(x0, y0, x1, y1)` (XPS
/// units), as a screen printed to XPS is.
fn xps_picture(width: u32, height: u32, (x0, y0, x1, y1): (f64, f64, f64, f64)) -> Vec<u8> {
    use std::io::Write;
    let ns = "http://schemas.microsoft.com/xps/2005/06";
    let mut png = Vec::new();
    image::GrayImage::from_pixel(width, height, image::Luma([255u8]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let parts: Vec<(&str, Vec<u8>)> = vec![
        (
            "_rels/.rels",
            format!(
                "<Relationships><Relationship Id=\"R0\" Type=\"{ns}/fixedrepresentation\" Target=\"/seq.fdseq\"/></Relationships>"
            )
            .into_bytes(),
        ),
        (
            "seq.fdseq",
            "<FixedDocumentSequence><DocumentReference Source=\"/doc.fdoc\"/></FixedDocumentSequence>".into(),
        ),
        (
            "doc.fdoc",
            "<FixedDocument><PageContent Source=\"/p0.fpage\"/></FixedDocument>".into(),
        ),
        (
            "p0.fpage",
            format!(
                "<FixedPage Width=\"816\" Height=\"1056\"><Path Data=\"M {x0},{y0} L {x1},{y0} {x1},{y1} {x0},{y1} Z\">\
                 <Path.Fill><ImageBrush ImageSource=\"/img.png\" Viewport=\"{x0},{y0},{},{}\" ViewportUnits=\"Absolute\" \
                 Viewbox=\"0,0,{width},{height}\" ViewboxUnits=\"Absolute\"/></Path.Fill></Path></FixedPage>",
                x1 - x0,
                y1 - y0
            )
            .into_bytes(),
        ),
        ("img.png", png),
    ];
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    for (name, body) in &parts {
        z.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(body).unwrap();
    }
    z.finish().unwrap();
    buf.into_inner()
}

#[test]
fn words_read_from_an_xps_pictures_crop_land_where_it_is_drawn() {
    // The three steps: render-routed writes the picture as it is, an
    // engine reads that image, `--ocr-words` places its words. They land
    // where `--ocr` lays them: in the box the picture is drawn in.
    let dir = scratch("xps-words");
    for (name, (w, h), drawn, at) in [
        // A portrait picture drawn inside the page's margins.
        (
            "inset",
            (612, 792),
            (96.0, 96.0, 708.0, 888.0),
            (72.0, 72.0),
        ),
        // A landscape screenshot printed on a portrait page.
        (
            "landscape",
            (624, 464),
            (0.0, 0.0, 816.0, 606.8),
            (0.0, 0.0),
        ),
    ] {
        let input = dir.join(format!("{name}.xps"));
        std::fs::write(&input, xps_picture(w, h, drawn)).unwrap();
        let crops = dir.join(format!("{name}-crops"));
        let o = Command::new(env!("CARGO_BIN_EXE_fdoc"))
            .args(["dev", "render-routed"])
            .arg(&input)
            .arg(&crops)
            .env("HOME", dir.join("home"))
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let png = image::open(crops.join(format!("{name}_p0_full.png"))).unwrap();
        assert_eq!((png.width(), png.height()), (w, h));
        // The manifest says where the crop is on its page.
        let manifest = std::fs::read_to_string(crops.join("manifest.jsonl")).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(manifest.lines().next().unwrap()).unwrap();
        assert_eq!(record["bbox"][0].as_f64(), Some(drawn.0 * 0.75), "{record}");
        let words = dir.join(format!("{name}-words"));
        std::fs::create_dir_all(&words).unwrap();
        let reading = serde_json::json!({
            "coords": "px", "width": w, "height": h,
            "words": [{"text": "HELLO", "box": [0, 0, w / 2, 40], "conf": 0.98}]
        });
        std::fs::write(
            words.join(format!("{name}_p0_full.json")),
            reading.to_string(),
        )
        .unwrap();
        let o = convert(
            &dir,
            &input,
            "json",
            &["--ocr-words", words.to_str().unwrap()],
        );
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let hello = v
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["text"] == "HELLO")
            .unwrap_or_else(|| panic!("{name}: {out} {}", String::from_utf8_lossy(&o.stderr)));
        // Its left edge where the word's is, its line within the word's
        // box: 40 pixels high, in the picture's scale on the page. A line
        // is sized by its letters, not by the box the engine gave, so it
        // may run a point or so past it.
        let tall = 40.0 * (drawn.3 - drawn.1) * 0.75 / f64::from(h);
        let b = &hello["bbox"];
        let (x0, y0, y1) = (
            b["x0"].as_f64().unwrap(),
            b["y0"].as_f64().unwrap(),
            b["y1"].as_f64().unwrap(),
        );
        assert!(
            (x0 - at.0).abs() < 1.0 && y0 >= at.1 - 2.0 && y1 <= at.1 + tall + 2.0,
            "{name}: {b}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// A Word 97 document of `paragraphs`, its text one Windows-1252 piece
/// after the file information block, in a version 3 compound file.
fn word97(paragraphs: &[&str]) -> Vec<u8> {
    use std::io::Write;
    let text: String = paragraphs.iter().map(|p| format!("{p}\r")).collect();
    let mut wd = vec![0u8; 0x200];
    let fc = wd.len() as u32;
    wd.extend(text.bytes());
    let mut clx = vec![0x02u8];
    let mut plc = Vec::new();
    for cp in [0u32, text.len() as u32] {
        plc.extend(cp.to_le_bytes());
    }
    plc.extend(0u16.to_le_bytes());
    plc.extend(((fc * 2) | 0x4000_0000).to_le_bytes());
    plc.extend(0u16.to_le_bytes());
    clx.extend((plc.len() as u32).to_le_bytes());
    clx.extend(&plc);
    let put = |wd: &mut Vec<u8>, at: usize, v: &[u8]| wd[at..at + v.len()].copy_from_slice(v);
    put(&mut wd, 0, &0xA5ECu16.to_le_bytes());
    put(&mut wd, 2, &0x00C1u16.to_le_bytes());
    put(&mut wd, 0x0A, &0x0200u16.to_le_bytes());
    put(&mut wd, 0x4C, &(text.len() as u32).to_le_bytes());
    put(
        &mut wd,
        0x9A + 8 * 33 + 4,
        &(clx.len() as u32).to_le_bytes(),
    );
    let mut cf =
        cfb::CompoundFile::create_with_version(cfb::Version::V3, std::io::Cursor::new(Vec::new()))
            .unwrap();
    for (name, data) in [("/WordDocument", &wd), ("/1Table", &clx)] {
        cf.create_stream(name).unwrap().write_all(data).unwrap();
    }
    cf.flush().unwrap();
    cf.into_inner().into_inner()
}

#[test]
fn a_word_97_document_reads_in_every_output_whatever_its_name() {
    let dir = scratch("doc");
    let bytes = word97(&["Quarterly report", "Alex Sample signed the orders."]);
    let input = dir.join("memo.doc");
    std::fs::write(&input, &bytes).unwrap();
    let out = every_format(&dir, &input);
    assert_parity(
        &out,
        "doco:Paragraph",
        &["Quarterly report", "Alex Sample signed the orders."],
    );
    // Renamed, it is still read as what it is.
    let renamed = dir.join("memo.bin");
    std::fs::write(&renamed, &bytes).unwrap();
    let o = convert(&dir, &renamed, "md", &[]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("Alex Sample signed"));
    let _ = std::fs::remove_dir_all(dir);
}

/// An OpenDocument package of `kind` whose body is `body`.
fn odf(kind: &str, body: &str) -> Vec<u8> {
    use std::io::Write;
    let ns = "xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" \
              xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\" \
              xmlns:table=\"urn:oasis:names:tc:opendocument:xmlns:table:1.0\" \
              xmlns:draw=\"urn:oasis:names:tc:opendocument:xmlns:drawing:1.0\" \
              xmlns:presentation=\"urn:oasis:names:tc:opendocument:xmlns:presentation:1.0\"";
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file("mimetype", stored).unwrap();
    write!(z, "application/vnd.oasis.opendocument.{kind}").unwrap();
    z.start_file("content.xml", zip::write::SimpleFileOptions::default())
        .unwrap();
    write!(
        z,
        "<office:document-content {ns}><office:body>{body}</office:body></office:document-content>"
    )
    .unwrap();
    z.finish().unwrap();
    buf.into_inner()
}

#[test]
fn opendocument_text_sheets_and_slides_reach_every_output() {
    let dir = scratch("odf");
    let cases = [
        (
            "memo.odt",
            odf(
                "text",
                "<office:text><text:h text:outline-level=\"1\">Orders</text:h>\
                 <text:p>Alex Sample signed the orders.</text:p></office:text>",
            ),
            "doco:SectionTitle",
            vec!["Orders", "Alex Sample signed the orders."],
        ),
        (
            "book.ods",
            odf(
                "spreadsheet",
                "<office:spreadsheet><table:table table:name=\"Orders\">\
                 <table:table-row><table:table-cell><text:p>Customer</text:p></table:table-cell>\
                 <table:table-cell><text:p>Amount</text:p></table:table-cell></table:table-row>\
                 <table:table-row><table:table-cell><text:p>Alex Sample</text:p></table:table-cell>\
                 <table:table-cell office:value-type=\"float\" office:value=\"12.5\"><text:p>12.50</text:p></table:table-cell></table:table-row>\
                 </table:table></office:spreadsheet>",
            ),
            "doco:Table",
            vec!["Orders", "Alex Sample", "12.50"],
        ),
        (
            "deck.odp",
            odf(
                "presentation",
                "<office:presentation><draw:page><draw:frame presentation:class=\"title\"><draw:text-box>\
                 <text:p>Quarterly review</text:p></draw:text-box></draw:frame><draw:frame><draw:text-box>\
                 <text:p>Sam Example presents.</text:p></draw:text-box></draw:frame></draw:page></office:presentation>",
            ),
            "doco:SectionTitle",
            vec!["Quarterly review", "Sam Example presents."],
        ),
    ];
    for (name, bytes, kind, facts) in &cases {
        let input = dir.join(name);
        std::fs::write(&input, bytes).unwrap();
        assert_parity(&every_format(&dir, &input), kind, facts);
    }
    // On stdin, by its mimetype.
    let mut child = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .args(["convert", "--no-escalate", "-"])
        .env("HOME", dir.join("home"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(&cases[1].1).unwrap();
    }
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("Alex Sample"));
    let _ = std::fs::remove_dir_all(dir);
}

/// A one-sheet Excel binary workbook of inline strings and numbers, by row.
fn xlsb(rows: &[&[&str]]) -> Vec<u8> {
    use std::io::Write;
    let rec = |typ: u16, data: &[u8]| {
        let mut v = if typ >= 0x80 {
            vec![(typ & 0x7F) as u8 | 0x80, (typ >> 7) as u8]
        } else {
            vec![typ as u8]
        };
        assert!(data.len() < 0x80);
        v.push(data.len() as u8);
        v.extend(data);
        v
    };
    let wide = |s: &str| {
        let mut v = (s.encode_utf16().count() as u32).to_le_bytes().to_vec();
        v.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
        v
    };
    let mut bundle = [0u32, 1]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<u8>>();
    bundle.extend(wide("rId1"));
    bundle.extend(wide("Orders"));
    let workbook = [rec(0x9C, &bundle), rec(0x90, &[]), rec(0x9D, &[0; 4])].concat();
    let mut sheet = [rec(0x94, &[0; 16]), rec(0x91, &[])].concat();
    for (r, row) in rows.iter().enumerate() {
        sheet.extend(rec(
            0x00,
            &[(r as u32).to_le_bytes().to_vec(), vec![0; 13]].concat(),
        ));
        for (c, v) in row.iter().enumerate() {
            let head = [(c as u32).to_le_bytes(), 0u32.to_le_bytes()].concat();
            sheet.extend(match v.parse::<f64>() {
                Ok(n) => rec(0x05, &[head, n.to_le_bytes().to_vec()].concat()),
                Err(_) => rec(0x06, &[head, wide(v)].concat()),
            });
        }
    }
    sheet.extend(rec(0x92, &[]));
    let rels = "<Relationships><Relationship Id=\"rId1\" Target=\"worksheets/sheet1.bin\"/></Relationships>";
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    for (name, data) in [
        ("xl/workbook.bin", workbook),
        ("xl/_rels/workbook.bin.rels", rels.as_bytes().to_vec()),
        ("xl/worksheets/sheet1.bin", sheet),
    ] {
        z.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(&data).unwrap();
    }
    z.finish().unwrap();
    buf.into_inner()
}

#[test]
fn an_xlsb_workbook_reads_in_every_output_whatever_its_name() {
    let dir = scratch("xlsb");
    let book = xlsb(&[
        &["Customer", "Amount"],
        &["Alex Sample", "12.5"],
        &["Sam", "7"],
    ]);
    let input = dir.join("orders.xlsb");
    std::fs::write(&input, &book).unwrap();
    let out = every_format(&dir, &input);
    assert_parity(&out, "doco:Table", &["Orders", "Alex Sample", "12.5"]);
    assert!(doc_has_cell(&out[2].1, "Alex Sample"));
    let renamed = dir.join("orders.bin");
    std::fs::write(&renamed, &book).unwrap();
    let o = convert(&dir, &renamed, "md", &[]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("Alex Sample"));
    let _ = std::fs::remove_dir_all(dir);
}

/// A one-slide PowerPoint 97 deck: a title and a line of body text, in a
/// shape's text box.
fn ppt() -> Vec<u8> {
    use std::io::Write;
    let rec = |container: bool, typ: u16, data: &[u8]| {
        let mut v = (if container { 0x0Fu16 } else { 0 }).to_le_bytes().to_vec();
        v.extend(typ.to_le_bytes());
        v.extend((data.len() as u32).to_le_bytes());
        v.extend(data);
        v
    };
    let text = |kind: u32, s: &str| {
        [
            rec(false, 0x0F9F, &kind.to_le_bytes()),
            rec(
                false,
                0x0FA0,
                &s.encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<u8>>(),
            ),
        ]
        .concat()
    };
    let persist = rec(
        false,
        0x03F3,
        &[2u32.to_le_bytes().to_vec(), vec![0; 16]].concat(),
    );
    let document = rec(true, 0x03E8, &rec(true, 0x0FF0, &persist));
    let slide = rec(
        true,
        0x03EE,
        &rec(
            true,
            0xF00D,
            &[
                text(0, "Quarterly review"),
                text(1, "Alex Sample presents."),
            ]
            .concat(),
        ),
    );
    let mut s = document.clone();
    let slide_at = s.len() as u32;
    s.extend(&slide);
    let dir_at = s.len() as u32;
    let mut dir = Vec::new();
    for (id, off) in [(1u32, 0u32), (2, slide_at)] {
        dir.extend(((1u32 << 20) | id).to_le_bytes());
        dir.extend(off.to_le_bytes());
    }
    s.extend(rec(false, 0x1772, &dir));
    let edit_at = s.len() as u32;
    let mut edit = vec![0u8; 8];
    for v in [0u32, dir_at, 1] {
        edit.extend(v.to_le_bytes());
    }
    s.extend(rec(false, 0x0FF5, &edit));
    let mut user = vec![0u8; 16];
    user.extend(edit_at.to_le_bytes());
    let mut cf =
        cfb::CompoundFile::create_with_version(cfb::Version::V3, std::io::Cursor::new(Vec::new()))
            .unwrap();
    for (name, data) in [("/PowerPoint Document", &s), ("/Current User", &user)] {
        cf.create_stream(name).unwrap().write_all(data).unwrap();
    }
    cf.flush().unwrap();
    cf.into_inner().into_inner()
}

#[test]
fn a_powerpoint_97_deck_reads_in_every_output_whatever_its_name() {
    let dir = scratch("ppt");
    let input = dir.join("deck.ppt");
    std::fs::write(&input, ppt()).unwrap();
    let out = every_format(&dir, &input);
    assert_parity(
        &out,
        "doco:SectionTitle",
        &["Quarterly review", "Alex Sample presents."],
    );
    let renamed = dir.join("deck.bin");
    std::fs::write(&renamed, ppt()).unwrap();
    let o = convert(&dir, &renamed, "md", &[]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("Alex Sample presents."));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_rtf_document_reads_in_every_output_whatever_its_name() {
    let dir = scratch("rtf");
    let rtf = "{\\rtf1\\ansi{\\stylesheet{\\s1\\outlinelevel0 heading 1;}}\\pard\\s1 Orders\\par
\\pard Alex Sample signed the orders.\\par
\\trowd\\cellx3000\\pard\\intbl Customer\\cell Amount\\cell\\row
\\trowd\\cellx3000\\pard\\intbl Sam\\cell 7\\cell\\row}";
    let input = dir.join("memo.rtf");
    std::fs::write(&input, rtf).unwrap();
    let out = every_format(&dir, &input);
    assert_parity(
        &out,
        "doco:SectionTitle",
        &["Orders", "Alex Sample signed the orders.", "Customer"],
    );
    assert!(doc_has_cell(&out[2].1, "Sam"));
    // Saved as `.doc`, as Word and Outlook often do, it is still RTF.
    let renamed = dir.join("memo.doc");
    std::fs::write(&renamed, rtf).unwrap();
    let o = convert(&dir, &renamed, "md", &[]);
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("Alex Sample signed"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A one-slide PowerPoint 97 deck: a title, then a pasted screenshot, a
/// picture shape showing the store's first entry, its PNG in the
/// `Pictures` stream.
fn ppt_with_picture(png: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let rec = |container: bool, inst: u16, typ: u16, data: &[u8]| {
        let mut v = ((inst << 4) | if container { 0x0F } else { 0 })
            .to_le_bytes()
            .to_vec();
        v.extend(typ.to_le_bytes());
        v.extend((data.len() as u32).to_le_bytes());
        v.extend(data);
        v
    };
    let mut fbse = vec![6u8, 6];
    fbse.extend([0u8; 26]);
    fbse.extend(0u32.to_le_bytes());
    fbse.extend([0u8; 4]);
    let store = rec(
        true,
        0,
        0x040B,
        &rec(
            true,
            0,
            0xF000,
            &rec(true, 0, 0xF001, &rec(false, 6, 0xF007, &fbse)),
        ),
    );
    let persist = rec(
        false,
        0,
        0x03F3,
        &[2u32.to_le_bytes().to_vec(), vec![0; 16]].concat(),
    );
    let document = rec(
        true,
        0,
        0x03E8,
        &[rec(true, 0, 0x0FF0, &persist), store].concat(),
    );
    let title = rec(
        true,
        0,
        0xF00D,
        &[
            rec(false, 0, 0x0F9F, &0u32.to_le_bytes()),
            rec(
                false,
                0,
                0x0FA0,
                &"Screens"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<u8>>(),
            ),
        ]
        .concat(),
    );
    let mut opt = (0x0104u16 | 0x4000).to_le_bytes().to_vec();
    opt.extend(1u32.to_le_bytes());
    let picture = rec(
        true,
        0,
        0xF004,
        &[rec(false, 75, 0xF00A, &[0; 8]), rec(false, 1, 0xF00B, &opt)].concat(),
    );
    let slide = rec(true, 0, 0x03EE, &[title, picture].concat());
    let mut s = document.clone();
    let slide_at = s.len() as u32;
    s.extend(&slide);
    let dir_at = s.len() as u32;
    let mut dir = Vec::new();
    for (id, off) in [(1u32, 0u32), (2, slide_at)] {
        dir.extend(((1u32 << 20) | id).to_le_bytes());
        dir.extend(off.to_le_bytes());
    }
    s.extend(rec(false, 0, 0x1772, &dir));
    let edit_at = s.len() as u32;
    let mut edit = vec![0u8; 8];
    for v in [0u32, dir_at, 1] {
        edit.extend(v.to_le_bytes());
    }
    s.extend(rec(false, 0, 0x0FF5, &edit));
    let mut user = vec![0u8; 16];
    user.extend(edit_at.to_le_bytes());
    let blip = rec(
        false,
        0x6E0,
        0xF01E,
        &[vec![0u8; 17], png.to_vec()].concat(),
    );
    let mut cf =
        cfb::CompoundFile::create_with_version(cfb::Version::V3, std::io::Cursor::new(Vec::new()))
            .unwrap();
    for (name, data) in [
        ("/PowerPoint Document", &s),
        ("/Current User", &user),
        ("/Pictures", &blip),
    ] {
        cf.create_stream(name).unwrap().write_all(data).unwrap();
    }
    cf.flush().unwrap();
    cf.into_inner().into_inner()
}

#[test]
fn a_screenshot_pasted_in_a_ppt_is_read_as_a_docx_ones_is() {
    let dir = scratch("ppt-picture");
    let mut png = Vec::new();
    // Lines of dark marks on white: text to the probe a picture is read by.
    image::GrayImage::from_fn(120, 40, |x, y| {
        let ink = (y / 4) % 3 == 0 && (x / 3) % 2 == 0;
        image::Luma([if ink { 20 } else { 250 }])
    })
    .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
    .unwrap();
    let input = dir.join("deck.ppt");
    std::fs::write(&input, ppt_with_picture(&png)).unwrap();
    // Nothing to read it: a figure, listed unread.
    let o = convert(&dir, &input, "doco", &[]);
    let doco = String::from_utf8_lossy(&o.stdout).into_owned();
    let unread = document_value(&doco, "doc:unreadImages");
    assert!(
        unread.contains("figure-0-1") && unread.contains("NotRead"),
        "{doco}"
    );
    // Words an engine wrote for it (`<stem>_m1.json`): its text, after
    // its figure.
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    let reading = serde_json::json!({
        "coords": "px", "width": 120, "height": 40,
        "words": [
            {"text": "Invoice", "box": [4.0, 10.0, 60.0, 24.0]},
            {"text": "4417", "box": [64.0, 10.0, 100.0, 24.0]}
        ]
    });
    std::fs::write(words.join("deck_m1.json"), reading.to_string()).unwrap();
    let o = convert(
        &dir,
        &input,
        "json",
        &["--ocr-words", words.to_str().unwrap()],
    );
    let json: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let kinds: Vec<(&str, &str)> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["type"].as_str().unwrap(), e["text"].as_str().unwrap()))
        .collect();
    assert_eq!(kinds[0], ("doco:SectionTitle", "Screens"));
    assert_eq!(kinds[1].0, "doco:Figure");
    assert!(kinds[2].1.contains("Invoice 4417"), "{kinds:?}");
    // `render-routed` writes it under the name its reading takes.
    let crops = dir.join("crops");
    let o = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .args(["dev", "render-routed"])
        .arg(&input)
        .arg(&crops)
        .env("HOME", dir.join("home"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(crops.join("deck_m1.png").is_file());
    let _ = std::fs::remove_dir_all(dir);
}
