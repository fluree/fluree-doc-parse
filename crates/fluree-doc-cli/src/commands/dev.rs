//! `fdoc dev` — pipeline-internals commands for debugging extraction.
//!
//! Everything here exposes intermediate layout state (raw glyphs, assembled
//! lines, blocks, furniture, table geometry, render crops). Output formats
//! are diagnostics, not a compatibility surface. Several report the T-series
//! metrics defined in `eval/TEST_PLAN.md`.

#![allow(clippy::too_many_lines)]

use crate::commands::common::{opts_for, sources_in, VLM_RENDER_SCALE};
use fluree_doc_pdf::{
    block, dedup, extract_file, furniture, heading, line, outline, overlay, rule, PageText,
};
use std::path::{Path, PathBuf};

pub(crate) fn probe(dir: &Path) {
    let files = sources_in(dir);
    let (mut pages, mut glyphs, mut uni, mut boxed) = (0usize, 0usize, 0usize, 0usize);
    let (mut deduped, mut lig_raw, mut lig_norm) = (0usize, 0usize, 0usize);
    let (mut errors, mut panics) = (Vec::new(), Vec::new());
    let t0 = std::time::Instant::now();

    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let path = f.clone();
        match std::panic::catch_unwind(move || extract_file(&path)) {
            Ok(Ok(mut doc)) => {
                for p in &mut doc.pages {
                    pages += 1;
                    deduped += dedup::remove_faux_bold(&mut p.glyphs, 8);
                    glyphs += p.glyphs.len();
                    uni += p.glyphs.iter().filter(|g| !g.text.is_empty()).count();
                    boxed += p.glyphs.iter().filter(|g| g.bbox.is_some()).count();
                    let texts: Vec<String> = p.glyphs.iter().map(|g| g.text.clone()).collect();
                    let pt = PageText::build(&texts);
                    lig_raw += pt.raw.chars().filter(is_lig).count();
                    lig_norm += pt.normalized.chars().filter(is_lig).count();
                }
            }
            Ok(Err(e)) => errors.push(format!("{name}: {e}")),
            Err(_) => panics.push(name),
        }
    }
    let secs = t0.elapsed().as_secs_f64();

    println!(
        "=== fdoc probe: {} PDFs in {} ===",
        files.len(),
        dir.display()
    );
    println!(
        "T0.1 parsed ok        : {}/{}",
        files.len() - errors.len() - panics.len(),
        files.len()
    );
    println!("T0.2 PANICS           : {}", panics.len());
    println!(
        "T0.5 throughput       : {:.1} pages/s ({:.2} ms/page)",
        pages as f64 / secs,
        1000.0 * secs / pages.max(1) as f64
    );
    println!();
    println!("     pages / glyphs   : {pages} / {glyphs}");
    println!(
        "T1.1 unicode rate     : {:.3}%",
        100.0 * uni as f64 / glyphs.max(1) as f64
    );
    println!(
        "     glyphs with bbox : {:.3}%  (remainder is whitespace: no outline)",
        100.0 * boxed as f64 / glyphs.max(1) as f64
    );
    println!("T1.4 ligatures raw    : {lig_raw}");
    println!("T1.4 ligatures read   : {lig_norm}   <- must be 0");
    println!("T1.5 faux-bold removed: {deduped}");
    for e in &errors {
        println!("  ERROR {e}");
    }
    for p in &panics {
        println!("  PANIC {p}");
    }

    if !panics.is_empty() || lig_norm > 0 {
        std::process::exit(1);
    }
}

fn is_lig(c: &char) -> bool {
    ('\u{FB00}'..='\u{FB06}').contains(c)
}

/// Resolve occurrences of `needle` to overlay rectangles — the mechanism behind
/// entity underlining on a rendered page.
pub(crate) fn find(pdf: &Path, needle: &str) {
    let mut doc = extract_file(pdf).expect("extract failed");
    let mut total = 0;
    for p in &mut doc.pages {
        dedup::remove_faux_bold(&mut p.glyphs, 8);
        let texts: Vec<String> = p.glyphs.iter().map(|g| g.text.clone()).collect();
        let pt = PageText::build(&texts);
        let hay: Vec<char> = pt.normalized.chars().collect();
        let nee: Vec<char> = needle.chars().collect();
        if nee.is_empty() || hay.len() < nee.len() {
            continue;
        }
        for i in 0..=(hay.len() - nee.len()) {
            if hay[i..i + nee.len()] != nee[..] {
                continue;
            }
            let Some((a, b)) = pt.glyph_range_for_norm_span(i, i + nee.len()) else {
                continue;
            };
            let rects = overlay::rects_for_glyph_range(&p.glyphs, a, b);
            total += 1;
            let json: Vec<_> = rects
                .iter()
                .map(|r| serde_json::json!({"x": r.x0, "y": r.y0, "w": r.width(), "h": r.height()}))
                .collect();
            println!(
                "page {:<3} norm_off {:<6} {}",
                p.index,
                i,
                serde_json::to_string(&json).unwrap()
            );
        }
    }
    println!("{total} match(es) for {needle:?}");
}

/// Dump assembled lines — the first layout pass.
pub(crate) fn lines(pdf: &Path, page: Option<usize>) {
    let mut doc = extract_file(pdf).expect("extract failed");
    let (mut n_lines, mut n_rot) = (0usize, 0usize);
    for p in &mut doc.pages {
        if let Some(want) = page {
            if p.index != want {
                continue;
            }
        }
        dedup::remove_faux_bold(&mut p.glyphs, 8);
        let ls = line::assemble_page(&p.glyphs);
        n_lines += ls.len();
        n_rot += ls.iter().filter(|l| !l.is_horizontal()).count();
        if page.is_some() {
            for l in &ls {
                let rot = if l.is_horizontal() {
                    String::new()
                } else {
                    format!(" [{}deg]", l.rotation_bucket)
                };
                println!(
                    "  y={:7.1} x={:7.1} sz={:5.1}{}  {}",
                    l.bbox.y0,
                    l.bbox.x0,
                    l.font_size,
                    rot,
                    l.text.chars().take(88).collect::<String>()
                );
            }
        }
    }
    println!("{n_lines} lines ({n_rot} rotated)");
}

