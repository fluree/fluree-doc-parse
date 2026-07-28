//! `fdoc render` — pages as PNG, in the space the output's coordinates use.

use crate::commands::common::pdfs_in;
use std::path::{Path, PathBuf};

pub(crate) fn run(path: &Path, outdir: &Path, scale: f32, pages: Option<&str>) -> i32 {
    if !(scale.is_finite() && scale > 0.0) {
        eprintln!("error: --scale must be a positive number");
        return 2;
    }
    let keep = match pages.map(crate::commands::convert::parse_pages) {
        Some(Ok(set)) => Some(set),
        Some(Err(e)) => {
            eprintln!("error: --pages: {e}");
            return 2;
        }
        None => None,
    };
    if let Err(e) = std::fs::create_dir_all(outdir) {
        eprintln!("error: {}: {e}", outdir.display());
        return 1;
    }
    let files: Vec<PathBuf> = if path.is_dir() {
        pdfs_in(path)
    } else {
        vec![path.to_path_buf()]
    };
    let (mut written, mut failed) = (0usize, 0usize);
    for f in &files {
        let Ok(data) = std::fs::read(f) else {
            eprintln!("error: cannot read {}", f.display());
            failed += 1;
            continue;
        };
        let Ok(pdf) = hayro_syntax::Pdf::new(std::sync::Arc::new(data)) else {
            eprintln!("error: cannot parse {}", f.display());
            failed += 1;
            continue;
        };
        let stem = crate::commands::common::stem_of(f);
        for index in 0..pdf.pages().len() {
            if keep.as_ref().is_some_and(|k| !k.contains(&index)) {
                continue;
            }
            let Some(png) =
                fluree_doc_pdf::render::page(&pdf, index, scale).and_then(|r| r.to_png())
            else {
                eprintln!(
                    "error: {}: page {} would not render",
                    f.display(),
                    index + 1
                );
                failed += 1;
                continue;
            };
            let dst = outdir.join(format!("{stem}_p{index}.png"));
            if let Err(e) = std::fs::write(&dst, png) {
                eprintln!("error: writing {}: {e}", dst.display());
                failed += 1;
                continue;
            }
            written += 1;
        }
    }
    // The scale is what turns a bbox into a pixel, so it is reported rather
    // than left for the caller to remember.
    println!(
        "{written} page(s) -> {} at {scale}x (multiply a bbox by {scale} for pixels)",
        outdir.display()
    );
    if failed > 0 {
        1
    } else {
        0
    }
}
