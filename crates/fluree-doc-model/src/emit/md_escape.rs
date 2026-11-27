//! Text written into Markdown so that it reads back as the same text.
//!
//! Markdown reads its syntax out of ordinary characters, and a document's
//! text is full of them: a paragraph that opens `1. ` is a list, a row of
//! `___` a rule, `~~~` a fence that swallows everything after it, `<name>`
//! an HTML tag, two `*` a span of emphasis whose stars are gone. Each
//! character is escaped only where Markdown would read it as markup, as
//! CommonMark and GFM define where it does: a backslash is a character more
//! in the text every reader sees.

/// Where a text is set, which decides what Markdown reads in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Context {
    /// A block of its own, a paragraph or a list item's text: its lines can
    /// open blocks, and its text can hold spans.
    Block,
    /// After other text on its line: spans only.
    Inline,
    /// An ATX heading's text: spans, and a closing run of `#`.
    Heading,
    /// A pipe table's cell, whose backslashes and pipes the cell writer
    /// escapes itself: spans only.
    Cell,
}

/// What becomes of each character of a text written into Markdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    Keep,
    /// A backslash before it.
    Escape,
    /// Left out: a line's leading whitespace, which Markdown would read as
    /// indentation, and a blank line, which would end the block.
    Drop,
}

/// The mark of each char of `chars`, set in `ctx`. In a `Block`, the text
/// from `start` is the block's first line (the chars before it, a list
/// item's own number, are its marker and kept as they are).
pub(crate) fn marks(chars: &[char], ctx: Context, start: usize) -> Vec<Mark> {
    let mut m = vec![Mark::Keep; chars.len()];
    if ctx == Context::Block {
        lines(chars, start, &mut m);
    }
    spans(chars, ctx, &mut m);
    if ctx == Context::Heading {
        closing_hashes(chars, &mut m);
    }
    m
}

/// The text with its marks applied.
pub(crate) fn apply(chars: &[char], m: &[Mark], range: std::ops::Range<usize>) -> String {
    let mut out = String::with_capacity(range.len());
    for i in range {
        match m[i] {
            Mark::Keep => out.push(chars[i]),
            Mark::Escape => {
                out.push('\\');
                out.push(chars[i]);
            }
            Mark::Drop => {}
        }
    }
    out
}

fn escape(m: &mut [Mark], i: usize) {
    if m[i] == Mark::Keep {
        m[i] = Mark::Escape;
    }
}

/// Punctuation as CommonMark's flanking rules read it: ASCII punctuation, or
/// any character that is neither a letter, a digit nor whitespace.
fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation() || (!c.is_alphanumeric() && !c.is_whitespace())
}

/// Characters Markdown reads as spans anywhere in a block's text.
fn spans(chars: &[char], ctx: Context, m: &mut [Mark]) {
    let n = chars.len();
    let at = |i: usize| chars.get(i).copied();
    for i in 0..n {
        match chars[i] {
            // A backslash before ASCII punctuation escapes it, and is gone.
            // A cell's are escaped by the cell writer, every one.
            '\\' if ctx != Context::Cell && at(i + 1).is_some_and(|c| c.is_ascii_punctuation()) => {
                escape(m, i)
            }
            // `<` where it opens an autolink or a piece of inline HTML, as
            // CommonMark defines them: `<a@b.org>`, `<https://…>`,
            // `<name>`, `</b>`, `<!-- … -->`. Anything else, `Re<2000` or
            // `<Model_rawdata>`, Markdown shows as it is.
            '<' if markup_at(chars, i) => escape(m, i),
            // An entity or numeric reference is read as the character it
            // names: `&amp;` as `&`.
            '&' if entity_at(chars, i + 1) => escape(m, i),
            // `]` straight before `(` closes a link's text, or an image's.
            ']' if at(i + 1) == Some('(') => escape(m, i),
            _ => {}
        }
    }
    for c in ['*', '_', '~'] {
        emphasis(chars, c, m);
    }
    code_spans(chars, m);
}

/// Whether `chars[i]`, a `<`, opens an autolink or inline HTML.
fn markup_at(chars: &[char], i: usize) -> bool {
    let rest = &chars[i + 1..];
    autolink(rest) || tag(rest).is_some() || other_html(rest)
}

