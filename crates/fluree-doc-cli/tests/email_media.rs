//! `fdoc convert` on an e-mail showing an image in its body: the words read
//! in it are placed where it is shown. Words come from a sidecar
//! (`--ocr-words`); no engine is run.

use std::path::PathBuf;
use std::process::Command;

/// A message whose plain text marks a screenshot between two paragraphs,
/// as Outlook writes it, the screenshot a related part.
fn eml() -> Vec<u8> {
    let png = {
        let img = image::RgbImage::from_pixel(300, 100, image::Rgb([200, 200, 200]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    eml_of("image/png", &png)
}

/// The message of [`eml`], showing `image` of type `content_type`.
fn eml_of(content_type: &str, image: &[u8]) -> Vec<u8> {
    let mut m = b"From: a@example.org\r\n\
To: b@example.org\r\n\
Subject: Record\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"rel-7f3a\"\r\n\
\r\n\
--rel-7f3a\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
See the screen below.\r\n\
\r\n\
[cid:shot1@example.org]\r\n\
\r\n\
That is all.\r\n\
--rel-7f3a\r\n"
        .to_vec();
    m.extend(
        format!(
            "Content-Type: {content_type}; name=\"shot.{}\"\r\n\
             Content-ID: <shot1@example.org>\r\n\
             Content-Transfer-Encoding: binary\r\n\r\n",
            content_type.trim_start_matches("image/")
        )
        .bytes(),
    );
    m.extend(image);
    m.extend(b"\r\n--rel-7f3a--\r\n");
    m
}

fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fdoc-email-media-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn convert(
    dir: &std::path::Path,
    input: &std::path::Path,
    args: &[&std::ffi::OsStr],
) -> std::process::Output {
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

/// The document node of a DoCO graph.
fn document(doco: &[u8]) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_slice(doco).unwrap();
    v["@graph"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["@type"] == "foaf:Document")
        .unwrap()
        .clone()
}

#[test]
fn an_inline_images_words_are_placed_where_the_body_shows_it() {
    let dir = scratch("words");
    let input = dir.join("mail.eml");
    std::fs::write(&input, eml()).unwrap();
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    let reading = serde_json::json!({
        "width": 300, "height": 100,
        "words": [{ "text": "Account 4417-0093-21", "box": [20, 30, 200, 50] }]
    });
    std::fs::write(words.join("mail_m1.json"), reading.to_string()).unwrap();
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
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("were not read"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_inline_image_nothing_read_is_listed_unread() {
    let dir = scratch("unread");
    let input = dir.join("mail.eml");
    std::fs::write(&input, eml()).unwrap();
    let out = convert(&dir, &input, &["-f".as_ref(), "doco".as_ref()]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("mail: 1 of 1 image(s)"), "{err}");
    let doc = document(&out.stdout);
    assert_eq!(
        doc["doc:unreadImages"]["@value"],
        serde_json::json!([{
            "figure": "figure-0-1", "image": "m1",
            "part": "cid:shot1@example.org", "reason": "NotRead"
        }])
    );
    // The image is still one of the message's files.
    assert_eq!(doc["doc:attachments"]["@value"][0]["inline"], true);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_logo_no_engine_decodes_is_listed_but_not_counted() {
    // A signature's GIF logo, with words written for it: OCR cannot decode
    // it, so it is listed unread, but nearly every signed message carries
    // one, and the note on stderr leaves it out.
    let gif: &[u8] = &[
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xff, 0xff,
        0xff, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02,
        0x02, 0x44, 0x01, 0x00, 0x3b,
    ];
    let dir = scratch("logo");
    let input = dir.join("mail.eml");
    std::fs::write(&input, eml_of("image/gif", gif)).unwrap();
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    std::fs::write(
        words.join("mail_m1.json"),
        r#"{"width":1,"height":1,"words":[]}"#,
    )
    .unwrap();
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
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("were not read"), "{err}");
    // Told by its first bytes, it is not given to OCR to fail on.
    assert!(!err.contains("warning"), "{err}");
    let doc = document(&out.stdout);
    assert_eq!(
        doc["doc:unreadImages"]["@value"][0]["reason"],
        "UnsupportedFormat"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A message showing a screenshot (PNG) and a signature's logo (GIF).
fn eml_with_logo() -> Vec<u8> {
    let gif: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00\
\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00\x3b";
    let mut m = eml();
    let end = b"\r\n--rel-7f3a--\r\n";
    m.truncate(m.len() - end.len());
    // The logo after the screenshot, as a signature follows the message.
    let at = m
        .windows(14)
        .position(|w| w == b"That is all.\r\n")
        .unwrap()
        + 14;
    m.splice(at..at, b"\r\n[cid:logo@example.org]\r\n".iter().copied());
    m.extend(
        b"\r\n--rel-7f3a\r\n\
Content-Type: image/gif; name=\"logo.gif\"\r\n\
Content-ID: <logo@example.org>\r\n\
Content-Transfer-Encoding: binary\r\n\r\n",
    );
    m.extend(gif);
    m.extend(end);
    m
}

#[test]
fn a_logo_no_engine_decodes_is_told_by_its_bytes_whatever_reads_the_rest() {
    // Nothing configured, words for neither image, or no reading supplied:
    // the screenshot is counted unread, the logo is listed as what it is
    // and not counted, and nothing is warned about it.
    let dir = scratch("logo-by-bytes");
    let input = dir.join("mail.eml");
    std::fs::write(&input, eml_with_logo()).unwrap();
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let doco = ["-f".as_ref(), "doco".as_ref()];
    for extra in [
        &[][..],
        &["--ocr-words".as_ref(), empty.as_os_str()][..],
        &["--tier-results".as_ref(), empty.as_os_str()][..],
    ] {
        let args: Vec<&std::ffi::OsStr> = doco.iter().chain(extra).copied().collect();
        let out = convert(&dir, &input, &args);
        assert!(out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("mail: 1 of 1 image(s)"), "{extra:?}: {err}");
        assert!(!err.contains("warning"), "{extra:?}: {err}");
        let doc = document(&out.stdout);
        let reasons: Vec<&str> = doc["doc:unreadImages"]["@value"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["reason"].as_str().unwrap())
            .collect();
        assert_eq!(reasons, ["NotRead", "UnsupportedFormat"], "{extra:?}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_image_part_too_short_for_any_format_failed() {
    // An empty part, and one cut short inside the PNG signature: neither is
    // a format no engine decodes, but an image that failed, counted and
    // warned about.
    for (test, part) in [("empty", &b""[..]), ("cut", &b"\x89PNG"[..])] {
        let dir = scratch(test);
        let input = dir.join("mail.eml");
        std::fs::write(&input, eml_of("image/png", part)).unwrap();
        let out = convert(&dir, &input, &["-f".as_ref(), "doco".as_ref()]);
        assert!(out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("too few for an image"), "{err}");
        assert!(err.contains("mail: 1 of 1 image(s)"), "{err}");
        let unread = &document(&out.stdout)["doc:unreadImages"]["@value"];
        assert_eq!(unread[0]["reason"], "Failed", "{unread}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn render_routed_crops_an_inline_image_as_media() {
    let dir = scratch("render");
    let input = dir.join("mail.eml");
    std::fs::write(&input, eml()).unwrap();
    let crops = dir.join("crops");
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .args(["dev", "render-routed"])
        .arg(&input)
        .arg(&crops)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("1 embedded images"), "{said}");
    let manifest = std::fs::read_to_string(crops.join("manifest.jsonl")).unwrap();
    let rec: serde_json::Value = serde_json::from_str(manifest.lines().next().unwrap()).unwrap();
    assert_eq!(rec["kind"], "media");
    assert_eq!(rec["png"], "mail_m1.png");
    assert_eq!(rec["figure"], "figure-0-1");
    assert_eq!(rec["part"], "cid:shot1@example.org");
    assert_eq!(rec["reason"], "embedded_image");
    assert!(crops.join("mail_m1.png").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_model_reading_supplied_for_the_image_is_placed_where_it_is() {
    let dir = scratch("tier");
    let input = dir.join("mail.eml");
    std::fs::write(&input, eml()).unwrap();
    let tiers = dir.join("tiers");
    std::fs::create_dir_all(&tiers).unwrap();
    let reading = serde_json::json!([{ "parsing_res_list": [
        { "block_label": "text", "block_content": "Reference 4417-0093-21", "block_order": 0 }
    ]}]);
    std::fs::write(tiers.join("mail_m1.json"), reading.to_string()).unwrap();
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
