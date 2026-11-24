//! The built-in OCR engine: PP-OCRv6 small through oar-ocr.
//!
//! oar-ocr runs its models on ONNX Runtime, a native library. It is not
//! linked into this crate: [`OarEngine::open`] loads it from the assets
//! directory ([`crate::assets`]), after checking its hash, so a build carries
//! the engine's code (5.8 MB in a release build of `fdoc`) and a deployment
//! ships the library and the models for its platform beside the binary.
//!
//! The detector's settings are the ones the corpus benchmark measured
//! (detection sized on the image's short side at 736 px, at most 2,000 px on
//! the long one): each word box this engine gives is a detected line, turned
//! upright about its center, with the line's angle. An image the detector
//! shrinks until its text is too small to be seen ([`text_too_small`]) is
//! read again in overlapping strips ([`strips`]), and its lines kept once.

use crate::assets::{Assets, Runtime};
use crate::{Coords, Layout, OcrEngine, OcrWord, Pixels, WordReading};
use oar_ocr::core::config::OrtSessionConfig;
use oar_ocr::domain::tasks::TextDetectionConfig;
use oar_ocr::oarocr::{OAROCRBuilder, OAROCR};
use oar_ocr::processors::LimitType;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

/// The name each reading records.
pub const ENGINE: &str = "pp-ocrv6-small/oar-ocr";

/// How the engine runs.
#[derive(Debug, Clone)]
pub struct OarOptions {
    /// ONNX Runtime threads per model run. One by default: a caller reading
    /// many images at once reads each on its own thread, and threads inside
    /// each run would fight over the same cores.
    pub threads: usize,
    /// Most images read at once, each by a pipeline of its own; a thread
    /// that finds them all busy waits for one. A pipeline holds about
    /// 0.6 to 1 GB once it has read pages, so this bounds the engine's memory
    /// whatever the caller's parallelism. By default the cores, at most
    /// four: measured on twelve cores, eight read no faster than four.
    pub pipelines: usize,
}

impl Default for OarOptions {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        OarOptions {
            threads: 1,
            pipelines: cores.min(4),
        }
    }
}

/// PP-OCRv6 small, opened once and shared across threads.
///
/// oar-ocr runs each model of a pipeline under a lock, so one pipeline reads
/// one image at a time. The engine keeps a pipeline per image being read at
/// once: one when it opens, and another each time a thread finds them all
/// busy, built from the same checked models and kept for the next reading,
/// up to [`OarOptions::pipelines`].
pub struct OarEngine {
    det: Vec<u8>,
    rec: Vec<u8>,
    dict: String,
    threads: usize,
    pipes: Pipelines<OAROCR>,
}

/// Up to `most` pipelines, built as readers need them and shared between
/// them: a reader takes an idle one, builds another while there may be
/// more, or waits for one to be given back.
struct Pipelines<T> {
    most: usize,
    pool: Mutex<Pool<T>>,
    freed: Condvar,
}

/// The pipelines not reading, and how many there are in all.
struct Pool<T> {
    idle: Vec<T>,
    built: usize,
}

/// A pipeline taken from the pool. Dropped, it goes back for the next
/// reading; dropped while its thread panics, or never built, it is counted
/// out instead (a pipeline a panic went through is not trusted again), so
/// the readers waiting for one are not left waiting for good.
struct Lease<'a, T> {
    pipes: &'a Pipelines<T>,
    item: Option<T>,
}

impl<T> std::ops::Deref for Lease<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.item.as_ref().expect("a lease holds its pipeline")
    }
}

impl<T> Drop for Lease<'_, T> {
    fn drop(&mut self) {
        let mut pool = self.pipes.pool.lock().unwrap_or_else(|e| e.into_inner());
        match self.item.take() {
            Some(item) if !std::thread::panicking() => pool.idle.push(item),
            _ => pool.built -= 1,
        }
        drop(pool);
        self.pipes.freed.notify_one();
    }
}

impl<T> Pipelines<T> {
    fn new(most: usize) -> Self {
        Pipelines {
            most: most.max(1),
            pool: Mutex::new(Pool {
                idle: Vec::new(),
                built: 0,
            }),
            freed: Condvar::new(),
        }
    }

    fn built(&self) -> usize {
        self.pool.lock().unwrap_or_else(|e| e.into_inner()).built
    }

    /// An idle pipeline; one `build` makes when every one is reading and
    /// there may be more; else the next one given back.
    fn take(&self, build: impl FnOnce() -> Result<T, String>) -> Result<Lease<'_, T>, String> {
        let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(item) = pool.idle.pop() {
                return Ok(Lease {
                    pipes: self,
                    item: Some(item),
                });
            }
            if pool.built < self.most {
                pool.built += 1;
                drop(pool);
                // Counted before it exists: whatever happens while it is
                // built, the lease gives the count back if it never is.
                let mut lease = Lease {
                    pipes: self,
                    item: None,
                };
                lease.item = Some(build()?);
                return Ok(lease);
            }
            pool = self.freed.wait(pool).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// The ONNX Runtime library loaded into this process, once one is: a process
/// holds one, so another may not be loaded beside it. A load that fails
/// leaves nothing loaded (`ort` loads the path it is given, and looks nowhere
/// else), and a later open may try again.
static RUNTIME: Mutex<Option<PathBuf>> = Mutex::new(None);

fn load_runtime(runtime: &Runtime) -> Result<(), String> {
    let mut state = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
    let path = &runtime.path;
    match &*state {
        Some(loaded) if loaded == path => Ok(()),
        Some(loaded) => Err(format!(
            "ONNX Runtime is already loaded from {}; it cannot be loaded again from {}",
            loaded.display(),
            path.display()
        )),
        None => {
            // Only the library is loaded here: oar-ocr builds the process's
            // one environment on it when it creates its first session, and
            // keeps it.
            ort::init_from(runtime.load_path()).map_err(|e| format!("{}: {e}", path.display()))?;
            *state = Some(path.clone());
            Ok(())
        }
    }
}

impl OarEngine {
    /// Check the assets in `dir` against the hashes this build pins, load
    /// ONNX Runtime from it and build a pipeline. Slow (the models are read
    /// and optimized), so open once and share.
    pub fn open(dir: &Path, options: &OarOptions) -> Result<OarEngine, String> {
        Self::open_assets(Assets::open(dir)?, options)
    }

