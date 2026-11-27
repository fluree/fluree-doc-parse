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

/// A one-page PDF `side` points square, with nothing on it.
fn square_page(side: u32) -> Vec<u8> {
    page_sized(u64::from(side), u64::from(side))
}

/// A one-page PDF `width` by `height` points, with nothing on it.
fn page_sized(width: u64, height: u64) -> Vec<u8> {
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] >>"),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(b"xref\n0 4\n0000000000 65535 f \n");
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").bytes());
    }
    out.extend(format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").bytes());
    out
}

/// The width and height a PNG declares.
fn png_size(png: &[u8]) -> (u32, u32) {
    let be = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    (be(16), be(20))
}

#[test]
fn a_page_past_the_render_pixels_is_drawn_smaller_and_said() {
    let dir = scratch("render");
    let pdf = dir.join("poster.pdf");
    std::fs::write(&pdf, square_page(3000)).unwrap();
    let (pdf, out) = (pdf.to_str().unwrap(), dir.join("out"));
    let out = out.to_str().unwrap();
    let o = fdoc(
        &dir,
        &["render", pdf, out, "--max-render-pixels", "1000000"],
        &[],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let e = stderr(&o);
    assert!(e.contains("page 1 drawn at 0.33"), "{e}");
    assert!(e.contains("(--max-render-pixels)"), "{e}");
    let (w, h) = png_size(&std::fs::read(dir.join("out/poster_p0.png")).unwrap());
    assert!(w * h <= 1_000_000 && w >= 999, "{w}x{h}");
    // A page within it is drawn as asked, without a word.
    let small = dir.join("letter.pdf");
    std::fs::write(&small, square_page(300)).unwrap();
    let o = fdoc(
        &dir,
        &[
            "render",
            small.to_str().unwrap(),
            out,
            "--max-render-pixels",
            "1000000",
        ],
        &[],
    );
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));
    let png = std::fs::read(dir.join("out/letter_p0.png")).unwrap();
    assert_eq!(png_size(&png), (600, 600));
}

#[test]
fn a_page_held_by_the_longest_side_says_so() {
    let dir = scratch("render-side");
    let out = dir.join("out");
    let out = out.to_str().unwrap();
    // 40,000 x 3 pt at 2x: within the pixels, past the side.
    let strip = dir.join("strip.pdf");
    std::fs::write(&strip, page_sized(40_000, 3)).unwrap();
    let o = fdoc(&dir, &["render", strip.to_str().unwrap(), out], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let e = stderr(&o);
    assert!(
        e.contains("the longest side a render may take, 32767 pixels"),
        "{e}"
    );
    assert!(!e.contains("--max-render-pixels"), "{e}");
    // A billion points long, its three points high are under a pixel at
    // the scale the side allows: said, not drawn.
    let thread = dir.join("thread.pdf");
    std::fs::write(&thread, page_sized(1_000_000_000, 3)).unwrap();
    let o = fdoc(&dir, &["render", thread.to_str().unwrap(), out], &[]);
    assert_eq!(o.status.code(), Some(1));
    let e = stderr(&o);
    assert!(e.contains("page 1 is less than a pixel high"), "{e}");
    assert!(!e.contains("would not render"), "{e}");
}

#[test]
fn render_and_forms_say_which_bound_a_pdf_is_past() {
    let dir = scratch("tree");
    // A page tree 300 levels deep, past the 256 a PDF is opened within.
    let mut objs = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string()];
    for i in 0..300 {
        objs.push(format!("<< /Type /Pages /Kids [{} 0 R] >>", i + 3));
    }
    objs.push("<< /Type /Page /MediaBox [0 0 612 792] >>".to_string());
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = pdf.len();
    pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
    for o in offsets {
        pdf.extend(format!("{o:010} 00000 n \n").bytes());
    }
    pdf.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .bytes(),
    );
    let path = dir.join("deep.pdf");
    std::fs::write(&path, pdf).unwrap();
    let path = path.to_str().unwrap();
    let out = dir.join("out");
    for args in [
        vec!["render", path, out.to_str().unwrap()],
        vec!["forms", path],
    ] {
        let o = fdoc(&dir, &args, &[]);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        let e = stderr(&o);
        assert!(
            e.contains("PDF too large: its page tree nests more than 256 deep"),
            "{args:?}: {e}"
        );
    }
}

