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
    scan_image(lines, scale, 0)
}

/// As [`scan_at`], the image turned `quarters` quarter turns
/// counter-clockwise: a sheet fed into the scanner sideways or the wrong
/// way round, with no `/Rotate` to say so. The page is square, so the image
/// still covers it.
fn scan_image(lines: &[&str], scale: f32, quarters: u8) -> Vec<u8> {
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
            assert_eq!(c.pixels.layout, fluree_doc_vision::Layout::Rgba8);
            gray = c.pixels.data.chunks_exact(4).map(|p| p[0]).collect();
        },
    );
    for _ in 0..quarters {
        // A quarter counter-clockwise: the new row y is the old column
        // w - 1 - y, read top to bottom.
        let turned: Vec<u8> = (0..w)
            .flat_map(|y| (0..h).map(move |x| (x, y)))
            .map(|(x, y)| gray[(x * w + (w - 1 - y)) as usize])
            .collect();
        gray = turned;
        (w, h) = (h, w);
    }
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
    pdf_sized(400, 400, content, resources, fifth)
}

/// As [`pdf`], on a page `width` × `height` pt.
fn pdf_sized(
    width: u32,
    height: u32,
    content: &str,
    resources: &str,
    fifth: Option<Vec<u8>>,
) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] \
             /Resources << {resources} >> /Contents 4 0 R >>"
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
        0,
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

#[test]
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn an_upside_down_scan_reads_upright_where_its_text_is() {
    let dir = PathBuf::from(std::env::var_os("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS"));
    let engine = OarEngine::open(&dir, &OarOptions::default()).unwrap();
    let lines = [
        "Account number 4417-0093-21",
        "Date of birth 15-OCT-1997",
        "Policy holder Margaret Holloway",
        "Premium due on the first of March",
    ];
    let bytes = scan_image(&lines, 2.0, 2);
    let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes.clone())).unwrap();
    let mut doc = fluree_doc_pdf::extract_bytes(bytes).unwrap();
    let pass = fluree_doc_pdf::ocr::read_pages(
        &mut doc,
        &raw,
        &engine,
        None,
        fluree_doc_pdf::escalate::CropScale::Stored,
        0,
    );
    assert!(pass.failed.is_empty(), "{:?}", pass.failed);
    let analysis = fluree_doc_pdf::document::analyze(&mut doc, &[]);
    let text: String = analysis
        .elements
        .iter()
        .map(|e| e.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for want in ["4417-0093-21", "15-OCT-1997", "Margaret Holloway"] {
        assert!(text.contains(want), "{want} in {text}");
    }
    // The first line is drawn 100 pt from the top of the upright sheet:
    // on the page as stored, upside down, it sits 100 pt from the bottom,
    // and that is where its box is, for a highlight to land on it.
    let first = analysis
        .elements
        .iter()
        .find(|e| e.text.contains("4417-0093-21"))
        .unwrap();
    let b = first.bbox.unwrap();
    let mid = (b.y0 + b.y1) / 2.0;
    assert!(mid > 285.0 && mid < 320.0, "{b:?}");
    assert!(b.x1 > 340.0, "the line ends near the right edge: {b:?}");
}

