//! A Markdown file, which `convert` reads only as UTF-8.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    fluree_doc_parse_fuzz::reading(data, || {
        // Past the document's budget, a file is refused as too large.
        if let Ok(Ok(elements)) = std::str::from_utf8(data).map(fluree_doc_markdown::parse) {
            fluree_doc_parse_fuzz::check(&elements);
        }
    });
});
