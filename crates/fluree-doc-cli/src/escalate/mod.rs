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

/// Read a bare image, or an image a document places in its flow: one crop,
/// which is the image itself, read as a whole page, its reading keyed
/// `crop` (`p0_full` for a bare image, `m<n>` for a placed one).
///
/// No rendering step — the bytes already are the page, and re-encoding them
/// would only lose information the reader could have used. No crop selection
/// either: an image has no regions to choose between, because there are no
/// glyphs to say where anything is.
pub(crate) fn read_image(
    file: &Path,
    crop: &str,
    bytes: &[u8],
    mime: &str,
    config: &Config,
    verbose: bool,
) -> Result<Readings, String> {
    let mut read = read_images(file, &[(crop, bytes, mime)], config, verbose)?;
    match read.failures.pop() {
        Some((_, e)) => Err(e),
        None => Ok(read.readings),
    }
}

/// What [`read_images`] gives, by crop.
#[derive(Default)]
pub(crate) struct ImagesRead {
    /// The text read in each image that has any.
    pub(crate) readings: Readings,
    /// The images read that hold no text: a real answer, and a reading.
    pub(crate) empty: Vec<String>,
    /// The images that could not be read, each with why.
    pub(crate) failures: Vec<(String, String)>,
}

/// Read the images a document places in its flow, each as [`read_image`]
/// reads one, with one reader for them all and up to
/// `escalation.concurrency` of them in flight, as a document's crops are
/// read: a message of many pictures pays for one sign-in, not one each, and
/// does not wait on each picture in turn. `images` are `(crop, bytes, mime)`.
///
/// Returns what came of each image: its reading, keyed by crop, that it
/// held no text, or why it could not be read. Fails as a whole only when
/// the reader cannot be opened.
pub(crate) fn read_images(
    file: &Path,
    images: &[(&str, &[u8], &str)],
    config: &Config,
    verbose: bool,
) -> Result<ImagesRead, String> {
    let mut out = ImagesRead::default();
    if images.is_empty() {
        return Ok(out);
    }
    let gemini = &config.escalation.gemini;
    let credentials = gemini
        .credentials
        .as_deref()
        .ok_or("no credentials are set for gemini")?;
    let reader = gemini::Reader::open(credentials, gemini.project.as_deref(), config.model())?;
    if verbose {
        eprintln!(
            "{}: reading {} image(s) as one page each with {}",
            file.display(),
            images.len(),
            config.model()
        );
    }
    // Prompted as the whole page it is, whatever its reading is keyed by:
    // an image is a whole page, so its reading has to carry structure and
    // not only text.
    let page = Crop {
        name: "p0_full".into(),
        page: 0,
        bbox: None,
        png: Vec::new(),
    };
    let prompt = escalate::prompt_for_crop(&page, &[]);
    let workers = config.escalation.concurrency.clamp(1, 32);
    let read = read_each(images, workers, |(_, bytes, mime)| {
        reader.read_typed(bytes, mime, &prompt)
    });
    for ((crop, _, _), got) in images.iter().zip(read) {
        match got {
            Ok(Some(text)) => out.readings.insert(crop.to_string(), text),
            // An image with nothing printed on it is a real answer.
            Ok(None) => out.empty.push(crop.to_string()),
            Err(e) => out.failures.push((crop.to_string(), e)),
        }
    }
    Ok(out)
}

/// What a reader answers for one image: its text, `None` for an image with
/// nothing printed on it, or why it could not be read.
type Answer = Result<Option<String>, String>;

/// `read` applied to each of `items` by up to `workers` threads, the
/// results in the order of `items`. A panic in `read` is not caught: the
/// scope raises it again once the threads are joined, as it would be raised
/// on one thread.
fn read_each<T: Sync>(
    items: &[T],
    workers: usize,
    read: impl Fn(&T) -> Answer + Sync,
) -> Vec<Answer> {
    let workers = workers.clamp(1, items.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: std::sync::Mutex<Vec<Option<Answer>>> = std::sync::Mutex::new(vec![None; items.len()]);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(item) = items.get(i) else { break };
                let got = read(item);
                out.lock().unwrap()[i] = Some(got);
            });
        }
    });
    // Past the scope, every worker returned, so every item has its answer.
    out.into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .into_iter()
        .map(|r| r.expect("each item is answered"))
        .collect()
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
    let crops = escalate::render_crops(pdf, doc, &jobs);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_read_side_by_side_and_answered_in_order() {
        // Eight images, four workers, each read taking a while: more than
        // one is in flight at once, and each answer stays with its image.
        let images: Vec<usize> = (0..8).collect();
        let in_flight = std::sync::atomic::AtomicUsize::new(0);
        let most = std::sync::atomic::AtomicUsize::new(0);
        let got = read_each(&images, 4, |&i| {
            use std::sync::atomic::Ordering::SeqCst;
            let now = in_flight.fetch_add(1, SeqCst) + 1;
            most.fetch_max(now, SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(30));
            in_flight.fetch_sub(1, SeqCst);
            match i {
                3 => Err("refused".into()),
                5 => Ok(None),
                _ => Ok(Some(format!("image {i}"))),
            }
        });
        assert!(most.into_inner() > 1);
        assert_eq!(got.len(), 8);
        assert_eq!(got[0], Ok(Some("image 0".into())));
        assert_eq!(got[3], Err("refused".into()));
        assert_eq!(got[5], Ok(None));
        assert_eq!(got[7], Ok(Some("image 7".into())));
    }
}
