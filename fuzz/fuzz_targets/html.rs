//! A web page, its bytes decoded as `convert` decodes them: its elements and
//! what its head declares; and as an e-mail's HTML body, with the cited
//! quotations around each element and the message's own images.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    fluree_doc_parse_fuzz::reading(data, || {
        let text = fluree_doc_html::decode(data);
        if let Ok(elements) = fluree_doc_html::parse(&text) {
            let _ = fluree_doc_html::info(&text);
            fluree_doc_parse_fuzz::check(&elements);
        }
        if let Ok((elements, depths)) = fluree_doc_html::parse_with_quote_depth(&text) {
            assert_eq!(elements.len(), depths.len());
            fluree_doc_parse_fuzz::check(&elements);
        }
    });
});