/// Histogram of horizontal gaps between consecutive glyphs sharing a baseline,
/// normalized by font size. Used to set the word-space and block-split
/// thresholds from data rather than by eye (defects L1/L2 in eval/TEST_PLAN.md).
pub(crate) fn gaps(target: &Path) {
    let files: Vec<PathBuf> = if target.is_dir() {
        sources_in(target)
    } else {
        vec![target.to_path_buf()]
    };
    // Buckets of 0.05 font-size units, up to 5.0.
    let mut hist = vec![0usize; 100];
    let mut over = 0usize;
    let mut samples: Vec<(f64, String, String)> = Vec::new();

    for f in &files {
        let Ok(mut doc) = extract_file(f) else {
            continue;
        };
        for p in &mut doc.pages {
            dedup::remove_faux_bold(&mut p.glyphs, 8);
            // Walk glyphs in baseline order within each rotation bucket.
            let mut idx: Vec<usize> = (0..p.glyphs.len())
                .filter(|&i| p.glyphs[i].bbox.is_some())
                .collect();
            idx.sort_by(|&a, &b| {
                let (ga, gb) = (&p.glyphs[a], &p.glyphs[b]);
                ga.rotation_bucket()
                    .cmp(&gb.rotation_bucket())
                    .then(
                        ga.origin
                            .1
                            .partial_cmp(&gb.origin.1)
                            .unwrap_or(std::cmp::Ordering::Equal),
                    )
                    .then(
                        ga.origin
                            .0
                            .partial_cmp(&gb.origin.0)
                            .unwrap_or(std::cmp::Ordering::Equal),
                    )
            });
            for w in idx.windows(2) {
                let (a, b) = (&p.glyphs[w[0]], &p.glyphs[w[1]]);
                if a.rotation_bucket() != b.rotation_bucket() {
                    continue;
                }
                // same baseline only
                let fs = a.font_size.max(b.font_size).max(1.0) as f64;
                if (a.origin.1 - b.origin.1).abs() > fs * 0.3 {
                    continue;
                }
                let (Some(ba), Some(bb)) = (a.bbox, b.bbox) else {
                    continue;
                };
                // Same basis as line assembly: pen advance, not ink box.
                let prev_end = match a.advance {
                    Some(adv) => a.origin.0 + adv,
                    None => ba.x1,
                };
                let gap = (bb.x0.min(b.origin.0) - prev_end) / fs;
                if gap < 0.0 {
                    continue;
                }
                let both_digit = a.text.chars().next().is_some_and(|c| c.is_ascii_digit())
                    && b.text.chars().next().is_some_and(|c| c.is_ascii_digit());
                if std::env::var("GAPS_DIGITS_ONLY").is_ok() && !both_digit {
                    continue;
                }
                if std::env::var("GAPS_NO_DIGITS").is_ok() && both_digit {
                    continue;
                }
                let bucket = (gap / 0.05) as usize;
                if bucket < hist.len() {
                    hist[bucket] += 1;
                } else {
                    over += 1;
                }
                if samples.len() < 4000 {
                    samples.push((gap, a.text.clone(), b.text.clone()));
                }
            }
        }
    }

    let total: usize = hist.iter().sum::<usize>() + over;
    println!(
        "gap distribution over {} file(s), {} adjacent-glyph pairs",
        files.len(),
        total
    );
    println!("(gap measured edge-to-edge, normalized by font size)\n");
    let max = *hist.iter().max().unwrap_or(&1);
    let mut cum = 0usize;
    for (i, &n) in hist.iter().enumerate() {
        if n == 0 && i > 40 {
            continue;
        }
        cum += n;
        let lo = i as f64 * 0.05;
        let bar = "#".repeat((n * 60 / max.max(1)).min(60));
        if n * 200 > max || i < 20 {
            println!(
                "  {:>4.2}-{:<4.2} {:>7} {:>5.1}% cum{:>5.1}% {}",
                lo,
                lo + 0.05,
                n,
                100.0 * n as f64 / total as f64,
                100.0 * cum as f64 / total as f64,
                bar
            );
        }
    }
    println!(
        "  >5.00      {:>7} {:>5.1}%",
        over,
        100.0 * over as f64 / total as f64
    );
}

/// Print the measured gap around a specific character pair, to diagnose a
/// missing or spurious word break rather than guessing at the threshold.
pub(crate) fn pair(pdf: &Path, needle: &str) {
    let mut doc = extract_file(pdf).expect("extract");
    let n: Vec<char> = needle.chars().collect();
    for p in &mut doc.pages {
        dedup::remove_faux_bold(&mut p.glyphs, 8);
        let gl: Vec<char> = p
            .glyphs
            .iter()
            .map(|g| g.text.chars().next().unwrap_or(' '))
            .collect();
        if gl.len() < n.len() {
            continue;
        }
        for i in 0..=(gl.len() - n.len()) {
            if gl[i..i + n.len()] != n[..] {
                continue;
            }
            println!("page {} at glyph {}:", p.index, i);
            for j in i..i + n.len() - 1 {
                let (a, b) = (&p.glyphs[j], &p.glyphs[j + 1]);
                let fs = a.font_size.max(b.font_size).max(1.0) as f64;
                match (a.bbox, b.bbox) {
                    (Some(ba), Some(bb)) => println!(
                        "   {:?}->{:?}  gap={:.4} fs-units (raw {:.2}, fs {:.1})",
                        a.text,
                        b.text,
                        (bb.x0 - ba.x1) / fs,
                        bb.x0 - ba.x1,
                        fs
                    ),
                    _ => println!(
                        "   {:?}->{:?}  *** one has NO BBOX (a={} b={}) origins {:.2} -> {:.2}",
                        a.text,
                        b.text,
                        a.bbox.is_some(),
                        b.bbox.is_some(),
                        a.origin.0,
                        b.origin.0
                    ),
                }
            }
            return;
        }
    }
    println!("not found: {needle:?}");
}

/// Report detected page furniture. T2.7 in eval/TEST_PLAN.md.
///
/// Read from the pipeline's own analysis rather than from a re-run of the
/// detector over every assembled line: grids take their glyphs before the
/// furniture pass sees a page, so a detector fed every line reported
/// strippings — a flow sheet's clock labels as page numbers — that the
/// pipeline never made.
pub(crate) fn furn(pdf: &Path) {
    let data = std::fs::read(pdf).expect("read");
    let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(data.clone())).expect("parse");
    let ol = outline::extract(&raw);
    let mut doc = fluree_doc_pdf::extract_bytes(data).expect("extract");
    let a = fluree_doc_pdf::document::analyze_with(&mut doc, &ol, &opts_for(pdf));
    // Group by (kind, text) so every distinct piece of furniture is visible -
    // reporting one example per kind hid the watermark behind the footer.
    let mut seen: std::collections::BTreeMap<(String, String), usize> = Default::default();
    for (_, text, kind) in &a.furniture_marks {
        let t: String = text.chars().take(66).collect();
        *seen.entry((format!("{kind:?}"), t)).or_default() += 1;
    }
    println!(
        "{} pages, {} lines removed as furniture",
        doc.pages.len(),
        a.furniture_removed
    );
    let mut rows: Vec<_> = seen.into_iter().collect();
    rows.sort_by_key(|((k, _), n)| (k.clone(), std::cmp::Reverse(*n)));
    for ((kind, text), n) in rows.iter().filter(|(_, n)| *n > 1) {
        println!("  {:<11} x{:<4} {:?}", kind, n, text);
    }
    let singles = rows.iter().filter(|(_, n)| *n == 1).count();
    if singles > 0 {
        println!("  ({singles} more seen once)");
    }
    let with: Vec<(usize, &str)> = a
        .folios
        .iter()
        .enumerate()
        .filter_map(|(i, f)| f.as_deref().map(|f| (i, f)))
        .collect();
    let mut distinct: Vec<&str> = with.iter().map(|(_, f)| *f).collect();
    distinct.sort_unstable();
    distinct.dedup();
    if with.is_empty() {
        println!("no folio: no page-number line covers enough pages");
    } else {
        let sample: Vec<String> = with
            .iter()
            .take(4)
            .map(|(i, f)| format!("p{} {f:?}", i + 1))
            .collect();
        println!(
            "folio on {} of {} pages ({} distinct): {}{}",
            with.len(),
            doc.pages.len(),
            distinct.len(),
            sample.join(", "),
            if with.len() > 4 { ", …" } else { "" }
        );
        let missing: Vec<String> = a
            .folios
            .iter()
            .enumerate()
            .filter(|(_, f)| f.is_none())
            .take(12)
            .map(|(i, _)| format!("p{}", i + 1))
            .collect();
        if !missing.is_empty() {
            println!("  none on {}", missing.join(" "));
        }
    }
}

/// Histogram of vertical gaps between consecutive lines, normalized by font
/// size. Used to separate intra-paragraph leading from paragraph breaks.
pub(crate) fn leading(target: &Path) {
    let files: Vec<PathBuf> = if target.is_dir() {
        sources_in(target)
    } else {
        vec![target.to_path_buf()]
    };
    let mut hist = vec![0usize; 120];
    let mut over = 0usize;
    for f in &files {
        let Ok(mut doc) = extract_file(f) else {
            continue;
        };
        for p in &mut doc.pages {
            dedup::remove_faux_bold(&mut p.glyphs, 8);
            let ls = line::assemble_page(&p.glyphs);
            for w in ls.windows(2) {
                let (a, b) = (&w[0], &w[1]);
                if a.rotation_bucket != b.rotation_bucket {
                    continue;
                }
                // Only compare lines that share horizontal extent; side-by-side
                // columns are not vertically adjacent in any meaningful sense.
                let ov = a.bbox.x1.min(b.bbox.x1) - a.bbox.x0.max(b.bbox.x0);
                if ov <= 0.0 {
                    continue;
                }
                let fs = a.font_size.max(b.font_size).max(1.0) as f64;
                // Baseline-to-baseline distance is the standard measure of
                // leading and is stable against glyph height variation.
                let d = (b.bbox.y0 - a.bbox.y0) / fs;
                if d <= 0.0 {
                    continue;
                }
                let bucket = (d / 0.05) as usize;
                if bucket < hist.len() {
                    hist[bucket] += 1;
                } else {
                    over += 1;
                }
            }
        }
    }
    let total: usize = hist.iter().sum::<usize>() + over;
    let max = *hist.iter().max().unwrap_or(&1);
    println!(
        "line-to-line vertical distance over {} file(s), {} pairs",
        files.len(),
        total
    );
    let mut cum = 0usize;
    for (i, &n) in hist.iter().enumerate() {
        cum += n;
        if n * 150 < max && !(20..=60).contains(&i) {
            continue;
        }
        let lo = i as f64 * 0.05;
        println!(
            "  {:>4.2}-{:<4.2} {:>7} {:>5.1}% cum{:>5.1}% {}",
            lo,
            lo + 0.05,
            n,
            100.0 * n as f64 / total as f64,
            100.0 * cum as f64 / total as f64,
            "#".repeat((n * 55 / max.max(1)).min(55))
        );
    }
    println!(
        "  >6.00      {:>7} {:>5.1}%",
        over,
        100.0 * over as f64 / total as f64
    );
}

