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
//!   space, with room for its input and a library's own copies. The copies
//!   DoCO writes of a table's cells (a merged value in each cell it covers,
//!   each cell's labels) are spent at the same costs, as cells that hold
//!   that text ([`Budget::spend_copies`]).
//!
//! What an element costs, and which copies are made, depend on the output
//! the document is converted to ([`Output`]): DoCO writes each element and
//! each cell that holds text as a node of its own, with its labels, where
//! Markdown, XHTML and JSON write each once. A converter that knows its
//! output says so around the reading ([`converting_to`]); a reader that is
//! not told counts at DoCO's costs, the largest.
//! * **Work**, in steps of a layout pass whose cost the file chooses (two
//!   islands of cells compared, a glyph checked against those it may
//!   overprint), at most [`MAX_WORK`]: a few seconds.
//!
//! A reader that guards a library's allocations it cannot see (what
//! calamine reserves for a sheet) keeps its own [`Meter`] for them.
//!
//! A document read inside another (an e-mail's attachment) is read
//! [`within`] what the outer one leaves.
//!
//! A part a package inflates whole (an Office file's main part, its
//! slides) is read within what is left of the memory budget and spent from
//! it, two bytes for each ([`Budget::spend_bytes`]).
//!
//! The bounds a caller may set are gathered in [`Limits`]: the memory and
//! the work above, the pages a paged document is read to, and the pixels a
//! page is rendered with. [`limited_to`] sets them for every reader called
//! inside it, on its thread; outside it, [`Limits::default`] holds. A
//! bound passed is a [`TooLarge`] that says which ([`TooLarge::limit`]).

use std::borrow::Cow;
use std::cell::Cell;

/// The memory budget as shipped, 2.5 GiB, whatever this build's is.
const SHIPPED_MEMORY: u64 = 5 << 29;

/// The memory a document's elements, cells and text may take, estimated
/// at the costs below: 2.5 GiB.
#[cfg(not(fuzzing))]
pub const MAX_MEMORY: u64 = SHIPPED_MEMORY;

/// Built for the fuzz targets (`fuzz/`), a sixteenth of it, 160 MiB: an
/// input that spends the whole budget then takes a fraction of a second
/// rather than seconds, and one that makes more than the budget sees stands
/// out under the targets' memory limit, rather than hiding under the room a
/// full budget needs.
#[cfg(fuzzing)]
pub const MAX_MEMORY: u64 = SHIPPED_MEMORY >> 4;

/// A reader's own bound on memory the budget does not count (what calamine
/// allocates for a workbook, the glyphs of an XPS page), `n` as sized for
/// the shipped budget, scaled as this build's [`MAX_MEMORY`] is: `n` as
/// shipped, a sixteenth of it for the fuzz targets. A bound that did not
/// scale would let one input take as much under the fuzzer as in
/// production, past the targets' memory limit, where the budget is a
/// sixteenth.
pub const fn scaled(n: u64) -> u64 {
    scaled_to(n, MAX_MEMORY)
}

/// `n`, sized for the shipped budget, for a budget of `memory`: divided as
/// many times as `memory` goes into the shipped one.
const fn scaled_to(n: u64, memory: u64) -> u64 {
    n / (SHIPPED_MEMORY / memory)
}

/// The steps of the layout passes whose cost a file chooses, per document:
/// about 268 million, a few seconds.
#[cfg(not(fuzzing))]
pub const MAX_WORK: u64 = 1 << 28;

/// Built for the fuzz targets, a sixteenth of it, as [`MAX_MEMORY`] is.
#[cfg(fuzzing)]
pub const MAX_WORK: u64 = 1 << 24;

/// The most pages a paged document (a PDF, an XPS document) is read to: ten
/// thousand. The pages past it are listed unread (`PastPageLimit`), never
/// dropped silently.
pub const MAX_PAGES: usize = 10_000;

/// The most pixels a page is rendered with, 64 million (256 MB of RGBA): a
/// page the scale asked for would draw larger is drawn at the scale that
/// fits, and the render says the scale it was drawn at.
pub const MAX_RENDER_PIXELS: u64 = 64_000_000;

/// The bounds one document is read within: what [`Budget::default`] holds,
/// the pages read, the pixels of a render. [`Limits::default`] is the
/// constants of this module (a sixteenth of the memory and work for the fuzz
/// targets); a caller sets its own for the readers it calls with
/// [`limited_to`].
///
/// A reader's own bounds on memory the budget does not count (a part read
/// beside the document, an XPS document's glyphs, an image's pixels) are
/// not among them: they are sized for the shipped budget ([`scaled`]) and
/// fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// The estimated memory a document's elements, cells, text and parts
    /// may take, in bytes ([`MAX_MEMORY`]).
    pub memory: u64,
    /// The steps of the layout passes and parsers' loops whose cost the
    /// file chooses ([`MAX_WORK`]).
    pub work: u64,
    /// The most pages a paged document is read to ([`MAX_PAGES`]).
    pub pages: usize,
    /// The most pixels a page is rendered with ([`MAX_RENDER_PIXELS`]).
    pub render_pixels: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory: MAX_MEMORY,
            work: MAX_WORK,
            pages: MAX_PAGES,
            render_pixels: MAX_RENDER_PIXELS,
        }
    }
}

