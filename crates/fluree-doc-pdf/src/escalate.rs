//! The escalation loop as a library: which crops a document asks for, the
//! pixels to send, the words to send with them, and the container a
//! reading comes back in.
//!
//! `fdoc convert --escalate` runs this loop with a configured Gemini
//! reader; an embedded consumer (a worker Lambda routing readings through
//! its own model proxy) runs the same loop with its own reader. What this
//! module owns is everything EXCEPT the reader call, so the two cannot
//! drift: the crop set, the render scale, and the prompts are the ones the
//! published benchmark scores describe.
//!
//! Differences from the CLI's private `escalate` module, deliberately:
//! layout-detector sidecars (`FDOC_TITLE_BOXES`) are a file-based
//! refinement an embedded consumer does not have, so [`crops_for`] takes
//! its table evidence from the analysis alone and a region crop is never
//! told "this holds a table" by a detector. Everything else is the same
//! code path.
//!
//! ```no_run
//! # use fluree_doc_pdf::{document, escalate, extract_bytes, outline};
//! # let bytes: Vec<u8> = vec![];
//! let pdf = hayro_syntax::Pdf::new(std::sync::Arc::new(bytes.clone())).unwrap();
//! let ol = outline::extract(&pdf);
//! let mut doc = extract_bytes(bytes).unwrap();
//! let opts = document::AnalyzeOptions { emit_anchors: true, ..Default::default() };
//! let mut analysis = document::analyze_with(&mut doc, &ol, &opts);
//!
//! let jobs = escalate::crops_for(&doc, &analysis, false);
//! let crops = escalate::render_crops(&pdf, &jobs);
//! let links = fluree_doc_pdf::link::extract(&pdf);
//! let mut readings = escalate::Readings::default();
//! for crop in &crops {
//!     let hints = escalate::links_in(&links, crop, &doc);
//!     let prompt = escalate::prompt_for_crop(crop, &hints);
//!     // let text = your_reader(&crop.png, &prompt)?;  // any VLM
//!     # let text = Some(String::new());
//!     if let Some(text) = text {
//!         readings.insert(crop.name.clone(), text);
//!     }
//! }
//! if !readings.is_empty() {
//!     let page_text: Vec<Vec<String>> = doc
//!         .pages
//!         .iter()
//!         .map(|p| fluree_doc_pdf::fidelity::page_lines(&p.glyphs))
//!         .collect();
//!     fluree_doc_pdf::arbiter::splice_with_page(
//!         &mut analysis.elements, "doc", &readings, None, &page_text);
//!     fluree_doc_pdf::arbiter::scrub_furniture(
//!         &mut analysis.elements, &analysis.furniture);
//! }
//! ```

use crate::arbiter::{Block, TierBackend};
use crate::document::Analysis;
use crate::extract::Document;
use crate::geom::BBox;
use hayro::vello_cpu::color::{AlphaColor, Srgb};
use hayro::{render, RenderCache, RenderSettings};
use hayro_syntax::Pdf;
use std::collections::HashMap;

/// Crop oversampling. Measured against the deep reader: 1×, 1.5× and 2×
/// all bill the same input tokens because the API resizes below a
/// threshold, and 3× costs two and a half times as many and reads *worse*.
pub const VLM_RENDER_SCALE: f32 = 2.0;

/// Margin, in PDF units, drawn around a region crop so the model sees a
/// clean edge rather than letters sliced mid-stroke.
pub const CROP_MARGIN: f64 = 6.0;

/// A page's render jobs: `None` = the whole page, else named crops.
pub type CropJobs = Vec<(usize, Option<Vec<(String, BBox)>>)>;

/// One rendered crop, named the way the splice will ask for it.
pub struct Crop {
    /// `p{page}_{tag}` — the crop name a reading is filed under.
    pub name: String,
    pub page: usize,
    /// The region on the page, or `None` for a whole page.
    pub bbox: Option<BBox>,
    pub png: Vec<u8>,
}

impl Crop {
    /// Whole pages carry a document's structure and are prompted differently.
    pub fn is_page(&self) -> bool {
        self.name.ends_with("_full")
    }

    /// Tables are transcribed as markup rather than prose.
    pub fn is_table(&self) -> bool {
        self.name
            .rsplit_once('_')
            .is_some_and(|(_, tag)| tag.starts_with('t'))
    }
}