#[test]
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn a_scan_whose_text_runs_up_reads_upright_where_its_text_is() {
    let dir = PathBuf::from(std::env::var_os("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS"));
    let engine = OarEngine::open(&dir, &OarOptions::default()).unwrap();
    let lines = [
        "Account number 4417-0093-21",
        "Date of birth 15-OCT-1997",
        "Policy holder Margaret Holloway",
        "Premium due on the first of March",
    ];
    // A quarter counter-clockwise: the text runs up the page, which the
    // pipeline alone reads as noise.
    let bytes = scan_image(&lines, 2.0, 1);
    let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes.clone())).unwrap();
    let mut doc = fluree_doc_pdf::extract_bytes(bytes).unwrap();
    let pass = fluree_doc_pdf::ocr::read_pages(
        &mut doc,
        &raw,
        &engine,
        None,
        fluree_doc_pdf::escalate::CropScale::Stored,
        0,
    );
    assert!(pass.failed.is_empty(), "{:?}", pass.failed);
    let analysis = fluree_doc_pdf::document::analyze(&mut doc, &[]);
    let texts: Vec<&str> = analysis.elements.iter().map(|e| e.text.as_str()).collect();
    let text = texts.join("\n");
    for want in ["4417-0093-21", "15-OCT-1997", "Margaret Holloway"] {
        assert!(text.contains(want), "{want} in {text}");
    }
    // Read in the frame where it runs upright: the first line comes first.
    assert!(texts[0].contains("4417-0093-21"), "{text}");
    // The first line is drawn 100 pt from the top of the upright sheet:
    // turned, it runs up the page 100 pt from its left edge, and that is
    // where its box is.
    let first = analysis
        .elements
        .iter()
        .find(|e| e.text.contains("4417-0093-21"))
        .unwrap();
    let b = first.bbox.unwrap();
    let mid = (b.x0 + b.x1) / 2.0;
    assert!(mid > 80.0 && mid < 120.0, "{b:?}");
    assert!(
        b.y1 > 340.0 && b.y1 - b.y0 > 200.0,
        "runs up from near the bottom: {b:?}"
    );
}

#[test]
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn an_upright_scan_reads_as_it_stands() {
    // An upright page of words and numbers written with the digits that
    // read as other digits turned a half (1068 as 8901), read clean and
    // degraded (a scan at 25 ppi under heavy noise) until the reading is
    // poor enough to be read again turned. Neither takes a line from the
    // turned reading: every line is read level, so nothing turns the page,
    // and the clean page reads as written.
    use fluree_doc_vision::OcrEngine;
    let dir = PathBuf::from(std::env::var_os("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS"));
    let engine = OarEngine::open(&dir, &OarOptions::default()).unwrap();
    let lines = [
        "Invoice number 1068",
        "Account holder Margaret",
        "Quantity 6",
        "1068",
        "8691",
        "Policy holder Holloway",
    ];
    for (scale, noise) in [(2.0, 0), (0.35, 110)] {
        let bytes = scan_image(&lines, scale, 0);
        let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes)).unwrap();
        let mut image = None;
        fluree_doc_pdf::escalate::for_each_crop(&raw, &vec![(0, None)], |c| image = Some(c.pixels));
        let image = noisy(image.unwrap(), noise);
        let reading = engine.read(&image).unwrap();
        let read: Vec<&str> = reading.words.iter().map(|w| w.text.as_str()).collect();
        for w in &reading.words {
            assert!(w.angle.unwrap().abs() < 5.0, "{scale}: {w:?}");
        }
        if noise == 0 {
            assert_eq!(read, lines);
            continue;
        }
        // Its lines holding a letter read under 0.8 on the median: the
        // reading the engine read again turned.
        let mut words: Vec<f64> = reading
            .words
            .iter()
            .filter(|w| w.text.chars().any(char::is_alphabetic))
            .map(|w| w.conf.unwrap())
            .collect();
        words.sort_by(f64::total_cmp);
        assert!(
            words.len() >= 3 && words[words.len() / 2] < 0.8,
            "{read:?} {words:?}"
        );
    }
}