thread_local! {
    static LIMITS: Cell<Option<Limits>> = const { Cell::new(None) };
}

/// Run `f` with every reader called in it, on this thread, held to
/// `limits` rather than [`Limits::default`]: the budgets made meanwhile
/// ([`Budget::default`]), the pages read, the pixels rendered. Nested, the
/// inner call's hold, and the outer's again once it returns or unwinds.
///
/// A document read inside another (an e-mail's attachment) is still read
/// [`within`] what the outer one leaves.
pub fn limited_to<R>(limits: Limits, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Limits>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LIMITS.with(|l| l.set(self.0));
        }
    }
    let _restore = Restore(LIMITS.with(|l| l.replace(Some(limits))));
    f()
}

/// The limits readers on this thread are held to: those [`limited_to`]
/// set, else [`Limits::default`].
pub fn limits() -> Limits {
    LIMITS.with(Cell::get).unwrap_or_default()
}

/// What one element, or one table cell that holds text, costs at most,
/// from the reader's copy through the largest output (`doco`, where a
/// table's numeric cell, its typed value beside it, was measured at about
/// 4 KiB less its slots), with no text of its own.
pub const ELEMENT_BYTES: u64 = 4 << 10;

/// What one element, or one table cell that holds text, costs at most when
/// the output writes it once (Markdown, XHTML, JSON, the text): the
/// reader's copy, which is most of it (an `.xlsx` cell was measured at
/// about 750 B, an `.odt` paragraph, its XML tree, 1.25 KB), then the
/// output's. A reader that counts a table cell's slots apart counts the
/// cell at [`FLAT_CELL_BYTES`] instead ([`Budget::spend_text_cells`]).
pub const FLAT_ELEMENT_BYTES: u64 = 2 << 10;

/// What one table cell that holds text costs when the output writes it once,
/// in its row (Markdown, XHTML, JSON, the text), beyond its slots
/// ([`CELL_BYTES`]) and its text, which are counted apart
/// ([`Budget::spend_text_cells`], [`filled_cell_bytes`]), in every format;
/// DoCO writes it as a node of its own, at [`ELEMENT_BYTES`]. A one-digit
/// cell of an `.xlsx` sheet was measured at about 750 B in all, from the
/// reader through Markdown, of which its slots and text count about 460 B:
/// with this, 980 B. A `.csv` record's cell takes about 110 B (counted at
/// [`CSV_CELL_BYTES`] instead), a Markdown table's 200 B with the parser's
/// tree, an HTML table's 500 B with the page's.
pub const FLAT_CELL_BYTES: u64 = 512;

/// What a `.csv` record's cell that holds text costs when the output writes
/// it once, beyond its slot and its text: the CSV reader keeps nothing but
/// the cell's string, so a cell was measured, from the reader through the
/// output, at about 100 B in all as Markdown and 216 B as the text (ten
/// million one-digit cells, 0.99 and 2.16 GB), and 120 and 236 B with
/// six-byte cells (a million records of five, 0.6 and 1.18 GB). Counted at
/// [`FLAT_CELL_BYTES`], such a file of a million records was refused at
/// four times what it takes; with this, a one-digit cell counts 268 B and
/// a six-byte one 328 B, so the budget holds about 8 million of these, 1.6
/// million records of five. DoCO writes it as a node of its own, at
/// [`ELEMENT_BYTES`] (2.9 KB measured).
pub const CSV_CELL_BYTES: u64 = 128;

/// What a document is converted to, which sets what its elements cost and
/// which copies of its tables' cells are made ([`Budget::spend_copies`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Output {
    /// The DoCO JSON-LD graph: each element and each cell that holds text a
    /// node, a merged value copied into each cell it covers, each cell its
    /// labels. The largest, and what a reader not told counts.
    #[default]
    Doco,
    /// The text projection: a value merged down shown on each row.
    Text,
    /// Markdown, XHTML or flat JSON: each cell written once.
    Flat,
}

impl Output {
    /// What one element, or one table cell that holds text, costs.
    pub fn element_bytes(self) -> u64 {
        match self {
            Output::Doco => ELEMENT_BYTES,
            Output::Text | Output::Flat => FLAT_ELEMENT_BYTES,
        }
    }

    /// What one table cell that holds text costs, its slot and its text
    /// apart.
    pub fn text_cell_bytes(self) -> u64 {
        match self {
            Output::Doco => ELEMENT_BYTES,
            Output::Text | Output::Flat => FLAT_CELL_BYTES,
        }
    }

