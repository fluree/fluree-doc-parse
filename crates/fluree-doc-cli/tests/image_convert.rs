//! `fdoc convert` on a bare image: the page it stands for, and the readings
//! it takes. No engine is run: words and readings come from sidecars.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A 300 × 100 grey JPEG; with `orientation`, an EXIF segment saying how to
/// turn it upright.
fn jpeg(orientation: Option<u16>) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(300, 100, image::Rgb([200, 200, 200]));
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
    let Some(o) = orientation else {
        return out;
    };
    let mut tiff = b"MM\0*".to_vec();
    tiff.extend(8u32.to_be_bytes());
    tiff.extend(1u16.to_be_bytes());
    tiff.extend(0x0112u16.to_be_bytes());
    tiff.extend(3u16.to_be_bytes());
    tiff.extend(1u32.to_be_bytes());
    tiff.extend(o.to_be_bytes());
    tiff.extend([0, 0]);
    tiff.extend(0u32.to_be_bytes());
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend(tiff);
    let mut rotated = out[..2].to_vec();
    rotated.extend([0xFF, 0xE1]);
    rotated.extend(((app1.len() + 2) as u16).to_be_bytes());
    rotated.extend(app1);
    rotated.extend(&out[2..]);
    rotated
}

/// A scratch directory for one test.
fn scratch(test: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("fdoc-image-convert-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `fdoc convert <input> -f doco <args>`, with no reader configured: its
/// JSON-LD output.
fn convert(input: &Path, args: &[&str]) -> serde_json::Value {
    let home = input.parent().unwrap().join("home");
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .arg("convert")
        .arg(input)
        .args(["-f", "doco", "-q"])
        .args(args)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
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
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The document node of a DoCO output.
fn document(out: &serde_json::Value) -> &serde_json::Value {
    out["@graph"]
        .as_array()
        .and_then(|g| g.iter().find(|n| n.get("doc:pages").is_some()))
        .unwrap_or(out)
}

fn page_size(out: &serde_json::Value) -> (f64, f64) {
    let p = &document(out)["doc:pages"]["@value"][0];
    (p["width"].as_f64().unwrap(), p["height"].as_f64().unwrap())
}

/// A one-block page reading of `text`, as `--tier-results` reads it.
fn tier_reading(dir: &Path, stem: &str, text: &str) {
    let reading = serde_json::json!([{ "parsing_res_list": [
        { "block_label": "text", "block_content": text, "block_order": 0 }
    ]}]);
    std::fs::write(
        dir.join(format!("{stem}_p0_full.json")),
        reading.to_string(),
    )
    .unwrap();
}

#[test]
fn a_photo_turned_by_exif_is_an_upright_page() {
    // Without OCR the image is never decoded; its page is still the size the
    // reader sees it at.
    let dir = scratch("upright");
    let input = dir.join("photo.jpg");
    std::fs::write(&input, jpeg(Some(6))).unwrap();
    let tiers = dir.join("tiers");
    std::fs::create_dir_all(&tiers).unwrap();
    tier_reading(
        &tiers,
        "photo",
        "Account 4417-0093-21 opened on 15 October 1997",
    );
    let out = convert(&input, &["--tier-results", tiers.to_str().unwrap()]);
    assert_eq!(page_size(&out), (100.0, 300.0));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_short_reading_reads_the_image() {
    // A reading of the whole image read it, however little it held.
    let dir = scratch("short");
    let input = dir.join("stamp.jpg");
    std::fs::write(&input, jpeg(None)).unwrap();
    let tiers = dir.join("tiers");
    std::fs::create_dir_all(&tiers).unwrap();
    tier_reading(&tiers, "stamp", "PAID");
    let out = convert(&input, &["--tier-results", tiers.to_str().unwrap()]);
    let text = out.to_string();
    assert!(text.contains("PAID"), "{text}");
    assert!(document(&out).get("doc:unreadPages").is_none(), "{text}");
    // With no reading at all, it is unread.
    let none = dir.join("none");
    std::fs::create_dir_all(&none).unwrap();
    let out = convert(&input, &["--tier-results", none.to_str().unwrap()]);
    assert!(document(&out).get("doc:unreadPages").is_some(), "{out}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_page_reading_replaces_ocr_on_an_image() {
    // OCR words and a page reading of the same image: the reading wins, as
    // on a PDF page.
    let dir = scratch("tiers");
    let input = dir.join("scan.jpg");
    std::fs::write(&input, jpeg(None)).unwrap();
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    let reading = serde_json::json!({
        "coords": "px", "width": 300, "height": 100,
        "words": [{ "text": "Acc0unt", "box": [20, 30, 120, 50] }]
    });
    std::fs::write(words.join("scan_p0_full.json"), reading.to_string()).unwrap();
    let tiers = dir.join("tiers");
    std::fs::create_dir_all(&tiers).unwrap();
    tier_reading(&tiers, "scan", "Account");
    let ocr_only = convert(&input, &["--ocr-words", words.to_str().unwrap()]).to_string();
    assert!(ocr_only.contains("Acc0unt"), "{ocr_only}");
    let both = convert(
        &input,
        &[
            "--ocr-words",
            words.to_str().unwrap(),
            "--tier-results",
            tiers.to_str().unwrap(),
        ],
    )
    .to_string();
    assert!(
        both.contains("Account") && !both.contains("Acc0unt"),
        "{both}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A one-word OCR reading of the whole image, in its pixels.
fn words(w: f64, h: f64, text: &str) -> String {
    serde_json::json!({
        "coords": "px", "width": w, "height": h,
        "words": [{ "text": text, "box": [w * 0.1, h * 0.3, w * 0.6, h * 0.5] }]
    })
    .to_string()
}

#[test]
fn words_named_after_the_image_are_read() {
    // `fdoc ocr photo.jpg` writes `photo.json`: convert finds it.
    let dir = scratch("named");
    let input = dir.join("photo.jpg");
    std::fs::write(&input, jpeg(None)).unwrap();
    let w = dir.join("words");
    std::fs::create_dir_all(&w).unwrap();
    std::fs::write(w.join("photo.json"), words(300.0, 100.0, "Invoice")).unwrap();
    let out = convert(&input, &["--ocr-words", w.to_str().unwrap()]);
    assert!(out.to_string().contains("Invoice"), "{out}");
    assert!(document(&out).get("doc:unreadPages").is_none(), "{out}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn words_already_read_need_no_decode() {
    // A one-bit PNG stating 30,000 × 30,000 pixels: too large to decode,
    // and with words already read, nothing needs decoding.
    let dir = scratch("huge");
    let input = dir.join("huge.png");
    let mut bomb = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut bomb, 30_000, 30_000);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::One);
        let mut writer = enc.write_header().unwrap();
        let mut s = writer.stream_writer().unwrap();
        let row = vec![0xffu8; 30_000 / 8];
        for _ in 0..30_000 {
            std::io::Write::write_all(&mut s, &row).unwrap();
        }
        s.finish().unwrap();
    }
    std::fs::write(&input, bomb).unwrap();
    let w = dir.join("words");
    std::fs::create_dir_all(&w).unwrap();
    std::fs::write(
        w.join("huge_p0_full.json"),
        words(30_000.0, 30_000.0, "Settlement"),
    )
    .unwrap();
    let out = convert(&input, &["--ocr-words", w.to_str().unwrap()]);
    assert!(out.to_string().contains("Settlement"), "{out}");
    assert_eq!(page_size(&out), (30_000.0, 30_000.0));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_pages_after_the_first_of_a_tiff_are_listed_unread() {
    let dir = scratch("pages");
    let input = dir.join("fax.tif");
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut enc = tiff::encoder::TiffEncoder::new(&mut out).unwrap();
        for _ in 0..3 {
            enc.write_image::<tiff::encoder::colortype::Gray8>(300, 100, &[200u8; 30_000])
                .unwrap();
        }
    }
    std::fs::write(&input, out.into_inner()).unwrap();
    let tiers = dir.join("tiers");
    std::fs::create_dir_all(&tiers).unwrap();
    tier_reading(&tiers, "fax", "Page one only");
    let out = convert(&input, &["--tier-results", tiers.to_str().unwrap()]);
    let unread = document(&out)["doc:unreadPages"].to_string();
    for page in [1, 2] {
        assert!(
            unread.contains(&format!("\"pageIndex\":{page}")),
            "{unread}"
        );
    }
    assert!(unread.contains("LaterPage"), "{unread}");
    assert!(!unread.contains("\"pageIndex\":0"), "{unread}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_image_crop_is_upright_opaque_and_named_for_its_page() {
    // A transparent PNG: black text on nothing. Its crop is drawn over white.
    let dir = scratch("crop");
    let input = dir.join("shot.png");
    let img = image::RgbaImage::from_fn(40, 20, |x, _| {
        image::Rgba(if x < 20 { [0, 0, 0, 0] } else { [0, 0, 0, 255] })
    });
    image::DynamicImage::ImageRgba8(img)
        .save_with_format(&input, image::ImageFormat::Png)
        .unwrap();
    let crops = dir.join("crops");
    let status = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .args(["dev", "render-routed"])
        .arg(&input)
        .arg(&crops)
        .status()
        .unwrap();
    assert!(status.success());
    let manifest = std::fs::read_to_string(crops.join("manifest.jsonl")).unwrap();
    let rec: serde_json::Value = serde_json::from_str(manifest.lines().next().unwrap()).unwrap();
    assert_eq!(rec["png"], "shot_p0_full.png");
    assert_eq!(rec["kind"], "page");
    assert_eq!(rec["scale"], 1.0);
    let crop = image::open(crops.join("shot_p0_full.png"))
        .unwrap()
        .to_rgba8();
    assert_eq!(crop.dimensions(), (40, 20));
    assert_eq!(
        crop.get_pixel(5, 5).0,
        [255, 255, 255, 255],
        "paper is white"
    );
    assert_eq!(crop.get_pixel(30, 5).0, [0, 0, 0, 255], "ink stays ink");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_image_ocr_left_unread_goes_to_the_configured_model() {
    // Words for another image only, and a model configured (with a key it
    // cannot use, so nothing leaves the machine): the image goes to it.
    let dir = scratch("to-model");
    let input = dir.join("scan.jpg");
    std::fs::write(&input, jpeg(None)).unwrap();
    let w = dir.join("words");
    std::fs::create_dir_all(&w).unwrap();
    let home = dir.join("fdoc-home");
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
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .current_dir(&dir)
        .arg("convert")
        .arg(&input)
        .args(["-f", "doco", "--ocr-words"])
        .arg(&w)
        .env("FDOC_HOME", &home)
        .env_remove("FDOC_TIER_RESULTS")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("the configured model reads it"), "{err}");
    // Not reported missing before the model is even asked.
    assert!(!err.contains("This output is missing it"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A one-page text PDF.
fn text_pdf() -> Vec<u8> {
    let content = "BT /F1 12 Tf 40 700 Td (Quarterly report) Tj ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> /Contents 4 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).into_bytes());
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
fn inputs_of_one_name_are_not_cropped_into_each_other() {
    let dir = scratch("same-stem");
    let corpus = dir.join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::write(corpus.join("report.pdf"), text_pdf()).unwrap();
    std::fs::write(corpus.join("report.jpg"), jpeg(None)).unwrap();
    let crops = dir.join("crops");
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .args(["dev", "render-routed"])
        .arg(&corpus)
        .arg(&crops)
        .output()
        .unwrap();
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("another input is named report"), "{err}");
    let manifest = std::fs::read_to_string(crops.join("manifest.jsonl")).unwrap();
    assert!(manifest.lines().count() <= 1, "{manifest}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_image_reads_the_words_sized_as_it_is() {
    // A shared words directory holding a PDF page's words under the image's
    // stem: the image reads its own, sized as it is.
    let dir = scratch("sized");
    let input = dir.join("scan.jpg");
    std::fs::write(&input, jpeg(None)).unwrap();
    let w = dir.join("words");
    std::fs::create_dir_all(&w).unwrap();
    std::fs::write(
        w.join("scan_p0_full.json"),
        words(1700.0, 2200.0, "PDFPAGEWORD"),
    )
    .unwrap();
    std::fs::write(w.join("scan.json"), words(300.0, 100.0, "IMAGEWORD")).unwrap();
    let out = convert(&input, &["--ocr-words", w.to_str().unwrap()]).to_string();
    assert!(
        out.contains("IMAGEWORD") && !out.contains("PDFPAGEWORD"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ocr_lists_the_pages_after_the_first_of_a_tiff_unread() {
    let dir = scratch("pages-ocr");
    let input = dir.join("fax.tif");
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut enc = tiff::encoder::TiffEncoder::new(&mut out).unwrap();
        for _ in 0..3 {
            enc.write_image::<tiff::encoder::colortype::Gray8>(300, 100, &[200u8; 30_000])
                .unwrap();
        }
    }
    std::fs::write(&input, out.into_inner()).unwrap();
    let w = dir.join("words");
    std::fs::create_dir_all(&w).unwrap();
    std::fs::write(w.join("fax.json"), words(300.0, 100.0, "Cover")).unwrap();
    let out = convert(&input, &["--ocr-words", w.to_str().unwrap()]);
    let unread = document(&out)["doc:unreadPages"].to_string();
    assert!(
        unread.contains("\"pageIndex\":2") && unread.contains("LaterPage"),
        "{unread}"
    );
    assert!(out.to_string().contains("Cover"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_image_ocr_read_nothing_of_is_reported_missing_without_a_model() {
    let dir = scratch("missing");
    let input = dir.join("scan.jpg");
    std::fs::write(&input, jpeg(None)).unwrap();
    let w = dir.join("words");
    std::fs::create_dir_all(&w).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .arg("convert")
        .arg(&input)
        .args(["-f", "doco", "--ocr-words"])
        .arg(&w)
        .env("FDOC_HOME", dir.join("no-config"))
        .env_remove("FDOC_TIER_RESULTS")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("This output is missing it"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}
