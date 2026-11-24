//! `fdoc ocr`, and the engine `fdoc convert --ocr` reads with.
//!
//! The engine is opened once per invocation (its models are read and
//! optimized, which takes a while) and shared by every document and thread;
//! it keeps a pipeline per image being read at once.

use crate::cli::{OcrArgs, OcrEngineName};
use fluree_doc_vision::OcrEngine;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The manifest reasons whose crops a page takes words from. A doubt page
/// has all its text, and a table crop asks about structure.
pub(crate) const OCR_REASONS: [&str; 5] = [
    "scanned",
    "near_blank",
    "broken_text",
    "region",
    "small_region",
];

thread_local! {
    /// The document this thread reads, for the engine's warnings: one engine
    /// serves every document of a run, and reads each on its caller's
    /// thread.
    static READING: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// While it lives, what the engine says about a reading on this thread
/// names `doc`. Dropped, the thread names what it named before.
pub(crate) struct Reading(Option<String>);

impl Reading {
    pub(crate) fn of(doc: &Path) -> Reading {
        Reading(READING.with(|r| r.replace(Some(doc.display().to_string()))))
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        READING.with(|r| *r.borrow_mut() = self.0.take());
    }
}

/// A warning from the engine, with the document this thread reads.
#[cfg(feature = "ocr")]
fn engine_warning(msg: &str) -> String {
    READING.with(|r| match &*r.borrow() {
        Some(doc) => format!("warning: {doc}: {msg}"),
        None => format!("warning: {msg}"),
    })
}

/// How the engine runs, as the command line, the environment and the config
/// set it. A build without the engine still checks the settings, so a bad
/// value is refused alike, but keeps none of them.
pub(crate) struct EngineSettings {
    #[cfg(feature = "ocr")]
    pub threads: usize,
    #[cfg(feature = "ocr")]
    pub sizing: fluree_doc_vision::sizing::OcrSizing,
    /// Bytes the passes read at once may take; the engine's default if
    /// none.
    #[cfg(feature = "ocr")]
    pub memory: Option<u64>,
}

impl EngineSettings {
    /// `ocr.threads`; the pixel cap a pass reads, `flag` over
    /// `FDOC_OCR_MAX_MEGAPIXELS` over `ocr.max_megapixels` over the default;
    /// and the memory budget, `FDOC_OCR_MAX_MEMORY` over `ocr.max_memory`
    /// (MiB) over the engine's default.
    pub(crate) fn of(flag: Option<f64>, config: &crate::config::Ocr) -> Result<Self, String> {
        let var = |name: &str| std::env::var(name).ok().filter(|s| !s.trim().is_empty());
        let memory =
            match var("FDOC_OCR_MAX_MEMORY") {
                Some(v) => Some(v.trim().parse::<u64>().map_err(|_| {
                    format!("FDOC_OCR_MAX_MEMORY={v:?} is not a whole number of MiB")
                })?),
                None => config.max_memory,
            };
        if memory == Some(0) {
            return Err("ocr.max_memory: 0 MiB leaves the engine nothing to read with".into());
        }
        let env = var("FDOC_OCR_MAX_MEGAPIXELS");
        let mpx = match (flag, env) {
            (Some(m), _) => m,
            (None, Some(v)) => v.trim().parse::<f64>().map_err(|_| {
                format!("FDOC_OCR_MAX_MEGAPIXELS={v:?} is not a number of megapixels")
            })?,
            (None, None) => config
                .max_megapixels
                .unwrap_or(fluree_doc_vision::sizing::DEFAULT_MAX_MEGAPIXELS),
        };
        let sizing = fluree_doc_vision::sizing::OcrSizing::megapixels(mpx)
            .map_err(|e| format!("ocr.max_megapixels: {e}"))?;
        #[cfg(not(feature = "ocr"))]
        let _ = sizing;
        Ok(EngineSettings {
            #[cfg(feature = "ocr")]
            threads: config.threads.unwrap_or(1),
            #[cfg(feature = "ocr")]
            sizing,
            #[cfg(feature = "ocr")]
            memory: memory.map(|mib| mib.saturating_mul(1 << 20)),
        })
    }
}

/// Open an engine on an assets directory. What it has to say about a
/// reading (an image shrunk to fit) goes to stderr as a warning.
#[cfg(feature = "ocr")]
pub(crate) fn open_engine(
    name: OcrEngineName,
    models: &Path,
    settings: &EngineSettings,
) -> Result<Arc<dyn OcrEngine>, String> {
    match name {
        OcrEngineName::OarOcr => {
            let defaults = fluree_doc_vision::oar::OarOptions::default();
            let opts = fluree_doc_vision::oar::OarOptions {
                threads: settings.threads,
                sizing: settings.sizing,
                memory: settings.memory.unwrap_or(defaults.memory),
                notices: Some(Arc::new(|msg: &str| eprintln!("{}", engine_warning(msg)))),
                ..defaults
            };
            let engine = fluree_doc_vision::oar::OarEngine::open(models, &opts)?;
            Ok(Arc::new(engine))
        }
    }
}

#[cfg(not(feature = "ocr"))]
pub(crate) fn open_engine(
    _name: OcrEngineName,
    _models: &Path,
    _settings: &EngineSettings,
) -> Result<Arc<dyn OcrEngine>, String> {
    Err("this build has no OCR engine (built without the `ocr` feature); use --ocr-words".into())
}

/// The engine `convert` reads with, if any: `--ocr` over `FDOC_OCR` over the
/// config's `ocr.engine`, and likewise `--models`, `FDOC_OCR_MODELS`,
/// `ocr.models`. An engine named without its assets is an error, not a
/// silent deterministic run.
pub(crate) fn engine_for_convert(
    flag: Option<OcrEngineName>,
    models: Option<&Path>,
    max_megapixels: Option<f64>,
    config: &crate::config::Ocr,
) -> Result<Option<Arc<dyn OcrEngine>>, String> {
    let named = match flag {
        Some(n) => Some(n),
        // An empty engine or assets variable is an unset one: the config's
        // setting, if any, applies.
        None => match std::env::var("FDOC_OCR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| config.engine.clone())
        {
            None => None,
            Some(s) => Some(parse_engine(&s)?),
        },
    };
    let Some(name) = named else {
        return Ok(None);
    };
    let dir = models_dir(models, config)
        .ok_or("an OCR engine needs its assets: --models DIR (or FDOC_OCR_MODELS, or ocr.models in the config)")?;
    let settings = EngineSettings::of(max_megapixels, config)?;
    open_engine(name, &dir, &settings).map(Some)
}

fn parse_engine(s: &str) -> Result<OcrEngineName, String> {
    match s {
        "oar-ocr" => Ok(OcrEngineName::OarOcr),
        other => Err(format!(
            "OCR engine {other:?} is not one this build knows (oar-ocr)"
        )),
    }
}

fn models_dir(flag: Option<&Path>, config: &crate::config::Ocr) -> Option<PathBuf> {
    flag.map(Path::to_path_buf)
        .or_else(|| {
            std::env::var_os("FDOC_OCR_MODELS")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| config.models.clone())
}

/// The images to read: a crops directory's entries whose reason a page
/// takes words from, or the files given.
fn images(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for input in inputs {
        if !input.is_dir() {
            out.push(input.clone());
            continue;
        }
        let manifest = input.join("manifest.jsonl");
        let text = std::fs::read_to_string(&manifest).map_err(|e| {
            format!(
                "{}: {e} (a directory is read through the manifest `fdoc dev render-routed` writes)",
                manifest.display()
            )
        })?;
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let rec: serde_json::Value = serde_json::from_str(line)
                .map_err(|e| format!("{} line {}: {e}", manifest.display(), n + 1))?;
            let Some(png) = rec["png"].as_str() else {
                continue;
            };
            if rec["reason"]
                .as_str()
                .is_some_and(|r| OCR_REASONS.contains(&r))
            {
                let path = input.join(png);
                if out.contains(&path) {
                    return Err(format!(
                        "{}: lists {png} twice (two inputs of one name cropped into one directory?)",
                        manifest.display()
                    ));
                }
                out.push(path);
            }
        }
    }
    Ok(out)
}

/// Each image's words go to `<stem>.json`; two images of one stem (`a.png`
/// and `a.jpg`, or one name in two directories) would write one file.
fn distinct_outputs(images: &[PathBuf]) -> Result<(), String> {
    let mut seen: std::collections::HashMap<&std::ffi::OsStr, &Path> = Default::default();
    for path in images {
        let stem = path
            .file_stem()
            .ok_or_else(|| format!("{}: no file name", path.display()))?;
        if let Some(other) = seen.insert(stem, path) {
            return Err(format!(
                "{} and {} would both be read into {}.json",
                other.display(),
                path.display(),
                stem.to_string_lossy()
            ));
        }
    }
    Ok(())
}

pub fn run(args: &OcrArgs, verbose: bool, quiet: bool) -> i32 {
    let config = match crate::config::load() {
        Ok(l) => l.config,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let Some(dir) = models_dir(args.models.as_deref(), &config.ocr) else {
        eprintln!(
            "error: --models DIR is required (or FDOC_OCR_MODELS, or ocr.models in the config)"
        );
        return 2;
    };
    let images = match images(&args.inputs) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if let Err(e) = distinct_outputs(&images) {
        eprintln!("error: {e}");
        return 2;
    }
    // The assets before anything is written: a run that cannot read leaves
    // no empty output directory behind.
    let engine = match EngineSettings::of(args.max_megapixels, &config.ocr)
        .and_then(|settings| open_engine(args.engine, &dir, &settings))
    {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&args.out) {
        eprintln!("error: {}: {e}", args.out.display());
        return 1;
    }
    let workers = match args.jobs {
        0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
        n => n,
    }
    .min(images.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failed = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(path) = images.get(i) else { break };
                match read_one(engine.as_ref(), path, &args.out) {
                    Ok((name, words)) if verbose => eprintln!("{name}: {words} words"),
                    Ok(_) => {}
                    Err(e) => {
                        failed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        eprintln!("error: {e}");
                    }
                }
            });
        }
    });
    let failed = failed.into_inner();
    if !quiet {
        eprintln!(
            "{} image(s) read into {}{}",
            images.len() - failed,
            args.out.display(),
            if failed > 0 {
                format!(", {failed} failed")
            } else {
                String::new()
            }
        );
    }
    i32::from(failed > 0)
}

