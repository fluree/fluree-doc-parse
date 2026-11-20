//! `fdoc convert` on PDFs the parser opens but reads nothing from: a page
//! tree that loops, one nested past what the parser follows, a page
//! reference one flipped bit broke. Each used to give empty output and exit
//! 0, like a blank document; now it is warned about, and fails under
//! `--strict`.

mod common;

use common::{objects_pdf, pdf};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FONT: &str = "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>";
const CONTENT: &str = "BT /F1 12 Tf 72 700 Td (Quarterly report) Tj ET";

/// A one-page text PDF whose catalog and page tree are `catalog` and
/// `pages`, its page object 3 and its content object 4.
fn with_tree(catalog: &str, pages: &str) -> Vec<u8> {
    objects_pdf(&[
        catalog.as_bytes().to_vec(),
        pages.as_bytes().to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << {FONT} >> \
             /Contents 4 0 R >>"
        )
        .into_bytes(),
        format!(
            "<< /Length {} >>\nstream\n{CONTENT}\nendstream",
            CONTENT.len()
        )
        .into_bytes(),
    ])
}

const CATALOG: &str = "<< /Type /Catalog /Pages 2 0 R >>";

fn good() -> Vec<u8> {
    with_tree(CATALOG, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
}

/// The page tree's only kid is the tree itself.
fn cycle() -> Vec<u8> {
    with_tree(CATALOG, "<< /Type /Pages /Kids [2 0 R] /Count 1 >>")
}

/// The page reference buried ten thousand arrays deep.
fn nested() -> Vec<u8> {
    let d = 10_000;
    with_tree(
        CATALOG,
        &format!(
            "<< /Type /Pages /Kids {}3 0 R{} /Count 1 >>",
            "[".repeat(d),
            "]".repeat(d)
        ),
    )
}

/// The good file with one bit flipped: the `3` of `/Kids [3 0 R]` turns
/// into a control character, and the tree has no page left.
fn bit_flipped() -> Vec<u8> {
    let mut b = good();
    let at = b.windows(8).position(|w| w == b"/Kids [3").unwrap() + 7;
    b[at] ^= 0x20;
    b
}

fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fdoc-malformed-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `fdoc convert <inputs> -f text <args>` with no reader configured.
fn convert(dir: &Path, inputs: &[&Path], args: &[&str]) -> Output {
    let home = dir.join("home");
    Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .arg("convert")
        .args(inputs)
        .args(["-f", "text", "--no-escalate"])
        .args(args)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("FDOC_HOME", &home)
        .env_remove("FDOC_OCR_WORDS")
        .env_remove("FDOC_TIER_RESULTS")
        .env_remove("FDOC_OCR_MODELS")
        .output()
        .unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn a_pdf_read_as_nothing_is_warned_about_and_fails_under_strict() {
    let dir = scratch("each");
    for (name, bytes) in [
        ("cycle", cycle()),
        ("nested", nested()),
        ("flipped", bit_flipped()),
    ] {
        let input = dir.join(format!("{name}.pdf"));
        std::fs::write(&input, bytes).unwrap();
        let o = convert(&dir, &[&input], &[]);
        assert!(o.status.success(), "{name}: {}", stderr(&o));
        assert!(o.stdout.is_empty(), "{name}");
        assert!(
            stderr(&o).contains(&format!("warning: {name}: nothing was read")),
            "{name}: {}",
            stderr(&o)
        );
        let o = convert(&dir, &[&input], &["--strict"]);
        assert!(!o.status.success(), "{name}");
        assert!(stderr(&o).contains("error:"), "{name}: {}", stderr(&o));
        // `-q` silences the warning.
        let o = convert(&dir, &[&input], &["-q"]);
        assert!(stderr(&o).is_empty(), "{name}: {}", stderr(&o));
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_pdf_with_text_says_nothing_and_passes_strict() {
    let dir = scratch("good");
    let input = dir.join("good.pdf");
    std::fs::write(&input, good()).unwrap();
    let o = convert(&dir, &[&input], &["--strict"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains("Quarterly report"));
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_page_selection_past_the_end_warns_and_fails_strict() {
    // `--pages 2` of a one-page PDF asked for a page it does not have: read
    // as nothing, it must not pass in silence.
    let dir = scratch("past-end");
    let input = dir.join("good.pdf");
    std::fs::write(&input, good()).unwrap();
    let o = convert(&dir, &[&input], &["--pages", "2"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("warning: good: nothing was read: --pages selects none of its 1 page"),
        "{}",
        stderr(&o)
    );
    let o = convert(&dir, &[&input], &["--strict", "--pages", "2"]);
    assert!(!o.status.success(), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("--pages selects none"),
        "{}",
        stderr(&o)
    );
    // A selection that keeps the page passes.
    let o = convert(&dir, &[&input], &["--strict", "--pages", "1"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_blank_page_is_warned_about_too() {
    // Blank and damaged cannot be told apart from the output, nor a page
    // whose text is drawn as vector outlines: no glyph, no image, only
    // paths. The message names all three.
    let dir = scratch("blank");
    let outlined = "0 0 0 rg 56 700 m 59 707 l 62 700 l f 63 700 m 66 707 l 69 700 l f";
    for (name, content) in [("blank.pdf", ""), ("outlined.pdf", outlined)] {
        let input = dir.join(name);
        std::fs::write(&input, pdf(&[(612.0, 792.0)], content, FONT, &[])).unwrap();
        let o = convert(&dir, &[&input], &[]);
        assert!(o.status.success());
        assert!(
            stderr(&o).contains(
                "nothing was read: 1 page with no text layer and no image \
                 (blank, drawn as vector outlines, or damaged)"
            ),
            "{name}: {}",
            stderr(&o)
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_batch_counts_a_pdf_read_as_nothing_as_failed_under_strict() {
    let dir = scratch("batch");
    let (ok, bad) = (dir.join("good.pdf"), dir.join("cycle.pdf"));
    std::fs::write(&ok, good()).unwrap();
    std::fs::write(&bad, cycle()).unwrap();
    let out = dir.join("out");
    let o = convert(
        &dir,
        &[&ok, &bad],
        &["--out-dir", out.to_str().unwrap(), "--strict"],
    );
    assert!(!o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("converted 1/2"), "{}", stderr(&o));
    assert!(out.join("good.txt").exists());
    let o = convert(&dir, &[&ok, &bad], &["--out-dir", out.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stderr(&o));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_scan_nothing_read_is_reported_unread_not_empty() {
    // A page of pixels with no reader: empty output, but it says why, and
    // the reason is not that the file read as nothing.
    let dir = scratch("scan");
    let input = dir.join("scan.pdf");
    let gray = vec![128u8; 64];
    let bytes = pdf(
        &[(612.0, 792.0)],
        "q 612 0 0 792 0 0 cm /I0 Do Q",
        FONT,
        &[(
            "I0".into(),
            "/Width 8 /Height 8 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 64".into(),
            gray,
        )],
    );
    std::fs::write(&input, bytes).unwrap();
    let o = convert(&dir, &[&input], &["--strict"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("page 1 carries content no reader transcribed (Scanned)"),
        "{}",
        stderr(&o)
    );
    assert!(!stderr(&o).contains("nothing was read"), "{}", stderr(&o));
    let _ = std::fs::remove_dir_all(dir);
}
