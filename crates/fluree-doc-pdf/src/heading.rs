//! Heading detection and levelling.
//!
//! Three independent signals, combined rather than ranked:
//!
//! 1. **The PDF outline tree** ([`crate::outline`]) — author-provided and
//!    explicitly hierarchical. Where a block matches an outline title, its
//!    level is taken directly. This is the signal no benchmarked engine uses.
//! 2. **Numbering** — `5.3.1` states its own depth. Independent of typography,
//!    so it works where font sizes are uniform.
//! 3. **Font size relative to body** — the classic signal, and the only one
//!    available on documents with neither outline nor numbering. Plenty of
//!    real reports carry no outline at all, so this path cannot be dropped.
//!
//! Deliberately *not* a weighted score summed to a threshold. Each signal is
//! strong enough alone to be worth acting on, and a sum lets two weak signals
//! outvote one authoritative one — the outline should not be overridden by a
//! font being 0.5pt large.

use crate::block::{self, Block};
use crate::outline::OutlineItem;
use std::collections::HashMap;

/// A block must exceed the body font size by this fraction to be a heading on
/// typography alone.
///
/// Typography is the *weakest* of the three signals and needs the tightest
/// guards. On one datasheet the modal size is 8pt (dense table text) while
/// body prose runs 9-10pt, so a bare "larger than modal" test promoted 320
/// paragraph fragments to headings. Font size only counts alongside the length
/// and line-count limits below.
const HEADING_SIZE_MARGIN: f64 = 0.15;

/// Shortest word that can end a sentence inside a heading candidate. Below this
/// the dot is abbreviating (`vs.`, `Sec.`, `Fig.`), not terminating.
const MIN_SENTENCE_WORD_CHARS: usize = 4;

/// Most bold heading candidates a page may hold before bold is judged to be
/// doing some other job on this document. Sections are coarse: a page with more
/// than a handful of them is a page of labels.
const MAX_BOLD_HEADINGS_PER_PAGE: usize = 10;

/// Headings are short. Applies to every signal.
const MAX_HEADING_CHARS: usize = 200;

/// Headings are short in *words*, not only characters.
const MAX_HEADING_WORDS: usize = 12;

/// Tighter limits for the typography-only path, where there is no outline or
/// numbering to corroborate.
const MAX_FONTSIZE_HEADING_CHARS: usize = 90;
const MAX_FONTSIZE_HEADING_LINES: usize = 2;

/// Deepest level we emit, matching `h1`-`h6`.
const MAX_LEVEL: usize = 6;

/// Largest value a single section number may take. Section numbering is small;
/// a bare four-digit number opening a sentence is a year or a quantity, and
/// "2026 was a difficult year…" must not become a level-1 heading.
const MAX_SECTION_NUMBER: u32 = 99;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// Matched a PDF outline entry — the strongest signal available.
    Outline,
    /// Prominent, isolated text near the start of the first page.
    Title,
    /// Section numbering stated the depth.
    Numbering,
    /// A display label immediately precedes a lettered structural item.
    Sequence,
    /// Set larger than body text.
    FontSize,
    /// Bold at body size — the most common heading cue after size.
    Bold,
}

#[derive(Debug, Clone)]
pub struct Heading {
    pub page: usize,
    /// Index of the source block within its page.
    ///
    /// Text is not an identity: a chart or form can repeat the same label
    /// several times on one page, and only one occurrence may be a heading.
    /// Keeping the block address prevents the emission pass from promoting
    /// every duplicate because one copy matched.
    pub block_index: usize,
    pub text: String,
    pub level: usize,
    pub evidence: Evidence,
    style: Style,
}

/// Normalize for matching outline titles against block text: outlines often
/// differ from the rendered heading in whitespace and case, and sometimes omit
/// the section number.
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Parse a leading section number and return its depth: `5.` → 1,
/// `5.3` → 2, `6.3.1` → 3. Returns `None` when the block does not start with
/// one, so ordinary prose beginning with a figure is not mistaken for a heading.
fn numbering_depth(text: &str) -> Option<usize> {
    // Separator between the label and the title. `01 - Find Open Educational
    // Resources` and `02- Prepare Your Content` both occur in the corpus and
    // neither was parsed when only whitespace was accepted.
    let split_label = |t: &str| -> Option<(String, String)> {
        let head: String = t
            .chars()
            .take_while(|c| c.is_ascii_digit() || c.is_ascii_alphabetic() || *c == '.')
            .collect();
        if head.is_empty() {
            return None;
        }
        let rest =
            t[head.len()..].trim_start_matches([' ', '\t', '-', '\u{2013}', '\u{2014}', ')']);
        let rest = rest.trim_start();
        Some((head, rest.to_string()))
    };

    let (head, rest) = split_label(text)?;
    if rest.is_empty() || !rest.chars().next().is_some_and(|c| c.is_alphabetic()) {
        return None;
    }
    // Section labels introduce titles, not sentence continuations. Applying
    // this to every numbering form (not only bare integers) rejects OCR
    // fragments such as `III. his report defines` and chart prose such as
    // `2.6 times that of`.
    if !rest
        .chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(char::is_uppercase)
    {
        return None;
    }
    // Long numbered lines ending in a full stop are overwhelmingly citations,
    // instructions, or prose (`6. Compared to 38% ...`). Genuine numbered
    // headings in the measured corpus may contain abbreviations, but do not
    // end as complete multi-word sentences.
    if rest.ends_with('.') && rest.split_whitespace().count() >= 4 {
        return None;
    }
    let parts: Vec<&str> = head
        .trim_end_matches('.')
        .split('.')
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() || parts.len() > MAX_LEVEL {
        return None;
    }

    // Each component is a decimal section number, a roman numeral, or a single
    // letter (appendix style: `B.1 Large Language Models`). Mixed forms are
    // fine — `B.1` is a letter then a number.
    let mut saw_alpha = false;
    for p in &parts {
        if let Ok(v) = p.parse::<u32>() {
            if v > MAX_SECTION_NUMBER {
                return None;
            }
            continue;
        }
        if is_roman(p) || (p.len() == 1 && p.chars().all(|c| c.is_ascii_uppercase())) {
            saw_alpha = true;
            continue;
        }
        return None;
    }

    // A single bare number is ambiguous: "7 Variants of SJ Observer Models" is
    // a section heading, "3 of the respondents said otherwise" is prose. The
    // discriminator is what follows — a heading continues in title case, prose
    // in lower case.
    if parts.len() == 1 && !head.contains('.') && !saw_alpha {
        let first_word = rest.split_whitespace().next().unwrap_or("");
        if !first_word
            .chars()
            .find(|c| c.is_alphabetic())
            .is_some_and(|c| c.is_uppercase())
        {
            return None;
        }
    }
    // A bare single letter with no dot is far too weak — "A test of the system"
    // would qualify. Require the dot form for lettered sections.
    if parts.len() == 1 && saw_alpha && !head.contains('.') {
        return None;
    }
    Some(parts.len())
}

