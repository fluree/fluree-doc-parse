//! `--mode deterministic|ocr|vlm`, run through the binary: what each input
//! resolves to, that a mode whose needs are not met stops the run (exit 2)
//! and names what to set, and that a config written before modes reads
//! deterministic and says so. The resolution table itself is tested in
//! `mode.rs`; these check the commands reach it and act on its answer.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn document() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/corpus/jp_stat.pdf")
}

/// A working directory of its own, with `config` (if any) as its
/// `.fdoc/config.toml`, an empty per-user config home, and an empty
/// directory to stand in for word or reading files.
fn workdir(name: &str, config: Option<&str>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fdoc-modes-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home")).unwrap();
    std::fs::create_dir_all(dir.join("empty")).unwrap();
    if let Some(config) = config {
        std::fs::create_dir_all(dir.join(".fdoc")).unwrap();
        std::fs::write(dir.join(".fdoc/config.toml"), config).unwrap();
    }
    dir
}

fn fdoc(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fdoc"));
    cmd.current_dir(dir)
        .args(args)
        .env("FDOC_HOME", dir.join("home"));
    // Every variable that takes part in choosing the mode, so a developer's
    // shell cannot change what is resolved.
    for var in [
        "FDOC_MODE",
        "FDOC_OCR",
        "FDOC_OCR_MODELS",
        "FDOC_OCR_WORDS",
        "FDOC_TIER_RESULTS",
        "FDOC_VLM_ANCHORS",
        "FDOC_TITLE_BOXES",
        "FDOC_STRUCTURE_RESULTS",
    ] {
        cmd.env_remove(var);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run fdoc")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `fdoc convert <document> args…` to stdout as Markdown.
fn convert(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let doc = document();
    let mut all = vec!["convert", doc.to_str().unwrap(), "-f", "md", "-o", "-"];
    all.extend_from_slice(args);
    fdoc(dir, &all, env)
}

#[test]
fn deterministic_is_the_default_and_reads_as_the_flags_that_asked_for_it() {
    let dir = workdir("default", None);
    let none = convert(&dir, &["-v"], &[]);
    assert!(none.status.success(), "{}", stderr(&none));
    assert!(
        stderr(&none).contains("mode: deterministic (from the default)"),
        "{}",
        stderr(&none)
    );
    for args in [
        &["--mode", "deterministic"][..],
        &["--no-escalate"][..],
        &["--mode", "ocr", "--no-escalate", "--ocr-words", "empty"][..],
    ] {
        let out = convert(&dir, args, &[]);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        if args.len() < 3 {
            assert_eq!(out.stdout, none.stdout, "{args:?}");
        }
    }
    let env = convert(&dir, &["-v"], &[("FDOC_MODE", "deterministic")]);
    assert!(
        stderr(&env).contains("mode: deterministic (from FDOC_MODE)"),
        "{}",
        stderr(&env)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_mode_whose_needs_are_not_met_stops_the_run_and_names_what_to_set() {
    let dir = workdir("unmet", None);
    let cases: &[(&[&str], &[(&str, &str)], &str)] = &[
        (
            &["--mode", "ocr"],
            &[],
            "mode ocr (from --mode) needs the OCR assets",
        ),
        (
            &["--mode", "vlm", "--ocr-words", "empty"],
            &[],
            "mode vlm (from --mode) needs a model reader",
        ),
        (
            &["--mode", "vlm"],
            &[],
            "mode vlm (from --mode) builds on ocr, which needs the OCR assets",
        ),
        (
            &["--tier-results", "empty"],
            &[],
            "mode vlm (from --tier-results, which needs it) builds on ocr",
        ),
        (
            &[],
            &[("FDOC_MODE", "ocr")],
            "mode ocr (from FDOC_MODE) needs the OCR assets",
        ),
        (
            &[],
            &[("FDOC_MODE", "fast")],
            "FDOC_MODE=\"fast\" is not a mode",
        ),
        (
            &["--mode", "vlm", "--no-escalate"],
            &[],
            "--no-escalate and --mode vlm contradict each other",
        ),
        (
            &["--mode", "deterministic", "--ocr-words", "empty"],
            &[],
            "--ocr-words needs --mode ocr or more",
        ),
        (
            &["--escalate", "--no-escalate"],
            &[],
            "the argument '--escalate' cannot be used with '--no-escalate'",
        ),
    ];
    for (args, env, says) in cases {
        let out = convert(&dir, args, env);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?} {env:?}: {}",
            stderr(&out)
        );
        assert!(out.stdout.is_empty(), "{args:?} {env:?} wrote output");
        assert!(
            stderr(&out).contains(says),
            "{args:?} {env:?}: {}",
            stderr(&out)
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_config_mode_is_used_and_a_flag_beats_it() {
    let dir = workdir("config", Some("mode = \"ocr\"\n"));
    let out = convert(&dir, &[], &[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("mode ocr (from the config's `mode`) needs the OCR assets"),
        "{}",
        stderr(&out)
    );
    let flag = convert(&dir, &["--mode", "deterministic", "-v"], &[]);
    assert!(flag.status.success(), "{}", stderr(&flag));
    assert!(
        stderr(&flag).contains("mode: deterministic (from --mode)"),
        "{}",
        stderr(&flag)
    );
    let words = convert(&dir, &["--ocr-words", "empty", "-v"], &[]);
    assert!(words.status.success(), "{}", stderr(&words));
    assert!(
        stderr(&words).contains("mode: ocr (from the config's `mode`)"),
        "{}",
        stderr(&words)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_config_written_before_modes_reads_deterministic_and_says_so_once() {
    for (name, config, says) in [
        (
            "legacy-ocr",
            "[ocr]\nengine = \"oar-ocr\"\n".to_string(),
            "note: the config sets up OCR but no mode; reading deterministic. Set mode = \"ocr\"",
        ),
        (
            "legacy-vlm",
            String::new(),
            "note: the config sets up a model reader but no mode; reading deterministic. Set mode = \"vlm\"",
        ),
    ] {
        let dir = workdir(name, None);
        let config = if config.is_empty() {
            // A reader is configured when its key file exists; nothing here
            // reads it, because nothing here calls the model.
            let key = dir.join("key.json");
            std::fs::write(&key, "{}").unwrap();
            format!(
                "[escalation]\nprovider = \"gemini\"\n\n[escalation.gemini]\ncredentials = {:?}\n",
                key.to_str().unwrap()
            )
        } else {
            config
        };
        std::fs::create_dir_all(dir.join(".fdoc")).unwrap();
        std::fs::write(dir.join(".fdoc/config.toml"), config).unwrap();
        let plain = workdir(&format!("{name}-plain"), None);
        let want = convert(&plain, &[], &[]);

        let out = convert(&dir, &[], &[]);
        assert!(out.status.success(), "{name}: {}", stderr(&out));
        assert_eq!(out.stdout, want.stdout, "{name}: not deterministic");
        assert_eq!(stderr(&out).matches(says).count(), 1, "{name}: {}", stderr(&out));

        let quiet = convert(&dir, &["--quiet"], &[]);
        assert!(!stderr(&quiet).contains("note:"), "{name}: {}", stderr(&quiet));
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&plain).unwrap();
    }
}

#[test]
fn config_show_and_triage_say_the_mode_and_where_it_came_from() {
    let dir = workdir("show", None);
    let show = fdoc(&dir, &["config", "show"], &[]);
    let text = String::from_utf8_lossy(&show.stdout);
    assert!(
        text.contains("mode     deterministic   (from the default)"),
        "{text}"
    );
    let show = fdoc(&dir, &["config", "show"], &[("FDOC_MODE", "ocr")]);
    let text = String::from_utf8_lossy(&show.stdout);
    assert!(
        text.contains("mode     not usable: mode ocr (from FDOC_MODE) needs the OCR assets"),
        "{text}"
    );

    let doc = document();
    let triage = fdoc(&dir, &["triage", doc.to_str().unwrap()], &[]);
    assert!(triage.status.success(), "{}", stderr(&triage));
    assert!(
        stderr(&triage).contains("mode: deterministic (from the default)"),
        "{}",
        stderr(&triage)
    );
    let triage = fdoc(
        &dir,
        &["triage", doc.to_str().unwrap(), "--mode", "vlm"],
        &[],
    );
    assert!(triage.status.success(), "{}", stderr(&triage));
    assert!(
        stderr(&triage).contains("mode: vlm (from --mode)"),
        "{}",
        stderr(&triage)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