/// `image`, grey, each pixel moved up to `noise` levels lighter or darker,
/// the same way on every run.
fn noisy(mut image: fluree_doc_vision::Pixels, noise: u32) -> fluree_doc_vision::Pixels {
    assert_eq!(image.layout, fluree_doc_vision::Layout::Rgba8);
    let mut seed = 12345u32;
    for px in image.data.chunks_exact_mut(4) {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        let n = ((seed >> 16) % (2 * noise + 1)) as i32 - noise as i32;
        let v = (i32::from(px[0]) + n).clamp(0, 255) as u8;
        px[..3].fill(v);
    }
    image
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
        r#"{"coords":"page","words":[{"text":"From the sidecar","box":[40,40,200,60]}]}"#,
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
fn render_routed_crops_for_ocr_under_the_documents_limit() {
    // A text page with three small captures, and a limit of two: the crops
    // `convert --ocr` would read, and a note for the one left. One mask,
    // drawn at three sizes: drawn at one, it would be read once.
    let dir = scratch("limit");
    std::fs::write(dir.join("config.toml"), "[ocr]\nmax_crops = 2\n").unwrap();
    let text: String = (0..12)
        .map(|i| {
            format!(
                "BT /F1 9 Tf 40 {} Td (Every account opened this year.) Tj ET\n",
                380 - 12 * i
            )
        })
        .collect();
    let shots: String = [(100, 40), (110, 90), (120, 140)]
        .map(|(w, y)| format!("q {w} 0 0 15 40 {y} cm /Im0 Do Q\n"))
        .concat();
    let mask = [
        b"<< /Type /XObject /Subtype /Image /Width 200 /Height 30 /ImageMask true \
           /BitsPerComponent 1 /Length 750 >>\nstream\n"
            .to_vec(),
        vec![0; 750],
        b"\nendstream".to_vec(),
    ]
    .concat();
    let input = dir.join("form.pdf");
    std::fs::write(
        &input,
        pdf(
            &format!("{text}{shots}"),
            "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> \
             /XObject << /Im0 5 0 R >>",
            Some(mask),
        ),
    )
    .unwrap();
    let crops = dir.join("crops");
    let out = fdoc(&dir)
        .current_dir(&dir)
        .args(["dev", "render-routed", "--ocr-regions"])
        .arg(&input)
        .arg(&crops)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("leaving 1 unread") && err.contains("ocr.max_crops"),
        "{err}"
    );
    let manifest = std::fs::read_to_string(crops.join("manifest.jsonl")).unwrap();
    assert_eq!(
        manifest.matches("\"small_region\"").count(),
        2,
        "{manifest}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn convert_opens_the_engine_only_for_a_page_it_reads() {
    use fluree_doc_vision::assets::{platform, DET, DICT, REC};
    // Assets that are there, but are not the files this build loads.
    let dir = scratch("on-first-read");
    let assets = dir.join("assets");
    let (rid, lib) = platform().expect("a platform ONNX Runtime ships for");
    let runtime = format!("onnxruntime/{rid}/{lib}");
    for rel in [runtime.as_str(), DET, REC, DICT] {
        let path = assets.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"placeholder").unwrap();
    }
    let convert = |input: &std::path::Path, models: Option<&std::path::Path>| {
        let mut c = fdoc(&dir);
        c.args(["convert", "-f", "text"]);
        if let Some(m) = models {
            c.args(["--ocr", "oar-ocr", "--models"]).arg(m);
        }
        c.arg(input).output().unwrap()
    };
    // A page with its own text asks the engine for nothing: it converts as
    // it does without `--ocr`, and the assets are never checked.
    let text = dir.join("text.pdf");
    std::fs::write(
        &text,
        pdf(
            "BT /F1 18 Tf 40 300 Td (Account number 4417) Tj ET",
            "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
            None,
        ),
    )
    .unwrap();
    let plain = convert(&text, None);
    let read = convert(&text, Some(&assets));
    let err = String::from_utf8_lossy(&read.stderr);
    assert!(read.status.success(), "{err}");
    assert_eq!(read.stdout, plain.stdout);
    // A directory without the files is refused before anything is read.
    let out = convert(&text, Some(&dir.join("no-assets")));
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no such file"));
    // A scanned page asks for a reading: the engine fails to open, which
    // is said once, and ends the run as a refused engine did.
    let scan = dir.join("scan.pdf");
    std::fs::write(&scan, scan_of(&["Account number 4417"])).unwrap();
    let out = convert(&scan, Some(&assets));
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert_eq!(err.matches("error: --ocr:").count(), 1, "{err}");
    assert!(err.contains("could not be opened"), "{err}");
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
    let image = fluree_doc_vision::Pixels::new(
        400,
        120,
        fluree_doc_vision::Layout::Luma8,
        vec![255; 400 * 120],
    )
    .unwrap();
    std::thread::scope(|s| {
        for _ in 0..6 {
            s.spawn(|| engine.read(&image).unwrap());
        }
    });
    assert_eq!(engine.pipelines(), 2, "no more than asked for");
}

