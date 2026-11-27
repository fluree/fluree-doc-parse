//! Markdown and XHTML emission from the element model.

use crate::element::{Element, Link, Notes, Turn};
use crate::message::Message;

/// Split an element's text at its located link anchors.
///
/// Yields `(text, link)` pairs in order, covering the string exactly once —
/// `link` is `None` for the stretches between anchors. Both markup emitters
/// walk this rather than each slicing the string themselves, so they cannot
/// disagree about where an anchor starts.
fn link_segments(e: &Element) -> Vec<(String, Option<&Link>)> {
    let Some(links) = &e.links else {
        return vec![(e.text.clone(), None)];
    };
    let chars: Vec<char> = e.text.chars().collect();
    let mut out: Vec<(String, Option<&Link>)> = Vec::new();
    let mut at = 0usize;
    for l in links {
        // `link::attach` sorts and de-overlaps, so a span failing these is one
        // built by hand; skipping it beats panicking on a slice.
        let Some((b, end)) = l.span().filter(|(b, e)| *b >= at && *e <= chars.len()) else {
            continue;
        };
        if b > at {
            out.push((chars[at..b].iter().collect(), None));
        }
        out.push((chars[b..end].iter().collect(), Some(l)));
        at = end;
    }
    if at < chars.len() {
        out.push((chars[at..].iter().collect(), None));
    }
    out
}

/// How Markdown writes a link.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MdLinks {
    /// `[the filing](https://…)`, or the autolink `<https://…>` where the
    /// words are the address itself.
    #[default]
    Inline,
    /// The anchor's words alone, as plain text. The addresses are still in
    /// `json`, `doco` and `xhtml`; this is for a reader that wants the prose
    /// without them, such as an embedding model.
    Text,
}

/// Choices for [`to_markdown_with_options`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarkdownOptions {
    pub links: MdLinks,
}

/// An element's text with its links marked up as Markdown.
fn md_linked(e: &Element, opts: &MarkdownOptions) -> String {
    if e.links.is_none() || opts.links == MdLinks::Text {
        return md_text(&e.text, true);
    }
    let mut out = String::new();
    for (i, (text, link)) in link_segments(e).into_iter().enumerate() {
        match link {
            None => out.push_str(&md_text(&text, i == 0)),
            Some(l) => out.push_str(&md_link(&text, &l.href())),
        }
    }
    out
}

/// One link in Markdown.
///
/// The autolink form is preferred where the anchor already *is* the address:
/// a page that prints `https://example.org/a` and links it there says the
/// same thing twice in `[…](…)`, and `<…>` is the idiom for exactly that.
/// A page's link rectangle often takes in the sentence's full stop as well,
/// so the punctuation after the address is written after the autolink,
/// outside it.
///
/// Only an address with a scheme can be an autolink. `<www.example.org>` is
/// not one: a renderer shows it as literal text, angle brackets included,
/// and links nothing, so a target written without a scheme keeps the
/// `[…](…)` form.
fn md_link(text: &str, href: &str) -> String {
    if is_absolute_uri(href) {
        if let Some(rest) = text.strip_prefix(href) {
            if rest.chars().all(|c| ".,:;!?)".contains(c)) {
                return format!("<{href}>{rest}");
            }
        }
    }
    // `]` would close the label early; a destination with a space or a
    // paren needs the angle-bracket form.
    let label = text.replace('[', "\\[").replace(']', "\\]");
    if href.contains(['(', ')', ' ', '<', '>']) {
        format!("[{label}](<{href}>)")
    } else {
        format!("[{label}]({href})")
    }
}

/// An address Markdown can write as an autolink, as CommonMark defines an
/// absolute URI: a scheme of 2 to 32 characters (a letter, then letters,
/// digits, `+`, `.` or `-`), a colon, and no space, `<`, `>` or control
/// character after it.
fn is_absolute_uri(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once(':') else {
        return false;
    };
    let mut cs = scheme.chars();
    (2..=32).contains(&scheme.len())
        && cs.next().is_some_and(|c| c.is_ascii_alphabetic())
        && cs.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        && !rest
            .chars()
            .any(|c| c.is_ascii_control() || matches!(c, ' ' | '<' | '>'))
}

