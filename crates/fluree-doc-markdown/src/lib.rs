//! Markdown to DoCO-typed document elements.
//!
//! Markdown declares the structure a PDF makes us infer: a heading states its
//! level, a list states its items, a table states where its header ends. So
//! this reader asserts nothing — every element's `evidence` is `"markdown"`,
//! and where the PDF engine reports a *measurement* (`header_rows` inferred
//! from shading and value types) this reports a *fact*.
//!
//! What is missing is geometry. Markdown has no pages and no coordinates, so
//! every element's `bbox` is `None` rather than a zeroed box that would read
//! as a real position to a consumer.
//!
//! **Bounds.** Each element, each table cell, a table's rows padded to the
//! widest, their text, each link, and the copies the output makes of a
//! table's cells (a cell's column header, row label and section band) are
//! counted against the document's [`Budget`] before they are made: a band's
//! label a megabyte long over a hundred columns and forty rows would
//! otherwise be written four thousand times.

use fluree_doc_model::budget::{Budget, TooLarge};
use fluree_doc_model::{Element, Link};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Parse Markdown into document elements in reading order; refused past
/// the document's budget.
pub fn parse(src: &str) -> Result<Vec<Element>, TooLarge> {
    parse_within(src, Budget::default())
}

/// [`parse`], against `budget`.
fn parse_within(src: &str, budget: Budget) -> Result<Vec<Element>, TooLarge> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_FOOTNOTES);
    Reader {
        budget,
        ..Reader::default()
    }
    .run(Parser::new_ext(src, opts))
}

/// The character offset of each byte offset in `s`, offsets in ascending
/// order, counted in one pass. An offset inside a character counts the
/// characters before it; one past the end, all of them.
fn char_offsets(s: &str, bytes: impl Iterator<Item = usize>) -> Vec<usize> {
    let mut chars = s.char_indices().map(|(i, _)| i).peekable();
    let mut n = 0;
    bytes
        .map(|b| {
            while chars.next_if(|&at| at < b).is_some() {
                n += 1;
            }
            n
        })
        .collect()
}

#[derive(Default)]
struct Reader {
    budget: Budget,
    out: Vec<Element>,
    text: String,
    /// Heading level being collected, if any.
    heading: Option<usize>,
    /// List items open around the text being read, nested ones included.
    /// Text inside any of them belongs to the innermost.
    items: usize,
    in_code: bool,
    /// Table under construction: rows, and whether the header row is open.
    rows: Vec<Vec<String>>,
    /// The cells spent for `rows` and `row`.
    cells: u64,
    row: Vec<String>,
    in_table: bool,
    in_table_head: bool,
    header_rows: usize,
    /// Per row of the table under construction: its first cell is set
    /// entirely in strong emphasis.
    first_bold: Vec<bool>,
    /// Within the cell being read: depth of strong emphasis, and whether
    /// any text arrived inside it and outside it.
    strong: usize,
    cell_strong_text: bool,
    cell_plain_text: bool,
    /// Links closed so far in the text under construction, as `(byte
    /// offsets where the anchor begins and ends, link)`, and the one still
    /// open, as `(byte offset where its anchor begins, target)`: in bytes,
    /// which [`flush_text`](Reader::flush_text) turns into characters once.
    /// Counting the characters at each link is quadratic in a paragraph of
    /// links: 600,000 on one line, 4 MB, took 16 s.
    links: Vec<(usize, usize, Link)>,
    open_link: Option<(usize, String)>,
}

impl Reader {
    fn element(&self, kind: &str, text: String, level: Option<usize>) -> Element {
        Element {
            level,
            ..Element::new(kind, text, "markdown", "markdown")
        }
    }

    fn flush_text(&mut self, kind: &str, level: Option<usize>) -> Result<(), TooLarge> {
        let raw = std::mem::take(&mut self.text);
        let links = std::mem::take(&mut self.links);
        self.open_link = None;
        if raw.trim().is_empty() {
            return Ok(());
        }
        self.budget.spend_elements(1)?;
        self.budget.spend_text(raw.trim())?;
        let mut e = self.element(kind, raw.trim().to_string(), level);
        // `element` holds the trimmed text; the offsets were taken against
        // the untrimmed string, so they shift by whatever the trim removed.
        let lead = raw.len() - raw.trim_start().len();
        let end = lead + e.text.len();
        let begins = char_offsets(&raw[lead..end], links.iter().map(|l| l.0.max(lead) - lead));
        let ends = char_offsets(&raw[lead..end], links.iter().map(|l| l.1.max(lead) - lead));
        let shifted: Vec<Link> = links
            .into_iter()
            .zip(begins.into_iter().zip(ends))
            .filter_map(|((b, end_at, link), (from, to))| {
                (b >= lead && end_at <= end && to > from).then(|| link.spanning(from, to))
            })
            .collect();
        if !shifted.is_empty() {
            e.links = Some(shifted);
        }
        self.out.push(e);
        Ok(())
    }