    /// As [`OarEngine::open`], on assets already checked.
    pub fn open_assets(assets: Assets, options: &OarOptions) -> Result<OarEngine, String> {
        load_runtime(&assets.runtime)?;
        let engine = OarEngine {
            det: assets.det,
            rec: assets.rec,
            dict: assets.dict,
            threads: options.threads.max(1),
            pipes: Pipelines::new(options.pipelines),
        };
        drop(engine.pipes.take(|| engine.pipeline())?);
        Ok(engine)
    }

    /// How many pipelines the engine holds now, reading or not: what its
    /// memory grows with.
    pub fn pipelines(&self) -> usize {
        self.pipes.built()
    }

    /// A new pipeline on the engine's models.
    fn pipeline(&self) -> Result<OAROCR, String> {
        let det = TextDetectionConfig {
            score_threshold: 0.2,
            box_threshold: 0.45,
            unclip_ratio: 1.4,
            max_candidates: 3000,
            limit_side_len: Some(DET_SHORT_SIDE),
            limit_type: Some(LimitType::Min),
            max_side_len: Some(DET_LONG_SIDE),
        };
        // The models as the bytes that were checked, not read again by path.
        OAROCRBuilder::new(self.det.clone(), self.rec.clone(), PathBuf::new())
            .character_dict_content(self.dict.clone())
            .ort_session(
                OrtSessionConfig::new()
                    .with_intra_threads(self.threads)
                    .with_inter_threads(1)
                    // Planning allocations per input shape only grows the
                    // memory of a pipeline fed images of every size.
                    .with_memory_pattern(false),
            )
            .text_detection_config(det)
            .image_batch_size(1)
            .build()
            .map_err(|e| format!("building the OCR pipeline: {e}"))
    }
}

impl OcrEngine for OarEngine {
    fn name(&self) -> &str {
        ENGINE
    }