/// Dump the link annotations and the text each one covers.
///
/// The anchor column is the point: an annotation whose rectangle sits over no
/// glyphs is a link on an image, and one whose text does not appear in any
/// element is a link the emitters cannot mark up.
pub(crate) fn links(pdf: &Path) {
    let data = std::fs::read(pdf).expect("read");
    let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(data.clone())).expect("parse");
    let links = fluree_doc_pdf::link::extract(&raw);
    if links.is_empty() {
        println!("(no link annotations in this document)");
        return;
    }
    let mut doc = fluree_doc_pdf::extract_bytes(data).expect("extract");
    let ol = fluree_doc_pdf::outline::extract(&raw);
    let mut a = fluree_doc_pdf::document::analyze_with(&mut doc, &ol, &opts_for(pdf));
    fluree_doc_pdf::link::attach(&mut a.elements, &links, &doc.pages);
    let resolved: usize = a
        .elements
        .iter()
        .filter_map(|e| e.links.as_ref())
        .flatten()
        .filter(|l| l.span().is_some())
        .count();
    println!("{} link annotation(s)", links.len());
    for l in links.iter().take(60) {
        println!(
            "  p{} [{:.0},{:.0},{:.0},{:.0}] {}",
            l.page,
            l.bbox.x0,
            l.bbox.y0,
            l.bbox.x1,
            l.bbox.y1,
            fluree_doc_model::Link {
                target: l.target.clone(),
                begin: None,
                end: None
            }
            .href()
        );
    }
    if links.len() > 60 {
        println!("  … {} more", links.len() - 60);
    }
    println!("\nanchors located in element text: {resolved}");
    for e in &a.elements {
        let Some(ls) = &e.links else { continue };
        for l in ls {
            match l.span() {
                Some((b, end)) => {
                    let anchor: String = e.text.chars().skip(b).take(end - b).collect();
                    println!("  p{} \"{anchor}\" -> {}", e.page, l.href());
                }
                None => println!("  p{} (whole element) -> {}", e.page, l.href()),
            }
        }
    }
}

/// Dump the PDF outline/bookmark tree.
pub(crate) fn outline_cmd(pdf: &Path) {
    let data = std::fs::read(pdf).expect("read");
    let doc = hayro_syntax::Pdf::new(std::sync::Arc::new(data)).expect("parse");
    let items = fluree_doc_pdf::outline::extract(&doc);
    if items.is_empty() {
        println!("(no outline in this document)");
        return;
    }
    println!("{} outline items", items.len());
    for i in items.iter().take(40) {
        println!(
            "  {}{}",
            "  ".repeat(i.level - 1),
            i.title.chars().take(80).collect::<String>()
        );
    }
    if items.len() > 40 {
        println!("  … {} more", items.len() - 40);
    }
}

/// Assemble blocks within each column separately, so "was this line wrapped?"
/// The layout pipeline, in one place: extract → dedup → columns → lines →
/// furniture → blocks (per column). Both `blocks` and `headings` run this, so
/// they cannot drift apart.
struct Doc {
    pages: Vec<Vec<fluree_doc_pdf::Block>>,
    leading: f64,
    furniture_removed: usize,
    body_lines: usize,
}

fn pipeline(pdf: &Path) -> Doc {
    let mut doc = extract_file(pdf).expect("extract");
    let mut cols_per_page: Vec<Vec<Vec<fluree_doc_pdf::Line>>> = Vec::new();
    let mut flat: Vec<(Vec<fluree_doc_pdf::Line>, f64)> = Vec::new();
    let mut fills_per_page: Vec<Vec<fluree_doc_pdf::geom::BBox>> = Vec::new();
    for p in &mut doc.pages {
        dedup::remove_faux_bold(&mut p.glyphs, 8);
        // The page's own bands and lattice are not structure, here as in
        // the pipeline; what remains marks checkboxes and row bands.
        rule::strip_layout_lattice(&mut p.rules, p.width, p.height);
        rule::strip_page_decoration(&mut p.rules, &mut p.fills, p.width, p.height);
        let cols = line::assemble_columns(&p.glyphs);
        flat.push((cols.iter().flatten().cloned().collect(), p.height));
        cols_per_page.push(cols);
        fills_per_page.push(p.fills.iter().map(|f| f.bbox).collect());
    }
    let marks = furniture::detect(&flat);
    let bare: Vec<Vec<fluree_doc_pdf::Line>> = flat.iter().map(|(l, _)| l.clone()).collect();
    let leading = block::modal_leading(&bare);

    let (mut removed, mut body_lines) = (0usize, 0usize);
    let mut pages = Vec::new();
    for (pi, cols) in cols_per_page.iter().enumerate() {
        removed += marks[pi].len();
        // Furniture indices are into the flattened per-page line list, so walk
        // the columns in the same order to line the indices up.
        let mut idx = 0usize;
        let mut out = Vec::new();
        for col in cols {
            let kept: Vec<fluree_doc_pdf::Line> = col
                .iter()
                .enumerate()
                .filter(|(k, _)| !marks[pi].contains_key(&(idx + k)))
                .map(|(_, l)| l.clone())
                .collect();
            idx += col.len();
            body_lines += kept.len();
            // Per column: "was this line wrapped?" is judged against the
            // column's own right edge, not the page's. The page's fills mark
            // checkboxes and row bands, and its OCR characters lines apart,
            // as they do in the pipeline.
            let checkboxes = rule::checkboxes(&doc.pages[pi].fills);
            let ocr = block::ocr_centers(&doc.pages[pi]);
            out.extend(
                block::assemble_with_marks(&kept, leading, &checkboxes, &fills_per_page[pi], &ocr)
                    .into_iter()
                    .flat_map(block::split_structural_prefix),
            );
        }
        pages.push(out);
    }
    Doc {
        pages,
        leading,
        furniture_removed: removed,
        body_lines,
    }
}

/// Paragraph blocks with furniture stripped.
pub(crate) fn blocks(pdf: &Path, page: Option<usize>) {
    let d = pipeline(pdf);
    let total: usize = d.pages.iter().map(|p| p.len()).sum();
    if let Some(pi) = page {
        for b in d.pages.get(pi).into_iter().flatten() {
            let m = b
                .marker
                .as_deref()
                .map(|m| format!("{m:?} "))
                .unwrap_or_default();
            println!(
                "  [{} line(s) sz={:.1} y={:.0}] {}{}",
                b.lines.len(),
                b.font_size,
                b.bbox.y0,
                m,
                b.text().chars().take(96).collect::<String>()
            );
        }
    }
    println!(
        "modal leading {:.2}x — {} body lines -> {} blocks ({} furniture removed)",
        d.leading, d.body_lines, total, d.furniture_removed
    );
}