/// Uppercase roman numeral, used for appendix and front-matter sections
/// (`II. Set Up the Restriction Digests`).
fn is_roman(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 5
        && s.chars()
            .all(|c| matches!(c, 'I' | 'V' | 'X' | 'L' | 'C' | 'D' | 'M'))
}

/// A bare integer (`5 Results`) has less syntactic authority than `5. Results`.
/// Corroborate it with body-sized typography and title-bearing words. This
/// excludes footnote/page labels and mathematical/chart fragments while
/// retaining body-sized section labels used by papers and datasheets.
fn bare_numbering_is_corroborated(text: &str, block: &Block, body_size: f32) -> bool {
    let mut words = text.split_whitespace();
    let Some(label) = words.next() else {
        return false;
    };
    if !label.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if block.font_size < body_size * 0.95 {
        return false;
    }
    let rest: Vec<&str> = words.collect();
    if rest
        .first()
        .is_some_and(|word| word.chars().any(|c| c.is_ascii_digit()))
    {
        return false;
    }
    rest.iter().any(|word| {
        word.chars()
            .fold((0usize, 0usize), |(run, longest), c| {
                let run = if c.is_alphabetic() { run + 1 } else { 0 };
                (run, longest.max(run))
            })
            .1
            >= 2
    })
}

/// Shape tests every heading candidate must pass, regardless of which signal
/// proposed it.
///
/// Measured precision per source before adding this was FontSize 33.8%,
/// Numbering 33.7%, Bold 51.2% — we emitted 287 headings where the corpus
/// has 193.
///
/// Two commonly used title-shape tests are deliberately *omitted*, because
/// measuring them against the corpus showed they reject real headings:
/// requiring title-case capitalisation loses 12%, and rejecting a trailing full
/// stop loses 9% (`CHAPTER 1.`). The heading metric penalises a missed heading
/// — two sections merge — far more than a spurious one, which merely splits a
/// section.
fn title_like(text: &str) -> bool {
    let t = text.trim();
    let words = t.split_whitespace().count();
    if !(1..=MAX_HEADING_WORDS).contains(&words) {
        return false;
    }
    if !(4..=MAX_HEADING_CHARS).contains(&t.chars().count()) {
        return false;
    }
    if !t.chars().any(|c| c.is_alphabetic()) {
        return false;
    }
    // Mid-sentence punctuation means the line continues: prose, not a title.
    if t.ends_with([',', ';']) {
        return false;
    }
    // Dot leaders: a table-of-contents entry.
    if t.contains("...") || t.contains(". . .") {
        return false;
    }
    if runs_into_prose(t) {
        return false;
    }
    !starts_like_list_item(t)
}

/// A bold run-in lead-in, not a heading: `**Filtered task names.** We present
/// task names…`.
///
/// LaTeX's `\paragraph{}` sets its argument bold on the same line as the text it
/// introduces, so the block reads as one bold phrase followed by prose. Weight
/// and length cannot tell it from a section title — but a title is a single
/// phrase, while this carries a sentence boundary with a new sentence after it.
///
/// Requires a following *word*, not just a capital: `Sec. 3` and `Fig. 1 Results`
/// keep their abbreviating dot, and a title ending in a full stop (`CHAPTER 1.`)
/// has nothing after it and is untouched.
fn runs_into_prose(text: &str) -> bool {
    let bytes = text.as_bytes();
    for (i, w) in bytes.windows(2).enumerate() {
        if !matches!(w[0], b'.' | b'?' | b'!') || w[1] != b' ' {
            continue;
        }
        // The word carrying the dot decides. Abbreviations are short — `vs.`,
        // `Sec.`, `Fig.`, `et al.`, `No.` — and a heading may contain any of
        // them without ending a sentence; ordinary words are longer.
        let head = &text[..=i];
        let last = head.split_whitespace().next_back().unwrap_or("");
        if last.trim_end_matches(['.', '?', '!']).chars().count() < MIN_SENTENCE_WORD_CHARS {
            continue;
        }
        // Require real text on both sides, so a title ending in a full stop
        // (`CHAPTER 1.`) and a trailing fragment are both left alone.
        if head.split_whitespace().count() >= 2 && text[i + 2..].split_whitespace().count() >= 2 {
            return true;
        }
    }
    false
}