    /// What one `.csv` record's cell that holds text costs, its slot and
    /// its text apart ([`CSV_CELL_BYTES`]).
    pub fn csv_cell_bytes(self) -> u64 {
        match self {
            Output::Doco => ELEMENT_BYTES,
            Output::Text | Output::Flat => CSV_CELL_BYTES,
        }
    }
}

/// What a table cell that holds `text` costs at `output` beyond its slot
/// ([`CELL_BYTES`], spent with the table's grid): nothing when it is empty,
/// else [`Output::text_cell_bytes`] and its text at [`text_cost`]. One cost
/// for a cell of every format, whichever reader made it (a CSV record, a
/// Markdown or HTML table, a Word or OpenDocument table, a sheet). The
/// slots each reader keeps are spent apart and differ (a sheet's layout
/// copies its cells, a Markdown table's parser builds a tree), so the same
/// table may still be refused from one format and not from another.
pub fn filled_cell_bytes(output: Output, text: &str) -> u64 {
    filled_at(output.text_cell_bytes(), text)
}

/// [`filled_cell_bytes`] for a `.csv` record's cell, at
/// [`Output::csv_cell_bytes`]: the CSV reader keeps nothing of a cell but
/// its text, and a cell costs it a fraction of a sheet's.
pub fn filled_csv_cell_bytes(output: Output, text: &str) -> u64 {
    filled_at(output.csv_cell_bytes(), text)
}

fn filled_at(cell: u64, text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    cell.saturating_add(text_cost(text).saturating_mul(TEXT_BYTES))
}

thread_local! {
    static OUTPUT: std::cell::Cell<Output> = const { std::cell::Cell::new(Output::Doco) };
}

/// Run `read` with each [`Budget::default`] made in it, on this thread,
/// counting at `output`'s costs: a converter wraps the reading of a document
/// in it once, and every reader's budget follows, without each taking the
/// output as an argument.
pub fn converting_to<R>(output: Output, read: impl FnOnce() -> R) -> R {
    struct Restore(Output);
    impl Drop for Restore {
        fn drop(&mut self) {
            OUTPUT.with(|o| o.set(self.0));
        }
    }
    let _restore = Restore(OUTPUT.with(|o| o.replace(output)));
    read()
}

/// The output the budgets made on this thread count at: [`Output::Doco`]
/// outside [`converting_to`].
pub fn output() -> Output {
    OUTPUT.with(std::cell::Cell::get)
}

/// What one cell slot costs at most: a table's cell, empty or not, its
/// typed value, its merge flags, the copies of a cell a layout keeps (each
/// spent), and the separators every output writes for it.
pub const CELL_BYTES: u64 = 128;

/// What one image a figure names by relationship id costs, beyond the id's
/// text: the reader's copies of the id (its list and the set that keeps it
/// once), then the media entry the caller lists, read or not. One `.docx`
/// picture naming a million images, each its own id, was measured to peak
/// at 566 MB in all.
pub const MEDIA_BYTES: u64 = 512;

/// What one byte of text costs at most, counted at [`text_cost`]: the
/// copies a reader and a layout make (a cell's text, a table's text, its
/// rows), then the output.
pub const TEXT_BYTES: u64 = 12;

/// What one glyph a PDF page draws costs, kept from the interpreter's
/// callback through the layout (its box, origin, size and text, its ink
/// measure, the lines' copies): 224 bytes. Text PDFs of 2,100 and 5,000
/// pages of fifty lines of words, 3,350 glyphs a page, peaked at 217 bytes
/// a glyph (1.47 and 3.64 GB). What a line or a crowded page costs beyond
/// that is spent apart: [`LINE_BYTES`], [`PAGE_GLYPH_BYTES`].
///
/// Dense text costs a little more than counted: 2,100 pages of fifty lines
/// of 100 characters in 7 pt peaked at 233 bytes a glyph (2.45 GB, 2.36
/// counted), 4% over: 90 MB there, about 100 MB at the budget's edge. The
/// gap is left, small, rather than closed: counted at 240, the 3,500 pages
/// of ordinary text that convert within the budget would be refused.
pub const GLYPH_BYTES: u64 = 224;

/// What a line of glyphs costs beyond its glyphs: 48 bytes, spent for each
/// glyph that does not follow the one before it on its baseline. Two
/// million glyphs each on a line of its own, over a thousand pages, peaked
/// at 251 bytes a glyph, where lines of words take 217.
pub const LINE_BYTES: u64 = 48;

