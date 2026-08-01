//! Lines → blocks (paragraphs).
//!
//! A paragraph break is a *relative* judgement, not an absolute one. Measured
//! baseline-to-baseline distance, normalized by font size, over the corpus:
//!
//! ```text
//! long-form prose report    mode 1.45-1.55
//! dense technical datasheet mode 1.15-1.25
//! ```
//!
//! Those are both ordinary single-spaced body text; the documents simply set
//! different leading. A fixed threshold that splits paragraphs correctly in one
//! would either over-split or under-split the other, so the modal leading is
//! derived per document and the break test is expressed as a multiple of it.

use crate::geom::BBox;
use crate::line::Line;

/// A gap this many times the document's modal leading starts a new block.
/// Chosen to sit between single spacing (1.0x by definition) and the
/// blank-line spacing typical of a paragraph break (>=1.5x in every document
/// measured).
const PARAGRAPH_FACTOR: f64 = 1.35;

/// Relative font-size change that forces a break regardless of spacing. A
/// heading set 2pt larger than body text must not absorb the paragraph under
/// it.
const FONT_SIZE_TOLERANCE: f64 = 0.12;

/// Left-edge shift, as a fraction of font size, that may indicate a new block.
const INDENT_TOLERANCE: f64 = 1.0;

/// How close to the content's right edge a line must end to count as "full",
/// i.e. wrapped rather than deliberately ended.
///
/// Indentation alone is not a paragraph break. Both common styles change the
/// left edge *within* a paragraph: a first-line indent moves the opening line
/// right, and a hanging indent (every bulleted list) moves the continuations
/// right. What actually separates them is whether the preceding line was full.
/// Without this, wrapped list continuations became separate blocks and then
/// single-line "headings" — `integrated MOSFET switch`, `count`, `size` on
/// one datasheet's first page.
const FULL_LINE_FRACTION: f64 = 0.85;

/// Fallback when a document is too short to establish a modal leading.
const DEFAULT_LEADING: f64 = 1.2;

/// Longest text still considered a marker rather than content. Bullets (`■`,
/// `-`), list numbers (`1.`, `iv.`) and footnote references are all short.
const MAX_MARKER_CHARS: usize = 4;

#[derive(Debug, Clone)]
pub struct Block {
    pub lines: Vec<Line>,
    pub bbox: BBox,
    pub page: usize,
    pub font_size: f32,
    /// True when the block's lines are predominantly bold.
    pub bold: bool,
    /// Vertical gap above this block, in font-size units, or `None` for the
    /// first block in its column. A heading is usually set off by more
    /// whitespace than ordinary paragraph spacing — a signal orthogonal to
    /// size, weight and case.
    pub gap_above: Option<f64>,
    /// Bullet, list number or footnote reference attached to this block.
    ///
    /// Markers are pulled out before blocks are formed. A bullet's baseline
    /// sits slightly below the first line of its own text, so in document order
    /// it lands *between* line 1 and line 2 and would otherwise cut the
    /// paragraph into three. List detection consumes this later to build
    /// label/body pairs.
    pub marker: Option<String>,
}

impl Block {
    /// Joined text. Lines are joined with a space rather than a newline: a
    /// paragraph wrapped across lines is one sentence flow, and NER must not
    /// see a spurious boundary mid-sentence.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for l in &self.lines {
            if !out.is_empty() {
                // A hyphen at a line end is usually a soft break introduced by
                // wrapping; joining without it restores the original word.
                if out.ends_with('-') {
                    out.pop();
                } else {
                    out.push(' ');
                }
            }
            out.push_str(&l.text);
        }
        out
    }
}

/// Largest gap that can plausibly be *within* a paragraph. Gaps beyond this are
/// paragraph or section breaks in any typography, and must not be mistaken for
/// body leading.
const MAX_PLAUSIBLE_LEADING: f64 = 2.0;