/// `Figure 2.1: Surveyed MSMEs…`, `Table 3 — results`, `Diagram 4 …`: a
/// caption. The label word plus a number is the test; a heading that merely
/// *mentions* a figure mid-phrase does not start with the pattern.
///
/// **Not wired into heading detection.** Measured twice (all evidence paths,
/// then bold-only): −0.0004 overall both times, because this corpus' ground
/// truth blesses prominent captions as headings often enough that MHS's
/// recall bias punishes the exclusion. Kept for the DoCO emitter, where
/// doco:Caption is simply the truth regardless of the benchmark's taste.
#[allow(dead_code)]
fn is_caption(text: &str) -> bool {
    let mut words = text.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    let label = matches!(
        first
            .trim_end_matches(['.', ':'])
            .to_ascii_lowercase()
            .as_str(),
        "figure"
            | "fig"
            | "table"
            | "chart"
            | "diagram"
            | "exhibit"
            | "graph"
            | "map"
            | "box"
            | "plate"
    );
    if !label {
        return false;
    }
    // The next token is a number or dotted number ("2.1", "3", "B.2").
    words.next().is_some_and(|w| {
        let w = w.trim_end_matches([':', '.', ',']);
        !w.is_empty()
            && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
            && w.chars().any(|c| c.is_ascii_digit())
    })
}

/// A block that announces a table of contents.
///
/// Everything after it on the same page is a ToC entry, not a heading. A
/// contents page's ground truth has exactly one heading, `Contents`, while
/// treating each entry as its own heading emits one per numbered line
/// beneath it.
pub(crate) fn is_toc_marker(text: &str) -> bool {
    let t = text.trim().trim_end_matches(':').to_ascii_lowercase();
    matches!(
        t.as_str(),
        "contents" | "table of contents" | "index" | "toc"
    )
}

/// A bullet or dash opening the block marks a list item, not a heading.
/// Datasheet feature lists are set larger than the surrounding table text and
/// would otherwise all qualify on size alone.
fn starts_like_list_item(text: &str) -> bool {
    matches!(
        text.chars().next(),
        Some('\u{2022}' | '\u{25A0}' | '\u{25CF}' | '-' | '\u{2013}' | '\u{2014}')
    )
}

fn style_of(b: &Block) -> Style {
    let caps = {
        let t = b.text();
        let letters: Vec<char> = t.chars().filter(|c| c.is_alphabetic()).collect();
        !letters.is_empty() && letters.iter().all(|c| c.is_uppercase())
    };
    Style {
        neg_size: -((b.font_size * 4.0).round() as i32),
        not_bold: !b.bold,
        not_caps: !caps,
    }
}

/// Modal font size across blocks, weighted by line count — the body size.
/// Weighting by lines rather than blocks stops a page of one-line captions
/// from redefining what "body" means.
pub fn body_font_size(pages: &[Vec<Block>]) -> f32 {
    let mut hist: HashMap<i32, usize> = HashMap::new();
    for blocks in pages {
        for b in blocks {
            *hist.entry((b.font_size * 4.0).round() as i32).or_default() += b.lines.len();
        }
    }
    hist.iter()
        .max_by(|(size_a, count_a), (size_b, count_b)| {
            count_a
                .cmp(count_b)
                // Equal-frequency sizes are deterministic: prefer the
                // smaller face, which is less likely to be a display title.
                .then_with(|| size_b.cmp(size_a))
        })
        .map(|(k, _)| *k as f32 / 4.0)
        .unwrap_or(10.0)
}

