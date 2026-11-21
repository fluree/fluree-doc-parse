//! A PDF, through `convert`'s pipeline without OCR or a model: glyphs and
//! images extracted, the outline, the layout analysis, links, the notes left
//! on its pages and the files it embeds, and its metadata. Bounded to the
//! first pages, so one input stays one input's work: a few kilobytes can
//! list one page a million times.
#![no_main]
use libfuzzer_sys::fuzz_target;
use std::sync::Arc;

/// Pages analysed at most.
const MAX_PAGES: usize = 4;

fuzz_target!(|data: &[u8]| {
    fluree_doc_parse_fuzz::reading(data, || {
        let Ok(raw) = hayro_syntax::Pdf::new(Arc::new(data.to_vec())) else {
            return;
        };
        let outline = fluree_doc_pdf::outline::extract(&raw);
        let _ = fluree_doc_pdf::info::read(&raw);
        let mut budget = fluree_doc_model::budget::Budget::default();
        if let Ok(notes) = fluree_doc_pdf::annot::annotations(&raw, &mut budget) {
            let _ = fluree_doc_pdf::annot::elements(&notes);
        }
        let _ = fluree_doc_pdf::annot::embedded_files(&raw);
        let links = fluree_doc_pdf::link::extract(&raw);
        if raw.pages().len() > MAX_PAGES {
            return;
        }
        let Ok(mut doc) = fluree_doc_pdf::extract_bytes(data.to_vec()) else {
            return;
        };
        let mut analysis = fluree_doc_pdf::document::analyze(&mut doc, &outline);
        fluree_doc_pdf::link::attach(&mut analysis.elements, &links, &doc.pages);
        let _ = fluree_doc_pdf::unread_pages(&doc, &analysis.elements);
        let _ = fluree_doc_pdf::unread_regions(&doc, &analysis.elements);
        let _ = fluree_doc_pdf::parse_warnings(&doc);
        fluree_doc_parse_fuzz::check(&analysis.elements);
    });
});
