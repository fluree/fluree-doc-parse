//! `fdoc convert` on a workbook with a picture anchored to a sheet's cells:
//! the words read in it follow its figure, placed by its anchor. Words come
//! from a sidecar (`--ocr-words`); no engine is run.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A workbook of one sheet: a line on row 1, a picture
/// (`xl/media/image1.png`) anchored at row 3, a line on row 6.
fn xlsx() -> Vec<u8> {
    xlsx_with(capture(300, 100))
}

/// A PNG of `w` × `h` pixels: lines of strokes a letter wide on a light
/// ground, text to the pixel probe.
fn capture(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let ink = (12..24).contains(&(y % 40)) && x % 10 < 6 && x < w * 3 / 4;
        image::Rgb(if ink { [40, 40, 40] } else { [200, 200, 200] })
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// [`xlsx`], its picture `png`.
fn xlsx_with(png: Vec<u8>) -> Vec<u8> {
    let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let ns = format!(
        "xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"{rel}\""
    );
    let cell = |r: &str, t: &str| format!("<c r=\"{r}\" t=\"inlineStr\"><is><t>{t}</t></is></c>");
    let parts: Vec<(&str, Vec<u8>)> = vec![
        (
            "xl/workbook.xml",
            format!("<workbook {ns}><sheets><sheet name=\"Record\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>")
                .into_bytes(),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            format!("<Relationships><Relationship Id=\"rId1\" Type=\"{rel}/worksheet\" Target=\"worksheets/sheet1.xml\"/></Relationships>")
                .into_bytes(),
        ),
        (
            "xl/worksheets/sheet1.xml",
            format!(
                "<worksheet {ns}><sheetData><row r=\"1\">{}</row><row r=\"6\">{}</row></sheetData>\
                 <drawing r:id=\"rId9\"/></worksheet>",
                cell("A1", "See the screen below."),
                cell("A6", "That is all.")
            )
            .into_bytes(),
        ),
        (
            "xl/worksheets/_rels/sheet1.xml.rels",
            format!("<Relationships><Relationship Id=\"rId9\" Type=\"{rel}/drawing\" Target=\"../drawings/drawing1.xml\"/></Relationships>")
                .into_bytes(),
        ),
        (
            "xl/drawings/drawing1.xml",
            format!(
                "<xdr:wsDr xmlns:xdr=\"x\" xmlns:a=\"a\" xmlns:r=\"{rel}\"><xdr:twoCellAnchor>\
                 <xdr:from><xdr:col>0</xdr:col><xdr:row>2</xdr:row></xdr:from>\
                 <xdr:pic><xdr:nvPicPr><xdr:cNvPr id=\"2\" name=\"Picture 1\"/></xdr:nvPicPr>\
                 <xdr:blipFill><a:blip r:embed=\"rId1\"/></xdr:blipFill></xdr:pic></xdr:twoCellAnchor></xdr:wsDr>"
            )
            .into_bytes(),
        ),
        (
            "xl/drawings/_rels/drawing1.xml.rels",
            format!("<Relationships><Relationship Id=\"rId1\" Type=\"{rel}/image\" Target=\"../media/image1.png\"/></Relationships>")
                .into_bytes(),
        ),
        ("xl/media/image1.png", png),
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
    let dir = std::env::temp_dir().join(format!("fdoc-xlsx-media-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn convert(dir: &Path, input: &Path, args: &[&std::ffi::OsStr]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .arg("convert")
        .arg(input)
        .args(args)
        .env("FDOC_HOME", dir.join("home"))
        .env_remove("FDOC_OCR_WORDS")
        .env_remove("FDOC_TIER_RESULTS")
        .output()
        .unwrap()
}

#[test]
fn a_pictures_words_follow_its_figure_by_its_anchor() {
    let dir = scratch("words");
    let input = dir.join("book.xlsx");
    std::fs::write(&input, xlsx()).unwrap();
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    let reading = serde_json::json!({
        "width": 300, "height": 100,
        "words": [{ "text": "Account 4417-0093-21", "box": [20, 30, 200, 50] }]
    });
    std::fs::write(words.join("book_m1.json"), reading.to_string()).unwrap();
    let out = convert(
        &dir,
        &input,
        &[
            "-f".as_ref(),
            "text".as_ref(),
            "--ocr-words".as_ref(),
            words.as_os_str(),
        ],
    );
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
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_picture_nothing_read_is_listed_unread() {
    let dir = scratch("unread");
    let input = dir.join("book.xlsx");
    std::fs::write(&input, xlsx()).unwrap();
    let out = convert(&dir, &input, &["-f".as_ref(), "doco".as_ref()]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("book: 1 of 1 image(s)"), "{err}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let doc = v["@graph"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["@type"] == "foaf:Document")
        .unwrap();
    assert_eq!(
        doc["doc:unreadImages"]["@value"],
        serde_json::json!([{
            "figure": "figure-0-1", "image": "m1",
            "part": "xl/media/image1.png", "reason": "NotRead"
        }])
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn render_routed_crops_a_sheets_picture_as_media() {
    let dir = scratch("render");
    let input = dir.join("book.xlsx");
    std::fs::write(&input, xlsx()).unwrap();
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
    assert_eq!(rec["png"], "book_m1.png");
    assert_eq!(rec["figure"], "figure-0-1");
    assert_eq!(rec["part"], "xl/media/image1.png");
    // Under the pixels a model is sent: for OCR only.
    assert_eq!(rec["reason"], "small_image");
    assert!(crops.join("book_m1.png").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_model_reading_supplied_for_the_image_is_placed_where_it_is() {
    let dir = scratch("tier");
    let input = dir.join("book.xlsx");
    std::fs::write(&input, xlsx()).unwrap();
    let tiers = dir.join("tiers");
    std::fs::create_dir_all(&tiers).unwrap();
    let reading = serde_json::json!([{ "parsing_res_list": [
        { "block_label": "text", "block_content": "Reference 4417-0093-21", "block_order": 0 }
    ]}]);
    std::fs::write(tiers.join("book_m1.json"), reading.to_string()).unwrap();
    let out = convert(
        &dir,
        &input,
        &[
            "-f".as_ref(),
            "text".as_ref(),
            "--tier-results".as_ref(),
            tiers.as_os_str(),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let (before, read, after) = (
        text.find("See the screen below.").unwrap(),
        text.find("Reference 4417-0093-21").expect(&text),
        text.find("That is all.").unwrap(),
    );
    assert!(before < read && read < after, "{text}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("were not read"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_pictures_ocr_reading_is_reported_with_its_confidence() {
    let dir = scratch("confidence");
    let input = dir.join("book.xlsx");
    std::fs::write(&input, xlsx()).unwrap();
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    let reading = serde_json::json!({
        "width": 300, "height": 100,
        "words": [{ "text": "Account 4417-0093-21", "box": [20, 30, 200, 50], "conf": 0.9 }]
    });
    std::fs::write(words.join("book_m1.json"), reading.to_string()).unwrap();
    let out = convert(
        &dir,
        &input,
        &[
            "-f".as_ref(),
            "doco".as_ref(),
            "--ocr-words".as_ref(),
            words.as_os_str(),
        ],
    );
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
    // A sheet's picture goes by its name too, as a DOCX's does.
    assert_eq!(
        doc["doc:ocrReadings"]["@value"],
        serde_json::json!([{ "image": "m1", "crop": "full", "words": 1, "meanConfidence": 0.9, "minConfidence": 0.9 }]),
        "{doc}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn with_no_escalate_no_picture_is_sent_to_the_configured_model() {
    // A capture over the floor a model is sent, and a model configured
    // whose key it cannot use: sent (and refused) by default, kept on the
    // machine with `--no-escalate`.
    let dir = scratch("no-escalate");
    let input = dir.join("book.xlsx");
    std::fs::write(&input, xlsx_with(capture(600, 200))).unwrap();
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let key = home.join("key.json");
    std::fs::write(&key, "{}").unwrap();
    std::fs::write(
        home.join("config.toml"),
        format!(
            "[escalation]\nprovider = \"gemini\"\n[escalation.gemini]\ncredentials = {:?}\n",
            key.to_str().unwrap()
        ),
    )
    .unwrap();
    let out = convert(&dir, &input, &["-f".as_ref(), "doco".as_ref()]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("the configured model"), "{err}");
    let out = convert(
        &dir,
        &input,
        &["-f".as_ref(), "doco".as_ref(), "--no-escalate".as_ref()],
    );
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("the configured model"), "{err}");
    let doco = String::from_utf8_lossy(&out.stdout);
    assert!(doco.contains("\"NotRead\""), "{doco}");
    let _ = std::fs::remove_dir_all(dir);
}