/// Detect headings across a document.
pub fn detect(pages: &[Vec<Block>], outline: &[OutlineItem]) -> Vec<Heading> {
    let body = body_font_size(pages);
    let document_title = pages.first().and_then(|blocks| {
        let largest = blocks.iter().map(|b| b.font_size).fold(0.0_f32, f32::max);
        blocks
            .iter()
            .take(4)
            // Once ordinary prose starts, a later isolated equation lead-in
            // is not the document title.
            .take_while(|b| b.text().split_whitespace().count() <= MAX_HEADING_WORDS)
            .enumerate()
            .find_map(|(i, b)| {
                let text = b.text();
                if b.marker.is_some() || b.lines.len() > 3 || !title_like(&text) {
                    return None;
                }
                let gap_after = blocks
                    .get(i + 1)
                    .map(|next| next.bbox.y0 - b.bbox.y1)
                    .unwrap_or(0.0);
                let prominent_size = b.font_size >= body * 1.05;
                let isolated_largest =
                    b.font_size >= largest * 0.98 && gap_after >= b.font_size as f64 * 0.75;
                let normalized = text.trim().trim_end_matches(':').to_lowercase();
                let contents_title =
                    matches!(normalized.as_str(), "contents" | "table of contents");
                (prominent_size || isolated_largest || contents_title).then_some(i)
            })
    });

    // Outline titles by normalized text. Duplicate titles ("Overview" under
    // several parents) keep the shallowest level: promoting is safer than
    // burying a real section.
    let mut by_title: HashMap<String, usize> = HashMap::new();
    for it in outline {
        let k = norm(&it.title);
        if k.is_empty() {
            continue;
        }
        by_title
            .entry(k)
            .and_modify(|l| *l = (*l).min(it.level))
            .or_insert(it.level);
    }

    // Distinct heading-sized fonts, largest first, for the typography fallback.
    let mut sizes: Vec<i32> = pages
        .iter()
        .flatten()
        .filter(|b| b.font_size as f64 > body as f64 * (1.0 + HEADING_SIZE_MARGIN))
        .map(|b| (b.font_size * 4.0).round() as i32)
        .collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    sizes.dedup();
    let size_level: HashMap<i32, usize> = sizes
        .iter()
        .enumerate()
        .map(|(i, s)| (*s, (i + 1).min(MAX_LEVEL)))
        .collect();

    // Bold marks section titles only where it is used sparingly. On pages that
    // set chart legends, axis labels or table headers bold, the same test
    // proposes a dozen "headings" per page — one page's chart labels alone
    // yield `October 2020`, `Oct 2020`, `Don't know`. No page carries that
    // many sections, so past a
    // plausible density the signal is measuring something else and is dropped
    // for the document. Counted before emission because one page's misuse
    // discredits the cue everywhere in the document, not just on that page.
    let bold_candidates = pages
        .iter()
        .flatten()
        .filter(|b| {
            let t = b.text();
            b.bold
                && b.lines.len() <= MAX_FONTSIZE_HEADING_LINES
                && t.chars().count() <= MAX_FONTSIZE_HEADING_CHARS
                && title_like(&t)
        })
        .count();
    let bold_is_structural = bold_candidates <= MAX_BOLD_HEADINGS_PER_PAGE * pages.len().max(1);

    // If most body text is already bold, weight carries no information.
    let bold_lines: usize = pages
        .iter()
        .flatten()
        .filter(|b| b.bold)
        .map(|b| b.lines.len())
        .sum();
    let all_lines: usize = pages.iter().flatten().map(|b| b.lines.len()).sum();
    let body_is_bold = all_lines > 0 && bold_lines * 2 > all_lines;

    let mut out = Vec::new();
    for (page_idx, blocks) in pages.iter().enumerate() {
        // Suppress headings after a contents marker on the same page: the
        // entries below it are the table of contents, not sections.
        let toc_from = blocks.iter().position(|b| is_toc_marker(&b.text()));
        for (block_idx, b) in blocks.iter().enumerate() {
            if toc_from.is_some_and(|i| block_idx > i) {
                continue;
            }
            let text = b.text();
            let n_chars = text.chars().count();
            if text.trim().is_empty() || n_chars > MAX_HEADING_CHARS {
                continue;
            }

            // A document title is often a one-off style, so it cannot form a
            // repeated font-size tier. Recover it before the generic detectors.
            if page_idx == 0 && document_title == Some(block_idx) {
                out.push(Heading {
                    page: b.page,
                    block_index: block_idx,
                    text,
                    level: 1,
                    evidence: Evidence::Title,
                    style: style_of(b),
                });
                continue;
            }

            // 1. Outline match wins outright — the author named this heading,
            // so the shape tests do not apply.
            if let Some(&level) = by_title.get(&norm(&text)) {
                out.push(Heading {
                    page: b.page,
                    block_index: block_idx,
                    text,
                    level,
                    evidence: Evidence::Outline,
                    style: style_of(b),
                });
                continue;
            }
            if !title_like(&text) {
                continue;
            }
            // 2. A display word followed by a lettered item is a structural
            // section label even when it shares the body face and size.
            if blocks
                .get(block_idx + 1)
                .is_some_and(|next| block::display_before_lettered_item(&text, &next.text()))
            {
                out.push(Heading {
                    page: b.page,
                    block_index: block_idx,
                    text,
                    level: 1,
                    evidence: Evidence::Sequence,
                    style: style_of(b),
                });
                continue;
            }
            // 3. Numbering states its own depth.
            if let Some(depth) = numbering_depth(&text) {
                if !bare_numbering_is_corroborated(&text, b, body) {
                    continue;
                }
                out.push(Heading {
                    page: b.page,
                    block_index: block_idx,
                    text,
                    level: depth,
                    evidence: Evidence::Numbering,
                    style: style_of(b),
                });
                continue;
            }
            // 4. Bold, at body size. Documents very commonly set section
            // titles in the body face at the body size, differing only in
            // weight, which no size-based test can see.
            if b.bold
                && !body_is_bold
                && bold_is_structural
                && n_chars <= MAX_FONTSIZE_HEADING_CHARS
                && b.lines.len() <= MAX_FONTSIZE_HEADING_LINES
                && !starts_like_list_item(&text)
            {
                out.push(Heading {
                    page: b.page,
                    block_index: block_idx,
                    text,
                    level: 3,
                    evidence: Evidence::Bold,
                    style: style_of(b),
                });
                continue;
            }
            // 5. Typography, for documents with neither. Uncorroborated, so
            // it must also look like a heading: short, and not a paragraph.
            if n_chars > MAX_FONTSIZE_HEADING_CHARS
                || b.lines.len() > MAX_FONTSIZE_HEADING_LINES
                || starts_like_list_item(&text)
            {
                continue;
            }
            let key = (b.font_size * 4.0).round() as i32;
            if let Some(&level) = size_level.get(&key) {
                out.push(Heading {
                    page: b.page,
                    block_index: block_idx,
                    text,
                    level,
                    evidence: Evidence::FontSize,
                    style: style_of(b),
                });
                continue;
            }
            // A colon-lead detector ("As a boater:") was tried here in three
            // forms — ungated, gated on a following marked list, gated on
            // inline bullets. Ungated it recovered the poster documents but
            // fired on every "For example:" in the corpus (MHS −0.003 net);
            // gated it recovered nothing because poster block order does not
            // put the introduced list adjacent. Rejected; see MHS_ANALYSIS.
        }
    }
    normalize_levels(&mut out);
    out
}

