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
    let body = "This memo summarises the account review and the record shown below";
    let mut ops: Vec<String> = (0..30)
        .map(|i| format!("BT /F1 10 Tf 56 {} Td ({body}) Tj ET", 760 - i * 14))
        .collect();
    let mut images = Vec::new();
    if with_capture {
        let (w, h, gray) = capture();
        ops.push("q 500 0 0 150 56 60 cm /I0 Do Q".into());
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
