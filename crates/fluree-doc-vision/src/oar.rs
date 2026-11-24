//! The built-in OCR engine: PP-OCRv6 small through oar-ocr.
//!
//! oar-ocr runs its models on ONNX Runtime, a native library. It is not
//! linked into this crate: [`OarEngine::open`] loads it from the assets
//! directory ([`crate::assets`]), after checking its hash, so a build carries
//! the engine's code (5.8 MB in a release build of `fdoc`) and a deployment
//! ships the library and the models for its platform beside the binary.
//!
//! An image is read at its own resolution, in passes the [`OcrSizing`] it
//! is opened with plans ([`crate::sizing`]): whole, or in overlapping strips
//! across its width, whose lines are kept once ([`merge_strips`]); and read
//! again turned a half if it reads poorly ([`read_upright`]). Each word
//! box this engine gives is a detected line, turned upright about its
//! center, with the line's angle.
//!
//! Memory. The detector's memory grows with the pixels it reads, about
//! [`BYTES_PER_PIXEL`] each; the pipelines reading at once share one budget
//! of it ([`OarOptions::memory`]), and one ONNX Runtime memory arena, which
//! gives back what a large image took once it is read.

use crate::assets::{Assets, Runtime};
use crate::sizing::{OcrPlan, OcrSizing, Pass};
use crate::{Coords, OcrEngine, OcrWord, Pixels, WordReading};
use oar_ocr::core::config::{OrtExecutionProvider, OrtSessionConfig};
use oar_ocr::domain::tasks::TextDetectionConfig;
use oar_ocr::oarocr::{OAROCRBuilder, OAROCR};
use oar_ocr::processors::LimitType;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

/// The name each reading records.
pub const ENGINE: &str = "pp-ocrv6-small/oar-ocr";

/// What the detector holds while it reads, per pixel it reads (the image
/// as the detector is given it: padded, enlarged), arena included.
///
/// Measured on PP-OCRv6 small with the shared arena, one image at a time,
/// less the pipeline's own memory: a page scanned at 300 dpi peaked at 150
/// bytes a pixel, a 3,024 × 4,032 photo at 210, the 12.5 Mpx strips of a
/// 2,400 × 9,000 capture at 240.
pub const BYTES_PER_PIXEL: u64 = 230;

/// The default of [`OarOptions::memory`]: 8 GiB, four pages scanned at 300
/// dpi read at once, or three 12 Mpx photos.
///
/// Measured with four pipelines, four images at a time: four photos peaked
/// at 7.9 GB in 6.6 s (8.9 GB in 5.5 s with no bound), four tall captures
/// read in strips at 6.6 GB in 175 s (10.2 GB in 121 s); four pages read as
/// with no bound, 6.9 GB in 18 s. At 6 GiB the pages took 27 s.
pub const DEFAULT_MEMORY: u64 = 8 << 30;

/// Whom an engine tells what a reading did that its caller should know.
pub type Notices = Arc<dyn Fn(&str) + Send + Sync>;

/// How the engine runs.
#[derive(Clone)]
pub struct OarOptions {
    /// ONNX Runtime threads per model run. One by default: a caller reading
    /// many images at once reads each on its own thread, and threads inside
    /// each run would fight over the same cores.
    pub threads: usize,
    /// Most images read at once, each by a pipeline of its own; a thread
    /// that finds them all busy waits for one. Idle, a pipeline holds about
    /// 0.3 GB. By default the cores, at most four: measured on twelve cores,
    /// eight read no faster than four.
    pub pipelines: usize,
    /// How images are sized: at most [`OcrSizing::max_pixels`] a pass.
    pub sizing: OcrSizing,
    /// The memory, in bytes, the passes being read at once may take, each
    /// counted at [`BYTES_PER_PIXEL`] a pixel: a pipeline whose pass does
    /// not fit waits for others to finish theirs. A pass larger than all of
    /// it is read alone.
    pub memory: u64,
    /// Told of what a reading does that its caller should know: an image
    /// shrunk to fit [`OcrSizing::max_pixels`]. Called on the thread that
    /// asked for the reading, so a caller reading many documents can say
    /// which one it is. None: nobody is told.
    pub notices: Option<Notices>,
}

impl std::fmt::Debug for OarOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OarOptions")
            .field("threads", &self.threads)
            .field("pipelines", &self.pipelines)
            .field("sizing", &self.sizing)
            .field("memory", &self.memory)
            .field("notices", &self.notices.is_some())
            .finish()
    }
}

impl Default for OarOptions {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        OarOptions {
            threads: 1,
            pipelines: cores.min(4),
            sizing: OcrSizing::default(),
            memory: DEFAULT_MEMORY,
            notices: None,
        }
    }
}

/// PP-OCRv6 small, opened once and shared across threads.
///
/// oar-ocr runs each model of a pipeline under a lock, so one pipeline reads
/// one image at a time. The engine keeps a pipeline per image being read at
/// once: one when it opens, and another each time a thread finds them all
/// busy, built from the same checked models and kept for the next reading,
/// up to [`OarOptions::pipelines`]. Sharing one pipeline between threads
/// would save only its own memory (about 70 MB), not what a reading takes.
pub struct OarEngine {
    det: Vec<u8>,
    rec: Vec<u8>,
    dict: String,
    threads: usize,
    sizing: OcrSizing,
    pipes: Pipelines<OAROCR>,
    budget: Budget,
    notices: Option<Notices>,
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

/// The memory the passes read at once may take, in bytes, handed out first
/// come, first served: a pass waits until it fits beside those being read,
/// or, larger than the whole budget, until none is. In turn, so a large
/// pass is not passed over for good by small ones.
struct Budget {
    most: u64,
    state: Mutex<BudgetState>,
    freed: Condvar,
}

struct BudgetState {
    /// Bytes held by the passes being read.
    held: u64,
    /// The next turn to be served, and the next to be handed out.
    serving: u64,
    issued: u64,
}

/// A share of the [`Budget`], given back when dropped (a panic included).
struct Grant<'a> {
    budget: &'a Budget,
    bytes: u64,
}

impl Budget {
    fn new(most: u64) -> Self {
        Budget {
            most: most.max(1),
            state: Mutex::new(BudgetState {
                held: 0,
                serving: 0,
                issued: 0,
            }),
            freed: Condvar::new(),
        }
    }

    /// `bytes` of the budget, once it is this caller's turn and they fit.
    fn take(&self, bytes: u64) -> Grant<'_> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let turn = st.issued;
        st.issued += 1;
        while st.serving != turn || (st.held > 0 && st.held + bytes > self.most) {
            st = self.freed.wait(st).unwrap_or_else(|e| e.into_inner());
        }
        st.held += bytes;
        st.serving += 1;
        drop(st);
        // The next in turn may fit beside this one.
        self.freed.notify_all();
        Grant {
            budget: self,
            bytes,
        }
    }

    fn held(&self) -> u64 {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).held
    }
}

