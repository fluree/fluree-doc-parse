//! A PowerPoint presentation (`.pptx`).
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    fluree_doc_parse_fuzz::reading(data, || {
        if let Ok((elements, _info)) = fluree_doc_pptx::parse_with_info(data) {
            fluree_doc_parse_fuzz::check(&elements);
        }
    });
});