#[test]
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn a_tall_capture_is_read_row_by_row() {
    // 800 × 9,000 pixels of 220 short rows, 13 px high: 7.2 Mpx, read whole
    // at its own size (shrunk to 178 × 2,000, the detector would see none of
    // them); and, under a cap of 2 Mpx, in strips across it, each row kept
    // once, the passes taking turns within a budget smaller than one.
    use fluree_doc_vision::OcrEngine;
    let dir = PathBuf::from(std::env::var_os("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS"));
    let whole = OarEngine::open(&dir, &OarOptions::default()).unwrap();
    let strips = OarEngine::open(
        &dir,
        &OarOptions {
            sizing: fluree_doc_vision::sizing::OcrSizing::megapixels(2.0).unwrap(),
            memory: 1,
            ..OarOptions::default()
        },
    )
    .unwrap();
    assert!(
        strips_of(&strips, 800, 9000) > 4,
        "read in strips under 2 Mpx"
    );
    let rows: String = (0..220)
        .map(|i| {
            format!(
                "BT /F1 6.5 Tf 30 {} Td (Row {i:03} account {}) Tj ET\n",
                4480 - 20 * i,
                4417 + 7 * i
            )
        })
        .collect();
    let bytes = pdf_sized(
        400,
        4500,
        &rows,
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        None,
    );
    let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes)).unwrap();
    let mut image = None;
    fluree_doc_pdf::escalate::for_each_crop(&raw, &vec![(0, None)], |c| image = Some(c.pixels));
    let image = image.unwrap();
    assert_eq!((image.width, image.height), (800, 9000));
    for engine in [&whole, &strips] {
        rows_read_once(&engine.read(&image).unwrap(), 220);
    }
}

/// How many passes `engine`'s sizing plans for an image `w` × `h`.
fn strips_of(engine: &OarEngine, w: u32, h: u32) -> usize {
    engine.sizing().plan(w, h).passes.len()
}

/// Each of the `n` rows `Row 000` … of a reading is read once, its boxes
/// in the image's rows.
fn rows_read_once(reading: &fluree_doc_vision::WordReading, n: usize) {
    let mut seen = vec![0; n];
    for w in &reading.words {
        let t: String = w.text.split_whitespace().collect();
        if let Some(i) = t
            .strip_prefix("Row")
            .and_then(|r| r.get(..3)?.parse::<usize>().ok())
        {
            if i < n {
                seen[i] += 1;
            }
        }
        assert!(
            w.bbox[3] <= reading.height.unwrap() + 1.0,
            "boxes are in the whole image's rows"
        );
    }
    let once = seen.iter().filter(|&&k| k == 1).count();
    assert_eq!(
        once,
        n,
        "rows read other than once: {:?}",
        seen.iter()
            .enumerate()
            .filter(|(_, n)| **n != 1)
            .collect::<Vec<_>>()
    );
}

#[test]
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn a_capture_of_small_text_is_read_at_its_own_size() {
    // 1,000 × 2,600 pixels of 160 rows of 10 px text: shrunk to 0.77, as
    // the detector once did to fit 2,000 px, its lines are 9 px thick and
    // some are missed. Read at its own size, every one is.
    use fluree_doc_vision::OcrEngine;
    let dir = PathBuf::from(std::env::var_os("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS"));
    let engine = OarEngine::open(&dir, &OarOptions::default()).unwrap();
    let rows: String = (0..160)
        .map(|i| {
            format!(
                "BT /F1 5 Tf 12 {} Td (Row {i:03} account {} transfer pending {}.{:02} reference {}) Tj ET\n",
                1290 - 8 * i,
                4417 + 7 * i,
                120 + 13 * i,
                i % 100,
                88310 + 29 * i
            )
        })
        .collect();
    let bytes = pdf_sized(
        500,
        1300,
        &rows,
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        None,
    );
    let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes)).unwrap();
    let mut image = None;
    fluree_doc_pdf::escalate::for_each_crop(&raw, &vec![(0, None)], |c| image = Some(c.pixels));
    let image = image.unwrap();
    assert_eq!((image.width, image.height), (1000, 2600));
    rows_read_once(&engine.read(&image).unwrap(), 160);
}