    /// Mark where a link's anchor starts. Its extent is known only when the
    /// closing event arrives, because the anchor's own text is emitted
    /// between the two.
    fn open_link(&mut self, dest: &str) {
        if dest.is_empty() {
            return;
        }
        self.open_link = Some((self.text.len(), dest.to_string()));
    }

    /// Close the open link, spent before it is kept.
    fn close_link(&mut self) -> Result<(), TooLarge> {
        let Some((begin, dest)) = self.open_link.take() else {
            return Ok(());
        };
        let end = self.text.len();
        if end > begin {
            let link = Link::uri(dest);
            self.budget.spend_links(std::slice::from_ref(&link))?;
            self.links.push((begin, end, link));
        }
        Ok(())
    }

    /// End the text read so far as the block it belongs to: the open list
    /// item's, or a paragraph of its own.
    fn flush_block(&mut self) -> Result<(), TooLarge> {
        let kind = if self.items > 0 {
            "doc:ListItem"
        } else {
            "doco:Paragraph"
        };
        self.flush_text(kind, None)
    }

    /// A table's cells as read, each row padded to the widest: spent before
    /// the padding is made, then what the output copies of them.
    fn table(&mut self) -> Result<Option<Element>, TooLarge> {
        let rows = std::mem::take(&mut self.rows);
        let read = std::mem::take(&mut self.cells);
        if rows.is_empty() {
            return Ok(None);
        }
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        let grid = (rows.len() as u64).saturating_mul(width as u64);
        // pulldown-cmark gives every row the header's cells, each spent as
        // read, so nothing is padded here; the padding is spent all the same,
        // should a row ever come short.
        self.budget.spend_cells(grid.saturating_sub(read))?;
        self.budget.spend_elements(1)?;
        let rows: Vec<Vec<String>> = rows
            .into_iter()
            .map(|mut r| {
                r.resize(width, String::new());
                r
            })
            .collect();
        let text = rows
            .iter()
            .map(|r| r.join(" | "))
            .collect::<Vec<_>>()
            .join("\n");
        // Markdown has no merged cells, so a section band is
        // written as a body row holding only a bold first cell
        // (`| **Food** | | |`). Bold is the author's mark: a row
        // whose other cells are merely empty is an item with
        // values missing. The last row has nothing to label.
        let sub_headers: Vec<usize> = (self.header_rows..rows.len().saturating_sub(1))
            .filter(|&r| {
                width > 1
                    && self.first_bold.get(r).copied().unwrap_or(false)
                    && !rows[r][0].is_empty()
                    && rows[r][1..].iter().all(|c| c.is_empty())
            })
            .collect();
        let mut e = self.element("doco:Table", text, None);
        e.header_rows = Some(self.header_rows);
        e.sub_headers = (!sub_headers.is_empty()).then_some(sub_headers);
        e.cells = Some(rows);
        self.budget.spend_copies(&e)?;
        Ok(Some(e))
    }