    fn read(&self, image: &Pixels) -> Result<WordReading, String> {
        // oar-ocr reads RGB: a grey image is spread to three channels, and
        // a crop's transparent pixels are composited over white paper.
        let rgb = || {
            image::RgbImage::from_raw(image.width, image.height, image.to_rgb())
                .ok_or("the image's buffer does not match its size")
        };
        let bands = strips(image.width, image.height);
        let lease = self.pipes.take(|| self.pipeline())?;
        let whole = read_one(&lease, rgb()?, 0)?;
        // Read whole first: only an image the detector shrinks until its
        // text is too small to be seen is read again, in strips, and that
        // reading kept.
        let words = if !bands.is_empty() && text_too_small(image.width, image.height, &whole) {
            let img = rgb()?;
            let mut read = Vec::new();
            for &(top, bottom) in &bands {
                let strip =
                    image::imageops::crop_imm(&img, 0, top, image.width, bottom - top).to_image();
                read.push((top, bottom, read_one(&lease, strip, top)?));
            }
            merge_strips(read, image.height)
        } else {
            whole.into_iter().map(|(w, _)| w).collect()
        };
        drop(lease);
        Ok(WordReading {
            coords: Coords::Px,
            width: Some(image.width as f64),
            height: Some(image.height as f64),
            engine: Some(ENGINE.into()),
            words,
        })
    }
}

/// The detector's sizing (see [`OarEngine::pipeline`]): an image whose short
/// side is under [`DET_SHORT_SIDE`] is scaled up to it, then one whose long
/// side is over [`DET_LONG_SIDE`] is scaled down to that.
const DET_SHORT_SIDE: u32 = 736;
const DET_LONG_SIDE: u32 = 2000;

/// Shortest strip worth reading on its own, in pixels: a narrower image is
/// read whole.
const MIN_STRIP: u32 = 200;

/// The scale the detector reads an image of `width` × `height` at (see
/// [`OarEngine::pipeline`]): up to [`DET_SHORT_SIDE`] on its short side,
/// then down to [`DET_LONG_SIDE`] on its long one.
fn det_scale(width: u32, height: u32) -> f64 {
    let (short, long) = (width.min(height).max(1) as f64, width.max(height) as f64);
    let up = (DET_SHORT_SIDE as f64 / short).max(1.0);
    up.min(DET_LONG_SIDE as f64 / long)
}

/// The thickness of a detected line, in the pixels the detector sees, under
/// which it starts to miss lines: an image whose lines read whole are
/// thinner than this, at the median, is read again in strips.
///
/// The detector draws a box at least about 9 px thick around any line it
/// finds, so the thickness it gives a line of small text is at that floor.
/// Measured on 172 synthetic captures (lines in DejaVu Sans at 1.6 em, 6 to
/// 16 px high, 736 to 2,000 px wide, 2,600 to 5,000 tall, and pages of 16
/// to 30 px text at 200 and 300 dpi). Of the 134 whose lines came out under
/// 10 px, or with no line, strips read more lines in 123 (up to a fifth at
/// 10 to 16 px, every line where the whole reading found none) and fewer in
/// 5, all 2,000 px wide with 6 to 9 px text. Of the 38 at 10 px or over,
/// the pages among them at 11.5 to 28 px, the whole reading lost one line,
/// in one image. Over all 172, the lines read: whole, 66 %; in strips, 92 %;
/// by this rule, 92 %, with no page cut.
const SMALL_TEXT: f64 = 10.0;

/// The least gain in scale, strips over the whole image, for which an image
/// where no line is seen is read again in strips.
///
/// Such an image has text too small to be seen, or none: a blank scanned
/// verso, a photo. Which of the two it is cannot be told from the reading,
/// so what decides is whether strips could see what the whole reading did
/// not. Measured on synthetic captures where no line was counted (5 to 8 px
/// text): 736 to 1,400 px wide and 2,600 to 5,000 tall, read 1.3 to 2.5
/// times larger, the strips read up to 452 lines where the whole reading
/// read none (156 of 259 in a 736 × 2,600 capture of 6 px text, 271 of 272
/// in a 1,000 × 3,000 one of 7 px); 2,200 to 2,400 tall, 1.1 to 1.2 times
/// larger, they read none either. A blank page 2,550 × 3,300, or a 3,024 ×
/// 4,032 photo, is 2,550 or 3,024 wide, and its strips are no more read at
/// full size (see [`text_too_small`]).
const NO_LINE_GAIN: f64 = 1.25;

/// Whether an image of `width` × `height`, read whole into `whole`, has text
/// too small for the detector to see at the scale it read it: the median
/// thickness of its detected lines, times [`det_scale`], is under
/// [`SMALL_TEXT`].
///
/// Only a line at least twice as long as thick, and covering less than half
/// the image, is counted. Text shrunk below what the detector resolves is
/// found, if at all, as one box over the whole image, read as a few
/// characters of noise: a 1,000 × 4,000 capture of 7 px lines gives a single
/// box 1,000 thick, and a photo gives the same. Where no line is counted,
/// the image is read in strips only where they are detected at full size,
/// the image under [`DET_LONG_SIDE`] wide, and at least [`NO_LINE_GAIN`]
/// times larger than the whole. A strip of an image 2,000 px wide or more is
/// as long as the image is wide, and shrunk past 2,000: of 17 captures 2,000
/// px wide where no line was counted, strips read 3 lines more at most, and
/// on a blank page or a photo nothing, for three times the time of the whole
/// reading.
///
/// A second reading costs a whole one, detection and recognition: oar-ocr
/// does not recognize the lines of boxes it is given.
///
/// It is the text's size, not the image's shape, that says whether a shrunk
/// image loses lines. A page scanned at 300 dpi is shrunk to 0.6, and its
/// 30 px lines are still 18 px to the detector; a capture of the same shape
/// with 10 px text loses a quarter of its lines.
fn text_too_small(width: u32, height: u32, whole: &[(OcrWord, Bounds)]) -> bool {
    let area = width as f64 * height as f64;
    let mut thick: Vec<f64> = whole
        .iter()
        .map(|(w, _)| (w.bbox[2] - w.bbox[0], w.bbox[3] - w.bbox[1]))
        .filter(|&(len, t)| len >= 2.0 * t && len * t < area / 2.0)
        .map(|(_, t)| t)
        .collect();
    if thick.is_empty() {
        let gain = det_scale(width, strip_len(width)) / det_scale(width, height);
        return width < DET_LONG_SIDE && gain >= NO_LINE_GAIN;
    }
    thick.sort_by(f64::total_cmp);
    thick[thick.len() / 2] * det_scale(width, height) < SMALL_TEXT
}

/// The rows a tall image is read in, `(top, bottom)` each, when strips
/// would read it larger than whole; empty when they would not. Whether it
/// is read in them is [`text_too_small`]'s call, on a reading of the whole.
///
/// Read whole, a capture 1,200 pixels wide and 9,000 tall is shrunk to 267 ×
/// 2,000 for detection, and its short rows go unseen. Cut across its height,
/// each strip as long as the detector takes at the image's width, it is
/// detected at full size, 4.5 times larger. A strip of an image 2,000 pixels
/// wide or more is as long as the image is wide, and the detector still
/// shrinks it to 2,000, though less than the whole. The strips overlap by a
/// fifth of their length, so a line cut by one strip's edge is whole in the
/// next; a wide image is not cut, as its lines would be.
fn strips(width: u32, height: u32) -> Vec<(u32, u32)> {
    let len = strip_len(width);
    if height <= len || len < MIN_STRIP {
        return Vec::new();
    }
    let step = len - len / 5;
    let mut out = Vec::new();
    let mut top = 0;
    while top + len < height {
        out.push((top, top + len));
        top += step;
    }
    out.push((height - len, height));
    out
}

/// How tall a strip of an image `width` wide is ([`strips`]): as long as the
/// detector takes it at that width.
fn strip_len(width: u32) -> u32 {
    if width >= DET_LONG_SIDE {
        width
    } else if width >= DET_SHORT_SIDE {
        DET_LONG_SIDE
    } else {
        (DET_LONG_SIDE as u64 * width as u64 / DET_SHORT_SIDE as u64) as u32
    }
}

/// Read one image, its rows starting at `dy` in the image the words are
/// given for: each word, with its quad's bounds (see [`Bounds`]).
fn read_one(ocr: &OAROCR, img: image::RgbImage, dy: u32) -> Result<Vec<(OcrWord, Bounds)>, String> {
    let result = ocr
        .predict(vec![img])
        .map_err(|e| format!("OCR: {e}"))?
        .pop()
        .ok_or("OCR returned no result")?;
    let dy = dy as f64;
    let mut words = Vec::new();
    for r in &result.text_regions {
        let Some(text) = r.text.as_ref() else {
            continue;
        };
        let p = &r.bounding_box.points;
        if p.len() != 4 || text.trim().is_empty() {
            continue;
        }
        let quad = [
            (p[0].x as f64, p[0].y as f64 + dy),
            (p[1].x as f64, p[1].y as f64 + dy),
            (p[2].x as f64, p[2].y as f64 + dy),
            (p[3].x as f64, p[3].y as f64 + dy),
        ];
        let (bbox, angle) = quad_box(quad);
        let word = OcrWord {
            text: text.to_string(),
            bbox,
            angle: Some((angle * 100.0).round() / 100.0),
            conf: r.confidence.map(f64::from),
        };
        words.push((word, bounds(&quad)));
    }
    Ok(words)
}

/// How near a strip's cut edge, in pixels, a word's box may reach before it
/// is taken for one the cut went through.
const CUT_EDGE: f64 = 2.0;

/// The rows and columns a detected quad spans, `[x0, y0, x1, y1]` in the
/// image: unlike the word's box, which is set upright about its center, a
/// line running down the image spans its length in rows.
type Bounds = [f64; 4];

/// The axis-aligned bounds of a quad.
fn bounds(quad: &[(f64, f64); 4]) -> Bounds {
    let xs = quad.iter().map(|q| q.0);
    let ys = quad.iter().map(|q| q.1);
    [
        xs.clone().fold(f64::INFINITY, f64::min),
        ys.clone().fold(f64::INFINITY, f64::min),
        xs.fold(f64::NEG_INFINITY, f64::max),
        ys.fold(f64::NEG_INFINITY, f64::max),
    ]
}

/// One strip's reading: its top and bottom rows, and its words, each with
/// its quad's bounds, in the image's rows.
type StripReading = (u32, u32, Vec<(OcrWord, Bounds)>);

/// The words of an image read in strips, `(top, bottom, words)` each in
/// the image's rows, as one reading; each word comes with its quad's
/// [`Bounds`], on which the cut-edge and duplicate tests below are made.
///
/// A word reaching a strip's cut edge is cut, and dropped: the strips
/// overlap by more than a line, so the next one holds it whole. A line in
/// an overlap is read by both strips, its boxes a pixel or two apart: of two
/// boxes from different strips covering each other at least half, the one
/// further inside its strip is kept. A rule by the overlap's middle would
/// lose a line read astride it, its center on the far side in both strips.
///
/// A line running down the image is tested on the rows it spans, so its
/// pieces in two strips are not both kept. One longer than the strips'
/// overlap (a fifth of a strip, 400 rows for a strip of 2,000) is whole in
/// no strip: it is lost, or kept cut short where the detector's box stops a
/// few pixels before the cut. A page is not cut ([`text_too_small`]): its
/// text is large enough to be seen whole, or, with none seen, its strips are
/// shrunk as well. So this is a tall capture's sideways text.
fn merge_strips(read: Vec<StripReading>, height: u32) -> Vec<OcrWord> {
    // Each word kept so far: its strip, how far inside it it lies, its
    // place in reading order, and its quad's bounds.
    let mut cands: Vec<(usize, f64, usize, OcrWord, Bounds)> = Vec::new();
    for (si, (top, bottom, words)) in read.into_iter().enumerate() {
        let (top, bottom) = (top as f64, bottom as f64);
        let cut_top = top > 0.0;
        let cut_bottom = bottom < height as f64;
        for (w, b) in words {
            if (cut_top && b[1] <= top + CUT_EDGE) || (cut_bottom && b[3] >= bottom - CUT_EDGE) {
                continue;
            }
            let cy = (b[1] + b[3]) / 2.0;
            let inside = match (cut_top, cut_bottom) {
                (true, true) => (cy - top).min(bottom - cy),
                (true, false) => cy - top,
                (false, true) => bottom - cy,
                (false, false) => f64::INFINITY,
            };
            let at = cands.len();
            cands.push((si, inside, at, w, b));
        }
    }
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| cands[b].1.total_cmp(&cands[a].1).then(a.cmp(&b)));
    let mut kept: Vec<usize> = Vec::new();
    for i in order {
        let twice = kept
            .iter()
            .any(|&k| cands[k].0 != cands[i].0 && covers_half(&cands[k].4, &cands[i].4));
        if !twice {
            kept.push(i);
        }
    }
    kept.sort_unstable();
    let mut out: Vec<Option<OcrWord>> = cands.into_iter().map(|c| Some(c.3)).collect();
    kept.into_iter().filter_map(|i| out[i].take()).collect()
}