/// What a glyph on the page that holds the most costs beyond its own: 192
/// bytes, spent once, for the largest page. A page is laid out with its
/// glyphs all at hand, and the passes over it take memory in proportion
/// to them, beside what the document keeps: two million glyphs in lines of
/// words on one page peaked at 374 bytes a glyph, on two pages at 278, on
/// ten at 211; on a line each, on one page, at 412. Counted at 384 a
/// glyph, whatever the page, a text PDF of 2,100 pages was refused; counted
/// at 192, 12 million glyphs on one page, a line each, were within the
/// budget and aborted the process under a 4 GB address space.
pub const PAGE_GLYPH_BYTES: u64 = 192;

/// What one byte of a part inflated whole costs: the part's own, and the
/// copy a reader's event buffer makes of an event as long as the part (a
/// run of text between two tags). A `.docx` whose main part is 1.9 GB of
/// spaces peaked at 4.0 GB.
pub const PART_BYTES: u64 = 2;

/// Which bound a document passed: the memory or the work of [`Limits`],
/// which a caller may raise, or a reader's own. The other two never refuse
/// a document: past the page limit it is read to it and the rest listed
/// unread, and a page past the render pixels is drawn smaller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Limit {
    /// [`Limits::memory`], in estimated bytes.
    Memory,
    /// [`Limits::work`], in steps.
    Work,
    /// A reader's own bound, fixed (a part read beside the document, the
    /// cells calamine may allocate, how deep a page tree nests and how many
    /// pages it lists).
    Fixed,
}

/// A bound passed: which ([`TooLarge::limit`]), its value
/// ([`TooLarge::max`]), and what was being counted, for the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TooLarge {
    limit: Limit,
    max: u64,
    what: Cow<'static, str>,
    /// `what` is the whole message, as a reader wrote it.
    said: bool,
    /// Where it was passed, before the message: "its sheets hold".
    context: Option<Cow<'static, str>>,
}

impl TooLarge {
    /// `what` counted past `max`, a bound of the kind `limit`.
    pub fn new(limit: Limit, max: u64, what: impl Into<Cow<'static, str>>) -> Self {
        Self {
            limit,
            max,
            what: what.into(),
            said: false,
            context: None,
        }
    }

    /// A reader's own bound passed, as its message says it
    /// ("more than 10000 pieces").
    pub fn fixed(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            said: true,
            ..Self::new(Limit::Fixed, 0, message)
        }
    }

    /// The same, said where it was passed: `context` comes first in the
    /// message ("its sheets hold").
    pub fn within(self, context: impl Into<Cow<'static, str>>) -> Self {
        Self {
            context: Some(context.into()),
            ..self
        }
    }

    /// Which bound was passed.
    pub fn limit(&self) -> Limit {
        self.limit
    }

    /// The bound's value, in its own unit (bytes for [`Limit::Memory`],
    /// steps for [`Limit::Work`]); zero for a reader's own bound stated only
    /// in its message.
    pub fn max(&self) -> u64 {
        self.max
    }
}

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(c) = &self.context {
            write!(f, "{c} ")?;
        }
        match self.limit {
            _ if self.said => write!(f, "{}", self.what),
            Limit::Memory => write!(
                f,
                "{} past the {} MiB of memory a document may take",
                self.what,
                self.max >> 20
            ),
            _ => write!(f, "more than {} {}", self.max, self.what),
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
    limit: Limit,
}

impl Meter {
    /// A reader's own bound ([`Limit::Fixed`]).
    pub fn new(max: u64, what: &'static str) -> Self {
        Self::of(Limit::Fixed, max, what)
    }

    /// A count against the bound `limit`.
    pub fn of(limit: Limit, max: u64, what: &'static str) -> Self {
        Self {
            used: 0,
            max,
            what,
            limit,
        }
    }