    fn run(mut self, parser: Parser<'_>) -> Result<Vec<Element>, TooLarge> {
        for ev in parser {
            match ev {
                Event::Start(Tag::Heading { level, .. }) => {
                    self.flush_block()?;
                    self.heading = Some(match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        HeadingLevel::H3 => 3,
                        HeadingLevel::H4 => 4,
                        HeadingLevel::H5 => 5,
                        HeadingLevel::H6 => 6,
                    });
                }
                Event::End(TagEnd::Heading(_)) => {
                    let level = self.heading.take();
                    self.flush_text("doco:SectionTitle", level)?;
                }
                // A nested list ends its parent item's own text, which is
                // that item's, before the nested items begin.
                Event::Start(Tag::List(_)) => self.flush_block()?,
                Event::Start(Tag::Item) => {
                    self.flush_block()?;
                    self.items += 1;
                }
                Event::End(TagEnd::Item) => {
                    self.flush_text("doc:ListItem", None)?;
                    self.items = self.items.saturating_sub(1);
                }
                Event::Start(Tag::CodeBlock(_)) => {
                    self.flush_block()?;
                    self.in_code = true;
                }
                Event::End(TagEnd::CodeBlock) => {
                    self.in_code = false;
                    // A code block is verbatim: keep its newlines rather than
                    // folding it into a paragraph's single flow.
                    let t = std::mem::take(&mut self.text);
                    if !t.trim().is_empty() {
                        self.budget.spend_elements(1)?;
                        self.budget.spend_text(t.trim_end())?;
                        let e = self.element("doco:Paragraph", t.trim_end().to_string(), None);
                        self.out.push(e);
                    }
                }
                Event::Start(Tag::Table(_)) => {
                    self.flush_block()?;
                    self.in_table = true;
                    self.rows.clear();
                    self.cells = 0;
                    self.first_bold.clear();
                    self.cell_strong_text = false;
                    self.cell_plain_text = false;
                    self.header_rows = 0;
                }
                Event::Start(Tag::TableHead) => self.in_table_head = true,
                Event::End(TagEnd::TableHead) => {
                    self.in_table_head = false;
                    // Markdown states where the header ends, so this is known
                    // rather than measured.
                    self.header_rows = 1;
                    self.rows.push(std::mem::take(&mut self.row));
                }
                Event::End(TagEnd::TableRow) => {
                    self.rows.push(std::mem::take(&mut self.row));
                }
                Event::End(TagEnd::TableCell) => {
                    self.budget.spend_cells(1)?;
                    self.cells += 1;
                    let c = self.text.trim();
                    // A cell that holds text is a node of the output of its
                    // own.
                    if !c.is_empty() {
                        self.budget.spend_elements(1)?;
                        self.budget.spend_text(c)?;
                    }
                    let c = c.to_string();
                    self.text.clear();
                    if self.row.is_empty() {
                        self.first_bold
                            .push(self.cell_strong_text && !self.cell_plain_text);
                    }
                    self.cell_strong_text = false;
                    self.cell_plain_text = false;
                    // A cell's text leaves the buffer without becoming an
                    // element, so any link inside it has nothing to attach to
                    // and must not follow the next paragraph out.
                    self.links.clear();
                    self.open_link = None;
                    self.row.push(c);
                }
                Event::End(TagEnd::Table) => {
                    self.in_table = false;
                    if let Some(e) = self.table()? {
                        self.out.push(e);
                    }
                }
                // A blank line between list items makes each item's text a
                // paragraph inside it, and an item can hold several. They
                // are the item's: run on, one item to a list entry.
                Event::End(TagEnd::Paragraph) if self.items > 0 => {
                    if !self.text.trim().is_empty() {
                        self.text.push(' ');
                    }
                }
                Event::End(TagEnd::Paragraph) => self.flush_text("doco:Paragraph", None)?,
                Event::Start(Tag::Link { dest_url, .. }) => self.open_link(&dest_url),
                Event::End(TagEnd::Link) => self.close_link()?,
                Event::Start(Tag::Strong) => self.strong += 1,
                Event::End(TagEnd::Strong) => self.strong = self.strong.saturating_sub(1),
                Event::Text(t) | Event::Code(t) => {
                    if !t.trim().is_empty() {
                        if self.strong > 0 {
                            self.cell_strong_text = true;
                        } else {
                            self.cell_plain_text = true;
                        }
                    }
                    self.text.push_str(&t)
                }
                Event::SoftBreak => self.text.push(' '),
                Event::HardBreak => {
                    if self.in_code {
                        self.text.push('\n');
                    } else {
                        self.text.push(' ');
                    }
                }
                Event::Rule => self.flush_block()?,
                _ => {}
            }
        }
        self.flush_text("doco:Paragraph", None)?;
        for (i, e) in self.out.iter_mut().enumerate() {
            e.id = format!("elem-{:05}", i + 1);
        }
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use fluree_doc_model::budget::{
        converting_to, Output, CELL_BYTES, ELEMENT_BYTES, FLAT_ELEMENT_BYTES, TEXT_BYTES,
    };

    fn parse(src: &str) -> Vec<Element> {
        super::parse(src).unwrap()
    }

