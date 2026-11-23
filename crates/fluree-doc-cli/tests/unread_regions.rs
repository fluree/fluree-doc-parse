//! `fdoc convert` on a text page with a screenshot pasted into it: the
//! region nothing read is reported, in `doc:unreadRegions` and on stderr,
//! and stops being reported once something reads it. No engine and no model
//! is run: readings come from sidecars.

mod common;

use common::{capture, pdf};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the capture sits, as `doc:bbox` writes it: 500 × 150 units, a
/// sixth of a Letter page, well over the router's tenth.
const REGION: &str = "56.00,582.00,556.00,732.00";

/// A Letter page of prose with, under it, a capture of two lines of text
/// and no glyph over it; `with_capture: false` is the same page without it.
fn page(with_capture: bool) -> Vec<u8> {
    page_placing(with_capture.then(|| ("500 0 0 150 56 60", capture())))
}

/// A grey image: its width and height in pixels, and one byte a pixel.
type Gray = (u32, u32, Vec<u8>);

/// The same page of prose with, when given, a grey image placed by this
/// `cm` matrix.
fn page_placing(image: Option<(&str, Gray)>) -> Vec<u8> {
    let body = "This memo summarises the account review and the record shown below";
    let mut ops: Vec<String> = (0..30)
        .map(|i| format!("BT /F1 10 Tf 56 {} Td ({body}) Tj ET", 760 - i * 14))
        .collect();
    let mut images = Vec::new();
    if let Some((cm, (w, h, gray))) = image {
        ops.push(format!("q {cm} cm /I0 Do Q"));
        images.push((
            "I0".to_string(),
            format!(
                "/Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {}",
                gray.len()
            ),
            gray,
        ));
    }
    pdf(
        &[(612.0, 792.0)],
        &ops.join("\n"),
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        &images,
    )
}

/// A form: 30 lines of prose, each after a checkbox 10 units a side drawn
/// as a stencil (a 1-bit image mask), and, when asked, the capture an inch
/// high under them.
fn form(with_capture: bool) -> Vec<u8> {
    let mask = vec![0xFF, 0x81, 0x81, 0x81, 0x81, 0x81, 0x81, 0xFF];
    let mut ops = Vec::new();
    for i in 0..30 {
        let y = 760 - i * 14;
        ops.push(format!(
            "BT /F1 10 Tf 76 {y} Td (I agree with the terms stated above.) Tj ET"
        ));
        ops.push(format!("q 10 0 0 10 56 {} cm /M0 Do Q", y - 1));
    }
    let mut images = vec![(
        "M0".to_string(),
        format!(
            "/Width 8 /Height 8 /ImageMask true /BitsPerComponent 1 /Length {}",
            mask.len()
        ),
        mask,
    )];
    if with_capture {
        let (w, h, gray) = capture();
        ops.push("q 400 0 0 60 56 60 cm /I0 Do Q".into());
        images.push((
            "I0".to_string(),
            format!(
                "/Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {}",
                gray.len()
            ),
            gray,
        ));
    }
    pdf(
        &[(612.0, 792.0)],
        &ops.join("\n"),
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        &images,
    )
}

/// A scratch directory for one test, holding `memo.pdf`.
fn scratch(test: &str, bytes: &[u8]) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("fdoc-unread-regions-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("memo.pdf"), bytes).unwrap();
    dir
}

/// A subdirectory of `dir` holding `files`.
fn sidecars(dir: &Path, name: &str, files: &[(&str, String)]) -> PathBuf {
    let d = dir.join(name);
    std::fs::create_dir_all(&d).unwrap();
    for (file, body) in files {
        std::fs::write(d.join(file), body).unwrap();
    }
    d
}

/// `fdoc convert memo.pdf -f doco <args>` with no reader configured: the
/// document node, stderr, and the whole graph as text.
fn convert(dir: &Path, args: &[&str]) -> (serde_json::Value, String, String) {
    let home = dir.join("home");
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .arg("convert")
        .arg(dir.join("memo.pdf"))
        .args(["-f", "doco", "--no-escalate"])
        .args(args)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("FDOC_HOME", &home)
        .env_remove("FDOC_OCR_WORDS")
        .env_remove("FDOC_TIER_RESULTS")
        .env_remove("FDOC_STRUCTURE_RESULTS")
        .env_remove("FDOC_TITLE_BOXES")
        .env_remove("FDOC_VLM_ANCHORS")
        .env_remove("FDOC_OCR_MODELS")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    let graph: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let text = graph.to_string();
    let doc = graph["@graph"]
        .as_array()
        .and_then(|g| g.iter().find(|n| n.get("doc:pages").is_some()))
        .cloned()
        .unwrap_or(graph);
    (doc, stderr, text)
}

/// A one-block reading of `text`, as `--tier-results` reads it.
fn tier_reading(text: &str) -> String {
    serde_json::json!([{ "parsing_res_list": [
        { "block_label": "text", "block_content": text, "block_order": 0 }
    ]}])
    .to_string()
}