impl Drop for Grant<'_> {
    fn drop(&mut self) {
        let mut st = self.budget.state.lock().unwrap_or_else(|e| e.into_inner());
        st.held -= self.bytes;
        drop(st);
        self.budget.freed.notify_all();
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
            let builder = ort::init_from(runtime.load_path())
                .map_err(|e| format!("{}: {e}", path.display()))?;
            install_environment(builder)?;
            *state = Some(path.clone());
            Ok(())
        }
    }
}

/// Build the process's one ONNX Runtime environment, register on it the CPU
/// memory arena every session shares (`session.use_env_allocators`), and
/// hand it to oar-ocr, which would otherwise build one without.
///
/// One arena for all pipelines rather than one each: a block freed by one
/// pipeline serves the next reading of another, and each session's run
/// options shrink it after the run ([`OarEngine::pipeline`]), so the memory
/// a large image took is given back. Measured with four pipelines reading
/// four 12 Mpx photos: 9 GB at the peak and 1 GB held after, against 11 GB
/// held for good with an arena per session, at the same speed. Grown by
/// powers of two (ONNX Runtime's default): growing by what is asked serves
/// images of varying sizes worse, at a higher peak.
///
/// `ort` has no safe call for registering an arena on an environment, so
/// this goes through its C API.
fn install_environment(builder: ort::environment::EnvironmentBuilder) -> Result<(), String> {
    use ort::AsPointer;
    let env = builder.build().map_err(|e| e.to_string())?;
    env.set_log_level(ort::logging::LogLevel::Error);
    let api = ort::api();
    let check = |status: ort::sys::OrtStatusPtr| -> Result<(), String> {
        if status.0.is_null() {
            return Ok(());
        }
        // SAFETY: a non-null status from the C API holds a message, and is
        // released here, once.
        let msg = unsafe { std::ffi::CStr::from_ptr((api.GetErrorMessage)(status.0)) }
            .to_string_lossy()
            .into_owned();
        unsafe { (api.ReleaseStatus)(status.0) };
        Err(format!("registering the shared ONNX Runtime arena: {msg}"))
    };
    let info = ort::memory::MemoryInfo::new(
        ort::memory::AllocationDevice::CPU,
        0,
        ort::memory::AllocatorType::Arena,
        ort::memory::MemoryType::Default,
    )
    .map_err(|e| e.to_string())?;
    // Grown by powers of two (`arena_extend_strategy` 0), no cap.
    let keys = [c"arena_extend_strategy".as_ptr()];
    let values: [usize; 1] = [0];
    let mut cfg: *mut ort::sys::OrtArenaCfg = std::ptr::null_mut();
    // SAFETY: one key and one value, both live for the call; `cfg` is
    // written by it.
    check(unsafe { (api.CreateArenaCfgV2)(keys.as_ptr(), values.as_ptr(), 1, &mut cfg) })?;
    // SAFETY: `env` and `info` are live handles, and `cfg` the one just
    // created; the environment copies what it needs from both.
    let registered =
        check(unsafe { (api.CreateAndRegisterAllocator)(env.ptr() as *mut _, info.ptr(), cfg) });
    // SAFETY: released once, whether the registration took it or not.
    unsafe { (api.ReleaseArenaCfg)(cfg) };
    registered?;
    oar_ocr::core::inference::set_ort_environment(env).map_err(|e| e.to_string())
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
            sizing: options.sizing,
            pipes: Pipelines::new(options.pipelines),
            budget: Budget::new(options.memory),
            notices: options.notices.clone(),
        };
        drop(engine.pipes.take(|| engine.pipeline())?);
        Ok(engine)
    }

    /// How many pipelines the engine holds now, reading or not: what its
    /// memory grows with.
    pub fn pipelines(&self) -> usize {
        self.pipes.built()
    }

    /// How the engine sizes the images it reads.
    pub fn sizing(&self) -> OcrSizing {
        self.sizing
    }

    /// The bytes of [`OarOptions::memory`] the passes being read hold now.
    pub fn memory_held(&self) -> u64 {
        self.budget.held()
    }

    /// The memory the passes read at once may take ([`OarOptions::memory`]).
    pub fn memory(&self) -> u64 {
        self.budget.most
    }

    /// A new pipeline on the engine's models.
    fn pipeline(&self) -> Result<OAROCR, String> {
        // The detector takes a short side under 736 up to it, which the
        // reading's padding turns into the enlargement it plans
        // ([`crate::sizing`]); it never shrinks a pass, planned within the
        // cap, whose long side is then at most `max_side`.
        let det = TextDetectionConfig {
            score_threshold: 0.2,
            box_threshold: 0.45,
            unclip_ratio: 1.4,
            max_candidates: 3000,
            limit_side_len: Some(crate::sizing::DET_SHORT_SIDE),
            limit_type: Some(LimitType::Min),
            max_side_len: Some(self.sizing.max_side()),
        };
        let mut session = OrtSessionConfig::new()
            .with_intra_threads(self.threads)
            .with_inter_threads(1)
            // Planning allocations per input shape only grows the memory of
            // a pipeline fed images of every size.
            .with_memory_pattern(false)
            .with_execution_providers(vec![OrtExecutionProvider::CPU])
            // Give the shared arena back what a run took beyond what it
            // still holds (see `install_environment`).
            .with_arena_shrinkage(true);
        session.session_config_entries =
            Some([("session.use_env_allocators".to_string(), "1".to_string())].into());
        // The models as the bytes that were checked, not read again by path.
        OAROCRBuilder::new(self.det.clone(), self.rec.clone(), PathBuf::new())
            .character_dict_content(self.dict.clone())
            .ort_session(session)
            .text_detection_config(det)
            .image_batch_size(1)
            .build()
            .map_err(|e| format!("building the OCR pipeline: {e}"))
    }

    /// Read one pass of a plan, given as the image the detector reads
    /// ([`pass_image`]), within the memory budget: its lines, in the
    /// canvas's pixels, and the canvas handed back.
    fn read_pass(&self, ocr: &OAROCR, pass: &Pass, canvas: image::RgbImage) -> Read {
        let bytes = (pass.det_pixels() as u64).saturating_mul(BYTES_PER_PIXEL);
        let _grant = self.budget.take(bytes);
        lines_of(ocr, canvas)
    }
}

impl OcrEngine for OarEngine {
    fn name(&self) -> &str {
        ENGINE
    }

