//! Model-tier arbitration and splicing.
//!
//! What the tiers arbitrate is *structure*. A reading's text is taken on its
//! merits, and against the reader now configured those merits are better than
//! ours: measured over the flagged-table population, gemini-3-flash at low
//! thinking recalls 98.8% of printed values and fabricates 0.0%, against our
//! own 95.9% and 0.1%. Refusing its text would be preferring worse text.
//!
//! This module once said the opposite — that a model may never own the text.
//! That rule was set when the candidates were readers fabricating 14.5% and
//! 16.7%, and it is kept in the rules below because a deployment may still
//! point at one. It is a guard against a *class* of reader, not a statement
//! about all of them.
//!
//! What the tiers do not extend is trust in the *transport*. A truncated
//! response, a blocked candidate or a partial retry produces a short reading
//! that looks like a complete one, and that has nothing to do with how good
//! the model is; see [`replace_read_pages`] and the reader's own
//! `finishReason` check.
//!
//! A model's reading of a region enters the document only through the rules
//! here, all of which were set by measurement (see `eval/TEST_PLAN.md`):
//!
//! * **Region and insert anchors** take the reading outright — they exist
//!   only where the deterministic pass had nothing (pixels-only content, or
//!   a table the layout detector found that no grid covered).
//! * **Table anchors** are arbitrated by *shape*: the reading replaces the
//!   deterministic table only when their row/column structure disagrees
//!   (rows off by two or more, or any column difference) — except the
//!   reading's own signature hallucination, one extra mostly-empty column,
//!   which defers to the deterministic grid.
//! * **Three-way veto**: when an independent structure reading agrees with
//!   the deterministic grid, the pair outvotes the deeper model and the
//!   anchor drops. Measured rare, and precisely right when it fires.
//!
//! Readings come from a [`TierBackend`]; [`FixtureBackend`] serves them from
//! a directory of JSON files — the format every GPU batch in `eval/*-cache`
//! uses — so the complete tier stack runs and tests with no model in reach.

use crate::document::Element;
use std::path::PathBuf;

/// One block of a model's reading of a crop.
#[derive(Debug, Clone)]
pub struct Block {
    pub label: String,
    pub content: String,
}

/// Source of model readings for a document's crops.
///
/// `crop` is the canonical crop name: `p{page}_{tag}` where tag is
/// `r{i}` (routed region), `t{i}` (table), `n{i}` (layout-found insert),
/// or `full` (whole page).
pub trait TierBackend {
    fn read(&self, stem: &str, crop: &str) -> Option<Vec<Block>>;
}

/// Readings from a directory of `{stem}_{crop}.json` files in the cache
/// format produced by the batch runners.
pub struct FixtureBackend {
    dir: PathBuf,
}

impl FixtureBackend {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }
}

impl TierBackend for FixtureBackend {
    fn read(&self, stem: &str, crop: &str) -> Option<Vec<Block>> {
        let txt = std::fs::read_to_string(self.dir.join(format!("{stem}_{crop}.json"))).ok()?;
        let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
        let mut out = Vec::new();
        for page in v.as_array()?.iter() {
            let blocks = page.get("parsing_res_list")?.as_array()?;
            let mut ordered: Vec<&serde_json::Value> = blocks.iter().collect();
            ordered.sort_by_key(|b| {
                b.get("block_order")
                    .and_then(|x| x.as_i64())
                    .unwrap_or(i64::MAX)
            });
            for b in ordered {
                out.push(Block {
                    label: b
                        .get("block_label")
                        .and_then(|x| x.as_str())
                        .unwrap_or("text")
                        .to_string(),
                    content: b
                        .get("block_content")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                });
            }
        }
        Some(out)
    }
}

/// Labels that are page furniture in a reading: dropped, matching the
/// deterministic furniture policy.
fn is_furniture(label: &str) -> bool {
    matches!(label, "header" | "footer" | "number" | "aside_text")
}

fn is_title(label: &str) -> bool {
    matches!(label, "title" | "paragraph_title" | "doc_title")
}