/// Detected headings with their level and which signal produced them.
pub(crate) fn headings(pdf: &Path) {
    let data = std::fs::read(pdf).expect("read");
    let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(data)).expect("parse");
    let ol = outline::extract(&raw);
    let d = pipeline(pdf);
    let hs = heading::detect(&d.pages, &ol);
    let body = heading::body_font_size(&d.pages);
    let mut by_ev: std::collections::BTreeMap<String, usize> = Default::default();
    for h in &hs {
        *by_ev.entry(format!("{:?}", h.evidence)).or_default() += 1;
    }
    println!(
        "body font {body:.1}pt · outline {} items · {} headings",
        ol.len(),
        hs.len()
    );
    for (k, n) in &by_ev {
        println!("   {k:<10} {n}");
    }
    // Precision per evidence source, so tightening targets the right detector.
    // FDOC_HEADING_SOURCES=1 fdoc headings <pdf> | join against ground truth.
    if std::env::var("FDOC_HEADING_SOURCES").is_ok() {
        for h in &hs {
            println!("SRC\t{:?}\t{}", h.evidence, h.text);
        }
        return;
    }
    for h in hs.iter().take(20) {
        println!(
            "  p{:<3} h{} [{:?}] {}",
            h.page + 1,
            h.level,
            h.evidence,
            h.text.chars().take(78).collect::<String>()
        );
    }
}

/// Ruling lines and filled areas — the raw geometry table detection consumes.
pub(crate) fn fidelity(pdf: &Path) {
    // The control the measure lives or dies by: our own reading can only
    // report glyphs that exist, so it must come out near zero. Anything else
    // means the check is wrong, not the reading.
    let doc = extract_file(pdf).expect("extract");
    let mut d = extract_file(pdf).expect("extract");
    let a = fluree_doc_pdf::document::analyze(&mut d, &[]);
    let mut tot = 0usize;
    let mut bad = 0usize;
    for (pi, p) in doc.pages.iter().enumerate() {
        let lines = fluree_doc_pdf::fidelity::page_lines(&p.glyphs);
        for e in a.elements.iter().filter(|e| e.page == pi) {
            let text = match &e.cells {
                Some(rows) => rows
                    .iter()
                    .map(|r| r.join(" "))
                    .collect::<Vec<_>>()
                    .join(" "),
                None => e.text.clone(),
            };
            for v in fluree_doc_pdf::fidelity::values(&text) {
                tot += 1;
                if !fluree_doc_pdf::fidelity::on_page(&v, &lines) {
                    bad += 1;
                    if bad <= 8 {
                        println!("  p{pi} not on page: {v:?}");
                    }
                }
            }
        }
    }
    let pct = if tot == 0 {
        0.0
    } else {
        bad as f64 / tot as f64 * 100.0
    };
    println!("{bad} of {tot} values not found ({pct:.2}%)");
}

pub(crate) fn figures(pdf: &Path, page: Option<usize>) {
    let doc = extract_file(pdf).expect("extract");
    let mut total = 0usize;
    for p in &doc.pages {
        let mut fills = p.fills.clone();
        fluree_doc_pdf::rule::strip_highlights(&mut fills, &p.glyphs);
        let found = fluree_doc_pdf::figure::detect(&fills, &p.rules, p.index, (p.width, p.height));
        total += found.len();
        if found.is_empty() || (page.is_some() && page != Some(p.index)) {
            continue;
        }
        for f in &found {
            println!(
                "page {:4} shapes={:3} bbox=({:.0},{:.0})-({:.0},{:.0}) {:.0}x{:.0}",
                p.index,
                f.shapes,
                f.bbox.x0,
                f.bbox.y0,
                f.bbox.x1,
                f.bbox.y1,
                f.bbox.width(),
                f.bbox.height()
            );
        }
    }
    println!("{total} figure region(s) across {} pages", doc.pages.len());
}

pub(crate) fn rules(pdf: &Path, page: Option<usize>) {
    let mut doc = extract_file(pdf).expect("extract");
    let (mut h, mut v, mut f) = (0usize, 0usize, 0usize);
    for p in doc.pages.iter_mut() {
        // Rules the page draws with characters, as the pipeline reads them.
        fluree_doc_pdf::textart::draw_rules(&mut p.glyphs, &mut p.rules, p.index);
        let p = &*p;
        let ph = p
            .rules
            .iter()
            .filter(|r| r.orientation == fluree_doc_pdf::rule::Orientation::Horizontal)
            .count();
        let pv = p.rules.len() - ph;
        h += ph;
        v += pv;
        f += p.fills.len();
        if page == Some(p.index) {
            println!(
                "page {}: {ph} horizontal, {pv} vertical rules, {} fills",
                p.index,
                p.fills.len()
            );
            let mut rs: Vec<_> = p.rules.iter().collect();
            rs.sort_by(|a, b| a.axis_pos().partial_cmp(&b.axis_pos()).unwrap());
            for r in rs.iter().take(14) {
                println!(
                    "   {:?} axis={:7.1} len={:6.1} bbox=({:.0},{:.0})-({:.0},{:.0})",
                    r.orientation,
                    r.axis_pos(),
                    r.length(),
                    r.bbox.x0,
                    r.bbox.y0,
                    r.bbox.x1,
                    r.bbox.y1
                );
            }
            for fill in p.fills.iter().take(60) {
                println!(
                    "   Fill bbox=({:.0},{:.0})-({:.0},{:.0})",
                    fill.bbox.x0, fill.bbox.y0, fill.bbox.x1, fill.bbox.y1
                );
            }
        }
    }
    println!(
        "total: {h} horizontal, {v} vertical rules, {f} fills across {} pages",
        doc.pages.len()
    );
}

/// Detected table grids, with text routed into cells.
pub(crate) fn tables(pdf: &Path, page: Option<usize>) {
    let mut doc = extract_file(pdf).expect("extract");
    let mut total = 0usize;
    for p in &mut doc.pages {
        dedup::remove_faux_bold(&mut p.glyphs, 8);
        let frames = fluree_doc_pdf::textart::draw_rules(&mut p.glyphs, &mut p.rules, p.index);
        dedup::remove_struck_underlines(&mut p.glyphs);
        let mut grids = fluree_doc_pdf::table::detect_framed(&p.rules, p.index, &frames);
        total += grids.len();
        if page != Some(p.index) {
            continue;
        }
        for g in grids.iter_mut() {
            g.trim_to_content(&p.glyphs);
            let spanning = g.split_by_alignment(&p.glyphs);
            println!(
                "grid on p{}: {} cols x {} rows{}",
                g.page,
                g.cols(),
                g.rows(),
                if spanning.is_empty() {
                    String::new()
                } else {
                    format!(" (columns from alignment; rows {spanning:?} span them)")
                }
            );
            let cells = g.cell_texts(&p.glyphs);
            for r in 0..g.rows() {
                if spanning.contains(&r) {
                    println!("  | {}", g.row_text(&p.glyphs, r));
                    continue;
                }
                let row: Vec<&str> = (0..g.cols())
                    .map(|c| cells[r * g.cols() + c].as_str())
                    .collect();
                println!("  | {}", row.join(" | "));
            }
        }
    }
    println!("{total} grid(s) across {} pages", doc.pages.len());
}

/// Diagnostic: how many aligned-table candidates exist before/after the
/// corroboration gate, and why they are rejected.
pub(crate) fn aligned_diag(pdf: &Path) {
    let mut doc = extract_file(pdf).expect("extract");
    for p in doc.pages.iter_mut() {
        dedup::remove_faux_bold(&mut p.glyphs, 8);
        let lines = line::assemble_page(&p.glyphs);
        let candidates = fluree_doc_pdf::table::detect_aligned_candidates(&p.glyphs, p.index);
        let gated = fluree_doc_pdf::table::detect_aligned(&p.glyphs, &p.rules, &p.fills, p.index);
        println!(
            "page {}: {} lines, {} h-rules, {} fills -> {} candidates, {} corroborated",
            p.index,
            lines.len(),
            p.rules
                .iter()
                .filter(|r| r.orientation == fluree_doc_pdf::rule::Orientation::Horizontal)
                .count(),
            p.fills.len(),
            candidates.len(),
            gated.len()
        );
        for g in candidates.iter().take(3) {
            let accepted = gated.iter().any(|accepted| accepted.bbox == g.bbox);
            println!(
                "   cand {}x{} bbox=({:.0},{:.0})-({:.0},{:.0}) {}",
                g.cols(),
                g.rows(),
                g.bbox.x0,
                g.bbox.y0,
                g.bbox.x1,
                g.bbox.y1,
                if accepted {
                    "accepted"
                } else {
                    "rejected by geometry/short-table gate"
                }
            );
        }
    }
}

