//! Reading a document's escalated crops in the same command that converts it.
//!
//! The engine still does not decide to call a model: the crop list comes from
//! the document's own signals, exactly as it does for an external reader, and
//! the readings go back through the same arbitration. What this adds is the
//! step in the middle, so a user with a configured provider gets one command
//! instead of three.
//!
//! With nothing configured this module is never entered and the binary makes
//! no network connection at all.

pub(crate) mod gemini;
pub(crate) mod jobs;

use crate::config::Config;
use fluree_doc_pdf::escalate::{self, Crop, CropHints, CropJobs, Readings};
use fluree_doc_pdf::Document;
use std::path::Path;

/// Read a bare image: one crop, which is the file itself.
///
/// No rendering step — the bytes already are the page, and re-encoding them
/// would only lose information the reader could have used. No crop selection
/// either: an image has no regions to choose between, because there are no
/// glyphs to say where anything is.
pub(crate) fn read_image(
    file: &Path,
    bytes: &[u8],
    mime: &str,
    config: &Config,
    verbose: bool,
) -> Result<Readings, String> {
    let gemini = &config.escalation.gemini;
    let credentials = gemini
        .credentials
        .as_deref()
        .ok_or("no credentials are set for gemini")?;
    let reader = gemini::Reader::open(credentials, gemini.project.as_deref(), config.model())?;
    if verbose {
        eprintln!(
            "{}: reading as one page ({mime}) with {}",
            file.display(),
            config.model()
        );
    }
    let crop = Crop {
        name: "p0_full".into(),
        page: 0,
        bbox: None,
        png: bytes.to_vec(),
    };
    // The page prompt: an image is a whole page, so its reading has to carry
    // structure and not only text.
    let prompt = escalate::prompt_for_crop(&crop, &[]);
    let mut readings = Readings::default();
    if let Some(text) = reader.read_typed(&crop.png, mime, &prompt)? {
        readings.insert(crop.name, text);
    }
    Ok(readings)
}

/// Read the crops a document asks for: `jobs`, from
/// [`fluree_doc_pdf::escalate::plan`], with the `hints` they were chosen
/// with. `doc` is the document as analysed, whose pages give link anchors
/// their text.
///
/// Returns an empty set — not an error — when the document asks for nothing,
/// which is the common case and must stay silent.
#[allow(clippy::too_many_arguments)]
pub(crate) fn read_document(
    file: &Path,
    mut jobs: CropJobs,
    hints: &CropHints,
    doc: &Document,
    pdf: &hayro_syntax::Pdf,
    config: &Config,
    keep: Option<&[usize]>,
    verbose: bool,
) -> Result<Readings, String> {
    // `--pages` narrows what is *read*, not only what is printed. Paying for
    // a whole document to print one page of it is the kind of surprise a
    // metered API should never hand anyone.
    if let Some(keep) = keep {
        jobs.retain(|(page, _)| keep.contains(page));
    }
    if jobs.is_empty() {
        return Ok(Readings::default());
    }
    // Say what is about to be paid for, before the first call. A chart of
    // a thousand templated tables asked for a reading each and printed
    // nothing for six minutes.
    let planned = fluree_doc_pdf::escalate::crop_count(&jobs);
    eprintln!(
        "note: {}: {planned} crop(s) to read with {}",
        file.display(),
        config.model()
    );
    let max = config.escalation.max_crops;
    let jobs = if max > 0 && planned > max {
        let (kept, dropped) = fluree_doc_pdf::escalate::within_budget(doc, jobs, max);
        eprintln!(
            "note: {}: reading the {max} most valuable and leaving {dropped} to the deterministic pass — raise `escalation.max_crops` (0 for no limit) to read them all",
            file.display()
        );
        kept
    } else {
        jobs
    };
    let crops = escalate::render_crops(pdf, &jobs);
    if crops.is_empty() {
        return Ok(Readings::default());
    }
    let gemini = &config.escalation.gemini;
    let credentials = gemini
        .credentials
        .as_deref()
        .ok_or("no credentials are set for gemini")?;
    let reader = gemini::Reader::open(credentials, gemini.project.as_deref(), config.model())?;

    // The addresses the file states, so the reader is told rather than left
    // to invent one.
    let links = fluree_doc_pdf::link::extract(pdf);

    if verbose {
        eprintln!(
            "{}: escalating {} crop(s) to {}",
            file.display(),
            crops.len(),
            config.model()
        );
    }

    let workers = config.escalation.concurrency.clamp(1, 32).min(crops.len());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());
    let failures: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(crop) = crops.get(i) else { break };
                let anchors = escalate::links_in(&links, crop, doc);
                let prompt = escalate::prompt_for_crop_with(crop, hints, &anchors);
                match reader.read(&crop.png, &prompt) {
                    Ok(Some(text)) => out.lock().unwrap().push((crop.name.clone(), text)),
                    // A crop with nothing printed on it is a real answer.
                    Ok(None) => {}
                    Err(e) => failures.lock().unwrap().push(format!("{}: {e}", crop.name)),
                }
            });
        }
    });

    let failures = failures.into_inner().map_err(|_| "worker panicked")?;
    if !failures.is_empty() {
        // A partial reading is worse than none: the crops that answered would
        // be spliced and the crops that failed would silently keep their
        // deterministic reading, with nothing in the output saying which was
        // which.
        return Err(format!(
            "{} of {} crop(s) could not be read — {}",
            failures.len(),
            crops.len(),
            failures.join("; ")
        ));
    }
    Ok(Readings::from_map(
        out.into_inner()
            .map_err(|_| "worker panicked")?
            .into_iter()
            .collect(),
    ))
}
