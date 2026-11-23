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
//! upright about its center, with the line's angle. An image taller than the
//! detector takes without shrinking it is read in overlapping strips
//! ([`strips`]), and its lines kept once.

use crate::assets::{Assets, Runtime};
use crate::{Coords, OcrEngine, OcrWord, Pixels, WordReading};
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
        let img = image::RgbImage::from_raw(image.width, image.height, image.to_rgb())
            .ok_or("the image's buffer does not match its size")?;
        let bands = strips(image.width, image.height);
        let lease = self.pipes.take(|| self.pipeline())?;
        let words = if bands.is_empty() {
            read_one(&lease, img, 0)?
        } else {
            let mut read = Vec::new();
            for &(top, bottom) in &bands {
                let strip =
                    image::imageops::crop_imm(&img, 0, top, image.width, bottom - top).to_image();
                read.push((top, bottom, read_one(&lease, strip, top)?));
            }
            merge_strips(read, image.height)
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

/// The rows a tall image is read in, `(top, bottom)` each, when read whole
/// the detector would shrink it; empty to read it whole.
///
/// Read whole, a capture 1,200 pixels wide and 9,000 tall is shrunk to 267 ×
/// 2,000 for detection, and its short rows go unseen. Cut across its height,
/// each strip as long as the detector takes without shrinking it at the
/// image's width, it is detected at full size. The strips overlap by a fifth
/// of their length, so a line cut by one strip's edge is whole in the next;
/// a wide image is not cut, as its lines would be.
fn strips(width: u32, height: u32) -> Vec<(u32, u32)> {
    let len = if width >= DET_LONG_SIDE {
        width
    } else if width >= DET_SHORT_SIDE {
        DET_LONG_SIDE
    } else {
        (DET_LONG_SIDE as u64 * width as u64 / DET_SHORT_SIDE as u64) as u32
    };
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

/// Read one image, its rows starting at `dy` in the image the words are
/// given for.
fn read_one(ocr: &OAROCR, img: image::RgbImage, dy: u32) -> Result<Vec<OcrWord>, String> {
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
        let (bbox, angle) = quad_box([
            (p[0].x as f64, p[0].y as f64 + dy),
            (p[1].x as f64, p[1].y as f64 + dy),
            (p[2].x as f64, p[2].y as f64 + dy),
            (p[3].x as f64, p[3].y as f64 + dy),
        ]);
        words.push(OcrWord {
            text: text.to_string(),
            bbox,
            angle: Some((angle * 100.0).round() / 100.0),
            conf: r.confidence.map(f64::from),
        });
    }
    Ok(words)
}

/// How near a strip's cut edge, in pixels, a word's box may reach before it
/// is taken for one the cut went through.
const CUT_EDGE: f64 = 2.0;

/// The words of an image read in strips, `(top, bottom, words)` each in
/// the image's rows, as one reading.
///
/// A word reaching a strip's cut edge is cut, and dropped: the strips
/// overlap by more than a line, so the next one holds it whole. A line in
/// an overlap is read by both strips, its boxes a pixel or two apart: of two
/// boxes from different strips covering each other at least half, the one
/// further inside its strip is kept. A rule by the overlap's middle would
/// lose a line read astride it, its center on the far side in both strips.
fn merge_strips(read: Vec<(u32, u32, Vec<OcrWord>)>, height: u32) -> Vec<OcrWord> {
    // Each word kept so far: its strip, how far inside it it lies, and its
    // place in reading order.
    let mut cands: Vec<(usize, f64, usize, OcrWord)> = Vec::new();
    for (si, (top, bottom, words)) in read.into_iter().enumerate() {
        let (top, bottom) = (top as f64, bottom as f64);
        let cut_top = top > 0.0;
        let cut_bottom = bottom < height as f64;
        for w in words {
            if (cut_top && w.bbox[1] <= top + CUT_EDGE)
                || (cut_bottom && w.bbox[3] >= bottom - CUT_EDGE)
            {
                continue;
            }
            let cy = (w.bbox[1] + w.bbox[3]) / 2.0;
            let inside = match (cut_top, cut_bottom) {
                (true, true) => (cy - top).min(bottom - cy),
                (true, false) => cy - top,
                (false, true) => bottom - cy,
                (false, false) => f64::INFINITY,
            };
            let at = cands.len();
            cands.push((si, inside, at, w));
        }
    }
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| cands[b].1.total_cmp(&cands[a].1).then(a.cmp(&b)));
    let mut kept: Vec<usize> = Vec::new();
    for i in order {
        let twice = kept
            .iter()
            .any(|&k| cands[k].0 != cands[i].0 && covers_half(&cands[k].3.bbox, &cands[i].3.bbox));
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

    fn w(text: &str, y0: f64, y1: f64) -> OcrWord {
        OcrWord {
            text: text.into(),
            bbox: [60.0, y0, 400.0, y1],
            angle: Some(0.0),
            conf: None,
        }
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
