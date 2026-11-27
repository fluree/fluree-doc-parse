//! An Excel 97-2003 workbook (BIFF8 in a compound file).
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    fluree_doc_parse_fuzz::reading(data, || {
        let _ = fluree_doc_xls::is_xls(data);
        if let Ok((elements, _info)) = fluree_doc_xls::parse_with_info(data) {
            fluree_doc_parse_fuzz::check(&elements);
        }
    });
});
