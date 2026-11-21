//! An OpenDocument text, spreadsheet or presentation (`.odt`, `.ods`,
//! `.odp`): one entry, which the package's `mimetype` sends to its reader.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    fluree_doc_parse_fuzz::reading(data, || {
        let _ = fluree_doc_odf::Format::sniff(data);
        if let Ok((elements, _info, _format)) = fluree_doc_odf::parse_with_info(data) {
            fluree_doc_parse_fuzz::check(&elements);
        }
    });
});