/// `scheme:…>` or `name@domain>`, after the `<`.
fn autolink(rest: &[char]) -> bool {
    let Some(end) = rest.iter().position(|c| *c == '>') else {
        return false;
    };
    let body = &rest[..end];
    if body
        .iter()
        .any(|c| c.is_whitespace() || c.is_control() || *c == '<')
    {
        return false;
    }
    // A URI: a scheme of 2 to 32 characters, a letter first, then a colon.
    if let Some(colon) = body.iter().position(|c| *c == ':') {
        let scheme = &body[..colon];
        if (2..=32).contains(&scheme.len())
            && scheme[0].is_ascii_alphabetic()
            && scheme
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(*c))
        {
            return true;
        }
    }
    // An address.
    let Some(at) = body.iter().position(|c| *c == '@') else {
        return false;
    };
    let (local, domain) = (&body[..at], &body[at + 1..]);
    !local.is_empty()
        && local
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || ".!#$%&'*+/=?^_`{|}~-".contains(*c))
        && !domain.is_empty()
        && domain.split(|c| *c == '.').all(|label| {
            (1..=63).contains(&label.len())
                && label.iter().all(|c| c.is_ascii_alphanumeric() || *c == '-')
                && label[0] != '-'
                && label[label.len() - 1] != '-'
        })
}

/// An open or closing tag after the `<`, as CommonMark defines one: its
/// length up to the `>` and whether it is a closing tag.
fn tag(rest: &[char]) -> Option<(usize, bool)> {
    let mut k = 0;
    let closing = rest.first() == Some(&'/');
    if closing {
        k = 1;
    }
    let name = |c: char| c.is_ascii_alphanumeric() || c == '-';
    if !rest.get(k).is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    while rest.get(k).copied().is_some_and(name) {
        k += 1;
    }
    let ws = |k: &mut usize| {
        let s = *k;
        while rest.get(*k).is_some_and(|c| c.is_whitespace()) {
            *k += 1;
        }
        *k > s
    };
    if closing {
        ws(&mut k);
        return (rest.get(k) == Some(&'>')).then_some((k + 1, true));
    }
    loop {
        let spaced = ws(&mut k);
        match rest.get(k) {
            Some('>') => return Some((k + 1, false)),
            Some('/') if rest.get(k + 1) == Some(&'>') => return Some((k + 2, false)),
            Some(c) if spaced && (c.is_ascii_alphabetic() || "_:".contains(*c)) => {
                k += 1;
                while rest
                    .get(k)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || "_.:-".contains(*c))
                {
                    k += 1;
                }
                // An attribute's value, if it has one.
                let before = k;
                ws(&mut k);
                if rest.get(k) == Some(&'=') {
                    k += 1;
                    ws(&mut k);
                    match rest.get(k) {
                        Some(q @ ('"' | '\'')) => {
                            let q = *q;
                            let close = rest[k + 1..].iter().position(|c| *c == q)?;
                            k += close + 2;
                        }
                        Some(_) => {
                            let s = k;
                            while rest
                                .get(k)
                                .is_some_and(|c| !c.is_whitespace() && !"\"'=<>`".contains(*c))
                            {
                                k += 1;
                            }
                            if k == s {
                                return None;
                            }
                        }
                        None => return None,
                    }
                } else {
                    k = before;
                }
            }
            _ => return None,
        }
    }
}

/// A comment, a processing instruction, a declaration or a CDATA section
/// after the `<`, closed.
fn other_html(rest: &[char]) -> bool {
    let s: String = rest.iter().take(4096).collect();
    if let Some(body) = s.strip_prefix("!--") {
        return body.starts_with('>') || body.starts_with("->") || body.contains("-->");
    }
    if let Some(body) = s.strip_prefix('?') {
        return body.contains("?>");
    }
    if let Some(body) = s.strip_prefix("![CDATA[") {
        return body.contains("]]>");
    }
    if let Some(body) = s.strip_prefix('!') {
        return body.starts_with(|c: char| c.is_ascii_alphabetic()) && body.contains('>');
    }
    false
}

