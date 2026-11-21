//! An e-mail (`.eml` or `.msg`), and the files it carries, read down
//! through attached messages as `convert --parse-attachments` reads them:
//! each attachment converted by the reader `convert` hands it to, with no
//! OCR and no model, its first four pages at most.
#![no_main]
use fluree_doc_email::{parse_attachments, AttachedFile, AttachmentLimits};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(email) = fluree_doc_email::parse(data) else {
        return;
    };
    let notes = email.notes();
    fluree_doc_parse_fuzz::check_with(&email.elements, &notes);
    for m in email.media.iter().filter_map(|m| m.source.part()) {
        let _ = email.media_bytes(m);
        let _ = email.media_attachment(m);
    }
    // Only the length of each conversion is kept, so the files a message
    // carries are converted one at a time.
    let mut read = |f: &AttachedFile, _depth: usize| {
        fluree_doc_cli::convert_attachment(f).map(|r| r.map(|out| out.len()))
    };
    let _ = parse_attachments(&email, &AttachmentLimits::default(), &mut read);
});