/// Readings held in memory, keyed by crop name — the same contract
/// `FixtureBackend` serves from a directory, for a consumer that read the
/// crops itself rather than through sidecar files.
#[derive(Default)]
pub struct Readings {
    by_crop: HashMap<String, String>,
}

impl Readings {
    pub fn insert(&mut self, crop: String, content: String) {
        self.by_crop.insert(crop, content);
    }

    pub fn is_empty(&self) -> bool {
        self.by_crop.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_crop.len()
    }

    /// Serialise for replay: a later pass (or an audit) re-splices the
    /// exact readings this run paid for instead of calling a model again.
    pub fn to_map(&self) -> &HashMap<String, String> {
        &self.by_crop
    }

    pub fn from_map(by_crop: HashMap<String, String>) -> Self {
        Self { by_crop }
    }
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

/// Every crop this document asks for, in page order.
///
/// Empty when it is read deterministically end to end, which is the common
/// case: over the evaluation corpus 113 of 200 documents ask for nothing.
///
/// `analysis` MUST come from [`crate::document::analyze_with`] with
/// `emit_anchors: true` — the table anchors this scans for are only
/// emitted then, and the splice later replaces those same anchors.
pub fn crops_for(doc: &Document, analysis: &Analysis, on_column_doubt: bool) -> CropJobs {
    let mut jobs: CropJobs = Vec::new();
    for p in &doc.pages {
        match crate::route::decide(p).0 {
            crate::route::Route::Vlm(_) => jobs.push((p.index, None)),
            crate::route::Route::VlmRegions(r) => jobs.push((
                p.index,
                Some(
                    r.iter()
                        .enumerate()
                        .map(|(i, b)| (format!("r{i}"), *b))
                        .collect(),
                ),
            )),
            crate::route::Route::Deterministic => {}
        }
    }

    // A hierarchy resting on nothing but font size is a whole-page
    // problem, not a region one — the escalation is the whole page, and it
    // supersedes any region or table crop on it. See the CLI's
    // `escalate::jobs` for the measurements behind this and the choices
    // below.
    for d in &analysis.suspect_headings {
        match jobs.iter_mut().find(|(pi, _)| *pi == d.page) {
            Some((_, slot)) => *slot = None,
            None => jobs.push((d.page, None)),
        }
    }

    // Column doubt escalates only when asked for: whether it helps depends
    // on the corpus, and nothing on the page says which kind you have.
    if on_column_doubt {
        for p in &doc.pages {
            if crate::column::doubt(&p.glyphs).is_some() {
                match jobs.iter_mut().find(|(pi, _)| *pi == p.index) {
                    Some((_, slot)) => *slot = None,
                    None => jobs.push((p.index, None)),
                }
            }
        }
    }

    for e in &analysis.elements {
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
                                  // anchor kept only a hairline.
        if let Some(tb) = analysis.elements.iter().find(|x| {
            x.kind == "doco:Table" && x.page == e.page && (x.rect().y0 - e.rect().y0).abs() < 1.0
        }) {
            bbox = tb.rect();
        }
        match jobs.iter_mut().find(|(pi, _)| *pi == e.page) {
            Some((_, Some(list))) => list.push((t.to_string(), bbox)),
            Some((_, None)) => {} // whole page already renders
            None => jobs.push((e.page, Some(vec![(t.to_string(), bbox)]))),
        }
    }
    jobs
}

/// Render every job to PNG bytes, in page order.
pub fn render_crops(pdf: &Pdf, jobs: &CropJobs) -> Vec<Crop> {
    let pages = pdf.pages();
    let cache = RenderCache::new();
    let white: AlphaColor<Srgb> = AlphaColor::new([1.0, 1.0, 1.0, 1.0]);
    let settings = RenderSettings {
        x_scale: VLM_RENDER_SCALE,
        y_scale: VLM_RENDER_SCALE,
        bg_color: white,
        ..Default::default()
    };
    let mut out = Vec::new();
    for (page_idx, regions) in jobs {
        let Some(page) = pages.get(*page_idx) else {
            continue;
        };
        let pix = render(
            page,
            &cache,
            &hayro::hayro_interpret::InterpreterSettings::default(),
            &settings,
        );
        let (w, h) = (pix.width() as usize, pix.height() as usize);
        let rgba: Vec<u8> = pix
            .take_unpremultiplied()
            .iter()
            .flat_map(|p| [p.r, p.g, p.b, p.a])
            .collect();
        match regions {
            None => {
                if let Some(png) = encode(&rgba, w, 0, 0, w, h) {
                    out.push(Crop {
                        name: format!("p{page_idx}_full"),
                        page: *page_idx,
                        bbox: None,
                        png,
                    });
                }
            }
            Some(regions) => {
                for (tag, b) in regions {
                    let sc = VLM_RENDER_SCALE as f64;
                    let x0 = (((b.x0 - CROP_MARGIN) * sc).floor().max(0.0)) as usize;
                    let y0 = (((b.y0 - CROP_MARGIN) * sc).floor().max(0.0)) as usize;
                    let x1 = ((((b.x1 + CROP_MARGIN) * sc).ceil()) as usize).min(w);
                    let y1 = ((((b.y1 + CROP_MARGIN) * sc).ceil()) as usize).min(h);
                    if x1 <= x0 || y1 <= y0 {
                        continue;
                    }
                    if let Some(png) = encode(&rgba, w, x0, y0, x1, y1) {
                        out.push(Crop {
                            name: format!("p{page_idx}_{tag}"),
                            page: *page_idx,
                            bbox: Some(*b),
                            png,
                        });
                    }
                }
            }
        }
    }
    out
}

/// Encode a sub-rectangle of an RGBA buffer as a PNG.
fn encode(
    rgba: &[u8],
    width: usize,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) -> Option<Vec<u8>> {
    let (cw, ch) = (x1.checked_sub(x0)?, y1.checked_sub(y0)?);
    if cw == 0 || ch == 0 {
        return None;
    }
    let mut buf = Vec::with_capacity(cw * ch * 4);
    for row in y0..y1 {
        let s = (row * width + x0) * 4;
        buf.extend_from_slice(rgba.get(s..s + cw * 4)?);
    }
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, cw as u32, ch as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().ok()?.write_image_data(&buf).ok()?;
    }
    Some(png)
}

