//! A running bound on what a reader of untrusted input produces.
//!
//! A reader's bounds on its input (bytes read, parts inflated) do not bound
//! what it makes of it: a cell repeated, a string shared by many cells, a
//! record named twice each copy their text again. So a reader that copies
//! counts what it is about to copy against a [`Budget`] *before* it
//! allocates, and stops with an error once the budget is spent. An error
//! here is the reader refusing the file; it never aborts the process.

/// The most bytes of text a reader produces from one document, cumulated
/// over every copy it makes: 256 MiB.
pub const MAX_TEXT_BYTES: u64 = 256 << 20;

/// A count against a bound: [`Budget::spend`] before each allocation it
/// guards.
#[derive(Debug, Clone)]
pub struct Budget {
    used: u64,
    max: u64,
    /// What is counted, for the error: "bytes of text", "cells".
    what: &'static str,
}

impl Budget {
    pub fn new(max: u64, what: &'static str) -> Self {
        Self { used: 0, max, what }
    }

    /// A budget of [`MAX_TEXT_BYTES`] bytes of text.
    pub fn text() -> Self {
        Self::new(MAX_TEXT_BYTES, "bytes of text")
    }

    /// Count `n` more, or say why not: the error names the bound passed.
    /// Once refused, the budget stays spent.
    pub fn spend(&mut self, n: u64) -> Result<(), String> {
        self.used = self.used.saturating_add(n);
        if self.used > self.max {
            return Err(format!("more than {} {}", self.max, self.what));
        }
        Ok(())
    }

    /// [`Budget::spend`] for a length in memory.
    pub fn spend_len(&mut self, n: usize) -> Result<(), String> {
        self.spend(u64::try_from(n).unwrap_or(u64::MAX))
    }

    pub fn used(&self) -> u64 {
        self.used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_budget_refuses_past_its_bound_and_stays_refused() {
        let mut b = Budget::new(10, "cells");
        assert!(b.spend(4).is_ok());
        assert!(b.spend(6).is_ok());
        assert_eq!(b.spend(1).unwrap_err(), "more than 10 cells");
        assert!(b.spend(0).is_err());
        let mut b = Budget::new(10, "cells");
        assert!(b.spend(u64::MAX).is_err());
        assert!(b.spend_len(usize::MAX).is_err());
    }
}
