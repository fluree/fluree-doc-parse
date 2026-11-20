//! `fdoc convert` on a format no reader reads yet: refused by name, in a
//! batch counted among the failures, rather than read as a PDF and failing
//! as a corrupt one (`parse: Invalid`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fdoc-unsupported-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fdoc(dir: &Path) -> Command {
    let home = dir.join("home");
    let mut c = Command::new(env!("CARGO_BIN_EXE_fdoc"));
    c.env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("FDOC_HOME", &home)
        .env_remove("FDOC_OCR_WORDS")
        .env_remove("FDOC_TIER_RESULTS");
    c
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// An OLE compound file's header, and nothing a reader could use.
fn compound() -> Vec<u8> {
    [
        &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1][..],
        &[0u8; 504],
    ]
    .concat()
}

#[test]
fn a_file_with_no_reader_fails_naming_its_format() {
    let dir = scratch("one");
    let input = dir.join("deck.key");
    std::fs::write(&input, compound()).unwrap();
    let o = fdoc(&dir)
        .args(["convert", "--no-escalate"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(
        err.contains("unsupported format (.key, Apple Keynote presentation)"),
        "{err}"
    );
    assert!(!err.contains("parse:"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_batch_counts_them_among_its_failures() {
    let dir = scratch("batch");
    let inputs = dir.join("in");
    std::fs::create_dir_all(&inputs).unwrap();
    std::fs::write(
        inputs.join("notes.md"),
        "# Notes\n\nAlex Sample wrote this.\n",
    )
    .unwrap();
    std::fs::write(inputs.join("memo.wpd"), "WordPerfect bytes").unwrap();
    let out = dir.join("out");
    let o = fdoc(&dir)
        .args(["convert", "--no-escalate", "--out-dir"])
        .arg(&out)
        .arg(&inputs)
        .output()
        .unwrap();
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("memo.wpd: unsupported format (.wpd"), "{err}");
    assert!(err.contains("converted 1/2 files"), "{err}");
    assert!(out.join("notes.md").is_file());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_compound_file_on_stdin_is_named_too() {
    let dir = scratch("stdin");
    let mut child = fdoc(&dir)
        .args(["convert", "--no-escalate", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(&compound()).unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(!o.status.success());
    assert!(stderr(&o).contains("OLE compound file"), "{}", stderr(&o));
    let _ = std::fs::remove_dir_all(dir);
}