    fn read(&self, image: &Pixels) -> Result<WordReading, String> {
        let plan = self.sizing.plan(image.width, image.height);
        // oar-ocr reads RGB: a grey image is spread to three channels, and
        // a crop's transparent pixels are composited over white paper.
        let mut img = image::RgbImage::from_raw(image.width, image.height, image.to_rgb())
            .ok_or("the image's buffer does not match its size")?;
        if let Some(f) = plan.shrunk {
            self.notice(&format!(
                "OCR: a {} × {} image is too wide to read in strips of at most {:.1} Mpx; \
                 read shrunk to {:.0} % (set ocr.max_megapixels higher to read it whole)",
                image.width,
                image.height,
                self.sizing.max_pixels() as f64 / 1e6,
                f * 100.0
            ));
            img = image::imageops::resize(
                &img,
                plan.width,
                plan.height,
                image::imageops::FilterType::Triangle,
            );
        }
        let lease = self.pipes.take(|| self.pipeline())?;
        let mut lines = read_upright(
            &plan,
            &mut |pass, canvas| self.read_pass(&lease, pass, canvas),
            img,
        )?;
        drop(lease);
        if plan.shrunk.is_some() {
            let (sx, sy) = (
                f64::from(image.width) / f64::from(plan.width),
                f64::from(image.height) / f64::from(plan.height),
            );
            for l in &mut lines {
                l.quad = l.quad.map(|(x, y)| (x * sx, y * sy));
            }
        }
        let mut words = Vec::new();
        // `lines_from` keeps only the lines that hold text.
        for l in lines {
            let (bbox, angle) = quad_box(l.quad);
            words.push(OcrWord {
                text: l.text,
                bbox,
                angle: Some((angle * 100.0).round() / 100.0),
                conf: l.conf,
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

impl OarEngine {
    fn notice(&self, msg: &str) {
        if let Some(n) = &self.notices {
            n(msg);
        }
    }
}

/// One pass through the pipeline ([`lines_of`]): its lines, in the pixels
/// of the image it was given, and that image handed back.
type Read = Result<(Vec<Line>, std::sync::Arc<image::RgbImage>), String>;

/// What reads one pass of a plan, given the image the detector reads.
type ReadPass<'a> = dyn FnMut(&Pass, image::RgbImage) -> Read + 'a;

/// An image's lines, read by `read` as `plan` says, either way up.
///
/// PP-OCRv6 small has no page orientation classifier, and the pipeline
/// turns every tall line a quarter counter-clockwise before reading it: a
/// sheet fed upside down, and text running up the page, are read as noise,
/// with little confidence. Turned a half, both read right (text running up
/// then runs down), so such a page is read again turned a half, by the same
/// plan (it is the same size), its quads mapped back onto the image as
/// given, and each line is taken from the reading that reads it better.
fn read_upright(
    plan: &OcrPlan,
    read: &mut ReadPass<'_>,
    img: image::RgbImage,
) -> Result<Vec<Line>, String> {
    let (w, h) = (f64::from(img.width()), f64::from(img.height()));
    // The pipeline hands the image back: a page read once is not copied.
    let (mut lines, img) = read_plan(plan, read, img)?;
    if worth_turning(&lines) {
        let (mut turned, _) = read_plan(plan, read, image::imageops::rotate180(&*img))?;
        for l in &mut turned {
            l.quad = l.quad.map(|(x, y)| (w - x, h - y));
        }
        lines = merge(lines, turned);
    }
    Ok(lines)
}

/// An image's lines, read by `read` in `plan`'s passes, their quads in the
/// image's pixels; and the image, handed back by the pipeline when it was
/// read as it is, so a page read once is not copied. Strips' lines are
/// merged ([`merge_line_strips`]).
fn read_plan(
    plan: &OcrPlan,
    read: &mut ReadPass<'_>,
    img: image::RgbImage,
) -> Result<(Vec<Line>, std::sync::Arc<image::RgbImage>), String> {
    let img = match plan.passes.as_slice() {
        [pass] if pass.canvas == img.dimensions() => return read(pass, img),
        _ => std::sync::Arc::new(img),
    };
    if let [pass] = plan.passes.as_slice() {
        // Padded at its right and bottom: the canvas's pixels are the
        // image's.
        let (lines, _) = read(pass, pass_image(&img, pass))?;
        return Ok((lines, img));
    }
    let mut strips = Vec::new();
    for pass in &plan.passes {
        let (mut lines, _) = read(pass, pass_image(&img, pass))?;
        for l in &mut lines {
            l.quad = l.quad.map(|(x, y)| (x, y + f64::from(pass.top)));
        }
        strips.push((pass.top, pass.bottom, lines));
    }
    Ok((merge_line_strips(strips, plan.height), img))
}

/// The image the detector is given for `pass`: its rows of `img`, at the
/// top left of its canvas, the rest white paper.
fn pass_image(img: &image::RgbImage, pass: &Pass) -> image::RgbImage {
    let (w, h) = (img.width(), pass.bottom - pass.top);
    let rows = if (pass.top, h) == (0, img.height()) {
        None
    } else {
        Some(image::imageops::crop_imm(img, 0, pass.top, w, h).to_image())
    };
    if pass.canvas == (w, h) {
        rows.unwrap_or_else(|| img.clone())
    } else {
        let mut c = image::RgbImage::from_pixel(pass.canvas.0, pass.canvas.1, image::Rgb([255; 3]));
        match &rows {
            Some(r) => image::imageops::replace(&mut c, r, 0, 0),
            None => image::imageops::replace(&mut c, img, 0, 0),
        }
        c
    }
}

/// The lines of an image read in strips, as one reading: each tested on its
/// quad's [`Bounds`], so the reading turned a half for a page or a capture
/// read upside down, or whose text runs up it, is merged as the reading as
/// given is.
fn merge_line_strips(read: Vec<(u32, u32, Vec<Line>)>, height: u32) -> Vec<Line> {
    merge_strips(read, height, |l| place(&l.quad))
}

/// How near a strip's cut edge, in pixels, a word's box may reach before it
/// is taken for one the cut went through.
const CUT_EDGE: f64 = 2.0;

/// The rows and columns a detected quad spans, `[x0, y0, x1, y1]` in the
/// image: unlike the word's box, which is set upright about its center, a
/// line running down the image spans its length in rows.
type Bounds = [f64; 4];

/// Where a detected line lies: the rows and columns its quad spans, and its
/// box set upright about its center, at its baseline's angle ([`quad_box`]).
#[derive(Debug, Clone, Copy)]
struct Place {
    bounds: Bounds,
    upright: [f64; 4],
    angle: f64,
}

/// The [`Place`] of a quad.
fn place(quad: &[(f64, f64); 4]) -> Place {
    let (upright, angle) = quad_box(*quad);
    Place {
        bounds: bounds(quad),
        upright,
        angle,
    }
}

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

/// What an image read in strips holds, `(top, bottom, items)` each in the
/// image's rows, as one reading; each item is tested where `place_of` says
/// it lies (for a detected line, its quad's [`Place`]).
///
/// A word reaching a strip's cut edge is cut, and dropped: the strips
/// overlap by more than a line, so the next one holds it whole. A line in
/// an overlap is read by both strips, its boxes a pixel or two apart: of two
/// boxes from different strips covering each other at least half, compared
/// along their baseline ([`same_line`]), the one further inside its strip is
/// kept. A rule by the overlap's middle would
/// lose a line read astride it, its center on the far side in both strips.
///
/// A line running down the image is tested on the rows it spans, so its
/// pieces in two strips are not both kept. One longer than the strips'
/// overlap (a fifth of a strip, 400 rows for a strip of 2,000) is whole in
/// no strip: it is lost, or kept cut short where the detector's box stops a
/// few pixels before the cut. Strips are at least as tall as the image is
/// wide ([`crate::sizing`]), so this is a tall capture's sideways text.
fn merge_strips<T>(
    read: Vec<(u32, u32, Vec<T>)>,
    height: u32,
    place_of: impl Fn(&T) -> Place,
) -> Vec<T> {
    // Each item kept so far: its strip, how far inside it it lies, its place
    // in reading order, and where it lies.
    let mut cands: Vec<(usize, f64, usize, T, Place)> = Vec::new();
    for (si, (top, bottom, words)) in read.into_iter().enumerate() {
        let (top, bottom) = (top as f64, bottom as f64);
        let cut_top = top > 0.0;
        let cut_bottom = bottom < height as f64;
        for w in words {
            let at_place = place_of(&w);
            let b = at_place.bounds;
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
            cands.push((si, inside, at, w, at_place));
        }
    }
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| cands[b].1.total_cmp(&cands[a].1).then(a.cmp(&b)));
    let mut kept: Vec<usize> = Vec::new();
    for i in order {
        let twice = kept
            .iter()
            .any(|&k| cands[k].0 != cands[i].0 && same_line(&cands[k].4, &cands[i].4));
        if !twice {
            kept.push(i);
        }
    }
    kept.sort_unstable();
    let mut out: Vec<Option<T>> = cands.into_iter().map(|c| Some(c.3)).collect();
    kept.into_iter().filter_map(|i| out[i].take()).collect()
}

/// Whether two lines from two strips, where [`Place`] says they lie, are
/// one line read twice.
///
/// Two words running the same way are compared along it: each one's box,
/// set upright about its center ([`quad_box`]), seen in the first's
/// direction, and one line if they share at least half of the smaller. On a
/// page a little askew the rows a long line spans overlap its neighbours'
/// (2,800 px at 2° spans 130 rows, over two lines 55 px apart either side),
/// and compared on those, a line kept from one strip took the next one's
/// copy in the other for itself: one line lost at each seam. Words not
/// running the same way are compared on the rows and columns their quads
/// span.
fn same_line(a: &Place, b: &Place) -> bool {
    if ((a.angle - b.angle + 180.0).rem_euclid(360.0) - 180.0).abs() < SAME_WAY {
        covers_half_along(&a.upright, &b.upright, a.angle)
    } else {
        covers_half(&a.bounds, &b.bounds)
    }
}

/// Most degrees apart two words' baselines may run to be compared along
/// them ([`same_line`]).
const SAME_WAY: f64 = 10.0;

/// Whether two upright boxes, `a` turned `deg` degrees about its center and
/// `b` turned as much about its own, share at least half of the smaller:
/// `b`'s center is taken into `a`'s frame, where both are level.
fn covers_half_along(a: &[f64; 4], b: &[f64; 4], deg: f64) -> bool {
    let (sin, cos) = deg.to_radians().sin_cos();
    let center = |r: &[f64; 4]| ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
    let ((ax, ay), (bx, by)) = (center(a), center(b));
    let (dx, dy) = (bx - ax, by - ay);
    // Along the baseline (up is -y) and across it.
    let (along, across) = (dx * cos - dy * sin, dx * sin + dy * cos);
    let (la, ta) = (a[2] - a[0], a[3] - a[1]);
    let (lb, tb) = (b[2] - b[0], b[3] - b[1]);
    let meet = |half_a: f64, at: f64, half_b: f64| {
        ((half_a).min(at + half_b) - (-half_a).max(at - half_b)).max(0.0)
    };
    let w = meet(la / 2.0, along, lb / 2.0);
    let h = meet(ta / 2.0, across, tb / 2.0);
    w * h >= 0.5 * (la * ta).min(lb * tb)
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

/// A line the pipeline read: its quad, clockwise from its top-left as the
/// image shows it, its text and the recognizer's confidence.
struct Line {
    quad: [(f64, f64); 4],
    text: String,
    conf: Option<f64>,
}

/// One image through the pipeline, as its lines, and the image.
fn lines_of(
    pipeline: &OAROCR,
    img: image::RgbImage,
) -> Result<(Vec<Line>, std::sync::Arc<image::RgbImage>), String> {
    let result = pipeline
        .predict(vec![img])
        .map_err(|e| format!("OCR: {e}"))?
        .pop()
        .ok_or("OCR returned no result")?;
    Ok((lines_from(&result.text_regions), result.input_img))
}

/// The lines of a reading's regions that hold text. A region the recognizer
/// read as nothing, or as spaces, is no line: kept, it would take part in
/// merging strips and readings, where an empty line further inside its
/// strip, or read more confidently, would stand in for the line read over
/// the same place, and then be dropped with it.
fn lines_from(regions: &[oar_ocr::domain::TextRegion]) -> Vec<Line> {
    regions
        .iter()
        .filter_map(|r| {
            let p = &r.bounding_box.points;
            let text = r.text.as_deref().unwrap_or_default();
            (p.len() == 4 && !text.trim().is_empty()).then(|| Line {
                quad: [0, 1, 2, 3].map(|i| (p[i].x as f64, p[i].y as f64)),
                text: text.to_string(),
                conf: r.confidence.map(f64::from),
            })
        })
        .collect()
}

/// Which way a line's quad runs: what decides how the pipeline reads it.
#[derive(Debug, PartialEq)]
enum Run {
    /// At least twice as long as thick: read as it stands.
    Level,
    /// At least [`TALL_LINE`] times as tall as wide: turned a quarter
    /// counter-clockwise before it is read, so text running down the page
    /// reads and text running up it does not.
    Tall,
    /// Neither: a short word, a box of a few characters.
    Square,
}

fn run_of(q: &[(f64, f64); 4]) -> Run {
    let [a, b, _, d] = *q;
    let along = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    let across = ((d.0 - a.0).powi(2) + (d.1 - a.1).powi(2)).sqrt();
    if along >= 2.0 * across.max(1.0) {
        Run::Level
    } else if across >= TALL_LINE * along.max(1.0) {
        Run::Tall
    } else {
        Run::Square
    }
}

/// The confidences of a reading's lines of two characters or more that run
/// `run`: what tells a line read the right way up from one read the wrong
/// way. A short line reads poorly either way. A number read the wrong way
/// up reads as another number, often confidently (`2024` at 0.92, where the
/// words around it read under 0.7), so when at least [`TURN_LINES`] lines
/// hold a letter, only those count: a table of numbers cannot outvote the
/// text of its page. A page with fewer, digits and all, is judged on all
/// its lines.
fn confidences(lines: &[Line], run: Run) -> Vec<f64> {
    let of_run = || {
        lines
            .iter()
            .filter(|l| l.text.trim().chars().count() >= 2 && run_of(&l.quad) == run)
            .filter_map(|l| Some((l.conf?, l.text.chars().any(char::is_alphabetic))))
    };
    let worded: Vec<f64> = of_run().filter(|&(_, w)| w).map(|(c, _)| c).collect();
    if worded.len() >= TURN_LINES {
        worded
    } else {
        of_run().map(|(c, _)| c).collect()
    }
}

/// Whether lines read with these confidences may have been read the wrong
/// way up: at least [`TURN_LINES`] of them, their median under
/// [`UPRIGHT_CONFIDENCE`].
fn poorly_read(mut c: Vec<f64>) -> bool {
    if c.len() < TURN_LINES {
        return false;
    }
    c.sort_by(f64::total_cmp);
    c[c.len() / 2] < UPRIGHT_CONFIDENCE
}

/// Whether a reading is worth reading again turned a half: its level lines
/// read poorly (a sheet upside down), or its tall ones do (text running up
/// the page, whether the page or a table on it). A page read upright, or
/// with its sideways text running down, reads well above the threshold.
fn worth_turning(lines: &[Line]) -> bool {
    poorly_read(confidences(lines, Run::Level)) || poorly_read(confidences(lines, Run::Tall))
}

/// The mean confidence of a reading's level and tall lines.
fn mean_confidence(lines: &[Line]) -> Option<f64> {
    let mut c = confidences(lines, Run::Level);
    c.extend(confidences(lines, Run::Tall));
    (!c.is_empty()).then(|| c.iter().sum::<f64>() / c.len() as f64)
}

/// A reading and the reading of the image turned a half, its quads mapped
/// back, made one. The reading more confident by over [`TURNED_MARGIN`] on
/// the whole is the base, the one given otherwise; a base line is replaced
/// by the other reading's line over the same place when that one is more
/// confident by over the margin, or at all for a tall line of two characters
/// or more. A page upside down is read turned; an upright page holding a
/// table whose text runs up keeps its text and takes the table's from the
/// turned reading.
fn merge(given: Vec<Line>, turned: Vec<Line>) -> Vec<Line> {
    let turned_wins = match (mean_confidence(&turned), mean_confidence(&given)) {
        (Some(t), Some(g)) => t > g + TURNED_MARGIN,
        (Some(_), None) => true,
        _ => false,
    };
    let (mut base, other) = if turned_wins {
        (turned, given)
    } else {
        (given, turned)
    };
    let mut other: Vec<Option<Line>> = other.into_iter().map(Some).collect();
    for line in &mut base {
        let best = other
            .iter()
            .enumerate()
            .filter_map(|(i, o)| o.as_ref().map(|o| (i, overlap(&line.quad, &o.quad))))
            .filter(|&(_, v)| v >= SAME_LINE)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let Some((i, _)) = best else { continue };
        let conf = |l: &Line| l.conf.unwrap_or(0.0);
        // A tall line is read turned a quarter in both readings, its text
        // running down in one and up in the other: neither is the likelier,
        // and the more confident reading of it is taken. A single character
        // reads poorly either way, so it keeps the base's unless clearly
        // beaten.
        let even = |o: &Line| run_of(&line.quad) == Run::Tall && o.text.trim().chars().count() >= 2;
        if other[i].as_ref().is_some_and(|o| {
            let margin = if even(o) { 0.0 } else { TURNED_MARGIN };
            conf(o) > conf(line) + margin
        }) {
            *line = other[i].take().expect("a line not taken yet");
        }
    }
    base
}

/// The overlap of two quads' bounds, over their union.
fn overlap(a: &[(f64, f64); 4], b: &[(f64, f64); 4]) -> f64 {
    let bounds = |q: &[(f64, f64); 4]| {
        q.iter().fold(
            [f64::MAX, f64::MAX, f64::MIN, f64::MIN],
            |[x0, y0, x1, y1], &(x, y)| [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
        )
    };
    let (a, b) = (bounds(a), bounds(b));
    let area = |r: [f64; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
    let inter = area([
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ]);
    let union = area(a) + area(b) - inter;
    if union > 0.0 {
        inter / union
    } else {
        0.0
    }
}

/// Fewest lines of one run that can tell a page was read the wrong way up.
const TURN_LINES: usize = 3;
/// Median line confidence under which a page is read again turned a half.
const UPRIGHT_CONFIDENCE: f64 = 0.8;
/// How much more confident the turned reading, or one of its lines that is
/// not tall, must be.
const TURNED_MARGIN: f64 = 0.15;
/// How much two lines' bounds overlap, over their union, to be one line.
const SAME_LINE: f64 = 0.5;

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
    fn a_region_read_as_nothing_is_no_line() {
        use oar_ocr::domain::TextRegion;
        use oar_ocr::processors::{BoundingBox, Point};
        let quad = |y: f32| {
            BoundingBox::new(
                [(10.0, y), (90.0, y), (90.0, y + 12.0), (10.0, y + 12.0)]
                    .map(|(x, y)| Point::new(x, y))
                    .to_vec(),
            )
        };
        let regions = [
            TextRegion::with_recognition(quad(0.0), Some("Total 12".into()), Some(0.9)),
            // Spaces, read confidently: in an overlap of strips, or against
            // the reading turned a half, it would win over a line read there.
            TextRegion::with_recognition(quad(1.0), Some(" \t".into()), Some(0.99)),
            TextRegion::with_recognition(quad(2.0), Some(String::new().into()), Some(0.99)),
            TextRegion::with_recognition(quad(3.0), None, None),
        ];
        let lines = lines_from(&regions);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Total 12");
        assert_eq!(lines[0].conf, Some(f64::from(0.9f32)));
        assert_eq!(lines[0].quad[2], (90.0, 12.0));
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

    /// A whole reading of `n` level lines `thick` pixels thick, each twenty
    /// times as long.
    fn lines(n: usize, thick: f64) -> Vec<Line> {
        (0..n)
            .map(|i| {
                let (y, len) = (i as f64 * thick * 1.6, 20.0 * thick);
                Line {
                    quad: [
                        (10.0, y),
                        (10.0 + len, y),
                        (10.0 + len, y + thick),
                        (10.0, y + thick),
                    ],
                    text: format!("line {i}"),
                    conf: Some(0.99),
                }
            })
            .collect()
    }

    #[test]
    fn passes_wait_for_the_memory_they_take() {
        let budget = Budget::new(100);
        let a = budget.take(60);
        let b = budget.take(40);
        assert_eq!(budget.held(), 100);
        let waited = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|s| {
            let t = s.spawn(|| {
                let _c = budget.take(10);
                assert!(
                    waited.load(std::sync::atomic::Ordering::SeqCst),
                    "it waited"
                );
            });
            std::thread::sleep(std::time::Duration::from_millis(50));
            waited.store(true, std::sync::atomic::Ordering::SeqCst);
            drop(a);
            t.join().unwrap();
        });
        drop(b);
        assert_eq!(budget.held(), 0);
    }

    #[test]
    fn a_pass_larger_than_the_budget_is_read_alone() {
        let budget = Budget::new(100);
        // Alone: it goes.
        drop(budget.take(500));
        // Behind another: it waits for it, then holds the budget alone.
        let small = budget.take(10);
        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|s| {
            let t = s.spawn(|| {
                let _big = budget.take(500);
                assert!(done.load(std::sync::atomic::Ordering::SeqCst));
                assert_eq!(budget.held(), 500);
            });
            std::thread::sleep(std::time::Duration::from_millis(50));
            done.store(true, std::sync::atomic::Ordering::SeqCst);
            drop(small);
            t.join().unwrap();
        });
    }

    #[test]
    fn passes_are_served_in_turn() {
        // A large pass waiting is not passed over by a small one that
        // would fit beside what is held.
        let budget = Budget::new(100);
        let held = budget.take(60);
        let order = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            let big = s.spawn(|| {
                let _g = budget.take(80);
                order.lock().unwrap().push("big");
            });
            std::thread::sleep(std::time::Duration::from_millis(50));
            let small = s.spawn(|| {
                let _g = budget.take(10);
                order.lock().unwrap().push("small");
            });
            std::thread::sleep(std::time::Duration::from_millis(50));
            assert!(order.lock().unwrap().is_empty(), "both wait");
            drop(held);
            big.join().unwrap();
            small.join().unwrap();
        });
        assert_eq!(order.into_inner().unwrap()[0], "big");
    }

    #[test]
    fn a_grant_is_given_back_when_its_reader_panics() {
        let budget = Budget::new(100);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = budget.take(70);
            panic!("the engine failed");
        }));
        assert!(r.is_err());
        assert_eq!(budget.held(), 0);
        drop(budget.take(100));
    }