/// Modal baseline-to-baseline distance, in font-size units, across a document.
///
/// The mode rather than the mean: a document is mostly body text, and the mean
/// is dragged by heading gaps and section breaks.
///
/// Only gaps up to [`MAX_PLAUSIBLE_LEADING`] are counted. Without that bound a
/// document made of single-line entries has no within-paragraph gaps at all, so
/// the mode becomes the *between-entry* gap and every entry merges into one
/// block — one statistical-listing document reported 3.43x and collapsed four
/// separate people into a single paragraph.
pub fn modal_leading(pages: &[Vec<Line>]) -> f64 {
    let mut hist = [0usize; 120];
    for lines in pages {
        for w in lines.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            if a.rotation_bucket != b.rotation_bucket {
                continue;
            }
            if a.bbox.x1.min(b.bbox.x1) - a.bbox.x0.max(b.bbox.x0) <= 0.0 {
                continue;
            }
            let fs = a.font_size.max(b.font_size).max(1.0) as f64;
            let d = (b.bbox.y0 - a.bbox.y0) / fs;
            if !(0.3..=MAX_PLAUSIBLE_LEADING).contains(&d) {
                continue;
            }
            let bucket = (d / 0.05) as usize;
            if bucket < hist.len() {
                hist[bucket] += 1;
            }
        }
    }
    let (best, n) = hist
        .iter()
        .enumerate()
        .max_by_key(|(_, n)| **n)
        .map(|(i, n)| (i, *n))
        .unwrap_or((0, 0));
    if n < 5 {
        return DEFAULT_LEADING;
    }
    best as f64 * 0.05 + 0.025
}

/// How far right of a block's left edge a marker may still sit and count as
/// introducing it. Covers rounding and a hanging indent, not a marker that has
/// been set inside the text.
const MARKER_OUTDENT_TOLERANCE: f64 = 2.0;

/// True when the text could be a bullet, list number or footnote reference —
/// as opposed to a short *word*.
///
/// Brevity alone is not enough. A marker's text is consumed: it becomes the
/// block's `marker` and never appears in the output. So a chart legend's
/// `EMEA`, set smaller than the percentage above it, was classified as a
/// marker for the neighbouring label and deleted — the region vanished from
/// a chart whose other three regions came through. Acronyms, units and short
/// labels are all vulnerable the same way.
///
/// Markers are punctuation (`•`, `-`, `■`), numbers (`3.`, `(2)`), roman
/// numerals (`iv.`) or a single letter (`a)`). Two or more letters that do
/// not spell a roman numeral are a word.
/// Characters used as bullets, and only those.
///
/// Geometric shapes and the typographic bullets. Dashes are deliberately
/// absent: a line opening with an en dash is as likely to be a range, a
/// dialogue dash or a torn compound as a list item, and treating it as a
/// bullet deletes the character from text where it belonged.
const BULLETS: &[char] = &[
    '\u{2022}', // •
    '\u{25AA}', // ▪
    '\u{25AB}', // ▫
    '\u{25A0}', // ■
    '\u{25A1}', // □
    '\u{25CF}', // ●
    '\u{25CB}', // ○
    '\u{25E6}', // ◦
    '\u{2023}', // ‣
    '\u{2219}', // ∙
    '\u{00B7}', // ·
    '\u{2043}', // ⁃
    '\u{25B8}', // ▸
    '\u{25AC}', // ▬
];

/// A bullet the layout pass read as the first character of its own line,
/// rather than as a marker standing apart from it.
///
/// A marker is normally pulled out before blocks form, because a bullet's
/// baseline sits slightly below the first line of its own text and it would
/// otherwise cut the paragraph in three. Where the producer sets the bullet
/// *inside* the text run there is nothing to pull out — the line simply
/// begins with it — and the list read as a paragraph beginning with a square.
///
/// Returns the text with the bullet and the space after it removed.
pub fn strip_leading_bullet(text: &str) -> Option<&str> {
    let t = text.trim_start();
    let mut chars = t.chars();
    let first = chars.next()?;
    if !BULLETS.contains(&first) {
        return None;
    }
    let rest = chars.as_str();
    // A bullet with nothing after it is a marker block, handled elsewhere; a
    // bullet run together with its word is more likely a glyph that happens
    // to look like one.
    let stripped = rest.strip_prefix([' ', '\u{00A0}', '\t'])?.trim_start();
    (!stripped.is_empty()).then_some(stripped)
}

fn marker_shaped(text: &str) -> bool {
    let t = text.trim().trim_end_matches(['.', ')', ']', ':', '(', '[']);
    let letters = t.chars().filter(|c| c.is_alphabetic()).count();
    if letters <= 1 {
        return true;
    }
    let lower = t.to_ascii_lowercase();
    lower
        .chars()
        .all(|c| matches!(c, 'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'))
}

