//! Content detection: the reader `convert` sends a file to, by its content
//! and its name. The first byte picks the name from the extensions the CLI
//! knows (or none), the rest is the file.
#![no_main]
use libfuzzer_sys::fuzz_target;

const NAMES: &[&str] = &[
    "f",
    "f.pdf",
    "f.csv",
    "f.tsv",
    "f.txt",
    "f.xls",
    "f.xlsx",
    "f.xlsb",
    "f.doc",
    "f.docx",
    "f.ppt",
    "f.pptx",
    "f.odt",
    "f.ods",
    "f.odp",
    "f.rtf",
    "f.xps",
    "f.oxps",
    "f.eml",
    "f.msg",
    "f.vtt",
    "f.srt",
    "f.html",
    "f.md",
    "f.png",
    "f.jpg",
    "f.tif",
    "f.pages",
    "f.key",
    "f.numbers",
    "f.mht",
    "f.PDF",
];

fuzz_target!(|data: &[u8]| {
    let Some((&pick, data)) = data.split_first() else {
        return;
    };
    let name = NAMES[pick as usize % NAMES.len()];
    let _ = fluree_doc_cli::detect(name, data);
});