/// Visual style of a heading, for ranking depth.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
struct Style {
    /// Quarter-point font size, negated so larger sorts first.
    neg_size: i32,
    /// Bold before regular at the same size.
    not_bold: bool,
    /// All-caps before mixed case at the same size and weight.
    not_caps: bool,
}

/// Assign levels from the document's own heading styles.
///
/// Each detector previously supplied its own level on its own scale — font size
/// ranked by size, numbering used its depth, caps and bold returned fixed
/// constants — so a document mixing signals got incoherent depths. Measured
/// against ground truth, only 38.7% of correctly-*found* headings had the right
/// level.
///
/// Instead, rank the distinct styles actually used for headings in this
/// document and assign 1..n. Numbering is authoritative where present, since it
/// states the depth explicitly; the outline likewise. Everything else is
/// levelled by style rank.
fn normalize_levels(hs: &mut [Heading]) {
    let mut styles: Vec<Style> = hs
        .iter()
        .filter(|h| {
            !matches!(
                h.evidence,
                Evidence::Outline | Evidence::Numbering | Evidence::Sequence
            )
        })
        .map(|h| h.style)
        .collect();
    styles.sort();
    styles.dedup();

    for h in hs.iter_mut() {
        if matches!(
            h.evidence,
            Evidence::Outline | Evidence::Title | Evidence::Numbering | Evidence::Sequence
        ) {
            continue;
        }
        if let Some(rank) = styles.iter().position(|s| *s == h.style) {
            h.level = (rank + 1).min(MAX_LEVEL);
        }
    }
}

/// Headings per element above which a document's hierarchy is doubtful.
///
/// A document is mostly prose; when a quarter of what it emits claims to be a
/// heading, the detector is promoting body text. Measured over the 105
/// benchmark documents that have both headings and a hierarchy score, mean
/// heading score falls monotonically with this ratio -- 0.890 below 0.1,
/// 0.798 to 0.2, 0.773 to 0.3, 0.732 to 0.5, and 0.505 above it. Set at 0.4
/// the signal fires on 7 documents, 4 of them scoring under 0.6, which is
/// 2.6x the base rate; their mean hierarchy score is 0.481 against 0.822
/// across the corpus.
///
/// Deliberately narrow. At 0.2 it fires on 41 documents for 1.34x, which is
/// barely better than guessing and would escalate a third of everything.
const DOUBTFUL_DENSITY: f64 = 0.4;

/// A page whose heading hierarchy rests on weak evidence.
///
/// The two existing triggers ask whether the *text* could be read -- an
/// unreadable page, a table disagreeing with itself. Neither fires on a page
/// that extracts perfectly and is organised wrongly, which is where the
/// remaining loss sits: of the documents that escalate nothing, the ones
/// scoring worst read their text well and their hierarchy badly.
///
/// Measured **per page**, not per document, because the ratio does not
/// survive averaging. A deck whose stat pages are four fifths headings sits
/// at 0.36 across twenty-eight pages and never fires, while six of those
/// pages exceed the threshold alone -- one of them at 0.80. The evaluation
/// corpus hid this: every document in it is a single page, so the two
/// measures were the same number and the weaker one was chosen by accident.
#[derive(Debug, Clone, PartialEq)]
pub struct Doubt {
    /// The page this doubt is about.
    pub page: usize,
    pub titles: usize,
    pub elements: usize,
    /// Headings as a fraction of all elements.
    pub density: f64,
    /// Headings resting on an outline entry or a numbering pattern rather
    /// than on relative font size, which is a guess about intent.
    pub corroborated: usize,
}

/// Report a doubtful hierarchy, or `None` when it looks sound.
///
/// Reported, never acted on here -- the same contract as
/// [`crate::table::suspect_tables`].
pub fn doubt(kinds: &[(&str, &str)]) -> Option<Doubt> {
    doubt_on_page(0, kinds)
}