/// True for a short line that is a bullet, list number or footnote reference
/// rather than content.
///
/// A marker's text is consumed. If it is later found to be positioned wrongly
/// it is not restored — it is simply gone, so this decision can delete text
/// outright. Punctuation carries nothing and losing it is free, but anything
/// with characters in it must first prove it is *placed* like a marker, at the
/// left margin of the content it introduces.
///
/// Without that proof a bar chart's tick labels destroy each other: `2024` and
/// `2023` sit side by side on one row, each outdented relative to the other, so
/// each reads as the other's bullet. Both vanished from every chart in an
/// annual report, leaving four figures with no years attached — the text was in
/// the glyph layer, and in the assembled lines, and disappeared here.
fn is_marker(l: &Line, neighbour: Option<&Line>, left_edge: f64) -> bool {
    let n = l.text.chars().filter(|c| !c.is_whitespace()).count();
    if n == 0 || n > MAX_MARKER_CHARS {
        return false;
    }
    if !marker_shaped(&l.text) {
        return false;
    }
    let Some(nb) = neighbour else { return false };
    if l.text.chars().any(char::is_alphanumeric)
        && l.bbox.x0 > left_edge + (l.font_size as f64).max(1.0)
    {
        return false;
    }
    let smaller = (l.font_size as f64) < nb.font_size as f64 * 0.92;
    let outdented = l.bbox.x1 <= nb.bbox.x0 + 1.0;
    smaller || outdented
}

/// Group one page's lines into blocks, given the document's modal leading.
pub fn assemble(lines: &[Line], leading: f64) -> Vec<Block> {
    assemble_with_marks(lines, leading, &[])
}

/// How far left of a line's start a checkbox may sit and still introduce it,
/// in font-size units. Covers the gap between box and text plus a hanging
/// indent; beyond it the box belongs to something else on the line.
const CHECKBOX_REACH: f64 = 3.0;

/// The checkbox introducing this line, if one sits immediately to its left on
/// the same visual row.
fn checkbox_for(l: &Line, boxes: &[BBox]) -> bool {
    let fs = (l.font_size as f64).max(1.0);
    boxes.iter().any(|b| {
        let vertical = b.y1 > l.bbox.y0 && b.y0 < l.bbox.y1;
        let gap = l.bbox.x0 - b.x1;
        vertical && gap > -fs && gap < fs * CHECKBOX_REACH
    })
}

