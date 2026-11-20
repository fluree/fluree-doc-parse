//! The built-in OCR engine end to end: a page that is only an image of text
//! is rendered, read by oar-ocr on ONNX Runtime and laid out as text.
//!
//! The engine needs its assets (the models and ONNX Runtime), which are not
//! in the repository, so the test is ignored unless asked for:
//!
//! ```text
//! FDOC_OCR_MODELS=/path/to/assets/ocr cargo test -p fluree-doc-cli --test ocr_engine -- --ignored
//! ```
#![cfg(feature = "ocr")]

use fluree_doc_vision::oar::{OarEngine, OarOptions};
use std::path::PathBuf;

/// A one-page PDF whose only content is an image of `lines`, drawn by
/// rendering a text PDF first: the text layer is gone, the pixels remain.
fn scan_of(lines: &[&str]) -> Vec<u8> {
    scan_at(lines, 2.0)
}

/// As [`scan_of`], the image `scale` pixels per point of the page (2 is
/// 144 ppi).
fn scan_at(lines: &[&str], scale: f32) -> Vec<u8> {
    let text = pdf(
        &lines
            .iter()
            .enumerate()
            .map(|(i, l)| format!("BT /F1 18 Tf 40 {} Td ({l}) Tj ET", 300 - 40 * i as i32))
            .collect::<Vec<_>>()
            .join("\n"),
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        None,
    );
    let doc = fluree_doc_pdf::extract_bytes(text.clone()).unwrap();
    assert!(!doc.pages[0].glyphs.is_empty(), "the source page has text");
    let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(text)).unwrap();
    let mut gray = Vec::new();
    let (mut w, mut h) = (0, 0);
    fluree_doc_pdf::escalate::for_each_crop_scaled(
        &raw,
        &vec![(0, None)],
        |_, _| scale,
        |c| {
            w = c.pixels.width;
            h = c.pixels.height;
            gray = c.pixels.rgba.chunks_exact(4).map(|p| p[0]).collect();
        },
    );
    let image = [
        format!(
            "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray \
             /BitsPerComponent 8 /Length {} >>\nstream\n",
            gray.len()
        )
        .into_bytes(),
        gray,
        b"\nendstream".to_vec(),
    ]
    .concat();
    pdf(
        "q 400 0 0 400 0 0 cm /Im0 Do Q",
        "/XObject << /Im0 5 0 R >>",
        Some(image),
    )
}

/// A 400 × 400 pt page with one content stream, its resources, and an
/// optional fifth object.
fn pdf(content: &str, resources: &str, fifth: Option<Vec<u8>>) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] /Resources << {resources} >> \
             /Contents 4 0 R >>"
        )
        .into_bytes(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        )
        .into_bytes(),
    ];
    objects.extend(fifth);
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).into_bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
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
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn a_scanned_page_reads_as_text() {
    let dir = PathBuf::from(std::env::var_os("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS"));
    let engine = OarEngine::open(&dir, &OarOptions::default()).unwrap();
    let bytes = scan_of(&["Account number 4417-0093-21", "Date of birth 15-OCT-1997"]);
    let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes.clone())).unwrap();
    let mut doc = fluree_doc_pdf::extract_bytes(bytes).unwrap();
    assert!(doc.pages[0].glyphs.is_empty(), "the scan has no text layer");
    let pass = fluree_doc_pdf::ocr::read_pages(
        &mut doc,
        &raw,
        &engine,
        None,
        fluree_doc_pdf::escalate::CropScale::Stored,
    );
    assert!(pass.failed.is_empty(), "{:?}", pass.failed);
    let analysis = fluree_doc_pdf::document::analyze(&mut doc, &[]);
    let text: String = analysis
        .elements
        .iter()
        .map(|e| e.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("4417-0093-21"), "{text}");
    assert!(text.contains("15-OCT-1997"), "{text}");
}

/// A scratch directory for one test.
fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fdoc-ocr-cli-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `fdoc` with no configuration and no OCR settings from the environment.
fn fdoc(home: &std::path::Path) -> std::process::Command {
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_fdoc"));
    c.env("FDOC_HOME", home)
        .env_remove("FDOC_OCR")
        .env_remove("FDOC_OCR_MODELS")
        .env_remove("FDOC_OCR_WORDS");
    c
}