/// Detected column regions per page, with the x-occupancy profile that produced
/// them — for diagnosing gutters that should have been found and were not.
pub(crate) fn columns(pdf: &Path, page: Option<usize>) {
    let mut doc = extract_file(pdf).expect("extract");
    for p in doc.pages.iter_mut() {
        dedup::remove_faux_bold(&mut p.glyphs, 8);
        let cols = fluree_doc_pdf::column::detect_with_rules(&p.glyphs, &p.rules, p.index);
        println!(
            "page {}: {} column(s) {:?}",
            p.index,
            cols.len(),
            cols.iter()
                .map(|c| (c.x0.round(), c.x1.round()))
                .collect::<Vec<_>>()
        );
        if page == Some(p.index) {
            // Coarse ink profile across x, to show where the gutter is.
            let boxed: Vec<_> = p
                .glyphs
                .iter()
                .filter(|g| g.bbox.is_some() && g.is_horizontal())
                .collect();
            if boxed.is_empty() {
                continue;
            }
            let x0 = boxed
                .iter()
                .map(|g| g.bbox.unwrap().x0)
                .fold(f64::MAX, f64::min);
            let x1 = boxed
                .iter()
                .map(|g| g.bbox.unwrap().x1)
                .fold(f64::MIN, f64::max);
            let n = 60usize;
            let mut hist = vec![0usize; n];
            for g in &boxed {
                let b = g.bbox.unwrap();
                let a = (((b.x0 - x0) / (x1 - x0) * n as f64) as usize).min(n - 1);
                let z = (((b.x1 - x0) / (x1 - x0) * n as f64) as usize).min(n - 1);
                #[allow(clippy::needless_range_loop)]
                for i in a..=z {
                    hist[i] += 1;
                }
            }
            let mx = *hist.iter().max().unwrap_or(&1);
            println!("  x-profile {:.0}..{:.0}:", x0, x1);
            print!("  ");
            for h in &hist {
                print!(
                    "{}",
                    if *h == 0 {
                        ' '
                    } else if h * 8 < mx {
                        '.'
                    } else {
                        '#'
                    }
                );
            }
            println!();
        }
    }
}

