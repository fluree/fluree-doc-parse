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
--rel-7f3a\r\n\
Content-Type: image/png; name=\"shot.png\"\r\n\
Content-ID: <shot1@example.org>\r\n\
Content-Transfer-Encoding: binary\r\n\
\r\n"
        .to_vec();
    m.extend(png);
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