#[cfg(feature = "ocr")]
fn read_one(engine: &dyn OcrEngine, path: &Path, out: &Path) -> Result<(String, usize), String> {
    let _doc = Reading::of(path);
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let image = fluree_doc_image::decode(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if image.more_pages {
        eprintln!(
            "warning: {}: only the first page of a multi-page image is read",
            path.display()
        );
    }
    if image.truncated {
        eprintln!(
            "warning: {}: the file ends before its image does; part of it may be blank",
            path.display()
        );
    }
    let reading = engine
        .read(&image.pixels)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("{}: no file name", path.display()))?;
    let target = out.join(format!("{stem}.json"));
    std::fs::write(&target, reading.to_json()).map_err(|e| format!("{}: {e}", target.display()))?;
    Ok((stem.to_string(), reading.words.len()))
}

#[cfg(not(feature = "ocr"))]
fn read_one(_: &dyn OcrEngine, _: &Path, _: &Path) -> Result<(String, usize), String> {
    unreachable!("no engine opens without the `ocr` feature")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "ocr")]
    fn an_engine_warning_names_the_document_its_thread_reads() {
        assert_eq!(engine_warning("shrunk"), "warning: shrunk");
        {
            let _a = Reading::of(Path::new("a.pdf"));
            assert_eq!(engine_warning("shrunk"), "warning: a.pdf: shrunk");
            {
                // An attachment read inside its message names itself, and
                // the message again once it is done.
                let _b = Reading::of(Path::new("b.png"));
                assert_eq!(engine_warning("shrunk"), "warning: b.png: shrunk");
            }
            assert_eq!(engine_warning("shrunk"), "warning: a.pdf: shrunk");
            // Another thread reads another document.
            std::thread::scope(|s| {
                s.spawn(|| assert_eq!(engine_warning("x"), "warning: x"));
            });
        }
        assert_eq!(engine_warning("shrunk"), "warning: shrunk");
    }

    #[test]
    fn a_crops_directory_is_read_through_its_reasons() {
        let dir = std::env::temp_dir().join(format!("fdoc-ocr-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lines = [
            r#"{"doc":"a","kind":"page","page":0,"png":"a_p0_full.png","reason":"scanned"}"#,
            r#"{"doc":"a","kind":"page","page":1,"png":"a_p1_full.png","reason":"heading_doubt"}"#,
            r#"{"doc":"a","kind":"region","page":2,"png":"a_p2_r0.png","reason":"region"}"#,
            r#"{"doc":"a","kind":"region","page":2,"png":"a_p2_r1.png","reason":"small_region"}"#,
            r#"{"doc":"a","kind":"region","page":2,"png":"a_p2_t0.png","reason":"table"}"#,
        ];
        std::fs::write(dir.join("manifest.jsonl"), lines.join("\n")).unwrap();
        let got: Vec<String> = images(std::slice::from_ref(&dir))
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(got, ["a_p0_full.png", "a_p2_r0.png", "a_p2_r1.png"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_manifest_naming_a_crop_twice_is_refused_by_name() {
        let dir = std::env::temp_dir().join(format!("fdoc-ocr-twice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let line = r#"{"doc":"a","kind":"page","page":0,"png":"a_p0_full.png","reason":"scanned"}"#;
        std::fs::write(dir.join("manifest.jsonl"), format!("{line}\n{line}\n")).unwrap();
        let e = images(std::slice::from_ref(&dir)).unwrap_err();
        assert!(e.contains("lists a_p0_full.png twice"), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn two_images_of_one_stem_are_refused() {
        let ok = [PathBuf::from("a/x.png"), PathBuf::from("a/y.png")];
        assert!(distinct_outputs(&ok).is_ok());
        let clash = [PathBuf::from("a/x.png"), PathBuf::from("b/x.jpg")];
        let e = distinct_outputs(&clash).unwrap_err();
        assert!(e.contains("x.json"), "{e}");
    }

    #[test]
    #[cfg(feature = "ocr")]
    fn the_pixel_cap_comes_from_the_flag_then_the_config() {
        if std::env::var_os("FDOC_OCR_MAX_MEGAPIXELS").is_some() {
            return;
        }
        let mut config = crate::config::Ocr::default();
        let px = |flag, config: &crate::config::Ocr| {
            EngineSettings::of(flag, config).map(|s| s.sizing.max_pixels())
        };
        assert_eq!(px(None, &config), Ok(12_500_000));
        config.max_megapixels = Some(8.0);
        assert_eq!(px(None, &config), Ok(8_000_000));
        assert_eq!(px(Some(20.0), &config), Ok(20_000_000));
        let e = px(Some(0.1), &config).unwrap_err();
        assert!(e.starts_with("ocr.max_megapixels: "), "{e}");
        if std::env::var_os("FDOC_OCR_MAX_MEMORY").is_none() {
            config.max_memory = Some(2048);
            let s = EngineSettings::of(None, &config).unwrap();
            assert_eq!(s.memory, Some(2 << 30));
            config.max_memory = Some(0);
            assert!(EngineSettings::of(None, &config).is_err());
        }
    }

    #[test]
    fn a_build_without_the_engine_still_refuses_bad_settings() {
        if std::env::var_os("FDOC_OCR_MAX_MEGAPIXELS").is_some()
            || std::env::var_os("FDOC_OCR_MAX_MEMORY").is_some()
        {
            return;
        }
        let mut config = crate::config::Ocr::default();
        assert!(EngineSettings::of(None, &config).is_ok());
        let e = EngineSettings::of(Some(0.1), &config).err().unwrap();
        assert!(e.starts_with("ocr.max_megapixels: "), "{e}");
        config.max_memory = Some(0);
        assert!(EngineSettings::of(None, &config).is_err());
    }

    #[test]
    fn an_engine_without_assets_is_an_error() {
        let config = crate::config::Ocr::default();
        if std::env::var_os("FDOC_OCR_MODELS").is_some() {
            return;
        }
        let e = engine_for_convert(Some(OcrEngineName::OarOcr), None, None, &config)
            .err()
            .unwrap();
        assert!(e.contains("--models"), "{e}");
        assert!(parse_engine("tesseract").is_err());
    }
}