/// Whether two boxes share at least half of the smaller one.
fn covers_half(a: &[f64; 4], b: &[f64; 4]) -> bool {
    let w = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let h = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let area = |r: &[f64; 4]| (r[2] - r[0]) * (r[3] - r[1]);
    w * h >= 0.5 * area(a).min(area(b))
}

/// A detected line's box and baseline angle, from the detector's quad,
/// clockwise from its top-left as the image shows it.
///
/// A quad at least [`TALL_LINE`] times as tall as wide is read turned: the
/// pipeline turns such a crop a quarter counter-clockwise before reading it,
/// so its text runs down the quad, along its left edge. Otherwise the top
/// edge gives the length and the angle, the left edge the thickness. The
/// box is that set upright about the quad's center.
fn quad_box(p: [(f64, f64); 4]) -> ([f64; 4], f64) {
    let (dx, dy) = (p[1].0 - p[0].0, p[1].1 - p[0].1);
    let (ex, ey) = (p[3].0 - p[0].0, p[3].1 - p[0].1);
    let (top, left) = ((dx * dx + dy * dy).sqrt(), (ex * ex + ey * ey).sqrt());
    let (len, thick, angle) = if left >= TALL_LINE * top {
        (left, top, (-ey).atan2(ex).to_degrees())
    } else {
        (top, left, (-dy).atan2(dx).to_degrees())
    };
    let cx = p.iter().map(|q| q.0).sum::<f64>() / 4.0;
    let cy = p.iter().map(|q| q.1).sum::<f64>() / 4.0;
    (
        [
            cx - len / 2.0,
            cy - thick / 2.0,
            cx + len / 2.0,
            cy + thick / 2.0,
        ],
        angle,
    )
}

