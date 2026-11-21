//! What a reader of untrusted input may make of one document.
//!
//! A reader's bounds on its input (bytes read, parts inflated) do not bound
//! what it makes of it: a cell repeated, a string shared by many cells, a
//! record named twice, pieces that all point at the same text each copy
//! again, and a layout step can cost the square of what it lays out. So a
//! reader is handed one [`Budget`] for the document and spends it *before*
//! it allocates or loops, and stops with a [`TooLarge`] error once it is
//! spent. An error here is the reader refusing the file; it never aborts
//! the process.
//!
//! The budget holds two counts:
//!
//! * **Memory**, in estimated bytes, which three kinds of spending share:
//!   an element made, or a table cell that holds text (DoCO writes each as
//!   a node of its own), costs [`ELEMENT_BYTES`]; a cell slot allocated
//!   (read, repeated, merged or padded, by a reader or by a layout, each
//!   copy) costs [`CELL_BYTES`]; and a byte of text costs [`TEXT_BYTES`]
//!   for each byte it may take in the worst output ([`text_cost`]: a
//!   control character is six, `\u0001` in JSON). Each cost is above the
//!   peak memory one unit was measured to take, from the reader through the
//!   largest output (`doco`), so a document that spends the whole of
//!   [`MAX_MEMORY`], however it mixes them, stays within a 4 GB address
//!   space, with room for its input and a library's own copies.
//! * **Work**, in steps of a layout pass whose cost the file chooses (two
//!   islands of cells compared, a glyph checked against those it may
//!   overprint), at most [`MAX_WORK`]: a few seconds.
//!
//! A reader that guards a library's allocations it cannot see (what
//! calamine reserves for a sheet) keeps its own [`Meter`] for them.

/// The memory a document's elements, cells and text may take, estimated
/// at the costs below: 2.5 GiB.
#[cfg(not(fuzzing))]
pub const MAX_MEMORY: u64 = 5 << 29;

/// Built for the fuzz targets (`fuzz/`), a sixteenth of it, 160 MiB: an
/// input that spends the whole budget then takes a fraction of a second
/// rather than seconds, and one that makes more than the budget sees stands
/// out under the targets' memory limit, rather than hiding under the room a
/// full budget needs.
#[cfg(fuzzing)]
pub const MAX_MEMORY: u64 = 5 << 25;

/// The steps of the layout passes whose cost a file chooses, per document:
/// about 268 million, a few seconds.
#[cfg(not(fuzzing))]
pub const MAX_WORK: u64 = 1 << 28;

/// Built for the fuzz targets, a sixteenth of it, as [`MAX_MEMORY`] is.
#[cfg(fuzzing)]
pub const MAX_WORK: u64 = 1 << 24;

/// What one element, or one table cell that holds text, costs at most,
/// from the reader's copy through the largest output (`doco`, where a
/// table's numeric cell, its typed value beside it, was measured at about
/// 4 KiB less its slots), with no text of its own.
pub const ELEMENT_BYTES: u64 = 4 << 10;

/// What one cell slot costs at most: a table's cell, empty or not, its
/// typed value, its merge flags, the copies of a cell a layout keeps (each
/// spent), and the separators every output writes for it.
pub const CELL_BYTES: u64 = 128;

/// What one byte of text costs at most, counted at [`text_cost`]: the
/// copies a reader and a layout make (a cell's text, a table's text, its
/// rows), then the output.
pub const TEXT_BYTES: u64 = 12;

/// A budget spent: what was being counted, and the bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TooLarge {
    what: &'static str,
    max: u64,
    /// The bound is the shared memory budget, in estimated bytes.
    memory: bool,
}

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.memory {
            write!(
                f,
                "{} past the {} MiB of memory a document may take",
                self.what,
                self.max >> 20
            )
        } else {
            write!(f, "more than {} {}", self.max, self.what)
        }
    }
}

impl std::error::Error for TooLarge {}

/// A count against a bound: [`Meter::spend`] before each allocation it
/// guards. Once refused, it stays spent.
#[derive(Debug, Clone)]
pub struct Meter {
    used: u64,
    max: u64,
    /// What is counted, for the error: "bytes of text", "cells".
    what: &'static str,
}

impl Meter {
    pub fn new(max: u64, what: &'static str) -> Self {
        Self { used: 0, max, what }
    }

    /// Count `n` more, or say why not: the error names the bound passed.
    pub fn spend(&mut self, n: u64) -> Result<(), TooLarge> {
        self.used = self.used.saturating_add(n);
        if self.used > self.max {
            return Err(TooLarge {
                what: self.what,
                max: self.max,
                memory: false,
            });
        }
        Ok(())
    }

    /// [`Meter::spend`] for a length in memory.
    pub fn spend_len(&mut self, n: usize) -> Result<(), TooLarge> {
        self.spend(u64::try_from(n).unwrap_or(u64::MAX))
    }

    pub fn used(&self) -> u64 {
        self.used
    }
}

/// The budget of one document: made once by the reader's entry point and
/// handed down to everything that reads or lays out the document.
#[derive(Debug, Clone)]
pub struct Budget {
    memory: u64,
    max_memory: u64,
    work: Meter,
}

impl Default for Budget {
    /// [`MAX_MEMORY`] and [`MAX_WORK`].
    fn default() -> Self {
        Self::new(MAX_MEMORY, MAX_WORK)
    }
}

