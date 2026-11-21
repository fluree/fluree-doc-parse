//! An e-mail (`.eml` or `.msg`), and the files it carries, read down
//! through attached messages as `convert --parse-attachments` reads them,
//! each attachment handed to a stub reader that runs content detection on
//! it as `convert` does before choosing a reader.
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
    let mut read = |f: &AttachedFile, _depth: usize| {
        let name = f.info.filename.as_deref().unwrap_or("attachment");
        let (kind, _) = fluree_doc_cli::detect(name, &f.bytes);
        (kind != "Unsupported").then_some(Ok::<usize, String>(f.bytes.len()))
    };
    let _ = parse_attachments(&email, &AttachmentLimits::default(), &mut read);
});
