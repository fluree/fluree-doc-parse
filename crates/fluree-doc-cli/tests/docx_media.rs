//! `fdoc convert` on a DOCX with a picture: the words read in it follow its
//! figure. Words come from a sidecar (`--ocr-words`); no engine is run.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

/// A DOCX of a paragraph, a picture (`word/media/image1.png`) and a
/// paragraph.
fn docx() -> Vec<u8> {
    docx_of(1)
}

/// A DOCX of a paragraph, `figures` pictures all drawing one image
/// (`word/media/image1.png`) and a paragraph.
fn docx_of(figures: usize) -> Vec<u8> {
    let png = {
        let img = image::RgbImage::from_pixel(300, 100, image::Rgb([200, 200, 200]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let para = |t: &str| format!("<w:p><w:r><w:t>{t}</w:t></w:r></w:p>");
    let picture = "<w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"1\" name=\"Picture 1\"/><a:graphic><a:graphicData>\
         <pic:pic><pic:blipFill><a:blip r:embed=\"rId7\"/></pic:blipFill></pic:pic></a:graphicData></a:graphic>\
         </wp:inline></w:drawing></w:r></w:p>";
    let body = format!(
        "<w:document xmlns:w=\"w\" xmlns:a=\"a\" xmlns:r=\"r\" xmlns:wp=\"wp\" xmlns:pic=\"pic\"><w:body>{}\
         {}{}</w:body></w:document>",
        para("See the screen below."),
        picture.repeat(figures),
        para("That is all.")
    );
    let parts: Vec<(&str, Vec<u8>)> = vec![
        ("word/document.xml", body.into_bytes()),
        (
            "word/_rels/document.xml.rels",
            format!(
                "<Relationships><Relationship Id=\"rId7\" Type=\"{rel}/image\" \
                 Target=\"media/image1.png\"/></Relationships>"
            )
            .into_bytes(),
        ),
        ("word/media/image1.png", png),
    ];
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let opts = zip::write::SimpleFileOptions::default();
    for (name, bytes) in parts {
        z.start_file(name, opts).unwrap();
        z.write_all(&bytes).unwrap();
    }
    z.finish().unwrap();
    buf.into_inner()
}

fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fdoc-docx-media-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_pictures_words_follow_its_figure() {
    let dir = scratch("words");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx()).unwrap();
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    let reading = serde_json::json!({
        "width": 300, "height": 100,
        "words": [{ "text": "Account 4417-0093-21", "box": [20, 30, 200, 50] }]
    });
    std::fs::write(words.join("report_m1.json"), reading.to_string()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .arg("convert")
        .arg(&input)
        .args(["-f", "text", "--ocr-words"])
        .arg(&words)
        .env("HOME", dir.join("home"))
        .env("XDG_CONFIG_HOME", dir.join("home"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let (before, read, after) = (
        text.find("See the screen below.").unwrap(),
        text.find("Account 4417-0093-21").expect(&text),
        text.find("That is all.").unwrap(),
    );
    assert!(before < read && read < after, "{text}");
    // A malformed word file is reported, not silently skipped.
    std::fs::write(words.join("report_m1.json"), "{ not json").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .arg("convert")
        .arg(&input)
        .args(["-f", "text", "--ocr-words"])
        .arg(&words)
        .env("HOME", dir.join("home"))
        .env("XDG_CONFIG_HOME", dir.join("home"))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("warning: report_m1"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_picture_read_with_no_text_is_not_counted_unread() {
    let dir = scratch("empty-reading");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx()).unwrap();
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    std::fs::write(
        words.join("report_m1.json"),
        r#"{"width":300,"height":100,"words":[]}"#,
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .arg("convert")
        .arg(&input)
        .args(["-f", "text", "--ocr-words"])
        .arg(&words)
        .env("FDOC_HOME", dir.join("home"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("were not read"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn render_routed_crops_a_docx_picture_as_media() {
    let dir = scratch("render");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx()).unwrap();
    let crops = dir.join("crops");
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .args(["dev", "render-routed"])
        .arg(&input)
        .arg(&crops)
        .output()
        .unwrap();
    assert!(out.status.success());
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("1 embedded images"), "{said}");
    let manifest = std::fs::read_to_string(crops.join("manifest.jsonl")).unwrap();
    let rec: serde_json::Value = serde_json::from_str(manifest.lines().next().unwrap()).unwrap();
    assert_eq!(rec["kind"], "media");
    assert_eq!(rec["png"], "report_m1.png");
    assert_eq!(rec["figure"], "figure-0-1");
    assert_eq!(rec["figures"], serde_json::json!(["figure-0-1"]));
    assert_eq!(rec["part"], "word/media/image1.png");
    assert_eq!(rec["reason"], "embedded_image");
    assert!(crops.join("report_m1.png").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_image_drawn_by_fifty_figures_is_cropped_and_read_once() {
    let dir = scratch("fifty");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx_of(50)).unwrap();
    // One crop, naming every figure drawing it.
    let crops = dir.join("crops");
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .args(["dev", "render-routed"])
        .arg(&input)
        .arg(&crops)
        .output()
        .unwrap();
    assert!(out.status.success());
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("1 embedded images"), "{said}");
    let manifest = std::fs::read_to_string(crops.join("manifest.jsonl")).unwrap();
    let recs: Vec<serde_json::Value> = manifest
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(recs.len(), 1, "{manifest}");
    let figures: Vec<String> = (1..=50).map(|i| format!("figure-0-{i}")).collect();
    assert_eq!(recs[0]["figures"], serde_json::json!(figures));
    let pngs = std::fs::read_dir(&crops)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "png")
        })
        .count();
    assert_eq!(pngs, 1);
    // One reading, the first figure's, placed after each of them.
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    let reading = serde_json::json!({
        "width": 300, "height": 100,
        "words": [{ "text": "Account 4417-0093-21", "box": [20, 30, 200, 50] }]
    });
    std::fs::write(words.join("report_m1.json"), reading.to_string()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .arg("convert")
        .arg(&input)
        .args(["-f", "text", "--ocr-words"])
        .arg(&words)
        .env("FDOC_HOME", dir.join("home"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(text.matches("Account 4417-0093-21").count(), 50, "{text}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("were not read"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

/// The document node of a DoCO graph.
fn document(doco: &str) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(doco).unwrap();
    v["@graph"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["@type"] == "foaf:Document")
        .unwrap()
        .clone()
}

#[test]
fn a_picture_nothing_read_is_listed_unread_in_the_graph() {
    let dir = scratch("unread");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx()).unwrap();
    let convert = |extra: &[&std::ffi::OsStr]| {
        Command::new(env!("CARGO_BIN_EXE_fdoc"))
            .arg("convert")
            .arg(&input)
            .args(["-f", "doco"])
            .args(extra)
            .env("FDOC_HOME", dir.join("home"))
            .env_remove("FDOC_OCR_WORDS")
            .output()
            .unwrap()
    };
    // Nothing asked to read it.
    let out = convert(&[]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("1 of 1 image(s)"), "{err}");
    let doc = document(&String::from_utf8(out.stdout).unwrap());
    assert_eq!(
        doc["doc:unreadImages"]["@value"],
        serde_json::json!([{
            "figure": "figure-0-1", "image": "m1",
            "part": "word/media/image1.png", "reason": "NotRead"
        }])
    );
    // A word directory with no reading of it: still not read.
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    let out = convert(&["--ocr-words".as_ref(), words.as_os_str()]);
    let doc = document(&String::from_utf8(out.stdout).unwrap());
    assert_eq!(doc["doc:unreadImages"]["@value"][0]["reason"], "NotRead");
    // A reading that fails is listed as failed.
    std::fs::write(words.join("report_m1.json"), "{ not json").unwrap();
    let out = convert(&["--ocr-words".as_ref(), words.as_os_str()]);
    let doc = document(&String::from_utf8(out.stdout).unwrap());
    assert_eq!(doc["doc:unreadImages"]["@value"][0]["reason"], "Failed");
    // Read, it is no longer listed.
    let reading = serde_json::json!({
        "width": 300, "height": 100,
        "words": [{ "text": "Account 4417-0093-21", "box": [20, 30, 200, 50] }]
    });
    std::fs::write(words.join("report_m1.json"), reading.to_string()).unwrap();
    let out = convert(&["--ocr-words".as_ref(), words.as_os_str()]);
    let doc = document(&String::from_utf8(out.stdout).unwrap());
    assert!(doc.get("doc:unreadImages").is_none(), "{doc}");
    let _ = std::fs::remove_dir_all(dir);
}
