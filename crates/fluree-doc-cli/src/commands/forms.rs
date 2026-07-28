//! `fdoc forms` — AcroForm fields as JSON: the filled-in values the glyph
//! pass never sees.
//!
//! PDF-only by nature: an AcroForm is a PDF construct, and the structural
//! formats have no equivalent. Handed anything else this says so and exits
//! non-zero, rather than panicking on the parse.

use std::path::Path;

pub fn run(pdf: &Path) -> i32 {
    let data = match std::fs::read(pdf) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", pdf.display());
            return 1;
        }
    };
    let Ok(raw) = hayro_syntax::Pdf::new(std::sync::Arc::new(data)) else {
        eprintln!(
            "error: {} is not a readable PDF — `forms` reads AcroForm fields, \
             which only PDFs have",
            pdf.display()
        );
        return 1;
    };
    let fields = fluree_doc_pdf::forms::fields(&raw);
    println!("{}", serde_json::to_string_pretty(&fields).unwrap());
    0
}
