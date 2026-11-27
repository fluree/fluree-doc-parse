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
    let load = &mut |_| Ok(std::borrow::Cow::Borrowed(bytes));
    let mut read = read_images(file, &[(crop, mime)], load, config, verbose)?;
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
/// does not wait on each picture in turn. `images` are `(crop, mime)`;
/// `load` reads the bytes of the `i`-th, as it is about to be sent
/// ([`in_batches`]), so no more of them are held at once than one batch.
///
/// Returns what came of each image: its reading, keyed by crop, that it
/// held no text, or why it could not be read (its bytes among them). Fails
/// as a whole only when the reader cannot be opened, before any image's
/// bytes are read.
pub(crate) fn read_images<'b>(
    file: &Path,
    images: &[(&str, &str)],
    load: &mut dyn FnMut(usize) -> Result<std::borrow::Cow<'b, [u8]>, String>,
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
    let read = in_batches(images.len(), workers, HELD_BYTES, load, |batch| {
        read_each(batch, workers, |(i, bytes)| {
            reader.read_typed(bytes.as_ref(), images[*i].1, &prompt)
        })
    });
    for ((crop, _), got) in images.iter().zip(read) {
        match got {
            Ok(Some(text)) => out.readings.insert(crop.to_string(), text),
            // An image with nothing printed on it is a real answer.
            Ok(None) => out.empty.push(crop.to_string()),
            Err(e) => out.failures.push((crop.to_string(), e)),
        }
    }
    Ok(out)
}

/// The bytes of the images [`read_images`] holds to send at once, past the
/// first of a batch: 64 MiB, the most one image part is read to
/// (`MAX_MEDIA_BYTES`). With the image loaded last, a batch never holds more
/// than twice that, however many images a document sends: a DOCX of two
/// hundred parts of 64 MiB each, a few megabytes zipped, would otherwise be
/// held whole, some 12.8 GiB, until its last image was read.
const HELD_BYTES: usize = 64 << 20;

/// `read` applied to the `n` items `load` gives, in batches: each of at
/// most `workers` items, loaded one after the other until the batch holds
/// `held` bytes or more, read, and dropped before the next batch is loaded.
/// An item that cannot be loaded is answered with why. The answers in the
/// order of the items.
fn in_batches<B: AsRef<[u8]>>(
    n: usize,
    workers: usize,
    held: usize,
    load: &mut dyn FnMut(usize) -> Result<B, String>,
    mut read: impl FnMut(&[(usize, B)]) -> Vec<Answer>,
) -> Vec<Answer> {
    let mut answers: Vec<Option<Answer>> = (0..n).map(|_| None).collect();
    let mut next = 0;
    while next < n {
        let mut batch: Vec<(usize, B)> = Vec::new();
        let mut bytes = 0usize;
        while next < n && batch.len() < workers.max(1) && (batch.is_empty() || bytes < held) {
            match load(next) {
                Ok(b) => {
                    bytes += b.as_ref().len();
                    batch.push((next, b));
                }
                Err(e) => answers[next] = Some(Err(e)),
            }
            next += 1;
        }
        for ((i, _), got) in batch.iter().zip(read(&batch)) {
            answers[*i] = Some(got);
        }
    }
    answers
        .into_iter()
        .map(|a| a.expect("each item is answered"))
        .collect()
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

    #[test]
    fn images_sent_are_held_a_batch_at_a_time() {
        // Two hundred images of 64 units each, sent four at a time with 100
        // units held past the first: each is loaded only as its batch is
        // read, so what is held at once stays under twice the bound, not
        // the 12,800 units of the whole document. One that cannot be
        // loaded is answered with why, in its place.
        let loaded = std::cell::Cell::new(0usize);
        let mut load = |i: usize| -> Result<Vec<u8>, String> {
            if i == 7 {
                return Err("gone".into());
            }
            loaded.set(loaded.get() + 1);
            Ok(vec![0; 64])
        };
        let mut most = 0usize;
        let mut batches = 0usize;
        let answers = in_batches(200, 4, 100, &mut load, |batch| {
            batches += 1;
            let held: usize = batch.iter().map(|(_, b)| b.len()).sum();
            most = most.max(held);
            // Nothing past this batch has been loaded yet.
            let last = batch.last().map_or(0, |(i, _)| *i);
            assert!(loaded.get() <= last + 1);
            batch.iter().map(|(i, _)| Ok(Some(i.to_string()))).collect()
        });
        assert_eq!(answers.len(), 200);
        assert!(most <= 128, "{most}");
        assert!(batches >= 100, "{batches}");
        assert_eq!(answers[7], Err("gone".into()));
        assert_eq!(answers[8], Ok(Some("8".into())));
        assert_eq!(answers[199], Ok(Some("199".into())));
    }

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
