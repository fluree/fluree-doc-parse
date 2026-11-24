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
//!
//! What a text makes is counted against the document's [`Budget`] before
//! it is kept: each line's text as it is read into its paragraph, and each
//! element. Past it, the text is refused ([`TooLarge`]).

use fluree_doc_model::budget::{Budget, TooLarge};
use fluree_doc_model::{outline, Element};

/// Decode a text file's bytes. A byte-order mark decides; then UTF-8, when
/// the bytes are valid UTF-8; then Windows-1252, the commonest encoding of
/// text that is not.
///
/// Bytes handed over owned that are UTF-8 become the string as they are,
/// not copied: a file read whole is then held once.
pub fn decode(bytes: impl Into<Vec<u8>>) -> String {
    let mut bytes = bytes.into();
    if let Some((enc, bom)) = encoding_rs::Encoding::for_bom(&bytes) {
        if let std::borrow::Cow::Owned(s) = enc.decode_without_bom_handling(&bytes[bom..]).0 {
            return s;
        }
        // UTF-8 after its mark: the bytes, less the mark.
        bytes.drain(..bom);
    }
    match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => encoding_rs::WINDOWS_1252
            .decode(e.as_bytes())
            .0
            .into_owned(),
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

/// Parse plain text into elements in reading order, or say why it makes
/// more than one document may.
pub fn parse(src: &str) -> Result<Vec<Element>, TooLarge> {
    parse_within(src, Budget::default())
}

/// [`parse`], against `budget`.
fn parse_within(src: &str, mut budget: Budget) -> Result<Vec<Element>, TooLarge> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let mut out = Vec::new();
    // The block of lines read since the last blank one, as the bytes of
    // `src` it spans: held as a list of its lines, a block of a hundred
    // million short ones took 1.6 GB before it was read.
    let mut block: Option<(usize, usize)> = None;
    for (start, line) in lines(src) {
        if line.trim().is_empty() {
            if let Some((b, e)) = block.take() {
                read_block(&src[b..e], &mut out, &mut budget)?;
            }
        } else {
            // Spent as it is read: the line, and the line break its
            // paragraph keeps after it.
            budget.spend_text(line)?;
            budget.spend_text_cost(1)?;
            let end = start + line.len();
            block = Some((block.map_or(start, |(b, _)| b), end));
        }
    }
    if let Some((b, e)) = block {
        read_block(&src[b..e], &mut out, &mut budget)?;
    }
    outline::infer_headings(&mut out);
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    Ok(out)
}

/// The lines of `s`, each with the byte it starts at. Windows ends a line
/// with CR LF and the classic Mac OS, as Word still exports, with CR alone:
/// each is a line's end, as LF is. They are told apart in place, where the
/// text with its ends rewritten to LF took two more copies of it.
fn lines(s: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut at = Some(0);
    std::iter::from_fn(move || {
        let start = at?;
        let rest = &s[start..];
        let Some(end) = rest.find(['\r', '\n']) else {
            at = None;
            return Some((start, rest));
        };
        let ending = if rest[end..].starts_with("\r\n") {
            2
        } else {
            1
        };
        at = Some(start + end + ending);
        Some((start, &rest[..end]))
    })
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

/// The elements of one block of lines with no blank line between them,
/// each spent as it is made; its lines' text was spent as they were read.
fn read_block(block: &str, out: &mut Vec<Element>, budget: &mut Budget) -> Result<(), TooLarge> {
    let mut lines = lines(block).map(|(_, line)| line).peekable();
    // A heading opening the block is a paragraph of its own, and so is a
    // heading straight under it (`TITLE 11. LAW` over `DIVISION 6.`). One
    // further down is left in its paragraph: there it is more likely a
    // reference wrapped onto a line of its own (`… as set out in` /
    // `Article 11.`).
    while let Some(first) = lines.next_if(|l| outline::is_heading(l) && item(l).is_none()) {
        budget.spend_elements(1)?;
        out.push(paragraph(first.trim().to_string()));
    }
    // The paragraph or item being read, its lines joined as they come.
    let mut text = String::new();
    let mut in_item = false;
    let flush = |text: &mut String,
                 in_item: bool,
                 out: &mut Vec<Element>,
                 budget: &mut Budget|
     -> Result<(), TooLarge> {
        if !text.is_empty() {
            budget.spend_elements(1)?;
            let joined = std::mem::take(text);
            out.push(if in_item {
                element("doc:ListItem", joined)
            } else {
                paragraph(joined)
            });
        }
        Ok(())
    };
    let join = |text: &mut String, line: &str| {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(line);
    };
    for line in lines {
        if let Some(rest) = item(line) {
            flush(&mut text, in_item, out, budget)?;
            in_item = true;
            if !rest.is_empty() {
                join(&mut text, rest);
            }
            continue;
        }
        join(&mut text, line.trim());
    }
    flush(&mut text, in_item, out, budget)
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
        parse(src)
            .unwrap()
            .into_iter()
            .map(|e| (e.kind, e.text))
            .collect()
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
        )
        .unwrap();
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
        assert_eq!(decode(vec![0xef, 0xbb, 0xbf, b'a']), "a");
        // UTF-8 handed over owned is the string, not a copy of it.
        for bytes in [b"text".to_vec(), b"\xef\xbb\xbftext".to_vec()] {
            let at = bytes.as_ptr();
            let text = decode(bytes);
            assert_eq!(text.as_ptr(), at);
        }
        assert_eq!(decode(b"\xff\xfea\x00".to_vec()), "a");
        // Classic Mac line ends.
        assert_eq!(read("one\rtwo\r\rthree").len(), 2);
    }

    #[test]
    fn a_line_ends_at_lf_cr_lf_or_cr_alone() {
        let got: Vec<(usize, &str)> = lines("a\r\nbc\rd\n\r\r\ne\r\r\n").collect();
        assert_eq!(
            got,
            [
                (0, "a"),
                (3, "bc"),
                (6, "d"),
                (8, ""),
                (9, ""),
                (11, "e"),
                (13, ""),
                (15, "")
            ]
        );
        assert_eq!(lines("").collect::<Vec<_>>(), [(0, "")]);
        // Read as the text with its ends rewritten to LF reads.
        let src = "Dear Ada,\r\rThe pilot\r\nruns.\r\n\r\n- one\r- two\r\n  more\rEnd";
        assert_eq!(
            read(src),
            read(&src.replace("\r\n", "\n").replace('\r', "\n"))
        );
        assert_eq!(read(src).len(), 4);
    }

    #[test]
    fn lines_past_the_budget_are_refused_before_they_are_made() {
        use fluree_doc_model::budget::{ELEMENT_BYTES, TEXT_BYTES};
        // A block of 10,000 lines of `abcd`, each held before the block is
        // read: its text and line breaks, and the one paragraph it makes.
        let src = "abcd\n".repeat(10_000);
        let need = 10_000 * 5 * TEXT_BYTES + ELEMENT_BYTES;
        let within = |memory: u64| parse_within(&src, Budget::new(memory, u64::MAX));
        assert_eq!(within(need).unwrap().len(), 1);
        let err = within(need - 1).unwrap_err().to_string();
        assert!(err.contains("past"), "{err}");
        // Refused as the lines are read, before the block is: a budget the
        // lines pass is not reached by the paragraph.
        let lines = within(10_000 * 5 * TEXT_BYTES - 1).unwrap_err().to_string();
        assert!(lines.starts_with("text past"), "{lines}");
    }
}
