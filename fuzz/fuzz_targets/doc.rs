//! A Word 97-2003 document.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = fluree_doc_doc::is_doc(data);
    if let Ok(elements) = fluree_doc_doc::parse(data) {
        fluree_doc_parse_fuzz::check(&elements);
    }
});
