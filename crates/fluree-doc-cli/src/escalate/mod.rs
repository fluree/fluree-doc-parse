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
pub(crate) mod prompt;
pub(crate) mod render;

use crate::config::Config;
use fluree_doc_pdf::arbiter::{Block, TierBackend};
use fluree_doc_pdf::Document;
use std::collections::HashMap;
use std::path::Path;

/// Readings held in memory, keyed by crop name — the same contract
/// `FixtureBackend` serves from a directory.
#[derive(Default)]
pub(crate) struct Readings {
    by_crop: HashMap<String, String>,
}

impl TierBackend for Readings {
    fn read(&self, _stem: &str, crop: &str) -> Option<Vec<Block>> {
        let content = self.by_crop.get(crop)?;
        Some(vec![Block {
            label: if content.trim_start().starts_with("<table") {
                "table".into()
            } else {
                "text".into()
            },
            content: content.clone(),
        }])
    }
}

impl Readings {
    pub fn is_empty(&self) -> bool {
        self.by_crop.is_empty()
    }
}

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
    let crop = render::Crop {
        name: "p0_full".into(),
        page: 0,
        bbox: None,
        png: bytes.to_vec(),
    };
    // The page prompt: an image is a whole page, so its reading has to carry
    // structure and not only text.
    let prompt = prompt::for_crop(&crop, false, &[]);
    match reader.read_typed(&crop.png, mime, &prompt)? {
        Some(text) => Ok(Readings {
            by_crop: [("p0_full".to_string(), text)].into_iter().collect(),
        }),
        None => Ok(Readings::default()),
    }
}

/// Read every crop this document asks for.
///
/// Returns an empty set — not an error — when the document asks for nothing,
/// which is the common case and must stay silent.
pub(crate) fn read_document(
    file: &Path,
    bytes: &[u8],
    doc: &Document,
    pdf: &hayro_syntax::Pdf,
    config: &Config,
    keep: Option<&[usize]>,
    verbose: bool,
) -> Result<Readings, String> {
    let mut jobs = jobs::crops_for(file, bytes, doc, config.escalation.on_column_doubt);
    // `--pages` narrows what is *read*, not only what is printed. Paying for
    // a whole document to print one page of it is the kind of surprise a
    // metered API should never hand anyone.
    if let Some(keep) = keep {
        jobs.retain(|(page, _)| keep.contains(page));
    }
    if jobs.is_empty() {
        return Ok(Readings::default());
    }
    let crops = render::render_crops(pdf, &jobs);
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

    let boxed = table_boxes(file, doc);
    let workers = config.escalation.concurrency.clamp(1, 32).min(crops.len());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());
    let failures: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(crop) = crops.get(i) else { break };
                let hints = links_in(&links, crop, doc);
                let table_boxed = boxed.get(&crop.page).is_some_and(|bs| {
                    crop.bbox
                        .is_some_and(|b| bs.iter().any(|t| overlaps(&b, t)))
                });
                let prompt = prompt::for_crop(crop, table_boxed, &hints);
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
    Ok(Readings {
        by_crop: out
            .into_inner()
            .map_err(|_| "worker panicked")?
            .into_iter()
            .collect(),
    })
}

/// Where the layout detector boxed a table on each page, so a region crop can
/// be told what it holds.
fn table_boxes(file: &Path, doc: &Document) -> HashMap<usize, Vec<fluree_doc_pdf::geom::BBox>> {
    let stem = file.file_stem().and_then(|x| x.to_str()).unwrap_or("doc");
    doc.pages
        .iter()
        .map(|p| (p.index, crate::commands::dev::layout_tables(stem, p.index)))
        .filter(|(_, b)| !b.is_empty())
        .collect()
}

fn overlaps(a: &fluree_doc_pdf::geom::BBox, b: &fluree_doc_pdf::geom::BBox) -> bool {
    let ix = (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0);
    let iy = (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0);
    ix * iy > 0.0
}

/// The links inside a crop, one entry per target with wrapped fragments
/// rejoined.
fn links_in(
    links: &[fluree_doc_pdf::link::Link],
    crop: &render::Crop,
    doc: &Document,
) -> Vec<(String, String)> {
    let Some(page) = doc.pages.iter().find(|p| p.index == crop.page) else {
        return Vec::new();
    };
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for l in links.iter().filter(|l| l.page == crop.page) {
        if !crop.bbox.is_none_or(|b| overlaps(&b, &l.bbox)) {
            continue;
        }
        let text = fluree_doc_pdf::link::anchor_text(page, l.bbox);
        if text.is_empty() {
            continue;
        }
        // An internal jump has no address a Markdown reader can follow. It is
        // still worth naming: it stops the model inventing one for an anchor
        // that never had an address.
        let target = match &l.target {
            fluree_doc_model::Target::Uri { uri } => uri.clone(),
            fluree_doc_model::Target::Page { page } => format!(
                "page {} of this document -- write the text alone, with no link",
                page + 1
            ),
        };
        match grouped.iter_mut().find(|(t, _)| *t == target) {
            Some((_, fragments)) => fragments.push(text),
            None => grouped.push((target, vec![text])),
        }
    }
    grouped
        .into_iter()
        .map(|(target, fragments)| {
            // A wrapped address breaks mid-token and rejoins with nothing;
            // wrapped prose rejoins with a space. The target says which.
            let tight = fragments.concat();
            let text = if fragments.len() > 1 && target.contains(tight.trim()) {
                tight
            } else {
                fragments.join(" ")
            };
            (text, target)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_is_served_under_its_crop_name() {
        let r = Readings {
            by_crop: [("p3_full".to_string(), "# Title".to_string())]
                .into_iter()
                .collect(),
        };
        let blocks = r.read("doc", "p3_full").expect("reading");
        assert_eq!(blocks[0].content, "# Title");
        assert_eq!(blocks[0].label, "text");
        assert!(r.read("doc", "p0_full").is_none());
    }

    #[test]
    fn markup_is_labelled_a_table_so_the_arbiter_treats_it_as_one() {
        let r = Readings {
            by_crop: [(
                "p0_t0".to_string(),
                "<table><tr><td>a</td></tr></table>".to_string(),
            )]
            .into_iter()
            .collect(),
        };
        assert_eq!(r.read("doc", "p0_t0").unwrap()[0].label, "table");
    }
}