/// Glyph weight histogram, bucketed by font size.
///
/// Bold detection is the weakest link in heading typography: a font whose
/// descriptor omits `/FontWeight` and whose PostScript name uses a house
/// convention (URW writes the bold face as `-Medi`) reads as regular, and every
/// weight-based signal silently goes dark. This shows what the extractor saw.
pub(crate) fn weights(path: &Path) {
    let doc = extract_file(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let mut hist: std::collections::BTreeMap<(i32, Option<u32>), usize> = Default::default();
    for page in &doc.pages {
        for g in &page.glyphs {
            if g.text.trim().is_empty() {
                continue;
            }
            *hist
                .entry(((g.font_size * 4.0).round() as i32, g.weight))
                .or_default() += 1;
        }
    }
    for ((sz, w), n) in &hist {
        println!(
            "  sz={:5.2}  weight={:<8}  {n:6} glyphs",
            *sz as f64 / 4.0,
            w.map_or("None".to_string(), |x| x.to_string())
        );
    }
}

/// Raw glyphs in draw order, optionally restricted to a y-band.
///
/// The layout passes all consume sorted views; when a line comes out scrambled
/// the question is always what the extractor actually saw, in the order the
/// page drew it. `lines` cannot answer that — it has already sorted.
pub(crate) fn glyphs(path: &Path, page: usize, y0: Option<f64>, y1: Option<f64>) {
    let doc = extract_file(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let Some(p) = doc.pages.get(page) else {
        eprintln!("no page {page}");
        return;
    };
    for g in &p.glyphs {
        let y = g.origin.1;
        if y0.is_some_and(|lo| y < lo) || y1.is_some_and(|hi| y > hi) {
            continue;
        }
        let b = g
            .bbox
            .map(|b| format!("({:7.2},{:7.2})-({:7.2},{:7.2})", b.x0, b.y0, b.x1, b.y1))
            .unwrap_or_else(|| "(no bbox)".into());
        println!(
            "  #{:<5} {:8.2},{:7.2}  sz={:5.2} weight={:<4} rot={:6.1} adv={:<6} {b}  {:?}",
            g.draw_index,
            g.origin.0,
            y,
            g.font_size,
            g.weight
                .map(|w| w.to_string())
                .unwrap_or_else(|| "-".into()),
            g.rotation_deg,
            g.advance
                .map(|a| format!("{a:.2}"))
                .unwrap_or_else(|| "-".into()),
            g.text
        );
    }
}

/// Confidence below which a layout detector's box is not worth acting on.
const LAYOUT_TABLE_SCORE: f64 = 0.6;

/// Share of a detected table that must fall inside a crop before the crop is
/// called a table crop. A region clipping a table's corner is not one.
const HINT_CONTAINMENT: f64 = 0.6;

/// The layout detector's table boxes for one page, in PDF points.
///
/// Sidecars are written at [`VLM_RENDER_SCALE`], so their coordinates are
/// halved back to page space here. Absent directory, absent file and
/// unparseable file all mean the same thing — no boxes — because every
/// caller treats the detector as an optional second opinion.
pub(crate) fn layout_tables(
    dir: Option<&Path>,
    stem: &str,
    page: usize,
) -> Vec<fluree_doc_pdf::geom::BBox> {
    let Some(dir) = dir else {
        return Vec::new();
    };
    let sidecar = dir.join(format!("{stem}_p{page}_page.json"));
    let Ok(txt) = std::fs::read_to_string(&sidecar) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return Vec::new();
    };
    let sc = f64::from(VLM_RENDER_SCALE);
    v.as_array()
        .into_iter()
        .flatten()
        .filter(|lb| {
            lb["label"].as_str() == Some("table")
                && lb["score"].as_f64().unwrap_or(0.0) >= LAYOUT_TABLE_SCORE
        })
        .filter_map(|lb| {
            let c: Vec<f64> = lb["box"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_f64)
                .collect();
            (c.len() == 4).then(|| fluree_doc_pdf::geom::BBox {
                x0: c[0] / sc,
                y0: c[1] / sc,
                x1: c[2] / sc,
                y1: c[3] / sc,
            })
        })
        .collect()
}

/// Does `crop` hold most of a table the layout detector found?
///
/// The routing triggers know a region is unreadable but not what it is, and
/// a model asked to decide for itself gets it wrong on exactly the tables
/// that matter: grids the text layer could not read come back transcribed as
/// one value per line and score TEDS 0.0, on pages where the detector already
/// says "table" with 0.98 and 0.96 confidence. Passing that on costs nothing
/// and replaces a guess with evidence.
fn holds_a_table(crop: &fluree_doc_pdf::geom::BBox, tables: &[fluree_doc_pdf::geom::BBox]) -> bool {
    tables.iter().any(|t| {
        let ix = (crop.x1.min(t.x1) - crop.x0.max(t.x0)).max(0.0);
        let iy = (crop.y1.min(t.y1) - crop.y0.max(t.y0)).max(0.0);
        let area = (t.x1 - t.x0) * (t.y1 - t.y0);
        area > 0.0 && ix * iy >= HINT_CONTAINMENT * area
    })
}

/// Render every routed page or region to `outdir` as PNG, with a JSONL
/// manifest (`manifest.jsonl`) recording where each crop belongs so the
/// hybrid adapter can splice VLM output back at the right position.
pub(crate) fn render_routed(
    path: &Path,
    outdir: &Path,
    ocr_regions: bool,
    stored_resolution: bool,
) {
    use hayro_syntax::Pdf;
    use std::io::Write;
    use std::sync::Arc;

    std::fs::create_dir_all(outdir).expect("create outdir");
    let files: Vec<PathBuf> = if path.is_dir() {
        sources_in(path)
    } else {
        vec![path.to_path_buf()]
    };
    let mut manifest =
        std::fs::File::create(outdir.join("manifest.jsonl")).expect("create manifest");
    let (mut n_pages, mut n_crops) = (0usize, 0usize);
    let tiers = crate::commands::common::TierConfig::from_env();
    let layout = tiers.layout_boxes.as_deref();
    // Crops are named by stem: a second input of a stem already cropped
    // (`report.pdf` and `report.png`) would overwrite the first's crops and
    // take their readings.
    let mut stems = std::collections::HashSet::new();
    // The limit `convert --ocr` reads under, from the same config.
    let max_ocr_crops = match ocr_regions.then(crate::config::load) {
        None => 0,
        Some(Ok(l)) => l.config.ocr.max_crops(),
        Some(Err(e)) => {
            eprintln!("warning: {e}");
            crate::config::DEFAULT_OCR_MAX_CROPS
        }
    };

    for f in &files {
        let stem = crate::commands::common::stem_of(f);
        if !stems.insert(stem.to_string()) {
            eprintln!(
                "warning: {}: another input is named {stem}, and its crops would share names with this one's: skipped",
                f.display()
            );
            continue;
        }
        // A bare image is one page of pixels: its crop is the image itself,
        // decoded upright, at its own resolution. Its first bytes say so;
        // nothing else is read twice.
        if is_image_file(f) {
            let stem = crate::commands::common::stem_of(f);
            match std::fs::read(f)
                .map_err(|e| e.to_string())
                .and_then(|bytes| image_crop(&bytes, stem, outdir))
            {
                Ok(rec) => {
                    n_pages += 1;
                    writeln!(manifest, "{rec}").unwrap();
                }
                Err(e) => eprintln!("warning: {}: {e}", f.display()),
            }
            continue;
        }
        let doc = match extract_file(f) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let data = std::fs::read(f).expect("read pdf");
        let pdf = Pdf::new(Arc::new(data)).expect("parse pdf");
        // Route first; render only when something needs the VLM. A job is a
        // page with either full/region routing or table-confidence anchors
        // (named crops so the adapter can find each by its anchor token).
        // The crop set is the library's `escalate::plan`, which
        // `convert --escalate` and `triage` take too, so they cannot drift
        // apart.
        let stem = crate::commands::common::stem_of(f);
        let hints = fluree_doc_pdf::escalate::CropHints {
            regions_apart: ocr_regions,
            ..crate::escalate::jobs::hints(&doc, stem, layout, false)
        };
        let (mut jobs, reasons) = fluree_doc_pdf::escalate::plan_explained(
            &doc,
            &fluree_doc_pdf::outline::extract(&pdf),
            &tiers.options_for(stem),
            &hints,
        );
        // The crops `convert --ocr` reads, from the same `ocr::ocr_jobs`
        // and under the same limit, so the three-step path reads what it
        // reads: the regions a model call would not pay for are added, and a
        // page read whole on a doubt about its structure, which an OCR
        // engine skips, gets its regions as crops of their own too.
        let small = if ocr_regions {
            let (ocr, capped) = fluree_doc_pdf::ocr::ocr_jobs(&doc, None, max_ocr_crops);
            if capped > 0 {
                eprintln!(
                    "{}",
                    crate::commands::convert::ocr_cap_note(stem, max_ocr_crops, capped)
                );
            }
            let routed: std::collections::HashMap<usize, usize> = doc
                .pages
                .iter()
                .map(|p| match fluree_doc_pdf::route::decide(p).0 {
                    fluree_doc_pdf::route::Route::VlmRegions(r) => (p.index, r.len()),
                    _ => (p.index, 0),
                })
                .collect();
            add_ocr_jobs(&mut jobs, ocr, |page| {
                routed.get(&page).copied().unwrap_or(0)
            })
        } else {
            Default::default()
        };
        if jobs.is_empty() {
            continue;
        }

        let links = fluree_doc_pdf::link::extract(&pdf);
        // The model scale unless asked for the stored resolution (#11),
        // which an OCR engine reads better and a model bills more for.
        let mode = if stored_resolution {
            fluree_doc_pdf::escalate::CropScale::Stored
        } else {
            fluree_doc_pdf::escalate::CropScale::Model
        };
        // Each crop is written as it is cut; its manifest line waits so the
        // document's lines come in the jobs' order, which a page rendered at
        // several scales hands its crops over out of.
        let order = fluree_doc_pdf::escalate::crop_order(&jobs);
        let mut lines: Vec<(usize, serde_json::Value)> = Vec::new();
        fluree_doc_pdf::escalate::for_each_crop_of(&pdf, &doc, &jobs, mode, |crop| {
            let at = order.get(&crop.name).copied().unwrap_or(usize::MAX);
            let page_idx = crop.page;
            let name = format!("{stem}_{}.png", crop.name);
            let file = std::fs::File::create(outdir.join(&name)).expect("create png");
            let mut enc = png::Encoder::new(
                std::io::BufWriter::new(file),
                crop.pixels.width,
                crop.pixels.height,
            );
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()
                .unwrap()
                .write_image_data(&crop.pixels.data)
                .unwrap();
            // A link is drawn as nothing: colour and an underline at most. A
            // reader looking at pixels sees an anchor and no address, and
            // will supply one — so the crop carries the addresses the file
            // already states, and the prompt has something true to hold to.
            let page_links: Vec<(fluree_doc_pdf::geom::BBox, String, serde_json::Value)> = links
                .iter()
                .filter(|l| l.page == page_idx)
                .map(|l| {
                    let text = doc
                        .pages
                        .iter()
                        .find(|p| p.index == page_idx)
                        .map(|p| fluree_doc_pdf::link::anchor_text(p, l.bbox))
                        .unwrap_or_default();
                    let target = match &l.target {
                        fluree_doc_model::Target::Uri { uri } => serde_json::json!(uri),
                        fluree_doc_model::Target::Page { page } => {
                            serde_json::json!({ "page": page })
                        }
                    };
                    (l.bbox, text, target)
                })
                .filter(|(_, text, _)| !text.is_empty())
                .collect();
            // One target, one hint. A link over wrapped text is annotated per
            // line, so a URL set inside a narrow column arrives as ten
            // fragments; listing each would tell the model to mark up ten
            // links where the page has one.
            let links_in = |b: Option<&fluree_doc_pdf::geom::BBox>| -> Vec<serde_json::Value> {
                let mut parts: Vec<(&serde_json::Value, Vec<&str>)> = Vec::new();
                for (r, text, target) in &page_links {
                    if !b.is_none_or(|b| b.intersects(r)) {
                        continue;
                    }
                    match parts.iter_mut().find(|(t, _)| *t == target) {
                        Some((_, fragments)) => fragments.push(text),
                        None => parts.push((target, vec![text])),
                    }
                }
                parts
                    .into_iter()
                    .map(|(target, fragments)| {
                        // A wrapped address breaks mid-token and rejoins with
                        // nothing; wrapped prose rejoins with a space. The
                        // target itself says which happened.
                        let tight = fragments.concat();
                        let text = if fragments.len() > 1
                            && target.as_str().is_some_and(|t| t.contains(tight.trim()))
                        {
                            tight
                        } else {
                            fragments.join(" ")
                        };
                        serde_json::json!({ "text": text, "target": target })
                    })
                    .collect()
            };
            let mut rec = match crop.bbox {
                None => {
                    n_pages += 1;
                    // Why the page is read whole: a reader that cannot see
                    // structure (OCR) has nothing to add to a doubt page.
                    let reason = reasons
                        .get(&page_idx)
                        .expect("every whole page in a job list has a reason")
                        .as_str();
                    serde_json::json!({
                        "doc": stem, "page": page_idx, "kind": "page", "png": name,
                        "reason": reason, "scale": crop.scale,
                    })
                }
                Some(b) => {
                    n_crops += 1;
                    let tag = crop.name.split_once('_').map_or("", |(_, t)| t);
                    let detected = layout_tables(layout, stem, page_idx);
                    serde_json::json!({
                        "doc": stem, "page": page_idx, "kind": "region", "png": name,
                        "bbox": [b.x0, b.y0, b.x1, b.y1],
                        "table": holds_a_table(&b, &detected),
                        "reason": if small.contains(&(page_idx, tag.to_string())) {
                            fluree_doc_pdf::escalate::CropReason::SmallRegion.as_str()
                        } else {
                            fluree_doc_pdf::escalate::CropReason::of_region_tag(tag)
                                .expect("a region crop's tag")
                                .as_str()
                        },
                        "scale": crop.scale,
                    })
                }
            };
            let ls = links_in(crop.bbox.as_ref());
            if !ls.is_empty() {
                rec["links"] = serde_json::Value::Array(ls);
            }
            lines.push((at, rec));
        });
        lines.sort_by_key(|(at, _)| *at);
        for (_, rec) in lines {
            writeln!(manifest, "{rec}").unwrap();
        }
    }
    println!(
        "{n_pages} full pages, {n_crops} region crops -> {}",
        outdir.display()
    );
}

/// Add to a document's crop jobs the crops an OCR engine reads
/// ([`fluree_doc_pdf::ocr::ocr_jobs`]), whose regions are numbered as the
/// model's are, the first `routed(page)` of a page being the ones the router
/// sends a model. A region crop the jobs do not hold yet is added to its
/// page's; a page with no job gets one of its regions; a page read whole on
/// a doubt gets all its regions as crops of their own, besides its whole
/// page, since an OCR engine reads those and skips the page. A page the
/// engine reads whole the model reads whole too. Returns the regions too
/// small for a model, by `(page, tag)`.
pub(crate) fn add_ocr_jobs(
    jobs: &mut fluree_doc_pdf::escalate::CropJobs,
    ocr: fluree_doc_pdf::ocr::OcrJobs,
    routed: impl Fn(usize) -> usize,
) -> std::collections::HashSet<(usize, String)> {
    let mut small = std::collections::HashSet::new();
    let mut doubted = Vec::new();
    for (page, crops) in ocr {
        // A page whose text is pixels: the router's verdict, which the
        // model's plan reads whole too.
        let Some(crops) = crops else { continue };
        small.extend(
            crops
                .iter()
                .filter(|(t, _)| {
                    t.strip_prefix('r')
                        .and_then(|i| i.parse::<usize>().ok())
                        .is_some_and(|i| i >= routed(page))
                })
                .map(|(t, _)| (page, t.clone())),
        );
        match jobs.iter_mut().find(|(pi, _)| *pi == page) {
            Some((_, None)) => doubted.push((page, Some(crops))),
            Some((_, Some(list))) => {
                for c in crops {
                    if !list.iter().any(|(t, _)| *t == c.0) {
                        list.push(c);
                    }
                }
            }
            None => jobs.push((page, Some(crops))),
        }
    }
    jobs.extend(doubted);
    small
}

/// Whether a file starts like a PNG, JPEG or TIFF, by its first bytes.
fn is_image_file(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 8];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    fluree_doc_pdf::image::Format::sniff(&head[..n]).is_some()
}

