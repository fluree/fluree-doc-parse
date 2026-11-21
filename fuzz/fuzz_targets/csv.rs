//! A delimited file: the delimiter sniffed, as for `.csv`, or tab, as for
//! `.tsv`, chosen by the input's first byte.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let tab = data.first().is_some_and(|b| b & 1 == 1);
    let data = data.get(1..).unwrap_or_default();
    if let Ok(elements) = fluree_doc_csv::parse_with(data, tab.then_some('\t')) {
        fluree_doc_parse_fuzz::check(&elements);
    }
});
