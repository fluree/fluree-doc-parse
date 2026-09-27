//! Address lists: `Ada Park <ada@example.com>, "Ortiz, Ben" <ben@example.com>`.

use crate::mime::{decode_words, unquote};
use fluree_doc_model::Mailbox;

/// An RFC 5322 address list, as `From:`, `To:` and `Cc:` headers write it.
///
/// Commas separate mailboxes only outside quotes, comments and angle
/// brackets, so `"Ortiz, Ben" <ben@example.com>` is one mailbox. A group
/// (`Team: a@example.com, b@example.com;`) contributes its members.
pub fn list(value: &str) -> Vec<Mailbox> {
    split_top(value, &[','])
        .iter()
        .filter_map(|item| {
            // A group's label names no mailbox, and its `;` closes it.
            let item = match top_level_colon(item) {
                Some(i) => &item[i + 1..],
                None => item.as_str(),
            };
            one(item.trim().trim_end_matches(';'))
        })
        .collect()
}

/// A list as a person or Outlook writes it in a quoted header, where `;`
/// separates recipients and a comma may be inside a name: `Ortiz, Ben
/// <ben@example.com>; Ada Park`.
pub fn loose(value: &str) -> Vec<Mailbox> {
    split_top(&strip_mailto(value), &[';'])
        .into_iter()
        .flat_map(|piece| {
            if piece.matches('@').count() > 1 {
                list(&piece)
            } else {
                one(&piece).into_iter().collect()
            }
        })
        .collect()
}

/// Outlook's plain text repeats an address as a link after it:
/// `ada@example.com<mailto:ada@example.com>`. The repeat is not a second
/// mailbox.
fn strip_mailto(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("<mailto:") {
        out.push_str(&rest[..i]);
        rest = rest[i..].find('>').map_or("", |j| &rest[i + j + 1..]);
    }
    out.push_str(rest);
    out
}

/// One mailbox: `Name <address>`, `address (Name)`, a bare address, or a
/// bare name.
pub fn one(item: &str) -> Option<Mailbox> {
    let item = item.trim();
    if item.is_empty() {
        return None;
    }
    let (name, address) = match (item.find('<'), item.rfind('>')) {
        (Some(lt), Some(gt)) if gt > lt => {
            let name = item[..lt].trim();
            // Outlook's plain text writes `ada@example.com<mailto:ada@…>`
            // inside the brackets; the address is what comes before.
            let inner = item[lt + 1..gt].split('<').next().unwrap_or("");
            (name.to_string(), clean_address(inner))
        }
        _ => {
            let (bare, comment) = match (item.find('('), item.rfind(')')) {
                (Some(o), Some(c)) if c > o => (
                    format!("{}{}", &item[..o], &item[c + 1..]),
                    item[o + 1..c].to_string(),
                ),
                _ => (item.to_string(), String::new()),
            };
            let bare = bare.trim();
            if bare.contains('@') && !bare.contains(char::is_whitespace) {
                (comment, clean_address(bare))
            } else {
                (item.to_string(), None)
            }
        }
    };
    let name = clean_name(&name);
    (name.is_some() || address.is_some()).then_some(Mailbox { name, address })
}

fn clean_address(a: &str) -> Option<String> {
    let a = a.trim().trim_start_matches("mailto:").trim();
    (!a.is_empty()).then(|| a.to_string())
}

fn clean_name(n: &str) -> Option<String> {
    let n = decode_words(&unquote(n.trim()));
    // Outlook quotes a display name in single quotes: 'Ada Park'.
    let n = n
        .strip_prefix('\'')
        .and_then(|r| r.strip_suffix('\''))
        .unwrap_or(&n);
    let n = n.split_whitespace().collect::<Vec<_>>().join(" ");
    (!n.is_empty()).then_some(n)
}

/// Split at any of `seps` outside quotes, comments and angle brackets.
fn split_top(s: &str, seps: &[char]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut angle, mut paren, mut escaped) = (false, false, 0usize, false);
    for c in s.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => escaped = true,
            '"' if paren == 0 => quoted = !quoted,
            '<' if !quoted && paren == 0 => angle = true,
            '>' if !quoted && paren == 0 => angle = false,
            '(' if !quoted && !angle => paren += 1,
            ')' if !quoted && !angle => paren = paren.saturating_sub(1),
            c if seps.contains(&c) && !quoted && !angle && paren == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter().filter(|s| !s.trim().is_empty()).collect()
}

/// The position of a group label's colon, outside quotes and brackets.
fn top_level_colon(s: &str) -> Option<usize> {
    let (mut quoted, mut angle) = (false, false);
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ':' if !quoted && !angle => return Some(i),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mb(name: Option<&str>, address: Option<&str>) -> Mailbox {
        Mailbox {
            name: name.map(Into::into),
            address: address.map(Into::into),
        }
    }

    #[test]
    fn a_header_list_splits_only_between_mailboxes() {
        assert_eq!(
            list(
                r#"Ada Park <ada@example.com>, "Ortiz, Ben" <ben@example.com>, kim@example.com (Kim Lee)"#
            ),
            vec![
                mb(Some("Ada Park"), Some("ada@example.com")),
                mb(Some("Ortiz, Ben"), Some("ben@example.com")),
                mb(Some("Kim Lee"), Some("kim@example.com")),
            ]
        );
        assert_eq!(
            list("Team: a@example.com, b@example.com;"),
            vec![
                mb(None, Some("a@example.com")),
                mb(None, Some("b@example.com"))
            ]
        );
        assert_eq!(
            list("=?utf-8?Q?Ren=C3=A9e?= <renee@example.com>"),
            vec![mb(Some("Renée"), Some("renee@example.com"))]
        );
        assert!(list("undisclosed-recipients:;").is_empty());
    }

    #[test]
    fn a_quoted_outlook_list_splits_at_semicolons() {
        assert_eq!(
            loose("Ortiz, Ben <ben@example.com<mailto:ben@example.com>>; 'Ada Park'"),
            vec![
                mb(Some("Ortiz, Ben"), Some("ben@example.com")),
                mb(Some("Ada Park"), None),
            ]
        );
    }
}
