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
use fluree_doc_pdf::escalate::{self, Crop, CropHints, CropJobs, PageReasons, Readings};
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

/// The crops of `jobs` a run reads: those on the pages `keep` names, and of
/// those at most `escalation.max_crops`, the most valuable first
/// ([`fluree_doc_pdf::escalate::within_budget`]). Says on stderr how many
/// are about to be paid for, and how many the ceiling left out.
///
/// `reasons` are those the selection that chose `jobs` gave its whole
/// pages ([`fluree_doc_pdf::escalate::crops_at_anchors_explained`]), by
/// which the budget ranks them.
pub(crate) fn to_read(
    file: &Path,
    mut jobs: CropJobs,
    reasons: &PageReasons,
    config: &Config,
    keep: Option<&[usize]>,
) -> CropJobs {
    // `--pages` narrows what is *read*, not only what is printed. Paying for
    // a whole document to print one page of it is the kind of surprise a
    // metered API should never hand anyone.
    if let Some(keep) = keep {
        jobs.retain(|(page, _)| keep.contains(page));
    }
    if jobs.is_empty() {
        return jobs;
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
    if max > 0 && planned > max {
        let (kept, dropped) = fluree_doc_pdf::escalate::within_budget(jobs, reasons, max);
        eprintln!(
            "note: {}: reading the {max} most valuable and leaving {dropped} to the deterministic pass — raise `escalation.max_crops` (0 for no limit) to read them all",
            file.display()
        );
        kept
    } else {
        jobs
    }
}

/// Read the crops a document asks for: `jobs`, from
/// [`fluree_doc_pdf::escalate::crops_at_anchors`] narrowed by [`to_read`],
/// with the `hints` they were chosen with. `doc` is the document as
/// analysed, whose pages give link anchors their text.
///
/// Returns an empty set — not an error — when the document asks for nothing,
/// which is the common case and must stay silent.
pub(crate) fn read_document(
    file: &Path,
    jobs: CropJobs,
    hints: &CropHints,
    doc: &Document,
    pdf: &hayro_syntax::Pdf,
    config: &Config,
    verbose: bool,
) -> Result<Readings, String> {
    if jobs.is_empty() {
        return Ok(Readings::default());
    }
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
    read_crops(
        &crops,
        workers,
        |crop| {
            let anchors = escalate::links_in(&links, crop, doc);
            escalate::prompt_for_crop_with(crop, hints, &anchors)
        },
        |crop, prompt| reader.read(&crop.png, prompt),
    )
}

/// Read every crop with `read`, prompted by `prompt`, on `workers` threads,
/// and file each answer under its crop's name.
///
/// Apart from [`read_document`], which hands it the configured model, so
/// what is filed can be checked with a stand-in reader and no network.
fn read_crops(
    crops: &[Crop],
    workers: usize,
    prompt: impl Fn(&Crop) -> String + Sync,
    read: impl Fn(&Crop, &str) -> Result<Option<String>, String> + Sync,
) -> Result<Readings, String> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());
    let failures: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(crop) = crops.get(i) else { break };
                match read(crop, &prompt(crop)) {
                    Ok(text) => out.lock().unwrap().push(answer(&crop.name, text)),
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

/// The reading a crop is filed under: what the reader read, or an empty
/// reading where it found nothing printed.
///
/// A crop with nothing printed on it is a real answer, and the splice must
/// see it as one. Left out, the crop would look unread: with
/// `--emit-anchors` its anchor would stay in the output, and whoever fills
/// anchors downstream would pay for the same reading again. An empty
/// reading answers the anchor with no text, so it drops.
fn answer(crop: &str, text: Option<String>) -> (String, String) {
    (crop.to_string(), text.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crop(name: &str) -> Crop {
        Crop {
            name: name.into(),
            page: 0,
            bbox: None,
            png: Vec::new(),
        }
    }

    /// What `read_document` files for each crop its reader answers: the
    /// reading, or an empty one where the reader found nothing printed.
    #[test]
    fn a_crop_the_reader_found_nothing_on_is_answered() {
        let crops = [crop("p0_r0"), crop("p0_r1")];
        let readings = read_crops(
            &crops,
            2,
            |_| String::new(),
            |c, _| Ok((c.name == "p0_r1").then(|| "Revenue rose.".to_string())),
        )
        .expect("a stand-in reader does not fail");
        let filed = readings.to_map();
        assert_eq!(filed.get("p0_r0").map(String::as_str), Some(""));
        assert_eq!(
            filed.get("p0_r1").map(String::as_str),
            Some("Revenue rose.")
        );
    }

    /// One crop that fails fails the document, rather than leaving it
    /// partly read.
    #[test]
    fn a_crop_that_cannot_be_read_fails_the_document() {
        let crops = [crop("p0_r0"), crop("p0_r1")];
        let read = read_crops(
            &crops,
            1,
            |_| String::new(),
            |c, _| match c.name.as_str() {
                "p0_r1" => Err("quota".into()),
                _ => Ok(None),
            },
        );
        assert!(read.is_err_and(|e| e.contains("p0_r1: quota")));
    }
}