    #[test]
    fn paragraphs_and_cells_past_the_budget_are_refused() {
        let src = "w\n\n".repeat(1000);
        let within = |memory: u64| parse_within(&src, Budget::new(memory, 0));
        let need = 1000 * (ELEMENT_BYTES + TEXT_BYTES);
        assert_eq!(within(need).unwrap().len(), 1000);
        assert!(within(need - 1).is_err());
        // A header of 3 cells over a row of 1, which the parser gives the
        // header's cells: 6 cells, 4 that hold text and the table, for
        // Markdown.
        let src = "| a | b | c |\n|---|---|---|\n| 1 |\n";
        let need = 6 * CELL_BYTES + 5 * FLAT_ELEMENT_BYTES + 4 * TEXT_BYTES;
        let within =
            |memory: u64| parse_within(src, Budget::new(memory, 0).for_output(Output::Flat));
        assert_eq!(within(need).unwrap()[0].cells.as_ref().unwrap()[1].len(), 3);
        assert!(within(need - 1).is_err());
    }

    #[test]
    fn a_bands_label_the_output_copies_into_each_cell_is_spent() {
        // A band of 1 MB over 100 columns and 40 rows: DoCO writes it as the
        // section label of each of their 4,000 cells, 4 GB of copies.
        let band = format!("| **{}** |{}\n", "B".repeat(1 << 20), " |".repeat(99));
        let row = format!("|{}\n", " 1 |".repeat(100));
        let src = format!(
            "|{}\n|{}\n{band}{}",
            " h |".repeat(100),
            "---|".repeat(100),
            row.repeat(40)
        );
        let refused = super::parse(&src).unwrap_err();
        assert!(refused.to_string().contains("copies"), "{refused}");
        // Markdown and the text write it once.
        for output in [Output::Flat, Output::Text] {
            let e = converting_to(output, || super::parse(&src)).unwrap();
            assert_eq!(e[0].sub_headers, Some(vec![1]));
        }
    }

    fn kinds(els: &[Element]) -> Vec<&str> {
        els.iter().map(|e| e.kind.as_str()).collect()
    }

    #[test]
    fn a_loose_list_is_still_a_list() {
        let els = parse("- First\n\n- Second\n");
        assert_eq!(kinds(&els), ["doc:ListItem", "doc:ListItem"]);
        assert_eq!(els[1].text, "Second");
        // An item's paragraphs are the item's.
        let els = parse("- First\n\n  more of it\n\n- Second\n\nAfter.\n");
        assert_eq!(
            kinds(&els),
            ["doc:ListItem", "doc:ListItem", "doco:Paragraph"]
        );
        assert_eq!(els[0].text, "First more of it");
    }

    #[test]
    fn a_nested_item_leaves_its_parent_an_item() {
        let els = parse("- Parent\n  - Child\n  - Sibling\n- Next\n");
        let read: Vec<(&str, &str)> = els
            .iter()
            .map(|e| (e.kind.as_str(), e.text.as_str()))
            .collect();
        assert_eq!(
            read,
            [
                ("doc:ListItem", "Parent"),
                ("doc:ListItem", "Child"),
                ("doc:ListItem", "Sibling"),
                ("doc:ListItem", "Next"),
            ]
        );
    }

    #[test]
    fn headings_carry_their_declared_level() {
        let els = parse("# One\n\ntext\n\n### Three\n");
        assert_eq!(
            kinds(&els),
            ["doco:SectionTitle", "doco:Paragraph", "doco:SectionTitle"]
        );
        assert_eq!(els[0].level, Some(1));
        assert_eq!(els[2].level, Some(3));
        assert_eq!(els[0].text, "One");
    }

    #[test]
    fn no_element_claims_geometry() {
        let els = parse("# Title\n\n- a\n- b\n\n| x | y |\n|---|---|\n| 1 | 2 |\n");
        assert!(
            els.iter().all(|e| e.bbox.is_none()),
            "markdown has no layout; a zeroed box would read as a real position"
        );
        assert!(els.iter().all(|e| e.provenance == "markdown"));
    }