    /// Count `n` more, or say why not: the error names the bound passed.
    pub fn spend(&mut self, n: u64) -> Result<(), TooLarge> {
        self.used = self.used.saturating_add(n);
        if self.used > self.max {
            return Err(TooLarge::new(self.limit, self.max, self.what));
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
    output: Output,
}

impl Default for Budget {
    /// The memory and work of the [`limits`] in effect ([`MAX_MEMORY`] and
    /// [`MAX_WORK`] unless [`limited_to`] says otherwise), at the costs of
    /// the [`output`] the document is converted to; inside [`within`], no
    /// more memory than it gives.
    fn default() -> Self {
        let limits = limits();
        Self::new(limits.memory.min(SHARE.with(Cell::get)), limits.work).for_output(output())
    }
}

thread_local! {
    /// The memory a budget made on this thread may hold, set by [`within`].
    static SHARE: Cell<u64> = const { Cell::new(u64::MAX) };
}

/// Run `f` with each [`Budget::default`] made on this thread meanwhile
/// holding at most `memory`, and no more than an enclosing call gives.
///
/// A document read while another is held (an e-mail's attachment, read
/// beside the message) shares what the outer one leaves rather than
/// taking a budget of its own on top of it, without each reader's entry
/// point taking a budget to be handed one.
pub fn within<R>(memory: u64, f: impl FnOnce() -> R) -> R {
    /// The share before, put back however `f` ends.
    struct Restore(u64);
    impl Drop for Restore {
        fn drop(&mut self) {
            SHARE.with(|s| s.set(self.0));
        }
    }
    let outer = SHARE.with(|s| s.replace(s.get().min(memory)));
    let _restore = Restore(outer);
    f()
}

impl Budget {
    /// A budget of `memory` estimated bytes and `work` steps, at DoCO's
    /// costs; a test's smaller one shows a bound without allocating up to
    /// it.
    pub fn new(memory: u64, work: u64) -> Self {
        Self {
            memory: 0,
            max_memory: memory,
            work: Meter::of(Limit::Work, work, "steps of work"),
            output: Output::Doco,
        }
    }

    /// This budget, counting at `output`'s costs.
    pub fn for_output(self, output: Output) -> Self {
        Self { output, ..self }
    }

    fn spend_memory(&mut self, what: &'static str, n: u64, each: u64) -> Result<(), TooLarge> {
        self.memory = self.memory.saturating_add(n.saturating_mul(each));
        if self.memory > self.max_memory {
            return Err(TooLarge::new(Limit::Memory, self.max_memory, what));
        }
        Ok(())
    }

    /// Count `n` elements about to be made, or table cells that hold text,
    /// each a node of its own in the DoCO output.
    pub fn spend_elements(&mut self, n: u64) -> Result<(), TooLarge> {
        self.spend_memory("elements", n, self.output.element_bytes())
    }

    /// Count `n` table cells that hold text about to be made: DoCO writes
    /// each as a node of its own, at an element's cost; the others write it
    /// once in its row, at [`FLAT_CELL_BYTES`]. Their slots and their text
    /// are counted apart.
    pub fn spend_text_cells(&mut self, n: u64) -> Result<(), TooLarge> {
        self.spend_memory("elements", n, self.output.text_cell_bytes())
    }

    /// Count the table cells about to be made that hold `texts`, beyond
    /// their slots, at [`filled_cell_bytes`]: an empty one costs nothing, one
    /// that holds text a cell that holds text, and its text. For a reader
    /// whose cells' text is not spent elsewhere.
    pub fn spend_cell_texts<'a>(
        &mut self,
        texts: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), TooLarge> {
        let output = self.output;
        let n = texts
            .into_iter()
            .fold(0u64, |n, t| n.saturating_add(filled_cell_bytes(output, t)));
        self.spend_memory("cells", n, 1)
    }

    /// [`Budget::spend_cell_texts`] for a `.csv` record's cells, at
    /// [`filled_csv_cell_bytes`].
    pub fn spend_csv_cell_texts<'a>(
        &mut self,
        texts: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), TooLarge> {
        let output = self.output;
        let n = texts.into_iter().fold(0u64, |n, t| {
            n.saturating_add(filled_csv_cell_bytes(output, t))
        });
        self.spend_memory("cells", n, 1)
    }

    /// Count `n` cell slots about to be allocated: cells read, each copy of
    /// a repeated one, each cell a merge covers, each a table is padded
    /// with.
    pub fn spend_cells(&mut self, n: u64) -> Result<(), TooLarge> {
        self.spend_memory("cells", n, CELL_BYTES)
    }

    /// Count an image a figure names by `id`, about to be kept, at
    /// [`MEDIA_BYTES`] and the id's text: a picture may name millions.
    pub fn spend_media(&mut self, id: &str) -> Result<(), TooLarge> {
        self.spend_memory("images", 1, MEDIA_BYTES)?;
        self.spend_text(id)
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
        self.spend_text_cost(n)
    }

    /// Count text about to be made whose cost is already known, `cost` as
    /// [`text_cost`] counts it: text a reader gathers from a tree, counted
    /// in a pass that copies nothing, then gathered into a string.
    pub fn spend_text_cost(&mut self, cost: u64) -> Result<(), TooLarge> {
        self.spend_memory("text", cost, TEXT_BYTES)
    }

    /// Count what the output copies of the table `e`, once its cells are
    /// spent ([`table_copies`](crate::doco::table_copies)): for DoCO, each
    /// cell a merged value fills, a node of its own, and the text copied, a
    /// merged value into each cell it covers and each cell's labels; for
    /// the text projection, a value merged down on each row; for the
    /// others, nothing. A reader calls it on every table it makes, before
    /// keeping it: a merge or a label the file states once is written once
    /// per cell.
    pub fn spend_copies(&mut self, e: &crate::Element) -> Result<(), TooLarge> {
        const WHAT: &str = "copies of table cells";
        if self.output == Output::Flat {
            return Ok(());
        }
        let copies = crate::doco::table_copies(e);
        if self.output == Output::Text {
            return self.spend_memory(WHAT, copies.merged, TEXT_BYTES);
        }
        self.spend_memory(WHAT, copies.cells, ELEMENT_BYTES)?;
        self.spend_memory(WHAT, copies.text, TEXT_BYTES)
    }

