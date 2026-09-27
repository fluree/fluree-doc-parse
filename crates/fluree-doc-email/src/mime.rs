//! RFC 5322 headers and MIME bodies: the machinery between an `.eml` file
//! and the text in it.

use encoding_rs::Encoding;

/// A message's or a part's header fields, in order, values unfolded but
/// otherwise as written.
#[derive(Debug, Default)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    pub fn parse(text: &str) -> Headers {
        let mut out: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            // A line that starts with whitespace continues the field above:
            // unfolding removes the line break and keeps the whitespace.
            if line.starts_with([' ', '\t']) {
                if let Some(last) = out.last_mut() {
                    last.1.push_str(line);
                }
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            // An mbox `From ada@example.com Fri Jul 17 …` separator has a
            // space in what would be the name.
            let name = name.trim_end();
            if name.is_empty() || name.contains(char::is_whitespace) {
                continue;
            }
            out.push((name.to_string(), value.to_string()));
        }
        Headers(out)
    }

    /// The first field of this name, trimmed. Names are case-insensitive.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// Split a message or a part at the end of its header: the blank line that
/// should end it, or the first line that is not a field, which is where
/// software that forgets the blank line starts the body.
pub fn split(bytes: &[u8]) -> (&[u8], &[u8]) {
    let mut at = 0;
    while at < bytes.len() {
        let end = bytes[at..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |i| at + i);
        let line = &bytes[at..end];
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return (&bytes[..at], bytes.get(end + 1..).unwrap_or(&[]));
        }
        let continues = line.starts_with(b" ") || line.starts_with(b"\t");
        let mbox = at == 0 && line.starts_with(b"From ");
        if !(continues || mbox || is_field(line)) {
            return (&bytes[..at], &bytes[at..]);
        }
        at = end + 1;
    }
    (bytes, &[])
}

/// `Name: value`, the name printable ASCII without a colon or a space.
fn is_field(line: &[u8]) -> bool {
    match line.iter().position(|&b| b == b':') {
        Some(0) | None => false,
        Some(i) => line[..i].iter().all(|&b| b.is_ascii_graphic()),
    }
}

/// Header bytes as text. Headers are ASCII by the original rules, UTF-8 by
/// the internationalised ones, and Windows-1252 by the habits of old mail
/// software, in that order of likelihood.
pub fn header_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

/// Decode RFC 2047 encoded words: `=?utf-8?Q?Caf=C3=A9?=` is `Café`.
///
/// Whitespace between two encoded words is dropped, since a long value is
/// split into several words and the split is not a space.
pub fn decode_words(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    let mut after_word = false;
    while let Some((start, end, decoded)) = next_word(rest) {
        let between = &rest[..start];
        if !(after_word && between.trim().is_empty()) {
            out.push_str(between);
        }
        out.push_str(&decoded);
        rest = &rest[end..];
        after_word = true;
    }
    out.push_str(rest);
    out
}

/// The next well-formed encoded word in `s`: its byte range and its text.
fn next_word(s: &str) -> Option<(usize, usize, String)> {
    let mut from = 0;
    while let Some(i) = s[from..].find("=?").map(|i| from + i) {
        if let Some((len, text)) = encoded_word(&s[i..]) {
            return Some((i, i + len, text));
        }
        from = i + 2;
    }
    None
}

fn encoded_word(s: &str) -> Option<(usize, String)> {
    let body = s.strip_prefix("=?")?;
    let (charset, rest) = body.split_once('?')?;
    let (enc, rest) = rest.split_once('?')?;
    let end = rest.find("?=")?;
    let text = &rest[..end];
    if text.contains(char::is_whitespace) || charset.is_empty() {
        return None;
    }
    // `utf-8*en`: a language tag may follow the charset.
    let charset = charset.split('*').next().unwrap_or(charset);
    let bytes = match enc {
        "B" | "b" => base64(text.as_bytes()),
        "Q" | "q" => q_decode(text.as_bytes()),
        _ => return None,
    };
    let len = 2 + body.len() - rest.len() + end + 2;
    Some((len, decode_charset(&bytes, Some(charset))))
}

