//! Plain text to DoCO-typed document elements.
//!
//! A text file declares nothing but its lines. A blank line ends a
//! paragraph, and the line breaks inside one are kept: a hard-wrapped
//! paragraph keeps its wrap, and an address keeps its lines. A line that
//! starts with a list marker (`-`, `*`, `•`, `1.`, `2)`) is a list item, and
//! the lines after it continue it until the next marker or a blank line.
//!
//! It is not Markdown. A file of text is read for what it shows: an
//! indented line is not code, a row of `=` under a line does not make that
//! line a heading, and `*` and `_` are characters like any other.
//!
//! Its headings are the ones a text shows without declaring them, a line
//! that is a structural keyword and its number (`CHAPTER IV`, `§ 7001.
//! Definitions.`); see [`fluree_doc_model::outline`]. Such a line at the top
//! of a paragraph is set apart from the lines under it, so a heading with
//! its title on the next line reads as a heading and its title, and not as
//! a sentence wrapped across two lines.
//!
//! There is no geometry, and no pages: `bbox` is `None` and `page` is 0.

use fluree_doc_model::{outline, Element};

/// Decode a text file's bytes. A byte-order mark decides; then UTF-8, when
/// the bytes are valid UTF-8; then Windows-1252, the commonest encoding of
/// text that is not.
pub fn decode(bytes: &[u8]) -> String {
    if let Some((enc, bom)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc
            .decode_without_bom_handling(&bytes[bom..])
            .0
            .into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

/// Is this text Markdown saved under a text file's name?
///
/// Notes, a model's answers and converted documents are written in Markdown
/// and saved as `.txt` all the time, and their headings and tables are then
/// in its markup. Only what plain text does not write by chance counts: two
/// headings marked with `#`, the rule row under a table's header
/// (`|---|---|`), a fenced block, two phrases in `**bold**`, or a link
/// written `[label](address)`. A line underlined with dashes does not:
/// plain text underlines a label, a signature and a column's header that
/// way, and Markdown would make each a heading.
pub fn is_markdown(src: &str) -> bool {
    let (mut headings, mut fences, mut bold) = (0, 0, 0);
    for line in src.lines() {
        bold += line.matches("**").count() / 2;
        if link(line) {
            return true;
        }
        let t = line.trim();
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes)
            && line[hashes..].starts_with([' ', '\t'])
            && !line[hashes..].trim().is_empty()
        {
            headings += 1;
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            fences += 1;
        }
        if table_rule(t) {
            return true;
        }
    }
    headings >= 2 || fences >= 2 || bold >= 2
}

/// A Markdown link on the line: `[label](https://…)`, `](/path)`,
/// `](#anchor)`, `](mailto:…)`.
fn link(line: &str) -> bool {
    line.match_indices("](").any(|(at, _)| {
        let target = &line[at + 2..];
        line[..at].contains('[')
            && ["http://", "https://", "/", "#", "mailto:"]
                .iter()
                .any(|p| target.starts_with(p))
    })
}

/// A Markdown table's rule row: `|---|:--:|`, `--- | ---`, two cells at
/// least.
fn table_rule(t: &str) -> bool {
    let inner = t.trim_matches('|');
    let cells: Vec<&str> = inner.split('|').map(str::trim).collect();
    t.contains('|')
        && cells.len() >= 2
        && cells.iter().all(|c| {
            let dashes = c.trim_matches(':');
            dashes.len() >= 3 && dashes.chars().all(|ch| ch == '-')
        })
}

/// Parse plain text into elements in reading order.
pub fn parse(src: &str) -> Vec<Element> {
    // Windows ends a line with CR LF and the classic Mac OS, as Word still
    // exports, with CR alone.
    let src = src.replace("\r\n", "\n").replace('\r', "\n");
    let src = src.strip_prefix('\u{feff}').unwrap_or(&src);
    let mut out = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    for line in src.split('\n') {
        if line.trim().is_empty() {
            read_block(&block, &mut out);
            block.clear();
        } else {
            block.push(line);
        }
    }
    read_block(&block, &mut out);
    outline::infer_headings(&mut out);
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    out
}

/// The text after a list marker at the start of a line: `- `, `* `, `• `,
/// `1. `, `2) `. A bullet is dropped; a number is kept, since a clause's
/// number is how it is cited.
fn item(line: &str) -> Option<&str> {
    let t = line.trim_start();
    for bullet in ["- ", "* ", "• ", "· ", "▪ ", "◦ "] {
        if let Some(rest) = t.strip_prefix(bullet) {
            return Some(rest.trim());
        }
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    let rest = &t[digits..];
    ((1..=2).contains(&digits) && (rest.starts_with(". ") || rest.starts_with(") ")))
        .then(|| t.trim())
}

/// The elements of one block of lines with no blank line between them.
fn read_block(lines: &[&str], out: &mut Vec<Element>) {
    let mut lines = lines;
    // A heading opening the block is a paragraph of its own, and so is a
    // heading straight under it (`TITLE 11. LAW` over `DIVISION 6.`). One
    // further down is left in its paragraph: there it is more likely a
    // reference wrapped onto a line of its own (`… as set out in` /
    // `Article 11.`).
    while let Some((first, rest)) = lines.split_first() {
        if !outline::is_heading(first) || item(first).is_some() {
            break;
        }
        out.push(paragraph(first.trim().to_string()));
        lines = rest;
    }
    let mut text: Vec<&str> = Vec::new();
    let mut in_item = false;
    let flush = |text: &mut Vec<&str>, in_item: bool, out: &mut Vec<Element>| {
        if !text.is_empty() {
            let joined = text.join("\n");
            out.push(if in_item {
                element("doc:ListItem", joined)
            } else {
                paragraph(joined)
            });
            text.clear();
        }
    };
    for line in lines {
        if let Some(rest) = item(line) {
            flush(&mut text, in_item, out);
            in_item = true;
            if !rest.is_empty() {
                text.push(rest);
            }
            continue;
        }
        text.push(line.trim());
    }
    flush(&mut text, in_item, out);
}

fn paragraph(text: String) -> Element {
    element("doco:Paragraph", text)
}

fn element(kind: &str, text: String) -> Element {
    Element::new(kind, text, "text", "text")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(src: &str) -> Vec<(String, String)> {
        parse(src).into_iter().map(|e| (e.kind, e.text)).collect()
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(k, t)| (k.to_string(), t.to_string()))
            .collect()
    }

    #[test]
    fn a_heading_over_its_title_is_a_heading_and_its_title() {
        // The statute as plain text, each heading's title on the line under
        // it with no blank line between.
        let els = parse(
            "CHAPTER IV\nController and processor\n\nArticle 30\nRecords of processing activities\n\n\
             1. Each controller shall maintain a record.\n",
        );
        let got: Vec<(&str, &str, Option<usize>)> = els
            .iter()
            .map(|e| (e.kind.as_str(), e.text.as_str(), e.level))
            .collect();
        assert_eq!(
            got,
            [
                (
                    "doco:SectionTitle",
                    "CHAPTER IV Controller and processor",
                    Some(1)
                ),
                (
                    "doco:SectionTitle",
                    "Article 30 Records of processing activities",
                    Some(2)
                ),
                (
                    "doc:ListItem",
                    "1. Each controller shall maintain a record.",
                    None
                ),
            ]
        );
        assert!(els
            .iter()
            .all(|e| e.provenance == "text" && e.bbox.is_none()));
        assert_eq!(els[2].id, "elem-00003");
    }

    #[test]
    fn paragraphs_keep_their_lines_and_lists_are_items() {
        assert_eq!(
            read("Dear Ada,\r\n\r\nThe pilot starts in May and\r\nruns to June.\r\n\r\nNext steps:\r\n- sign the order\r\n- book the kickoff\r\n  for May\r\n\r\nBen Ruiz\r\nOperations\r\n"),
            pairs(&[
                ("doco:Paragraph", "Dear Ada,"),
                ("doco:Paragraph", "The pilot starts in May and\nruns to June."),
                ("doco:Paragraph", "Next steps:"),
                ("doc:ListItem", "sign the order"),
                ("doc:ListItem", "book the kickoff\nfor May"),
                ("doco:Paragraph", "Ben Ruiz\nOperations"),
            ])
        );
    }

    #[test]
    fn it_is_not_markdown() {
        assert_eq!(
            read("Totals\n======\n\n    indented, not code\n\n# not a heading *or* _emphasis_"),
            pairs(&[
                ("doco:Paragraph", "Totals\n======"),
                ("doco:Paragraph", "indented, not code"),
                ("doco:Paragraph", "# not a heading *or* _emphasis_"),
            ])
        );
    }

    #[test]
    fn headings_stacked_at_the_top_of_a_block_each_stand_alone() {
        let els = read(
            "TITLE 11. LAW\nDIVISION 6. PRIVACY AGENCY\n\n§ 7001. Definitions.\n\nThese apply, as set out in\nArticle 11.\n",
        );
        assert_eq!(
            els,
            pairs(&[
                ("doco:SectionTitle", "TITLE 11. LAW"),
                ("doco:SectionTitle", "DIVISION 6. PRIVACY AGENCY"),
                ("doco:SectionTitle", "§ 7001. Definitions."),
                // A reference wrapped onto its own line stays in its sentence.
                ("doco:Paragraph", "These apply, as set out in\nArticle 11."),
            ])
        );
    }

    #[test]
    fn markdown_saved_as_text_is_told_by_its_markup() {
        assert!(is_markdown("# Minutes\n\nText.\n\n## Attendance\n"));
        assert!(is_markdown("| Year | Total |\n|---|---:|\n| 2024 | 12 |\n"));
        assert!(is_markdown("Run it:\n\n```\nfdoc convert a.pdf\n```\n"));
        assert!(is_markdown("**Source:** Reuters\n**Author:** A. Park\n"));
        assert!(is_markdown("See [the filing](https://example.org/f).\n"));
        // What plain text writes by chance.
        assert!(!is_markdown(
            "# of units\n\nPOLICY NUMBER:\n--------------\n"
        ));
        assert!(!is_markdown("/s/ Ada Park\n------------\nName  | Room\n"));
        assert!(!is_markdown("CHAPTER IV\nController and processor\n"));
        assert!(!is_markdown("Note [1] (see above) and **one** emphasis.\n"));
    }

    #[test]
    fn bytes_decode_by_mark_then_utf8_then_windows_1252() {
        assert_eq!(decode(b"\xef\xbb\xbfcaf\xc3\xa9"), "café");
        assert_eq!(decode("café".as_bytes()), "café");
        assert_eq!(decode(b"\x93caf\xe9\x94"), "\u{201c}café\u{201d}");
        // Classic Mac line ends.
        assert_eq!(read("one\rtwo\r\rthree").len(), 2);
    }
}
