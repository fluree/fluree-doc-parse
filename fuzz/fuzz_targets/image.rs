//! A bare image (PNG, JPEG, TIFF): its header, as `convert` reads it with
//! no OCR; and decoded, its words read by a stand-in engine (no OCR
//! models) and laid out as a scanned page's are.
#![no_main]
use fluree_doc_vision::{Coords, OcrEngine, OcrWord, Pixels, WordReading};
use libfuzzer_sys::fuzz_target;

/// Reads a word in each band of 32 rows, four across, as many as sixteen
/// lines: enough to lay out, whatever the image shows.
struct Bands;

impl OcrEngine for Bands {
    fn name(&self) -> &str {
        "bands"
    }
    fn read(&self, image: &Pixels) -> Result<WordReading, String> {
        let (w, h) = (f64::from(image.width), f64::from(image.height));
        let lines = (image.height / 32).clamp(1, 16);
        let words = (0..lines)
            .flat_map(|l| {
                (0..4).map(move |k| OcrWord {
                    text: format!("w{l}x{k}"),
                    bbox: [
                        w * f64::from(k) / 4.0,
                        h * f64::from(l) / f64::from(lines),
                        w * f64::from(k + 1) / 4.0 - 1.0,
                        h * f64::from(l + 1) / f64::from(lines) - 1.0,
                    ],
                    angle: None,
                    conf: Some(0.9),
                })
            })
            .collect();
        Ok(WordReading {
            coords: Coords::Px,
            width: Some(w),
            height: Some(h),
            engine: Some("bands".into()),
            words,
        })
    }
}

fuzz_target!(|data: &[u8]| {
    if fluree_doc_pdf::image::Format::sniff(data).is_none() {
        return;
    }
    let _ = fluree_doc_pdf::image::dimensions(data);
    let _ = fluree_doc_image::header(data);
    // The decoder's bound on pixels is scaled as the budget is
    // (`fluree_doc_model::budget::scaled`), so an image within it stays
    // within the fuzzer's memory limit.
    fluree_doc_parse_fuzz::reading(data, || {
        if let Ok(read) = fluree_doc_image::elements(data, &Bands) {
            fluree_doc_parse_fuzz::check(&read.elements);
        }
    });
});