/// Text that is not a link, kept from reading as one.
///
/// `]` directly before `(` makes an inline link of whatever bracket opened
/// it: mathematics such as `Γ[w](t)` renders as a link to `t` whose text
/// is `w`, and the brackets and the parenthesis are gone. An image,
/// `![…](…)`, is the same construct. A backslash before that `]` is all the
/// link needs to fail, and it renders as nothing.
///
/// A block that opens with `[label]:` is worse: it is a link reference
/// definition, which a renderer does not show at all. Where `opens_block`,
/// such a bracket is escaped as well. Nothing else is: every other bracket
/// pairing needs a definition to become a link, and this writer emits none.
fn md_text(text: &str, opens_block: bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    // A definition may be indented up to three spaces, and its label holds no
    // unescaped bracket.
    let lead = chars.iter().take_while(|c| **c == ' ').count();
    let defines = opens_block && lead <= 3 && chars.get(lead) == Some(&'[') && {
        let label = chars[lead + 1..]
            .iter()
            .position(|c| matches!(c, '[' | ']'));
        label.is_some_and(|n| chars[lead + 1 + n] == ']' && chars.get(lead + 2 + n) == Some(&':'))
    };
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        if (c == ']' && chars.get(i + 1) == Some(&'(')) || (defines && i == lead) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Text that opens with an ordered list's marker as Markdown reads one: up
/// to nine digits, then `.` or `)`, then a space.
fn opens_with_ordinal(text: &str) -> bool {
    let digits = text.chars().take_while(|c| c.is_ascii_digit()).count();
    (1..=9).contains(&digits)
        && matches!(text[digits..].chars().next(), Some('.' | ')'))
        && text[digits + 1..].starts_with(' ')
}

/// A recording offset as a clock reading: `04:32`, or `1:04:32` past the
/// hour.
fn clock(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// A transcript turn's speaker label and the words after it, when its text
/// opens with the label the way the transcript reader writes it.
fn speaker_and_words<'a>(e: &'a Element, t: &'a Turn) -> Option<(&'a str, &'a str)> {
    let s = t.speaker.as_deref()?;
    let words = e.text.strip_prefix(s)?.strip_prefix(": ")?;
    Some((s, words))
}

/// A transcript turn for a person to read: the speaker in bold and where in
/// the recording the turn starts.
///
/// The one projection that shows the time. The text and the graph leave it
/// out because an extractor reading `04:32` in the prose takes it for a time
/// the speaker mentioned; Markdown is for people, and a person wants to know
/// where to scrub to.
fn md_turn(e: &Element, t: &Turn) -> String {
    let at = clock(t.start_ms);
    match speaker_and_words(e, t) {
        Some((s, words)) => format!(
            "**{}** ({at}): {}",
            s.replace('*', "\\*").replace('_', "\\_"),
            md_text(words, false)
        ),
        None => format!("({at}) {}", md_text(&e.text, false)),
    }
}

/// A message's header for a person to read: one header line to a line, and
/// a rule above a quoted message so a thread reads as its messages.
fn md_message(e: &Element, m: &Message) -> String {
    let lines = e
        .text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .enumerate()
        .map(|(i, l)| md_text(l, i == 0))
        .collect::<Vec<_>>()
        .join("  \n");
    if m.quoted {
        format!("---\n\n{lines}")
    } else {
        lines
    }
}

/// A message's header as a paragraph whose attributes name the sender and
/// the time, and a rule above one that is quoted.
fn html_message(e: &Element, m: &Message) -> String {
    let attr = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let mut attrs = String::new();
    if let Some(f) = m.from.first() {
        attrs.push_str(&format!(" data-from=\"{}\"", attr(&f.display())));
    }
    if let Some(d) = &m.date {
        attrs.push_str(&format!(" data-sent-at=\"{}\"", attr(d)));
    }
    format!(
        "{}<p{attrs}>{}</p>\n",
        if m.quoted { "<hr/>\n" } else { "" },
        html_linked(e)
    )
}

/// Was this heading's *depth* inferred from typography rather than stated by
/// the document?
///
/// A level from the bookmark outline, from a numbering scheme, or from the
/// document's own title is something the author declared. A level from font
/// size or weight is this library's reading of how the page looks, and on a
/// document that declares nothing it is a guess — an accurate one often
/// enough to be worth making, and never one to present as fact.
///
/// Marking them is what makes a downstream pass affordable: a model
/// adjudicating a bounded set of doubtful headings costs a fraction of one
/// re-reading the document, and on a file that does declare its structure the
/// set is empty.
fn level_is_inferred(e: &Element) -> bool {
    e.kind == "doco:SectionTitle" && matches!(e.evidence, "font-size" | "bold")
}

/// An element's text, escaped for XHTML, with its links as `<a href>`.
///
/// Kept out of [`to_xhtml`] so that escaping happens per segment: escaping the
/// whole string first and inserting tags afterwards would put the anchor
/// boundaries at offsets the escaping has already moved.
fn html_linked(e: &Element) -> String {
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
    if e.links.is_none() {
        return esc(&e.text);
    }
    let mut out = String::new();
    for (text, link) in link_segments(e) {
        match link {
            None => out.push_str(&esc(&text)),
            Some(l) => out.push_str(&format!(
                "<a href=\"{}\">{}</a>",
                esc(&l.href()).replace('"', "&quot;"),
                esc(&text)
            )),
        }
    }
    out
}

/// A transcript turn as a paragraph whose attributes say who spoke and when.
///
/// The text nodes read exactly as the element's text does, speaker label
/// included, so text pulled from the markup matches the text projection; the
/// `<b>` only marks where the label ends. Times ride on attributes and never
/// in the text, for the same reason they stay out of the projection.
fn html_turn(e: &Element, t: &Turn) -> String {
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
    let mut attrs = String::new();
    if let Some(s) = &t.speaker {
        attrs.push_str(&format!(" data-speaker=\"{}\"", esc(s)));
    }
    attrs.push_str(&format!(
        " data-start-ms=\"{}\" data-end-ms=\"{}\"",
        t.start_ms, t.end_ms
    ));
    let body = match speaker_and_words(e, t) {
        Some((s, words)) => format!("<b>{}</b>: {}", esc(s), esc(words)),
        None => esc(&e.text),
    };
    format!("<p{attrs}>{body}</p>\n")
}

/// Render elements to XHTML, matching the contract a downstream extraction
/// worker already consumes (headings, paragraphs, lists, tables; text nodes
/// carry the content). A drop-in for such a worker: downstream text
/// extraction and NER see the same shape they see today.
/// Whether the table at `i` carries on the one just before it, across a page
/// break, so that its rows are rendered as more rows of that table.
/// How many of the leading rows of the table at `i`, carried on from the one
/// before it, repeat that table's header and are left out. None where the
/// earlier part is nothing but header: the break fell inside the header and
/// these rows finish it.
fn repeated_header_rows(elements: &[Element], i: usize) -> usize {
    if elements[i - 1].header_only() {
        0
    } else {
        elements[i].header_rows.unwrap_or(0)
    }
}

fn carries_on_table(elements: &[Element], i: usize) -> bool {
    let (Some(prev), Some(e)) = (i.checked_sub(1).map(|p| &elements[p]), elements.get(i)) else {
        return false;
    };
    let cols = |e: &Element| e.cells.as_ref().and_then(|r| r.first()).map(Vec::len);
    prev.kind == "doco:Table"
        && e.continues.as_deref() == Some(prev.id.as_str())
        && cols(prev).is_some()
        && cols(prev) == cols(e)
}

/// Text escaped for HTML markup.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A table's rows as HTML, `<tr>` by `<tr>`, from row `skip` on. `cell`
/// writes a cell's text into the markup.
///
/// `<th>` only where the header row was *measured* (`header_rows`);
/// undetected tables keep plain `<td>` rather than asserting a header that
/// may not exist. A cell spanning rows or columns is written once, with its
/// `rowspan` and `colspan`.
fn html_table_rows(
    e: &Element,
    rows: &[Vec<String>],
    skip: usize,
    cell: impl Fn(&str) -> String,
    out: &mut String,
) {
    let th_rows = e.header_rows.unwrap_or(0);
    let subs = e.sub_headers.clone().unwrap_or_default();
    let ncols = rows.first().map(Vec::len).unwrap_or(0);
    let band = |ri: usize| {
        subs.contains(&ri) || (ri < th_rows && rows[ri][1..].iter().all(|c| c.is_empty()))
    };
    let (md, ml) = spans_kept(
        rows,
        e.merged_down
            .as_ref()
            .filter(|m| ncols > 0 && m.len() == rows.len() * ncols),
        e.merged_left
            .as_ref()
            .filter(|m| ncols > 0 && m.len() == rows.len() * ncols),
        skip,
        band,
    );
    for (ri, r) in rows.iter().enumerate() {
        if ri < skip {
            continue;
        }
        // A merged full-width band is one spanning header cell, not a row of
        // blanks after the first column. Its text may be split across
        // columns in the raw grid, so join it back.
        if band(ri) {
            let joined = r
                .iter()
                .map(|c| c.trim())
                .filter(|c| !c.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            out.push_str(&format!(
                "<tr><th colspan=\"{ncols}\">{}</th></tr>\n",
                cell(&joined)
            ));
            continue;
        }
        let tag = if ri < th_rows { "th" } else { "td" };
        out.push_str("<tr>");
        for (ci, c) in r.iter().enumerate() {
            // A cell continuing the one above or to its left was already
            // emitted carrying that span.
            if md.as_ref().is_some_and(|m| m[ri * ncols + ci])
                || ml.as_ref().is_some_and(|m| m[ri * ncols + ci])
            {
                continue;
            }
            let span = md
                .as_ref()
                .map(|m| {
                    (ri + 1..rows.len())
                        .take_while(|k| m[k * ncols + ci])
                        .count()
                        + 1
                })
                .unwrap_or(1);
            let colspan = ml
                .as_ref()
                .map(|m| (ci + 1..ncols).take_while(|k| m[ri * ncols + k]).count() + 1)
                .unwrap_or(1);
            // A horizontally merged run's text lives across the cells it
            // spans in the raw grid. So may a vertical one's: the cells it
            // spans are written once, here, and text a reader left in any
            // of them is this cell's, as DoCO reads it, never dropped.
            let text = if colspan > 1 || span > 1 {
                rows[ri..ri + span]
                    .iter()
                    .flat_map(|row| row[ci..ci + colspan].iter())
                    .map(|t| t.trim())
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                c.clone()
            };
            let mut attrs = String::new();
            if span > 1 {
                attrs.push_str(&format!(" rowspan=\"{span}\""));
            }
            if colspan > 1 {
                attrs.push_str(&format!(" colspan=\"{colspan}\""));
            }
            out.push_str(&format!("<{tag}{attrs}>{}</{tag}>", cell(&text)));
        }
        out.push_str("</tr>\n");
    }
}

/// The merge flags a table is written with: each cell continues the one
/// above (`down`) or to its left (`left`) as flagged, but for a cell holding
/// text that no written cell's span takes in. A span is written once, by its
/// first cell, with the text of every cell it covers; a `band` row is
/// written whole as one cell, and rows before `skip` not at all. A cell
/// holding text outside every span so written would be lost: it is written
/// as a cell of its own.
fn spans_kept(
    rows: &[Vec<String>],
    down: Option<&Vec<bool>>,
    left: Option<&Vec<bool>>,
    skip: usize,
    band: impl Fn(usize) -> bool,
) -> (Option<Vec<bool>>, Option<Vec<bool>>) {
    let ncols = rows.first().map_or(0, Vec::len);
    let n = rows.len() * ncols;
    let (mut down, mut left) = (down.cloned(), left.cloned());
    let flagged = |m: &Option<Vec<bool>>, k: usize| m.as_ref().is_some_and(|m| m[k]);
    loop {
        let mut covered = vec![false; n];
        for ri in skip..rows.len() {
            if band(ri) {
                covered[ri * ncols..(ri + 1) * ncols].fill(true);
                continue;
            }
            for ci in 0..ncols {
                let k = ri * ncols + ci;
                if flagged(&down, k) || flagged(&left, k) {
                    continue;
                }
                let span = (ri + 1..rows.len())
                    .take_while(|&r| flagged(&down, r * ncols + ci))
                    .count()
                    + 1;
                let colspan = (ci + 1..ncols)
                    .take_while(|&c| flagged(&left, ri * ncols + c))
                    .count()
                    + 1;
                for r in ri..ri + span {
                    covered[r * ncols + ci..r * ncols + ci + colspan].fill(true);
                }
            }
        }
        let lost: Vec<usize> = (skip * ncols..n)
            .filter(|&k| !covered[k] && !rows[k / ncols][k % ncols].trim().is_empty())
            .collect();
        if lost.is_empty() {
            return (down, left);
        }
        for k in lost {
            for m in [&mut down, &mut left].into_iter().flatten() {
                m[k] = false;
            }
        }
    }
}

/// Whether a table's structure needs more than a grid of cells to say: a
/// cell holding a value that spans rows or columns, a header of two rows or
/// more, or a band across it labelling the rows under it.
///
/// An empty cell continued into its neighbours reads the same as the blank
/// cells a grid leaves there: a worksheet's empty answer boxes, ruled two
/// rows tall, need no markup to say so.
fn spans_cells(e: &Element) -> bool {
    let Some(rows) = &e.cells else {
        return false;
    };
    let ncols = rows.first().map_or(0, Vec::len);
    let n = rows.len() * ncols;
    let flags = |m: &Option<Vec<bool>>| m.clone().filter(|m| ncols > 0 && m.len() == n);
    let text = |r: usize, c: usize| rows[r].get(c).is_some_and(|t| !t.trim().is_empty());
    // The cell each flagged one continues, up or to the left.
    let spans_a_value = |m: Option<Vec<bool>>, up: bool| {
        m.is_some_and(|m| {
            (0..n).any(|k| {
                let (mut r, mut c) = (k / ncols, k % ncols);
                if !m[k] {
                    return false;
                }
                let held = text(r, c);
                while m[r * ncols + c] && (if up { r > 0 } else { c > 0 }) {
                    if up {
                        r -= 1;
                    } else {
                        c -= 1;
                    }
                }
                held || text(r, c)
            })
        })
    };
    spans_a_value(flags(&e.merged_down), true)
        || spans_a_value(flags(&e.merged_left), false)
        || e.header_rows.is_some_and(|h| h >= 2)
        || e.sub_headers.as_ref().is_some_and(|s| !s.is_empty())
}

/// Whether the table at `i` is written to Markdown as HTML: some part of the
/// table it belongs to, its parts across page breaks taken together, spans
/// cells ([`spans_cells`]). One table is written one way, so a part carried
/// on from the page before goes on in the markup its first part opened.
fn md_table_as_html(elements: &[Element], i: usize) -> bool {
    let mut first = i;
    while first > 0 && carries_on_table(elements, first) {
        first -= 1;
    }
    let mut last = i;
    while carries_on_table(elements, last + 1) {
        last += 1;
    }
    elements[first..=last].iter().any(spans_cells)
}

/// Close the open lists past the first `keep`, innermost first, with the
/// item each was set in.
fn close_lists(out: &mut String, lists: &mut Vec<usize>, keep: usize) {
    while lists.len() > keep {
        lists.pop();
        out.push_str("</ul>\n");
        if !lists.is_empty() {
            out.push_str("</li>\n");
        }
    }
}

pub fn to_xhtml(elements: &[Element]) -> String {
    to_xhtml_with(elements, &Notes::default())
}

/// As [`to_xhtml`], with document-level notes emitted in band.
///
/// The note is an HTML comment. It has to travel with the markup — a warning
/// on stderr is gone by the time anyone reads the file — but it must not be
/// text, because a consumer extracting text would then read a sentence this
/// library wrote as though the document had said it.
pub fn to_xhtml_with(elements: &[Element], notes: &Notes) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>\n",
    );
    // The depths of the lists open here, outermost first. Each but the
    // innermost sits in an item left open to hold the list after it.
    let mut lists: Vec<usize> = Vec::new();
    let mut open_figure: Option<String> = None;
    for (i, e) in elements.iter().enumerate() {
        if open_figure.is_some() && e.kind != "doco:Figure" {
            out.push_str("</figure>\n");
            open_figure = None;
        }
        if e.kind != "doc:ListItem" {
            close_lists(&mut out, &mut lists, 0);
        }
        match e.kind.as_str() {
            // The document's own title reads as its top heading.
            "doco:Title" => out.push_str(&format!("<h1>{}</h1>\n", html_linked(e))),
            "doco:SectionTitle" => {
                let l = e.level.unwrap_or(1).clamp(1, 6);
                out.push_str(&format!(
                    "<h{l}{}>{}</h{l}>\n",
                    if level_is_inferred(e) {
                        " class=\"doco-level-uncertain\""
                    } else {
                        ""
                    },
                    html_linked(e)
                ));
            }
            "doc:ListItem" => {
                // Close the lists deeper than the item; the outermost takes
                // the shallowest depth it holds.
                let d = e.depth;
                while lists.len() > 1 && lists.last().is_some_and(|&l| l > d) {
                    lists.pop();
                    out.push_str("</ul>\n");
                    // The item that held it ends, unless this item is
                    // deeper than it too and opens a list beside it there.
                    if lists.last().is_some_and(|&l| l >= d) {
                        out.push_str("</li>\n");
                    }
                }
                if let [outer] = lists.as_mut_slice() {
                    *outer = (*outer).min(d);
                }
                if lists.last().is_none_or(|&l| l < d) {
                    out.push_str("<ul>\n");
                    lists.push(d);
                }
                // An item holding a deeper list stays open for it.
                let holds = elements
                    .get(i + 1)
                    .is_some_and(|n| n.kind == "doc:ListItem" && n.depth > d);
                out.push_str(&format!(
                    "<li>{}{}",
                    html_linked(e),
                    if holds { "\n" } else { "</li>\n" }
                ));
            }
            "doco:Table" => {
                if let Some(rows) = &e.cells {
                    // A part carried on from the page before goes on in
                    // the table it continues, without a repeated header.
                    let carried = carries_on_table(elements, i);
                    let skip = if carried && out.ends_with("</table>\n") {
                        out.truncate(out.len() - "</table>\n".len());
                        repeated_header_rows(elements, i)
                    } else {
                        out.push_str("<table>\n");
                        0
                    };
                    html_table_rows(e, rows, skip, esc, &mut out);
                    out.push_str("</table>\n");
                } else if !e.text.is_empty() {
                    // Model-arbitrated table: already HTML.
                    out.push_str(&e.text);
                    out.push('\n');
                }
            }
            // Fragments of one drawing are wrapped together, in the order
            // the page sets them, which `<figure>` marks as a grouping
            // rather than a sentence. Nothing here says which label goes
            // with which value: that pairing lives in the drawing, and only
            // a reader that can see it may assert it.
            "doco:Figure" => {
                if open_figure.as_deref() != e.figure.as_deref() {
                    if open_figure.is_some() {
                        out.push_str("</figure>\n");
                    }
                    // The name, not adjacency, carries the grouping. Two
                    // charts set side by side interleave their labels in
                    // reading order, so a run of `<figure>` elements can
                    // belong to either; keeping document order and naming the
                    // drawing lets a consumer reassemble one without the
                    // emitter having to move text to do it. `data-figure`
                    // rather than `id` because the same drawing legitimately
                    // opens more than once, and ids must be unique.
                    match &e.figure {
                        Some(id) => {
                            out.push_str(&format!("<figure data-figure=\"{}\">\n", esc(id)))
                        }
                        None => out.push_str("<figure>\n"),
                    }
                    open_figure = e.figure.clone();
                }
                out.push_str(&format!("<span>{}</span>\n", html_linked(e)));
            }
            _ => match (&e.turn, &e.message) {
                (Some(t), _) => out.push_str(&html_turn(e, t)),
                (None, Some(m)) => out.push_str(&html_message(e, m)),
                // A paragraph whose lines were stacked by hand, an
                // address, keeps their breaks.
                (None, None) => out.push_str(&format!(
                    "<p>{}</p>\n",
                    html_linked(e).replace('\n', "<br/>\n")
                )),
            },
        }
    }
    close_lists(&mut out, &mut lists, 0);
    if open_figure.is_some() {
        out.push_str("</figure>\n");
    }
    if let Some(note) = notes.summary() {
        out.push_str(&format!("<!-- {} -->\n", comment_safe(&note)));
    }
    out.push_str("</body></html>\n");
    out
}