/// A bare image's one crop: the image decoded upright, over white where it
/// is transparent (as the renderer draws a page), and written as PNG, with
/// its manifest record. Its scale is 1: the page's units are its pixels.
fn image_crop(bytes: &[u8], stem: &str, outdir: &Path) -> Result<serde_json::Value, String> {
    let mut image = fluree_doc_image::decode(bytes)?;
    // Written in the layout it was decoded in. A transparent screenshot is
    // black ink on nothing: flattened by a reader that drops the alpha
    // channel, it would be black on black, so it is laid on white here.
    let color = match image.pixels.layout {
        fluree_doc_vision::Layout::Luma8 => png::ColorType::Grayscale,
        fluree_doc_vision::Layout::Rgb8 => png::ColorType::Rgb,
        fluree_doc_vision::Layout::Rgba8 => {
            for p in image.pixels.data.chunks_exact_mut(4) {
                let a = u16::from(p[3]);
                for c in &mut p[..3] {
                    *c = ((u16::from(*c) * a + 255 * (255 - a)) / 255) as u8;
                }
                p[3] = 255;
            }
            png::ColorType::Rgba
        }
    };
    let doc = fluree_doc_image::document(&image);
    let reason = match fluree_doc_pdf::route::decide(&doc.pages[0]).0 {
        fluree_doc_pdf::route::Route::Vlm(r) => fluree_doc_pdf::escalate::CropReason::from(r),
        _ => fluree_doc_pdf::escalate::CropReason::Scanned,
    };
    if image.more_pages {
        eprintln!("warning: {stem}: only the first page of a multi-page image is cropped");
    }
    if image.truncated {
        eprintln!("warning: {stem}: the file ends before its image does; part of it may be blank");
    }
    let name = format!("{stem}_p0_full.png");
    let file = std::fs::File::create(outdir.join(&name)).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(file),
        image.pixels.width,
        image.pixels.height,
    );
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut w| w.write_image_data(&image.pixels.data))
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "doc": stem, "page": 0, "kind": "page", "png": name,
        "reason": reason.as_str(), "scale": 1.0,
    }))
}

/// Render the crops a region manifest asks for, through
/// `escalate::render_crops` — the pipeline's own crop path — so an
/// evaluation scores the pixels the pipeline actually sends: same
/// renderer, same scale, same margin. One PNG per entry, `{name}.png`;
/// existing files are kept.
///
/// The manifest is `eval/llm-tier`'s `regions.json`: an array of
/// `{name: "{doc}_p{page}_{tag}", doc, page, bbox: {x0, y0, x1, y1}}`.
/// Every entry must end up on disk or the command exits nonzero — a
/// silently absent crop scores as a model defect downstream.
pub(crate) fn render_crops(manifest: &Path, corpus: &Path, outdir: &Path) {
    use fluree_doc_pdf::escalate;
    use fluree_doc_pdf::geom::BBox;
    use hayro_syntax::Pdf;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    let text = std::fs::read_to_string(manifest).expect("read manifest");
    let regions: Vec<serde_json::Value> = serde_json::from_str(&text).expect("parse manifest");
    std::fs::create_dir_all(outdir).expect("create outdir");

    let mut wanted: Vec<String> = Vec::new();
    let mut by_doc: BTreeMap<String, BTreeMap<usize, Vec<(String, BBox)>>> = BTreeMap::new();
    for r in &regions {
        let name = r["name"].as_str().expect("manifest entry: name");
        let doc = r["doc"].as_str().expect("manifest entry: doc");
        let page = r["page"].as_u64().expect("manifest entry: page") as usize;
        wanted.push(name.to_string());
        if outdir.join(format!("{name}.png")).exists() {
            continue;
        }
        // The tag is the name's last segment; the crop comes back from the
        // library as `p{page}_{tag}`, so the name must spell exactly that.
        let (_, tag) = name.rsplit_once('_').expect("manifest entry: unnamed tag");
        assert_eq!(
            name,
            format!("{doc}_p{page}_{tag}"),
            "manifest name does not spell {{doc}}_p{{page}}_{{tag}}"
        );
        let b = &r["bbox"];
        let bbox = BBox {
            x0: b["x0"].as_f64().expect("bbox.x0"),
            y0: b["y0"].as_f64().expect("bbox.y0"),
            x1: b["x1"].as_f64().expect("bbox.x1"),
            y1: b["y1"].as_f64().expect("bbox.y1"),
        };
        by_doc
            .entry(doc.to_string())
            .or_default()
            .entry(page)
            .or_default()
            .push((tag.to_string(), bbox));
    }

    let mut n = 0usize;
    for (doc, pages) in &by_doc {
        let pdf_path = corpus.join(format!("{doc}.pdf"));
        let data =
            std::fs::read(&pdf_path).unwrap_or_else(|e| panic!("read {}: {e}", pdf_path.display()));
        let pdf = Pdf::new(Arc::new(data))
            .unwrap_or_else(|e| panic!("parse {}: {e:?}", pdf_path.display()));
        let jobs: escalate::CropJobs = pages
            .iter()
            .map(|(page, list)| (*page, Some(list.clone())))
            .collect();
        // The manifest's boxes are elements' `doc:bbox`, in the display
        // frame already.
        for crop in escalate::render_crops_displayed(&pdf, &jobs) {
            std::fs::write(outdir.join(format!("{doc}_{}.png", crop.name)), &crop.png)
                .expect("write crop");
            n += 1;
        }
    }
    println!("{n} crops rendered -> {}", outdir.display());

    let missing: Vec<&String> = wanted
        .iter()
        .filter(|name| !outdir.join(format!("{name}.png")).exists())
        .collect();
    if !missing.is_empty() {
        eprintln!("{} manifest entries produced no crop:", missing.len());
        for m in &missing {
            eprintln!("  {m}");
        }
        std::process::exit(1);
    }
}

