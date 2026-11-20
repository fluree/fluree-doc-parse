//! A library that does not load leaves nothing loaded. Its own test binary,
//! so its process has loaded nothing before.
#![cfg(feature = "oar")]

use fluree_doc_vision::assets::{platform, Assets, DET, DICT, REC};
use fluree_doc_vision::oar::{OarEngine, OarOptions};
use sha2::{Digest, Sha256};

#[test]
fn a_failed_load_names_the_library_and_leaves_nothing_loaded() {
    // Pinned files with matching hashes, the library being no library.
    let dir = std::env::temp_dir().join(format!("fdoc-runtime-load-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (rid, lib) = platform().expect("a shipped platform");
    let runtime = format!("onnxruntime/{rid}/{lib}");
    let mut pins = Vec::new();
    for rel in [runtime.as_str(), DET, REC, DICT] {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, rel.as_bytes()).unwrap();
        let hex: String = Sha256::digest(rel.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        pins.push((rel.to_string(), hex));
    }
    let pins: Vec<(&str, &str)> = pins.iter().map(|(p, h)| (p.as_str(), h.as_str())).collect();
    let open = || {
        let assets = Assets::open_pinned(&dir, &pins).expect("the files are the pinned ones");
        OarEngine::open_assets(assets, &OarOptions::default())
            .err()
            .expect("no library")
    };
    let first = open();
    assert!(first.contains(lib), "names the library: {first}");
    // Nothing was loaded, so a second open tries again rather than reporting
    // a library already in place.
    let second = open();
    assert!(!second.contains("already loaded"), "{second}");
    assert_eq!(first, second);
    let _ = std::fs::remove_dir_all(dir);
}