/// `text` made safe to sit in an HTML or XML comment, which may not hold
/// `--`: each `-` of such a run becomes a hyphen (U+2010), so a flag the
/// text names (`--probe-images`) still reads as itself, where a dash in its
/// place (`—probe-images`) read as punctuation.
fn comment_safe(text: &str) -> String {
    if !text.contains("--") {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_run = false;
    while let Some(c) = chars.next() {
        if c == '-' && (in_run || chars.peek() == Some(&'-')) {
            out.push('\u{2010}');
            in_run = chars.peek() == Some(&'-');
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

/// A table cell, made safe to sit between two pipes.
///
/// The row separator is the only structure a Markdown table has, so a cell
/// carrying a literal `|` silently becomes two cells and the row no longer
/// lines up with its header — which is worse than losing the character,
/// because the table still parses and now says something untrue. A newline
/// ends the row outright. Both occur in real documents: 18 tables across the
/// evaluation corpus are ragged for this reason, one of them a Chinese
/// worksheet whose cells contain `|` as punctuation.
///
/// GFM reads `\|` as a literal pipe inside a cell, and has no way to express
/// a line break in one, so a newline becomes a space.
///
/// The backslash has to be escaped as well, and not for symmetry: a cell
/// whose text *ends* in one turns the delimiter that follows it into `\|`,
/// which eats the delimiter and merges the cell with its neighbour. Two of
/// the eighteen ragged rows survived escaping the pipe alone for exactly
/// this reason, both of them mathematics set with trailing backslashes.
///
/// A `]` before `(` is escaped as it is in running text (see [`md_text`]), so
/// a cell of mathematics does not render as a link.
fn md_cell(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            '\n' | '\r' => out.push(' '),
            ']' if chars.peek() == Some(&'(') => out.push_str("\\]"),
            c => out.push(c),
        }
    }
    out
}

/// Render elements to Markdown, for benchmark comparison.
///
/// A table is a pipe table where a grid of cells says all of it. One that
/// does not (a cell holding a value that spans rows or columns, a header of
/// two rows or more, a band labelling the rows under it) is written as an
/// HTML `<table>`, which Markdown passes through, with its `rowspan`,
/// `colspan` and `<th>` cells as the `xhtml` writer sets them. A pipe table
/// can only leave a span's other positions blank; repeating its value in each
/// of them instead measured worse still (TEDS 0.8288 against 0.8441), since a
/// reference encodes a span as one cell, not N copies. `doco` carries the
/// spans as flags; see [`crate::merges`].
pub fn to_markdown(elements: &[Element]) -> String {
    to_markdown_with(elements, &Notes::default())
}

/// As [`to_markdown`], with document-level notes emitted in band.
///
/// An HTML comment, which Markdown passes through and no renderer shows —
/// visible to anything reading the file, invisible in the rendered document,
/// and not part of the prose.
pub fn to_markdown_with(elements: &[Element], notes: &Notes) -> String {
    to_markdown_with_options(elements, notes, &MarkdownOptions::default())
}

/// As [`to_markdown_with`], with the choices in `opts`.
pub fn to_markdown_with_options(
    elements: &[Element],
    notes: &Notes,
    opts: &MarkdownOptions,
) -> String {
    let mut out = String::new();
    // The lists open here, outermost first: each one's depth, the indent of
    // its items, and the column its last item's text starts at, where a
    // list inside that item is indented to.
    let mut lists: Vec<(usize, usize, usize)> = Vec::new();
    for (i, e) in elements.iter().enumerate() {
        if e.kind != "doc:ListItem" {
            lists.clear();
        }
        match e.kind.as_str() {
            "doco:Title" => out.push_str(&format!("\n# {}\n\n", md_linked(e, opts))),
            "doco:SectionTitle" => {
                let l = e.level.unwrap_or(1).clamp(1, 6);
                out.push_str(&format!("\n{} {}\n\n", "#".repeat(l), md_linked(e, opts)));
            }
            "doco:Table" => {
                if let Some(rows) = &e.cells {
                    // A part carried on from the page before goes on in the
                    // table it continues: no blank line, no second header.
                    let carried = carries_on_table(elements, i);
                    // A table whose cells span rows or columns is written
                    // as HTML, which Markdown passes through: a grid of pipes
                    // can only leave a span's other cells blank. Each cell
                    // on one line, since a blank line would end the markup.
                    if md_table_as_html(elements, i) {
                        let skip = if carried && out.ends_with("</table>\n\n") {
                            out.truncate(out.len() - "</table>\n\n".len());
                            repeated_header_rows(elements, i)
                        } else {
                            out.push_str("\n<table>\n");
                            0
                        };
                        let cell = |t: &str| esc(&t.replace(['\n', '\r'], " "));
                        html_table_rows(e, rows, skip, cell, &mut out);
                        out.push_str("</table>\n\n");
                        continue;
                    }
                    let skip = if carried {
                        out.pop();
                        repeated_header_rows(elements, i)
                    } else {
                        out.push('\n');
                        0
                    };
                    for (ri, r) in rows.iter().enumerate().skip(skip) {
                        let cells: Vec<String> = r.iter().map(|c| md_cell(c)).collect();
                        out.push_str(&format!("|{}|\n", cells.join("|")));
                        if ri == 0 && !carried {
                            out.push_str(&format!("|{}|\n", vec!["---"; r.len()].join("|")));
                        }
                    }
                    out.push('\n');
                } else if !e.text.is_empty() {
                    // A model-arbitrated table: raw HTML carries the
                    // structure (spans included), which markdown cannot.
                    out.push_str(&format!("\n{}\n\n", e.text));
                }
            }
            "doc:ListItem" => {
                let d = e.depth;
                while lists.len() > 1 && lists.last().is_some_and(|l| l.0 > d) {
                    lists.pop();
                }
                if let [outer] = lists.as_mut_slice() {
                    outer.0 = outer.0.min(d);
                }
                match lists.last() {
                    None => lists.push((d, 0, 0)),
                    Some(&(depth, _, text_at)) if depth < d => lists.push((d, text_at, 0)),
                    Some(_) => {}
                }
                let open = lists.last_mut().unwrap();
                let indent = " ".repeat(open.1);
                let text = md_linked(e, opts);
                if opens_with_ordinal(&e.text) {
                    // An ordered list's item that carries its own number,
                    // `3. FAQ`: the number is the item's Markdown marker,
                    // and a bullet before it would set a numbered line
                    // inside a bulleted item.
                    let marker = e.text.find(' ').unwrap_or(0) + 1;
                    open.2 = open.1 + marker;
                    out.push_str(&format!("{indent}{text}\n"));
                } else {
                    open.2 = open.1 + 2;
                    out.push_str(&format!("{indent}- {text}\n"));
                }
            }
            // A figure with no words of its own (no alternative text, no
            // label) prints nothing: a blank paragraph would say nothing
            // either. What was read in its image follows it as paragraphs
            // of their own.
            "doco:Figure" if e.text.trim().is_empty() => {}
            _ => {
                // A list's last item ends its line and no more. A paragraph
                // set straight under it is a lazy continuation of the item
                // under CommonMark, read as the item's own text: a blank line
                // ends the list first.
                if !out.is_empty() && !out.ends_with("\n\n") {
                    out.push('\n');
                }
                match (&e.turn, &e.message) {
                    (Some(t), _) => out.push_str(&format!("{}\n\n", md_turn(e, t))),
                    (None, Some(m)) => out.push_str(&format!("{}\n\n", md_message(e, m))),
                    // A paragraph whose lines were stacked by hand keeps their
                    // breaks, as CommonMark hard breaks: a plain line end reads
                    // as a space.
                    (None, None) => {
                        out.push_str(&format!("{}\n\n", md_linked(e, opts).replace('\n', "  \n")))
                    }
                }
            }
        }
    }
    if let Some(note) = notes.summary() {
        out.push_str(&format!("\n<!-- {} -->\n", comment_safe(&note)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::{Link, Target};

    fn para(text: &str, links: Vec<Link>) -> Element {
        Element {
            links: (!links.is_empty()).then_some(links),
            ..Element::new("doco:Paragraph", text, "rust", "layout")
        }
    }

    fn table(id: &str, rows: &[&[&str]], header: usize) -> Element {
        Element {
            id: id.into(),
            cells: Some(
                rows.iter()
                    .map(|r| r.iter().map(|c| c.to_string()).collect())
                    .collect(),
            ),
            header_rows: Some(header),
            ..Element::new("doco:Table", "", "rust", "rules")
        }
    }

    fn item(text: &str, depth: usize) -> Element {
        Element {
            depth,
            ..Element::new("doc:ListItem", text, "rust", "layout")
        }
    }

    #[test]
    fn a_figure_with_no_words_prints_nothing_in_markdown() {
        let figure = |text: &str| Element {
            figure: Some("figure-0-1".into()),
            ..Element::new("doco:Figure", text, "eml", "eml")
        };
        let els = [para("Before", vec![]), figure(""), para("After", vec![])];
        assert_eq!(to_markdown(&els), "Before\n\nAfter\n\n");
        let els = [
            para("Before", vec![]),
            figure("Org chart"),
            para("After", vec![]),
        ];
        assert_eq!(to_markdown(&els), "Before\n\nOrg chart\n\nAfter\n\n");
    }

    #[test]
    fn a_paragraphs_kept_line_breaks_are_hard_breaks() {
        let address = para("Pino Bluebird\nSessamestreet 32", vec![]);
        assert!(to_markdown(std::slice::from_ref(&address))
            .contains("Pino Bluebird  \nSessamestreet 32\n\n"));
        assert!(to_xhtml(&[address]).contains("<p>Pino Bluebird<br/>\nSessamestreet 32</p>"));
    }

    #[test]
    fn a_nested_list_nests_in_xhtml_and_markdown() {
        let els = [
            item("Fruit", 0),
            item("Apples", 1),
            item("Seedless", 2),
            item("Vegetables", 0),
            item("10. Leeks", 1),
            item("Onions", 2),
            para("After", vec![]),
        ];
        let html = to_xhtml(&els);
        let body = html.split("<body>\n").nth(1).unwrap();
        assert_eq!(
            body.split("</body>").next().unwrap(),
            "<ul>\n<li>Fruit\n<ul>\n<li>Apples\n<ul>\n<li>Seedless</li>\n</ul>\n</li>\n</ul>\n</li>\n\
             <li>Vegetables\n<ul>\n<li>10. Leeks\n<ul>\n<li>Onions</li>\n</ul>\n</li>\n</ul>\n</li>\n</ul>\n\
             <p>After</p>\n"
        );
        // Each sub-list is indented to where its parent item's text starts,
        // past a bullet or past the item's own number. The paragraph after
        // the list stands apart from its last item, a blank line between.
        assert_eq!(
            to_markdown(&els),
            "- Fruit\n  - Apples\n    - Seedless\n- Vegetables\n  10. Leeks\n      - Onions\n\nAfter\n\n"
        );
        // Back out of a list two deeper to one deeper: a second list in the
        // same item.
        let skip = [item("Fruit", 0), item("Seedless", 2), item("Apples", 1)];
        let body = to_xhtml(&skip);
        assert!(body.contains(
            "<li>Fruit\n<ul>\n<li>Seedless</li>\n</ul>\n<ul>\n<li>Apples</li>\n</ul>\n</li>\n</ul>\n"
        ));
        assert_eq!(to_markdown(&skip), "- Fruit\n  - Seedless\n  - Apples\n");
        // A flat list reads as it always has.
        let flat = [item("one", 0), item("two", 0)];
        assert_eq!(to_markdown(&flat), "- one\n- two\n");
        assert!(to_xhtml(&flat).contains("<ul>\n<li>one</li>\n<li>two</li>\n</ul>\n"));
    }

    #[test]
    fn a_paragraph_after_a_list_is_no_part_of_its_last_item() {
        let els = [
            item("Spatula", 0),
            item("Weigh paper", 0),
            para("Figure 3.", vec![]),
        ];
        assert_eq!(
            to_markdown(&els),
            "- Spatula\n- Weigh paper\n\nFigure 3.\n\n"
        );
        // Headings and tables after a list already open on a blank line, and
        // a paragraph after a paragraph has one: none gains a second.
        let heading = Element {
            level: Some(2),
            ..Element::new("doco:SectionTitle", "Method", "rust", "layout")
        };
        assert_eq!(
            to_markdown(&[item("Spatula", 0), heading]),
            "- Spatula\n\n## Method\n\n"
        );
        assert_eq!(
            to_markdown(&[
                item("Spatula", 0),
                table("t", &[&["A", "B"], &["1", "2"]], 1)
            ]),
            "- Spatula\n\n|A|B|\n|---|---|\n|1|2|\n\n"
        );
        assert_eq!(
            to_markdown(&[para("One.", vec![]), para("Two.", vec![])]),
            "One.\n\nTwo.\n\n"
        );
    }

    #[test]
    fn a_continued_table_reads_as_one_table() {
        let a = table("t1", &[&["Problem", "Cause"], &["Noise", "Dirty cell"]], 1);
        // Carried on, its header repeated: the repeat is left out.
        let mut b = table("t2", &[&["Problem", "Cause"], &["", "Dirty system"]], 1);
        b.continues = Some("t1".into());
        assert_eq!(
            to_markdown(&[a.clone(), b.clone()]),
            "\n|Problem|Cause|\n|---|---|\n|Noise|Dirty cell|\n||Dirty system|\n\n"
        );
        let html = to_xhtml(&[a.clone(), b]);
        assert_eq!(html.matches("<table>").count(), 1);
        assert_eq!(html.matches("Problem").count(), 1);
        // Not carried on: two tables.
        let c = table("t3", &[&["X", "Y"], &["1", "2"]], 1);
        assert_eq!(to_markdown(&[a, c]).matches("|---|---|").count(), 2);
    }

    #[test]
    fn a_table_with_spans_is_html_in_markdown_and_one_without_is_pipes() {
        // A plain table, one header row: pipes, as ever.
        let plain = table("t", &[&["Name", "Count"], &["alpha", "12"]], 1);
        assert_eq!(
            to_markdown(std::slice::from_ref(&plain)),
            "\n|Name|Count|\n|---|---|\n|alpha|12|\n\n"
        );
        // `Measures` spans two columns over a second header row.
        let mut spanned = table(
            "s",
            &[
                &["Name", "Measures", ""],
                &["", "Count", "Share"],
                &["alpha", "12", "30"],
            ],
            2,
        );
        spanned.merged_down = Some(vec![
            false, false, false, true, false, false, false, false, false,
        ]);
        spanned.merged_left = Some(vec![
            false, false, true, false, false, false, false, false, false,
        ]);
        let md = to_markdown(std::slice::from_ref(&spanned));
        assert_eq!(
            md,
            "\n<table>\n\
             <tr><th rowspan=\"2\">Name</th><th colspan=\"2\">Measures</th></tr>\n\
             <tr><th>Count</th><th>Share</th></tr>\n\
             <tr><td>alpha</td><td>12</td><td>30</td></tr>\n\
             </table>\n\n"
        );
        // The same rows as `xhtml` writes them.
        assert!(to_xhtml(&[spanned]).contains(md.trim()));
        // Two header rows and no span: HTML too, each header cell a `<th>`.
        let two = table("h", &[&["Size", "Mass"], &["mm", "g"], &["4", "12"]], 2);
        assert!(to_markdown(&[two]).contains("<tr><th>mm</th><th>g</th></tr>"));
        // An empty cell ruled two rows tall spans nothing a grid cannot say.
        let mut boxes = table("b", &[&["Item", "Answer"], &["one", ""], &["two", ""]], 1);
        boxes.merged_down = Some(vec![false, false, false, false, false, true]);
        assert!(to_markdown(&[boxes]).starts_with("\n|Item|Answer|"));
        // Text set on two lines in a cell stays on one: a blank line would
        // end the markup.
        let mut wrapped = table("w", &[&["A", "B"], &["x\n\ny", "1"], &["", "2"]], 1);
        wrapped.merged_down = Some(vec![false, false, false, false, true, false]);
        assert!(to_markdown(&[wrapped]).contains("<td rowspan=\"2\">x  y</td>"));
    }

    #[test]
    fn a_cell_holding_text_is_never_hidden_under_a_span() {
        // A form's label over its value, the value's cell flagged as the
        // rest of the label's: the value is written, with the label in the
        // one cell the span is written as.
        let mut form = table(
            "f",
            &[
                &["Order", "Contract"],
                &["1. DATE OF ORDER", "89303321"],
                &["05/13/2021", ""],
            ],
            1,
        );
        form.merged_down = Some(vec![false, false, false, false, true, true]);
        for out in [
            to_markdown(std::slice::from_ref(&form)),
            to_xhtml(std::slice::from_ref(&form)),
        ] {
            assert!(
                out.contains("rowspan=\"2\">1. DATE OF ORDER 05/13/2021<"),
                "{out}"
            );
            // The blank cell under the contract number still continues it.
            assert!(out.contains("rowspan=\"2\">89303321<"), "{out}");
        }
        // Under a band written whole as one cell, a cell flagged as the rest
        // of the band's holds text no span takes in: it is a cell of its own.
        let mut banded = table("b", &[&["Totals", "", ""], &["2024", "12", "30"]], 1);
        banded.merged_down = Some(vec![false, false, false, false, true, false]);
        let out = to_markdown(std::slice::from_ref(&banded));
        assert!(out.contains("<td>12</td>"), "{out}");
    }

    #[test]
    fn a_continued_table_with_spans_is_one_html_table() {
        let mut a = table("t1", &[&["Group", ""], &["Noise", "Dirty cell"]], 1);
        a.merged_left = Some(vec![false, true, false, false]);
        // Carried on, its header repeated; it spans nothing itself, and is
        // written as the part it continues.
        let mut b = table("t2", &[&["Group", ""], &["Hum", "Loose lead"]], 1);
        b.merged_left = Some(vec![false, true, false, false]);
        b.continues = Some("t1".into());
        let mut c = table("t3", &[&["Group", ""], &["Buzz", "Earth"]], 1);
        c.continues = Some("t2".into());
        let md = to_markdown(&[a, b, c]);
        assert_eq!(md.matches("<table>").count(), 1, "{md}");
        assert_eq!(md.matches("Group").count(), 1, "{md}");
        assert!(md.contains("<tr><td>Hum</td><td>Loose lead</td></tr>\n"));
        assert!(!md.contains('|'), "{md}");
    }

    #[test]
    fn an_anchor_becomes_a_markdown_link() {
        let e = para(
            "see the filing for detail",
            vec![Link::uri("https://example.org/f").spanning(4, 14)],
        );
        assert_eq!(
            to_markdown(&[e]).trim(),
            "see [the filing](https://example.org/f) for detail"
        );
    }

    #[test]
    fn an_anchor_that_is_its_own_address_autolinks() {
        let e = para(
            "https://example.org/f",
            vec![Link::uri("https://example.org/f").spanning(0, 21)],
        );
        assert_eq!(to_markdown(&[e]).trim(), "<https://example.org/f>");
    }

    #[test]
    fn the_full_stop_after_an_address_stays_outside_its_autolink() {
        // The link rectangle took in the sentence's full stop.
        let e = para(
            "see https://example.org/f.",
            vec![Link::uri("https://example.org/f").spanning(4, 26)],
        );
        assert_eq!(to_markdown(&[e]).trim(), "see <https://example.org/f>.");
    }

    #[test]
    fn an_address_without_a_scheme_is_not_an_autolink() {
        // `<www.example.org>` renders as literal text, brackets and all.
        let e = para(
            "www.example.org",
            vec![Link::uri("www.example.org").spanning(0, 15)],
        );
        assert_eq!(
            to_markdown(&[e]).trim(),
            "[www.example.org](www.example.org)"
        );
    }

    #[test]
    fn links_as_text_keep_the_words_and_drop_the_addresses() {
        let e = para(
            "see the filing at https://example.org/f.",
            vec![
                Link::uri("https://example.org/a").spanning(4, 14),
                Link::uri("https://example.org/f").spanning(18, 40),
            ],
        );
        let opts = MarkdownOptions {
            links: MdLinks::Text,
        };
        assert_eq!(
            to_markdown_with_options(&[e], &Notes::default(), &opts).trim(),
            "see the filing at https://example.org/f."
        );
    }

    #[test]
    fn bracketed_mathematics_does_not_read_as_a_link() {
        let e = para("Γ[w](t) = (t, w(t))", vec![]);
        assert_eq!(to_markdown(&[e]).trim(), "Γ[w\\](t) = (t, w(t))");
        let t = table("t", &[&["f", "g"], &["Γ[w](t)", "x"]], 1);
        assert!(to_markdown(&[t]).contains("|Γ[w\\](t)|x|"));
        // As text, the anchor's words get the same escape.
        let e = para("Γ[w](t)", vec![Link::uri("https://e.org").spanning(4, 7)]);
        let opts = MarkdownOptions {
            links: MdLinks::Text,
        };
        assert_eq!(
            to_markdown_with_options(&[e], &Notes::default(), &opts).trim(),
            "Γ[w\\](t)"
        );
    }

    #[test]
    fn a_line_shaped_like_a_link_definition_is_kept() {
        // A renderer shows a definition as nothing at all.
        let e = para("[1]: Smith, 2019", vec![]);
        assert_eq!(to_markdown(&[e]).trim(), "\\[1]: Smith, 2019");
        // Only where it opens the block: mid-sentence it defines nothing.
        let e = para("see [1]: Smith", vec![]);
        assert_eq!(to_markdown(&[e]).trim(), "see [1]: Smith");
        let e = para("[1] Smith, 2019", vec![]);
        assert_eq!(to_markdown(&[e]).trim(), "[1] Smith, 2019");
    }

    #[test]
    fn an_internal_jump_becomes_a_page_fragment() {
        let e = para("see Chapter 4", vec![Link::page(11).spanning(4, 13)]);
        assert_eq!(to_markdown(&[e]).trim(), "see [Chapter 4](#page=12)");
    }

    #[test]
    fn a_bracket_in_an_anchor_does_not_close_the_label() {
        let e = para(
            "see [a] here",
            vec![Link::uri("https://e.org").spanning(4, 7)],
        );
        assert!(to_markdown(&[e]).contains("[\\[a\\]](https://e.org)"));
    }

    #[test]
    fn a_destination_with_a_paren_takes_the_angle_form() {
        let e = para("ref", vec![Link::uri("https://e.org/a(b)").spanning(0, 3)]);
        assert_eq!(to_markdown(&[e]).trim(), "[ref](<https://e.org/a(b)>)");
    }

    #[test]
    fn text_between_and_around_anchors_survives() {
        let e = para(
            "a b c d",
            vec![
                Link::uri("https://one.example").spanning(0, 1),
                Link::uri("https://two.example").spanning(4, 5),
            ],
        );
        assert_eq!(
            to_markdown(&[e]).trim(),
            "[a](https://one.example) b [c](https://two.example) d"
        );
    }

    #[test]
    fn an_anchor_becomes_an_html_link_with_escaping_intact() {
        let e = para(
            "a & <b> c",
            vec![Link::uri("https://e.org/?x=1&y=2").spanning(4, 7)],
        );
        let html = to_xhtml(&[e]);
        assert!(
            html.contains("<p>a &amp; <a href=\"https://e.org/?x=1&amp;y=2\">&lt;b&gt;</a> c</p>"),
            "{html}"
        );
    }

    #[test]
    fn a_link_with_no_located_anchor_leaves_the_text_alone() {
        let e = para("a picture", vec![Link::uri("https://e.org")]);
        assert_eq!(to_markdown(std::slice::from_ref(&e)).trim(), "a picture");
        assert!(to_xhtml(&[e]).contains("<p>a picture</p>"));
    }

    #[test]
    fn a_span_past_the_end_of_the_text_is_skipped_rather_than_panicking() {
        let e = para("short", vec![Link::uri("https://e.org").spanning(2, 99)]);
        assert_eq!(to_markdown(&[e]).trim(), "short");
    }

    #[test]
    fn an_unread_page_is_declared_in_band_but_is_not_text() {
        use crate::element::{Notes, UnreadPage};
        let notes = Notes {
            unread: vec![UnreadPage {
                index: 0,
                reason: "NearBlank".into(),
            }],
            ..Default::default()
        };
        let e = para("MASTER DRAWING", vec![]);
        let x = to_xhtml_with(std::slice::from_ref(&e), &notes);
        assert!(
            x.contains("<!-- fluree-doc-parse: page 1 carries content"),
            "{x}"
        );
        // A comment, so extracting text from the markup does not read a
        // sentence this library wrote as though the document had said it.
        assert!(!x.contains("<p>fluree-doc-parse"));
        assert!(to_markdown_with(&[e], &notes).contains("<!-- fluree-doc-parse:"));
    }

    #[test]
    fn a_documents_title_reads_as_its_top_heading() {
        let mut t = para("Annual Report", vec![]);
        t.kind = "doco:Title".into();
        assert!(to_markdown(std::slice::from_ref(&t)).contains("\n# Annual Report\n"));
        assert!(to_xhtml(std::slice::from_ref(&t)).contains("<h1>Annual Report</h1>"));
    }

    #[test]
    fn a_level_read_off_the_page_says_so_and_a_declared_one_does_not() {
        let mut inferred = para("From waste to new beginnings", vec![]);
        inferred.kind = "doco:SectionTitle".into();
        inferred.level = Some(6);
        inferred.evidence = "font-size";
        assert!(to_xhtml(std::slice::from_ref(&inferred))
            .contains("<h6 class=\"doco-level-uncertain\">"));

        // The author stated these, one way or another.
        for declared in ["outline", "numbering", "title"] {
            let mut e = para("1. Empowerment", vec![]);
            e.kind = "doco:SectionTitle".into();
            e.level = Some(1);
            e.evidence = declared;
            let x = to_xhtml(std::slice::from_ref(&e));
            assert!(x.contains("<h1>"), "{declared}: {x}");
            assert!(!x.contains("doco-level-uncertain"), "{declared}");
        }
    }

    #[test]
    fn a_healthy_document_says_nothing() {
        let e = para("body", vec![]);
        let quiet = crate::element::Notes::default();
        assert!(!to_xhtml_with(std::slice::from_ref(&e), &quiet).contains("fluree-doc-parse"));
        assert!(!to_markdown_with(&[e], &quiet).contains("fluree-doc-parse"));
        assert_eq!(quiet.summary(), None);
    }

    #[test]
    fn several_unread_pages_are_listed_once_each_with_their_reasons() {
        use crate::element::{Notes, UnreadPage};
        let n = Notes {
            unread: vec![
                UnreadPage {
                    index: 4,
                    reason: "Scanned".into(),
                },
                UnreadPage {
                    index: 0,
                    reason: "NearBlank".into(),
                },
                UnreadPage {
                    index: 6,
                    reason: "Scanned".into(),
                },
            ],
            ..Default::default()
        };
        let s = n.summary().unwrap();
        assert!(s.contains("pages 1, 5, 7"), "{s}");
        assert!(s.contains("NearBlank, Scanned"), "{s}");
    }

    #[test]
    fn a_comment_holds_no_double_hyphen_and_a_flag_still_reads() {
        assert_eq!(
            comment_safe("probe (--probe-images), a well-known one---or not"),
            "probe (\u{2010}\u{2010}probe-images), a well-known one\u{2010}\u{2010}\u{2010}or not"
        );
        assert!(!comment_safe("a --- b -- c -").contains("--"));
        assert_eq!(comment_safe("page 3 at 56,582"), "page 3 at 56,582");
    }

    #[test]
    fn an_unread_region_is_declared_with_its_page_and_box() {
        use crate::element::{Notes, UnreadPage, UnreadRegion};
        use crate::geom::BBox;
        let region = |page, y0| UnreadRegion {
            page,
            bbox: BBox {
                x0: 56.0,
                y0,
                x1: 556.4,
                y1: y0 + 150.0,
            },
            reason: "Region".into(),
        };
        let one = Notes {
            unread_regions: vec![region(2, 582.0)],
            ..Default::default()
        };
        assert!(!one.is_empty());
        assert_eq!(
            one.summary().as_deref(),
            Some(
                "fluree-doc-parse: 1 raster region carries content no reader \
                 transcribed: page 3 at 56,582,556,732. This output is missing it."
            )
        );
        let e = para("Review memo", vec![]);
        let x = to_xhtml_with(std::slice::from_ref(&e), &one);
        assert!(x.contains("<!-- fluree-doc-parse: 1 raster region"), "{x}");
        assert!(to_markdown_with(&[e], &one).contains("page 3 at 56,582,556,732"));

        // A region the router declined is listed, not warned about.
        let declined = Notes {
            unread_regions: vec![UnreadRegion {
                reason: "Small".into(),
                ..region(0, 40.0)
            }],
            ..Default::default()
        };
        assert_eq!(declined.summary(), None);
        let mixed = Notes {
            unread_regions: vec![
                UnreadRegion {
                    reason: "NotTexty".into(),
                    ..region(0, 40.0)
                },
                region(2, 582.0),
            ],
            ..Default::default()
        };
        assert_eq!(mixed.summary(), one.summary());

        // Images not examined are listed, not warned about: a plain run
        // examines none, and the Markdown and XHTML carry no comment for
        // them, alone or beside what is missing.
        let unprobed = |page| UnreadRegion {
            reason: "Unprobed".into(),
            ..region(page, 40.0)
        };
        let alone = Notes {
            unread_regions: vec![unprobed(4), unprobed(0), unprobed(1)],
            ..Default::default()
        };
        assert_eq!(alone.summary(), None);
        let e = para("Review memo", vec![]);
        assert!(!to_markdown_with(std::slice::from_ref(&e), &alone).contains("<!--"));
        assert!(!to_xhtml_with(std::slice::from_ref(&e), &alone).contains("<!--"));
        let after = Notes {
            unread_regions: vec![unprobed(0), region(2, 582.0)],
            ..Default::default()
        };
        assert_eq!(after.summary(), one.summary());

        // Listed in page order, then down the page; after unread pages.
        let both = Notes {
            unread: vec![UnreadPage {
                index: 0,
                reason: "Scanned".into(),
            }],
            unread_regions: vec![region(4, 300.0), region(2, 582.0), region(2, 100.0)],
            ..Default::default()
        };
        let s = both.summary().unwrap();
        assert!(
            s.contains(
                "page 1 carries content no reader transcribed (Scanned). \
                 3 raster regions do too: page 3 at 56,100,556,250; \
                 page 3 at 56,582,556,732; page 5 at 56,300,556,450."
            ),
            "{s}"
        );
    }

    fn turn(text: &str, speaker: Option<&str>, start_ms: u64) -> Element {
        let mut e = para(text, vec![]);
        e.turn = Some(Turn {
            speaker: speaker.map(Into::into),
            start_ms,
            end_ms: start_ms + 4000,
        });
        e
    }

    #[test]
    fn a_turn_shows_a_reader_of_markdown_who_spoke_and_when() {
        let e = turn(
            "Ada Park: The pilot starts in May.",
            Some("Ada Park"),
            272_404,
        );
        assert_eq!(
            to_markdown(&[e]).trim(),
            "**Ada Park** (04:32): The pilot starts in May."
        );
        let e = turn("Today we look at rocks.", None, 3_723_000);
        assert_eq!(
            to_markdown(&[e]).trim(),
            "(1:02:03) Today we look at rocks."
        );
    }

    #[test]
    fn a_turn_in_xhtml_keeps_its_text_and_carries_time_on_attributes() {
        let e = turn("Ada Park: Q&A <later>", Some("Ada Park"), 1_000);
        let x = to_xhtml(&[e]);
        assert!(
            x.contains(
                "<p data-speaker=\"Ada Park\" data-start-ms=\"1000\" data-end-ms=\"5000\">\
                 <b>Ada Park</b>: Q&amp;A &lt;later&gt;</p>"
            ),
            "{x}"
        );
    }

    #[test]
    fn a_turn_serializes_beside_the_text_and_is_absent_elsewhere() {
        let v = serde_json::to_value(turn("Ada Park: Hi.", Some("Ada Park"), 1000)).unwrap();
        assert_eq!(
            v["turn"],
            serde_json::json!({"speaker": "Ada Park", "start_ms": 1000, "end_ms": 5000})
        );
        assert!(serde_json::to_value(para("x", vec![]))
            .unwrap()
            .get("turn")
            .is_none());
    }

    #[test]
    fn a_target_serializes_as_the_field_that_names_it() {
        let uri = serde_json::to_value(Target::Uri {
            uri: "https://e.org".into(),
        })
        .unwrap();
        assert_eq!(uri, serde_json::json!({"uri": "https://e.org"}));
        let page = serde_json::to_value(Target::Page { page: 3 }).unwrap();
        assert_eq!(page, serde_json::json!({"page": 3}));
    }
}