/// A PDF of `n` pages, each saying its number.
fn pages_pdf(n: usize) -> Vec<u8> {
    pages_pdf_with(n, 0, |_| String::new())
}

/// A PDF of `n` pages, each saying its number, then `lines` lines of
/// words, with `extra(i)` written in the dictionary of page `i`.
fn pages_pdf_with(n: usize, lines: usize, extra: impl Fn(usize) -> String) -> Vec<u8> {
    let font = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";
    let mut objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {n} >>",
            (0..n)
                .map(|i| format!("{} 0 R", 4 + 2 * i))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        font.to_string(),
    ];
    for i in 0..n {
        let words: String = (0..lines)
            .map(|l| format!(" 0 -14 Td (Line {l} of page {}, in words) Tj", i + 1))
            .collect();
        let text = format!(
            "BT /F1 12 Tf 72 740 Td (Page number {}) Tj{words} ET",
            i + 1
        );
        objs.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R {} >>",
            5 + 2 * i,
            extra(i)
        ));
        objs.push(format!(
            "<< /Length {} >>\nstream\n{text}\nendstream",
            text.len()
        ));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .bytes(),
    );
    out
}

#[test]
fn a_pdf_past_the_page_limit_is_read_to_it_and_says_so() {
    let dir = scratch("pages");
    let pdf = dir.join("long.pdf");
    std::fs::write(&pdf, pages_pdf(5)).unwrap();
    let pdf = pdf.to_str().unwrap();
    let o = fdoc(
        &dir,
        &["convert", pdf, "--no-escalate", "--max-pages", "2"],
        &[],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("Page number 2") && !out.contains("Page number 3"),
        "{out}"
    );
    let e = stderr(&o);
    assert!(
        e.contains("pages 3-5 are past the page limit, not read (3 pages)"),
        "{e}"
    );
    assert!(
        e.contains("read to page 2 (raise it with --max-pages)"),
        "{e}"
    );
    // In the graph, each page past it, by its reason.
    let o = fdoc(
        &dir,
        &["convert", pdf, "-f", "doco", "--no-escalate", "-q"],
        &[("FDOC_MAX_PAGES", "4")],
    );
    let graph = String::from_utf8_lossy(&o.stdout);
    assert_eq!(graph.matches("PastPageLimit").count(), 1, "{graph}");
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));
    // Within the limit: nothing said.
    let o = fdoc(&dir, &["convert", pdf, "--no-escalate"], &[]);
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));
}

