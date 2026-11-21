//! What every fuzz target checks of what a reader made: that it fits the
//! budget one document may spend, and that every output renders it.
//!
//! A reader that returns an error has refused the file, which is fine; one
//! that panics, aborts, hangs or runs out of memory is a defect, and
//! libFuzzer reports those itself (`-timeout`, `-rss_limit_mb`).

use fluree_doc_model::budget::{text_cost, CELL_BYTES, ELEMENT_BYTES, MAX_MEMORY, TEXT_BYTES};
use fluree_doc_model::doco::table_copies;
use fluree_doc_model::{DocoOptions, Element, Notes};

/// The memory `elements` stand for at the budget's costs
/// (`fluree_doc_model::budget`): an element, or a cell holding text, at
/// `ELEMENT_BYTES`, each cell slot at `CELL_BYTES`, each byte of text at
/// `TEXT_BYTES` times what it may take in the worst output. A table's text
/// is its cells' (the cost of a byte covers the copies made of it), so it
/// is counted once, through its cells, and then the copies DoCO writes of
/// them (`fluree_doc_model::doco::table_copies`): a merged value in each
/// cell it covers, each cell's labels.
pub fn estimated_memory(elements: &[Element]) -> u64 {
    let mut cost = 0u64;
    for e in elements {
        let copies = table_copies(e);
        cost = cost
            .saturating_add(ELEMENT_BYTES)
            .saturating_add(copies.cells.saturating_mul(ELEMENT_BYTES))
            .saturating_add(copies.text.saturating_mul(TEXT_BYTES));
        if e.cells.is_none() {
            cost = cost.saturating_add(text_cost(&e.text).saturating_mul(TEXT_BYTES));
        }
        for row in e.cells.iter().flatten() {
            for cell in row {
                cost = cost.saturating_add(CELL_BYTES);
                if !cell.is_empty() {
                    cost = cost
                        .saturating_add(ELEMENT_BYTES)
                        .saturating_add(text_cost(cell).saturating_mul(TEXT_BYTES));
                }
            }
        }
    }
    cost
}

/// Assert `elements` fit one document's budget, then render them through
/// every output, as `convert` would.
pub fn check(elements: &[Element]) {
    check_with(elements, &Notes::default());
}

/// [`check`], with the notes a reader left beside its elements.
pub fn check_with(elements: &[Element], notes: &Notes) {
    let cost = estimated_memory(elements);
    assert!(
        cost <= MAX_MEMORY,
        "{} elements stand for {cost} bytes, past the {MAX_MEMORY} a document may take",
        elements.len()
    );
    let _ = fluree_doc_model::to_markdown_with(elements, notes);
    let _ = fluree_doc_model::to_xhtml_with(elements, notes);
    let _ = fluree_doc_model::to_text(elements);
    let _ = fluree_doc_model::to_doco(elements, &DocoOptions::default());
}

/// Each picture a reader listed, read through `read` as `convert` reads it:
/// once however many figures draw it, and only one the file holds, at most
/// the first `max` of them (so one input stays one input's work).
pub fn read_media<F>(media: &[fluree_doc_model::Media], max: usize, mut read: F)
where
    F: FnMut(&str) -> Result<Vec<u8>, String>,
{
    let pictures = fluree_doc_model::pictures(media.to_vec());
    for part in pictures.iter().filter_map(|p| p.source.part()).take(max) {
        let _ = read(part);
    }
}
