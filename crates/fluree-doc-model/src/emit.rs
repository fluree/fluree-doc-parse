//! Markdown and XHTML emission from the element model.

use crate::element::{Element, Link, Notes};

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

/// An element's text with its links marked up as Markdown.
///
/// The autolink form is preferred where the anchor already *is* the address:
/// a page that prints `https://example.org/a` and links it there says the
/// same thing twice in `[…](…)`, and `<…>` is the idiom for exactly that.
fn md_linked(e: &Element) -> String {
    if e.links.is_none() {
        return e.text.clone();
    }
    let mut out = String::new();
    for (text, link) in link_segments(e) {
        match link {
            None => out.push_str(&text),
            Some(l) => {
                let href = l.href();
                if text == href {
                    out.push_str(&format!("<{href}>"));
                } else {
                    // `]` would close the label early; a destination with a
                    // space or a paren needs the angle-bracket form.
                    let label = text.replace('[', "\\[").replace(']', "\\]");
                    if href.contains(['(', ')', ' ', '<', '>']) {
                        out.push_str(&format!("[{label}](<{href}>)"));
                    } else {
                        out.push_str(&format!("[{label}]({href})"));
                    }
                }
            }
        }
    }
    out
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

/// Render elements to XHTML, matching the contract a downstream extraction
/// worker already consumes (headings, paragraphs, lists, tables; text nodes
/// carry the content). A drop-in for such a worker: downstream text
/// extraction and NER see the same shape they see today.
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
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>\n",
    );
    let mut in_list = false;
    let mut open_figure: Option<String> = None;
    for e in elements {
        if open_figure.is_some() && e.kind != "doco:Figure" {
            out.push_str("</figure>\n");
            open_figure = None;
        }
        if in_list && e.kind != "doco:ListItem" {
            out.push_str("</ul>\n");
            in_list = false;
        }
        match e.kind.as_str() {
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
            "doco:ListItem" => {
                if !in_list {
                    out.push_str("<ul>\n");
                    in_list = true;
                }
                out.push_str(&format!("<li>{}</li>\n", html_linked(e)));
            }
            "doco:Table" => {
                if let Some(rows) = &e.cells {
                    // `<th>` only where the header row was *measured*
                    // (`header_rows == Some(1)`); undetected tables keep
                    // plain `<td>` rather than asserting a header that may
                    // not exist.
                    let th_rows = e.header_rows.unwrap_or(0);
                    let subs = e.sub_headers.clone().unwrap_or_default();
                    let ncols = rows.first().map(Vec::len).unwrap_or(0);
                    let md = e
                        .merged_down
                        .as_ref()
                        .filter(|m| ncols > 0 && m.len() == rows.len() * ncols);
                    let ml = e
                        .merged_left
                        .as_ref()
                        .filter(|m| ncols > 0 && m.len() == rows.len() * ncols);
                    out.push_str("<table>\n");
                    for (ri, r) in rows.iter().enumerate() {
                        // A merged full-width band is one spanning header
                        // cell, not a row of blanks after the first column.
                        // A merged full-width band is one spanning header
                        // cell; its text may be split across columns in the
                        // raw grid, so join it back.
                        if subs.contains(&ri)
                            || (ri < th_rows && r[1..].iter().all(|c| c.is_empty()))
                        {
                            let joined = r
                                .iter()
                                .map(|c| c.trim())
                                .filter(|c| !c.is_empty())
                                .collect::<Vec<_>>()
                                .join(" ");
                            out.push_str(&format!(
                                "<tr><th colspan=\"{ncols}\">{}</th></tr>\n",
                                esc(&joined)
                            ));
                            continue;
                        }
                        let tag = if ri < th_rows { "th" } else { "td" };
                        out.push_str("<tr>");
                        for (ci, c) in r.iter().enumerate() {
                            // A cell continuing the one above or to its left
                            // was already emitted carrying that span.
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
                                .map(|m| {
                                    (ci + 1..ncols).take_while(|k| m[ri * ncols + k]).count() + 1
                                })
                                .unwrap_or(1);
                            // A horizontally merged run's text lives across
                            // the cells it spans in the raw grid.
                            let text = if colspan > 1 {
                                r[ci..ci + colspan]
                                    .iter()
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
                            out.push_str(&format!("<{tag}{attrs}>{}</{tag}>", esc(&text)));
                        }
                        out.push_str("</tr>\n");
                    }
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
            _ => out.push_str(&format!("<p>{}</p>\n", html_linked(e))),
        }
    }
    if in_list {
        out.push_str("</ul>\n");
    }
    if open_figure.is_some() {
        out.push_str("</figure>\n");
    }
    if let Some(note) = notes.summary() {
        out.push_str(&format!("<!-- {} -->\n", note.replace("--", "—")));
    }
    out.push_str("</body></html>\n");
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
fn md_cell(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Render elements to Markdown, for benchmark comparison.
///
/// Cells are emitted **as detected**, so a merged cell's value appears once
/// and the positions it spans are blank. That is deliberate and measured, not
/// an oversight: Markdown has no rowspan, so the alternative is repeating the
/// value in every position it covers, and doing that scores TEDS 0.8288
/// against 0.8441 — a reference encodes a rowspan as one cell, not N copies.
/// A consumer that needs the spans should read `doco` or `xhtml`, which carry
/// them properly; see [`crate::merges`].
pub fn to_markdown(elements: &[Element]) -> String {
    to_markdown_with(elements, &Notes::default())
}

/// As [`to_markdown`], with document-level notes emitted in band.
///
/// An HTML comment, which Markdown passes through and no renderer shows —
/// visible to anything reading the file, invisible in the rendered document,
/// and not part of the prose.
pub fn to_markdown_with(elements: &[Element], notes: &Notes) -> String {
    let mut out = String::new();
    for e in elements {
        match e.kind.as_str() {
            "doco:SectionTitle" => {
                let l = e.level.unwrap_or(1).clamp(1, 6);
                out.push_str(&format!("\n{} {}\n\n", "#".repeat(l), md_linked(e)));
            }
            "doco:Table" => {
                if let Some(rows) = &e.cells {
                    out.push('\n');
                    for (i, r) in rows.iter().enumerate() {
                        let cells: Vec<String> = r.iter().map(|c| md_cell(c)).collect();
                        out.push_str(&format!("|{}|\n", cells.join("|")));
                        if i == 0 {
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
            "doco:ListItem" => out.push_str(&format!("- {}\n", md_linked(e))),
            _ => out.push_str(&format!("{}\n\n", md_linked(e))),
        }
    }
    if let Some(note) = notes.summary() {
        out.push_str(&format!("\n<!-- {} -->\n", note.replace("--", "—")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::{Link, Target};

    fn para(text: &str, links: Vec<Link>) -> Element {
        Element {
            id: String::new(),
            kind: "doco:Paragraph".into(),
            page: 0,
            bbox: None,
            text: text.into(),
            level: None,
            cells: None,
            header_rows: None,
            sub_headers: None,
            merged_down: None,
            merged_left: None,
            figure: None,
            links: (!links.is_empty()).then_some(links),
            provenance: "rust",
            evidence: "layout",
        }
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
            running_text: Vec::new(),
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
            running_text: Vec::new(),
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
        };
        let s = n.summary().unwrap();
        assert!(s.contains("pages 1, 5, 7"), "{s}");
        assert!(s.contains("NearBlank, Scanned"), "{s}");
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
