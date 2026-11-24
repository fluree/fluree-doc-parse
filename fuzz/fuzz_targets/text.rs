//! A plain text file, as `convert` reads one: decoded, told Markdown or
//! not by its markup, and read as Markdown or as lines.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    fluree_doc_parse_fuzz::reading(data, || {
        let text = fluree_doc_text::decode(data);
        // Past the document's budget, a file is refused as too large.
        let read = if fluree_doc_text::is_markdown(&text) {
            fluree_doc_markdown::parse(&text)
        } else {
            fluree_doc_text::parse(&text)
        };
        if let Ok(elements) = read {
            fluree_doc_parse_fuzz::check(&elements);
        }
    });
});
