//! A Rich Text Format document.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    fluree_doc_parse_fuzz::reading(data, || {
        let _ = fluree_doc_rtf::is_rtf(data);
        if let Ok((elements, _info)) = fluree_doc_rtf::parse_with_info(data) {
            fluree_doc_parse_fuzz::check(&elements);
        }
    });
});