/// How many times taller than wide a line's crop is for the pipeline to turn
/// it a quarter before reading it (oar-ocr's `get_rotate_crop_image`, as
/// PaddleOCR's).
const TALL_LINE: f64 = 1.5;

/// Most pixels an image file may hold to be read: 128 Mi, 512 MiB as RGBA.
pub const MAX_PIXELS: u64 = 128 << 20;

/// Longest side an image file may have, in pixels: what bounds one row's
/// buffer, so a 1-pixel-high image a hundred million wide is refused too.
pub const MAX_SIDE: u32 = 65_535;

/// What a PNG decoder may allocate besides the image: a colour profile or
/// a text chunk, inflated. A profile runs to kilobytes; a file of a few
/// hundred kilobytes can inflate one to a gigabyte, held by the header read
/// alone under the image crate's default (512 MiB). Past it, the decoder
/// drops the profile and reads on.
const PNG_CHUNK_BUDGET: u64 = 16 << 20;

/// Decoding limits: at most [`MAX_SIDE`] a side, `alloc` bytes.
fn limits(alloc: u64) -> image::Limits {
    let mut l = image::Limits::default();
    l.max_image_width = Some(MAX_SIDE);
    l.max_image_height = Some(MAX_SIDE);
    l.max_alloc = Some(alloc);
    l
}

/// Decode an image file (PNG, JPEG, TIFF, …) into [`Pixels`], in the
/// layout it holds: grey stays one byte a pixel, colour three, and only an
/// image with transparency takes four.
///
/// Under explicit limits: an image over [`MAX_PIXELS`] or [`MAX_SIDE`] is
/// refused from its header, before its pixels are allocated, and a PNG's
/// chunks get [`PNG_CHUNK_BUDGET`].
pub fn load_pixels(path: &Path) -> Result<Pixels, String> {
    use image::ImageDecoder;
    let at = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let file = || -> Result<_, String> {
        Ok(std::io::BufReader::new(
            std::fs::File::open(path).map_err(|e| at(&e))?,
        ))
    };
    // 16-bit RGBA, the widest layout a decoder gives, at the pixel cap.
    let image_budget = MAX_PIXELS * 8;
    let reader = image::ImageReader::new(file()?)
        .with_guessed_format()
        .map_err(|e| at(&e))?;
    let img = if reader.format() == Some(image::ImageFormat::Png) {
        // The image crate gives a PNG decoder one budget for its chunks and
        // the image both; this one is built with the chunks' own.
        let mut dec = png_decoder(file()?).map_err(|e| at(&e))?;
        within_cap(dec.dimensions()).map_err(|e| at(&e))?;
        dec.set_limits(limits(image_budget)).map_err(|e| at(&e))?;
        image::DynamicImage::from_decoder(dec).map_err(|e| at(&e))?
    } else {
        let mut reader = reader;
        reader.limits(limits(image_budget));
        let dec = reader.into_decoder().map_err(|e| at(&e))?;
        within_cap(dec.dimensions()).map_err(|e| at(&e))?;
        image::DynamicImage::from_decoder(dec).map_err(|e| at(&e))?
    };
    let (w, h) = (img.width(), img.height());
    let color = img.color();
    let (layout, data) = match img {
        image::DynamicImage::ImageLuma8(b) => (Layout::Luma8, b.into_raw()),
        image::DynamicImage::ImageRgb8(b) => (Layout::Rgb8, b.into_raw()),
        image::DynamicImage::ImageRgba8(b) => (Layout::Rgba8, b.into_raw()),
        other if color.has_alpha() => (Layout::Rgba8, other.into_rgba8().into_raw()),
        other if color.has_color() => (Layout::Rgb8, other.into_rgb8().into_raw()),
        other => (Layout::Luma8, other.into_luma8().into_raw()),
    };
    Pixels::new(w, h, layout, data).ok_or_else(|| format!("{}: empty image", path.display()))
}

/// A PNG decoder that has read the file's header under
/// [`PNG_CHUNK_BUDGET`].
fn png_decoder<R: std::io::BufRead + std::io::Seek>(
    r: R,
) -> image::ImageResult<image::codecs::png::PngDecoder<R>> {
    image::codecs::png::PngDecoder::with_limits(r, limits(PNG_CHUNK_BUDGET))
}