#[test]
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn a_page_scanned_sideways_at_300_dpi_is_read_whole() {
    // A landscape sheet scanned on a Letter page at 300 dpi, 2,550 × 3,300
    // pixels: its 15 lines run down the page, from row 100 or so past row
    // 2,600. Cut as a tall image would be, in strips 0-2,550 and 750-3,300,
    // each line would reach a cut in both and be lost, or be kept twice.
    use fluree_doc_vision::OcrEngine;
    let dir = PathBuf::from(std::env::var_os("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS"));
    let engine = OarEngine::open(&dir, &OarOptions::default()).unwrap();
    let line = |i: usize| {
        format!(
            "Line {i:02} quarterly statement for account {} opening balance {}.{:02} closing balance due on payment of all fees this quarter",
            4417 + 31 * i,
            1200 + 17 * i,
            i
        )
    };
    let lines: String = (0..15)
        .map(|i| {
            // Turned a quarter clockwise: the text runs down the page.
            format!(
                "BT /F1 24 Tf 0 -1 1 0 {} 1600 Tm ({}) Tj ET\n",
                1150 - 70 * i,
                line(i)
            )
        })
        .collect();
    let bytes = pdf_sized(
        1275,
        1650,
        &lines,
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        None,
    );
    let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes)).unwrap();
    let mut image = None;
    fluree_doc_pdf::escalate::for_each_crop(&raw, &vec![(0, None)], |c| image = Some(c.pixels));
    let image = image.unwrap();
    assert_eq!((image.width, image.height), (2550, 3300));
    let reading = engine.read(&image).unwrap();
    let read: Vec<String> = reading
        .words
        .iter()
        .map(|w| w.text.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    for w in &reading.words {
        // Upright about its center, a line running down spans its length
        // in rows.
        let (cy, len) = ((w.bbox[1] + w.bbox[3]) / 2.0, w.bbox[2] - w.bbox[0]);
        assert!(
            cy - len / 2.0 < 740.0 && cy + len / 2.0 > 2560.0,
            "{} spans rows {} to {}",
            w.text,
            cy - len / 2.0,
            cy + len / 2.0
        );
    }
    for i in 0..15 {
        let n = read.iter().filter(|t| **t == line(i)).count();
        assert_eq!(n, 1, "line {i} read {n} times: {read:?}");
    }
    assert_eq!(read.len(), 15, "{read:?}");
}

