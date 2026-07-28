//! `fdoc triage` — per-page routing verdicts and the corpus escalation rate.

use crate::commands::common::{opts_for, pdfs_in};
use fluree_doc_pdf::extract_file;
use std::path::{Path, PathBuf};

/// Routing decisions with the signals behind them, one line per routed page,
/// with pages numbered the way they are printed.
///
/// 1-based, because this report exists to be acted on and the flag it feeds —
/// `fdoc convert --pages` — is 1-based. Reporting the internal index made
/// `p12` mean page 13: a translation the reader has to do every time and was
/// never told about. The 0-based index is still what `doc:pageIndex` carries
/// in the output, and it stays that way — an index into a sequence is a
/// different thing from a page's printed number.
/// one summary line per file. Timing is wall-clock per document because the
/// router's cost model is the whole point: a routed page costs seconds of GPU
/// against milliseconds here, so the route *rate* is printed with the same
/// prominence as the decisions.
pub fn run(path: &Path) -> i32 {
    let files: Vec<PathBuf> = if path.is_dir() {
        // Routing is a property of a rendered page, so this is PDF-only.
        // Directory expansion now yields every readable format, and a deck
        // or a memo is not a triage failure — it simply has no pages to
        // route.
        pdfs_in(path)
            .into_iter()
            .filter(|p| {
                p.extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| x.eq_ignore_ascii_case("pdf"))
            })
            .collect()
    } else {
        vec![path.to_path_buf()]
    };
    let (mut pages_total, mut pages_routed, mut ms_total) = (0usize, 0usize, 0.0f64);
    let mut failed = 0usize;
    let (mut tables_total, mut tables_suspect) = (0usize, 0usize);
    let mut heading_doubt = 0usize;
    for f in &files {
        let t0 = std::time::Instant::now();
        let doc = match extract_file(f) {
            Ok(d) => d,
            Err(e) => {
                // `triage` prices a deployment, so a file it could not read
                // must not look like a file that needs no escalation.
                eprintln!("error: {}: {e}", f.display());
                failed += 1;
                continue;
            }
        };
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        ms_total += ms;
        let mut routed = Vec::new();
        for p in &doc.pages {
            pages_total += 1;
            let (r, s) = fluree_doc_pdf::route::decide(p);
            match r {
                fluree_doc_pdf::route::Route::Vlm(reason) => {
                    pages_routed += 1;
                    routed.push(format!(
                        "p{} {:?} glyphs={} unicode={:.2} img={:.2}",
                        p.index, reason, s.glyphs, s.unicode_rate, s.image_coverage
                    ));
                }
                fluree_doc_pdf::route::Route::VlmRegions(regions) => {
                    pages_routed += 1;
                    routed.push(format!(
                        "p{} Regions({}) glyphs={} img={:.2} {:?}",
                        p.index + 1,
                        regions.len(),
                        s.glyphs,
                        s.image_coverage,
                        regions
                            .iter()
                            .map(|b| (b.x0 as i64, b.y0 as i64, b.x1 as i64, b.y1 as i64))
                            .collect::<Vec<_>>()
                    ));
                }
                fluree_doc_pdf::route::Route::Deterministic => {}
            }
        }
        // Table-structure escalation: regions whose detected structure
        // disagrees with itself. Separate from page routing — the page is
        // readable, one table on it is not.
        let mut suspects = Vec::new();
        let mut doubt: Vec<fluree_doc_pdf::heading::Doubt> = Vec::new();
        {
            if let (Ok(mut d), Some(raw)) = (
                extract_file(f),
                std::fs::read(f)
                    .ok()
                    .and_then(|b| hayro_syntax::Pdf::new(std::sync::Arc::new(b)).ok()),
            ) {
                let ol = fluree_doc_pdf::outline::extract(&raw);
                let a = fluree_doc_pdf::document::analyze_with(&mut d, &ol, &opts_for(f));
                doubt = a.suspect_headings.clone();
                tables_total += a.tables;
                tables_suspect += a.suspect_tables.len();
                for s in &a.suspect_tables {
                    suspects.push(format!(
                        "p{} {:?} cols={:?}",
                        s.page + 1,
                        s.reason,
                        s.fragment_cols
                    ));
                }
            }
        }
        let name = f.file_stem().and_then(|x| x.to_str()).unwrap_or("?");
        if !suspects.is_empty() {
            println!("{name}\tTABLE\t{}", suspects.join("; "));
        }
        if !doubt.is_empty() {
            heading_doubt += 1;
            for d in &doubt {
                println!(
                    "{name}\tHEADING\tp{} {} of {} elements are titles ({:.0}%), {} corroborated",
                    d.page + 1,
                    d.titles,
                    d.elements,
                    d.density * 100.0,
                    d.corroborated
                );
            }
        }
        let mut column_doubt = false;
        for p in &doc.pages {
            if let Some(c) = fluree_doc_pdf::column::doubt(&p.glyphs) {
                column_doubt = true;
                println!(
                    "{name}\tCOLUMN\tp{} {} column(s) found, {} gutter(s) visible only in a band covering {:.0}% of rows",
                    p.index + 1, c.found, c.missed, c.band * 100.0
                );
            }
        }
        // The report is only useful if it says what to do about it. These
        // pages are the ones that read across their panels, and they do not
        // escalate unless a corpus asks for it.
        if column_doubt && files.len() == 1 {
            println!(
                "\nCOLUMN pages do not escalate by default — `fdoc config set escalation.on_column_doubt true`"
            );
        }
        if routed.is_empty() {
            // For a single file, show the signals even when nothing routes —
            // threshold tuning needs to see the near-misses.
            if std::env::var_os("FDOC_ROUTE_VERBOSE").is_some() || files.len() == 1 {
                for p in &doc.pages {
                    let (_, s) = fluree_doc_pdf::route::decide(p);
                    println!(
                        "{name}\tdeterministic\t{:.1}ms\tp{} glyphs={} unicode={:.2} img={:.2} n_img={} boxes={:?} page={}x{}",
                        ms, p.index + 1, s.glyphs, s.unicode_rate, s.image_coverage, p.images.len(),
                        p.images.iter().take(3).map(|b| (b.bbox.x0 as i64, b.bbox.y0 as i64, b.bbox.x1 as i64, b.bbox.y1 as i64, b.texty)).collect::<Vec<_>>(),
                        p.width as i64, p.height as i64
                    );
                }
            } else {
                println!("{name}\tdeterministic\t{:.1}ms", ms);
            }
        } else {
            println!("{name}\tROUTE\t{:.1}ms\t{}", ms, routed.join("; "));
        }
    }
    if files.len() > 1 {
        println!(
            "{} tables, {} with disagreeing structure ({:.1}%) — model-tier candidates",
            tables_total,
            tables_suspect,
            tables_suspect as f64 * 100.0 / tables_total.max(1) as f64
        );
        println!(
            "{heading_doubt} of {} documents have a doubtful heading hierarchy ({:.1}%)",
            files.len(),
            heading_doubt as f64 * 100.0 / files.len().max(1) as f64
        );
        println!(
            "\n{} files, {} pages, {} routed ({:.1}%), deterministic parse {:.1}ms total",
            files.len(),
            pages_total,
            pages_routed,
            pages_routed as f64 * 100.0 / pages_total.max(1) as f64,
            ms_total
        );
    }
    if failed > 0 {
        eprintln!("{failed} file(s) could not be read");
        return 1;
    }
    0
}