// ---- prompts -------------------------------------------------------------
//
// Every rule below was put in by a measurement; see the CLI's
// `escalate::prompt` for the full accounting. The same text drives
// `eval/llm-tier/run_tier.py` — if one changes and the other does not, the
// committed scores stop describing this code.

/// A table crop: markup, because a grid is the whole content.
const TABLE: &str = "This image is one table cropped from a document page.

Transcribe it as a single HTML table.

Requirements:
- Every row and every column that is printed, in the order printed.
- Transcribe values exactly as printed, including currency symbols, commas,
  decimals, percent signs, and parentheses for negatives.
- NEVER infer, compute, complete or correct a value. If a cell is blank in
  the image, emit an empty cell. If a value is unreadable, emit an empty cell.
- Merged cells: use rowspan / colspan.
- Use <th> for header cells, <td> for data cells.

Respond with the table markup only, starting with <table> and nothing else.";

/// A routed region is whatever the text layer could not read; the router
/// does not know what it is, so the prompt must not assume.
const REGION: &str = "Transcribe what is printed in this image, exactly as printed,
in the order printed: top to bottom, then left to right.

Give each thing in the image the form that fits it.

- A TABLE -- values arranged in rows and columns -- becomes HTML:
  <table><tr><td>...</td></tr></table>, one <tr> per printed row and one <td>
  per printed column. It is still a table when it has no ruling lines, when
  it runs to many rows, and when it repeats its headers side by side to form
  a second pair of columns.
- A chart, plot or diagram is NOT a table. Give its text only, one item per
  line: its title, its legend, every axis tick label, and both axis titles.
- A caption or note printed beside the table or chart becomes a plain text
  line, in the position it is printed.

In every case:
- This image is a crop of a larger page. Skip any line that runs off its left
  or right edge, and any line the top or bottom edge cuts through so that the
  letters are only part-height. That text belongs to something outside the
  crop and is transcribed elsewhere; transcribing it here duplicates it.
- Copy text exactly, including punctuation and decimal marks as printed.
- NEVER invent a value. Do not read a number off a bar's height or a wedge's
  angle: if it is not printed as text, it is not there.
- NEVER invent a link. Text may be coloured or underlined to show it is one,
  but the address is not printed and you cannot see it. Write the text alone.
- If nothing is printed in the image, return nothing at all.
- Do not describe the image and do not add commentary.";

/// A whole page is not a big region: a page *is* the shape, so its reading
/// has to carry headings and reading order.
const FULL: &str = "Transcribe this page exactly as printed, as Markdown.

Reading order follows the page's own layout. Where the page is laid out in
columns or panels, read each column to its end before starting the next; do
not read straight across the page.

Mark structure as the page marks it:

