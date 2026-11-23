//! `fdoc dev render-routed --ocr-regions --stored-resolution`: the crops an
//! outside OCR engine reads, at the resolution of the captures they show.
//! No engine is run; the manifest is checked.

mod common;

use common::{capture, pdf};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The resolutions the captures are placed at, in pixels per inch.
const PPI: [f64; 4] = [150.0, 200.0, 250.0, 300.0];

/// A Letter page of ten headings over body text, which the router doubts the
/// structure of, with the capture pasted left to right at each of `ppis`
/// ([`PPI`] in the tests below); `background` lays a one-bit full-page image
/// under everything.
fn page(background: bool, ppis: &[f64]) -> Vec<u8> {
    let mut ops = Vec::new();
    let body = "The quick brown fox jumps over the lazy dog and keeps running";
    for i in 0..10 {
        let y = 770 - i * 46;
        ops.push(format!(
            "BT /F1 18 Tf 40 {y} Td (Section {} Title Here) Tj ET",
            i + 1
        ));
        ops.push(format!("BT /F1 9 Tf 40 {} Td ({body}) Tj ET", y - 22));
    }
    let (w, h, gray) = capture();
    let mut images = Vec::new();
    if background {
        // 612 × 792 one-bit pixels, white with a black frame row.
        let row = 612usize.div_ceil(8);
        let mut bits = vec![0xffu8; row * 792];
        bits[..row].fill(0);
        ops.push("q 612 0 0 792 0 0 cm /Bg Do Q".into());
        images.push((
            "Bg".to_string(),
            format!(
                "/Width 612 /Height 792 /ColorSpace /DeviceGray /BitsPerComponent 1 /Length {}",
                bits.len()
            ),
            bits,
        ));
    }
    for (k, ppi) in ppis.iter().enumerate() {
        let (pw, ph) = (f64::from(w) * 72.0 / ppi, f64::from(h) * 72.0 / ppi);
        let x = 40.0 + k as f64 * 140.0;
        ops.push(format!("q {pw:.3} 0 0 {ph:.3} {x:.1} 40 cm /I{k} Do Q"));
        images.push((
            format!("I{k}"),
            format!(
                "/Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {}",
                gray.len()
            ),
            gray.clone(),
        ));
    }
    pdf(
        &[(612.0, 792.0)],
        &ops.join("\n"),
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        &images,
    )
}

/// Run render-routed on `bytes`; the manifest's records.
fn render_routed(test: &str, bytes: &[u8]) -> Vec<serde_json::Value> {
    let dir =
        std::env::temp_dir().join(format!("fdoc-render-routed-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("page.pdf");
    std::fs::write(&input, bytes).unwrap();
    let out: PathBuf = dir.join("crops");
    let status = Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .args(["dev", "render-routed"])
        .arg(&input)
        .arg(&out)
        .args(["--ocr-regions", "--stored-resolution"])
        .env_remove("FDOC_LAYOUT_BOXES")
        .status()
        .unwrap();
    assert!(status.success());
    let records = read_manifest(&out.join("manifest.jsonl"));
    let _ = std::fs::remove_dir_all(dir);
    records
}

fn read_manifest(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// The scales are the captures' resolutions, [`PPI`], in pixels per point.
fn assert_ppi(scales: &[f64]) {
    assert_eq!(scales.len(), PPI.len(), "{scales:?}");
    for (s, ppi) in scales.iter().zip(PPI) {
        assert!((s - ppi / 72.0).abs() < 1e-3, "{scales:?}");
    }
}

/// The regions' scales, left to right as the captures are placed.
fn region_scales(records: &[serde_json::Value]) -> Vec<f64> {
    let mut regions: Vec<(f64, f64)> = records
        .iter()
        .filter(|r| r["kind"] == "region")
        .map(|r| (r["bbox"][0].as_f64().unwrap(), r["scale"].as_f64().unwrap()))
        .collect();
    regions.sort_by(|a, b| a.0.total_cmp(&b.0));
    regions.into_iter().map(|(_, s)| s).collect()
}

#[test]
fn a_doubted_page_still_gets_its_region_crops() {
    // The page is read whole on a doubt, which an OCR engine skips; the
    // captures on it are crops of their own, as `convert --ocr` reads them.
    let records = render_routed("doubt", &page(false, &PPI));
    let reasons: Vec<&str> = records
        .iter()
        .filter_map(|r| r["reason"].as_str())
        .collect();
    assert!(reasons.iter().any(|r| r.ends_with("_doubt")), "{reasons:?}");
    let regions = reasons
        .iter()
        .filter(|r| **r == "small_region" || **r == "region")
        .count();
    assert_eq!(regions, PPI.len(), "{reasons:?}");
}

#[test]
fn each_capture_renders_at_its_own_resolution() {
    // Each at its own resolution: 150 to 300 ppi, all above the model's
    // 144.
    let scales = region_scales(&render_routed("scales", &page(false, &PPI)));
    assert_ppi(&scales);
}

#[test]
fn a_background_does_not_set_a_captures_resolution() {
    // The same captures over a one-bit page background.
    let scales = region_scales(&render_routed("background", &page(true, &PPI)));
    assert_ppi(&scales);
}

#[test]
fn manifest_lines_come_in_the_jobs_order() {
    // The captures finest first: a page's renders go coarsest first, so the
    // crops are cut in the reverse of their regions' order, and the manifest
    // still lists them in it.
    let finest_first: Vec<f64> = PPI.iter().rev().copied().collect();
    let records = render_routed("order", &page(false, &finest_first));
    let regions: Vec<(usize, f64)> = records
        .iter()
        .filter(|r| r["kind"] == "region")
        .map(|r| {
            let png = r["png"].as_str().unwrap();
            let tag = png.rsplit_once("_r").unwrap().1.trim_end_matches(".png");
            (tag.parse().unwrap(), r["scale"].as_f64().unwrap())
        })
        .collect();
    let tags: Vec<usize> = regions.iter().map(|r| r.0).collect();
    assert_eq!(tags, [0, 1, 2, 3], "{records:?}");
    assert!(
        regions.windows(2).all(|w| w[0].1 > w[1].1),
        "cut coarsest first, so not in this order: {regions:?}"
    );
}

#[test]
fn a_doubted_page_whose_regions_are_cropped_apart_keeps_its_doubt() {
    // Ten headings over body text, and a capture large enough for the router
    // to send a model: read whole on the doubt, the page keeps the doubt's
    // reason, so a reader of characters reads the region's own crop and not
    // the page, as `convert --ocr` does.
    let mut ops = Vec::new();
    let body = "The quick brown fox jumps over the lazy dog and keeps running";
    for i in 0..10 {
        let y = 770 - i * 46;
        ops.push(format!(
            "BT /F1 18 Tf 40 {y} Td (Section {} Title Here) Tj ET",
            i + 1
        ));
        ops.push(format!("BT /F1 9 Tf 40 {} Td ({body}) Tj ET", y - 22));
    }
    let (w, h, gray) = capture();
    ops.push("q 500 0 0 100 50 40 cm /I0 Do Q".into());
    let bytes = pdf(
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
    );
    let records = render_routed("apart", &bytes);
    let page: Vec<&serde_json::Value> = records.iter().filter(|r| r["kind"] == "page").collect();
    assert_eq!(page.len(), 1, "{records:?}");
    assert!(
        page[0]["reason"].as_str().unwrap().ends_with("_doubt"),
        "{records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| r["kind"] == "region" && r["reason"] == "region"),
        "{records:?}"
    );
}
