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
    fluree_doc_pdf::escalate::for_each_crop(&raw, &vec![(0, None)], |c| {
        w = c.pixels.width;
        h = c.pixels.height;
        assert_eq!(c.pixels.layout, fluree_doc_vision::Layout::Rgba8);
        gray = c.pixels.data.chunks_exact(4).map(|p| p[0]).collect();
    });
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
    let pass = fluree_doc_pdf::ocr::read_pages(&mut doc, &raw, &engine, None, 0);
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
    // `convert --ocr` would read, and a note for the one left.
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
    let shots: String = [40, 90, 140]
        .map(|y| format!("q 100 0 0 15 40 {y} cm /Im0 Do Q\n"))
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
    std::fs::write(
        &input,
        scan_of(&["Account number 4417-0093-21", "Date of birth 15-OCT-1997"]),
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
            .args(["convert", "-f", "text", "--ocr", "oar-ocr", "--models"])
            .arg(&models)
            .arg(&input)
            .output()
            .unwrap(),
    );
    assert!(direct.contains("4417-0093-21"), "{direct}");
    let crops = dir.join("crops");
    let words = dir.join("words");
    let status = fdoc(&dir)
        .args(["dev", "render-routed", "--ocr-regions"])
        .arg(&input)
        .arg(&crops)
        .status()
        .unwrap();
    assert!(status.success());
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
            .args(["convert", "-f", "text", "--ocr-words"])
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