/// Whether a line, `<` first, opens an HTML block: a raw-text tag
/// (`script`, `pre`, `style`, `textarea`), a comment, an instruction, a
/// declaration, CDATA or a block-level tag, any of which interrupts a
/// paragraph; or, on a block's first line, any complete tag alone on it.
fn html_block_at(line: &[char], first: bool) -> bool {
    let rest = &line[1..];
    let s: String = rest
        .iter()
        .take(16)
        .collect::<String>()
        .to_ascii_lowercase();
    let after = |n: usize| {
        rest.get(n).is_none_or(|c| {
            c.is_whitespace() || *c == '>' || (*c == '/' && rest.get(n + 1) == Some(&'>'))
        })
    };
    for t in ["script", "pre", "style", "textarea"] {
        if s.starts_with(t) && after(t.len()) {
            return true;
        }
    }
    if s.starts_with("!--") || s.starts_with('?') || s.starts_with("![cdata[") {
        return true;
    }
    if s.starts_with('!') && s[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
        return true;
    }
    let name_at = usize::from(s.starts_with('/'));
    let name: String = s[name_at..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    if BLOCK_TAGS.contains(&name.as_str()) && after(name_at + name.len()) {
        return true;
    }
    first && tag(rest).is_some_and(|(n, _)| rest[n..].iter().all(|c| c.is_whitespace()))
}

/// The tags that open an HTML block of their own (CommonMark's sixth kind).
const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "base",
    "basefont",
    "blockquote",
    "body",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hr",
    "html",
    "iframe",
    "legend",
    "li",
    "link",
    "main",
    "menu",
    "menuitem",
    "nav",
    "noframes",
    "ol",
    "optgroup",
    "option",
    "p",
    "param",
    "search",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
];

/// `&name;`, `&#123;` or `&#x1F;` from `i`.
fn entity_at(chars: &[char], i: usize) -> bool {
    let rest = &chars[i.min(chars.len())..];
    let Some(end) = rest.iter().take(34).position(|c| *c == ';') else {
        return false;
    };
    let body = &rest[..end];
    match body {
        ['#', 'x' | 'X', hex @ ..] => {
            (1..=6).contains(&hex.len()) && hex.iter().all(char::is_ascii_hexdigit)
        }
        ['#', dec @ ..] => (1..=7).contains(&dec.len()) && dec.iter().all(char::is_ascii_digit),
        [first, ..] => first.is_ascii_alphabetic() && body.iter().all(char::is_ascii_alphanumeric),
        [] => false,
    }
}

/// Runs of `c` that would pair as emphasis (`*`, `_`) or strikethrough
/// (`~`): a run that can open before one that can close. Then every run
/// that can do either is escaped, and none of them pairs. A lone star after
/// a value (`p < 0.05*`) or a word joined by underscores opens nothing and
/// is left alone.
fn emphasis(chars: &[char], c: char, m: &mut [Mark]) {
    let n = chars.len();
    let mut runs: Vec<(usize, usize, bool, bool)> = Vec::new();
    let mut i = 0;
    while i < n {
        if chars[i] != c {
            i += 1;
            continue;
        }
        let s = i;
        while i < n && chars[i] == c {
            i += 1;
        }
        let prev = s.checked_sub(1).map_or(' ', |p| chars[p]);
        let next = chars.get(i).copied().unwrap_or(' ');
        let left =
            !next.is_whitespace() && (!is_punct(next) || prev.is_whitespace() || is_punct(prev));
        let right =
            !prev.is_whitespace() && (!is_punct(prev) || next.is_whitespace() || is_punct(next));
        let (open, close) = if c == '_' {
            (
                left && (!right || is_punct(prev)),
                right && (!left || is_punct(next)),
            )
        } else {
            (left, right)
        };
        runs.push((s, i, open, close));
    }
    let pairs = runs
        .iter()
        .position(|r| r.2)
        .is_some_and(|f| runs[f + 1..].iter().any(|r| r.3));
    if !pairs {
        return;
    }
    for &(s, e, open, close) in &runs {
        if open || close {
            for k in s..e {
                escape(m, k);
            }
        }
    }
}

/// Backticks that would make a code span, whose own escapes do not work:
/// an unescaped backtick opens one that any later backtick run of its length
/// closes, an escaped one included. So where a text holds two or more, every
/// one is escaped; a lone one opens nothing.
fn code_spans(chars: &[char], m: &mut [Mark]) {
    if chars.iter().filter(|c| **c == '`').count() < 2 {
        return;
    }
    for (i, c) in chars.iter().enumerate() {
        if *c == '`' {
            escape(m, i);
        }
    }
}

/// A closing run of `#` ends an ATX heading's text and is not shown:
/// `Section #` would read `Section`.
fn closing_hashes(chars: &[char], m: &mut [Mark]) {
    let end = chars
        .iter()
        .rposition(|c| !c.is_whitespace())
        .map_or(0, |p| p + 1);
    let run = chars[..end].iter().rev().take_while(|c| **c == '#').count();
    if run == 0 {
        return;
    }
    let s = end - run;
    if s == 0 || chars[s - 1].is_whitespace() {
        escape(m, s);
    }
}