/// An image of `(width, height)` within [`MAX_PIXELS`].
fn within_cap((w, h): (u32, u32)) -> Result<(), String> {
    let n = w as u64 * h as u64;
    if n > MAX_PIXELS {
        return Err(format!(
            "{w} × {h} pixels: more than the {MAX_PIXELS} an image may hold to be read"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_while_reading_frees_its_place() {
        // One pipeline; a reader panics with it: the next reader gets a new
        // one rather than waiting for good.
        let pipes = Pipelines::<u32>::new(1);
        let built = std::sync::atomic::AtomicU32::new(0);
        let build = || Ok(built.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lease = pipes.take(build).unwrap();
            panic!("the engine failed");
        }));
        assert!(r.is_err());
        assert_eq!(pipes.built(), 0, "the panicked pipeline is not kept");
        let again = std::thread::scope(|s| s.spawn(|| *pipes.take(build).unwrap()).join().unwrap());
        assert_eq!(again, 1, "a new pipeline, not the one a panic went through");
    }

    #[test]
    fn a_failed_build_frees_its_place() {
        let pipes = Pipelines::<u32>::new(1);
        assert!(pipes.take(|| Err("no".into())).is_err());
        assert_eq!(pipes.built(), 0);
        assert_eq!(*pipes.take(|| Ok(7)).unwrap(), 7);
    }

    #[test]
    fn a_pipeline_given_back_is_taken_again() {
        let pipes = Pipelines::<u32>::new(2);
        drop(pipes.take(|| Ok(1)).unwrap());
        assert_eq!(*pipes.take(|| Ok(2)).unwrap(), 1);
        assert_eq!(pipes.built(), 1);
    }

    #[test]
    fn an_image_file_keeps_its_layout() {
        let dir = std::env::temp_dir().join(format!("fdoc-load-pixels-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let grey = dir.join("grey.png");
        image::GrayImage::from_pixel(30, 10, image::Luma([200]))
            .save(&grey)
            .unwrap();
        let colour = dir.join("colour.png");
        image::RgbImage::from_pixel(30, 10, image::Rgb([1, 2, 3]))
            .save(&colour)
            .unwrap();
        let clear = dir.join("clear.png");
        image::GrayAlphaImage::from_pixel(30, 10, image::LumaA([0, 0]))
            .save(&clear)
            .unwrap();
        let g = load_pixels(&grey).unwrap();
        assert_eq!((g.layout, g.data.len()), (Layout::Luma8, 300));
        assert_eq!(load_pixels(&colour).unwrap().layout, Layout::Rgb8);
        let c = load_pixels(&clear).unwrap();
        assert_eq!(c.layout, Layout::Rgba8, "transparency is kept");
        assert_eq!(c.to_rgb()[..3], [255, 255, 255], "and reads as paper");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A PNG of one grey pixel claiming `(width, height)`, and with an
    /// `iCCP` chunk holding `profile` zero bytes when given.
    fn png_claiming(width: u32, height: u32, profile: Option<usize>) -> Vec<u8> {
        fn crc(bytes: &[u8]) -> u32 {
            let mut c = 0xffff_ffffu32;
            for &b in bytes {
                c ^= b as u32;
                for _ in 0..8 {
                    c = if c & 1 == 1 {
                        0xedb8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
            out.extend((data.len() as u32).to_be_bytes());
            let body = [kind, data].concat();
            out.extend(&body);
            out.extend(crc(&body).to_be_bytes());
        }
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let ihdr = [width.to_be_bytes(), height.to_be_bytes()].concat();
        chunk(&mut out, b"IHDR", &[&ihdr[..], &[8, 0, 0, 0, 0]].concat());
        if let Some(n) = profile {
            chunk(
                &mut out,
                b"iCCP",
                &[b"p\0\0".as_slice(), &zlib_zeros(n)].concat(),
            );
        }
        // One row: filter byte 0, one grey pixel, as a stored zlib block.
        chunk(
            &mut out,
            b"IDAT",
            &[0x78, 0x01, 1, 2, 0, 0xfd, 0xff, 0, 128, 0, 0x81, 0, 0x81],
        );
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// A zlib stream of `n` zero bytes, `n - 1` a multiple of 258: one
    /// literal zero, then matches of 258 a distance of 1 back, in a block
    /// of the fixed codes, a few bytes per 64 KiB.
    fn zlib_zeros(n: usize) -> Vec<u8> {
        assert_eq!((n - 1) % 258, 0);
        let mut bits: Vec<bool> = Vec::new();
        let mut put = |code: u32, len: u32, msb_first: bool| {
            for i in 0..len {
                let bit = if msb_first { len - 1 - i } else { i };
                bits.push(code >> bit & 1 == 1);
            }
        };
        put(1, 1, false); // final block
        put(1, 2, false); // fixed codes
        put(0x30, 8, true); // literal 0
        for _ in 0..(n - 1) / 258 {
            put(0b1100_0101, 8, true); // length 258
            put(0, 5, true); // distance 1
        }
        put(0, 7, true); // end of block
        let mut out = vec![0x78, 0x01];
        for byte in bits.chunks(8) {
            out.push(byte.iter().enumerate().map(|(i, &b)| (b as u8) << i).sum());
        }
        let b = (n % 65521) as u32;
        out.extend(((b << 16) | 1).to_be_bytes());
        out
    }

    #[test]
    fn an_image_file_is_read_under_explicit_limits() {
        let dir = std::env::temp_dir().join(format!("fdoc-limits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, bytes: Vec<u8>| {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        // The fixture is a real PNG: one pixel reads.
        let one = load_pixels(&file("one.png", png_claiming(1, 1, None))).unwrap();
        assert_eq!((one.width, one.height, one.layout), (1, 1, Layout::Luma8));
        // A header claiming 20,000 × 20,000 is refused before any pixel is
        // allocated, and so is a side past the longest.
        let e = load_pixels(&file("big.png", png_claiming(20_000, 20_000, None))).unwrap_err();
        assert!(e.contains("more than"), "{e}");
        assert!(load_pixels(&file("wide.png", png_claiming(100_000, 1, None))).is_err());
        // A colour profile that inflates past the chunk budget is dropped
        // as it is read, the image read on; a small one is kept.
        let open = |p: &Path| {
            png_decoder(std::io::BufReader::new(std::fs::File::open(p).unwrap())).unwrap()
        };
        use image::ImageDecoder;
        let bomb = file("bomb.png", png_claiming(1, 1, Some(258 * 130_000 + 1)));
        assert!(
            std::fs::metadata(&bomb).unwrap().len() < 250_000,
            "33 MB from a small file"
        );
        assert_eq!(open(&bomb).icc_profile().unwrap(), None);
        assert!(load_pixels(&bomb).is_ok());
        let small = file("icc.png", png_claiming(1, 1, Some(258 * 4 + 1)));
        assert_eq!(
            open(&small).icc_profile().unwrap().map(|p| p.len()),
            Some(1033)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A whole reading of `n` level lines `thick` pixels thick, each twenty
    /// times as long, as [`read_one`] gives it.
    fn lines(n: usize, thick: f64) -> Vec<(OcrWord, Bounds)> {
        (0..n)
            .map(|i| {
                let y = i as f64 * thick * 1.6;
                let bbox = [10.0, y, 10.0 + 20.0 * thick, y + thick];
                let word = OcrWord {
                    text: format!("line {i}"),
                    bbox,
                    angle: Some(0.0),
                    conf: Some(0.99),
                };
                (word, bbox)
            })
            .collect()
    }

    #[test]
    fn a_scanned_page_is_read_whole() {
        // Letter, A4, A3 and Legal, at 200 to 600 dpi, upright or turned:
        // 10 pt body text, its detected lines about an em thick (0.96 to
        // 1.16 measured; 0.9 here), is 15 px or more to the detector,
        // however much the page is shrunk.
        for (w, h, dpi) in [
            (1700, 2200, 200.0),
            (2550, 3300, 300.0),
            (5100, 6600, 600.0),
            (1654, 2339, 200.0),
            (2481, 3508, 300.0),
            (3508, 4961, 300.0),
            (1700, 2800, 200.0),
            (2550, 4200, 300.0),
            (3400, 5600, 400.0),
        ] {
            let body = lines(40, 0.9 * 10.0 / 72.0 * dpi);
            assert!(!text_too_small(w, h, &body), "{w} × {h} is read whole");
            assert!(!text_too_small(h, w, &body), "{h} × {w} is read whole");
        }
        // Its 6 pt footnotes too.
        assert!(!text_too_small(
            2550,
            3300,
            &lines(40, 0.9 * 6.0 / 72.0 * 300.0)
        ));
        // A few specks or lines of fine print under the body do not cut it:
        // the median is the body's.
        let mut body = lines(40, 0.9 * 10.0 / 72.0 * 300.0);
        body.extend(lines(3, 4.0));
        assert!(!text_too_small(2550, 3300, &body));
    }

    #[test]
    fn a_capture_of_small_text_is_read_again_in_strips() {
        // 10 px lines, their boxes 12 px thick: shrunk to 0.77 for 1,000 ×
        // 2,600, they are 9.2 px to the detector, which misses some.
        assert!(text_too_small(1000, 2600, &lines(150, 12.0)));
        // 13 px lines, 15 px boxes: 11.5 px, all seen.
        assert!(!text_too_small(1000, 2600, &lines(120, 15.0)));
        // The same 12 px boxes in a capture 1,400 wide, read at the same
        // scale, are as small; at 1,000 × 2,000, read at full size, not.
        assert!(text_too_small(1400, 2600, &lines(150, 12.0)));
        assert!(!text_too_small(1000, 2000, &lines(150, 12.0)));
        // The median decides: a few large headings do not hide the body,
        // nor do many, short of half the lines.
        let mut mixed = lines(100, 12.0);
        mixed.extend(lines(20, 40.0));
        assert!(text_too_small(1000, 2600, &mixed));
        let mut mixed = lines(60, 12.0);
        mixed.extend(lines(40, 40.0));
        assert!(text_too_small(1000, 2600, &mixed));
    }

    #[test]
    fn a_capture_where_no_line_is_seen_is_read_in_strips() {
        assert!(text_too_small(1000, 4000, &[]), "nothing detected");
        // Strips 2,000 long read 736 × 2,600 1.3 times larger.
        assert!(text_too_small(736, 2600, &[]));
        // Text the detector cannot resolve comes back as one box over the
        // whole image, read as noise: no line.
        let all = [0.0, 0.0, 1000.0, 4000.0];
        let blob = |bbox: [f64; 4]| OcrWord {
            text: "WNm".into(),
            bbox,
            angle: Some(90.0),
            conf: Some(0.2),
        };
        // Upright about its center, as [`quad_box`] sets a tall quad.
        let upright = [-1500.0, 1500.0, 2500.0, 2500.0];
        assert!(text_too_small(1000, 4000, &[(blob(upright), all)]));
        // Nor is a box as thick as it is long.
        let square = [100.0, 100.0, 400.0, 400.0];
        assert!(text_too_small(1000, 4000, &[(blob(square), square)]));
    }

    #[test]
    fn an_image_where_no_line_is_seen_is_read_whole_where_strips_see_no_more() {
        // A blank scanned verso and a phone photo, its noise one box over
        // all of it: their strips, as long as they are wide, are shrunk too.
        assert!(!text_too_small(2550, 3300, &[]));
        let all = [0.0, 0.0, 3024.0, 4032.0];
        let noise = OcrWord {
            text: "中".into(),
            bbox: all,
            angle: Some(0.0),
            conf: Some(0.3),
        };
        assert!(!text_too_small(3024, 4032, &[(noise, all)]));
        assert!(!text_too_small(2000, 4000, &[]));
        // Strips that read it barely larger: 1.2 and 1.02 times.
        assert!(!text_too_small(1000, 2400, &[]));
        assert!(!text_too_small(1536, 2048, &[]));
    }

    #[test]
    fn strips_are_offered_wherever_they_read_an_image_larger() {
        // Strips 2,000 long for a capture 736 to 2,000 wide: one a row
        // taller is cut, in two.
        assert!(strips(1000, 2000).is_empty());
        assert_eq!(strips(1000, 2001), [(0, 2000), (1, 2001)]);
        assert_eq!(strips(1000, 3600), [(0, 2000), (1600, 3600)]);
        // As long as the image is wide from 2,000 on, so a page is offered
        // them, and read whole on its text's size.
        assert!(!strips(2550, 3300).is_empty());
        assert!(strips(3300, 2550).is_empty());
        // Scaled up to 736 across first.
        assert!(strips(368, 1000).is_empty());
        assert!(!strips(368, 1001).is_empty(), "strips of 1,000 rows");
    }

    #[test]
    fn a_tall_image_is_read_in_overlapping_strips() {
        // Within what the detector takes whole: no strips.
        assert!(strips(1224, 1584).is_empty());
        assert!(strips(9000, 1200).is_empty(), "a wide image is not cut");
        // 1,200 × 9,000: strips of 2,000, a fifth overlapping, the last
        // flush with the bottom.
        let s = strips(1200, 9000);
        assert_eq!(s[0], (0, 2000));
        assert_eq!(s[1], (1600, 3600));
        assert_eq!(*s.last().unwrap(), (7000, 9000));
        assert!(
            s.windows(2).all(|w| w[1].0 < w[0].1),
            "each overlaps the next"
        );
        // A narrow receipt is scaled up to 736 across: shorter strips.
        assert_eq!(strips(368, 6000)[0], (0, 1000));
        // Too narrow to be worth strips.
        assert!(strips(40, 9000).is_empty());
    }

    fn w(text: &str, y0: f64, y1: f64) -> (OcrWord, Bounds) {
        let quad = [(60.0, y0), (400.0, y0), (400.0, y1), (60.0, y1)];
        let (bbox, angle) = quad_box(quad);
        let word = OcrWord {
            text: text.into(),
            bbox,
            angle: Some(angle),
            conf: None,
        };
        (word, bounds(&quad))
    }

    /// A line running down the image from row `y0` to `y1`, 30 px thick.
    fn down(text: &str, x: f64, y0: f64, y1: f64) -> (OcrWord, Bounds) {
        let quad = [(x, y0), (x + 30.0, y0), (x + 30.0, y1), (x, y1)];
        let (bbox, angle) = quad_box(quad);
        let word = OcrWord {
            text: text.into(),
            bbox,
            angle: Some(angle),
            conf: None,
        };
        (word, bounds(&quad))
    }

    #[test]
    fn a_line_down_a_tiled_image_is_tested_on_the_rows_it_spans() {
        let texts =
            |words: Vec<OcrWord>| -> Vec<String> { words.into_iter().map(|w| w.text).collect() };
        // Strips 0-2000 and 1600-3600. "inside" runs down rows 1650-1950,
        // inside the overlap: the first strip reads it as one line, the
        // second as two, "ins" and "ide". Their upright boxes (30 tall) do
        // not meet; the rows they span do, and the line is kept once, from
        // the second strip, where "ide" lies further inside. "cut" runs
        // down 1700-2100: the first strip's piece reaches its bottom edge,
        // though its upright box is far from it, and is dropped; the second
        // holds it whole. "long", 1000-2500, is longer than the overlap:
        // cut in both, lost.
        let read = vec![
            (
                0,
                2000,
                vec![
                    down("inside", 100.0, 1650.0, 1950.0),
                    down("cu", 300.0, 1700.0, 2000.0),
                    down("lo", 500.0, 1000.0, 2000.0),
                ],
            ),
            (
                1600,
                3600,
                vec![
                    down("ins", 101.0, 1651.0, 1790.0),
                    down("ide", 101.0, 1800.0, 1951.0),
                    down("cut", 300.0, 1700.0, 2100.0),
                    down("ng", 500.0, 1600.0, 2500.0),
                ],
            ),
        ];
        assert_eq!(texts(merge_strips(read, 3600)), ["ins", "ide", "cut"]);
    }

    #[test]
    fn a_line_read_by_two_strips_is_kept_once() {
        let texts =
            |words: Vec<OcrWord>| -> Vec<String> { words.into_iter().map(|w| w.text).collect() };
        // Strips 0-2000 and 1600-3600 of a 3,600-row image. "astride" sits
        // on the overlap's middle (1800), read a pixel lower by the second
        // strip: kept once. "cut" reaches the first strip's bottom edge and
        // is whole in the second. "first" and "second" each lie in one.
        let read = vec![
            (
                0,
                2000,
                vec![
                    w("first", 100.0, 120.0),
                    w("astride", 1791.0, 1810.0),
                    w("cut", 1985.0, 2000.0),
                ],
            ),
            (
                1600,
                3600,
                vec![
                    w("astride", 1792.0, 1811.0),
                    w("cut", 1985.0, 2005.0),
                    w("second", 3000.0, 3020.0),
                ],
            ),
        ];
        assert_eq!(
            texts(merge_strips(read, 3600)),
            ["first", "astride", "cut", "second"]
        );
        // Two lines one above the other in one strip are both kept.
        let read = vec![(0, 2000, vec![w("a", 100.0, 120.0), w("b", 110.0, 130.0)])];
        assert_eq!(texts(merge_strips(read, 2000)), ["a", "b"]);
    }

    #[test]
    fn a_tall_line_reads_down_its_quad() {
        // A level line: 200 wide, 20 tall.
        let (b, a) = quad_box([(10.0, 10.0), (210.0, 10.0), (210.0, 30.0), (10.0, 30.0)]);
        assert_eq!((b, a), ([10.0, 10.0, 210.0, 30.0], 0.0));
        // Text running down a sideways capture: the detector's quad is 20
        // wide and 200 tall, and the pipeline read it turned a quarter.
        let (b, a) = quad_box([(100.0, 50.0), (120.0, 50.0), (120.0, 250.0), (100.0, 250.0)]);
        assert_eq!(a, -90.0);
        assert_eq!(
            b,
            [10.0, 140.0, 210.0, 160.0],
            "200 long, 20 thick, about the center"
        );
        // A short word set upright is not read turned under the threshold.
        let (_, a) = quad_box([(0.0, 0.0), (20.0, 0.0), (20.0, 25.0), (0.0, 25.0)]);
        assert_eq!(a, 0.0);
    }
}