/// Assemble blocks, treating each supplied checkbox as a list marker: the
/// line it introduces starts its own block and carries the marker.
pub fn assemble_with_marks(lines: &[Line], leading: f64, checkboxes: &[BBox]) -> Vec<Block> {
    // Right edge of the content, used to decide whether a line was wrapped.
    let right_edge = lines.iter().map(|l| l.bbox.x1).fold(f64::MIN, f64::max);
    let left_edge = lines.iter().map(|l| l.bbox.x0).fold(f64::MAX, f64::min);
    let span = (right_edge - left_edge).max(1.0);
    // Separate markers first so they cannot act as block boundaries.
    let mut content: Vec<Line> = Vec::new();
    let mut markers: Vec<Line> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let neighbour = lines
            .get(i + 1)
            .or_else(|| i.checked_sub(1).and_then(|j| lines.get(j)));
        if is_marker(l, neighbour, left_edge) {
            markers.push(l.clone());
        } else {
            content.push(l.clone());
        }
    }
    let lines = &content[..];

    let mut blocks: Vec<Block> = Vec::new();
    let mut cur: Vec<Line> = Vec::new();

    // Lines a checkbox introduces, by index into `lines`.
    let ticked: Vec<bool> = lines.iter().map(|l| checkbox_for(l, checkboxes)).collect();
    let mut marked: Vec<usize> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if cur.is_empty() {
            if ticked[i] {
                marked.push(blocks.len());
            }
            cur.push(l.clone());
            continue;
        }
        // A checkbox always begins a new option, however the geometry reads.
        if !ticked[i] && continues(cur.last().unwrap(), l, leading, right_edge, span) {
            cur.push(l.clone());
        } else {
            blocks.push(build(std::mem::take(&mut cur)));
            if ticked[i] {
                marked.push(blocks.len());
            }
            cur.push(l.clone());
        }
    }
    if !cur.is_empty() {
        blocks.push(build(cur));
    }
    for i in marked {
        if let Some(b) = blocks.get_mut(i) {
            b.marker = Some("\u{2610}".into());
        }
    }

    // Vertical gap above each block, for the isolation signal.
    for i in 1..blocks.len() {
        let fs = blocks[i].font_size.max(1.0) as f64;
        let gap = (blocks[i].bbox.y0 - blocks[i - 1].bbox.y1) / fs;
        if gap >= 0.0 {
            blocks[i].gap_above = Some(gap);
        }
    }

    // Attach each marker to the block whose first line it sits nearest, by
    // vertical distance. A bullet belongs to the paragraph it introduces.
    for m in markers {
        let my = m.bbox.y0;
        // Containment first: a superscript footnote reference sits *inside* the
        // paragraph it annotates, so nearest-start would wrongly hand it to the
        // following block. Only when nothing contains it does it become a
        // leading marker for the block that starts just below.
        let contained = blocks
            .iter()
            .position(|b| my >= b.bbox.y0 - 1.0 && my <= b.bbox.y1 + 1.0);
        let idx = contained.or_else(|| {
            blocks
                .iter()
                .enumerate()
                .filter(|(_, b)| b.bbox.y0 >= my - m.bbox.height() * 2.0)
                .min_by(|(_, a), (_, b)| {
                    (a.bbox.y0 - my)
                        .abs()
                        .partial_cmp(&(b.bbox.y0 - my).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i)
        });
        if let Some(i) = idx {
            // A list marker stands to the *left* of what it introduces. One
            // sitting inside the text is an affiliation or footnote reference
            // annotating a word — an author line with `∗†` set above three
            // names mid-line read as a marker and turned the whole author
            // block into a list item. Vertical containment alone cannot tell
            // them apart.
            if m.bbox.x1 > blocks[i].bbox.x0 + MARKER_OUTDENT_TOLERANCE {
                continue;
            }
            if blocks[i].marker.is_none() {
                blocks[i].marker = Some(m.text.clone());
            }
        }
    }
    blocks
}

fn continues(prev: &Line, next: &Line, leading: f64, right_edge: f64, span: f64) -> bool {
    if prev.rotation_bucket != next.rotation_bucket {
        return false;
    }
    // Different columns: no vertical relationship at all.
    if prev.bbox.x1.min(next.bbox.x1) - prev.bbox.x0.max(next.bbox.x0) <= 0.0 {
        return false;
    }
    let fs = prev.font_size.max(next.font_size).max(1.0) as f64;

    // Weight change — a run-in heading. Very common in academic papers, where
    // a bold section title sits directly above body text *at the same size*, so
    // the font-size test below cannot see it. Measured case: `2 Depth
    // Up-Scaling` was absorbed into an 11-line paragraph and never reached
    // heading detection at all.
    if prev.bold != next.bold {
        return false;
    }

    // Font size change — a heading, or a shift to a caption.
    let rel = (prev.font_size - next.font_size).abs() as f64 / fs;
    if rel > FONT_SIZE_TOLERANCE {
        return false;
    }

    let dy = (next.bbox.y0 - prev.bbox.y0) / fs;
    if dy <= 0.0 {
        // Not below the previous line: a side-by-side run the line pass kept
        // separate, so not a continuation.
        return false;
    }
    if dy > leading * PARAGRAPH_FACTOR {
        return false;
    }

    // Indentation change, but only when the previous line was *not* full. A
    // full line ended because it ran out of room, so the next line continues
    // the same paragraph however its left edge moves.
    let indent = (next.bbox.x0 - prev.bbox.x0).abs() / fs;
    if indent > INDENT_TOLERANCE {
        let prev_full =
            (prev.bbox.x1 - (right_edge - span * (1.0 - FULL_LINE_FRACTION))) >= -f64::EPSILON;
        if !prev_full {
            return false;
        }
    }

    true
}

/// Whether a poster-like display word governs the lettered item below it.
///
/// Both lines may use the body face and size, so typography provides no
/// boundary. The sequence itself does: a short display label followed by a
/// lowercase lettered item starts a new structural section.
pub fn display_before_lettered_item(first: &str, second: &str) -> bool {
    let first = first.trim();
    let second = second.trim();
    first.split_whitespace().count() == 1
        && first.chars().count() >= 4
        && first
            .chars()
            .find(|c| c.is_alphabetic())
            .is_some_and(char::is_uppercase)
        && second.as_bytes().get(1) == Some(&b'.')
        && second
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_lowercase)
        && second
            .as_bytes()
            .get(2)
            .is_some_and(u8::is_ascii_whitespace)
}