impl Budget {
    /// A budget of `memory` estimated bytes and `work` steps; a test's
    /// smaller one shows a bound without allocating up to it.
    pub fn new(memory: u64, work: u64) -> Self {
        Self {
            memory: 0,
            max_memory: memory,
            work: Meter::new(work, "steps of layout"),
        }
    }

    fn spend_memory(&mut self, what: &'static str, n: u64, each: u64) -> Result<(), TooLarge> {
        self.memory = self.memory.saturating_add(n.saturating_mul(each));
        if self.memory > self.max_memory {
            return Err(TooLarge {
                what,
                max: self.max_memory,
                memory: true,
            });
        }
        Ok(())
    }

    /// Count `n` elements about to be made, or table cells that hold text,
    /// each a node of its own in the DoCO output.
    pub fn spend_elements(&mut self, n: u64) -> Result<(), TooLarge> {
        self.spend_memory("elements", n, ELEMENT_BYTES)
    }

    /// Count `n` cell slots about to be allocated: cells read, each copy of
    /// a repeated one, each cell a merge covers, each a table is padded
    /// with.
    pub fn spend_cells(&mut self, n: u64) -> Result<(), TooLarge> {
        self.spend_memory("cells", n, CELL_BYTES)
    }

    /// Count a copy of `s` about to be made, at [`text_cost`].
    pub fn spend_text(&mut self, s: &str) -> Result<(), TooLarge> {
        self.spend_chars(s.chars())
    }

    /// Count text about to be decoded into a string, from its characters,
    /// before the string is made.
    pub fn spend_chars(&mut self, chars: impl IntoIterator<Item = char>) -> Result<(), TooLarge> {
        let n = chars
            .into_iter()
            .fold(0u64, |n, c| n.saturating_add(char_cost(c)));
        self.spend_memory("text", n, TEXT_BYTES)
    }

    /// Count `n` steps of a layout pass about to be taken.
    pub fn spend_work(&mut self, n: u64) -> Result<(), TooLarge> {
        self.work.spend(n)
    }
}

/// The most bytes `c` takes in any output: a control character is six
/// (`\u0001` in JSON), `"` six (`&quot;` in XHTML), `&` five, `<` and `>`
/// four; a character Markdown escapes two; any other its UTF-8.
fn char_cost(c: char) -> u64 {
    match c {
        c if c.is_ascii_control() => 6,
        '"' | '&' => 6,
        '<' | '>' => 4,
        '\\' | '|' | '*' | '_' | '[' | ']' => 2,
        c => c.len_utf8() as u64,
    }
}

/// The most bytes `s` takes in any output, each character at its worst.
pub fn text_cost(s: &str) -> u64 {
    s.chars().map(char_cost).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_meter_refuses_past_its_bound_and_stays_refused() {
        let mut b = Meter::new(10, "cells");
        assert!(b.spend(4).is_ok());
        assert!(b.spend(6).is_ok());
        assert_eq!(b.spend(1).unwrap_err().to_string(), "more than 10 cells");
        assert!(b.spend(0).is_err());
        let mut b = Meter::new(10, "cells");
        assert!(b.spend(u64::MAX).is_err());
        assert!(b.spend_len(usize::MAX).is_err());
    }

    #[test]
    fn elements_cells_and_text_share_one_memory_budget() {
        let max = 10 * ELEMENT_BYTES;
        let mut b = Budget::new(max, 0);
        b.spend_elements(5).unwrap();
        b.spend_cells(5 * ELEMENT_BYTES / CELL_BYTES - 1).unwrap();
        // What is left is less than one element, or one more cell.
        assert!(b.clone().spend_elements(1).is_err());
        b.clone().spend_cells(1).unwrap();
        assert!(b.clone().spend_cells(2).is_err());
        let e = b.spend_text(&"y".repeat(1 << 10)).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!(
                "text past the {} MiB of memory a document may take",
                max >> 20
            )
        );
        // Spent, it stays spent.
        assert!(b.spend_cells(0).is_err());
        assert!(Budget::default().spend_elements(u64::MAX).is_err());
    }

    #[test]
    fn text_counts_at_its_worst_output() {
        assert_eq!(text_cost("abc"), 3);
        assert_eq!(text_cost("é€"), 5);
        // Escaped by JSON, XHTML or Markdown.
        assert_eq!(text_cost("\u{1}\u{7f}"), 12);
        assert_eq!(text_cost("\"&<>"), 20);
        assert_eq!(text_cost("|*_\\"), 8);
        // A control character costs six bytes of text, a letter one.
        let mut b = Budget::new(6 * TEXT_BYTES, 0);
        b.spend_text("\u{1}").unwrap();
        assert!(b.spend_text("y").is_err());
        let mut b = Budget::new(6 * TEXT_BYTES, 0);
        b.spend_chars("yyyyyy".chars()).unwrap();
        assert!(b.spend_chars(['y']).is_err());
    }

    #[test]
    fn work_is_counted_apart_from_memory() {
        let mut b = Budget::new(0, 10);
        b.spend_work(10).unwrap();
        assert_eq!(
            b.spend_work(1).unwrap_err().to_string(),
            "more than 10 steps of layout"
        );
        // Memory is still whole.
        assert!(Budget::new(ELEMENT_BYTES, 0).spend_elements(1).is_ok());
    }
}