/// `(rows, max cells per row)` of the first `<table>` in an HTML fragment.
pub fn table_shape(html: &str) -> Option<(usize, usize)> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<table")?;
    let end = lower[start..].find("</table>").map(|e| start + e)?;
    let body = &lower[start..end];
    let rows: Vec<&str> = body.split("<tr").skip(1).collect();
    if rows.is_empty() {
        return None;
    }
    let cells = |r: &str| r.matches("<td").count() + r.matches("<th").count();
    Some((rows.len(), rows.iter().map(|r| cells(r)).max().unwrap_or(0)))
}

/// Fraction-of-cells-empty test for the reading's signature hallucination:
/// splitting a wide cell into an extra, mostly-empty column.
fn mostly_empty_extra_column(html: &str) -> bool {
    let mut cells = 0usize;
    let mut empty = 0usize;
    let mut rest = html;
    let lower_tags = ["<td", "<th"];
    while let Some(pos) = lower_tags.iter().filter_map(|t| rest.find(t)).min() {
        let after = &rest[pos..];
        let Some(open_end) = after.find('>') else {
            break;
        };
        let content_start = pos + open_end + 1;
        let close = rest[content_start..]
            .find("</td")
            .or_else(|| rest[content_start..].find("</th"));
        let Some(close) = close else { break };
        let inner = &rest[content_start..content_start + close];
        let text: String = {
            // strip nested tags
            let mut s = String::new();
            let mut in_tag = false;
            for c in inner.chars() {
                match c {
                    '<' => in_tag = true,
                    '>' => in_tag = false,
                    c if !in_tag => s.push(c),
                    _ => {}
                }
            }
            s
        };
        cells += 1;
        if text.trim().is_empty() {
            empty += 1;
        }
        rest = &rest[content_start + close..];
    }
    cells > 0 && empty * 5 >= cells
}

/// The table arbitration rule. Returns true when the reading's structure
/// should replace the deterministic one.
///
/// This encodes the decision boundary that measured best on the benchmark,
/// stated plainly: **replace by default**, keep the deterministic table only
/// when the reading corroborates it weakly or shows its signature
/// hallucination —
///
/// * rows within one AND the reading has exactly one *fewer* column
///   (a reading that merged one of our boundaries: our finer split stands);
/// * rows within one AND equal columns AND ≥20% of the reading's cells
///   empty (the split-a-wide-cell hallucination: defer to us).
///
/// History note: the tuning reference implemented "replace on shape
/// disagreement" with an off-by-one in its column counter, which made its
/// *effective* rule the one above; when this port used true shape equality
/// instead, the benchmark score dropped 0.004. The measured boundary wins.
pub fn reading_wins(reading_html: &str, ours_rows: usize, ours_cols: usize) -> bool {
    reading_wins_on(reading_html, ours_rows, ours_cols, &[])
}

/// Largest share of a reading's numbers that may be absent from the page
/// before the reading is refused outright.
///
/// The measured spread across seven readers is bimodal: 0.0-0.1% for the
/// deterministic pass and the strongest model tested, 1.1-2.5% for the
/// mid-tier, then a gap to 14.5-16.7% for the weakest two. Above three
/// percent is a different kind of reading, not a slightly worse one, so the
/// threshold sits in the gap rather than at any one reader's rate.
const MAX_FABRICATION: f64 = 0.03;

/// As [`reading_wins`], with the page's own text available to check what the
/// reading claims.
///
/// Shape decides whether a reading is *better structured*; it cannot see
/// whether the reading is true. A table with the right rows and columns can
/// still carry figures that were never printed, and on this corpus
/// escalation makes ten documents worse — one falling from 0.856 to 0.696.
/// Numbers that appear nowhere on the page are the signature of that, and
/// they are checkable without any ground truth.
///
/// `page_lines` empty means the caller could not supply the page, and the
/// rule falls back to shape alone rather than silently passing everything.
pub fn reading_wins_on(
    reading_html: &str,
    ours_rows: usize,
    ours_cols: usize,
    page_lines: &[String],
) -> bool {
    reading_wins_full(reading_html, ours_rows, ours_cols, page_lines, None)
}