/// The `Q` encoding: quoted-printable, with `_` for a space.
fn q_decode(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        match text[i] {
            b'_' => out.push(b' '),
            b'=' => {
                if let (Some(h), Some(l)) = (hex(text.get(i + 1)), hex(text.get(i + 2))) {
                    out.push(h << 4 | l);
                    i += 3;
                    continue;
                }
                out.push(b'=');
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

/// A structured header value: `text/plain; charset="utf-8"; format=flowed`.
#[derive(Debug, Default)]
pub struct Params {
    /// The value before the parameters, lower-cased: `text/plain`.
    pub value: String,
    list: Vec<(String, String)>,
}

impl Params {
    pub fn parse(field: &str) -> Params {
        let mut pieces = split_unquoted(field, ';').into_iter();
        let value = pieces.next().unwrap_or_default().trim().to_lowercase();
        // (name, section, extended, value) before RFC 2231 reassembly.
        let mut raw: Vec<(String, Option<u32>, bool, String)> = Vec::new();
        for piece in pieces {
            let Some((name, v)) = piece.split_once('=') else {
                continue;
            };
            let mut name = name.trim().to_lowercase();
            let extended = name.ends_with('*');
            if extended {
                name.pop();
            }
            // `filename*0*=`, `filename*1=`: a long value in sections.
            let section = match name.rsplit_once('*') {
                Some((base, n)) if n.bytes().all(|b| b.is_ascii_digit()) && !n.is_empty() => {
                    let n = n.parse().ok();
                    name = base.to_string();
                    n
                }
                _ => None,
            };
            raw.push((name, section, extended, unquote(v.trim())));
        }
        let mut list: Vec<(String, String)> = Vec::new();
        let mut names: Vec<&str> = raw.iter().map(|r| r.0.as_str()).collect();
        names.dedup();
        for name in names {
            let mut parts: Vec<&(String, Option<u32>, bool, String)> =
                raw.iter().filter(|r| r.0 == name).collect();
            let assembled = if parts.iter().any(|p| p.1.is_some() || p.2) {
                parts.retain(|p| p.1.is_some() || p.2);
                parts.sort_by_key(|p| p.1.unwrap_or(0));
                rfc2231(&parts)
            } else {
                decode_words(&parts[0].3)
            };
            if !list.iter().any(|(n, _)| n == name) {
                list.push((name.to_string(), assembled));
            }
        }
        Params { value, list }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.list
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Reassemble an RFC 2231 parameter: `utf-8''na%C3%AFve.pdf`, possibly in
/// sections, the extended ones percent-encoded in the charset the first
/// names.
fn rfc2231(parts: &[&(String, Option<u32>, bool, String)]) -> String {
    let mut charset: Option<String> = None;
    let mut bytes: Vec<u8> = Vec::new();
    for (i, (_, _, extended, v)) in parts.iter().enumerate() {
        if !extended {
            bytes.extend_from_slice(v.as_bytes());
            continue;
        }
        let mut v = v.as_str();
        if i == 0 {
            if let Some((cs, rest)) = v.split_once('\'') {
                if let Some((_lang, rest)) = rest.split_once('\'') {
                    charset = Some(cs.to_string());
                    v = rest;
                }
            }
        }
        bytes.extend(percent_decode(v.as_bytes()));
    }
    decode_charset(&bytes, charset.as_deref())
}

fn percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            if let (Some(h), Some(l)) = (hex(s.get(i + 1)), hex(s.get(i + 2))) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// Split at `sep` outside double quotes.
fn split_unquoted(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut escaped) = (false, false);
    for c in s.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => {
                cur.push(c);
                escaped = true;
            }
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            c if c == sep && !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// A quoted string's content, escapes resolved; anything else as is.
pub fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else {
        return s.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.extend(chars.next());
        } else {
            out.push(c);
        }
    }
    out
}

/// Undo a part's `Content-Transfer-Encoding`.
pub fn transfer_decode(body: &[u8], encoding: Option<&str>) -> Vec<u8> {
    match encoding.map(|e| e.trim().to_ascii_lowercase()).as_deref() {
        Some("base64") => base64(body),
        Some("quoted-printable") => quoted_printable(body),
        _ => body.to_vec(),
    }
}

/// Base64, read leniently: line breaks and stray characters skipped, padding
/// optional, as mail software writes it.
pub fn base64(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in input {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => continue,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

/// Quoted-printable: `=XX` is a byte, `=` at the end of a line joins it to
/// the next.
pub fn quoted_printable(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] != b'=' {
            out.push(input[i]);
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while matches!(input.get(j), Some(b' ' | b'\t')) {
            j += 1;
        }
        if input.get(j) == Some(&b'\r') {
            j += 1;
        }
        if input.get(j) == Some(&b'\n') || j >= input.len() {
            i = j + 1;
            continue;
        }
        if let (Some(h), Some(l)) = (hex(input.get(i + 1)), hex(input.get(i + 2))) {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(b'=');
            i += 1;
        }
    }
    out
}

fn hex(b: Option<&u8>) -> Option<u8> {
    let b = *b?;
    (b as char).to_digit(16).map(|d| d as u8)
}

/// Bytes in a declared charset as text.
///
/// Mail lies about its charset in one direction far more than the other:
/// UTF-8 text labelled `us-ascii` or `iso-8859-1`. So bytes that are valid
/// UTF-8 are read as UTF-8 when the label is one of those, or missing, or
/// unknown; a label that names anything else is believed.
pub fn decode_charset(bytes: &[u8], charset: Option<&str>) -> String {
    let label = charset.map(str::trim).filter(|c| !c.is_empty());
    let named = label.and_then(|l| Encoding::for_label(l.as_bytes()));
    let weak = named.is_none_or(|e| e == encoding_rs::WINDOWS_1252);
    if weak {
        if let Ok(s) = std::str::from_utf8(bytes) {
            return s.to_string();
        }
    }
    named
        .unwrap_or(encoding_rs::WINDOWS_1252)
        .decode(bytes)
        .0
        .into_owned()
}

/// The bodies of a multipart body's parts, preamble and epilogue dropped.
pub fn multipart<'a>(body: &'a [u8], boundary: &str) -> Vec<&'a [u8]> {
    let delim = format!("--{boundary}");
    let mut parts = Vec::new();
    let mut start: Option<usize> = None;
    let mut at = 0;
    while at < body.len() {
        let end = body[at..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(body.len(), |i| at + i);
        let line = body[at..end].trim_ascii_end();
        if let Some(tail) = line.strip_prefix(delim.as_bytes()) {
            let closing = tail.starts_with(b"--");
            if tail.is_empty() || closing || tail.trim_ascii().is_empty() {
                if let Some(s) = start {
                    // The line break before a delimiter belongs to it.
                    let part = &body[s..at];
                    let part = part.strip_suffix(b"\n").unwrap_or(part);
                    parts.push(part.strip_suffix(b"\r").unwrap_or(part));
                }
                if closing {
                    return parts;
                }
                start = Some(end + 1);
            }
        }
        at = end + 1;
    }
    // No closing delimiter: the last part runs to the end.
    if let Some(s) = start.filter(|&s| s < body.len()) {
        parts.push(&body[s..]);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_words_decode_and_the_space_between_two_goes() {
        assert_eq!(
            decode_words("=?utf-8?Q?Caf=C3=A9_au_lait?="),
            "Café au lait"
        );
        assert_eq!(
            decode_words("=?utf-8?B?w5xiZXI=?= =?utf-8?Q?_alles?= today"),
            "Über alles today"
        );
        assert_eq!(decode_words("Re: =?iso-8859-1?Q?na=EFve?="), "Re: naïve");
        assert_eq!(decode_words("plain =?bogus"), "plain =?bogus");
    }

    #[test]
    fn parameters_unquote_and_reassemble() {
        let p = Params::parse(r#"text/plain; charset="utf-8"; format=flowed; name="a \"b\".txt""#);
        assert_eq!(p.value, "text/plain");
        assert_eq!(p.get("charset"), Some("utf-8"));
        assert_eq!(p.get("format"), Some("flowed"));
        assert_eq!(p.get("name"), Some(r#"a "b".txt"#));

        let p = Params::parse("attachment; filename*=utf-8''na%C3%AFve%20plan.pdf");
        assert_eq!(p.value, "attachment");
        assert_eq!(p.get("filename"), Some("naïve plan.pdf"));

        let p =
            Params::parse("attachment; filename*0*=utf-8''Quarterly%20; filename*1=\"review.pdf\"");
        assert_eq!(p.get("filename"), Some("Quarterly review.pdf"));
    }

    #[test]
    fn transfer_encodings_decode() {
        assert_eq!(base64(b"SGVs\r\nbG8="), b"Hello");
        assert_eq!(base64(b"SGVsbG8"), b"Hello");
        assert_eq!(
            quoted_printable(b"caf=C3=A9 is=\r\n open = fine=\n"),
            "café is open = fine".as_bytes()
        );
    }

    #[test]
    fn a_mislabelled_utf8_body_reads_as_utf8() {
        assert_eq!(decode_charset("Café".as_bytes(), Some("us-ascii")), "Café");
        assert_eq!(decode_charset(b"Caf\xe9", Some("iso-8859-1")), "Café");
        assert_eq!(decode_charset(b"Caf\xe9", None), "Café");
        assert_eq!(
            decode_charset(&[0x93, 0xfa, 0x96, 0x7b], Some("shift_jis")),
            "日本"
        );
    }

    #[test]
    fn multipart_bodies_split_at_their_boundary() {
        let body = b"preamble\r\n--XY\r\nContent-Type: text/plain\r\n\r\none\r\n--XY\r\n\r\ntwo\r\n--XY--\r\nepilogue";
        let parts = multipart(body, "XY");
        assert_eq!(parts.len(), 2);
        assert_eq!(split(parts[0]).1, b"one");
        assert_eq!(split(parts[1]).1, b"two");
        // A boundary that another merely starts with is not a delimiter.
        assert_eq!(
            multipart(b"--XYZ\n\nnot\n--XY\n\nyes\n--XY--", "XY").len(),
            1
        );
    }

    #[test]
    fn a_body_with_no_blank_line_before_it_is_still_the_body() {
        let (head, body) = split(b"From: a@example.com\nSubject: s\nno blank line here\nmore");
        assert_eq!(head, b"From: a@example.com\nSubject: s\n");
        assert_eq!(body, b"no blank line here\nmore");
        let (head, body) = split(b"From ada Fri Jul 17\nTo: b@example.com\n\nbody");
        assert!(head.ends_with(b"b@example.com\n"));
        assert_eq!(body, b"body");
    }

    #[test]
    fn folded_headers_unfold() {
        let h = Headers::parse(
            "From Ada Fri Jul 17 13:48:00 2026\nTo: a@example.com,\n b@example.com\nSubject: Hi\n",
        );
        assert_eq!(h.get("to"), Some("a@example.com, b@example.com"));
        assert_eq!(h.get("SUBJECT"), Some("Hi"));
        assert_eq!(h.len(), 2);
    }
}