    #[test]
    fn a_tables_header_row_is_known_not_inferred() {
        let els = parse("| Year | Total |\n|---|---|\n| 2023 | 10 |\n| 2024 | 12 |\n");
        let t = els.iter().find(|e| e.kind == "doco:Table").unwrap();
        assert_eq!(t.header_rows, Some(1));
        let cells = t.cells.as_ref().unwrap();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0], vec!["Year", "Total"]);
        assert_eq!(cells[2], vec!["2024", "12"]);
    }

    #[test]
    fn a_bold_first_cell_alone_on_its_row_is_a_section_band() {
        let els = parse(
            "| Item | Qty | Price |\n|---|---|---|\n\
             | **Food** | | |\n\
             | Fish pie | 2 | 14.50 |\n\
             | Bread | | 3.00 |\n\
             | **Drinks** | | |\n\
             | Lemonade | 3 | 3.20 |\n\
             | **Total** | | |\n",
        );
        let t = els.iter().find(|e| e.kind == "doco:Table").unwrap();
        // Bread is missing a value, not a band; the trailing bold row has
        // nothing beneath it to label.
        assert_eq!(t.sub_headers, Some(vec![1, 4]));
        assert_eq!(t.cells.as_ref().unwrap()[1][0], "Food");
        // Bold that is not alone on its row is data.
        let els = parse("| a | b |\n|---|---|\n| **x** | 1 |\n| y | 2 |\n");
        assert_eq!(els[0].sub_headers, None);
    }

    #[test]
    fn list_items_are_separate_elements() {
        let els = parse("- first\n- second\n- third\n");
        assert_eq!(kinds(&els), ["doc:ListItem"; 3]);
        assert_eq!(els[1].text, "second");
    }

    #[test]
    fn wrapped_lines_join_into_one_paragraph() {
        // A soft break is a wrap, not a boundary — the same rule the PDF
        // block assembler follows, so NER sees no spurious sentence break.
        let els = parse("one line\ncontinues here\n\nsecond para\n");
        assert_eq!(els.len(), 2);
        assert_eq!(els[0].text, "one line continues here");
    }

    #[test]
    fn ragged_rows_are_padded_to_a_rectangle() {
        let els = parse("| a | b | c |\n|---|---|---|\n| 1 |\n");
        let t = els.iter().find(|e| e.kind == "doco:Table").unwrap();
        let cells = t.cells.as_ref().unwrap();
        assert!(cells.iter().all(|r| r.len() == 3));
    }

    #[test]
    fn a_link_survives_as_a_span_of_the_element_text() {
        let e = parse("See [the filing](https://sec.example/x) for detail.");
        let links = e[0].links.as_ref().expect("links");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].href(), "https://sec.example/x");
        let (b, end) = links[0].span().unwrap();
        let anchor: String = e[0].text.chars().skip(b).take(end - b).collect();
        assert_eq!(anchor, "the filing");
    }

    #[test]
    fn links_on_one_line_are_located_in_linear_time() {
        // Each link counted the characters of the paragraph before it:
        // 600,000 on one line, 4 MB, took 16 s.
        let n = 1_000_000;
        let src = format!("é {}[ç](u) ü", "[a](b) ".repeat(n));
        // As Markdown: as DoCO, a million links are past the budget.
        let start = std::time::Instant::now();
        let e = converting_to(Output::Flat, || parse(&src));
        let took = start.elapsed();
        let links = e[0].links.as_ref().unwrap();
        assert_eq!(links.len(), n + 1);
        // Spans in characters, past the multi-byte ones.
        assert_eq!(links[1].span(), Some((4, 5)));
        assert_eq!(links[n].span(), Some((2 + 2 * n, 3 + 2 * n)));
        assert_eq!(links[n].href(), "u");
        assert!(took.as_secs() < 10, "{took:?}");
    }

    #[test]
    fn each_link_is_spent_before_it_is_kept() {
        // DoCO writes each as a node: two million on one line aborted.
        let src = "[a](bc) ".repeat(100);
        let within = |memory: u64| parse_within(&src, Budget::new(memory, 0));
        let text = 2 * 100 - 1;
        let need =
            ELEMENT_BYTES + text * TEXT_BYTES + 100 * (ELEMENT_BYTES + CELL_BYTES + 2 * TEXT_BYTES);
        assert_eq!(within(need).unwrap()[0].links.as_ref().unwrap().len(), 100);
        assert!(within(need - 1).is_err());
    }

    #[test]
    fn a_link_inside_a_table_cell_does_not_escape_onto_the_next_paragraph() {
        let e = parse("| a | b |\n|---|---|\n| [x](https://e.example) | y |\n\nAfter.");
        let after = e.iter().find(|x| x.text == "After.").expect("paragraph");
        assert!(after.links.is_none());
    }
}