#[test]
fn a_region_nothing_read_is_reported_with_its_box() {
    let dir = scratch("none", &page(true));
    let (doc, stderr, _) = convert(&dir, &[]);
    let regions = &doc["doc:unreadRegions"]["@value"];
    assert_eq!(
        regions,
        &serde_json::json!([{ "pageIndex": 0, "bbox": REGION, "reason": "Region" }]),
        "{doc}"
    );
    // A page of prose is not an unread page: only its region is.
    assert!(doc.get("doc:unreadPages").is_none(), "{doc}");
    assert!(
        stderr.contains(
            "warning: memo: fluree-doc-parse: 1 raster region carries content no reader \
             transcribed: page 1 at 56,582,556,732"
        ),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_model_reading_of_the_region_reads_it() {
    let dir = scratch("tier", &page(true));
    let tiers = sidecars(
        &dir,
        "tiers",
        &[("memo_p0_r0.json", tier_reading("Account 4417 ref"))],
    );
    let (doc, stderr, text) = convert(&dir, &["--tier-results", tiers.to_str().unwrap()]);
    assert!(text.contains("region-splice"), "{text}");
    assert!(doc.get("doc:unreadRegions").is_none(), "{doc}");
    assert!(!stderr.contains("raster region"), "{stderr}");
    // No reading of it, or an empty one: still unread.
    for (name, files) in [
        ("missing", vec![]),
        ("empty", vec![("memo_p0_r0.json", tier_reading(""))]),
    ] {
        let tiers = sidecars(&dir, name, &files);
        let (doc, _, _) = convert(&dir, &["--tier-results", tiers.to_str().unwrap()]);
        assert!(doc.get("doc:unreadRegions").is_some(), "{name}: {doc}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_model_reading_of_the_whole_page_reads_the_region() {
    let dir = scratch("page-tier", &page(true));
    let body = "This memo summarises the account review and the record shown below\n".repeat(30);
    let tiers = sidecars(
        &dir,
        "tiers",
        &[(
            "memo_p0_full.json",
            tier_reading(&format!("{body}Account 4417 ref")),
        )],
    );
    let (doc, _, text) = convert(&dir, &["--tier-results", tiers.to_str().unwrap()]);
    assert!(text.contains("page-tier"), "{text}");
    assert!(doc.get("doc:unreadRegions").is_none(), "{doc}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_short_model_reading_of_a_scan_reads_the_page() {
    // A page that is one image and no text: a scan. A reading of the whole
    // page read it, however short, as an OCR reading of it would.
    let (w, h, gray) = capture();
    let image = (
        "I0".to_string(),
        format!(
            "/Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {}",
            gray.len()
        ),
        gray,
    );
    let dir = scratch(
        "short-scan",
        &pdf(
            &[(612.0, 792.0)],
            "q 612 0 0 792 0 0 cm /I0 Do Q",
            "",
            &[image],
        ),
    );
    let (doc, _, _) = convert(&dir, &[]);
    assert!(doc.get("doc:unreadPages").is_some(), "{doc}");
    let tiers = sidecars(
        &dir,
        "tiers",
        &[("memo_p0_full.json", tier_reading("Short note"))],
    );
    let (doc, stderr, text) = convert(&dir, &["--tier-results", tiers.to_str().unwrap()]);
    assert!(text.contains("Short note"), "{text}");
    assert!(doc.get("doc:unreadPages").is_none(), "{doc}");
    assert!(!stderr.contains("no reader transcribed"), "{stderr}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ocr_words_of_one_region_leave_the_other_reported() {
    // A short page whose second font names its glyphs with nothing a
    // reader can map to Unicode: 40 glyphs resolve, 55 do not, under the
    // 100 glyph events the router needs before it judges their Unicode.
    // Two captures, the upper read by OCR words: their glyph events, counted,
    // would route the page as broken text and leave the lower one unread
    // and unreported.
    let names: String = (0..26).map(|i| format!("/zq{i} ")).collect();
    let fonts = format!(
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> \
         /F2 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
         /Encoding << /Type /Encoding /Differences [97 {names}] >> >> >>"
    );
    let mut ops = vec![
        "BT /F1 10 Tf 56 740 Td (ACCOUNTREVIEWOFRECORD) Tj ET".to_string(),
        "BT /F1 10 Tf 56 726 Td (SHOWNBELOWASDRAWN) Tj ET".to_string(),
        "BT /F2 10 Tf 56 712 Td (abcdefghijklmnopqrstuvwxyz) Tj ET".to_string(),
        "BT /F2 10 Tf 56 698 Td (abcdefghijklmnopqrstuvwxyz) Tj ET".to_string(),
        "BT /F2 10 Tf 56 684 Td (abc) Tj ET".to_string(),
    ];
    ops.push("q 500 0 0 150 56 400 cm /I0 Do Q".into());
    ops.push("q 500 0 0 150 56 60 cm /I0 Do Q".into());
    let (w, h, gray) = capture();
    let bytes = pdf(
        &[(612.0, 792.0)],
        &ops.join("\n"),
        &fonts,
        &[(
            "I0".to_string(),
            format!(
                "/Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {}",
                gray.len()
            ),
            gray,
        )],
    );
    let dir = scratch("broken-short", &bytes);
    let (doc, _, _) = convert(&dir, &[]);
    assert_eq!(
        doc["doc:unreadRegions"]["@value"],
        serde_json::json!([
            { "pageIndex": 0, "bbox": "56.00,242.00,556.00,392.00", "reason": "Region" },
            { "pageIndex": 0, "bbox": REGION, "reason": "Region" }
        ]),
        "{doc}"
    );
    let words = serde_json::json!({
        "coords": "page",
        "words": [{ "text": "Account 4417 ref", "box": [80, 260, 300, 280] }]
    });
    let w = sidecars(&dir, "words", &[("memo_p0_r0.json", words.to_string())]);
    let (doc, stderr, text) = convert(&dir, &["--ocr-words", w.to_str().unwrap()]);
    assert!(text.contains("Account 4417 ref"), "{text}");
    assert_eq!(
        doc["doc:unreadRegions"]["@value"],
        serde_json::json!([{ "pageIndex": 0, "bbox": REGION, "reason": "Region" }]),
        "{doc}"
    );
    assert!(stderr.contains("page 1 at 56,582,556,732"), "{stderr}");
    let (_, _, text) = convert(
        &dir,
        &["--ocr-words", w.to_str().unwrap(), "--emit-anchors"],
    );
    assert!(text.contains("[[VLM:p0:r1]]"), "{text}");
    assert!(!text.contains("[[VLM:p0:r0]]"), "{text}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ocr_words_of_the_region_read_it() {
    let dir = scratch("ocr", &page(true));
    let words = serde_json::json!({
        "coords": "page",
        "words": [{ "text": "Account 4417 ref", "box": [80, 600, 300, 620] }]
    });
    let w = sidecars(&dir, "words", &[("memo_p0_r0.json", words.to_string())]);
    let (doc, stderr, text) = convert(&dir, &["--ocr-words", w.to_str().unwrap()]);
    assert!(text.contains("Account 4417 ref"), "{text}");
    assert!(doc.get("doc:unreadRegions").is_none(), "{doc}");
    assert!(!stderr.contains("raster region"), "{stderr}");
    let _ = std::fs::remove_dir_all(dir);
}

/// The page of [`page`] with, between its prose and its capture, a second
/// capture under a caption the page draws twice in one place, as a
/// producer drawing bold may: 22 glyphs over it as drawn, 11 once analysis
/// drops the copy.
fn page_with_a_caption_drawn_twice() -> Vec<u8> {
    let body = "This memo summarises the account review and the record shown below";
    let mut ops: Vec<String> = (0..30)
        .map(|i| format!("BT /F1 10 Tf 56 {} Td ({body}) Tj ET", 760 - i * 14))
        .collect();
    ops.push("q 500 0 0 120 56 222 cm /I0 Do Q".into());
    ops.push("BT /F1 11 Tf 100 280 Td (Figure 1 data) Tj ET".into());
    ops.push("BT /F1 11 Tf 100 280 Td (Figure 1 data) Tj ET".into());
    ops.push("q 500 0 0 150 56 60 cm /I0 Do Q".into());
    let (w, h, gray) = capture();
    pdf(
        &[(612.0, 792.0)],
        &ops.join("\n"),
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        &[(
            "I0".to_string(),
            format!(
                "/Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {}",
                gray.len()
            ),
            gray,
        )],
    )
}

#[test]
fn a_caption_drawn_twice_over_a_capture_keeps_one_region_numbering() {
    // As drawn, the text layer covers the upper capture and the lower one
    // is the page's one region, r0: its crop, the OCR words for it, its
    // splice anchor and the report all name it so. Numbered on the page
    // analysis cleaned, the upper capture would be r0 and the lower r1.
    let dir = scratch("caption-twice", &page_with_a_caption_drawn_twice());
    let crops = dir.join("crops");
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .args(["dev", "render-routed"])
        .arg(dir.join("memo.pdf"))
        .arg(&crops)
        .arg("--ocr-regions")
        .env_remove("FDOC_LAYOUT_BOXES")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let manifest = std::fs::read_to_string(crops.join("manifest.jsonl")).unwrap();
    let regions: Vec<(String, serde_json::Value)> = manifest
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|r| r["kind"] == "region")
        .map(|r| (r["png"].as_str().unwrap().to_string(), r["bbox"].clone()))
        .collect();
    assert_eq!(
        regions,
        [(
            "memo_p0_r0.png".to_string(),
            serde_json::json!([56.0, 582.0, 556.0, 732.0])
        )],
        "{manifest}"
    );

    let (doc, stderr, _) = convert(&dir, &[]);
    assert_eq!(
        doc["doc:unreadRegions"]["@value"],
        serde_json::json!([{ "pageIndex": 0, "bbox": REGION, "reason": "Region" }]),
        "{doc}"
    );
    assert!(stderr.contains("page 1 at 56,582,556,732"), "{stderr}");

    let (_, _, text) = convert(&dir, &["--emit-anchors"]);
    assert!(text.contains("[[VLM:p0:r0]]"), "{text}");
    assert!(!text.contains("[[VLM:p0:r1]]"), "{text}");
    let graph: serde_json::Value = serde_json::from_str(&text).unwrap();
    let anchor = graph["@graph"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n.to_string().contains("[[VLM:p0:r0]]"))
        .unwrap();
    assert!(anchor.to_string().contains(REGION), "{anchor}");

    let words = serde_json::json!({
        "coords": "page",
        "words": [{ "text": "Account 4417 ref", "box": [80, 600, 300, 620] }]
    });
    let w = sidecars(&dir, "words", &[("memo_p0_r0.json", words.to_string())]);
    let (doc, stderr, text) = convert(&dir, &["--ocr-words", w.to_str().unwrap()]);
    assert!(text.contains("Account 4417 ref"), "{text}");
    assert!(doc.get("doc:unreadRegions").is_none(), "{doc}");
    assert!(!stderr.contains("raster region"), "{stderr}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_page_without_a_capture_says_nothing() {
    let dir = scratch("clean", &page(false));
    let (doc, stderr, _) = convert(&dir, &[]);
    assert!(doc.get("doc:unreadRegions").is_none(), "{doc}");
    assert!(!stderr.contains("fluree-doc-parse:"), "{stderr}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_page_not_asked_for_is_not_reported() {
    // `--pages` leaves the page out of the output; what it carries was not
    // read because nobody asked, which the output need not say.
    let dir = scratch("pages", &page(true));
    let (doc, _, _) = convert(&dir, &["--pages", "1"]);
    assert!(doc.get("doc:unreadRegions").is_some(), "{doc}");
    let (doc, stderr, _) = convert(&dir, &["--pages", "2"]);
    assert!(doc.get("doc:unreadRegions").is_none(), "{doc}");
    assert!(!stderr.contains("raster region"), "{stderr}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_capture_too_small_for_a_model_is_listed_without_a_warning() {
    // The same capture an inch high: under a tenth of the page, so the
    // router sends no model, and lists it for the caller to judge. Not a
    // warning: as often as not such a region is a letterhead's logo.
    let dir = scratch(
        "small",
        &page_placing(Some(("400 0 0 60 56 60", capture()))),
    );
    let (doc, stderr, _) = convert(&dir, &[]);
    assert_eq!(
        doc["doc:unreadRegions"]["@value"],
        serde_json::json!([{ "pageIndex": 0, "bbox": "56.00,672.00,456.00,732.00", "reason": "Small" }]),
        "{doc}"
    );
    assert!(!stderr.contains("fluree-doc-parse:"), "{stderr}");
    // OCR words for it (the first region an engine reads): read, so gone.
    let words = serde_json::json!({
        "coords": "page",
        "words": [{ "text": "Account 4417 ref", "box": [70, 680, 300, 700] }]
    });
    let w = sidecars(&dir, "words", &[("memo_p0_r0.json", words.to_string())]);
    let (doc, _, text) = convert(&dir, &["--ocr-words", w.to_str().unwrap()]);
    assert!(text.contains("Account 4417 ref"), "{text}");
    assert!(doc.get("doc:unreadRegions").is_none(), "{doc}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_photograph_is_listed_as_not_texty_without_a_warning() {
    // A flat grey image the probe cannot take for text, a sixth of the page.
    let photo = (64, 64, vec![128u8; 64 * 64]);
    let dir = scratch("photo", &page_placing(Some(("500 0 0 150 56 60", photo))));
    let (doc, stderr, _) = convert(&dir, &[]);
    assert_eq!(
        doc["doc:unreadRegions"]["@value"],
        serde_json::json!([{ "pageIndex": 0, "bbox": REGION, "reason": "NotTexty" }]),
        "{doc}"
    );
    assert!(!stderr.contains("fluree-doc-parse:"), "{stderr}");
    let _ = std::fs::remove_dir_all(dir);
}

/// The text of each element of a DoCO graph, in order.
fn texts(graph: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(graph).unwrap();
    v["@graph"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["nif:isString"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn a_photograph_is_read_by_nothing_and_a_captures_noise_leaves_it_listed() {
    // The probe does not take the photograph for text: no engine is offered
    // it, so even a sure reading named after it is not one of its regions.
    let photo = (64, 64, vec![128u8; 64 * 64]);
    let dir = scratch(
        "photo-noise",
        &page_placing(Some(("500 0 0 150 56 60", photo))),
    );
    let (_, _, plain) = convert(&dir, &[]);
    let reading = serde_json::json!({ "coords": "page", "words": [
        { "text": "Sunset over the bay", "box": [80, 600, 300, 620], "conf": 0.99 }
    ]});
    let w = sidecars(&dir, "words", &[("memo_p0_r0.json", reading.to_string())]);
    let (doc, stderr, text) = convert(&dir, &["--ocr-words", w.to_str().unwrap()]);
    assert_eq!(texts(&text), texts(&plain), "{text}");
    assert_eq!(
        doc["doc:unreadRegions"]["@value"],
        serde_json::json!([{ "pageIndex": 0, "bbox": REGION, "reason": "NotTexty" }]),
        "{doc}"
    );
    assert!(!stderr.contains("fluree-doc-parse:"), "{stderr}");
    let _ = std::fs::remove_dir_all(dir);
    // A capture the probe takes for text is read, and its reading held to
    // the noise rule: a stroke or two, unsure; fragments, not sure enough
    // to be a chart's; or one fragment boxed over the whole crop (its
    // margin included), however sure, or scored not at all. Set aside, the
    // capture stays listed.
    let dir = scratch(
        "small-noise",
        &page_placing(Some(("400 0 0 60 56 60", capture()))),
    );
    let (_, _, plain) = convert(&dir, &[]);
    for words in [
        serde_json::json!([
            { "text": "M", "box": [80, 676, 140, 726], "conf": 0.46 },
            { "text": "—", "box": [300, 690, 360, 700], "conf": 0.1 }
        ]),
        foliage_words(1.0),
        serde_json::json!([{ "text": "7", "box": [50, 666, 462, 738], "conf": 0.66 }]),
        serde_json::json!([{ "text": "2", "box": [50, 666, 462, 738] }]),
    ] {
        let reading = serde_json::json!({ "coords": "page", "words": words });
        let w = sidecars(&dir, "words", &[("memo_p0_r0.json", reading.to_string())]);
        let (doc, stderr, text) = convert(&dir, &["--ocr-words", w.to_str().unwrap()]);
        assert_eq!(texts(&text), texts(&plain), "{text}");
        assert_eq!(
            doc["doc:unreadRegions"]["@value"],
            serde_json::json!([{ "pageIndex": 0, "bbox": "56.00,672.00,456.00,732.00", "reason": "Small" }]),
            "{doc}"
        );
        assert!(!stderr.contains("fluree-doc-parse:"), "{stderr}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// What the built-in engine reads in a photograph of foliage, its scores
/// on a scale of `scale` (1, or a Tesseract's 100): fragments of one or two
/// letters or figures, at a mean of 0.6, as sure as real text may be.
fn foliage_words(scale: f64) -> serde_json::Value {
    let words = [
        ("I7", 0.59),
        ("1", 0.96),
        ("J", 0.38),
        ("X", 0.58),
        ("米", 0.5),
    ];
    let words: Vec<_> = (words.iter().enumerate())
        .map(|(i, (text, conf))| {
            let x = 80 + 90 * i;
            serde_json::json!({ "text": text, "box": [x, 690, x + 12, 702], "conf": conf * scale })
        })
        .collect();
    serde_json::Value::Array(words)
}

/// A 800 × 600 px photograph with nothing in it to read: smooth grey
/// noise, as light and shade fall.
#[cfg(feature = "ocr")]
fn smooth_noise() -> Gray {
    let mut seed = 0x9e37_79b9_u32;
    let coarse = image::GrayImage::from_fn(80, 60, |_, _| {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        image::Luma([(seed >> 24) as u8])
    });
    let fine = image::imageops::resize(&coarse, 800, 600, image::imageops::FilterType::CatmullRom);
    let img = image::imageops::blur(&fine, 6.0);
    (800, 600, img.into_raw())
}

/// A 800 × 600 px gradient, darker towards one corner.
#[cfg(feature = "ocr")]
fn gradient() -> Gray {
    let img = image::GrayImage::from_fn(800, 600, |x, y| {
        image::Luma([(x * 255 / 1600 + y * 255 / 1200) as u8])
    });
    (800, 600, img.into_raw())
}

/// A 1200 × 800 px field of foliage: 3000 short dark strokes, two pixels
/// wide, on a mid-grey ground, softened as a photograph's are. The engine
/// reads dozens of one- and two-letter "words" in it, as sure of them in the
/// mean (about 0.6) as of a date stamped in a corner of a landscape.
#[cfg(feature = "ocr")]
fn foliage() -> Gray {
    let (w, h) = (1200_i64, 800_i64);
    let mut img = image::GrayImage::from_pixel(w as u32, h as u32, image::Luma([140]));
    let mut seed = 0x2545_f491_u32;
    let mut next = |n: i64| {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        i64::from(seed >> 8) % n
    };
    for _ in 0..3000 {
        let (x, y) = (next(w), next(h));
        let (dx, dy) = (2 + next(13), next(29) - 14);
        let steps = dx.abs().max(dy.abs());
        for k in 0..=steps {
            let (px, py) = (x + dx * k / steps, y + dy * k / steps);
            for (ox, oy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                if (0..w).contains(&(px + ox)) && (0..h).contains(&(py + oy)) {
                    img.put_pixel((px + ox) as u32, (py + oy) as u32, image::Luma([20]));
                }
            }
        }
    }
    let img = image::imageops::blur(&img, 0.8);
    (1200, 800, img.into_raw())
}

#[cfg(feature = "ocr")]
#[test]
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn an_engine_reads_no_text_into_a_photograph_or_a_gradient() {
    let models = std::env::var("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS");
    // A quarter of the page under the prose, as a report's illustration.
    const PICTURE: &str = "72.00,472.00,472.00,772.00";
    for (name, picture) in [
        ("photo", smooth_noise()),
        ("gradient", gradient()),
        ("foliage", foliage()),
    ] {
        let dir = scratch(
            &format!("{name}-engine"),
            &page_placing(Some(("400 0 0 300 72 20", picture))),
        );
        let (_, _, plain) = convert(&dir, &[]);
        let (doc, _, text) = convert(&dir, &["--ocr", "oar-ocr", "--models", &models]);
        assert_eq!(texts(&text), texts(&plain), "{name}: {text}");
        assert_eq!(
            doc["doc:unreadRegions"]["@value"],
            serde_json::json!([{ "pageIndex": 0, "bbox": PICTURE, "reason": "NotTexty" }]),
            "{name}: {doc}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A 1366 × 768 px screen capture, a light ground with five short lines of
/// text at its top left and nothing under them, as issue #10's capture:
/// too little of it ink for the probe's first rule, lines of text for its
/// second.
fn screen() -> (u32, u32, Vec<u8>) {
    let lines = [
        "Record",
        "Name: Alex Sample",
        "Date of birth: 14 MAR 1987",
        "Reference: 4417-0093-21",
        "Address: 12 Example Road, Springfield 01101",
    ];
    let mut ops = vec!["0.96 g 0 0 683 384 re f 0 g".to_string()];
    for (i, l) in lines.iter().enumerate() {
        ops.push(format!(
            "BT /F1 7 Tf 20 {} Td ({l}) Tj ET",
            360 - 14 * i as i32
        ));
    }
    let text = pdf(
        &[(683.0, 384.0)],
        &ops.join("\n"),
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        &[],
    );
    let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(text)).unwrap();
    let mut out = (0, 0, Vec::new());
    fluree_doc_pdf::escalate::for_each_crop_scaled(
        &raw,
        &vec![(0, None)],
        |_, _| 2.0,
        |c| {
            let gray = c.pixels.data.chunks_exact(4).map(|p| p[0]).collect();
            out = (c.pixels.width, c.pixels.height, gray);
        },
    );
    out
}

/// Where [`screen`] sits on the page of prose, as `doc:bbox` writes it:
/// 468 × 263 units, a quarter of the page.
const SCREEN: &str = "72.00,469.00,540.00,732.00";

#[test]
fn a_screen_with_a_few_lines_at_its_top_is_routed_as_text() {
    let dir = scratch(
        "screen",
        &page_placing(Some(("468 0 0 263 72 60", screen()))),
    );
    // The probe takes it for text: a quarter of the page, it is routed to a
    // reader, and warned about when nothing read it.
    let (doc, stderr, _) = convert(&dir, &[]);
    assert_eq!(
        doc["doc:unreadRegions"]["@value"],
        serde_json::json!([{ "pageIndex": 0, "bbox": SCREEN, "reason": "Region" }]),
        "{doc}"
    );
    assert!(
        stderr.contains("1 raster region carries content"),
        "{stderr}"
    );
    // The first region read on the page.
    let words = serde_json::json!({
        "coords": "page",
        "words": [{ "text": "Name: Alex Sample", "box": [82, 490, 200, 500], "conf": 0.97 }]
    });
    let w = sidecars(&dir, "words", &[("memo_p0_r0.json", words.to_string())]);
    let (doc, _, text) = convert(&dir, &["--ocr-words", w.to_str().unwrap()]);
    assert!(text.contains("Alex Sample"), "{text}");
    assert!(doc.get("doc:unreadRegions").is_none(), "{doc}");
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(feature = "ocr")]
#[test]
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn a_screen_with_a_few_lines_at_its_top_is_read_by_the_engine() {
    let models = std::env::var("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS");
    let dir = scratch(
        "screen-engine",
        &page_placing(Some(("468 0 0 263 72 60", screen()))),
    );
    let (doc, _, text) = convert(&dir, &["--ocr", "oar-ocr", "--models", &models]);
    assert!(text.contains("Alex Sample"), "{text}");
    assert!(text.contains("4417-0093-21"), "{text}");
    assert!(doc.get("doc:unreadRegions").is_none(), "{doc}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A 24 x 24 stencil: a box two pixels thick, filled when `filled`.
fn checkbox_mask(filled: bool) -> Vec<u8> {
    (0..24u32)
        .flat_map(|y| {
            let row: Vec<bool> = (0..24u32)
                .map(|x| filled || x < 2 || y < 2 || x >= 22 || y >= 22)
                .collect();
            // A set bit is unpainted under the default Decode.
            row.chunks(8)
                .map(|c| c.iter().fold(0u8, |b, &ink| (b << 1) | u8::from(!ink)))
                .collect::<Vec<u8>>()
        })
        .collect()
}

/// A page of prose with, under it, `empty` boxes drawn from one 24 x 24
/// stencil and `ticked` from another, each 12 units a side, 20 to a row.
fn checkbox_form(empty: usize, ticked: usize) -> Vec<u8> {
    let mut ops: Vec<String> = (0..40)
        .map(|i| {
            format!(
                "BT /F1 10 Tf 56 {} Td (The applicant confirms the statements below.) Tj ET",
                760 - i * 12
            )
        })
        .collect();
    for k in 0..empty + ticked {
        let name = if k < empty { "M0" } else { "M1" };
        let (x, y) = (56 + (k % 20) * 25, 40 + (k / 20) * 16);
        ops.push(format!("q 12 0 0 12 {x} {y} cm /{name} Do Q"));
    }
    let mask = |name: &str, filled: bool| {
        let m = checkbox_mask(filled);
        (
            name.to_string(),
            format!(
                "/Width 24 /Height 24 /ImageMask true /BitsPerComponent 1 /Length {}",
                m.len()
            ),
            m,
        )
    };
    pdf(
        &[(612.0, 792.0)],
        &ops.join("\n"),
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        &[mask("M0", false), mask("M1", true)],
    )
}

#[test]
fn a_checkbox_drawn_two_hundred_times_is_cropped_once() {
    // Each box holds the pixels of a line and is texty, as a stencil is, so
    // an OCR engine is offered it; drawn from one image at one size, the
    // two hundred show the same pixels and are read once. The other
    // stencil is another image, read once too.
    let dir = scratch("checkboxes", &checkbox_form(200, 3));
    let crops = dir.join("crops");
    let out = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .args(["dev", "render-routed"])
        .arg(dir.join("memo.pdf"))
        .arg(&crops)
        .args(["--ocr-regions", "--stored-resolution"])
        .env_remove("FDOC_LAYOUT_BOXES")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let manifest = std::fs::read_to_string(crops.join("manifest.jsonl")).unwrap();
    let regions: Vec<String> = manifest
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|r| r["kind"] == "region")
        .map(|r| r["png"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        regions,
        ["memo_p0_r0.png", "memo_p0_r200.png"],
        "{manifest}"
    );

    // The one reading of the box, in its crop's pixels, is laid on every
    // box drawn from the same image, and on none of the others.
    let words = serde_json::json!({
        "coords": "px", "width": 100, "height": 100,
        "words": [{ "text": "Yes", "box": [30, 40, 70, 60], "conf": 0.99 }]
    });
    let w = sidecars(&dir, "words", &[("memo_p0_r0.json", words.to_string())]);
    let (_, stderr, text) = convert(&dir, &["--ocr-words", w.to_str().unwrap(), "-v"]);
    let taken: Vec<&str> = (stderr.lines())
        .filter(|l| l.contains(": 1 words taken"))
        .collect();
    assert_eq!(taken.len(), 200, "{stderr}");
    assert!(taken[0].starts_with("ocr: memo_p0_r0:"), "{stderr}");
    assert!(taken[199].starts_with("ocr: memo_p0_r199:"), "{stderr}");
    // Each box's word is in the output, an element's `nif:isString` and
    // `rdfs:label`.
    assert_eq!(text.matches("Yes").count(), 2 * 200, "{text}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_form_with_stencil_checkboxes_lists_nothing() {
    // A stencil is texty to the probe whatever it draws, and each box is in
    // a glyph void; under the side a region needs to be worth listing.
    let dir = scratch("form", &form(false));
    let (doc, stderr, _) = convert(&dir, &[]);
    assert!(doc.get("doc:unreadRegions").is_none(), "{doc}");
    assert!(!stderr.contains("fluree-doc-parse:"), "{stderr}");
    let _ = std::fs::remove_dir_all(dir);

    // A capture on the same form is listed. The 30 boxes before it are 8 × 8
    // masks, under the pixels a line of text needs, so no engine is offered
    // them either and OCR words for the capture name it region 0.
    let dir = scratch("form-capture", &form(true));
    let (doc, _, _) = convert(&dir, &[]);
    assert_eq!(
        doc["doc:unreadRegions"]["@value"],
        serde_json::json!([{ "pageIndex": 0, "bbox": "56.00,672.00,456.00,732.00", "reason": "Small" }]),
        "{doc}"
    );
    let words = serde_json::json!({
        "coords": "page",
        "words": [{ "text": "Account 4417 ref", "box": [70, 680, 300, 700] }]
    });
    let w = sidecars(&dir, "words", &[("memo_p0_r0.json", words.to_string())]);
    let (doc, _, text) = convert(&dir, &["--ocr-words", w.to_str().unwrap()]);
    assert!(text.contains("Account 4417 ref"), "{text}");
    assert!(doc.get("doc:unreadRegions").is_none(), "{doc}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_model_reading_of_a_pasted_screen_keeps_the_documents_outline() {
    // A reading of the region naming the screen's title and a heading of
    // it: they are the screen's, read as paragraphs, as OCR's are.
    let reading = serde_json::json!([{ "parsing_res_list": [
        { "block_label": "doc_title", "block_content": "SCREENSHOT", "block_order": 0 },
        { "block_label": "text", "block_content": "## Balance 9,876.54", "block_order": 1 }
    ]}])
    .to_string();
    let dir = scratch("screen-headings", &page(true));
    let tiers = sidecars(&dir, "tiers", &[("memo_p0_r0.json", reading.clone())]);
    let (_, _, text) = convert(&dir, &["--tier-results", tiers.to_str().unwrap()]);
    assert!(text.contains("SCREENSHOT\\n\\nBalance 9,876.54"), "{text}");
    let _ = std::fs::remove_dir_all(dir);
    // A region covering most of a page under a few lines of text of its own
    // is the page's content, a scan under a stamped header: its headings
    // are the document's.
    let line = "RECEIVED BY FAX FROM THE BRANCH OFFICE AND FORWARDED FOR REVIEW";
    let mut ops: Vec<String> = (0..3)
        .map(|i| format!("BT /F1 10 Tf 26 {} Td ({line}) Tj ET", 760 - i * 14))
        .collect();
    ops.push("q 560 0 0 600 26 60 cm /I0 Do Q".into());
    let (w, h, gray) = capture();
    let scan = pdf(
        &[(612.0, 792.0)],
        &ops.join("\n"),
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        &[(
            "I0".to_string(),
            format!(
                "/Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {}",
                gray.len()
            ),
            gray.clone(),
        )],
    );
    let dir = scratch("scan-headings", &scan);
    let tiers = sidecars(&dir, "tiers", &[("memo_p0_r0.json", reading.clone())]);
    let (_, _, text) = convert(&dir, &["--tier-results", tiers.to_str().unwrap()]);
    assert!(text.contains("region-splice"), "{text}");
    assert!(text.contains("# SCREENSHOT"), "{text}");
    let _ = std::fs::remove_dir_all(dir);
    // The same scan stored in two strips, each under half the page: routed
    // a region each, and together still the page's content.
    ops.pop();
    ops.push("q 560 0 0 300 26 360 cm /I0 Do Q".into());
    ops.push("q 560 0 0 300 26 60 cm /I1 Do Q".into());
    let image = |name: &str| {
        (
            name.to_string(),
            format!(
                "/Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {}",
                gray.len()
            ),
            gray.clone(),
        )
    };
    let strips = pdf(
        &[(612.0, 792.0)],
        &ops.join("\n"),
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        &[image("I0"), image("I1")],
    );
    let dir = scratch("strip-headings", &strips);
    let tiers = sidecars(&dir, "tiers", &[("memo_p0_r0.json", reading)]);
    let (_, _, text) = convert(&dir, &["--tier-results", tiers.to_str().unwrap()]);
    assert!(text.contains("region-splice"), "{text}");
    assert!(text.contains("# SCREENSHOT"), "{text}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A Letter page carrying only `text` at its top and the capture placed by
/// this `cm` matrix: too few glyphs for a page of text, so it routes near
/// blank and is read whole.
fn near_blank(text: &str, cm: &str) -> Vec<u8> {
    let (w, h, gray) = capture();
    pdf(
        &[(612.0, 792.0)],
        &format!(
            "BT /F1 10 Tf 72 {} Td ({text}) Tj ET\nq {cm} cm /I0 Do Q",
            792 - 340
        ),
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        &[(
            "I0".to_string(),
            format!(
                "/Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {}",
                gray.len()
            ),
            gray,
        )],
    )
}

/// The element of the graph whose text holds `needle`: its type and text.
fn element_with(graph: &str, needle: &str) -> (String, String) {
    let graph: serde_json::Value = serde_json::from_str(graph).unwrap();
    graph["@graph"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|n| {
            let text = n["nif:isString"]["@value"]
                .as_str()
                .or_else(|| n["nif:isString"].as_str())?;
            text.contains(needle)
                .then(|| (n["@type"].to_string(), text.to_string()))
        })
        .unwrap_or_else(|| panic!("no element holds {needle}"))
}

/// A model's whole reading of `dir`'s page holding `blocks` (label,
/// content): the graph.
fn model_reading(dir: &Path, blocks: &[(&str, &str)]) -> String {
    let blocks: Vec<_> = blocks
        .iter()
        .enumerate()
        .map(|(i, (label, content))| {
            serde_json::json!({ "block_label": label, "block_content": content, "block_order": i })
        })
        .collect();
    let reading = serde_json::json!([{ "parsing_res_list": blocks }]).to_string();
    let tiers = sidecars(dir, "tiers", &[("memo_p0_full.json", reading)]);
    convert(dir, &["--tier-results", tiers.to_str().unwrap()]).2
}

#[test]
fn a_page_near_blank_read_whole_keeps_a_models_headings_where_ocr_judges_them_by_position() {
    // A screenshot under a quarter of the page, with a caption that repeats
    // the screen's name. OCR demotes the screen's title, whose words land
    // in the picture; a model's reading has no positions, and keeps it.
    let dir = scratch(
        "near-blank-screen",
        &near_blank("Customer Portal", "490 0 0 230 60 482"),
    );
    let (doc, _, _) = convert(&dir, &[]);
    assert_eq!(
        doc["doc:unreadPages"]["@value"][0]["reason"], "NearBlank",
        "{doc}"
    );
    let prose = "Reference ABC-7781 opened at the branch office on the date shown";
    let mut words = vec![
        serde_json::json!({ "text": "Customer", "box": [80, 100, 230, 130] }),
        serde_json::json!({ "text": "Portal", "box": [240, 100, 350, 130] }),
    ];
    for row in 0..9 {
        let mut x = 80.0;
        let y = 150.0 + row as f64 * 16.0;
        for w in prose.split(' ') {
            let len = w.len() as f64 * 6.0;
            words.push(serde_json::json!({ "text": w, "box": [x, y, x + len, y + 11.0] }));
            x += len + 6.0;
        }
    }
    let words = serde_json::json!({ "coords": "page", "words": words });
    let w = sidecars(&dir, "words", &[("memo_p0_full.json", words.to_string())]);
    let (_, _, ocr) = convert(&dir, &["--ocr-words", w.to_str().unwrap()]);
    // The screen's title, above the screen's prose (the page's caption
    // comes after it).
    let (kind, _) = element_with(&ocr, "Portal");
    assert_eq!(kind, "\"doco:Paragraph\"", "{ocr}");
    assert!(
        ocr.find("Portal").unwrap() < ocr.find("Reference").unwrap(),
        "{ocr}"
    );
    let model = model_reading(
        &dir,
        &[
            ("doc_title", "Customer Portal"),
            ("text", prose),
            ("figure_title", "Customer Portal"),
        ],
    );
    let (_, text) = element_with(&model, "Portal");
    assert!(text.starts_with("# Customer Portal"), "{model}");
    let _ = std::fs::remove_dir_all(dir);
    // A cover with a logo, its title drawn in outlines: no text layer to
    // tell it is the page's own, and a model's reading keeps it.
    let dir = scratch("near-blank-cover", &near_blank("", "60 0 0 12 72 600"));
    let model = model_reading(&dir, &[("doc_title", "Annual Report 2025")]);
    let (_, text) = element_with(&model, "Annual");
    assert!(text.starts_with("# Annual Report 2025"), "{model}");
    let _ = std::fs::remove_dir_all(dir);
}