/// Split that display word from the lettered content block it was joined to.
///
/// Shared by the product and diagnostic pipelines so heading block identity
/// cannot drift between them.
pub fn split_structural_prefix(b: Block) -> Vec<Block> {
    if b.lines.len() < 2 || !display_before_lettered_item(&b.lines[0].text, &b.lines[1].text) {
        return vec![b];
    }
    let marker = b.marker.clone();
    let gap_above = b.gap_above;
    let mut lines = b.lines;
    let tail = lines.split_off(1);
    let mut prefix = build(lines);
    prefix.marker = marker;
    prefix.gap_above = gap_above;
    let mut remainder = build(tail);
    let gap = (remainder.bbox.y0 - prefix.bbox.y1) / remainder.font_size.max(1.0) as f64;
    if gap >= 0.0 {
        remainder.gap_above = Some(gap);
    }
    vec![prefix, remainder]
}

pub(crate) fn build(lines: Vec<Line>) -> Block {
    let mut bbox = lines[0].bbox;
    for l in &lines[1..] {
        bbox = bbox.union(&l.bbox);
    }
    let mut sizes: Vec<f32> = lines.iter().map(|l| l.font_size).collect();
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let font_size = sizes[sizes.len() / 2];
    let bold = lines.iter().filter(|l| l.bold).count() * 2 > lines.len();
    Block {
        page: lines[0].page,
        bbox,
        font_size,
        bold,
        gap_above: None,
        lines,
        marker: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(text: &str, x: f64, y: f64, fs: f32) -> Line {
        Line {
            text: text.into(),
            bbox: BBox {
                x0: x,
                y0: y,
                x1: x + 300.0,
                y1: y + fs as f64,
            },
            page: 0,
            rotation_bucket: 0,
            glyphs: vec![],
            font_size: fs,
            bold: false,
        }
    }

    #[test]
    fn short_words_are_not_markers() {
        // A marker's text is consumed, so misjudging one deletes it. A chart
        // legend's `EMEA` set smaller than the figure above it took the
        // region out of the document entirely.
        for word in ["EMEA", "USA", "R&D", "Note", "FTE"] {
            assert!(!marker_shaped(word), "{word} is a word, not a marker");
        }
        for mark in ["•", "-", "3.", "(2)", "iv.", "a)", "*", "vi", "IV"] {
            assert!(marker_shaped(mark), "{mark} is a marker");
        }
    }

    #[test]
    fn an_acronym_beside_larger_text_survives() {
        let mut small = l("EMEA", 186.0, 394.0, 9.0);
        small.bbox.x1 = 210.0;
        let lines = vec![l("28.0%", 189.0, 380.0, 12.0), small];
        let b = assemble(&lines, 1.2);
        let text: String = b.iter().map(|x| x.text()).collect::<Vec<_>>().join("|");
        assert!(text.contains("EMEA"), "EMEA was swallowed: {text}");
    }

    #[test]
    fn a_checkbox_starts_its_own_option() {
        // Three options set as consecutive lines. Nothing in the geometry
        // separates them — without the boxes they read as one paragraph.
        let lines = vec![
            l("Cured", 50.0, 0.0, 9.0),
            l("Servicing Transferred", 50.0, 11.0, 9.0),
            l("Remains Delinquent", 50.0, 22.0, 9.0),
        ];
        assert_eq!(assemble(&lines, 1.2).len(), 1, "no boxes: one paragraph");

        let boxes: Vec<BBox> = (0..3)
            .map(|i| BBox {
                x0: 40.0,
                y0: i as f64 * 11.0,
                x1: 47.0,
                y1: i as f64 * 11.0 + 7.0,
            })
            .collect();
        let b = assemble_with_marks(&lines, 1.2, &boxes);
        assert_eq!(b.len(), 3, "one block per option, got {b:?}");
        assert!(b.iter().all(|x| x.marker.as_deref() == Some("\u{2610}")));
    }

    #[test]
    fn a_box_far_from_the_text_does_not_mark_it() {
        let lines = vec![
            l("first line of", 10.0, 0.0, 10.0),
            l("the paragraph", 10.0, 12.0, 10.0),
        ];
        // A box off in another column entirely.
        let boxes = vec![BBox {
            x0: 400.0,
            y0: 0.0,
            x1: 407.0,
            y1: 7.0,
        }];
        let b = assemble_with_marks(&lines, 1.2, &boxes);
        assert_eq!(b.len(), 1);
        assert!(b[0].marker.is_none());
    }

    #[test]
    fn joins_wrapped_lines_into_one_paragraph() {
        let lines = vec![
            l("first line of", 10.0, 0.0, 10.0),
            l("the paragraph", 10.0, 12.0, 10.0),
        ];
        let b = assemble(&lines, 1.2);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].text(), "first line of the paragraph");
    }

    #[test]
    fn splits_on_paragraph_gap() {
        // 12pt leading, then a 24pt gap.
        let lines = vec![
            l("para one", 10.0, 0.0, 10.0),
            l("still one", 10.0, 12.0, 10.0),
            l("para two", 10.0, 36.0, 10.0),
        ];
        assert_eq!(assemble(&lines, 1.2).len(), 2);
    }

    #[test]
    fn splits_on_weight_change() {
        // A bold run-in heading above same-size body text.
        let mut h = l("2 Depth Up-Scaling", 10.0, 0.0, 10.0);
        h.bold = true;
        let lines = vec![
            h,
            l("To efficiently scale-up LLMs, we aim to", 10.0, 12.0, 10.0),
        ];
        let b = assemble(&lines, 1.2);
        assert_eq!(
            b.len(),
            2,
            "a bold heading must not absorb the paragraph below it"
        );
        assert_eq!(b[0].text(), "2 Depth Up-Scaling");
    }

    #[test]
    fn splits_on_font_size_change() {
        // A heading immediately above body text, same spacing.
        let lines = vec![
            l("Introduction", 10.0, 0.0, 16.0),
            l("body text", 10.0, 19.0, 10.0),
        ];
        assert_eq!(assemble(&lines, 1.2).len(), 2);
    }

    #[test]
    fn splits_on_indent_after_a_short_line() {
        // The first line ends well short of the right edge, so it ended
        // deliberately; the indent that follows starts a new block.
        let mut short = l("short", 10.0, 0.0, 10.0);
        short.bbox.x1 = 60.0;
        let lines = vec![short, l("indented", 40.0, 12.0, 10.0)];
        assert_eq!(assemble(&lines, 1.2).len(), 2);
    }

    #[test]
    fn hanging_indent_does_not_split_a_list_item() {
        // A bulleted item whose continuation is indented under the marker.
        // Both lines are full; this is wrapping, not a new block.
        let lines = vec![
            l(
                "\u{2022} High efficiency up to 95% enabled by",
                10.0,
                0.0,
                10.0,
            ),
            l("integrated MOSFET switch", 24.0, 12.0, 10.0),
        ];
        let b = assemble(&lines, 1.2);
        assert_eq!(
            b.len(),
            1,
            "hanging indent is wrapping, not a paragraph break"
        );
    }

    #[test]
    fn rejoins_hyphenated_line_break() {
        let lines = vec![l("innova-", 10.0, 0.0, 10.0), l("tion", 10.0, 12.0, 10.0)];
        assert_eq!(assemble(&lines, 1.2)[0].text(), "innovation");
    }

    #[test]
    fn separate_columns_never_merge() {
        let mut right = l("right column", 400.0, 0.0, 10.0);
        right.bbox.x1 = 700.0;
        let lines = vec![l("left column", 10.0, 0.0, 10.0), right];
        assert_eq!(
            assemble(&lines, 1.2).len(),
            2,
            "no x-overlap means no relationship"
        );
    }

    #[test]
    fn bullet_does_not_split_its_own_paragraph() {
        // The bullet's baseline sits between line 1 and line 2 of the text it
        // introduces.
        let mut bullet = l("\u{25A0}", 0.0, 3.0, 9.0);
        bullet.bbox.x1 = 8.0;
        let lines = vec![
            l("Anticipate next steps in strategy", 30.0, 0.0, 10.0),
            bullet,
            l("and execution by matching", 30.0, 12.0, 10.0),
        ];
        let b = assemble(&lines, 1.2);
        assert_eq!(b.len(), 1, "bullet must not cut the paragraph");
        assert_eq!(b[0].marker.as_deref(), Some("\u{25A0}"));
        assert!(b[0]
            .text()
            .starts_with("Anticipate next steps in strategy and execution"));
    }

    #[test]
    fn superscript_footnote_marker_does_not_split() {
        let mut sup = l("1", 0.0, 3.0, 7.0);
        sup.bbox.x1 = 4.0;
        let lines = vec![
            l("body text before", 30.0, 0.0, 10.0),
            sup,
            l("body text after", 30.0, 12.0, 10.0),
        ];
        assert_eq!(assemble(&lines, 1.2).len(), 1);
    }

    #[test]
    fn marker_inside_a_paragraph_attaches_to_that_paragraph() {
        // A footnote reference midway down a long paragraph belongs to it, not
        // to the block that follows.
        let mut sup = l("1", 0.0, 24.0, 7.0);
        sup.bbox.x1 = 4.0;
        let lines = vec![
            l("line one", 30.0, 0.0, 10.0),
            l("line two", 30.0, 12.0, 10.0),
            sup,
            l("line three", 30.0, 24.0, 10.0),
            l("far below, new block", 30.0, 200.0, 10.0),
        ];
        let b = assemble(&lines, 1.2);
        assert_eq!(b.len(), 2);
        assert_eq!(
            b[0].marker.as_deref(),
            Some("1"),
            "must attach to the containing paragraph"
        );
        assert_eq!(b[1].marker, None);
    }

    #[test]
    fn a_superscript_inside_the_text_is_not_a_list_marker() {
        // Affiliation marks above author names: vertically inside the line,
        // but far to the right of its left edge.
        let mut sup = l("*", 200.0, 0.0, 7.0);
        sup.bbox.x1 = 204.0;
        let lines = vec![
            l("Dahyun Kim, Chanjun Park, Sanghoon Kim", 30.0, 2.0, 10.0),
            sup,
        ];
        let b = assemble(&lines, 1.2);
        assert_eq!(b.len(), 1);
        assert_eq!(
            b[0].marker, None,
            "an inline superscript must not make a list item"
        );
    }

    #[test]
    fn a_short_real_line_is_not_treated_as_a_marker() {
        // Same size, same left edge: content, not a marker.
        let lines = vec![l("Yes.", 30.0, 0.0, 10.0), l("Next para", 30.0, 40.0, 10.0)];
        let b = assemble(&lines, 1.2);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].text(), "Yes.");
    }

    #[test]
    fn single_line_entries_fall_back_to_default_leading() {
        // Every gap is 3.4x — a list of one-line entries, not a paragraph.
        // Taking that as body leading would merge every entry.
        let page: Vec<Line> = (0..10)
            .map(|i| l("entry", 10.0, i as f64 * 34.0, 10.0))
            .collect();
        let m = modal_leading(std::slice::from_ref(&page));
        assert!(
            (m - DEFAULT_LEADING).abs() < 1e-9,
            "expected default, got {m}"
        );
        assert_eq!(
            assemble(&page, m).len(),
            10,
            "each entry stays its own block"
        );
    }

    #[test]
    fn modal_leading_is_document_specific() {
        // 1.5x leading throughout; the mode must reflect that, not a default.
        let page: Vec<Line> = (0..20)
            .map(|i| l("x", 10.0, i as f64 * 15.0, 10.0))
            .collect();
        let m = modal_leading(&[page]);
        assert!((m - 1.5).abs() < 0.06, "expected ~1.5, got {m}");
    }

    #[test]
    fn a_bullet_inside_the_text_run_still_marks_a_list() {
        assert_eq!(
            strip_leading_bullet("\u{25A0} Advanced batteries could be key"),
            Some("Advanced batteries could be key")
        );
        assert_eq!(
            strip_leading_bullet("\u{2022}\u{00A0}Non-breaking space"),
            Some("Non-breaking space")
        );
    }

    #[test]
    fn a_dash_is_not_a_bullet() {
        // An opening en dash is as likely a range, a dialogue dash or a torn
        // compound, and stripping it deletes a character that belonged.
        assert_eq!(strip_leading_bullet("\u{2013} 2019 saw a decline"), None);
        assert_eq!(strip_leading_bullet("- plain hyphen"), None);
    }

    #[test]
    fn a_bullet_run_into_its_word_is_left_alone() {
        // No space after it: more likely a glyph that looks like a bullet
        // than a marker.
        assert_eq!(strip_leading_bullet("\u{25A0}Advanced"), None);
        assert_eq!(strip_leading_bullet("\u{25A0}"), None);
        assert_eq!(strip_leading_bullet("\u{25A0}   "), None);
    }

    #[test]
    fn ordinary_prose_is_untouched() {
        assert_eq!(strip_leading_bullet("Advanced batteries"), None);
        assert_eq!(strip_leading_bullet(""), None);
    }
}
