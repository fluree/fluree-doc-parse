//! The supported library API, end to end.
//!
//! Doubles as a compile check for the snippets in
//! `docs/getting-started/rust-library.md` — if this stops compiling, that page
//! is lying.
//!
//! ```text
//! cargo run --release --example library_usage -- path/to/document.pdf [ocr-words.json]
//! ```
//!
//! The optional second argument is an `--ocr-words` sidecar reading of the
//! first page (see `docs/integration/sidecar-formats.md`).

use fluree_doc_model::{to_markdown, to_xhtml};
use fluree_doc_pdf::{doco, document, extract_pdf, ocr, outline, route};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: library_usage <pdf>")?;
    let data = std::fs::read(&path)?;

    // The outline (bookmark tree) is a near-ground-truth heading signal, but it
    // comes from the PDF object graph rather than the content stream, so it is
    // read separately and handed to `analyze`. Pass `&[]` to skip it.
    // Opened once, its page tree measured first (`open`), for both.
    let pdf = fluree_doc_pdf::open(data)?;
    let ol = outline::extract(&pdf);

    let mut doc = extract_pdf(&pdf)?;

    // Escalation verdicts, before any structure work.
    for (i, page) in doc.pages.iter().enumerate() {
        let (verdict, signals) = route::decide(page);
        println!(
            "p{i}\t{verdict:?}\tglyphs={}\tunicode={:.3}",
            signals.glyphs, signals.unicode_rate
        );
    }

    // OCR words, where an engine read the page's pixels: glyphs before the
    // analysis, taken only where the router sends a reader.
    let injected = match std::env::args().nth(2) {
        Some(words) => {
            let reading = ocr::WordReading::from_json(&std::fs::read_to_string(words)?)?;
            ocr::inject(&mut doc, &[(0, ocr::Crop::Page, reading)])
        }
        None => Vec::new(),
    };

    let mut analysis = document::analyze(&mut doc, &ol);
    ocr::mark_provenance(&mut analysis.elements, &injected);

    for e in analysis.elements.iter().take(5) {
        println!("{}\tp{}\t{}", e.kind, e.page, e.text);
    }

    // Every emitter is a projection of the same elements.
    let _md = to_markdown(&analysis.elements);
    let _xhtml = to_xhtml(&analysis.elements);

    // `to_text` and `to_doco` are a pair: the graph's nif:beginIndex /
    // nif:endIndex are character offsets into exactly this string.
    let text = doco::to_text(&analysis.elements);
    let graph = doco::to_doco(
        &analysis.elements,
        &doco::DocoOptions {
            base_iri: "urn:fluree-doc-parse:example".into(),
            doc_iri: None,
            ..Default::default()
        },
    );

    // Both emitters return serialized strings, ready to write or POST.
    println!(
        "\n{} chars of text, {} bytes of JSON-LD",
        text.chars().count(),
        graph.len()
    );
    Ok(())
}