    /// Count the links about to be kept on an element, and each target's
    /// text. DoCO writes each as a node of its own, listed on its element:
    /// 600,000 on one line of Markdown peaked at 2.5 GB, 4.2 KB a link,
    /// and two million aborted at 4 GB. The others write it in the text,
    /// at four cell slots a link: the reader's copies as it locates the
    /// anchor, and the markup (two million, 400 B a link, as Markdown).
    pub fn spend_links(&mut self, links: &[crate::Link]) -> Result<(), TooLarge> {
        const WHAT: &str = "links";
        let n = links.len() as u64;
        match self.output {
            Output::Doco => self.spend_memory(WHAT, n, ELEMENT_BYTES + CELL_BYTES)?,
            Output::Text | Output::Flat => self.spend_memory(WHAT, n, 4 * CELL_BYTES)?,
        }
        for l in links {
            if let crate::Target::Uri { uri } = &l.target {
                self.spend_memory(WHAT, text_cost(uri), TEXT_BYTES)?;
            }
        }
        Ok(())
    }

    /// Count `n` glyphs a PDF page draws, about to be kept, at
    /// [`GLYPH_BYTES`] each.
    pub fn spend_glyphs(&mut self, n: u64) -> Result<(), TooLarge> {
        self.spend_memory("glyphs", n, GLYPH_BYTES)
    }

    /// Count `n` lines the glyphs kept start, at [`LINE_BYTES`] each.
    pub fn spend_lines(&mut self, n: u64) -> Result<(), TooLarge> {
        self.spend_memory("glyphs", n, LINE_BYTES)
    }

    /// Count `n` glyphs the largest page holds past what the largest page
    /// before it held, at [`PAGE_GLYPH_BYTES`] each.
    pub fn spend_page_glyphs(&mut self, n: u64) -> Result<(), TooLarge> {
        self.spend_memory("glyphs", n, PAGE_GLYPH_BYTES)
    }

    /// Count `n` bytes a PDF reader keeps beside a glyph to read it by
    /// its shape once the document is read: the outline of a glyph whose
    /// characters the document left unknown, or of a letter a ligature
    /// is compared with. Its size is the font's to choose.
    pub fn spend_outline(&mut self, n: u64) -> Result<(), TooLarge> {
        self.spend_memory("glyph outlines", n, 1)
    }

    /// Count `n` steps of a layout pass about to be taken.
    pub fn spend_work(&mut self, n: u64) -> Result<(), TooLarge> {
        self.work.spend(n)
    }

    /// The estimated memory not spent yet.
    pub fn memory_left(&self) -> u64 {
        self.max_memory.saturating_sub(self.memory)
    }

    /// Count `n` bytes of a part inflated whole, at [`PART_BYTES`] each:
    /// what an archive holds in a few kilobytes can be gigabytes once
    /// inflated, and a document's main part has no smaller bound of its
    /// own.
    pub fn spend_bytes(&mut self, n: u64) -> Result<(), TooLarge> {
        self.spend_memory("bytes of parts inflated", n, PART_BYTES)
    }
}

