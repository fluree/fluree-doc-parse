//! The OCR assets directory: the models and the ONNX Runtime library, each
//! checked against the SHA-256 this build was made with before it is loaded.
//!
//! ```text
//! <dir>/
//!   SHA256SUMS                          every file below, `<hex>  <relative path>`
//!   onnxruntime/<platform>/<library>    linux-x64/libonnxruntime.so, osx-arm64/libonnxruntime.dylib, …
//!   pp-ocrv6-small/det.onnx             text detection
//!   pp-ocrv6-small/rec.onnx             text recognition
//!   pp-ocrv6-small/dict.txt             the recognizer's characters
//! ```
//!
//! The library is native code loaded into the process, and the models decide
//! what text comes out, so neither is used unless its hash is the one pinned
//! here ([`PINNED`]). A list kept beside the files would only say they are the
//! files the list was written for: anyone able to swap a file can rewrite
//! its list. `SHA256SUMS` stays in the directory for a person or another
//! tool to check by; this check does not read it. A directory holds the
//! library for every platform it is shipped to; only the running platform's
//! is read.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The list of hashes, at the top of the directory.
pub const SUMS: &str = "SHA256SUMS";

/// The files this build loads, and their SHA-256: the asset pack
/// `tools/ocr-assets/build.py` makes (PP-OCRv6 small, ONNX Runtime 1.28.0).
/// The engine's settings were measured on these models; another pack is
/// another build. `build.py` prints this table for a pack it makes.
pub const PINNED: &[(&str, &str)] = &[
    (
        "onnxruntime/linux-arm64/libonnxruntime.so",
        "f1ec1a08eb99bd6e5401340f0a2b101381bf4694415480291dc13bcaa30f9ec7",
    ),
    (
        "onnxruntime/linux-x64/libonnxruntime.so",
        "1461ef7cc3d9e49982591721683cc3e3a55580aeca9a5254e7aac47b75ee4bab",
    ),
    (
        "onnxruntime/osx-arm64/libonnxruntime.dylib",
        "244e53902258550e4b3afc3231c3cf8ba860173fc47a1fb572082ed822e6973f",
    ),
    (
        "onnxruntime/win-arm64/onnxruntime.dll",
        "102342326311ad62879b808840b4e3fd1372c41d8c871b59536d89a3c64d53f2",
    ),
    (
        "onnxruntime/win-x64/onnxruntime.dll",
        "18370c375f07357fa5874344a9d9ac17e6b6fe1eb18b1dd209d79483b4470257",
    ),
    (
        "pp-ocrv6-small/det.onnx",
        "d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e",
    ),
    (
        "pp-ocrv6-small/dict.txt",
        "b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d",
    ),
    (
        "pp-ocrv6-small/rec.onnx",
        "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634",
    ),
];

/// The models, relative to the directory.
pub const DET: &str = "pp-ocrv6-small/det.onnx";
pub const REC: &str = "pp-ocrv6-small/rec.onnx";
pub const DICT: &str = "pp-ocrv6-small/dict.txt";

/// The running platform's name for its ONNX Runtime library, as NuGet's
/// `runtimes/<rid>/native` names it, and the library's file name; `None` on
/// a platform no build of ONNX Runtime is shipped for.
pub fn platform() -> Option<(&'static str, &'static str)> {
    let rid = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "aarch64") => "osx-arm64",
        ("windows", "x86_64") => "win-x64",
        ("windows", "aarch64") => "win-arm64",
        _ => return None,
    };
    let lib = match std::env::consts::OS {
        "linux" => "libonnxruntime.so",
        "macos" => "libonnxruntime.dylib",
        _ => "onnxruntime.dll",
    };
    Some((rid, lib))
}

/// The files the engine loads, checked.
///
/// What is checked is what is used, with nothing to swap in between: the
/// models and the dictionary are read once and handed over as the bytes that
/// were hashed, and the library is hashed through a file kept open, which on
/// Linux is the very file loaded ([`Runtime::load_path`]). Elsewhere it is
/// loaded again by its path, so its directory must be no more writable than
/// the binary is.
#[derive(Debug)]
pub struct Assets {
    pub runtime: Runtime,
    pub det: Vec<u8>,
    pub rec: Vec<u8>,
    pub dict: String,
}

