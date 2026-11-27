//! A PDF, through `convert`'s pipeline without OCR or a model: glyphs and
//! images extracted, the outline, the layout analysis, links, the notes left
//! on its pages and the files it embeds, and its metadata. Bounded to the
//! first pages, so one input stays one input's work: a few kilobytes can
//! list one page a million times.
#![no_main]
use libfuzzer_sys::fuzz_target;

/// Pages analysed at most.
const MAX_PAGES: usize = 4;

fuzz_target!(|data: &[u8]| {
    fluree_doc_parse_fuzz::reading(data, || {
        let Ok(raw) = fluree_doc_pdf::open(data.to_vec()) else {
            return;
        };
        let outline = fluree_doc_pdf::outline::extract(&raw);
        let _ = fluree_doc_pdf::info::read(&raw);
        let mut budget = fluree_doc_model::budget::Budget::default();
        if let Ok(notes) = fluree_doc_pdf::annot::annotations(&raw, raw.pages().len(), &mut budget) {
            let _ = fluree_doc_pdf::annot::elements(&notes);
        }
        let _ = fluree_doc_pdf::annot::embedded_files(&raw);
        let links = fluree_doc_pdf::link::extract(&raw);
        if raw.pages().len() > MAX_PAGES {
            return;
        }
        // One budget for the pages and the copies of the tables, as
        // `convert` spends it.
        let mut spent = fluree_doc_pdf::PageBudget::default();
        let options = fluree_doc_pdf::ExtractOptions::default();
        let Ok(mut doc) = fluree_doc_pdf::extract_pdf_within(&raw, &options, &mut spent) else {
            return;
        };
        let mut analysis = fluree_doc_pdf::document::analyze(&mut doc, &outline);
        fluree_doc_pdf::link::attach(&mut analysis.elements, &links, &doc.pages);
        if fluree_doc_pdf::spend_table_copies(&mut doc, &mut analysis.elements, &mut spent).is_err()
        {
            return;
        }
        let _ = fluree_doc_pdf::unread_pages(&doc, &analysis.elements);
        let _ = fluree_doc_pdf::unread_regions(&doc, &analysis.elements);
        let _ = fluree_doc_pdf::parse_warnings(&doc);
        fluree_doc_parse_fuzz::check(&analysis.elements);
    });
});
