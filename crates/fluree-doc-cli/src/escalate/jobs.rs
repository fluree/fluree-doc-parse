//! Which crops a document's own signals ask for.
//!
//! One list, two consumers: `fdoc dev render-routed` writes them to disk for
//! an external reader, and `fdoc convert --escalate` sends them to a
//! configured one. They must agree — a crop set that differs between the two
//! means the cached benchmark scores describe a pipeline nobody runs.

use crate::commands::common::opts_for;
use crate::commands::dev::layout_tables;
use fluree_doc_pdf::{extract_bytes, outline, Document};
use std::path::Path;

/// A page's render jobs: `None` = the whole page, else named crops.
pub(crate) type CropJobs = Vec<(usize, Option<Vec<(String, fluree_doc_pdf::geom::BBox)>>)>;

/// Every crop this document asks for, in page order.
///
/// Empty when it is read deterministically end to end, which is the common
/// case: over the evaluation corpus 113 of 200 documents ask for nothing.
///
/// `f` is used only for its stem, to find the layout sidecars, so a caller
/// holding bytes rather than a file can pass the name it would have had.
pub(crate) fn crops_for(f: &Path, bytes: &[u8], doc: &Document, on_column_doubt: bool) -> CropJobs {
    let mut jobs: CropJobs = Vec::new();
    for p in &doc.pages {
        match fluree_doc_pdf::route::decide(p).0 {
            fluree_doc_pdf::route::Route::Vlm(_) => jobs.push((p.index, None)),
            fluree_doc_pdf::route::Route::VlmRegions(r) => jobs.push((
                p.index,
                Some(
                    r.iter()
                        .enumerate()
                        .map(|(i, b)| (format!("r{i}"), *b))
                        .collect(),
                ),
            )),
            fluree_doc_pdf::route::Route::Deterministic => {}
        }
    }
    // Low-confidence tables, from the analysis pass.
    {
        let Ok(raw) = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes.to_vec())) else {
            return jobs;
        };
        let ol = outline::extract(&raw);
        let Ok(mut d2) = extract_bytes(bytes.to_vec()) else {
            return jobs;
        };
        let mut ropts = opts_for(f);
        ropts.emit_anchors = true;
        let a = fluree_doc_pdf::document::analyze_with(&mut d2, &ol, &ropts);

        // A hierarchy resting on nothing but font size is a whole-page
        // problem, not a region one: the text is all there and legible,
        // and what is wrong is how it is organised. So the escalation is
        // the whole page, and it supersedes any region or table crop on
        // it — a reading that owns the page owns its structure too.
        //
        // Measured: the six documents this fires on gain 1.459 between
        // them, five of six better, the worst loss 0.087. Three of them
        // more than triple. See `column::doubt` for the signal that was
        // tried first and rejected: it flags 22 documents for the same
        // escalation, gains less in total, and makes five worse.
        //
        // Only the doubtful pages, not the whole document. The signal is
        // per page, and a long document with one badly organised page
        // should cost one reading rather than all of them.
        for d in &a.suspect_headings {
            match jobs.iter_mut().find(|(pi, _)| *pi == d.page) {
                Some((_, slot)) => *slot = None,
                None => jobs.push((d.page, None)),
            }
        }
        // Column doubt escalates only when asked for, because whether it
        // helps depends on the document and nothing on the page tells you
        // which kind you have. Measured over the evaluation corpus it is
        // net negative -- fourteen documents better, seven worse, -0.0016
        // -- and the ones it hurts have hierarchies that were already
        // sound. On layout-heavy material the same signal marks exactly
        // the pages that read across their panels.
        //
        // Four discriminators were tried and none separates the two
        // populations: band coverage, missed-gutter count, whether our
        // lines are concatenations of the reading's, and whether the
        // document carries a PDF outline. Until one is found, this is a
        // choice the caller makes about their corpus rather than one the
        // page can make for them — `escalation.on_column_doubt` in the
        // config, or FDOC_ESCALATE_COLUMNS for a single run.
        if on_column_doubt || std::env::var_os("FDOC_ESCALATE_COLUMNS").is_some() {
            for p in &doc.pages {
                if fluree_doc_pdf::column::doubt(&p.glyphs).is_some() {
                    match jobs.iter_mut().find(|(pi, _)| *pi == p.index) {
                        Some((_, slot)) => *slot = None,
                        None => jobs.push((p.index, None)),
                    }
                }
            }
        }
        for e in &a.elements {
            if e.evidence == "table-missing" {
                // Insert anchor: the box is the crop, nothing to re-derive.
                let tag = e
                    .text
                    .trim_start_matches("[[VLMNEW:")
                    .trim_end_matches("]]");
                let Some((_, n)) = tag.split_once(':') else {
                    continue;
                };
                match jobs.iter_mut().find(|(pi, _)| *pi == e.page) {
                    Some((_, Some(list))) => list.push((n.to_string(), e.rect())),
                    Some((_, None)) => {}
                    None => jobs.push((e.page, Some(vec![(n.to_string(), e.rect())]))),
                }
                continue;
            }
            if e.evidence != "table-confidence" {
                continue;
            }
            // text is "[[VLMTAB:pN:tK]]" — the crop takes the tK name.
            let tag = e
                .text
                .trim_start_matches("[[VLMTAB:")
                .trim_end_matches("]]");
            let Some((_, t)) = tag.split_once(':') else {
                continue;
            };
            let mut bbox = e.rect();
            bbox.y1 = bbox.y0 + 0.02; // undo the ordering nudge…
                                      // …by re-deriving the real box from the matching grid: the
                                      // anchor kept only a hairline. Use the element's page and find
                                      // the table element that follows it.
            if let Some(tb) = a.elements.iter().find(|x| {
                x.kind == "doco:Table"
                    && x.page == e.page
                    && (x.rect().y0 - e.rect().y0).abs() < 1.0
            }) {
                bbox = tb.rect();
                // Where the layout detector boxes the same table with
                // different bounds, crop the union: our grid may cover
                // only part of the real table, and the VLM cannot read
                // pixels it is not shown.
                let stem = f.file_stem().and_then(|x| x.to_str()).unwrap_or("doc");
                for lt in layout_tables(stem, e.page) {
                    let ix = (bbox.x1.min(lt.x1) - bbox.x0.max(lt.x0)).max(0.0);
                    let iy = (bbox.y1.min(lt.y1) - bbox.y0.max(lt.y0)).max(0.0);
                    if ix * iy <= 0.0 {
                        continue;
                    }
                    let ux0 = bbox.x0.min(lt.x0);
                    let uy0 = bbox.y0.min(lt.y0);
                    let ux1 = bbox.x1.max(lt.x1);
                    let uy1 = bbox.y1.max(lt.y1);
                    // Expand only when the detector says the table is
                    // substantially larger than our grid; small
                    // disagreements are box jitter and would only bust
                    // crop caches.
                    let a0 = (bbox.x1 - bbox.x0) * (bbox.y1 - bbox.y0);
                    let au = (ux1 - ux0) * (uy1 - uy0);
                    if au >= 1.15 * a0 {
                        bbox.x0 = ux0;
                        bbox.y0 = uy0;
                        bbox.x1 = ux1;
                        bbox.y1 = uy1;
                    }
                }
            }
            match jobs.iter_mut().find(|(pi, _)| *pi == e.page) {
                Some((_, Some(list))) => list.push((t.to_string(), bbox)),
                Some((_, None)) => {} // whole page already renders
                None => jobs.push((e.page, Some(vec![(t.to_string(), bbox)]))),
            }
        }
    }
    jobs
}