#[test]
fn an_ocr_flag_beats_the_words_the_environment_names() {
    // FDOC_OCR_WORDS holds a reading of the page, and `--ocr` asks for the
    // engine: the engine it is, so its missing assets are the error, not a
    // quiet run on the words.
    let dir = scratch("precedence");
    let words = dir.join("words");
    std::fs::create_dir_all(&words).unwrap();
    std::fs::write(
        words.join("scan_p0_full.json"),
        r#"{"coords":"pdf","words":[{"text":"From the sidecar","box":[40,40,200,60]}]}"#,
    )
    .unwrap();
    let input = dir.join("scan.pdf");
    std::fs::write(&input, scan_of(&["Account number 4417"])).unwrap();
    let out = fdoc(&dir)
        .args(["convert", "-f", "text", "--ocr", "oar-ocr", "-v"])
        .arg(&input)
        .env("FDOC_OCR_WORDS", &words)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert!(err.contains("FDOC_OCR_WORDS ignored"), "{err}");
    assert!(err.contains("--models"), "{err}");
    // Without the flag, the environment's words are read.
    let out = fdoc(&dir)
        .args(["convert", "-f", "text"])
        .arg(&input)
        .env("FDOC_OCR_WORDS", &words)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("From the sidecar"));
    // An empty FDOC_OCR is no engine named.
    let out = fdoc(&dir)
        .args(["convert", "-f", "text"])
        .arg(&input)
        .env("FDOC_OCR", "")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn fdoc_ocr_refuses_before_it_writes() {
    let dir = scratch("refuses");
    let png = dir.join("a.png");
    std::fs::write(&png, b"not read").unwrap();
    let out_dir = dir.join("out");
    let run = |args: &[&std::ffi::OsStr]| fdoc(&dir).arg("ocr").args(args).output().unwrap();
    // No assets named.
    let out = run(&[png.as_os_str(), "--out".as_ref(), out_dir.as_os_str()]);
    assert_eq!(out.status.code(), Some(2));
    // Assets that are not there: refused, and no output directory left.
    let missing = dir.join("no-assets");
    let out = run(&[
        png.as_os_str(),
        "--out".as_ref(),
        out_dir.as_os_str(),
        "--models".as_ref(),
        missing.as_os_str(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(!out_dir.exists());
    // Two images that would write one file.
    let other = dir.join("sub");
    std::fs::create_dir_all(&other).unwrap();
    let twin = other.join("a.jpg");
    std::fs::write(&twin, b"not read").unwrap();
    let out = run(&[
        png.as_os_str(),
        twin.as_os_str(),
        "--out".as_ref(),
        out_dir.as_os_str(),
        "--models".as_ref(),
        missing.as_os_str(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("a.json"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn the_three_steps_read_what_convert_ocr_reads() {
    let models = PathBuf::from(std::env::var_os("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS"));
    let dir = scratch("three-steps");
    let input = dir.join("scan.pdf");
    // At 288 ppi, so the stored resolution is not the model's 2x: the paths
    // agree only if both read at the resolution the docs prescribe.
    std::fs::write(
        &input,
        scan_at(
            &["Account number 4417-0093-21", "Date of birth 15-OCT-1997"],
            4.0,
        ),
    )
    .unwrap();
    let text = |out: std::process::Output| {
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let direct = text(
        fdoc(&dir)
            .args(["convert", "-f", "json", "--ocr", "oar-ocr", "--models"])
            .arg(&models)
            .arg(&input)
            .output()
            .unwrap(),
    );
    assert!(direct.contains("4417-0093-21"), "{direct}");
    let crops = dir.join("crops");
    let words = dir.join("words");
    let status = fdoc(&dir)
        .args([
            "dev",
            "render-routed",
            "--ocr-regions",
            "--stored-resolution",
        ])
        .arg(&input)
        .arg(&crops)
        .status()
        .unwrap();
    assert!(status.success());
    let manifest = std::fs::read_to_string(crops.join("manifest.jsonl")).unwrap();
    assert!(manifest.contains("\"scale\":4.0"), "{manifest}");
    let out = fdoc(&dir)
        .arg("ocr")
        .arg(&crops)
        .arg("--out")
        .arg(&words)
        .arg("--models")
        .arg(&models)
        .args(["-j", "4"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let three = text(
        fdoc(&dir)
            .args(["convert", "-f", "json", "--ocr-words"])
            .arg(&words)
            .arg(&input)
            .output()
            .unwrap(),
    );
    assert_eq!(direct, three);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn images_read_at_once_share_a_bounded_set_of_pipelines() {
    use fluree_doc_vision::OcrEngine;
    let dir = PathBuf::from(std::env::var_os("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS"));
    let opts = OarOptions {
        pipelines: 2,
        ..OarOptions::default()
    };
    let engine = OarEngine::open(&dir, &opts).unwrap();
    assert_eq!(engine.pipelines(), 1, "one built when it opens");
    let image = fluree_doc_vision::Pixels::new(400, 120, vec![255; 400 * 120 * 4]).unwrap();
    std::thread::scope(|s| {
        for _ in 0..6 {
            s.spawn(|| engine.read(&image).unwrap());
        }
    });
    assert_eq!(engine.pipelines(), 2, "no more than asked for");
}