/// The ONNX Runtime library, checked and held open.
#[derive(Debug)]
pub struct Runtime {
    /// Where it is, absolute.
    pub path: PathBuf,
    file: std::fs::File,
}

impl Runtime {
    /// The path to load the library by: the file this check hashed. On
    /// Linux it is the open file itself, so a file put in its place since
    /// the check is not the one loaded.
    pub fn load_path(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = &self.file;
            self.path.clone()
        }
    }
}

impl Assets {
    /// Check the files the running platform needs against [`PINNED`].
    pub fn open(dir: &Path) -> Result<Assets, String> {
        Self::open_pinned(dir, PINNED)
    }

    /// Check the files the running platform needs against `pins`,
    /// `(relative path, SHA-256 hex)`: for a caller that ships assets of its
    /// own and vouches for them.
    ///
    /// Fails, naming the file, when one is missing, not pinned, or does not
    /// hash to its pinned value. A relative `dir` is taken from the working
    /// directory, as every path given to `fdoc` is.
    pub fn open_pinned(dir: &Path, pins: &[(&str, &str)]) -> Result<Assets, String> {
        let (rid, lib) = platform().ok_or_else(|| {
            format!(
                "no ONNX Runtime is shipped for {}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        })?;
        // Absolute, so that nothing further down resolves it another way: a
        // library loader given a relative path may look beside the binary
        // first, and find a file this check never saw.
        let dir = std::path::absolute(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let checked = |rel: &str, got: String| -> Result<(), String> {
            let want = pins
                .iter()
                .find(|(p, _)| *p == rel)
                .map(|(_, h)| h.to_ascii_lowercase())
                .ok_or_else(|| format!("{rel} is not one of the files this build loads"))?;
            if got != want {
                return Err(format!(
                    "{} is not the file this build loads: sha256 {got}, expected {want}",
                    dir.join(rel).display()
                ));
            }
            Ok(())
        };
        let read = |rel: &str| -> Result<Vec<u8>, String> {
            let path = dir.join(rel);
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            checked(rel, hex(&Sha256::digest(&bytes)))?;
            Ok(bytes)
        };
        let runtime = {
            let rel = format!("onnxruntime/{rid}/{lib}");
            let path = dir.join(&rel);
            let mut file =
                std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            checked(&rel, sha256(&mut file, &path)?)?;
            Runtime { path, file }
        };
        let det = read(DET)?;
        let rec = read(REC)?;
        let dict = String::from_utf8(read(DICT)?)
            .map_err(|_| format!("{}: not UTF-8", dir.join(DICT).display()))?;
        Ok(Assets {
            runtime,
            det,
            rec,
            dict,
        })
    }
}

fn sha256(f: &mut std::fs::File, path: &Path) -> Result<String, String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_pin_is_the_hash_build_py_writes_for_its_file() {
        // The pack's builder holds the same eight hashes, in `RUNTIMES` (an
        // output path, then its hash) and `MODELS` (`path: hash`): a pin
        // changed in one place and not the other is a build that refuses
        // every pack it can be given.
        let build = include_str!("../../../tools/ocr-assets/build.py");
        for (path, hash) in PINNED {
            let quoted = format!("\"{path}\"");
            let at = build
                .find(&quoted)
                .unwrap_or_else(|| panic!("build.py does not write {path}"));
            let after = build[at + quoted.len()..]
                .trim_start_matches(|c: char| c == ',' || c == ':' || c.is_whitespace());
            assert!(
                after.starts_with(&format!("\"{hash}\"")),
                "build.py pins {path} to another hash than {hash}"
            );
        }
    }

    /// A directory holding the four files for this platform, each holding
    /// its own path, and the pins they match.
    fn fixture(test: &str, tamper: Option<&str>) -> (PathBuf, Vec<(String, String)>) {
        let dir = std::env::temp_dir().join(format!("fdoc-assets-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (rid, lib) = platform().expect("a shipped platform");
        let runtime = format!("onnxruntime/{rid}/{lib}");
        let mut pins = Vec::new();
        for rel in [runtime.as_str(), DET, REC, DICT] {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, rel.as_bytes()).unwrap();
            pins.push((rel.to_string(), hex(&Sha256::digest(rel.as_bytes()))));
            if tamper == Some(rel) {
                std::fs::write(&path, b"changed").unwrap();
            }
        }
        (dir, pins)
    }

    fn open(dir: &Path, pins: &[(String, String)]) -> Result<Assets, String> {
        let pins: Vec<(&str, &str)> = pins.iter().map(|(p, h)| (p.as_str(), h.as_str())).collect();
        Assets::open_pinned(dir, &pins)
    }

    #[test]
    fn listed_files_that_match_open() {
        let (dir, pins) = fixture("ok", None);
        let a = open(&dir, &pins).unwrap();
        assert_eq!(a.det, DET.as_bytes());
        assert_eq!(a.dict, DICT);
        assert!(a.runtime.path.starts_with(dir.join("onnxruntime")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_relative_dir_is_made_absolute() {
        // A relative path the library loader could resolve beside the binary
        // instead: the check makes it absolute, from the working directory.
        let (dir, pins) = fixture("relative", None);
        let rel = pathdiff(&dir, &std::env::current_dir().unwrap());
        assert!(rel.is_relative());
        let a = open(&rel, &pins).unwrap();
        assert!(a.runtime.path.is_absolute(), "{}", a.runtime.path.display());
        assert!(a.runtime.load_path().is_absolute());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_library_loaded_is_the_file_checked() {
        // The checked file swapped for another after the check: the path to
        // load still reads the one that was hashed.
        let (dir, pins) = fixture("swapped", None);
        let a = open(&dir, &pins).unwrap();
        let (rid, lib) = platform().unwrap();
        let path = dir.join(format!("onnxruntime/{rid}/{lib}"));
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"another library").unwrap();
        let loaded = std::fs::read(a.runtime.load_path()).unwrap();
        assert_eq!(loaded, format!("onnxruntime/{rid}/{lib}").as_bytes());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `path` relative to `base`, through `..` as far as needed.
    fn pathdiff(path: &Path, base: &Path) -> PathBuf {
        let (p, b): (Vec<_>, Vec<_>) = (path.components().collect(), base.components().collect());
        let common = p.iter().zip(&b).take_while(|(x, y)| x == y).count();
        let mut out = PathBuf::new();
        for _ in common..b.len() {
            out.push("..");
        }
        for c in &p[common..] {
            out.push(c);
        }
        out
    }

    #[test]
    fn a_changed_model_is_refused() {
        let (dir, pins) = fixture("changed", Some(REC));
        let e = open(&dir, &pins).unwrap_err();
        assert!(
            e.contains("rec.onnx") && e.contains("is not the file this build loads"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_not_pinned_is_refused() {
        let (dir, pins) = fixture("unpinned", None);
        let kept: Vec<(String, String)> = pins.into_iter().filter(|(p, _)| p != DICT).collect();
        let e = open(&dir, &kept).unwrap_err();
        assert!(e.contains("dict.txt is not one of the files"), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_pins_cover_what_every_platform_loads() {
        let pinned = |rel: &str| PINNED.iter().any(|(p, h)| *p == rel && h.len() == 64);
        for rel in [DET, REC, DICT] {
            assert!(pinned(rel), "{rel}");
        }
        for rid in [
            "linux-x64",
            "linux-arm64",
            "osx-arm64",
            "win-x64",
            "win-arm64",
        ] {
            assert!(
                PINNED
                    .iter()
                    .any(|(p, _)| p.starts_with(&format!("onnxruntime/{rid}/"))),
                "{rid}"
            );
        }
    }
}
