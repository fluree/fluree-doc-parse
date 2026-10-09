//! A plain-text body as paragraphs and list items.

use fluree_doc_model::Element;

/// Undo `format=flowed` (RFC 3676): a line ending in a space continues on
/// the next line at the same quote depth. Clients that send it wrap every
/// paragraph at 72 columns, and the wrap is not the author's.
pub fn unflow(text: &str, delsp: bool) -> String {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut open = false;
    for line in text.lines() {
        let depth = line.chars().take_while(|&c| c == '>').count();
        let content = &line[depth..];
        // Space-stuffing: a leading space protects a line that would
        // otherwise read as quoted or as `From `.
        let content = content.strip_prefix(' ').unwrap_or(content);
        let flowed = content.ends_with(' ') && content != "-- ";
        let piece = if flowed && delsp {
            &content[..content.len() - 1]
        } else {
            content
        };
        match out.last_mut() {
            Some((d, s)) if open && *d == depth => s.push_str(piece),
            _ => out.push((depth, piece.to_string())),
        }
        open = flowed;
    }
    out.into_iter()
        .map(|(d, s)| {
            if d == 0 {
                s
            } else {
                format!("{} {s}", ">".repeat(d))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A list marker at the start of a line: `- `, `* `, `• `, `1. `, `2) `.
/// Returns the text after it.
fn item(line: &str) -> Option<&str> {
    let t = line.trim_start();
    for bullet in ["- ", "* ", "• ", "· ", "▪ ", "◦ ", "o "] {
        if let Some(rest) = t.strip_prefix(bullet) {
            // `o ` is Outlook's rendering of a second-level bullet; as a
            // plain word it would be rare at a line's start.
            if bullet != "o " || line.starts_with(char::is_whitespace) {
                return Some(rest.trim());
            }
        }
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if (1..=2).contains(&digits) {
        let rest = &t[digits..];
        if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some(r.trim());
        }
    }
    None
}

/// Remove `[cid:image001.png@01DA…]`, which Outlook writes where an inline
/// image was, and `<image001.png>`, which Apple Mail writes: a reference to
/// an attachment, not words.
pub fn strip_cid(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(i) = rest.find("[cid:") {
        out.push_str(&rest[..i]);
        rest = rest[i..].find(']').map_or("", |j| &rest[i + j + 1..]);
    }
    out.push_str(rest);
    let mut rest = std::mem::take(&mut out);
    while let Some((i, len)) = image_name(&rest) {
        out.push_str(&rest[..i]);
        rest = rest[i + len..].to_string();
    }
    out.push_str(&rest);
    out
}

/// Where `<name.png>` is in `text`, and its length: a file name in angle
/// brackets, not an address or a link.
fn image_name(text: &str) -> Option<(usize, usize)> {
    const IMAGES: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".bmp", ".heic", ".webp"];
    let mut from = 0;
    while let Some(i) = text[from..].find('<').map(|i| from + i) {
        let j = text[i..].find('>')?;
        let inner = &text[i + 1..i + j];
        let lower = inner.to_ascii_lowercase();
        let plain = !inner.is_empty()
            && !inner.contains(|c: char| c.is_whitespace() || matches!(c, '@' | ':' | '/' | '<'));
        if plain && IMAGES.iter().any(|x| lower.ends_with(x)) {
            return Some((i, j + 1));
        }
        from = i + 1;
    }
    None
}

/// A line as it reads: without image placeholders, and trimmed of spaces and
/// of the zero-width characters clients leave at the ends of lines (Apple
/// Mail a byte-order mark at the start of a quote, Outlook spaces of no
/// width after a name).
pub fn visible(line: &str) -> String {
    strip_cid(line)
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '\u{200b}' | '\u{feff}'))
        .to_string()
}

/// Paragraphs and list items from a body's lines, in order.
///
/// A blank line ends a paragraph. Inside one, lines keep their breaks: a
/// signature's name, title and company are three lines, not one sentence.
/// Consecutive lines with list markers are list items, and an indented line
/// under one continues it.
pub fn elements(lines: &[String], provenance: &'static str, out: &mut Vec<Element>) {
    let mut para: Vec<String> = Vec::new();
    let mut last_item: Option<usize> = None;
    let flush = |para: &mut Vec<String>, out: &mut Vec<Element>| {
        let text = para.join("\n").trim().to_string();
        para.clear();
        if !text.is_empty() {
            out.push(element("doco:Paragraph", text, provenance));
        }
    };
    for raw in lines {
        let line = strip_cid(raw);
        let text = visible(&line);
        if text.is_empty() {
            flush(&mut para, out);
            last_item = None;
            continue;
        }
        if let Some(rest) = item(&line) {
            flush(&mut para, out);
            if !rest.is_empty() {
                out.push(element("doc:ListItem", rest.to_string(), provenance));
                last_item = Some(out.len() - 1);
            }
            continue;
        }
        if let (Some(i), true) = (last_item, line.starts_with(char::is_whitespace)) {
            out[i].text.push(' ');
            out[i].text.push_str(&text);
            continue;
        }
        last_item = None;
        para.push(text);
    }
    flush(&mut para, out);
}

pub fn element(kind: &str, text: String, provenance: &'static str) -> Element {
    Element::new(kind, text, provenance, provenance)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Vec<(String, String)> {
        let lines: Vec<String> = text.lines().map(String::from).collect();
        let mut out = Vec::new();
        elements(&lines, "eml", &mut out);
        out.into_iter().map(|e| (e.kind, e.text)).collect()
    }

    #[test]
    fn paragraphs_keep_their_lines_and_lists_are_items() {
        assert_eq!(
            read("Hi Kai,\n\nNext steps:\n- sign the order\n- book the kickoff\n  for May\n\nLena Holt\nOperations Lead\n[cid:image001.png@01DA]"),
            vec![
                ("doco:Paragraph".into(), "Hi Kai,".into()),
                ("doco:Paragraph".into(), "Next steps:".into()),
                ("doc:ListItem".into(), "sign the order".into()),
                ("doc:ListItem".into(), "book the kickoff for May".into()),
                ("doco:Paragraph".into(), "Lena Holt\nOperations Lead".into()),
            ]
        );
    }

    #[test]
    fn image_placeholders_and_zero_width_marks_are_not_text() {
        assert_eq!(
            read("\u{feff}Hi Kai,\n\nLena Holt \u{200b}\u{200b}\nOps <image001.png>\n<lena@example.com>"),
            vec![
                ("doco:Paragraph".into(), "Hi Kai,".into()),
                (
                    "doco:Paragraph".into(),
                    "Lena Holt\nOps\n<lena@example.com>".into()
                ),
            ]
        );
    }

    #[test]
    fn flowed_text_rejoins_its_soft_breaks() {
        assert_eq!(
            unflow(
                "The pilot starts \nin May.\n> Quoted and \n> wrapped.\n-- \nKai",
                false
            ),
            "The pilot starts in May.\n> Quoted and wrapped.\n-- \nKai"
        );
        assert_eq!(unflow("日本 \n語", true), "日本語");
    }
}
