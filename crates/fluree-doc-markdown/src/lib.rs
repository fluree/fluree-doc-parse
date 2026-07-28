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

use fluree_doc_model::{Element, Link};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Parse Markdown into document elements in reading order.
pub fn parse(src: &str) -> Vec<Element> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_FOOTNOTES);
    Reader::default().run(Parser::new_ext(src, opts))
}

#[derive(Default)]
struct Reader {
    out: Vec<Element>,
    text: String,
    /// Heading level being collected, if any.
    heading: Option<usize>,
    /// Depth of nested lists; item text is collected at the innermost.
    list_depth: usize,
    in_item: bool,
    in_code: bool,
    /// Table under construction: rows, and whether the header row is open.
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    in_table: bool,
    in_table_head: bool,
    header_rows: usize,
    /// Links closed so far in the text under construction, and the one still
    /// open, as `(char offset where its anchor begins, target)`.
    links: Vec<Link>,
    open_link: Option<(usize, String)>,
}

impl Reader {
    fn element(&self, kind: &str, text: String, level: Option<usize>) -> Element {
        Element {
            id: String::new(),
            kind: kind.into(),
            page: 0,
            bbox: None,
            text,
            level,
            cells: None,
            header_rows: None,
            sub_headers: None,
            merged_down: None,
            merged_left: None,
            figure: None,
            links: None,
            provenance: "markdown",
            evidence: "markdown",
        }
    }

    fn flush_text(&mut self, kind: &str, level: Option<usize>) {
        let raw = std::mem::take(&mut self.text);
        let links = std::mem::take(&mut self.links);
        self.open_link = None;
        let t = raw.trim().to_string();
        if t.is_empty() {
            return;
        }
        let mut e = self.element(kind, t, level);
        // `element` holds the trimmed text; the offsets were taken against
        // the untrimmed string, so they shift by whatever the trim removed.
        let lead = raw.chars().take_while(|c| c.is_whitespace()).count();
        let len = e.text.chars().count();
        let shifted: Vec<Link> = links
            .into_iter()
            .filter_map(|l| {
                let (b, end) = l.span()?;
                (b >= lead && end - lead <= len)
                    .then(|| Link::uri(l.href()).spanning(b - lead, end - lead))
            })
            .collect();
        if !shifted.is_empty() {
            e.links = Some(shifted);
        }
        self.out.push(e);
    }

    /// Mark where a link's anchor starts. Its extent is known only when the
    /// closing event arrives, because the anchor's own text is emitted
    /// between the two.
    fn open_link(&mut self, dest: &str) {
        if dest.is_empty() {
            return;
        }
        self.open_link = Some((self.text.chars().count(), dest.to_string()));
    }

    fn close_link(&mut self) {
        let Some((begin, dest)) = self.open_link.take() else {
            return;
        };
        let end = self.text.chars().count();
        if end > begin {
            self.links.push(Link::uri(dest).spanning(begin, end));
        }
    }

    fn run(mut self, parser: Parser<'_>) -> Vec<Element> {
        for ev in parser {
            match ev {
                Event::Start(Tag::Heading { level, .. }) => {
                    self.flush_text("doco:Paragraph", None);
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
                    self.flush_text("doco:SectionTitle", level);
                }
                Event::Start(Tag::List(_)) => {
                    self.flush_text("doco:Paragraph", None);
                    self.list_depth += 1;
                }
                Event::End(TagEnd::List(_)) => {
                    self.list_depth = self.list_depth.saturating_sub(1);
                }
                Event::Start(Tag::Item) => {
                    self.flush_text("doco:Paragraph", None);
                    self.in_item = true;
                }
                Event::End(TagEnd::Item) => {
                    self.flush_text("doco:ListItem", None);
                    self.in_item = false;
                }
                Event::Start(Tag::CodeBlock(_)) => {
                    self.flush_text("doco:Paragraph", None);
                    self.in_code = true;
                }
                Event::End(TagEnd::CodeBlock) => {
                    self.in_code = false;
                    // A code block is verbatim: keep its newlines rather than
                    // folding it into a paragraph's single flow.
                    let t = std::mem::take(&mut self.text);
                    if !t.trim().is_empty() {
                        let e = self.element("doco:Paragraph", t.trim_end().to_string(), None);
                        self.out.push(e);
                    }
                }
                Event::Start(Tag::Table(_)) => {
                    self.flush_text("doco:Paragraph", None);
                    self.in_table = true;
                    self.rows.clear();
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
                    let c = self.text.trim().to_string();
                    self.text.clear();
                    // A cell's text leaves the buffer without becoming an
                    // element, so any link inside it has nothing to attach to
                    // and must not follow the next paragraph out.
                    self.links.clear();
                    self.open_link = None;
                    self.row.push(c);
                }
                Event::End(TagEnd::Table) => {
                    self.in_table = false;
                    let rows = std::mem::take(&mut self.rows);
                    if rows.is_empty() {
                        continue;
                    }
                    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
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
                    let mut e = self.element("doco:Table", text, None);
                    e.header_rows = Some(self.header_rows);
                    e.cells = Some(rows);
                    self.out.push(e);
                }
                Event::End(TagEnd::Paragraph) => self.flush_text("doco:Paragraph", None),
                Event::Start(Tag::Link { dest_url, .. }) => self.open_link(&dest_url),
                Event::End(TagEnd::Link) => self.close_link(),
                Event::Text(t) | Event::Code(t) => self.text.push_str(&t),
                Event::SoftBreak => self.text.push(' '),
                Event::HardBreak => {
                    if self.in_code {
                        self.text.push('\n');
                    } else {
                        self.text.push(' ');
                    }
                }
                Event::Rule => self.flush_text("doco:Paragraph", None),
                _ => {}
            }
        }
        self.flush_text("doco:Paragraph", None);
        for (i, e) in self.out.iter_mut().enumerate() {
            e.id = format!("elem-{:05}", i + 1);
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(els: &[Element]) -> Vec<&str> {
        els.iter().map(|e| e.kind.as_str()).collect()
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
    fn list_items_are_separate_elements() {
        let els = parse("- first\n- second\n- third\n");
        assert_eq!(kinds(&els), ["doco:ListItem"; 3]);
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
    fn a_link_inside_a_table_cell_does_not_escape_onto_the_next_paragraph() {
        let e = parse("| a | b |\n|---|---|\n| [x](https://e.example) | y |\n\nAfter.");
        let after = e.iter().find(|x| x.text == "After.").expect("paragraph");
        assert!(after.links.is_none());
    }
}
