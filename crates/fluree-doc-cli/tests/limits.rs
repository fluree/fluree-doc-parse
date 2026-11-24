//! `fdoc` held to limits other than its defaults: by a `--max-*` flag, an
//! `FDOC_MAX_*` variable or the config's `[limits]`, each over the next,
//! and a document past one refused with the flag that raises it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fdoc-limits-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home")).unwrap();
    dir
}

/// `fdoc <args>` run in `dir`, with no config but what `dir` holds and no
/// limit in the environment but `env`.
fn fdoc(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let home = dir.join("home");
    let mut c = Command::new(env!("CARGO_BIN_EXE_fdoc"));
    c.current_dir(dir)
        .args(args)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("FDOC_HOME", &home);
    for v in [
        "FDOC_MAX_MEMORY",
        "FDOC_MAX_WORK",
        "FDOC_MAX_PAGES",
        "FDOC_MAX_RENDER_PIXELS",
    ] {
        c.env_remove(v);
    }
    c.envs(env.iter().copied()).output().unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A CSV of 3,000 cells of ten letters: about 1.1 MiB at Markdown's costs.
fn table(dir: &Path) -> PathBuf {
    let row = ["abcdefghij"; 10].join(",");
    let csv: String = (0..300).map(|_| format!("{row}\n")).collect();
    let path = dir.join("table.csv");
    std::fs::write(&path, csv).unwrap();
    path
}

#[test]
fn a_document_past_a_limit_set_lower_is_refused_naming_its_flag() {
    let dir = scratch("flag");
    let csv = table(&dir);
    let csv = csv.to_str().unwrap();
    let ok = fdoc(&dir, &["convert", csv], &[]);
    assert!(ok.status.success(), "{}", stderr(&ok));
    for (args, env) in [
        (vec!["convert", csv, "--max-memory", "1"], vec![]),
        (vec!["convert", csv], vec![("FDOC_MAX_MEMORY", "1")]),
    ] {
        let o = fdoc(&dir, &args, &env);
        assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
        let e = stderr(&o);
        assert!(
            e.contains("past the 1 MiB of memory a document may take (raise it with --max-memory)"),
            "{e}"
        );
    }
    // The flag over the variable.
    let o = fdoc(
        &dir,
        &["convert", csv, "--max-memory", "64"],
        &[("FDOC_MAX_MEMORY", "1")],
    );
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn each_worker_of_a_batch_reads_within_the_limits_set() {
    // A batch's documents are read on worker threads, and the limits are
    // set per thread: a worker not given them would read at the defaults.
    let dir = scratch("batch");
    let inputs = dir.join("in");
    std::fs::create_dir_all(&inputs).unwrap();
    let csv = table(&dir);
    for name in ["a.csv", "b.csv"] {
        std::fs::copy(&csv, inputs.join(name)).unwrap();
    }
    let (inputs, out) = (inputs.to_str().unwrap(), dir.join("out"));
    let out = out.to_str().unwrap();
    let o = fdoc(
        &dir,
        &[
            "convert",
            inputs,
            "--out-dir",
            out,
            "-j",
            "2",
            "--max-memory",
            "1",
        ],
        &[],
    );
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    let e = stderr(&o);
    assert_eq!(e.matches("(raise it with --max-memory)").count(), 2, "{e}");
    let o = fdoc(&dir, &["convert", inputs, "--out-dir", out, "-j", "2"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn the_configs_limits_hold_under_the_variables_and_flags() {
    let dir = scratch("config");
    let csv = table(&dir);
    let csv = csv.to_str().unwrap();
    std::fs::create_dir_all(dir.join(".fdoc")).unwrap();
    std::fs::write(dir.join(".fdoc/config.toml"), "[limits]\nmax_memory = 1\n").unwrap();
    let o = fdoc(&dir, &["convert", csv], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stderr(&o).contains("--max-memory"), "{}", stderr(&o));
    let o = fdoc(&dir, &["convert", csv], &[("FDOC_MAX_MEMORY", "64")]);
    assert!(o.status.success(), "{}", stderr(&o));
    // `config show` says what is in effect.
    let o = fdoc(&dir, &["config", "show"], &[("FDOC_MAX_PAGES", "12")]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("limits   1 MiB of memory"), "{out}");
    assert!(out.contains(" 12 pages"), "{out}");
    // A key the table does not know is said, once, and ignored: the rest
    // of the file still holds.
    std::fs::write(
        dir.join(".fdoc/config.toml"),
        "[limits]\nmax_memroy = 64\nmax_memory = 1\n",
    )
    .unwrap();
    let o = fdoc(&dir, &["convert", csv], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    let e = stderr(&o);
    assert_eq!(
        e.matches("[limits] has no key \"max_memroy\"").count(),
        1,
        "{e}"
    );
    assert!(e.contains("(raise it with --max-memory)"), "{e}");
    // `config show`, run to check the file, says it too.
    let o = fdoc(&dir, &["config", "show"], &[]);
    let e = stderr(&o);
    assert!(e.contains("[limits] has no key \"max_memroy\""), "{e}");
    // A value of the wrong type loses its limit alone, said, the others
    // held; a variable set empty is unset.
    std::fs::write(
        dir.join(".fdoc/config.toml"),
        "[limits]\nmax_pages = \"12\"\nmax_memory = 1\n",
    )
    .unwrap();
    for args in [vec!["convert", csv], vec!["config", "show"]] {
        let o = fdoc(&dir, &args, &[("FDOC_MAX_MEMORY", "")]);
        let e = stderr(&o);
        assert_eq!(
            e.matches("[limits] max_pages = \"12\" is not a whole number")
                .count(),
            1,
            "{e}"
        );
        if args[0] == "convert" {
            assert_eq!(o.status.code(), Some(1), "{e}");
            assert!(e.contains("(raise it with --max-memory)"), "{e}");
        } else {
            let out = String::from_utf8_lossy(&o.stdout);
            assert!(out.contains("limits   1 MiB of memory"), "{out}");
            assert!(out.contains(" 10000 pages"), "{out}");
        }
    }
}

#[test]
fn a_limit_that_does_not_read_stops_before_any_document() {
    let dir = scratch("bad");
    let csv = table(&dir);
    let csv = csv.to_str().unwrap();
    let o = fdoc(&dir, &["convert", csv], &[("FDOC_MAX_PAGES", "many")]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("FDOC_MAX_PAGES"), "{}", stderr(&o));
    let o = fdoc(&dir, &["convert", csv, "--max-work", "0"], &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(o.stdout.is_empty());
    // Set empty, a variable is unset, not a number that does not read.
    let o = fdoc(&dir, &["completions", "bash"], &[("FDOC_MAX_MEMORY", "")]);
    assert!(o.status.success(), "{}", stderr(&o));
}
