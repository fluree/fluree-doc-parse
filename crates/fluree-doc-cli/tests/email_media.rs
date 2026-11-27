//! `fdoc convert` on an e-mail showing an image in its body: the words read
//! in it are placed where it is shown. Words come from a sidecar
//! (`--ocr-words`); no engine is run.

use std::path::PathBuf;
use std::process::Command;

/// A message whose plain text marks a screenshot between two paragraphs,
/// as Outlook writes it, the screenshot a related part.
fn eml() -> Vec<u8> {
    let png = {
        let img = image::RgbImage::from_fn(300, 100, |x, y| {
            // Lines of strokes a letter wide on a light ground: text, to
            // the pixel probe.
            let ink = (12..24).contains(&(y % 40)) && x % 10 < 6 && x < 225;
            image::Rgb(if ink { [40, 40, 40] } else { [200, 200, 200] })
        });
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
    with_logo(eml(), "image/gif", gif)
}

/// The message `m` of [`eml_of`], showing after its text a signature's
/// logo, `logo` of type `content_type`.
fn with_logo(mut m: Vec<u8>, content_type: &str, logo: &[u8]) -> Vec<u8> {
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
        format!(
            "\r\n--rel-7f3a\r\n\
             Content-Type: {content_type}; name=\"logo\"\r\n\
             Content-ID: <logo@example.org>\r\n\
             Content-Transfer-Encoding: binary\r\n\r\n"
        )
        .bytes(),
    );
    m.extend(logo);
    m.extend(end);
    m
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

#[test]
fn a_signatures_logo_is_not_sent_to_the_model_and_a_screenshot_is() {
    // A screenshot and a logo, both of text, with a model configured whose
    // key it cannot use: the screenshot is sent (and refused), the logo,
    // under the pixels an image with no page must have, is not; it is
    // listed, with why, and not counted on stderr, as a GIF logo is not:
    // only the screenshot the model failed on is.
    let dir = scratch("logo-model");
    let input = dir.join("mail.eml");
    let mail = with_logo(
        eml_of("image/png", &screen(600, 200)),
        "image/png",
        &screen(200, 60),
    );
    std::fs::write(&input, mail).unwrap();
    with_model(&dir);
    let out = convert(&dir, &input, &["-f".as_ref(), "doco".as_ref()]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(err.matches("the configured model").count(), 1, "{err}");
    assert!(
        err.contains("mail: 1 of 1 image(s) in the document were not read\n"),
        "{err}"
    );
    assert_eq!(reasons(&out.stdout), ["Failed", "Small"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn with_no_escalate_no_image_is_sent_to_the_configured_model() {
    // A screenshot a model configured would be sent (and refuse, here):
    // `--no-escalate` keeps it on the machine.
    let dir = scratch("no-escalate");
    let input = dir.join("mail.eml");
    std::fs::write(&input, eml_of("image/png", &screen(600, 200))).unwrap();
    with_model(&dir);
    let out = convert(&dir, &input, &["-f".as_ref(), "doco".as_ref()]);
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
    assert_eq!(reasons(&out.stdout), ["NotRead"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_strip_too_short_to_hold_a_line_is_not_sent_to_the_model() {
    // A strip of text wide enough, but 30 pixels high: under the 48 an
    // image must have on its shorter side, as a rule or a banner is.
    let dir = scratch("strip-model");
    let input = dir.join("mail.eml");
    std::fs::write(&input, eml_of("image/png", &screen(600, 30))).unwrap();
    with_model(&dir);
    let out = convert(&dir, &input, &["-f".as_ref(), "doco".as_ref()]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("the configured model"), "{err}");
    assert!(!err.contains("were not read"), "{err}");
    assert_eq!(reasons(&out.stdout), ["Small"]);
    let _ = std::fs::remove_dir_all(dir);
}

/// A PNG of `w` × `h` pixels: one line of strokes, a pixel of paper above
/// and below.
fn line(w: u32, h: u32) -> Vec<u8> {
    let img = image::GrayImage::from_fn(w, h, |x, y| {
        let ink = y > 0 && y + 1 < h && x % 6 < 3;
        image::Luma([if ink { 30 } else { 230 }])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[test]
fn words_written_for_an_image_are_read_whatever_the_probe_makes_of_it() {
    // A strip of 9 pixels is under the floor a PDF's region is held to (10
    // on its short side), and the pixel probe does not take it for text;
    // the words written for it are read all the same, as one of 10 is: a
    // reading supplied costs neither OCR nor a model's call.
    let dir = scratch("thin");
    let input = dir.join("mail.eml");
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    for h in [9, 10] {
        std::fs::write(&input, eml_of("image/png", &line(700, h))).unwrap();
        let reading = serde_json::json!({
            "width": 700, "height": h,
            "words": [{ "text": "Ref 4417-0093", "box": [10, 1, 300, h - 1] }]
        });
        std::fs::write(words.join("mail_m1.json"), reading.to_string()).unwrap();
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
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("4417-0093"), "{h}: {text}");
        assert!(reasons(&out.stdout).is_empty(), "{h}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// A model configured in `dir`'s home, whose key it cannot sign in with:
/// what is sent to it fails, and nothing leaves the machine.
fn with_model(dir: &std::path::Path) {
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
}

/// The reasons `doc:unreadImages` gives in a DoCO graph, in order.
fn reasons(doco: &[u8]) -> Vec<String> {
    document(doco)["doc:unreadImages"]["@value"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|u| u["reason"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
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

#[test]
fn an_image_over_the_floor_is_sent_to_the_model_when_the_probe_takes_it_for_text() {
    // A capture of a wide screen with a few lines of text and a line of a
    // form captured on its own look like text: sent (and refused, here). A
    // photograph does not: read by nothing, listed as `NotTexty` and not
    // counted on stderr. A small dialog box is under the floor, listed and
    // not counted either.
    for (content_type, image, reason) in [
        ("image/png", sparse_capture(1366, 768), "Failed"),
        ("image/png", screen(2000, 56), "Failed"),
        ("image/jpeg", photo(600, 800), "NotTexty"),
        ("image/png", screen(380, 150), "Small"),
    ] {
        let dir = scratch(&format!("over-floor-{reason}"));
        let input = dir.join("mail.eml");
        std::fs::write(&input, eml_of(content_type, &image)).unwrap();
        with_model(&dir);
        let out = convert(&dir, &input, &["-f".as_ref(), "doco".as_ref()]);
        assert!(out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            err.contains("mail: 1 of 1 image(s)"),
            reason == "Failed",
            "{err}"
        );
        assert_eq!(reasons(&out.stdout), [reason]);
        let _ = std::fs::remove_dir_all(dir);
    }
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
    // 300 pixels wide: under the floor a model is sent, for OCR only.
    assert_eq!(rec["reason"], "small_image");
    assert!(crops.join("mail_m1.png").exists());
    // A screenshot over it is one a pipeline may send its model.
    std::fs::write(&input, eml_of("image/png", &screen(600, 200))).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .args(["dev", "render-routed"])
        .arg(&input)
        .arg(&crops)
        .output()
        .unwrap();
    assert!(out.status.success());
    let manifest = std::fs::read_to_string(crops.join("manifest.jsonl")).unwrap();
    let rec: serde_json::Value = serde_json::from_str(manifest.lines().next().unwrap()).unwrap();
    assert_eq!(rec["reason"], "embedded_image");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn two_pasted_images_of_one_name_are_each_read_where_they_are_shown() {
    // Two inline images a client called alike, without a Content-ID: each
    // has its own words, its own crop, and neither is read for the other.
    let dir = scratch("same-name");
    let input = dir.join("mail.eml");
    let mut m = b"From: a@example.org\r\n\
Subject: Record\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"mix\"\r\n\
\r\n\
--mix\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
See both.\r\n"
        .to_vec();
    for image in [screen(300, 100), screen(320, 100)] {
        m.extend(
            b"--mix\r\n\
Content-Type: image/png\r\n\
Content-Disposition: inline; filename=\"image.png\"\r\n\
Content-Transfer-Encoding: binary\r\n\r\n",
        );
        m.extend(image);
        m.extend(b"\r\n");
    }
    m.extend(b"--mix--\r\n");
    std::fs::write(&input, &m).unwrap();
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    for (tag, text) in [("m1", "ALPHA-1111"), ("m2", "BRAVO-2222")] {
        let reading = serde_json::json!({
            "width": 300, "height": 100,
            "words": [{ "text": text, "box": [20, 30, 200, 50] }]
        });
        std::fs::write(words.join(format!("mail_{tag}.json")), reading.to_string()).unwrap();
    }
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
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(text.matches("ALPHA-1111").count(), 1, "{text}");
    assert_eq!(text.matches("BRAVO-2222").count(), 1, "{text}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("not converted"), "{err}");
    let crops = dir.join("crops");
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .args(["dev", "render-routed"])
        .arg(&input)
        .arg(&crops)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(crops.join("mail_m1.png").exists());
    assert!(crops.join("mail_m2.png").exists());
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

#[test]
fn an_inline_image_read_into_the_body_is_not_an_attachment_left_out() {
    let dir = scratch("note");
    let input = dir.join("mail.eml");
    // The screenshot, and a PDF attached.
    let mut m = eml();
    let end = b"\r\n--rel-7f3a--\r\n";
    m.truncate(m.len() - end.len());
    m.extend(
        b"\r\n--rel-7f3a\r\n\
Content-Type: application/pdf\r\n\
Content-Disposition: attachment; filename=\"budget.pdf\"\r\n\
\r\n\
%PDF-1.7 stand-in\r\n\
--rel-7f3a--\r\n",
    );
    std::fs::write(&input, &m).unwrap();
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    let ocr = [
        "-f".as_ref(),
        "text".as_ref(),
        "--ocr-words".as_ref(),
        words.as_os_str(),
    ];
    // Not read: both files are left out.
    let out = convert(&dir, &input, &ocr);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("2 attachments not converted (shot.png, budget.pdf); pass"),
        "{err}"
    );
    // Read, but no text in it: nothing of it is in the body, and it is
    // still a file left out.
    let empty = serde_json::json!({ "width": 300, "height": 100, "words": [] });
    std::fs::write(words.join("mail_m1.json"), empty.to_string()).unwrap();
    let out = convert(&dir, &input, &ocr);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("2 attachments not converted (shot.png, budget.pdf); pass"),
        "{err}"
    );
    // Read: only the PDF is.
    let reading = serde_json::json!({
        "width": 300, "height": 100,
        "words": [{ "text": "Account 4417-0093-21", "box": [20, 30, 200, 50] }]
    });
    std::fs::write(words.join("mail_m1.json"), reading.to_string()).unwrap();
    let out = convert(&dir, &input, &ocr);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("1 attachment not converted (budget.pdf); 1 inline image read into the body"),
        "{err}"
    );
    // The image alone, read: nothing is left out.
    std::fs::write(&input, eml()).unwrap();
    let out = convert(&dir, &input, &ocr);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("not converted"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn render_routed_warns_of_an_input_it_cannot_read_as_convert_fails_it() {
    // Text saved as a PDF, and bytes no kind claims: read as PDFs, as
    // `convert` reads them, and warned of, not skipped in silence.
    let dir = scratch("render-junk");
    for (name, bytes) in [
        ("junk.pdf", &b"not a PDF at all\n"[..]),
        ("junk.bin", b"\x00\x01\x02"),
    ] {
        let input = dir.join(name);
        std::fs::write(&input, bytes).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
            .args(["dev", "render-routed"])
            .arg(&input)
            .arg(dir.join(format!("crops-{name}")))
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains(&format!("warning: {}: ", input.display())),
            "{name}: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn render_routed_knows_a_message_by_its_content_as_convert_does() {
    // Saved under a name that says nothing of mail, or says text.
    let dir = scratch("render-sniff");
    for name in ["mail.txt", "mail"] {
        let input = dir.join(name);
        std::fs::write(&input, eml()).unwrap();
        let crops = dir.join(format!("crops-{name}"));
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
        assert!(said.contains("1 embedded images"), "{name}: {said}");
        assert!(crops.join("mail_m1.png").exists(), "{name}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_text_file_that_is_no_message_is_converted_as_text() {
    // Told apart from a message by its content, a `.txt` that is none is
    // read by the reader its extension names, not as a PDF.
    let dir = scratch("plain-text");
    let input = dir.join("notes.txt");
    std::fs::write(&input, "Meeting notes\n\nThe budget is agreed.\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .args(["convert", "-f", "text"])
        .arg(&input)
        .env("FDOC_HOME", dir.join("home"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("The budget is agreed."), "{said}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_signatures_icons_lend_the_body_no_words_and_an_empty_figure_prints_nothing() {
    // An HTML message showing a screenshot with alternative text, a
    // signature's icons (a PNG and a GIF, each named by its alternative
    // text, as most corporate mail has them) and an image with none. The
    // icons are under the floor a model is sent: their figures keep their
    // place, without the words. The screenshot keeps its own; the image
    // with none prints no empty paragraph.
    let dir = scratch("icons");
    let input = dir.join("mail.eml");
    let gif: &[u8] = b"GIF89a\x10\x00\x10\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00\
\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00\x3b";
    let mut m = b"From: a@example.org\r\n\
Subject: Record\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"rel\"\r\n\
\r\n\
--rel\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>See the screen below.</p>\
<p><img src=\"cid:shot@x\" alt=\"Record screen\"></p>\
<p><img src=\"cid:plain@x\"></p>\
<p>That is all.</p>\
<p>Ann Smith <img src=\"cid:in@x\" alt=\"LinkedIn\"> <img src=\"cid:logo@x\" alt=\"Logo\"></p>\r\n"
        .to_vec();
    for (id, ct, bytes) in [
        ("shot@x", "image/png", screen(600, 200)),
        ("plain@x", "image/png", screen(600, 200)),
        ("in@x", "image/png", screen(24, 24)),
        ("logo@x", "image/gif", gif.to_vec()),
    ] {
        m.extend(
            format!(
                "--rel\r\nContent-Type: {ct}\r\nContent-ID: <{id}>\r\n\
                 Content-Transfer-Encoding: binary\r\n\r\n"
            )
            .bytes(),
        );
        m.extend(bytes);
        m.extend(b"\r\n");
    }
    m.extend(b"--rel--\r\n");
    std::fs::write(&input, &m).unwrap();
    let out = convert(&dir, &input, &["-f".as_ref(), "md".as_ref()]);
    assert!(out.status.success());
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(md.contains("Record screen"), "{md}");
    assert!(!md.contains("LinkedIn") && !md.contains("Logo"), "{md}");
    assert!(!md.contains("\n\n\n"), "{md:?}");
    // The figures are all there, in the graph.
    let out = convert(&dir, &input, &["-f".as_ref(), "json".as_ref()]);
    let els: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let figures = els
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "doco:Figure")
        .count();
    assert_eq!(figures, 4);
    let _ = std::fs::remove_dir_all(dir);
}