/// The most bytes `c` takes in any output: a control character is six
/// (`\u0001` in JSON), `"` six (`&quot;` in XHTML), `&` five, `<` and `>`
/// four; a character Markdown escapes two; any other its UTF-8.
pub fn char_cost(c: char) -> u64 {
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
    fn a_link_is_a_node_in_doco_and_a_few_slots_in_the_others() {
        let links = [crate::Link::uri("ab"), crate::Link::page(3)];
        let spend = |output: Output, memory: u64| {
            Budget::new(memory, 0)
                .for_output(output)
                .spend_links(&links)
        };
        let doco = 2 * (ELEMENT_BYTES + CELL_BYTES) + 2 * TEXT_BYTES;
        assert!(spend(Output::Doco, doco).is_ok());
        assert!(spend(Output::Doco, doco - 1).is_err());
        let flat = 2 * 4 * CELL_BYTES + 2 * TEXT_BYTES;
        for output in [Output::Flat, Output::Text] {
            assert!(spend(output, flat).is_ok());
            assert!(spend(output, flat - 1).is_err());
        }
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

    /// A two-column table whose first row's value, `len` bytes, is merged
    /// down its `rows` rows, each row's second cell holding a letter.
    fn merged_down(len: usize, rows: usize) -> crate::Element {
        let mut t = crate::Element::new("doco:Table", String::new(), "test", "test");
        let mut cells = vec![vec!["y".repeat(len), "a".into()]];
        cells.resize(rows, vec![String::new(), "a".into()]);
        t.cells = Some(cells);
        t.header_rows = Some(0);
        t.merged_down = Some((0..rows * 2).map(|i| i >= 2 && i % 2 == 0).collect());
        t
    }

    #[test]
    fn what_an_element_and_a_copy_cost_follows_the_output() {
        let at = |output| Budget::new(ELEMENT_BYTES, 0).for_output(output);
        // DoCO writes an element as a node: one fills the budget.
        assert!(at(Output::Doco).spend_elements(2).is_err());
        assert!(at(Output::Flat).spend_elements(2).is_ok());
        assert!(at(Output::Text).spend_elements(2).is_ok());
        assert!(at(Output::Flat).spend_elements(3).is_err());
        // A table cell that holds text: a node for DoCO, a cell of a row,
        // its slots and text counted apart, for the others.
        let cells = |output| {
            let mut b = Budget::new(MAX_MEMORY, 0).for_output(output);
            b.spend_text_cells(3).unwrap();
            b.memory / 3
        };
        assert_eq!(cells(Output::Doco), ELEMENT_BYTES);
        assert_eq!(cells(Output::Flat), FLAT_CELL_BYTES);
        assert_eq!(cells(Output::Text), FLAT_CELL_BYTES);
        // The same, with their text, for a reader that spends it with them;
        // an empty one costs nothing beyond its slot.
        for output in [Output::Doco, Output::Flat, Output::Text] {
            let cell = output.text_cell_bytes();
            assert_eq!(filled_cell_bytes(output, "a&"), cell + 7 * TEXT_BYTES);
            assert_eq!(filled_cell_bytes(output, ""), 0);
            let need = 2 * cell + 13 * TEXT_BYTES;
            let mut b = Budget::new(need, 0).for_output(output);
            b.spend_cell_texts(["a&", "", "\u{1}"]).unwrap();
            assert_eq!(b.memory_left(), 0);
            assert!(b.spend_cell_texts([""]).is_ok());
            assert!(b.spend_cell_texts(["y"]).is_err());
        }
        // A value merged down: DoCO copies it into each row's cell and
        // labels the next cell with it, the text shows it on each row,
        // Markdown, XHTML and JSON write it once.
        let t = merged_down(1000, 10);
        let copies = crate::doco::table_copies(&t);
        assert_eq!(copies.merged, 9 * 1000);
        assert_eq!(copies.text, 9 * 1000 + 10 * 1000);
        let spend = |output, memory| {
            Budget::new(memory, 0)
                .for_output(output)
                .spend_copies(&t)
                .is_ok()
        };
        let doco = copies.cells * ELEMENT_BYTES + copies.text * TEXT_BYTES;
        assert!(spend(Output::Doco, doco));
        assert!(!spend(Output::Doco, doco - 1));
        assert!(spend(Output::Text, copies.merged * TEXT_BYTES));
        assert!(!spend(Output::Text, copies.merged * TEXT_BYTES - 1));
        assert!(spend(Output::Flat, 0));
    }

    #[test]
    fn a_budget_counts_at_the_output_it_is_converted_to() {
        let element = |b: &mut Budget| {
            let before = b.memory;
            b.spend_elements(1).unwrap();
            b.memory - before
        };
        // Not told, at DoCO's costs.
        assert_eq!(output(), Output::Doco);
        assert_eq!(element(&mut Budget::default()), ELEMENT_BYTES);
        let inner = converting_to(Output::Flat, || {
            let flat = element(&mut Budget::default());
            // Nested, the inner one wins and gives way again.
            let text = converting_to(Output::Text, output);
            (flat, text, output())
        });
        assert_eq!(inner, (FLAT_ELEMENT_BYTES, Output::Text, Output::Flat));
        assert_eq!(output(), Output::Doco);
        // Restored even when the reading panics.
        let panicked = std::panic::catch_unwind(|| converting_to(Output::Flat, || panic!()));
        assert!(panicked.is_err());
        assert_eq!(output(), Output::Doco);
        // On this thread only: another converts to its own.
        converting_to(Output::Flat, || {
            assert_eq!(std::thread::spawn(output).join().unwrap(), Output::Doco);
        });
        // A test's budget is DoCO's, whatever it is read in.
        converting_to(Output::Flat, || {
            assert_eq!(element(&mut Budget::new(MAX_MEMORY, 0)), ELEMENT_BYTES)
        });
    }

    #[test]
    fn bytes_inflated_share_the_memory_budget() {
        let mut b = Budget::new(ELEMENT_BYTES + 10 * PART_BYTES, 0);
        b.spend_elements(1).unwrap();
        assert_eq!(b.memory_left(), 10 * PART_BYTES);
        b.spend_bytes(10).unwrap();
        assert_eq!(b.memory_left(), 0);
        assert_eq!(
            b.spend_bytes(1).unwrap_err().to_string(),
            "bytes of parts inflated past the 0 MiB of memory a document may take"
        );
        assert_eq!(b.memory_left(), 0);
    }

    #[test]
    fn a_budget_made_within_a_share_holds_no_more_than_it() {
        let full = Budget::default().memory_left();
        assert_eq!(full, MAX_MEMORY);
        let inner = within(1 << 20, || {
            // An enclosing share bounds a larger one inside it.
            let nested = within(1 << 30, || Budget::default().memory_left());
            (Budget::default().memory_left(), nested)
        });
        assert_eq!(inner, (1 << 20, 1 << 20));
        // Put back when it returns, and when it unwinds.
        assert_eq!(Budget::default().memory_left(), full);
        let unwound = std::panic::catch_unwind(|| within(0, || panic!("refused")));
        assert!(unwound.is_err());
        assert_eq!(Budget::default().memory_left(), full);
        // Another thread's budgets are its own.
        within(0, || {
            let other = std::thread::spawn(|| Budget::default().memory_left());
            assert_eq!(other.join().unwrap(), full);
        });
    }

    #[test]
    fn a_bound_of_a_readers_own_scales_as_the_budget_does() {
        // As shipped, itself; for the fuzz targets' budget, a sixteenth.
        assert_eq!(scaled_to(1 << 22, SHIPPED_MEMORY), 1 << 22);
        assert_eq!(scaled_to(1 << 22, SHIPPED_MEMORY >> 4), 1 << 18);
        assert_eq!(scaled_to(250_000, SHIPPED_MEMORY >> 4), 15_625);
        // This build's bounds follow this build's budget.
        assert_eq!(scaled(SHIPPED_MEMORY), MAX_MEMORY);
        assert_eq!(scaled(1 << 22), scaled_to(1 << 22, MAX_MEMORY));
    }

    #[test]
    fn a_bound_passed_says_which_and_its_value() {
        let e = Budget::new(1 << 20, 0).spend_elements(1 << 10).unwrap_err();
        assert_eq!((e.limit(), e.max()), (Limit::Memory, 1 << 20));
        let e = Budget::new(0, 7).spend_work(8).unwrap_err();
        assert_eq!((e.limit(), e.max()), (Limit::Work, 7));
        let e = Meter::new(3, "cells").spend(4).unwrap_err();
        assert_eq!((e.limit(), e.max()), (Limit::Fixed, 3));
        assert_eq!(
            Meter::of(Limit::Work, 3, "steps")
                .spend(4)
                .unwrap_err()
                .limit(),
            Limit::Work
        );
        // A reader's own message, as written, and where it was passed.
        let e = TooLarge::fixed("more than 9 pieces");
        assert_eq!(
            (e.limit(), e.to_string().as_str()),
            (Limit::Fixed, "more than 9 pieces")
        );
        let e = Meter::new(3, "cells")
            .spend(4)
            .unwrap_err()
            .within("its sheets hold");
        assert_eq!(e.to_string(), "its sheets hold more than 3 cells");
        assert_eq!(e.limit(), Limit::Fixed);
    }

    #[test]
    fn limits_set_for_a_call_hold_the_budgets_made_in_it() {
        assert_eq!(limits(), Limits::default());
        let small = Limits {
            memory: 1 << 20,
            work: 5,
            pages: 2,
            render_pixels: 100,
        };
        let inner = limited_to(small, || {
            let b = Budget::default();
            let nested = limited_to(Limits::default(), || limits().pages);
            // A share given inside still bounds it, and the other way round.
            let shared = within(1 << 10, || Budget::default().memory_left());
            (
                b.memory_left(),
                b.clone().spend_work(6).is_err(),
                limits(),
                nested,
                shared,
            )
        });
        assert_eq!(inner, (1 << 20, true, small, MAX_PAGES, 1 << 10));
        assert_eq!(
            within(1 << 40, || limited_to(small, || Budget::default()
                .memory_left())),
            1 << 20
        );
        // Put back when it returns, and when it unwinds; other threads'
        // are their own.
        assert_eq!(limits(), Limits::default());
        assert!(std::panic::catch_unwind(|| limited_to(small, || panic!())).is_err());
        assert_eq!(Budget::default().memory_left(), MAX_MEMORY);
        limited_to(small, || {
            assert_eq!(
                std::thread::spawn(limits).join().unwrap(),
                Limits::default()
            );
        });
    }

    #[test]
    fn work_is_counted_apart_from_memory() {
        let mut b = Budget::new(0, 10);
        b.spend_work(10).unwrap();
        assert_eq!(
            b.spend_work(1).unwrap_err().to_string(),
            "more than 10 steps of work"
        );
        // Memory is still whole.
        assert!(Budget::new(ELEMENT_BYTES, 0).spend_elements(1).is_ok());
    }
}
