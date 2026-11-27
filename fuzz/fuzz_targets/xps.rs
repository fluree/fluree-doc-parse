//! An XPS or OpenXPS document: its pages read as a PDF's are, through the
//! same analysis, and the pictures on them read out of the package.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    fluree_doc_parse_fuzz::reading(data, || {
        let _ = fluree_doc_xps::is_xps(data);
        let Ok(xps) = fluree_doc_xps::parse(data) else {
            return;
        };
        for image in xps.images.iter().take(8) {
            let _ = fluree_doc_xps::read_image(data, &image.part);
        }
        for p in &xps.doc.pages {
            let _ = fluree_doc_pdf::route::decide(p);
            let _ = xps.page_image(p.index);
        }
        let mut doc = xps.doc.clone();
        let mut analysis = fluree_doc_pdf::document::analyze(&mut doc, &[]);
        let mut budget = fluree_doc_pdf::PageBudget::default();
        if fluree_doc_pdf::spend_table_copies(&mut doc, &mut analysis.elements, &mut budget)
            .is_err()
        {
            return;
        }
        let _ = fluree_doc_pdf::unread_pages(&doc, &analysis.elements);
        fluree_doc_parse_fuzz::check(&analysis.elements);
    });
});
