//! A PowerPoint 97-2003 presentation, and the pictures it lists.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = fluree_doc_ppt::is_ppt(data);
    if let Ok((elements, media)) = fluree_doc_ppt::parse_with_media(data) {
        fluree_doc_parse_fuzz::check(&elements);
        // The deck opened once for its pictures, as `convert` opens it.
        if let Ok(deck) = fluree_doc_ppt::MediaReader::open(data) {
            fluree_doc_parse_fuzz::read_media(&media, 8, |part| deck.read(part));
        }
    }
});
