//! An e-mail (`.eml` or `.msg`), and the files it carries, read down
//! through attached messages as `convert --parse-attachments` reads them:
//! each attachment converted by the reader `convert` hands it to, within
//! what the message leaves of the budget, with no OCR and no model, its
//! first four pages at most.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    fluree_doc_parse_fuzz::reading(data, || {
        let Ok(email) = fluree_doc_email::parse(data) else {
            return;
        };
        let notes = email.notes();
        fluree_doc_parse_fuzz::check_with(&email.elements, &notes);
        for m in email.media.iter().filter_map(|m| m.source.part()) {
            let _ = email.media_bytes(m);
            let _ = email.media_attachment(m);
        }
        // Each conversion is dropped once made, so the files a message
        // carries are converted one at a time, as `convert` writes each.
        fluree_doc_cli::convert_attachments(&email, drop);
    });
});