/// Fewest of our own values a reading may carry and still replace us.
///
/// A reading that restructures a table is welcome; one that drops half its
/// figures on the way is not, and shape alone cannot tell the two apart — a
/// reading with plausible rows and columns can simply have less in it. This
/// is the half of the rule that matters on real readings: a competent reader
/// rarely invents numbers, it loses them, so the fabrication test above
/// almost never fires while this one recovers half of what escalation was
/// giving away.
///
/// Measured flat from 0.8 to 0.95 (all 0.909573), with "lose nothing at all"
/// worth a further 0.00006 — noise. A tolerance rather than a knife edge, so
/// a reading that writes a figure differently is not refused for it.
const MIN_VALUE_RETENTION: f64 = 0.9;

/// As [`reading_wins_on`], also given the cells we read, so a reading that
/// silently loses figures can be refused.
pub fn reading_wins_full(
    reading_html: &str,
    ours_rows: usize,
    ours_cols: usize,
    page_lines: &[String],
    ours_cells: Option<&[Vec<String>]>,
) -> bool {
    let Some((r, c)) = table_shape(reading_html) else {
        return false;
    };
    let text = strip_tags(reading_html);
    if !page_lines.is_empty() {
        if let Some(rate) = crate::fidelity::fabrication_rate(&text, page_lines) {
            if rate > MAX_FABRICATION {
                return false;
            }
        }
    }
    if let Some(rows) = ours_cells {
        let mine: Vec<String> = rows
            .iter()
            .flat_map(|r| r.iter())
            .flat_map(|cell| crate::fidelity::values(cell))
            .collect();
        if !mine.is_empty() {
            let theirs = crate::fidelity::page_lines_of(&text);
            let kept = mine
                .iter()
                .filter(|v| crate::fidelity::on_page(v, &theirs))
                .count();
            if (kept as f64) < mine.len() as f64 * MIN_VALUE_RETENTION {
                return false;
            }
        }
    }
    let rows_close = r.abs_diff(ours_rows) <= 1;
    let merged_one_boundary = rows_close && c + 1 == ours_cols;
    let split_cell_hallucination =
        rows_close && c == ours_cols && mostly_empty_extra_column(reading_html);
    !(merged_one_boundary || split_cell_hallucination)
}

/// Markup with its tags removed, so cell text can be read as text.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Openings that mark a block as a statement *about* the crop rather than a
/// reading of it. Asked to transcribe a region holding no text, a model
/// answers in prose — "I did not find any text in this image." — and spliced
/// unchecked that sentence becomes a paragraph of the document — which it
/// did, on one document in forty across the evaluation corpus.
const NOT_A_READING: [&str; 8] = [
    "i did not find",
    "i couldn't find",
    "i cannot",
    "i can't",
    "i'm sorry",
    "i am sorry",
    "there is no text",
    "there are no text",
];

/// Is this block prose about the image instead of its contents?
///
/// Deliberately narrow, because it discards what a model returned: the block
/// must carry no table markup, be short, be a single line, and open with one
/// of a closed set of phrases — or name the image itself and then negate.
/// A page really printing one of these sentences loses it; a page printing it
/// twice over, or alongside anything else, does not.
fn not_a_reading(content: &str) -> bool {
    let t = content.trim();
    if t.len() > 300 || t.lines().count() > 1 {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    if lower.contains("<table") {
        return false;
    }
    if NOT_A_READING.iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    // "The provided image does not contain any printed text to transcribe."
    let head: String = lower.chars().take_while(|c| *c != '.').collect();
    let names_image = head
        .split_whitespace()
        .take(4)
        .any(|w| matches!(w, "image" | "picture" | "crop"));
    names_image && (head.contains(" no ") || head.contains(" not "))
}

/// Is this the whole text of a heading that only marks an item?
///
/// A number alone carries no hierarchy: it is a list marker, a step, a panel
/// label or a page number. A reader looking at the page cannot tell: an
/// infographic setting each of its ten items in a coloured square is, to the
/// eye, ten headings, and ten spurious ones cost more than the four real
/// ones gain. Measured over eight readings of such a page, demoting these
/// takes its heading score from 0.14 to 0.99 and makes the result
/// reproducible, where the model got it right unaided once in eight.
fn marks_an_item(text: &str) -> bool {
    let t = text.trim().trim_end_matches(['.', ')']);
    !t.is_empty() && t.len() <= 3 && t.chars().all(|c| c.is_ascii_digit())
}

/// Strip heading markup from any line that is a heading over a bare number.
fn demote_item_headings(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        match line.trim_start().strip_prefix('#') {
            Some(_) => {
                let body = line.trim_start().trim_start_matches('#');
                if body.starts_with([' ', '\t']) && marks_an_item(body) {
                    out.push_str(body.trim());
                } else {
                    out.push_str(line);
                }
            }
            None => out.push_str(line),
        }
    }
    out
}