/// Render every page of every document at the VLM scale — input for the
/// layout-detector arbitration pass, which is cheap enough to run on all
/// pages (no autoregressive decode, ~100ms each on GPU).
pub(crate) fn render_pages(path: &Path, outdir: &Path) {
    use hayro::vello_cpu::color::{AlphaColor, Srgb};
    use hayro::{render, RenderCache, RenderSettings};
    use hayro_syntax::Pdf;
    use std::sync::Arc;

    std::fs::create_dir_all(outdir).expect("create outdir");
    let files: Vec<PathBuf> = if path.is_dir() {
        sources_in(path)
    } else {
        vec![path.to_path_buf()]
    };
    let mut n = 0usize;
    for f in &files {
        let Ok(data) = std::fs::read(f) else { continue };
        let Ok(pdf) = Pdf::new(Arc::new(data)) else {
            continue;
        };
        let pages = pdf.pages();
        let cache = RenderCache::new();
        let white: AlphaColor<Srgb> = AlphaColor::new([1.0, 1.0, 1.0, 1.0]);
        let settings = RenderSettings {
            x_scale: VLM_RENDER_SCALE,
            y_scale: VLM_RENDER_SCALE,
            bg_color: white,
            ..Default::default()
        };
        let stem = f.file_stem().and_then(|x| x.to_str()).unwrap_or("doc");
        for (pi, page) in pages.iter().enumerate() {
            let pix = render(
                page,
                &cache,
                &hayro::hayro_interpret::InterpreterSettings::default(),
                &settings,
            );
            let (w, h) = (pix.width() as u32, pix.height() as u32);
            let rgba: Vec<u8> = pix
                .take_unpremultiplied()
                .iter()
                .flat_map(|p| [p.r, p.g, p.b, p.a])
                .collect();
            let file =
                std::fs::File::create(outdir.join(format!("{stem}_p{pi}_page.png"))).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&rgba).unwrap();
            n += 1;
        }
    }
    println!("{n} pages -> {}", outdir.display());
}

/// Per-stage wall clock over a file or corpus, measured in one process.
///
/// The evaluation harness spawns a process per document, so its per-document
/// figure is mostly startup and page cache — a corpus that parses in under
/// nine milliseconds a document cannot be profiled that way, and a change
/// that removed real work once measured *slower* than the code it replaced.
/// This runs everything in one process, warms up, and reports the median of
/// several passes, so a stage's share is readable and a change to one stage
/// is visible.
pub(crate) fn timings(path: &Path, warmup: usize, runs: usize) {
    use std::time::{Duration, Instant};

    let files: Vec<PathBuf> = if path.is_dir() {
        sources_in(path)
    } else {
        vec![path.to_path_buf()]
    };
    if files.is_empty() {
        println!("no PDFs under {}", path.display());
        return;
    }

    // One pass: extraction timed around the call, stages from the analysis.
    let pass = || -> (Duration, Duration, fluree_doc_pdf::document::StageTimings, usize) {
        let (mut extract, mut whole) = (Duration::ZERO, Duration::ZERO);
        let mut stages = fluree_doc_pdf::document::StageTimings::default();
        let mut pages = 0usize;
        for f in &files {
            let t0 = Instant::now();
            let Ok(mut doc) = extract_file(f) else { continue };
            extract += t0.elapsed();
            pages += doc.pages.len();
            let ol = std::fs::read(f)
                .ok()
                .and_then(|d| hayro_syntax::Pdf::new(std::sync::Arc::new(d)).ok())
                .map(|raw| outline::extract(&raw))
                .unwrap_or_default();
            let opts = opts_for(f);
            let t1 = Instant::now();
            let a = fluree_doc_pdf::document::analyze_with(&mut doc, &ol, &opts);
            whole += t1.elapsed();
            stages.add(&a.timings);
        }
        (extract, whole, stages, pages)
    };

    for _ in 0..warmup {
        pass();
    }
    let mut passes: Vec<(
        Duration,
        Duration,
        fluree_doc_pdf::document::StageTimings,
        usize,
    )> = (0..runs.max(1)).map(|_| pass()).collect();
    passes.sort_by_key(|(e, w, _, _)| *e + *w);
    let (extract, whole, stages, pages) = passes.remove(passes.len() / 2);

    let total = extract + whole;
    let docs = files.len() as u32;
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let share = |d: Duration| 100.0 * d.as_secs_f64() / total.as_secs_f64().max(f64::EPSILON);

    println!(
        "{} documents, {pages} pages — median of {runs} pass(es) after {warmup} warmup",
        files.len()
    );
    println!(
        "  {:<12}{:>9.1} ms{:>9.3} ms/doc{:>8.1}%",
        "extract",
        ms(extract),
        ms(extract / docs),
        share(extract)
    );
    for (name, d) in stages.ranked() {
        println!(
            "  {name:<12}{:>9.1} ms{:>9.3} ms/doc{:>8.1}%",
            ms(d),
            ms(d / docs),
            share(d)
        );
    }
    let other = whole.saturating_sub(stages.measured());
    println!(
        "  {:<12}{:>9.1} ms{:>9.3} ms/doc{:>8.1}%   (emission, interleave, sidecars)",
        "unattributed",
        ms(other),
        ms(other / docs),
        share(other)
    );
    println!(
        "  {:<12}{:>9.1} ms{:>9.3} ms/doc",
        "TOTAL",
        ms(total),
        ms(total / docs)
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluree_doc_pdf::geom::BBox;

    fn b(y: f64) -> BBox {
        BBox {
            x0: 10.0,
            y0: y,
            x1: 100.0,
            y1: y + 20.0,
        }
    }

    #[test]
    fn ocr_regions_join_each_kind_of_page_job() {
        // Page 0 routed with one region, plus a small one; page 1 read whole
        // on a doubt, with a routed region and a small one; page 2 with a
        // small region only; page 3 read whole for its pixels; page 4 routed
        // with two regions, the engine's limit leaving it the first.
        let tagged = |l: &[(&str, f64)]| -> Option<Vec<(String, BBox)>> {
            Some(l.iter().map(|(t, y)| (t.to_string(), b(*y))).collect())
        };
        let mut jobs: fluree_doc_pdf::escalate::CropJobs = vec![
            (0, tagged(&[("r0", 0.0)])),
            (1, None),
            (3, None),
            (4, tagged(&[("r0", 0.0), ("r1", 50.0)])),
        ];
        let ocr: fluree_doc_pdf::ocr::OcrJobs = vec![
            (0, tagged(&[("r0", 0.0), ("r1", 50.0)])),
            (1, tagged(&[("r0", 0.0), ("r1", 50.0)])),
            (2, tagged(&[("r0", 80.0)])),
            (3, None),
            (4, tagged(&[("r0", 0.0)])),
        ];
        let routed = |page: usize| [1, 1, 0, 0, 2][page];
        let small = add_ocr_jobs(&mut jobs, ocr, routed);
        let tags = |page: usize| -> Vec<Option<Vec<String>>> {
            jobs.iter()
                .filter(|(p, _)| *p == page)
                .map(|(_, r)| {
                    r.as_ref()
                        .map(|l| l.iter().map(|(t, _)| t.clone()).collect())
                })
                .collect()
        };
        assert_eq!(tags(0), [Some(vec!["r0".to_string(), "r1".to_string()])]);
        assert_eq!(
            tags(1),
            [None, Some(vec!["r0".to_string(), "r1".to_string()])],
            "the doubted page keeps its whole crop and gets its regions"
        );
        assert_eq!(tags(2), [Some(vec!["r0".to_string()])]);
        assert_eq!(tags(3), [None], "read whole once");
        assert_eq!(
            tags(4),
            [Some(vec!["r0".to_string(), "r1".to_string()])],
            "the model's crops stay whatever the engine's limit"
        );
        let mut small: Vec<(usize, String)> = small.into_iter().collect();
        small.sort();
        assert_eq!(
            small,
            [
                (0, "r1".to_string()),
                (1, "r1".to_string()),
                (2, "r0".to_string())
            ]
        );
    }
}
