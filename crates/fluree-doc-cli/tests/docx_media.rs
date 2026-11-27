//! `fdoc convert` on a DOCX with a picture: the words read in it follow its
//! figure. Words come from a sidecar (`--ocr-words`), readings from
//! `--tier-results`; no engine is run, and no model is called: the model
//! configured has a key it cannot use.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

/// A DOCX of a paragraph, a picture (`word/media/image1.png`) and a
/// paragraph.
fn docx() -> Vec<u8> {
    docx_of(1)
}

/// A DOCX of a paragraph, `figures` pictures all drawing one image
/// (`word/media/image1.png`, a screen of text, [`screen`]) and a paragraph.
fn docx_of(figures: usize) -> Vec<u8> {
    docx_with(figures, "media/image1.png", screen(600, 200))
}

/// A PNG of `w` × `h` pixels: lines of dark marks on white, which look
/// like text to the probe a model's images are chosen by.
fn screen(w: u32, h: u32) -> Vec<u8> {
    let img = image::GrayImage::from_fn(w, h, |x, y| {
        let ink = (y / 4) % 3 == 0 && (x / 3) % 2 == 0;
        image::Luma([if ink { 20 } else { 250 }])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// A DOCX of a paragraph, `images` pictures each drawing an image of its
/// own (`word/media/image<i>.png`, alike in their pixels, [`screen`]) and a
/// paragraph: each read on its own.
fn docx_distinct(images: usize) -> Vec<u8> {
    let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let png = screen(600, 200);
    let picture = |i: usize| {
        format!(
            "<w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"{i}\" name=\"Picture {i}\"/><a:graphic><a:graphicData>\
             <pic:pic><pic:blipFill><a:blip r:embed=\"rId{i}\"/></pic:blipFill></pic:pic></a:graphicData></a:graphic>\
             </wp:inline></w:drawing></w:r></w:p>"
        )
    };
    let body = format!(
        "<w:document xmlns:w=\"w\" xmlns:a=\"a\" xmlns:r=\"r\" xmlns:wp=\"wp\" xmlns:pic=\"pic\"><w:body>\
         <w:p><w:r><w:t>See the screens below.</w:t></w:r></w:p>{}\
         <w:p><w:r><w:t>That is all.</w:t></w:r></w:p></w:body></w:document>",
        (1..=images).map(picture).collect::<String>()
    );
    let rels: String = (1..=images)
        .map(|i| {
            format!(
                "<Relationship Id=\"rId{i}\" Type=\"{rel}/image\" Target=\"media/image{i}.png\"/>"
            )
        })
        .collect();
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let opts = zip::write::SimpleFileOptions::default();
    z.start_file("word/document.xml", opts).unwrap();
    z.write_all(body.as_bytes()).unwrap();
    z.start_file("word/_rels/document.xml.rels", opts).unwrap();
    z.write_all(format!("<Relationships>{rels}</Relationships>").as_bytes())
        .unwrap();
    for i in 1..=images {
        z.start_file(format!("word/media/image{i}.png"), opts)
            .unwrap();
        z.write_all(&png).unwrap();
    }
    z.finish().unwrap();
    buf.into_inner()
}

/// A DOCX of a paragraph, `figures` pictures all drawing the image
/// `word/<name>` holding `image`, and a paragraph.
fn docx_with(figures: usize, name: &str, image: Vec<u8>) -> Vec<u8> {
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
    let image_part = format!("word/{name}");
    let parts: Vec<(&str, Vec<u8>)> = vec![
        ("word/document.xml", body.into_bytes()),
        (
            "word/_rels/document.xml.rels",
            format!(
                "<Relationships><Relationship Id=\"rId7\" Type=\"{rel}/image\" \
                 Target=\"{name}\"/></Relationships>"
            )
            .into_bytes(),
        ),
        (image_part.as_str(), image),
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
fn a_pictures_reading_is_reported_by_its_name() {
    let dir = scratch("confidence");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx()).unwrap();
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    let reading = serde_json::json!({
        "width": 300, "height": 100,
        "words": [
            { "text": "Account 4417-0093-21", "box": [20, 30, 200, 50], "conf": 0.9 },
            // Tesseract's row of a block, with no text: no word of it.
            { "text": "", "box": [20, 30, 200, 50], "conf": -1 }
        ]
    });
    std::fs::write(words.join("report_m1.json"), reading.to_string()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .arg("convert")
        .arg(&input)
        .args(["-f", "doco", "--ocr-words"])
        .arg(&words)
        .env("FDOC_HOME", dir.join("home"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let doc = v["@graph"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["@type"] == "foaf:Document")
        .unwrap();
    // A DOCX has no pages: the picture goes by the name its words do.
    assert_eq!(
        doc["doc:ocrReadings"]["@value"],
        serde_json::json!([{ "image": "m1", "crop": "full", "words": 1, "meanConfidence": 0.9, "minConfidence": 0.9 }]),
        "{doc}"
    );
    // Scored 0-100, as Tesseract scores: no confidence, and a word of why.
    let reading = reading.to_string().replace("0.9", "90");
    std::fs::write(words.join("report_m1.json"), reading).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .arg("convert")
        .arg(&input)
        .args(["-f", "doco", "--ocr-words"])
        .arg(&words)
        .env("FDOC_HOME", dir.join("home"))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("report_m1.json: 1 of 1 word score(s) outside 0-1, so no confidence"),
        "{err}"
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let doc = v["@graph"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["@type"] == "foaf:Document")
        .unwrap();
    assert_eq!(
        doc["doc:ocrReadings"]["@value"],
        serde_json::json!([{ "image": "m1", "crop": "full", "words": 1 }]),
        "{doc}"
    );
    // A reading set aside, most of its words off the picture: not listed,
    // and its scores not warned of.
    let word =
        |x: u32| serde_json::json!({ "text": "Account", "box": [x, 30, x + 80, 50], "conf": 90 });
    let reading = serde_json::json!({
        "width": 300, "height": 100,
        "words": [word(20), word(1000), word(2000)]
    });
    std::fs::write(words.join("report_m1.json"), reading.to_string()).unwrap();
    let (doco, err) = run(
        &input,
        &dir.join("home"),
        "doco",
        &["--ocr-words".as_ref(), words.as_os_str()],
    );
    assert!(document(&doco).get("doc:ocrReadings").is_none(), "{doco}");
    assert!(!err.contains("outside 0-1"), "{err}");
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
            .env_remove("FDOC_TIER_RESULTS")
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

#[test]
fn a_drawing_no_engine_decodes_is_told_by_its_first_bytes() {
    // A drawing Word keeps as EMF: listed as such with nothing configured,
    // or with words written for it, and counted in no note.
    let dir = scratch("emf");
    let input = dir.join("report.docx");
    let emf = [&[1u8, 0, 0, 0, 0x6c, 0, 0, 0][..], &[0u8; 120]].concat();
    std::fs::write(&input, docx_with(1, "media/image1.emf", emf)).unwrap();
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    std::fs::write(
        words.join("report_m1.json"),
        r#"{"width":300,"height":100,"words":[]}"#,
    )
    .unwrap();
    for extra in [&[][..], &["--ocr-words".as_ref(), words.as_os_str()][..]] {
        let (doco, err) = run(&input, &dir.join("home"), "doco", extra);
        assert!(
            !err.contains("not read") && !err.contains("warning"),
            "{err}"
        );
        assert_eq!(
            document(&doco)["doc:unreadImages"]["@value"][0]["reason"],
            "UnsupportedFormat"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// A model reading of one block of `text`, as `--tier-results` reads it.
fn tier_reading(dir: &std::path::Path, name: &str, text: &str) {
    let reading = serde_json::json!([{ "parsing_res_list": [
        { "block_label": "text", "block_content": text, "block_order": 0 }
    ]}]);
    std::fs::write(dir.join(format!("{name}.json")), reading.to_string()).unwrap();
}

/// A configuration home with a model configured whose key it cannot use, so
/// nothing leaves the machine; at most `max_crops` crops a document.
fn model_home(dir: &std::path::Path, max_crops: usize) -> PathBuf {
    let home = dir.join("model-home");
    std::fs::create_dir_all(&home).unwrap();
    let key = home.join("key.json");
    std::fs::write(&key, "{}").unwrap();
    std::fs::write(
        home.join("config.toml"),
        format!(
            "[escalation]\nprovider = \"gemini\"\nmax_crops = {max_crops}\n[escalation.gemini]\ncredentials = {:?}\n",
            key.to_str().unwrap()
        ),
    )
    .unwrap();
    home
}

/// `fdoc convert <input> -f <format> <args>` with the configuration in
/// `home`: stdout and stderr.
fn run(
    input: &std::path::Path,
    home: &std::path::Path,
    format: &str,
    args: &[&std::ffi::OsStr],
) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .arg("convert")
        .arg(input)
        .args(["-f", format])
        .args(args)
        .env("FDOC_HOME", home)
        .env_remove("FDOC_OCR_WORDS")
        .env_remove("FDOC_TIER_RESULTS")
        .env_remove("FDOC_OCR_MODELS")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_model_reading_of_a_picture_follows_its_figure() {
    let dir = scratch("tier");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx()).unwrap();
    let tiers = dir.join("tiers");
    std::fs::create_dir_all(&tiers).unwrap();
    tier_reading(&tiers, "report_m1", "Reference 4417-0093-21");
    let (text, _) = run(
        &input,
        &dir.join("home"),
        "text",
        &["--tier-results".as_ref(), tiers.as_os_str()],
    );
    let (before, read, after) = (
        text.find("See the screen below.").unwrap(),
        text.find("Reference 4417-0093-21").expect(&text),
        text.find("That is all.").unwrap(),
    );
    assert!(before < read && read < after, "{text}");
    let (json, _) = run(
        &input,
        &dir.join("home"),
        "json",
        &["--tier-results".as_ref(), tiers.as_os_str()],
    );
    let els: serde_json::Value = serde_json::from_str(&json).unwrap();
    let read = els
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["text"] == "Reference 4417-0093-21")
        .unwrap();
    assert_eq!(read["provenance"], "vlm");
    // No reading of it: listed unread, and no model was asked.
    std::fs::remove_file(tiers.join("report_m1.json")).unwrap();
    let home = model_home(&dir, 0);
    let (doco, err) = run(
        &input,
        &home,
        "doco",
        &["--tier-results".as_ref(), tiers.as_os_str()],
    );
    assert!(!err.contains("configured model"), "{err}");
    assert!(doco.contains("\"NotRead\""), "{doco}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_model_reading_of_a_picture_keeps_the_documents_outline() {
    // A reading whose blocks name the screen's title and a heading of it:
    // they are the screen's, read as paragraphs, as OCR's are.
    let dir = scratch("model-headings");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx()).unwrap();
    let tiers = dir.join("tiers");
    std::fs::create_dir_all(&tiers).unwrap();
    let reading = serde_json::json!([{ "parsing_res_list": [
        { "block_label": "doc_title", "block_content": "SCREENSHOT", "block_order": 0 },
        { "block_label": "text", "block_content": "## Balance 9,876.54", "block_order": 1 },
        { "block_label": "text", "block_content": "Reference ABC-7781", "block_order": 2 }
    ]}]);
    std::fs::write(tiers.join("report_m1.json"), reading.to_string()).unwrap();
    let (md, _) = run(
        &input,
        &dir.join("home"),
        "md",
        &["--tier-results".as_ref(), tiers.as_os_str()],
    );
    assert!(md.contains("Reference ABC-7781"), "{md}");
    assert!(!md.lines().any(|l| l.starts_with('#')), "{md}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_model_reading_replaces_what_ocr_read_unless_it_lost_it() {
    let dir = scratch("tier-over-ocr");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx()).unwrap();
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    // Enough letters for a reading to be judged against them. Scored
    // 0-100: warned of only where OCR's reading is the one taken.
    let line = "Statement of account for the period ending in March with balances carried forward";
    let reading = serde_json::json!({
        "width": 300, "height": 100,
        "words": [
            { "text": line, "box": [10, 20, 290, 40], "conf": 95 },
            { "text": "Reference 4417-0093-21 opened by the branch office", "box": [10, 50, 290, 70], "conf": 90 }
        ]
    });
    std::fs::write(words.join("report_m1.json"), reading.to_string()).unwrap();
    let tiers = dir.join("tiers");
    std::fs::create_dir_all(&tiers).unwrap();
    tier_reading(
        &tiers,
        "report_m1",
        &format!("{line}. Reference 4417-0093-21 opened by the branch office on 14 MAR 1987"),
    );
    let args = [
        "--ocr-words".as_ref(),
        words.as_os_str(),
        "--tier-results".as_ref(),
        tiers.as_os_str(),
    ];
    let (text, _) = run(&input, &dir.join("home"), "text", &args);
    assert!(text.contains("on 14 MAR 1987"), "{text}");
    assert_eq!(text.matches("4417-0093-21").count(), 1, "{text}");
    // OCR's words are not in the output, so its reading is not reported,
    // nor its scores warned of.
    let (doco, err) = run(&input, &dir.join("home"), "doco", &args);
    assert!(document(&doco).get("doc:ocrReadings").is_none(), "{doco}");
    assert!(!err.contains("outside 0-1"), "{err}");
    // A reading that lost what OCR read is a truncated one: OCR's stays.
    tier_reading(&tiers, "report_m1", "Statement");
    let (text, _) = run(&input, &dir.join("home"), "text", &args);
    assert!(
        text.contains("4417-0093-21") && !text.contains("14 MAR"),
        "{text}"
    );
    let (doco, err) = run(&input, &dir.join("home"), "doco", &args);
    assert_eq!(
        document(&doco)["doc:ocrReadings"]["@value"][0]["image"],
        "m1",
        "{doco}"
    );
    assert!(
        err.contains("report_m1.json: 2 of 2 word score(s) outside 0-1"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_picture_ocr_read_no_text_in_goes_to_the_configured_model() {
    let dir = scratch("to-model");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx()).unwrap();
    let home = model_home(&dir, 0);
    // Nothing else configured: the model reads it (and fails to, here).
    let (doco, err) = run(&input, &home, "doco", &[]);
    assert!(
        err.contains("warning: report: the configured model"),
        "{err}"
    );
    assert!(doco.contains("\"Failed\""), "{doco}");
    // Words written for it with text: OCR read it, and no model is asked.
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    let reading = serde_json::json!({
        "width": 300, "height": 100,
        "words": [{ "text": "Account 4417-0093-21", "box": [20, 30, 200, 50] }]
    });
    std::fs::write(words.join("report_m1.json"), reading.to_string()).unwrap();
    let (_, err) = run(
        &input,
        &home,
        "text",
        &["--ocr-words".as_ref(), words.as_os_str()],
    );
    assert!(!err.contains("configured model"), "{err}");
    // Words that hold no text: the model is asked.
    std::fs::write(
        words.join("report_m1.json"),
        r#"{"width":300,"height":100,"words":[]}"#,
    )
    .unwrap();
    let (_, err) = run(
        &input,
        &home,
        "text",
        &["--ocr-words".as_ref(), words.as_os_str()],
    );
    assert!(err.contains("the configured model"), "{err}");
    // No words for it at all: the model is asked too.
    std::fs::remove_file(words.join("report_m1.json")).unwrap();
    let (_, err) = run(
        &input,
        &home,
        "text",
        &["--ocr-words".as_ref(), words.as_os_str()],
    );
    assert!(err.contains("the configured model"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn no_more_pictures_go_to_the_model_than_a_document_may_send() {
    let dir = scratch("budget");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx_distinct(3)).unwrap();
    let home = model_home(&dir, 1);
    let (_, err) = run(&input, &home, "text", &[]);
    assert_eq!(err.matches("the configured model").count(), 1, "{err}");
    assert!(err.contains("2 image(s) not sent to the model"), "{err}");
    assert!(err.contains("3 of 3 image(s)"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

/// The reasons `doc:unreadImages` gives, in order.
fn reasons(doco: &str) -> Vec<String> {
    document(doco)["doc:unreadImages"]["@value"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|u| u["reason"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_picture_of_few_pixels_is_not_sent_to_the_model() {
    // A screen of text 300 pixels wide: small, as an e-mail's image of that
    // size is, whatever size the document shows it at. It is listed, with
    // why; left out by choice, as a logo is, it is not counted on stderr,
    // and `--verbose` names it, with what reads it.
    let dir = scratch("small");
    let input = dir.join("report.docx");
    let small = docx_with(1, "media/image1.png", screen(300, 200));
    std::fs::write(&input, &small).unwrap();
    let home = model_home(&dir, 0);
    let (doco, err) = run(&input, &home, "doco", &[]);
    assert!(!err.contains("configured model"), "{err}");
    assert!(!err.contains("were not read"), "{err}");
    assert_eq!(reasons(&doco), ["Small"]);
    let (_, err) = run(&input, &home, "doco", &["--verbose".as_ref()]);
    assert!(
        err.contains("model: report_m1: not sent (Small); --ocr reads it"),
        "{err}"
    );
    // OCR still reads it, whatever its size.
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    let reading = serde_json::json!({
        "width": 300, "height": 200,
        "words": [{ "text": "Account 4417-0093-21", "box": [20, 60, 280, 100] }]
    });
    std::fs::write(words.join("report_m1.json"), reading.to_string()).unwrap();
    let (text, _) = run(
        &input,
        &home,
        "text",
        &["--ocr-words".as_ref(), words.as_os_str()],
    );
    assert!(text.contains("4417-0093-21"), "{text}");
    // A reading supplied is taken, whatever its size.
    let tiers = dir.join("tiers");
    std::fs::create_dir_all(&tiers).unwrap();
    tier_reading(&tiers, "report_m1", "Reference 4417-0093-21");
    let (text, _) = run(
        &input,
        &home,
        "text",
        &["--tier-results".as_ref(), tiers.as_os_str()],
    );
    assert!(text.contains("Reference 4417-0093-21"), "{text}");
    // 400 pixels wide or more: sent (and refused, here).
    let large = docx_with(1, "media/image1.png", screen(600, 200));
    std::fs::write(&input, large).unwrap();
    let (doco, err) = run(&input, &home, "doco", &[]);
    assert!(err.contains("the configured model"), "{err}");
    assert_eq!(reasons(&doco), ["Failed"]);
    // With no model, nothing was asked to read it.
    std::fs::write(&input, small).unwrap();
    let (doco, err) = run(&input, &dir.join("home"), "doco", &[]);
    assert!(err.contains("1 of 1 image(s)"), "{err}");
    assert_eq!(reasons(&doco), ["NotRead"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_pictures_reading_that_is_noise_leaves_it_unread() {
    // What an engine reads in a photograph or a gradient the probe took for
    // text: a stroke or two, unsure, or one fragment boxed over all of it.
    // Set aside: listed, as a PDF region whose reading is noise is, and not
    // counted on stderr.
    let dir = scratch("noise");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx()).unwrap();
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    for read in [
        serde_json::json!([
            { "text": "M", "box": [20, 20, 80, 70], "conf": 0.46 },
            { "text": "—", "box": [150, 40, 200, 60], "conf": 0.1 }
        ]),
        serde_json::json!([{ "text": "7", "box": [0, 0, 300, 100], "conf": 0.66 }]),
        // Scored 0-100: judged by its boxes alone, and warned of.
        serde_json::json!([{ "text": "7", "box": [0, 0, 300, 100], "conf": 66 }]),
    ] {
        let hundred = read[0]["conf"] == 66;
        let reading = serde_json::json!({ "width": 300, "height": 100, "words": read });
        std::fs::write(words.join("report_m1.json"), reading.to_string()).unwrap();
        let ocr_words = ["--ocr-words".as_ref(), words.as_os_str()];
        let (doco, err) = run(&input, &dir.join("home"), "doco", &ocr_words);
        assert_eq!(reasons(&doco), ["NotTexty"], "{doco}");
        assert!(!err.contains("were not read"), "{err}");
        let warned = "report_m1.json: 1 of 1 word score(s) outside 0-1";
        assert_eq!(err.contains(warned), hundred, "{err}");
        // Its reading is listed, set aside as noise, with the words it held
        // and the engine's confidence in them (none, scored 0-100).
        let listed = &document(&doco)["doc:ocrReadings"]["@value"][0];
        let words = read.as_array().unwrap().len();
        assert_eq!(
            (&listed["image"], &listed["words"], &listed["setAside"]),
            (&"m1".into(), &words.into(), &"noise".into()),
            "{doco}"
        );
        let mean = (!hundred).then(|| {
            let confs: Vec<f64> = (read.as_array().unwrap().iter())
                .map(|w| w["conf"].as_f64().unwrap())
                .collect();
            // To four decimals, as listed.
            (confs.iter().sum::<f64>() / confs.len() as f64 * 1e4).round() / 1e4
        });
        assert_eq!(listed["meanConfidence"].as_f64(), mean, "{doco}");
        let (text, _) = run(&input, &dir.join("home"), "text", &ocr_words);
        assert_eq!(
            text.trim(),
            "See the screen below.\n\nThat is all.",
            "{text}"
        );
        // A model may still read what OCR read no text in.
        let (_, err) = run(&input, &model_home(&dir, 0), "text", &ocr_words);
        assert!(err.contains("the configured model"), "{err}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_pictures_reading_that_lands_off_it_leaves_it_failed() {
    // A sidecar whose words all land off the picture, as a converter that
    // took the wrong origin or scale writes them: set aside, so the picture
    // is listed, warned of and counted, not taken for one with no text.
    // Unsure, it is refused all the same, before it could be judged noise.
    let dir = scratch("astray");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx()).unwrap();
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    for conf in [0.9, 0.3] {
        let reading = serde_json::json!({
            "width": 300, "height": 100,
            "words": [
                { "text": "Total", "box": [1000, 20, 1080, 50], "conf": conf },
                { "text": "4471", "box": [1100, 20, 1160, 50], "conf": conf }
            ]
        });
        std::fs::write(words.join("report_m1.json"), reading.to_string()).unwrap();
        let ocr_words = ["--ocr-words".as_ref(), words.as_os_str()];
        let (doco, err) = run(&input, &dir.join("home"), "doco", &ocr_words);
        assert_eq!(reasons(&doco), ["Failed"], "{conf} {doco}");
        assert!(
            err.contains("warning: report_m1: none of the reading could be placed"),
            "{conf} {err}"
        );
        assert!(err.contains("1 of 1 image(s)"), "{conf} {err}");
        assert!(document(&doco).get("doc:ocrReadings").is_none(), "{doco}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// A 800 × 600 PNG of colour running from black to red across and to green
/// down: nothing in it to read.
#[cfg(feature = "ocr")]
fn gradient() -> Vec<u8> {
    let img = image::RgbImage::from_fn(800, 600, |x, y| {
        image::Rgb([(x * 255 / 800) as u8, (y * 255 / 600) as u8, 120])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[cfg(feature = "ocr")]
#[test]
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn an_engine_reads_no_text_into_a_gradient() {
    let models = std::env::var("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS");
    let dir = scratch("gradient-engine");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx_with(1, "media/image1.png", gradient())).unwrap();
    let engine = [
        "--ocr".as_ref(),
        "oar-ocr".as_ref(),
        "--models".as_ref(),
        models.as_ref(),
    ];
    let (text, _) = run(&input, &dir.join("home"), "text", &engine);
    assert_eq!(
        text.trim(),
        "See the screen below.\n\nThat is all.",
        "{text}"
    );
    let (doco, _) = run(&input, &dir.join("home"), "doco", &engine);
    assert_eq!(reasons(&doco), ["NotTexty"], "{doco}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A PNG of `w` × `h` pixels like a capture of a wide screen holding a few
/// short lines of text at its top, the rest of it a light ground: the
/// screenshot of #10's own report, whose text a model reads.
fn sparse_capture(w: u32, h: u32) -> Vec<u8> {
    let img = image::GrayImage::from_fn(w, h, |x, y| {
        let line = (40..180).contains(&y) && (y - 40) % 28 < 12;
        let ink = line && (40..360).contains(&x) && (x / 3) % 2 == 0;
        image::Luma([if ink { 30 } else { 245 }])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// A JPEG of `w` × `h` pixels like a photograph: light falling unevenly,
/// and the noise of a phone's sensor.
fn photo(w: u32, h: u32) -> Vec<u8> {
    let mut seed = 0x2545_f491_u32;
    let img = image::GrayImage::from_fn(w, h, |x, y| {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let light = 120 + 80 * (x + y) / (w + h);
        let noise = (seed >> 24) as i32 % 41 - 20;
        image::Luma([(light as i32 + noise).clamp(0, 255) as u8])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageLuma8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
    out
}

#[test]
fn a_picture_over_the_floor_is_sent_to_the_model_when_the_probe_takes_it_for_text() {
    // A capture of a wide screen with a few lines of text and a line of a
    // form captured on its own look like text: sent (and refused, here). A
    // flat picture and a photograph do not: read by nothing, listed as
    // `NotTexty`, and no request is made.
    let grey = {
        let img = image::RgbImage::from_pixel(600, 400, image::Rgb([200, 200, 200]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    for (name, image, sent) in [
        ("media/image1.png", sparse_capture(1366, 768), true),
        ("media/image1.png", screen(2000, 56), true),
        ("media/image1.png", grey, false),
        ("media/image1.jpeg", photo(600, 800), false),
    ] {
        let dir = scratch("over-floor");
        let input = dir.join("report.docx");
        std::fs::write(&input, docx_with(1, name, image)).unwrap();
        let (doco, err) = run(&input, &model_home(&dir, 0), "doco", &[]);
        assert_eq!(err.contains("the configured model"), sent, "{name}: {err}");
        let want = if sent { "Failed" } else { "NotTexty" };
        assert_eq!(reasons(&doco), [want], "{name}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn a_documents_pictures_go_to_the_model_through_one_reader() {
    // Three pictures for the model, and a key it cannot sign in with: the
    // reader is opened once for the document, so the failure is said once,
    // and each picture is listed as one the model failed to read.
    let dir = scratch("one-reader");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx_distinct(3)).unwrap();
    let home = model_home(&dir, 0);
    let (doco, err) = run(&input, &home, "doco", &[]);
    assert_eq!(err.matches("the configured model").count(), 1, "{err}");
    assert_eq!(doco.matches("\"Failed\"").count(), 3, "{doco}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_reading_supplied_is_taken_whatever_the_probe_makes_of_the_picture() {
    // A photograph the pixel probe does not take for text, with a model
    // configured: it is not sent. A reading of it supplied, by a model
    // (`--tier-results`) or by OCR (`--ocr-words`), costs no call and is
    // taken all the same: the probe is asked only before a billed call.
    let dir = scratch("supplied");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx_with(1, "media/image1.jpeg", photo(600, 800))).unwrap();
    let home = model_home(&dir, 0);
    let (doco, err) = run(&input, &home, "doco", &[]);
    assert!(!err.contains("configured model"), "{err}");
    assert_eq!(reasons(&doco), ["NotTexty"]);
    let tiers = dir.join("tiers");
    std::fs::create_dir_all(&tiers).unwrap();
    tier_reading(&tiers, "report_m1", "Badge 4417-0093-21");
    let (text, _) = run(
        &input,
        &home,
        "text",
        &["--tier-results".as_ref(), tiers.as_os_str()],
    );
    assert!(text.contains("Badge 4417-0093-21"), "{text}");
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    let reading = serde_json::json!({
        "width": 600, "height": 800,
        "words": [{ "text": "Badge 4417-0093-21", "box": [40, 60, 400, 100] }]
    });
    std::fs::write(words.join("report_m1.json"), reading.to_string()).unwrap();
    let (text, err) = run(
        &input,
        &home,
        "text",
        &["--ocr-words".as_ref(), words.as_os_str()],
    );
    assert!(text.contains("4417-0093-21"), "{text}");
    assert!(!err.contains("configured model"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn with_no_escalate_no_picture_is_sent_to_the_configured_model() {
    // A model configured and a picture it would be sent: `--no-escalate`
    // keeps it on the machine. Nothing is sent, so nothing fails to be.
    let dir = scratch("no-escalate");
    let input = dir.join("report.docx");
    std::fs::write(&input, docx()).unwrap();
    let home = model_home(&dir, 0);
    let (doco, err) = run(&input, &home, "doco", &[]);
    assert!(err.contains("the configured model"), "{err}");
    let (doco_kept, err) = run(&input, &home, "doco", &["--no-escalate".as_ref()]);
    assert!(!err.contains("the configured model"), "{err}");
    assert_eq!(reasons(&doco), ["Failed"]);
    assert_eq!(reasons(&doco_kept), ["NotRead"]);
    let _ = std::fs::remove_dir_all(dir);
}

/// A DOCX of `parts` pictures, each its own part holding `image`.
fn docx_of_parts(parts: usize, image: &[u8]) -> Vec<u8> {
    let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let picture = |i: usize| {
        format!(
            "<w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"{i}\" name=\"Picture {i}\"/><a:graphic><a:graphicData>\
             <pic:pic><pic:blipFill><a:blip r:embed=\"rId{i}\"/></pic:blipFill></pic:pic></a:graphicData></a:graphic>\
             </wp:inline></w:drawing></w:r></w:p>"
        )
    };
    let body = format!(
        "<w:document xmlns:w=\"w\" xmlns:a=\"a\" xmlns:r=\"r\" xmlns:wp=\"wp\" xmlns:pic=\"pic\"><w:body>\
         <w:p><w:r><w:t>Pictures.</w:t></w:r></w:p>{}</w:body></w:document>",
        (1..=parts).map(picture).collect::<String>()
    );
    let rels: String = (1..=parts)
        .map(|i| {
            format!(
                "<Relationship Id=\"rId{i}\" Type=\"{rel}/image\" Target=\"media/image{i}.png\"/>"
            )
        })
        .collect();
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let opts = zip::write::SimpleFileOptions::default();
    z.start_file("word/document.xml", opts).unwrap();
    z.write_all(body.as_bytes()).unwrap();
    z.start_file("word/_rels/document.xml.rels", opts).unwrap();
    z.write_all(format!("<Relationships>{rels}</Relationships>").as_bytes())
        .unwrap();
    for i in 1..=parts {
        z.start_file(format!("word/media/image{i}.png"), opts)
            .unwrap();
        z.write_all(image).unwrap();
    }
    z.finish().unwrap();
    buf.into_inner()
}

#[test]
fn a_part_that_only_opens_like_an_image_is_never_sent() {
    // Forty parts, each a PNG's signature and then zeros: they open like an
    // image, but no header says what they hold. With a model configured,
    // none is sent (no request fails, none is made) and none is held to
    // be: each is listed as failed, its header named on stderr.
    let dir = scratch("hostile");
    let input = dir.join("report.docx");
    let fake = [&b"\x89PNG\r\n\x1a\n"[..], &[0u8; 4096]].concat();
    std::fs::write(&input, docx_of_parts(40, &fake)).unwrap();
    let (doco, err) = run(&input, &model_home(&dir, 0), "doco", &[]);
    assert!(!err.contains("configured model"), "{err}");
    assert_eq!(
        err.matches("its header does not parse").count(),
        40,
        "{err}"
    );
    assert_eq!(reasons(&doco), vec!["Failed"; 40]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_part_that_cannot_be_read_failed_with_nothing_to_read_it() {
    // A relationship naming a part the package does not hold: it failed,
    // whether or not anything was there to read it, and is said to have.
    let dir = scratch("absent-part");
    let input = dir.join("report.docx");
    // The package written again without the image's part.
    let full = docx();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&full)).unwrap();
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    for i in 0..zip.len() {
        let file = zip.by_index_raw(i).unwrap();
        if file.name().unwrap() != "word/media/image1.png" {
            z.raw_copy_file(file).unwrap();
        }
    }
    z.finish().unwrap();
    let docx = buf.into_inner();
    std::fs::write(&input, docx).unwrap();
    let (doco, err) = run(&input, &dir.join("home"), "doco", &[]);
    assert_eq!(reasons(&doco), ["Failed"], "{err}");
    assert!(err.contains("warning: report_m1:"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}
