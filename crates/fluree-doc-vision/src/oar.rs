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
//! across its width, whose lines are kept once ([`merge_strips`]). Each word
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
    /// ([`pass_image`]), within the memory budget.
    fn read_pass(
        &self,
        ocr: &OAROCR,
        canvas: image::RgbImage,
        pass: &Pass,
    ) -> Result<Words, String> {
        let bytes = (pass.det_pixels() as u64).saturating_mul(BYTES_PER_PIXEL);
        let _grant = self.budget.take(bytes);
        read_one(ocr, canvas, pass.top)
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
        let mut words = match plan.passes.as_slice() {
            // Read as it is: the image is given to the detector, not copied.
            [pass] if pass.canvas == img.dimensions() => self
                .read_pass(&lease, img, pass)?
                .into_iter()
                .map(|(w, _)| w)
                .collect(),
            _ => read_plan(&plan, &mut |pass| {
                self.read_pass(&lease, pass_image(&img, pass), pass)
            })?,
        };
        drop(lease);
        if plan.shrunk.is_some() {
            let (sx, sy) = (
                f64::from(image.width) / f64::from(plan.width),
                f64::from(image.height) / f64::from(plan.height),
            );
            for w in &mut words {
                w.bbox = [
                    w.bbox[0] * sx,
                    w.bbox[1] * sy,
                    w.bbox[2] * sx,
                    w.bbox[3] * sy,
                ];
            }
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

/// The words of an image read by `plan`, each pass read by `read`: one
/// pass's words as they are, strips' merged ([`merge_strips`]).
fn read_plan(
    plan: &OcrPlan,
    read: &mut dyn FnMut(&Pass) -> Result<Words, String>,
) -> Result<Vec<OcrWord>, String> {
    if let [whole] = plan.passes.as_slice() {
        return Ok(read(whole)?.into_iter().map(|(w, _)| w).collect());
    }
    let mut strips = Vec::new();
    for pass in &plan.passes {
        strips.push((pass.top, pass.bottom, read(pass)?));
    }
    Ok(merge_strips(strips, plan.height))
}

/// Read one image, its rows starting at `dy` in the image the words are
/// given for: each word, with its quad's bounds (see [`Bounds`]).
fn read_one(ocr: &OAROCR, img: image::RgbImage, dy: u32) -> Result<Words, String> {
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
type StripReading = (u32, u32, Words);

/// One pass's words, each with its quad's [`Bounds`].
type Words = Vec<(OcrWord, Bounds)>;

/// The words of an image read in strips, `(top, bottom, words)` each in
/// the image's rows, as one reading; each word comes with its quad's
/// [`Bounds`], on which the cut-edge and duplicate tests below are made.
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
            .any(|&k| cands[k].0 != cands[i].0 && same_line(&cands[k], &cands[i]));
        if !twice {
            kept.push(i);
        }
    }
    kept.sort_unstable();
    let mut out: Vec<Option<OcrWord>> = cands.into_iter().map(|c| Some(c.3)).collect();
    kept.into_iter().filter_map(|i| out[i].take()).collect()
}

/// Whether two words, each with its quad's [`Bounds`], from two strips are
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
fn same_line(
    a: &(usize, f64, usize, OcrWord, Bounds),
    b: &(usize, f64, usize, OcrWord, Bounds),
) -> bool {
    let (wa, wb) = (&a.3, &b.3);
    match (wa.angle, wb.angle) {
        (Some(x), Some(y)) if ((x - y + 180.0).rem_euclid(360.0) - 180.0).abs() < SAME_WAY => {
            covers_half_along(&wa.bbox, &wb.bbox, x)
        }
        _ => covers_half(&a.4, &b.4),
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
        // Each strip reads one line at its own top quarter, and the line
        // in the overlap with the next, at the same rows in the image.
        let mut seen = Vec::new();
        let words = read_plan(&plan, &mut |pass| {
            seen.push((pass.top, pass.bottom));
            let mid = pass.bottom - (pass.bottom - pass.top) / 10;
            Ok(vec![
                w(
                    &format!("own {}", pass.top),
                    f64::from(pass.top) + 300.0,
                    f64::from(pass.top) + 320.0,
                ),
                w("shared", f64::from(mid), f64::from(mid) + 20.0),
            ])
        })
        .unwrap();
        assert_eq!(seen.len(), plan.passes.len());
        let own = words.iter().filter(|w| w.text.starts_with("own")).count();
        assert_eq!(own, plan.passes.len(), "every strip's own line");
        // One whole plan: the words as read.
        let one = OcrSizing::default().plan(800, 600);
        let words = read_plan(&one, &mut |_| Ok(vec![w("a", 10.0, 30.0)])).unwrap();
        assert_eq!(words.len(), 1);
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

    /// A line `len` long and `thick` thick about `(cx, cy)`, its baseline
    /// rising `deg` degrees, as the detector draws it.
    fn skewed(text: &str, cx: f64, cy: f64, len: f64, thick: f64, deg: f64) -> (OcrWord, Bounds) {
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
        (word, bounds(&quad))
    }

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
            let read: Vec<StripReading> = plan
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
                            (p.top == 0 || b[1] > top + CUT_EDGE)
                                && (p.bottom == 4400 || b[3] < bottom - CUT_EDGE)
                        })
                        .collect();
                    (p.top, p.bottom, words)
                })
                .collect();
            let got: Vec<String> = merge_strips(read, 4400)
                .into_iter()
                .map(|w| w.text)
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
        let got: Vec<String> = merge_strips(read, 4400)
            .into_iter()
            .map(|w| w.text)
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
}
