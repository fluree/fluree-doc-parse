//! `dev render-routed` and `triage` choose their crops by the config file in
//! effect, as `convert` does, and `FDOC_ESCALATE_COLUMNS=0` turns column
//! doubt off for one run whatever the file says. Run through the binary,
//! because what is checked is that each command loads the file at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A page of this document is in column doubt: read whole when the config
/// asks for it, not cropped otherwise.
fn document() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/corpus/jp_stat.pdf")
}

/// A working directory of its own, with a `.fdoc/config.toml` that turns
/// column doubt on when `configured`, and an empty per-user config home.
fn workdir(name: &str, configured: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fdoc-cols-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home")).unwrap();
    if configured {
        std::fs::create_dir_all(dir.join(".fdoc")).unwrap();
        std::fs::write(
            dir.join(".fdoc/config.toml"),
            "[escalation]\non_column_doubt = true\n",
        )
        .unwrap();
    }
    dir
}

fn fdoc(dir: &Path, args: &[&str], columns: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fdoc"));
    cmd.current_dir(dir)
        .args(args)
        .env("FDOC_HOME", dir.join("home"));
    // Every variable that changes the crops, or the analysis they are taken
    // from, so a developer's shell cannot change what is counted.
    for var in [
        "FDOC_ESCALATE_COLUMNS",
        "FDOC_TIER_RESULTS",
        "FDOC_STRUCTURE_RESULTS",
        "FDOC_VLM_ANCHORS",
        "FDOC_TABLE_CONF",
        "FDOC_NO_REJOIN",
    ] {
        cmd.env_remove(var);
    }
    if let Some(v) = columns {
        cmd.env("FDOC_ESCALATE_COLUMNS", v);
    }
    let out = cmd.output().expect("run fdoc");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Whether the page in column doubt was written whole, and what was said
/// on stderr.
fn render_routed(name: &str, configured: bool, columns: Option<&str>) -> (bool, String) {
    let dir = workdir(name, configured);
    let doc = document();
    let out = fdoc(
        &dir,
        &["dev", "render-routed", doc.to_str().unwrap(), "crops"],
        columns,
    );
    let whole = dir.join("crops/jp_stat_p0_full.png").is_file();
    std::fs::remove_dir_all(&dir).unwrap();
    (whole, String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn render_routed_writes_the_crops_the_config_file_asks_for_and_says_so() {
    let (whole, stderr) = render_routed("rr-none", false, None);
    assert!(!whole, "no config: the page is not read whole");
    assert!(!stderr.contains("column doubt"), "{stderr}");

    let (whole, stderr) = render_routed("rr-on", true, None);
    assert!(whole, "the config turns column doubt on");
    assert!(stderr.contains("FDOC_ESCALATE_COLUMNS=0"), "{stderr}");

    let (whole, stderr) = render_routed("rr-off", true, Some("0"));
    assert!(!whole, "FDOC_ESCALATE_COLUMNS=0 overrides the config");
    assert!(!stderr.contains("column doubt"), "{stderr}");
}

#[test]
fn triage_counts_the_crops_the_config_file_asks_for_and_says_so() {
    let triage = |name: &str, configured: bool, columns: Option<&str>| {
        let dir = workdir(name, configured);
        let out = fdoc(&dir, &["triage", document().to_str().unwrap()], columns);
        std::fs::remove_dir_all(&dir).unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let advice = "COLUMN pages do not escalate by default";

    let (stdout, stderr) = triage("tr-none", false, None);
    assert!(!stdout.contains("CROPS"), "{stdout}");
    assert!(stdout.contains(advice), "{stdout}");
    assert!(!stderr.contains("column doubt"), "{stderr}");

    let (stdout, stderr) = triage("tr-on", true, None);
    assert!(stdout.contains("CROPS\t1 crop(s)"), "{stdout}");
    assert!(!stdout.contains(advice), "{stdout}");
    assert!(stderr.contains("FDOC_ESCALATE_COLUMNS=0"), "{stderr}");

    // The file already asks for it: advising to set it again would not
    // change this run, the variable would.
    let (stdout, _) = triage("tr-off", true, Some("0"));
    assert!(!stdout.contains("CROPS"), "{stdout}");
    assert!(!stdout.contains(advice), "{stdout}");
    assert!(
        stdout.contains("kept off for this run by FDOC_ESCALATE_COLUMNS=0"),
        "{stdout}"
    );
}