/// Block syntax at the start of each line of a block. The first line opens
/// the block; a later one is a continuation, which only what can interrupt
/// a paragraph breaks, and an underline turns into a heading.
fn lines(chars: &[char], start: usize, m: &mut [Mark]) {
    let n = chars.len();
    let mut ls = start;
    let mut first = true;
    let mut prev_line: Option<(usize, usize)> = None;
    while ls <= n {
        let le = chars[ls..]
            .iter()
            .position(|c| *c == '\n')
            .map_or(n, |p| ls + p);
        // Leading whitespace is indentation to Markdown: four spaces open a
        // code block, where every escape would show. It is left out.
        let mut s = ls;
        while s < le && chars[s].is_whitespace() {
            m[s] = Mark::Drop;
            s += 1;
        }
        if s == le {
            // A blank line ends the block; it is left out, its line break
            // with it. One at the start leaves the block's first line to
            // come.
            if le < n {
                m[le] = Mark::Drop;
            }
            ls = le + 1;
            continue;
        }
        line_start(chars, s, le, first, prev_line, m);
        prev_line = Some((s, le));
        first = false;
        ls = le + 1;
    }
    // A break after the last line would leave a line break at the end.
    if let Some(last) = chars.iter().rposition(|c| !c.is_whitespace()) {
        for k in last + 1..n {
            m[k] = Mark::Drop;
        }
    }
}

fn line_start(
    chars: &[char],
    s: usize,
    e: usize,
    first: bool,
    prev_line: Option<(usize, usize)>,
    m: &mut [Mark],
) {
    let line = &chars[s..e];
    let at = |i: usize| line.get(i).copied();
    let blank_after = |i: usize| at(i).is_none_or(|c| c == ' ' || c == '\t');
    let content_after = |i: usize| line[i.min(line.len())..].iter().any(|c| !c.is_whitespace());
    let c0 = line[0];
    // A row of one of `-*_` (spaces between allowed), three or more: a rule.
    let marks: Vec<char> = line
        .iter()
        .copied()
        .filter(|c| !c.is_whitespace())
        .collect();
    let rule = marks.len() >= 3 && "-*_".contains(marks[0]) && marks.iter().all(|c| *c == marks[0]);
    // A row of `=` or of `-` under a line makes that line a heading.
    let underline = !first
        && (c0 == '=' || c0 == '-')
        && line
            .iter()
            .skip_while(|c| **c == c0)
            .all(|c| c.is_whitespace());
    // A table's delimiter row under a row with a pipe in it.
    let delimiter = !first
        && marks.contains(&'-')
        && marks.iter().all(|c| "-:|".contains(*c))
        && (marks.contains(&'|') || prev_line.is_some_and(|(ps, pe)| chars[ps..pe].contains(&'|')));
    let hashes = line.iter().take_while(|c| **c == '#').count();
    let digits = line.iter().take_while(|c| c.is_ascii_digit()).count();
    let escape_first = if (1..=6).contains(&hashes) && blank_after(hashes) {
        true
    } else if rule || underline || delimiter {
        true
    } else {
        match c0 {
            '>' => true,
            // A bullet; one can interrupt a paragraph only with text after it.
            '-' | '+' | '*' => blank_after(1) && (first || content_after(1)),
            // A fence.
            '`' | '~' => line.iter().take_while(|c| **c == c0).count() >= 3,
            // An HTML block.
            '<' => html_block_at(line, first),
            // A link reference or footnote definition opens a block only.
            '[' => first && defines(line),
            _ => false,
        }
    };
    if escape_first {
        escape(m, s);
        // The rest of a rule or a fence of `*`, `_` or `~` would be a run
        // of its own after the escaped first, one that can close emphasis:
        // every one is escaped.
        if "*_~".contains(c0) {
            for (k, c) in line.iter().enumerate() {
                if *c == c0 {
                    escape(m, s + k);
                }
            }
        }
    }
    // An ordered list's number: any opens a block, only `1` interrupts one.
    if (1..=9).contains(&digits)
        && matches!(at(digits), Some('.' | ')'))
        && blank_after(digits + 1)
        && (first || (line[..digits] == ['1'] && content_after(digits + 1)))
    {
        escape(m, s + digits);
    }
}

/// `[label]:`, a link reference definition, or `[^label]:`, a footnote's:
/// a label holding no unescaped bracket, then `]:`.
fn defines(line: &[char]) -> bool {
    let Some(n) = line[1..].iter().position(|c| matches!(c, '[' | ']')) else {
        return false;
    };
    line[1 + n] == ']' && line.get(2 + n) == Some(&':')
}