- A heading -- a line set apart by size, weight, colour, or its own banner --
  becomes a Markdown heading, `#` for the most prominent rank and `##`, `###`
  below it.
- A bulleted or numbered list becomes Markdown list items.
- A TABLE -- values arranged in rows and columns -- becomes HTML:
  <table><tr><td>...</td></tr></table>, one <tr> per printed row and one <td>
  per printed column.
- A chart, plot or diagram is NOT a table. Give its text only, one item per
  line: its title, its legend, every axis tick label, and both axis titles.
- Everything else is a paragraph.

In every case:
- Copy text exactly, including punctuation and dashes as printed.
- NEVER invent a value. Do not read a number off a bar's height or a wedge's
  angle: if it is not printed as text, it is not there.
- NEVER invent a link. Text may be coloured or underlined to show it is one,
  but the address is not printed and you cannot see it. Write the text alone.
- If nothing is printed on the page, return nothing at all.
- Do not describe the page and do not add commentary.";

/// A link's anchor is visible and its address is not — where the file
/// states the addresses, they are given, so the model does not invent one.
fn links_hint(listing: &str) -> String {
    format!(
        "This image contains links. Their addresses are not printed on the\n\
         page, so you cannot read them from the image; they are given here:\n\n\
         {listing}\n\n\
         Where you transcribe one of those texts, write it as a Markdown link:\n\
         [text](address), using the address exactly as given. Any other text,\n\
         however it is styled, is not a link.\n\n"
    )
}

/// The prompt for one crop. `links` are the anchors and targets the file
/// states inside the crop — see [`links_in`].
pub fn prompt_for_crop(crop: &Crop, links: &[(String, String)]) -> String {
    if crop.is_table() {
        // Table markup has no place to put a Markdown link.
        return TABLE.to_string();
    }
    let base = if crop.is_page() {
        FULL.to_string()
    } else {
        REGION.to_string()
    };
    if links.is_empty() {
        return base;
    }
    let listing = links
        .iter()
        .map(|(text, target)| format!("  \"{text}\" links to {target}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}{base}", links_hint(&listing))
}

/// The links inside a crop, one entry per target with wrapped fragments
/// rejoined — the listing [`prompt_for_crop`] wants.
pub fn links_in(links: &[crate::link::Link], crop: &Crop, doc: &Document) -> Vec<(String, String)> {
    let Some(page) = doc.pages.iter().find(|p| p.index == crop.page) else {
        return Vec::new();
    };
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for l in links.iter().filter(|l| l.page == crop.page) {
        if !crop.bbox.is_none_or(|b| overlaps(&b, &l.bbox)) {
            continue;
        }
        let text = crate::link::anchor_text(page, l.bbox);
        if text.is_empty() {
            continue;
        }
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

fn overlaps(a: &BBox, b: &BBox) -> bool {
    let ix = (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0);
    let iy = (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0);
    ix * iy > 0.0
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

    #[test]
    fn each_crop_kind_gets_its_own_prompt() {
        assert!(prompt_for_crop(&crop("p0_t0"), &[]).starts_with("This image is one table"));
        assert!(prompt_for_crop(&crop("p0_full"), &[]).starts_with("Transcribe this page"));
        assert!(prompt_for_crop(&crop("p0_r0"), &[]).starts_with("Transcribe what is printed"));
    }

    #[test]
    fn known_links_are_listed_and_tables_are_not_offered_them() {
        let links = vec![(
            "the filing".to_string(),
            "https://sec.example/x".to_string(),
        )];
        let p = prompt_for_crop(&crop("p0_full"), &links);
        assert!(p.contains("\"the filing\" links to https://sec.example/x"));
        assert!(!prompt_for_crop(&crop("p0_t0"), &links).contains("sec.example"));
    }

    #[test]
    fn readings_round_trip_and_label_tables() {
        let mut r = Readings::default();
        r.insert("p0_t0".into(), "<table><tr><td>a</td></tr></table>".into());
        r.insert("p3_full".into(), "# Title".into());
        assert_eq!(r.len(), 2);
        assert_eq!(r.read("doc", "p0_t0").unwrap()[0].label, "table");
        assert_eq!(r.read("doc", "p3_full").unwrap()[0].label, "text");
        assert!(r.read("doc", "p9_full").is_none());

        let replay = Readings::from_map(r.to_map().clone());
        assert_eq!(replay.read("doc", "p3_full").unwrap()[0].content, "# Title");
    }
}