#[test]
fn a_page_tree_nested_past_the_parsers_stack_is_refused_not_a_crash() {
    let dir = scratch("tree");
    // 20,000 `/Pages` nodes, each the parent of the next, over one page:
    // walking it overflowed the stack and aborted the process.
    let depth = 20_000;
    let mut objects = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string()];
    for i in 0..depth {
        objects.push(format!("<< /Type /Pages /Kids [{} 0 R] /Count 1 >>", i + 3));
    }
    objects.push("<< /Type /Page /MediaBox [0 0 612 792] >>".to_string());
    let mut pdf = b"%PDF-1.4\n".to_vec();
    for (i, o) in objects.iter().enumerate() {
        pdf.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    pdf.extend(b"trailer\n<< /Root 1 0 R >>\n%%EOF\n");
    let path = dir.join("deep.pdf");
    std::fs::write(&path, pdf).unwrap();
    let path = path.to_str().unwrap();
    for args in [
        vec!["convert", path, "--no-escalate"],
        vec!["render", path, dir.join("r").to_str().unwrap()],
    ] {
        let o = fdoc(&dir, &args, &[]);
        assert_eq!(o.status.code(), Some(1), "{args:?}: {}", stderr(&o));
    }
    let o = fdoc(&dir, &["convert", path, "--no-escalate"], &[]);
    assert!(
        stderr(&o).contains("its page tree nests more than 256 deep"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn a_pdf_past_its_memory_is_read_to_the_last_whole_page_and_says_so() {
    let dir = scratch("cut");
    let pdf = dir.join("long.pdf");
    std::fs::write(&pdf, pages_pdf_with(20, 40, |_| String::new())).unwrap();
    let pdf = pdf.to_str().unwrap();
    let o = fdoc(
        &dir,
        &["convert", pdf, "--no-escalate", "--max-memory", "1"],
        &[],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    let e = stderr(&o);
    // Read to a page, the rest listed by the limit that stopped it, and
    // the flag that raises it named.
    let read: usize = e
        .split("read to page ")
        .nth(1)
        .and_then(|t| t.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{e}"));
    assert!(read > 0 && read < 20, "{e}");
    assert!(out.contains(&format!("of page {read},")), "{out}");
    assert!(!out.contains(&format!("of page {},", read + 1)), "{out}");
    assert!(
        e.contains(&format!(
            "pages {}-20 are past the memory limit, not read ({} pages)",
            read + 1,
            20 - read
        )),
        "{e}"
    );
    assert!(
        e.contains(&format!("read to page {read} (raise it with --max-memory)")),
        "{e}"
    );
    // In the graph, each page by its reason; quiet, nothing said, the
    // graph saying it.
    let o = fdoc(
        &dir,
        &[
            "convert",
            pdf,
            "-f",
            "doco",
            "--no-escalate",
            "-q",
            "--max-memory",
            "1",
        ],
        &[],
    );
    let graph = String::from_utf8_lossy(&o.stdout);
    assert_eq!(
        graph.matches("PastMemoryLimit").count(),
        20 - read,
        "{graph}"
    );
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));
}

#[test]
fn a_document_read_short_is_said_under_quiet_where_its_output_cannot() {
    // JSON and text carry no notes: quiet, the cut is still said.
    let dir = scratch("quiet");
    let pdf = dir.join("long.pdf");
    std::fs::write(&pdf, pages_pdf(5)).unwrap();
    let pdf = pdf.to_str().unwrap();
    for format in ["json", "text"] {
        let o = fdoc(
            &dir,
            &[
                "convert",
                pdf,
                "-f",
                format,
                "--no-escalate",
                "-q",
                "--max-pages",
                "2",
            ],
            &[],
        );
        assert!(o.status.success(), "{}", stderr(&o));
        assert_eq!(
            stderr(&o),
            "note: long: read to page 2 (raise it with --max-pages)\n",
            "{format}"
        );
    }
    // Within the limit, quiet is quiet.
    let o = fdoc(
        &dir,
        &["convert", pdf, "-f", "json", "-q", "--no-escalate"],
        &[],
    );
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));
}

#[test]
fn a_note_on_a_page_past_the_page_limit_is_not_read() {
    let dir = scratch("notes");
    let pdf = dir.join("noted.pdf");
    let note = |i: usize| {
        format!(
            "/Annots [<< /Type /Annot /Subtype /Text /Rect [100 700 120 720] \
             /Contents (A note on page {}) >>]",
            i + 1
        )
    };
    std::fs::write(&pdf, pages_pdf_with(3, 0, note)).unwrap();
    let pdf = pdf.to_str().unwrap();
    let o = fdoc(
        &dir,
        &["convert", pdf, "--no-escalate", "-q", "--comments"],
        &[],
    );
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("A note on page 3"), "{out}");
    let o = fdoc(
        &dir,
        &[
            "convert",
            pdf,
            "--no-escalate",
            "-q",
            "--comments",
            "--max-pages",
            "2",
        ],
        &[],
    );
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("A note on page 2"), "{out}");
    assert!(!out.contains("A note on page 3"), "{out}");
}
