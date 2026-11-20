//! `fdoc convert` on the formats read beside PDF and the Office packages:
//! each reaches every output format alike (Markdown, JSON, DoCO, text), so
//! a reader is only as wired as its weakest output.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fdoc-formats-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `fdoc convert <input> -f <format> <args>` with no reader configured.
fn convert(dir: &Path, input: &Path, format: &str, args: &[&str]) -> Output {
    let home = dir.join("home");
    Command::new(env!("CARGO_BIN_EXE_fdoc"))
        .arg("convert")
        .arg(input)
        .args(["-f", format, "--no-escalate"])
        .args(args)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("FDOC_HOME", &home)
        .env_remove("FDOC_OCR_WORDS")
        .env_remove("FDOC_TIER_RESULTS")
        .output()
        .unwrap()
}

/// The input converted to each output format, which must all succeed.
fn every_format(dir: &Path, input: &Path) -> [(String, String); 4] {
    ["md", "json", "doco", "text"].map(|f| {
        let o = convert(dir, input, f, &[]);
        assert!(
            o.status.success(),
            "{f}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        (f.to_string(), String::from_utf8(o.stdout).unwrap())
    })
}

/// Every output holds each of `facts`; the JSON and the graph type the
/// element `kind`.
fn assert_parity(outputs: &[(String, String)], kind: &str, facts: &[&str]) {
    for (format, out) in outputs {
        for fact in facts {
            assert!(out.contains(fact), "{format} lacks {fact:?}:\n{out}");
        }
        if matches!(format.as_str(), "json" | "doco") {
            assert!(out.contains(kind), "{format} lacks {kind}:\n{out}");
        }
    }
}

#[test]
fn a_csv_and_a_tsv_are_tables_in_every_output() {
    let dir = scratch("csv");
    let csv = dir.join("orders.csv");
    std::fs::write(
        &csv,
        "Customer;Amount\r\nAlex Sample;12,50\r\nSam Example;7,00\r\n",
    )
    .unwrap();
    let out = every_format(&dir, &csv);
    assert_parity(&out, "doco:Table", &["Alex Sample", "12,50", "Sam Example"]);
    let doco = &out[2].1;
    assert!(doc_has_cell(doco, "Alex Sample"), "{doco}");
    // A `.tsv` is split on tabs, whatever else its fields hold: here as
    // many commas on each line, which a sniff would take.
    let tsv = dir.join("codes.tsv");
    std::fs::write(
        &tsv,
        "Code, short\tLabel, long\nA,1\tFirst, then\nB,2\tSecond, too\n",
    )
    .unwrap();
    let json = &every_format(&dir, &tsv)[1].1;
    assert!(json.contains("\"A,1\""), "{json}");
    assert!(json.contains("\"First, then\""), "{json}");
    let _ = std::fs::remove_dir_all(dir);
}

/// The graph has a cell node whose value is `text`.
fn doc_has_cell(doco: &str, text: &str) -> bool {
    let v: serde_json::Value = serde_json::from_str(doco).unwrap();
    let mut found = false;
    walk(&v, &mut |n| {
        if n.get("doc:cellValue").and_then(|c| c.as_str()) == Some(text) {
            found = true;
        }
    });
    found
}

fn walk(v: &serde_json::Value, f: &mut dyn FnMut(&serde_json::Value)) {
    f(v);
    match v {
        serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, f)),
        serde_json::Value::Object(o) => o.values().for_each(|x| walk(x, f)),
        _ => {}
    }
}
