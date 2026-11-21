//! An Excel binary workbook (`.xlsb`).
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = fluree_doc_xls::is_xlsb(data);
    if let Ok((elements, _info)) = fluree_doc_xls::parse_xlsb(data) {
        fluree_doc_parse_fuzz::check(&elements);
    }
});