    #[test]
    fn a_pass_is_its_rows_padded_with_paper() {
        let img = image::RgbImage::from_fn(30, 100, |_, y| image::Rgb([y as u8; 3]));
        let pass = Pass {
            top: 40,
            bottom: 60,
            canvas: (50, 25),
            up: 2.0,
        };
        let c = pass_image(&img, &pass);
        assert_eq!(c.dimensions(), (50, 25));
        assert_eq!(c.get_pixel(0, 0).0, [40; 3], "its first row at the top");
        assert_eq!(c.get_pixel(29, 19).0, [59; 3]);
        assert_eq!(c.get_pixel(30, 0).0, [255; 3], "paper to the right");
        assert_eq!(c.get_pixel(0, 20).0, [255; 3], "and below");
        // A whole image within its canvas is read as it is.
        let whole = Pass {
            top: 0,
            bottom: 100,
            canvas: (30, 100),
            up: 1.0,
        };
        assert_eq!(pass_image(&img, &whole), img);
    }

    #[test]
    fn a_plan_in_strips_reads_each_and_keeps_a_line_once() {
        let plan = OcrSizing::megapixels(1.0).unwrap().plan(800, 5000);
        assert!(plan.passes.len() > 2, "{plan:?}");
        // Each strip reads one line of its own a quarter down, and the line
        // in its overlap with the next, at the same rows of the image.
        let mut seen = Vec::new();
        let line = |text: String, y: f64| Line {
            quad: [(60.0, y), (400.0, y), (400.0, y + 20.0), (60.0, y + 20.0)],
            text,
            conf: Some(0.9),
        };
        let (lines, _) = read_plan(
            &plan,
            &mut |pass, img| {
                seen.push((pass.top, pass.bottom, img.dimensions()));
                let len = pass.bottom - pass.top;
                let own = line(format!("own {}", pass.top), 300.0);
                // In the strip's pixels: the overlap is its last fifth.
                let shared = line("shared".into(), f64::from(len - len / 10));
                Ok((vec![own, shared], std::sync::Arc::new(img)))
            },
            image::RgbImage::new(800, 5000),
        )
        .unwrap();
        assert_eq!(seen.len(), plan.passes.len());
        assert!(seen.iter().all(|&(t, b, (w, h))| w == 800 && h == b - t));
        let own: Vec<f64> = lines
            .iter()
            .filter(|l| l.text.starts_with("own"))
            .map(|l| l.quad[0].1)
            .collect();
        assert_eq!(own.len(), plan.passes.len(), "every strip's own line");
        assert!(own
            .iter()
            .zip(&plan.passes)
            .all(|(y, p)| *y == f64::from(p.top) + 300.0));
        // A small image is padded, and read once.
        let one = OcrSizing::default().plan(1200, 40);
        let mut sizes = Vec::new();
        let (lines, img) = read_plan(
            &one,
            &mut |_, img| {
                sizes.push(img.dimensions());
                Ok((vec![line("a".into(), 10.0)], std::sync::Arc::new(img)))
            },
            image::RgbImage::new(1200, 40),
        )
        .unwrap();
        assert_eq!(sizes, [(1200, 442)]);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            img.dimensions(),
            (1200, 40),
            "the image as given is handed back"
        );
    }

    fn w(text: &str, y0: f64, y1: f64) -> (OcrWord, Place) {
        let quad = [(60.0, y0), (400.0, y0), (400.0, y1), (60.0, y1)];
        let (bbox, angle) = quad_box(quad);
        let word = OcrWord {
            text: text.into(),
            bbox,
            angle: Some(angle),
            conf: None,
        };
        (word, place(&quad))
    }

    /// A line running down the image from row `y0` to `y1`, 30 px thick.
    fn down(text: &str, x: f64, y0: f64, y1: f64) -> (OcrWord, Place) {
        let quad = [(x, y0), (x + 30.0, y0), (x + 30.0, y1), (x, y1)];
        let (bbox, angle) = quad_box(quad);
        let word = OcrWord {
            text: text.into(),
            bbox,
            angle: Some(angle),
            conf: None,
        };
        (word, place(&quad))
    }

    #[test]
    fn a_line_down_a_tiled_image_is_tested_on_the_rows_it_spans() {
        let texts = |words: Vec<(OcrWord, Place)>| -> Vec<String> {
            words.into_iter().map(|(w, _)| w.text).collect()
        };
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
        assert_eq!(
            texts(merge_strips(read, 3600, |(_, p)| *p)),
            ["ins", "ide", "cut"]
        );
    }

    #[test]
    fn a_reading_s_lines_are_merged_on_the_rows_they_span() {
        // A capture read in strips 0-2000 and 1600-3600, as it is again
        // turned a half: a line running down rows 1650-1950, in the
        // overlap, read whole by the first strip and in two pieces by the
        // second, is kept once; a piece reaching the first strip's bottom
        // edge is dropped for the second's whole line.
        let down = |text: &str, x: f64, y0: f64, y1: f64| Line {
            quad: [(x, y0), (x + 30.0, y0), (x + 30.0, y1), (x, y1)],
            text: text.into(),
            conf: Some(0.9),
        };
        let read = vec![
            (
                0,
                2000,
                vec![
                    down("inside", 100.0, 1650.0, 1950.0),
                    down("cu", 300.0, 1700.0, 2000.0),
                ],
            ),
            (
                1600,
                3600,
                vec![
                    down("ins", 101.0, 1651.0, 1790.0),
                    down("ide", 101.0, 1800.0, 1951.0),
                    down("cut", 300.0, 1700.0, 2100.0),
                ],
            ),
        ];
        let texts: Vec<String> = merge_line_strips(read, 3600)
            .into_iter()
            .map(|l| l.text)
            .collect();
        assert_eq!(texts, ["ins", "ide", "cut"]);
    }

    #[test]
    fn a_line_read_by_two_strips_is_kept_once() {
        let texts = |words: Vec<(OcrWord, Place)>| -> Vec<String> {
            words.into_iter().map(|(w, _)| w.text).collect()
        };
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
            texts(merge_strips(read, 3600, |(_, p)| *p)),
            ["first", "astride", "cut", "second"]
        );
        // Two lines one above the other in one strip are both kept.
        let read = vec![(0, 2000, vec![w("a", 100.0, 120.0), w("b", 110.0, 130.0)])];
        assert_eq!(texts(merge_strips(read, 2000, |(_, p)| *p)), ["a", "b"]);
    }

    /// A line `len` long and `thick` thick about `(cx, cy)`, its baseline
    /// rising `deg` degrees, as the detector draws it.
    fn skewed(text: &str, cx: f64, cy: f64, len: f64, thick: f64, deg: f64) -> (OcrWord, Place) {
        let (sin, cos) = deg.to_radians().sin_cos();
        let (u, n) = ((cos, -sin), (sin, cos));
        let at = |a: f64, b: f64| (cx + u.0 * a + n.0 * b, cy + u.1 * a + n.1 * b);
        let (l, t) = (len / 2.0, thick / 2.0);
        let quad = [at(-l, -t), at(l, -t), at(l, t), at(-l, t)];
        let (bbox, angle) = quad_box(quad);
        let word = OcrWord {
            text: text.into(),
            bbox,
            angle: Some(angle),
            conf: None,
        };
        (word, place(&quad))
    }

    /// A strip's rows and the words it read, each where it lies.
    type Strip = (u32, u32, Vec<(OcrWord, Place)>);

    #[test]
    fn a_skewed_page_read_in_strips_keeps_every_line_once() {
        // A Letter page scanned at 400 dpi, 3,400 × 4,400, a little askew:
        // 75 lines 2,800 px long every 55 px. Read in two strips, each
        // reads the lines it holds whole, the second a pixel off. At 2° a
        // line's rows span 130 px, over two lines either side: the strips'
        // copies must still be told from their neighbours.
        let plan = crate::sizing::OcrSizing::default().plan(3400, 4400);
        assert_eq!(plan.passes.len(), 2, "{plan:?}");
        for deg in [0.0, 1.0, 2.0, 3.0, -2.5] {
            let read: Vec<Strip> = plan
                .passes
                .iter()
                .enumerate()
                .map(|(si, p)| {
                    let (top, bottom) = (f64::from(p.top), f64::from(p.bottom));
                    let words = (0..75)
                        .map(|i| {
                            let jitter = si as f64;
                            let cy = 100.0 + 55.0 * i as f64 + jitter;
                            skewed(&format!("line {i}"), 1700.0 + jitter, cy, 2800.0, 40.0, deg)
                        })
                        .filter(|(_, b)| {
                            (p.top == 0 || b.bounds[1] > top + CUT_EDGE)
                                && (p.bottom == 4400 || b.bounds[3] < bottom - CUT_EDGE)
                        })
                        .collect();
                    (p.top, p.bottom, words)
                })
                .collect();
            let got: Vec<String> = merge_strips(read, 4400, |(_, p)| *p)
                .into_iter()
                .map(|(w, _)| w.text)
                .collect();
            let want: Vec<String> = (0..75).map(|i| format!("line {i}")).collect();
            assert_eq!(got, want, "at {deg}°");
        }
        // A line one strip reads whole and the other in two pieces, askew:
        // kept once, from the strip it lies further inside.
        let rise = 700.0 * 2f64.to_radians().sin();
        let whole = skewed("one two", 1700.0, 1990.0, 2800.0, 40.0, 2.0);
        let one = skewed("one", 1000.0, 1990.0 + rise, 1400.0, 40.0, 2.0);
        let two = skewed("two", 2400.0, 1990.0 - rise, 1400.0, 40.0, 2.0);
        let read = vec![(0, 2100, vec![whole]), (1800, 4400, vec![one, two])];
        let got: Vec<String> = merge_strips(read, 4400, |(_, p)| *p)
            .into_iter()
            .map(|(w, _)| w.text)
            .collect();
        assert_eq!(got, ["one", "two"]);
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

    /// A level line 200 wide and 20 tall at `y`, read with `conf`.
    fn line(y: f64, conf: f64) -> Line {
        Line {
            quad: [(0.0, y), (200.0, y), (200.0, y + 20.0), (0.0, y + 20.0)],
            text: "Account number".into(),
            conf: Some(conf),
        }
    }

    /// A tall line 20 wide and 200 tall at `x`, read with `conf`: text set
    /// sideways, as the detector boxes it.
    fn tall(x: f64, conf: f64) -> Line {
        Line {
            quad: [(x, 0.0), (x + 20.0, 0.0), (x + 20.0, 200.0), (x, 200.0)],
            text: "SKU-60138".into(),
            conf: Some(conf),
        }
    }

    /// Five level lines one under the other, all read with `conf`.
    fn page(conf: f64) -> Vec<Line> {
        (0..5).map(|i| line(30.0 * i as f64, conf)).collect()
    }

    #[test]
    fn a_page_read_with_little_confidence_is_read_again_turned() {
        assert!(!worth_turning(&page(0.97)));
        let noise = page(0.45);
        assert!(worth_turning(&noise));
        // Two lines say too little to turn a page for.
        assert!(!worth_turning(&noise[..2]));
        // Nor do short lines, which read poorly whichever way up they are.
        let mut short = page(0.45);
        short.iter_mut().for_each(|l| l.text = "7".into());
        assert!(!worth_turning(&short));
    }

    #[test]
    fn tall_lines_read_poorly_are_read_again_turned() {
        // Text running down the page reads well: nothing to do.
        let down: Vec<_> = (0..4)
            .map(|i| tall(300.0 + 30.0 * i as f64, 0.95))
            .collect();
        assert!(!worth_turning(&down));
        // Text running up reads as noise, on a page whose level text reads
        // well: the tall lines alone are reason to read it again.
        let mut up = page(0.97);
        up.extend((0..4).map(|i| tall(300.0 + 30.0 * i as f64, 0.4)));
        assert!(worth_turning(&up));
        assert_eq!(run_of(&tall(0.0, 1.0).quad), Run::Tall);
        assert_eq!(run_of(&line(0.0, 1.0).quad), Run::Level);
    }

    #[test]
    fn a_table_of_numbers_does_not_outvote_its_page_s_text() {
        // `text` lines of text read with `words`, over twelve numbers read
        // with `numbers`.
        let page = |text: usize, words: f64, numbers: f64| -> Vec<Line> {
            let mut p: Vec<Line> = (0..text).map(|i| line(30.0 * i as f64, words)).collect();
            p.extend((0..12).map(|i| Line {
                text: "12850".into(),
                ..line(100.0 + 30.0 * i as f64, numbers)
            }));
            p
        };
        // A page upside down: its text reads as noise, its numbers as other
        // numbers, confidently. The numbers make the median; the text alone
        // decides.
        assert!(worth_turning(&page(3, 0.62, 0.85)));
        // Turned, all of it reads right, the numbers hardly more confidently
        // than upside down: the text still makes the turned reading the
        // base, so the numbers are taken from it too.
        let merged = merge(page(3, 0.62, 0.85), page(3, 0.97, 0.95));
        assert!(merged.iter().all(|l| l.conf >= Some(0.95)));
        // Two lines of text are too few to tell: the numbers count as before,
        // and read confidently, the page is not read again.
        assert!(!worth_turning(&page(2, 0.62, 0.85)));
        assert!(worth_turning(&page(2, 0.62, 0.5)));
        // A page of numbers alone is judged on them, as before.
        assert!(!worth_turning(&page(0, 0.0, 0.85)));
        assert!(worth_turning(&page(0, 0.0, 0.5)));
    }

    #[test]
    fn a_page_upside_down_is_read_turned() {
        let merged = merge(page(0.45), page(0.96));
        assert!(merged.iter().all(|l| l.conf == Some(0.96)));
        // A poor scan read upright reads no better turned: it stays.
        let merged = merge(page(0.45), page(0.55));
        assert!(merged.iter().all(|l| l.conf == Some(0.45)));
        let merged = merge(page(0.97), page(0.45));
        assert!(merged.iter().all(|l| l.conf == Some(0.97)));
    }

    #[test]
    fn a_table_running_up_on_an_upright_page_takes_its_lines_turned() {
        // Upright text read well, a table running up read as noise; turned,
        // the other way round.
        let mut given = page(0.97);
        given.extend((0..4).map(|i| tall(300.0 + 30.0 * i as f64, 0.4)));
        let mut turned = page(0.3);
        turned.extend((0..4).map(|i| tall(300.0 + 30.0 * i as f64, 0.95)));
        let merged = merge(given, turned);
        assert_eq!(merged.len(), 9, "one reading of each line");
        for l in &merged {
            let want = if run_of(&l.quad) == Run::Tall {
                0.95
            } else {
                0.97
            };
            assert_eq!(l.conf, Some(want));
        }
    }

    #[test]
    fn a_tall_line_takes_the_more_confident_reading() {
        // A table running up on an upright page: the given reading has one
        // cell cut short but fairly confident, the turned one reads it whole
        // and a little more confident. Both read it turned a quarter, so
        // neither is the likelier: no margin to beat.
        let mut given = page(0.97);
        given.extend((0..4).map(|i| tall(300.0 + 30.0 * i as f64, 0.4)));
        given[5].text = "SKU45".into();
        given[5].conf = Some(0.84);
        let mut turned = page(0.3);
        turned.extend((0..4).map(|i| tall(300.0 + 30.0 * i as f64, 0.95)));
        let merged = merge(given, turned);
        assert_eq!(merged[5].text, "SKU-60138");
        assert_eq!(merged[5].conf, Some(0.95));
        // A single character reads poorly either way: it stays unless beaten
        // by the margin.
        let mut given = page(0.97);
        given.extend((0..4).map(|i| tall(300.0 + 30.0 * i as f64, 0.9)));
        given.iter_mut().skip(5).for_each(|l| l.text = "6".into());
        let mut turned = page(0.3);
        turned.extend((0..4).map(|i| tall(300.0 + 30.0 * i as f64, 0.95)));
        turned.iter_mut().skip(5).for_each(|l| l.text = "9".into());
        let merged = merge(given, turned);
        assert!(merged[5..].iter().all(|l| l.text == "6"));
    }

    /// The heights of the images `read_upright` has the pipeline read, for
    /// an image `width` × `height` under a cap of `mpx`, its lines read
    /// poorly in each.
    fn heights_read(mpx: f64, width: u32, height: u32) -> Vec<u32> {
        let plan = OcrSizing::megapixels(mpx).unwrap().plan(width, height);
        let mut read = Vec::new();
        let mut pipeline = |_: &Pass, img: image::RgbImage| {
            read.push(img.height());
            let poorly = lines(150, 12.0).into_iter().map(|l| Line {
                conf: Some(0.3),
                ..l
            });
            Ok((poorly.collect(), std::sync::Arc::new(img)))
        };
        assert!(
            !read_upright(&plan, &mut pipeline, image::RgbImage::new(width, height))
                .unwrap()
                .is_empty()
        );
        read
    }

    #[test]
    fn a_capture_read_poorly_is_read_turned_as_it_was_planned() {
        // Within the cap: whole, either way up.
        assert_eq!(heights_read(12.5, 1000, 2600), [2600, 2600]);
        // Over it: in the same strips either way up.
        let h = heights_read(1.0, 800, 5000);
        assert!(h.len() > 4 && h.len().is_multiple_of(2), "{h:?}");
        let (given, turned) = h.split_at(h.len() / 2);
        assert_eq!(given, turned);
        assert!(given.iter().all(|&r| r < 5000));
    }

    #[test]
    fn lines_over_the_same_place_overlap() {
        let a = line(0.0, 1.0).quad;
        assert_eq!(overlap(&a, &a), 1.0);
        assert_eq!(overlap(&a, &line(100.0, 1.0).quad), 0.0);
        assert!(overlap(&a, &line(5.0, 1.0).quad) > SAME_LINE);
    }
}
