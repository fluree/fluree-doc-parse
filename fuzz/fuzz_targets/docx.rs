//! A Word document (`.docx`), with its notes and comments, and the pictures
//! it lists.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok((elements, _info, media)) = fluree_doc_docx::parse_with_media(data) {
        fluree_doc_parse_fuzz::check(&elements);
        // The package opened once for its pictures, as `convert` opens it.
        if let Ok(mut pkg) = fluree_doc_docx::Package::open(data) {
            fluree_doc_parse_fuzz::read_media(&media, 8, |part| {
                fluree_doc_docx::read_media(&mut pkg, part)
            });
        }
    }
});
