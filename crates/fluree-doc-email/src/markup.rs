//! Whether a plain-text alternative was rendered from the HTML, or only
//! had its tags stripped.
//!
//! The plain text is preferred because a mail client writes it: it renders
//! the HTML, states the quoting outright and carries the same words. Many
//! bulk senders build it with a converter instead, and some converters
//! strip the tags and stop there, leaving the style sheet's rules and every
//! character reference (`03&#47;14&#47;2026`) as text, or leave whole runs
//! of markup in. Such a part says less than the HTML it came from, so it is
//! set aside for that.
//!
//! A plain-text writer can mean `&amp;` or `<td>` literally, in a message
//! about markup. Then the HTML alternative shows them as text too, so only
//! markup that the HTML does not show counts.

/// Does this plain text carry markup that the HTML alternative does not
/// show as text? One piece is enough: a sender's converter that left one
/// left the rest of its rendering undone too.
pub fn unrendered(plain: &str, html: &str) -> bool {
    let marks = marks(plain);
    if marks.is_empty() {
        return false;
    }
    let shown = fluree_doc_html::parse(html)
        .into_iter()
        .map(|e| e.text)
        .collect::<Vec<_>>()
        .join("\n");
    marks.iter().any(|m| !shown.contains(m.as_str()))
}

/// Character references, tags and style rules in a text, each with its
/// whitespace collapsed as the HTML reader collapses what it shows.
fn marks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, c) in text.char_indices() {
        let found = match c {
            '&' => reference(&text[i..]),
            '<' => tag(&text[i..]),
            '{' => rule(&text[i..]),
            _ => None,
        };
        if let Some(m) = found {
            out.push(m.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    out
}

/// `&#47;`, `&#x2F;` or `&nbsp;` at the start of `s`.
fn reference(s: &str) -> Option<&str> {
    let body = &s[1..];
    let semi = body.bytes().take(33).position(|b| b == b';')?;
    let name = &body[..semi];
    let ok = if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
        !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit())
    } else if let Some(dec) = name.strip_prefix('#') {
        !dec.is_empty() && dec.bytes().all(|b| b.is_ascii_digit())
    } else {
        // No named reference is shorter than `lt`.
        name.len() >= 2
            && name.starts_with(|c: char| c.is_ascii_alphabetic())
            && name.bytes().all(|b| b.is_ascii_alphanumeric())
    };
    ok.then(|| &s[..semi + 2])
}

/// The elements an email's markup is made of. Only these count, so an
/// address in angle brackets, `<ben@example.com>`, or a link,
/// `<https://…>`, is never taken for one.
const TAGS: &[&str] = &[
    "a",
    "b",
    "blockquote",
    "body",
    "br",
    "center",
    "div",
    "em",
    "font",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "hr",
    "html",
    "i",
    "img",
    "li",
    "meta",
    "ol",
    "p",
    "span",
    "strong",
    "style",
    "table",
    "tbody",
    "td",
    "th",
    "thead",
    "tr",
    "u",
    "ul",
];

/// `<td align='left'>` or `</span>` at the start of `s`.
fn tag(s: &str) -> Option<&str> {
    let after = s[1..].strip_prefix('/').unwrap_or(&s[1..]);
    let len = after
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(after.len());
    let name = after[..len].to_ascii_lowercase();
    if !TAGS.contains(&name.as_str()) {
        return None;
    }
    if !after[len..].starts_with(|c: char| c == '>' || c == '/' || c.is_whitespace()) {
        return None;
    }
    // An image's address alone can run past a thousand characters.
    let end = s
        .char_indices()
        .skip(1)
        .take(4096)
        .find_map(|(i, c)| match c {
            '>' => Some(Some(i)),
            '<' => Some(None),
            _ => None,
        })??;
    Some(&s[..=end])
}

/// A style rule's block at the start of `s`: `{ color: #EEEEEE; }`.
fn rule(s: &str) -> Option<&str> {
    let end = s[1..].find(['{', '}']).map(|n| n + 1)?;
    if !s[end..].starts_with('}') {
        return None;
    }
    let inner = &s[1..end];
    let declares = inner.split(';').any(|d| {
        d.split_once(':').is_some_and(|(prop, value)| {
            let prop = prop.trim();
            prop.len() >= 3
                && prop.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
                && !value.trim().is_empty()
        })
    });
    (declares && inner.contains(';')).then(|| &s[..=end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_tags_and_rules_are_found() {
        assert_eq!(
            marks("Due 03&#47;14&#47;2026 &nbsp;&#x2F; R&D; AT&T;"),
            ["&#47;", "&#47;", "&nbsp;", "&#x2F;"]
        );
        assert_eq!(
            marks("<span style='color: #336699'><TD\nalign=left>x</td> <ben@example.com> <https://a.example/> a <b and c"),
            ["<span style='color: #336699'>", "<TD align=left>", "</td>"]
        );
        assert_eq!(
            marks(".banner\n{\n  color: #EEEEEE;\n}\nf(x) { return 1 } {\"a\": 1}"),
            ["{ color: #EEEEEE; }"]
        );
    }

    #[test]
    fn a_converter_that_stopped_at_the_tags_is_set_aside() {
        let html = "<html><head><style>.t { color: #EEEEEE; }</style></head>\
                    <body><p>Invoice Date 03&#47;14&#47;2026</p></body></html>";
        assert!(unrendered("Invoice Date 03&#47;14&#47;2026", html));
        assert!(unrendered(
            ".t\n{\n  color: #EEEEEE;\n}\nInvoice Date 03/14/2026",
            html
        ));
        assert!(!unrendered("Invoice Date 03/14/2026", html));
    }

    #[test]
    fn markup_the_writer_meant_is_kept() {
        // A message about markup: the HTML shows the same text, escaped.
        let html = "<div>Use &amp;amp; for an ampersand, and close each &lt;td&gt;.</div>";
        assert!(!unrendered(
            "Use &amp; for an ampersand, and close each <td>.",
            html
        ));
    }
}
