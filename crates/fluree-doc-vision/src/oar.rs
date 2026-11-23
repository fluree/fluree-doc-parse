//! The built-in OCR engine: PP-OCRv6 small through oar-ocr.
//!
//! oar-ocr runs its models on ONNX Runtime, a native library. It is not
//! linked into this crate: [`OarEngine::open`] loads it from the assets
//! directory ([`crate::assets`]), after checking its hash, so a build carries
//! the engine's code (about 6 MB in `fdoc`, 5 MB stripped) and a deployment
//! ships the library and the models for its platform beside the binary.
//!
//! The detector's settings are the ones the corpus benchmark measured
//! (detection sized on the image's short side at 736 px, at most 2,000 px on
//! the long one): each word box this engine gives is a detected line, turned
//! upright about its center, with the line's angle.

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
            limit_side_len: Some(736),
            limit_type: Some(LimitType::Min),
            max_side_len: Some(2000),
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
        let lease = self.pipes.take(|| self.pipeline())?;
        let result = lease.predict(vec![img]);
        drop(lease);
        let result = result
            .map_err(|e| format!("OCR: {e}"))?
            .pop()
            .ok_or("OCR returned no result")?;
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
                (p[0].x as f64, p[0].y as f64),
                (p[1].x as f64, p[1].y as f64),
                (p[2].x as f64, p[2].y as f64),
                (p[3].x as f64, p[3].y as f64),
            ]);
            words.push(OcrWord {
                text: text.to_string(),
                bbox,
                angle: Some((angle * 100.0).round() / 100.0),
                conf: r.confidence.map(f64::from),
            });
        }
        Ok(WordReading {
            coords: Coords::Px,
            width: Some(image.width as f64),
            height: Some(image.height as f64),
            engine: Some(ENGINE.into()),
            words,
        })
    }
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
