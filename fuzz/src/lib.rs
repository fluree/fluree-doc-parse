//! What every fuzz target checks of what a reader made: that it fits the
//! budget one document may spend, and that every output renders it.
//!
//! A reader that returns an error has refused the file, which is fine; one
//! that panics, aborts, hangs or runs out of memory is a defect, and
//! libFuzzer reports those itself (`-timeout`, `-rss_limit_mb`).

use fluree_doc_model::budget::{
    converting_to, filled_cell_bytes, filled_csv_cell_bytes, output, text_cost, Output, CELL_BYTES,
    FLAT_CELL_BYTES, MAX_MEMORY, TEXT_BYTES,
};
use fluree_doc_model::doco::table_copies;
use fluree_doc_model::{DocoOptions, Element, Notes};

/// The output `data` is read for, as `fdoc convert -f` names it: DoCO, the
/// text, or Markdown, XHTML and JSON (`Output::Flat`), picked by the XOR of
/// its bytes. A seed is read as it is written, and a mutation of any byte
/// picks another output about two times in three, so over a campaign each input
/// is tried at each output's costs: the flat outputs admit the most table
/// cells (no copies of them, a `.csv` record's cheaper), DoCO the most
/// copies.
pub fn output_of(data: &[u8]) -> Output {
    match data.iter().fold(0u8, |x, b| x ^ b) % 3 {
        0 => Output::Doco,
        1 => Output::Flat,
        _ => Output::Text,
    }
}

/// Run `read` as `fdoc convert` reads `data` for the output its bytes pick
/// ([`output_of`]): each budget a reader makes meanwhile counts at that
/// output's costs (`fluree_doc_model::budget::converting_to`), and
/// [`check`] estimates and renders at it.
pub fn reading<R>(data: &[u8], read: impl FnOnce() -> R) -> R {
    converting_to(output_of(data), read)
}

/// The memory `elements` stand for at the budget's costs for `output`
/// (`fluree_doc_model::budget`): an element at the output's element cost,
/// each cell slot at `CELL_BYTES`, a cell that holds text at what every
/// reader spends for one (`filled_cell_bytes`, its text included; a
/// `.csv` record's at `filled_csv_cell_bytes`), each
/// other byte of text at `TEXT_BYTES` times what it may take in the worst
/// output. A table's text is its cells' (the cost of a byte covers the
/// copies made of it), so it is counted once, through its cells, and then
/// the copies the output writes of them
/// (`fluree_doc_model::doco::table_copies`): for DoCO, a merged value in
/// each cell it covers, each cell's labels; for the text, a value merged
/// down on each row; for the others, none.
pub fn estimated_memory(elements: &[Element], output: Output) -> u64 {
    let element = output.element_bytes();
    let mut cost = 0u64;
    for e in elements {
        let copies = table_copies(e);
        let copied = match output {
            Output::Doco => copies
                .cells
                .saturating_mul(FLAT_CELL_BYTES)
                .saturating_add(copies.text.saturating_mul(TEXT_BYTES)),
            Output::Text => copies.merged.saturating_mul(TEXT_BYTES),
            Output::Flat => 0,
        };
        cost = cost.saturating_add(element).saturating_add(copied);
        if e.cells.is_none() {
            cost = cost.saturating_add(text_cost(&e.text).saturating_mul(TEXT_BYTES));
        }
        // A `.csv` record's cell costs less than another table's: its
        // reader keeps nothing of it but its text.
        let filled = if e.provenance == "csv" {
            filled_csv_cell_bytes
        } else {
            filled_cell_bytes
        };
        for row in e.cells.iter().flatten() {
            for cell in row {
                cost = cost
                    .saturating_add(CELL_BYTES)
                    .saturating_add(filled(output, cell));
            }
        }
    }
    cost
}

/// Assert `elements` fit one document's budget, at the costs of the output
/// they are read for ([`reading`]; DoCO's outside it), then render them
/// through that output, as `convert` would: through every output for DoCO,
/// whose costs are above every other's.
pub fn check(elements: &[Element]) {
    check_with(elements, &Notes::default());
}

/// [`check`], with the notes a reader left beside its elements.
pub fn check_with(elements: &[Element], notes: &Notes) {
    let output = output();
    let cost = estimated_memory(elements, output);
    assert!(
        cost <= MAX_MEMORY,
        "{} elements stand for {cost} bytes as {output:?}, past the {MAX_MEMORY} a document may take",
        elements.len()
    );
    if output != Output::Text {
        let _ = fluree_doc_model::to_markdown_with(elements, notes);
        let _ = fluree_doc_model::to_xhtml_with(elements, notes);
        let _ = serde_json::to_string_pretty(elements);
    }
    if output != Output::Flat {
        let _ = fluree_doc_model::to_text(elements);
    }
    if output == Output::Doco {
        // Written as `fdoc` writes it, a node at a time, never held whole.
        let _ = fluree_doc_model::write_doco(elements, &DocoOptions::default(), std::io::sink());
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use fluree_doc_model::budget::within;

    #[test]
    fn the_oracle_counts_a_table_as_its_reader_does() {
        // 8,000 records of ten one-digit cells: what the CSV reader needs
        // is what the oracle counts, at each output's costs, so a table a
        // reader accepts never fails the check.
        let csv = format!("{}\n", ["1"; 10].join(",")).repeat(8_000);
        for output in [Output::Flat, Output::Text] {
            let read = |memory| {
                within(memory, || {
                    converting_to(output, || fluree_doc_csv::parse(csv.as_bytes()))
                })
            };
            let elements = read(MAX_MEMORY).unwrap();
            let need = estimated_memory(&elements, output);
            assert!(read(need).is_ok(), "{output:?}");
            assert!(read(need - 1).is_err(), "{output:?}");
        }
    }
}