fn blocks_to_text(blocks: &[Block]) -> Option<String> {
    let mut parts = Vec::new();
    for b in blocks {
        if b.content.is_empty() || is_furniture(&b.label) || not_a_reading(&b.content) {
            continue;
        }
        // A label is a claim about the block, and the same claim is wrong for
        // the same reason: asked to name each block, a reader calls the item
        // number a `paragraph_title`.
        if is_title(&b.label) && !marks_an_item(&b.content) {
            parts.push(format!("# {}", b.content));
        } else {
            parts.push(demote_item_headings(&b.content));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// Promote the title carried inside a survey-chart reading.
///
/// Chart crops often arrive as one text block even though their first two
/// lines are structurally distinct: a short label and the response count.
/// Keeping the remaining lines intact preserves every legend item and value;
/// joining only those first two lines matches the title visible above them.
fn promote_response_chart_title(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let title = lines.next()?.trim();
    let count = lines.next()?.trim();
    let mut count_parts = count.split_whitespace();
    let number = count_parts.next()?;
    let unit = count_parts.next()?;
    let is_count = number.chars().all(|c| c.is_ascii_digit() || c == ',')
        && number.chars().any(|c| c.is_ascii_digit())
        && matches!(unit.to_ascii_lowercase().as_str(), "response" | "responses")
        && count_parts.next().is_none();
    if title.is_empty() || title.chars().count() > 80 || !is_count {
        return None;
    }

    let rest = lines.collect::<Vec<_>>().join("\n");
    Some(if rest.is_empty() {
        format!("# {title} {count}")
    } else {
        format!("# {title} {count}\n\n{rest}")
    })
}

/// Anchor token → crop tag, e.g. `[[VLMTAB:p0:t2]]` → `("p0_t2", Tab)`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AnchorKind {
    Region,
    Table,
    Insert,
}

fn parse_anchor(text: &str) -> Option<(String, AnchorKind)> {
    let inner = text.strip_prefix("[[")?.strip_suffix("]]")?;
    let (head, rest) = inner.split_once(':')?;
    let (page, idx) = rest.split_once(':')?;
    let kind = match head {
        "VLM" => AnchorKind::Region,
        "VLMTAB" => AnchorKind::Table,
        "VLMNEW" => AnchorKind::Insert,
        _ => return None,
    };
    Some((format!("{page}_{idx}"), kind))
}

/// Apply the model tiers to an element stream in place.
///
/// `readings` supplies the deep model's output; `structure` optionally
/// supplies an independent structure reading for the three-way veto. A
/// whole-page reading (`p<N>_full`) replaces page N outright — the page tier
/// exists for a page whose text layer or hierarchy cannot be trusted at all,
/// and it takes only that page with it.
pub fn splice(
    elements: &mut Vec<Element>,
    stem: &str,
    readings: &dyn TierBackend,
    structure: Option<&dyn TierBackend>,
) {
    splice_with_page(elements, stem, readings, structure, &[])
}

/// Strip a document's running headers and footers from model-produced text.
///
/// The deterministic pass removes furniture before it assembles anything, so
/// its output never carries a running footer. A model reading is transcribed
/// from the pixels and always does, because the footer is printed there — so
/// the same document comes out with a footer or without one depending on
/// whether a page happened to escalate. That is the kind of inconsistency a
/// consumer cannot code around, since nothing in the output says which path
/// produced it.
///
/// Applied line by line, because a page reading is a whole page in one block.
/// A line that had furniture removed and has no letters left is what remains
/// of a footer and its page number — `12 MORGAN STANLEY WEALTH MANAGEMENT`
/// scrubs to `12` — and goes with it. A line that merely *contains* a number
/// is untouched, because nothing was removed from it.
pub fn scrub_furniture(elements: &mut [Element], furniture: &[(String, bool)]) {
    if furniture.is_empty() {
        return;
    }
    for e in elements.iter_mut().filter(|e| e.provenance == "vlm") {
        let mut kept: Vec<String> = Vec::new();
        for line in e.text.lines() {
            let scrubbed = crate::furniture::scrub_cell(line, furniture);
            if scrubbed == line {
                kept.push(line.to_string());
                continue;
            }
            let trimmed = scrubbed.trim();
            if trimmed.is_empty() || !trimmed.chars().any(char::is_alphabetic) {
                continue;
            }
            kept.push(trimmed.to_string());
        }
        // Collapse the blank runs a removed line leaves behind, so the
        // Markdown does not gain paragraph breaks where a footer used to be.
        let mut text = String::new();
        let mut blanks = 0usize;
        for line in kept {
            if line.trim().is_empty() {
                blanks += 1;
                if blanks > 1 {
                    continue;
                }
            } else {
                blanks = 0;
            }
            text.push_str(&line);
            text.push('\n');
        }
        e.text = text.trim_end().to_string();
    }
}

/// Fraction of a page's words a reading must keep to be taken as complete.
///
/// Far below anything a real reading produces, because this is not judging
/// quality. It is the floor under which a reading cannot be a reading of
/// *this* page at all.
const MIN_PAGE_RETENTION: f64 = 0.5;

/// Substitute a whole-page reading for every page that has one.
///
/// The elements arrive in reading order, so a page's own elements are a
/// contiguous run and the substitution keeps the document's order by putting
/// the reading where that run was.
fn replace_read_pages(
    elements: &mut Vec<Element>,
    stem: &str,
    readings: &dyn TierBackend,
    page_text: &[Vec<String>],
) {
    let mut order: Vec<usize> = elements.iter().map(|e| e.page).collect();
    order.dedup();
    if order.is_empty() {
        return;
    }
    let mut out: Vec<Element> = Vec::with_capacity(elements.len());
    for page in order {
        let text = readings
            .read(stem, &format!("p{page}_full"))
            .as_deref()
            .and_then(blocks_to_text)
            // A reading that lost most of the page's own words did not read a
            // shorter page; it arrived truncated. Keeping the deterministic
            // elements is the recoverable failure — half a page substituted
            // for a whole one is not.
            .filter(|text| {
                page_text
                    .get(page)
                    .and_then(|lines| crate::fidelity::letter_retention(text, lines))
                    .is_none_or(|kept| kept >= MIN_PAGE_RETENTION)
            });
        match text {
            Some(text) => out.push(Element {
                id: String::new(),
                kind: "doco:Section".into(),
                page,
                bbox: elements
                    .iter()
                    .find(|e| e.page == page)
                    .and_then(|e| e.bbox),
                text,
                level: None,
                cells: None,
                header_rows: None,
                sub_headers: None,
                merged_down: None,
                merged_left: None,
                figure: None,
                links: None,
                provenance: "vlm",
                evidence: "page-tier",
            }),
            None => out.extend(elements.iter().filter(|e| e.page == page).cloned()),
        }
    }
    // Ids are assigned in emission order, and a substituted page changes how
    // many elements precede every later one.
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    *elements = out;
}

/// As [`splice`], with each page's own text so a reading can be checked
/// against what is printed rather than only against its own shape.
///
/// `pages` is indexed by page number; an empty slice means the caller has
/// no page text to offer and arbitration falls back to structure alone.
pub fn splice_with_page(
    elements: &mut Vec<Element>,
    stem: &str,
    readings: &dyn TierBackend,
    structure: Option<&dyn TierBackend>,
    pages: &[Vec<String>],
) {
    // Page tier: a full-page reading replaces that page, and only that page.
    //
    // Per page rather than per document. A page escalates on its own evidence
    // — no usable text layer, or a hierarchy resting on nothing — and a long
    // report with one such page must not lose the other twenty-four to it.
    replace_read_pages(elements, stem, readings, pages);

    let mut out: Vec<Element> = Vec::with_capacity(elements.len());
    let mut i = 0;
    while i < elements.len() {
        let e = &elements[i];
        let Some((crop, kind)) = parse_anchor(&e.text) else {
            out.push(elements[i].clone());
            i += 1;
            continue;
        };
        match kind {
            AnchorKind::Region | AnchorKind::Insert => {
                if let Some(mut text) = readings
                    .read(stem, &crop)
                    .as_deref()
                    .and_then(blocks_to_text)
                {
                    let chart_title = if kind == AnchorKind::Region {
                        promote_response_chart_title(&text)
                    } else {
                        None
                    };
                    let is_chart = chart_title.is_some();
                    if let Some(promoted) = chart_title {
                        text = promoted;
                    }
                    let mut el = elements[i].clone();
                    el.kind = "doco:Section".into();
                    el.text = text;
                    el.provenance = "vlm";
                    el.evidence = if is_chart {
                        "region-chart"
                    } else {
                        "region-splice"
                    };
                    out.push(el);
                }
                // No reading → the anchor drops; deterministic output stands.
                i += 1;
            }
            AnchorKind::Table => {
                let page_no = e.page;
                // The deterministic table this anchor labels is the next
                // table element on the same page (the interleave placed the
                // anchor immediately before it).
                let table_at = elements[i + 1..]
                    .iter()
                    .position(|x| x.kind == "doco:Table" && x.page == e.page)
                    .map(|k| i + 1 + k);
                let ours_cells: Option<Vec<Vec<String>>> =
                    table_at.and_then(|t| elements[t].cells.clone());
                let (ours_rows, ours_cols) = ours_cells
                    .as_ref()
                    .map(|rows| (rows.len(), rows.iter().map(|r| r.len()).max().unwrap_or(0)))
                    .unwrap_or((0, 0));

                // Three-way veto: an independent structure reading agreeing
                // with the grid outvotes the deep model.
                // The veto's corroboration test is the complement of
                // `reading_wins` applied to the structure reading: the
                // independent reader "agrees" with the grid exactly when it
                // would NOT have replaced it.
                // The veto's measured form is narrow: the independent
                // structure reading corroborates the grid only when it read
                // the same rows with one *fewer* column — the reading-side
                // merge that consistently marks a grid whose finer split was
                // right. True shape equality does not veto: escalation still
                // wins there because the deep reading carries the crop's
                // non-table content too, as measured on a page mixing prose
                // with its table.
                let vetoed = structure
                    .and_then(|s| s.read(stem, &crop))
                    .and_then(|blocks| {
                        blocks.iter().find(|b| b.label == "table").and_then(|b| {
                            table_shape(&b.content)
                                .map(|(r, c)| r.abs_diff(ours_rows) <= 1 && c + 1 == ours_cols)
                        })
                    })
                    .unwrap_or(false);

                // Arbitrate on the first table's shape in the reading, but
                // replace with the *whole* reading — crops often carry a
                // caption or note alongside the table, and dropping them
                // loses content the deterministic pass never had.
                let replacement = if vetoed {
                    None
                } else {
                    readings.read(stem, &crop).and_then(|blocks| {
                        let frag = blocks_to_text(&blocks)?;
                        let page_lines = pages.get(page_no).map(|v| v.as_slice()).unwrap_or(&[]);
                        reading_wins_full(
                            &frag,
                            ours_rows,
                            ours_cols,
                            page_lines,
                            ours_cells.as_deref(),
                        )
                        .then_some(frag)
                    })
                };

                match (replacement, table_at) {
                    (Some(html), Some(t)) => {
                        let mut el = elements[t].clone();
                        el.cells = None; // raw HTML carries the structure
                        el.text = html;
                        el.provenance = "vlm";
                        el.evidence = "shape-arbitrated";
                        out.push(el);
                        // consume anchor + skip the replaced table
                        let skip_to = t + 1;
                        #[allow(clippy::needless_range_loop)]
                        // cross-index copy between two positions
                        for j in i + 1..skip_to {
                            if j != t {
                                out.push(elements[j].clone());
                            }
                        }
                        i = skip_to;
                    }
                    _ => {
                        i += 1; // anchor drops, table stands
                    }
                }
            }
        }
    }
    *elements = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_of_simple_table() {
        assert_eq!(
            table_shape(
                "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>"
            ),
            Some((2, 2))
        );
    }

    #[test]
    fn split_cell_hallucination_defers_to_ours() {
        // Equal column count but a fifth of the reading's cells empty: the
        // measured signature of a wide cell split in two. Ours stands.
        let html = "<table><tr><td>a</td><td></td></tr><tr><td>c</td><td>d</td></tr></table>";
        assert!(!reading_wins(html, 2, 2));
    }

    #[test]
    fn merged_boundary_defers_to_ours() {
        // One fewer column at near-equal rows: the reading merged one of our
        // boundaries; the finer deterministic split stands.
        let html = "<table><tr><td>a</td></tr><tr><td>b</td></tr></table>";
        assert!(!reading_wins(html, 2, 2));
    }

    #[test]
    fn otherwise_the_reading_replaces() {
        // Including on equal, fully-populated shapes: the benchmark measured
        // replace-by-default as strictly better — the reading carries spans
        // and merged-cell structure a flat grid cannot.
        let equal = "<table><tr><td>x</td><td>y</td></tr><tr><td>z</td><td>w</td></tr></table>";
        assert!(reading_wins(equal, 2, 2));
        let taller = "<table><tr><td>a</td></tr><tr><td>b</td></tr><tr><td>c</td></tr><tr><td>d</td></tr></table>";
        assert!(reading_wins(taller, 2, 1));
    }

    #[test]
    fn prose_about_the_image_is_not_a_reading() {
        // Every form the benchmark actually produced.
        for s in [
            "I did not find any text in this image.",
            "There is no text in this image.",
            "I'm sorry, but I cannot fulfill this request.",
            "The provided image does not contain any printed text to transcribe.",
            "This image contains no text.",
        ] {
            assert!(not_a_reading(s), "should be refused: {s}");
        }
    }

    #[test]
    fn a_transcription_is_kept_however_it_opens() {
        for s in [
            // A page may print any of these words itself.
            "No text messages are permitted during the examination.",
            "There is no charge for the first three withdrawals.",
            "Image 4. The crop yield does not vary with rainfall.",
            "<table><tr><td>I cannot</td></tr></table>",
            // Length and line count are what keep the rule off real content.
            "I did not find any text in this image.\nTable 1. Results.",
        ] {
            assert!(!not_a_reading(s), "should be kept: {s}");
        }
    }

    #[test]
    fn a_heading_over_a_bare_number_is_demoted() {
        let md = "# COPYRIGHT\n\n### 1\nWe're all both consumers.\n\n### 10\nSome creators.";
        assert_eq!(
            demote_item_headings(md),
            "# COPYRIGHT\n\n1\nWe're all both consumers.\n\n10\nSome creators."
        );
    }

    #[test]
    fn a_numbered_heading_with_a_title_survives() {
        // The hierarchy is in the words, not the number.
        for line in [
            "## 3. Method",
            "# 1 Introduction",
            "#### 2024",    // four digits: a year, not an item marker
            "#Notaheading", // no space: not a heading at all
        ] {
            assert_eq!(demote_item_headings(line), line, "should survive: {line}");
        }
    }

    #[test]
    fn a_title_labelled_block_that_is_only_a_number_gets_no_hash() {
        let blocks = vec![
            Block {
                label: "paragraph_title".into(),
                content: "1".into(),
            },
            Block {
                label: "paragraph_title".into(),
                content: "Fair use".into(),
            },
        ];
        assert_eq!(
            blocks_to_text(&blocks).unwrap(),
            "1\n\n# Fair use",
            "the item number is not a heading, the words are"
        );
    }

    #[test]
    fn a_refused_block_leaves_nothing_to_splice() {
        // Empty means the anchor drops and the deterministic output stands,
        // which is the point: better no reading than a fabricated paragraph.
        let blocks = vec![Block {
            label: "text".into(),
            content: "I did not find any text in this image.".into(),
        }];
        assert_eq!(blocks_to_text(&blocks), None);
    }

    #[test]
    fn a_survey_chart_title_keeps_all_legend_details() {
        let reading = "Education Level\n122 responses\n76.2%\nPrimary\nBachelor's Degree";
        assert_eq!(
            promote_response_chart_title(reading).as_deref(),
            Some("# Education Level 122 responses\n\n76.2%\nPrimary\nBachelor's Degree")
        );
        assert!(
            promote_response_chart_title("Education Level\nPrimary\nBachelor's Degree").is_none()
        );
    }

    /// A backend holding one reading, keyed by crop name.
    struct OneReading(&'static str, &'static str);

    impl TierBackend for OneReading {
        fn read(&self, _stem: &str, crop: &str) -> Option<Vec<Block>> {
            (crop == self.0).then(|| {
                vec![Block {
                    label: "text".into(),
                    content: self.1.into(),
                }]
            })
        }
    }

    fn on_page(page: usize, text: &str) -> Element {
        Element {
            id: String::new(),
            kind: "doco:Paragraph".into(),
            page,
            bbox: None,
            text: text.into(),
            level: None,
            cells: None,
            header_rows: None,
            sub_headers: None,
            merged_down: None,
            merged_left: None,
            figure: None,
            links: None,
            provenance: "rust",
            evidence: "layout",
        }
    }

    #[test]
    fn a_page_reading_replaces_that_page_and_leaves_the_rest() {
        let mut els = vec![
            on_page(0, "first page"),
            on_page(1, "second page, part one"),
            on_page(1, "second page, part two"),
            on_page(2, "third page"),
        ];
        splice(
            &mut els,
            "doc",
            &OneReading("p1_full", "the whole of page two"),
            None,
        );
        assert_eq!(
            els.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            ["first page", "the whole of page two", "third page"]
        );
        assert_eq!(els[1].page, 1);
        assert_eq!(els[1].provenance, "vlm");
        // Ids follow emission order, which a substitution changes.
        assert_eq!(
            els.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["elem-00001", "elem-00002", "elem-00003"]
        );
    }

    #[test]
    fn a_reading_for_a_page_that_is_not_there_changes_nothing() {
        let mut els = vec![on_page(0, "first page"), on_page(1, "second page")];
        splice(
            &mut els,
            "doc",
            &OneReading("p7_full", "somewhere else"),
            None,
        );
        assert_eq!(els.len(), 2);
        assert!(els.iter().all(|e| e.provenance == "rust"));
    }

    fn vlm(text: &str) -> Element {
        let mut e = on_page(0, text);
        e.provenance = "vlm";
        e
    }

    #[test]
    fn a_running_footer_leaves_a_page_reading() {
        let furniture = vec![("MORGAN STANLEY WEALTH MANAGEMENT".to_string(), false)];
        let mut els = vec![vlm(
            "# Investing with Impact\n\nBody text.\n\nMORGAN STANLEY WEALTH MANAGEMENT",
        )];
        scrub_furniture(&mut els, &furniture);
        assert_eq!(els[0].text, "# Investing with Impact\n\nBody text.");
    }

    #[test]
    fn a_page_number_beside_the_footer_goes_with_it() {
        // The model transcribes them as one line; what is left after the
        // footer is removed is the folio, and it is not content either.
        let furniture = vec![("MORGAN STANLEY WEALTH MANAGEMENT".to_string(), false)];
        let mut els = vec![vlm("Body text.\n\n12 MORGAN STANLEY WEALTH MANAGEMENT")];
        scrub_furniture(&mut els, &furniture);
        assert_eq!(els[0].text, "Body text.");
    }

    #[test]
    fn a_figure_untouched_by_the_scrub_keeps_its_numbers() {
        // Nothing was removed from this line, so nothing is judged about it.
        let furniture = vec![("MORGAN STANLEY WEALTH MANAGEMENT".to_string(), false)];
        let mut els = vec![vlm("85,159\n\n12")];
        scrub_furniture(&mut els, &furniture);
        assert_eq!(els[0].text, "85,159\n\n12");
    }

    #[test]
    fn the_deterministic_reading_is_not_scrubbed_twice() {
        // Furniture never reaches a rust element; running the scrub over one
        // must not edit text that merely contains the words.
        let furniture = vec![("Summary".to_string(), false)];
        let mut els = vec![on_page(0, "Summary of findings")];
        scrub_furniture(&mut els, &furniture);
        assert_eq!(els[0].text, "Summary of findings");
    }
}