#[test]
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn fdoc_ocr_names_the_image_it_shrinks() {
    // 3,000 × 400 px over a cap of 1 Mpx: a strip as tall as the image is
    // wide would not fit, so the image is read shrunk, and the warning says
    // which image, as a run reading many would need.
    let models = PathBuf::from(std::env::var_os("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS"));
    let dir = scratch("names-shrunk");
    let png = dir.join("wide-capture.png");
    let (w, h) = (3000u32, 400u32);
    let mut pixels = vec![255u8; (w * h) as usize];
    for y in 180..220 {
        for x in 100..2900 {
            pixels[(y * w + x) as usize] = 0;
        }
    }
    let mut file = std::io::BufWriter::new(std::fs::File::create(&png).unwrap());
    let mut enc = png::Encoder::new(&mut file, w, h);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .unwrap()
        .write_image_data(&pixels)
        .unwrap();
    drop(file);
    let out = fdoc(&dir)
        .arg("ocr")
        .arg(&png)
        .arg("--out")
        .arg(dir.join("out"))
        .arg("--models")
        .arg(&models)
        .args(["--max-megapixels", "1"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let warning = stderr
        .lines()
        .find(|l| l.contains("read shrunk"))
        .unwrap_or_else(|| panic!("no shrink warning: {stderr}"));
    assert!(
        warning.starts_with(&format!(
            "warning: {}: OCR: a 3000 × 400 image",
            png.display()
        )),
        "{warning}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn a_page_scanned_askew_and_read_in_strips_keeps_every_line() {
    // A Letter page scanned at 400 dpi, 3,400 × 4,400 px, turned 3°: 75
    // lines of about 2,800 px every 55 px. Read in two strips; at the seam,
    // each line's rows overlap its neighbours'. Compared on those rows, the
    // strips' copies of row 37 were each taken for a neighbour's, and it
    // was lost; every line is kept once.
    use fluree_doc_vision::OcrEngine;
    let dir = PathBuf::from(std::env::var_os("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS"));
    let engine = OarEngine::open(&dir, &OarOptions::default()).unwrap();
    let (sin, cos) = 3f64.to_radians().sin_cos();
    let (cx, cy) = (850.0, 1100.0);
    let mut content = format!(
        "q {cos:.6} {sin:.6} {:.6} {cos:.6} {:.3} {:.3} cm\n",
        -sin,
        cx - cos * cx + sin * cy,
        cy - sin * cx - cos * cy
    );
    for i in 0..75 {
        content.push_str(&format!(
            "BT /F1 18 Tf 150 {} Td (Row {i:03} account {} statement of the transfer pending {}.{:02} reference {} cleared on the quarterly close) Tj ET\n",
            2130.0 - 27.5 * f64::from(i),
            4417 + 7 * i,
            120 + 13 * i,
            i % 100,
            88310 + 29 * i
        ));
    }
    content.push('Q');
    let bytes = pdf_sized(
        1700,
        2200,
        &content,
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        None,
    );
    let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes)).unwrap();
    let mut image = None;
    fluree_doc_pdf::escalate::for_each_crop(&raw, &vec![(0, None)], |c| image = Some(c.pixels));
    let image = image.unwrap();
    assert_eq!((image.width, image.height), (3400, 4400));
    assert_eq!(
        engine.sizing().plan(3400, 4400).passes.len(),
        2,
        "read in two strips"
    );
    rows_read_once(&engine.read(&image).unwrap(), 75);
}

#[test]
#[ignore = "needs the OCR assets: set FDOC_OCR_MODELS"]
fn a_reading_waits_while_the_memory_budget_is_held() {
    // A budget of one byte: every pass is larger, and is read alone. Two
    // pipelines, two threads: a small image asked for while a page is read
    // waits for the page, though a pipeline is free for it.
    use fluree_doc_vision::{Layout, OcrEngine, Pixels};
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = PathBuf::from(std::env::var_os("FDOC_OCR_MODELS").expect("FDOC_OCR_MODELS"));
    let opts = OarOptions {
        pipelines: 2,
        memory: 1,
        ..OarOptions::default()
    };
    let engine = OarEngine::open(&dir, &opts).unwrap();
    let image = |w: u32, h: u32| {
        let mut data = vec![255u8; (w * h) as usize];
        for y in (40..h.saturating_sub(40)).step_by(60) {
            for row in y..y + 12 {
                let at = (row * w) as usize;
                data[at + 40..at + (w as usize - 40)].fill(0);
            }
        }
        Pixels::new(w, h, Layout::Luma8, data).unwrap()
    };
    let (page, small) = (image(2550, 3300), image(300, 120));
    let page_done = AtomicBool::new(false);
    std::thread::scope(|s| {
        let reader = s.spawn(|| {
            engine.read(&page).unwrap();
            page_done.store(true, Ordering::SeqCst);
        });
        while engine.memory_held() == 0 {
            assert!(
                !reader.is_finished(),
                "the page was read before it was seen held"
            );
            std::thread::yield_now();
        }
        engine.read(&small).unwrap();
        assert!(
            page_done.load(Ordering::SeqCst),
            "the small image was read beside the page, past the budget"
        );
        reader.join().unwrap();
    });
}