/// As [`doubt`], recording which page the ratio was measured over.
pub fn doubt_on_page(page: usize, kinds: &[(&str, &str)]) -> Option<Doubt> {
    // Splice anchors are addresses, not content. Counting them changes the
    // ratio according to whether the caller happens to have asked for them,
    // so two callers looking at the same document disagreed about whether to
    // doubt it — and the one that routes escalation was the one that had
    // them.
    let kinds: Vec<(&str, &str)> = kinds
        .iter()
        .filter(|(_, e)| !matches!(*e, "route" | "table-confidence" | "table-missing"))
        .copied()
        .collect();
    let elements = kinds.len();
    if elements < 4 {
        // Too little to judge: a two-element page of one title and one
        // paragraph is 50% headings and perfectly correct.
        return None;
    }
    let titles: Vec<&(&str, &str)> = kinds
        .iter()
        .filter(|(k, _)| *k == "doco:SectionTitle")
        .collect();
    if titles.is_empty() {
        return None;
    }
    let density = titles.len() as f64 / elements as f64;
    if density <= DOUBTFUL_DENSITY {
        return None;
    }
    let corroborated = titles
        .iter()
        .filter(|(_, e)| matches!(*e, "outline" | "numbering" | "sequence"))
        .count();
    Some(Doubt {
        page,
        titles: titles.len(),
        elements,
        density,
        corroborated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::BBox;
    use crate::line::Line;

    fn blk(text: &str, fs: f32, lines: usize) -> Block {
        let l = Line {
            text: text.into(),
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 100.0,
                y1: fs as f64,
            },
            page: 0,
            rotation_bucket: 0,
            glyphs: vec![],
            font_size: fs,
            bold: false,
        };
        Block {
            lines: vec![l; lines.max(1)],
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 100.0,
                y1: fs as f64,
            },
            page: 0,
            font_size: fs,
            bold: false,
            gap_above: None,
            marker: None,
        }
    }

    #[test]
    fn numbering_depth_reads_section_numbers() {
        assert_eq!(numbering_depth("5. Specifications"), Some(1));
        assert_eq!(
            numbering_depth("5.3 Recommended Operating Conditions"),
            Some(2)
        );
        assert_eq!(numbering_depth("6.3.1 Oscillator Frequency"), Some(3));
    }

    #[test]
    fn numbering_reads_separators_letters_and_romans() {
        // Digit with a dash separator.
        assert_eq!(
            numbering_depth("01 - Find Open Educational Resources"),
            Some(1)
        );
        assert_eq!(numbering_depth("02- Prepare Your Content"), Some(1));
        // Appendix-style lettered sections.
        assert_eq!(numbering_depth("B.1 Large Language Models"), Some(2));
        assert_eq!(numbering_depth("B. Related Works"), Some(1));
        // Roman numerals.
        assert_eq!(
            numbering_depth("II. Set Up the Restriction Digests"),
            Some(1)
        );
        // A bare capital with no dot is too weak to be a section label.
        assert_eq!(numbering_depth("A test of the system"), None);
    }

    #[test]
    fn numbering_ignores_prose_starting_with_a_number() {
        // A year or quantity opening a sentence must not become a heading.
        assert_eq!(
            numbering_depth("2026 was a difficult year for the market"),
            None
        );
        assert_eq!(numbering_depth("15% of respondents"), None);
        assert_eq!(numbering_depth("plain prose"), None);
    }

    #[test]
    fn numbering_rejects_sentences_and_lowercase_continuations() {
        assert_eq!(numbering_depth("III. his report defines"), None);
        assert_eq!(numbering_depth("2.6 times that of"), None);
        assert_eq!(
            numbering_depth("6. Compared to 38% in July 2020 and 22% in October 2020."),
            None
        );
    }

    #[test]
    fn bare_numbering_needs_typographic_and_lexical_corroboration() {
        assert!(bare_numbering_is_corroborated(
            "7 Variants of Observer Models",
            &blk("7 Variants of Observer Models", 10.0, 1),
            10.0
        ));
        assert!(!bare_numbering_is_corroborated(
            "68 APPLIED FLUID MECHANICS LAB MANUAL",
            &blk("68 APPLIED FLUID MECHANICS LAB MANUAL", 7.0, 1),
            10.0
        ));
        assert!(!bare_numbering_is_corroborated(
            "1 OCR-F1 92.",
            &blk("1 OCR-F1 92.", 12.0, 1),
            10.0
        ));
        assert!(!bare_numbering_is_corroborated(
            "2 Q(h) − Q(2h)",
            &blk("2 Q(h) − Q(2h)", 10.0, 1),
            10.0
        ));
    }

    #[test]
    fn display_word_before_a_lettered_item_is_a_heading() {
        let pages = vec![vec![
            blk("Replace", 10.0, 1),
            blk("l. Replace Plastics with Recyclable Materials.", 10.0, 2),
            blk("ordinary body prose", 10.0, 20),
        ]];
        let headings = detect(&pages, &[]);
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].text, "Replace");
        assert_eq!(headings[0].evidence, Evidence::Sequence);
    }

    #[test]
    fn bare_single_number_needs_a_trailing_dot() {
        // "5 Specifications" is real in component datasheets, but indistinguishable
        // from prose without more context, so the outline carries those. A
        // trailing dot is unambiguous.
        // A bare number followed by title case is a section heading...
        assert_eq!(numbering_depth("5 Specifications"), Some(1));
        assert_eq!(numbering_depth("7 Variants of SJ Observer Models"), Some(1));
        assert_eq!(numbering_depth("5. Specifications"), Some(1));
        // ...followed by lower case it is prose.
        assert_eq!(numbering_depth("3 of the respondents said otherwise"), None);
    }

    #[test]
    fn outline_match_beats_typography() {
        // Body-sized text that the outline names is still a heading, and takes
        // the outline's level rather than a font-derived one.
        let pages = vec![vec![
            blk("Introduction", 10.0, 1),
            blk("body text here", 10.0, 8),
        ]];
        let outline = vec![OutlineItem {
            title: "Introduction".into(),
            level: 2,
        }];
        let h = detect(&pages, &outline);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].level, 2);
        assert_eq!(h[0].evidence, Evidence::Outline);
    }

    #[test]
    fn figure_captions_are_not_headings() {
        assert!(is_caption(
            "Figure 2.1: Surveyed MSMEs by size across sectors (%)"
        ));
        assert!(is_caption("Table 3 Results of the second survey"));
        assert!(is_caption("Diagram 4 Distribution of Instagram Content"));
        assert!(!is_caption("Figures of speech in modern prose"));
        assert!(!is_caption("The Table Mountain region"));
        assert!(!is_caption("2. General Profile of MSMEs"));
    }

    #[test]
    fn a_bold_run_in_lead_in_is_not_a_heading() {
        // LaTeX \paragraph{}: bold phrase, then the paragraph it introduces.
        assert!(runs_into_prose(
            "Filtered task names. We present task names"
        ));
        assert!(runs_into_prose(
            "Results on data contamination. To show the in-"
        ));
    }

    #[test]
    fn abbreviations_and_trailing_stops_are_not_run_ins() {
        assert!(!runs_into_prose("CHAPTER 1."));
        assert!(!runs_into_prose("Introduction"));
        // The dot abbreviates; the heading continues.
        assert!(!runs_into_prose("Results vs. Baseline Methods"));
        assert!(!runs_into_prose("See Fig. 3 for details"));
        assert!(!runs_into_prose("Method of Doe et al. Revisited"));
    }

    #[test]
    fn font_size_fallback_when_no_outline() {
        let pages = vec![vec![
            blk("Big Title", 20.0, 1),
            blk("Sub Title", 14.0, 1),
            blk("body text", 10.0, 20),
        ]];
        let h = detect(&pages, &[]);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].level, 1);
        assert_eq!(h[1].level, 2);
        assert_eq!(h[0].evidence, Evidence::Title);
    }

    #[test]
    fn heading_identity_is_its_source_block_not_its_text() {
        let pages = vec![vec![
            blk("Repeated label", 20.0, 1),
            blk("body text begins here", 10.0, 20),
            blk("Repeated label", 10.0, 1),
        ]];
        let h = detect(&pages, &[]);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].block_index, 0);
    }

    #[test]
    fn isolated_first_page_title_can_share_body_size() {
        let mut title = blk("Print vs. Digital", 11.5, 1);
        title.bbox = BBox {
            x0: 0.0,
            y0: 10.0,
            x1: 100.0,
            y1: 21.5,
        };
        let mut body = blk("ordinary body prose here", 11.5, 20);
        body.bbox = BBox {
            x0: 0.0,
            y0: 40.0,
            x1: 100.0,
            y1: 51.5,
        };
        let hs = detect(&[vec![title, body]], &[]);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].evidence, Evidence::Title);
    }

    #[test]
    fn title_search_stops_when_prose_begins() {
        let mut prose = blk(
            "The jet velocity can be assumed to remain constant over the measured distance",
            11.0,
            2,
        );
        prose.bbox = BBox {
            x0: 0.0,
            y0: 10.0,
            x1: 100.0,
            y1: 32.0,
        };
        let mut equation = blk("Rearranging Equation (8) gives:", 11.0, 1);
        equation.bbox = BBox {
            x0: 0.0,
            y0: 80.0,
            x1: 100.0,
            y1: 91.0,
        };
        let hs = detect(&[vec![prose, equation]], &[]);
        assert!(hs.iter().all(|h| h.evidence != Evidence::Title));
    }

    #[test]
    fn multi_line_blocks_are_not_font_size_headings() {
        // A three-line paragraph set slightly larger than the modal size is
        // still a paragraph — the dense-datasheet false-positive class.
        let pages = vec![vec![
            blk("a fairly long paragraph of body prose that wraps", 10.0, 3),
            blk("table cell", 8.0, 40),
        ]];
        assert!(detect(&pages, &[]).is_empty());
    }

    #[test]
    fn list_items_are_not_font_size_headings() {
        let pages = vec![vec![
            blk("\u{2022} Wide input voltage range", 10.0, 1),
            blk("table cell", 8.0, 40),
        ]];
        assert!(detect(&pages, &[]).is_empty());
    }

    #[test]
    fn levels_are_ranked_by_style_across_the_document() {
        // Three heading styles at one document: 20pt, 14pt, and bold body size.
        // They must come out h1/h2/h3 regardless of which detector found them.
        let mut b = blk("Bold Section", 10.0, 1);
        b.bold = true;
        let pages = vec![vec![
            blk("Big Title", 20.0, 1),
            blk("Sub Title", 14.0, 1),
            b,
            blk("ordinary body prose here", 10.0, 40),
        ]];
        let h = detect(&pages, &[]);
        let mut lv: Vec<usize> = h.iter().map(|x| x.level).collect();
        lv.sort();
        assert_eq!(
            lv,
            vec![1, 2, 3],
            "distinct styles must map to distinct ranks: {h:?}"
        );
    }

    #[test]
    fn numbering_keeps_its_own_depth_through_normalization() {
        let pages = vec![vec![
            blk("6.3.1 Oscillator Frequency", 10.0, 1),
            blk("ordinary body prose here", 10.0, 40),
        ]];
        let h = detect(&pages, &[]);
        assert_eq!(h[0].level, 3, "numbering states its own depth");
    }

    #[test]
    fn bold_line_at_body_size_is_a_heading() {
        let mut h = blk("Market Penetration", 10.0, 1);
        h.bold = true;
        let pages = vec![vec![h, blk("ordinary body prose", 10.0, 30)]];
        let hs = detect(&pages, &[]);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].evidence, Evidence::Bold);
    }

    #[test]
    fn bold_is_ignored_when_the_whole_document_is_bold() {
        let mut a = blk("Heading-ish", 10.0, 1);
        let mut b = blk("body prose", 10.0, 30);
        a.bold = true;
        b.bold = true;
        assert!(detect(&[vec![a, b]], &[]).is_empty());
    }

    #[test]
    fn long_blocks_are_never_headings() {
        let long = "word ".repeat(60);
        let pages = vec![vec![blk(&long, 20.0, 1), blk("body", 10.0, 20)]];
        assert!(detect(&pages, &[]).is_empty());
    }

    #[test]
    fn body_size_is_weighted_by_lines_not_blocks() {
        // Ten one-line captions at 8pt against two big paragraphs at 11pt:
        // the body is 11pt, and block-counting would wrongly say 8pt.
        let mut blocks: Vec<Block> = (0..10).map(|_| blk("caption", 8.0, 1)).collect();
        blocks.push(blk("para", 11.0, 30));
        blocks.push(blk("para", 11.0, 30));
        assert_eq!(body_font_size(&[blocks]), 11.0);
    }

    #[test]
    fn body_size_ties_prefer_the_smaller_font() {
        let blocks = vec![blk("large", 12.0, 10), blk("small", 10.0, 10)];
        assert_eq!(body_font_size(&[blocks]), 10.0);
    }

    #[test]
    fn doubt_carries_the_page_it_measured() {
        let kinds: Vec<(&str, &str)> = vec![
            ("doco:SectionTitle", "size"),
            ("doco:SectionTitle", "size"),
            ("doco:SectionTitle", "size"),
            ("doco:Paragraph", "layout"),
        ];
        assert_eq!(doubt_on_page(7, &kinds).map(|d| d.page), Some(7));
        // The page-less form is the same measurement on page zero, which is
        // why a single-page corpus could not tell the two apart.
        assert_eq!(doubt(&kinds), doubt_on_page(0, &kinds));
    }

    #[test]
    fn a_document_of_mostly_headings_is_doubtful() {
        // 01030000000181: seven of ten elements claim to be titles, none
        // corroborated by an outline or a numbering pattern. Its text scores
        // 0.9654 and its hierarchy 0.2846.
        let kinds: Vec<(&str, &str)> = vec![
            ("doco:SectionTitle", "title"),
            ("doco:SectionTitle", "font-size"),
            ("doco:SectionTitle", "font-size"),
            ("doco:SectionTitle", "font-size"),
            ("doco:SectionTitle", "font-size"),
            ("doco:SectionTitle", "font-size"),
            ("doco:SectionTitle", "font-size"),
            ("doco:Paragraph", "layout"),
            ("doco:Paragraph", "layout"),
            ("doco:Paragraph", "layout"),
        ];
        let d = doubt(&kinds).expect("seven of ten is doubtful");
        assert_eq!(d.titles, 7);
        assert_eq!(d.corroborated, 0, "font size is not corroboration");
    }

    #[test]
    fn an_ordinary_document_is_not_doubtful() {
        let mut kinds: Vec<(&str, &str)> = vec![("doco:SectionTitle", "outline")];
        kinds.extend(std::iter::repeat_n(("doco:Paragraph", "layout"), 9));
        assert!(doubt(&kinds).is_none());
    }

    #[test]
    fn splice_anchors_do_not_change_the_verdict() {
        // The same document, once plain and once with anchors emitted, must
        // be doubted the same way: the caller that routes escalation is the
        // one that asks for anchors, so a ratio counting them made the signal
        // depend on who was looking.
        let plain: Vec<(&str, &str)> = vec![
            ("doco:SectionTitle", "size"),
            ("doco:SectionTitle", "size"),
            ("doco:SectionTitle", "size"),
            ("doco:Paragraph", "layout"),
            ("doco:Paragraph", "layout"),
        ];
        let mut anchored = plain.clone();
        anchored.push(("doco:Figure", "route"));
        anchored.push(("doco:Figure", "table-confidence"));
        anchored.push(("doco:Figure", "table-missing"));
        assert_eq!(doubt(&plain), doubt(&anchored));
        assert!(doubt(&plain).is_some(), "three of five is doubtful");
    }

    #[test]
    fn an_outline_backed_hierarchy_is_reported_as_corroborated() {
        // A slide deck is legitimately mostly titles; the outline says so,
        // and the count is what tells a consumer to trust it.
        let kinds: Vec<(&str, &str)> = vec![
            ("doco:SectionTitle", "outline"),
            ("doco:SectionTitle", "outline"),
            ("doco:SectionTitle", "outline"),
            ("doco:Paragraph", "layout"),
        ];
        let d = doubt(&kinds).expect("density is high");
        assert_eq!(d.corroborated, 3);
    }

    #[test]
    fn too_few_elements_to_judge() {
        // One title and one paragraph is 50% headings and perfectly correct.
        let kinds: Vec<(&str, &str)> = vec![
            ("doco:SectionTitle", "font-size"),
            ("doco:Paragraph", "layout"),
        ];
        assert!(doubt(&kinds).is_none());
    }
}
