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
use std::collections::HashMap;

/// A gap this many times the document's modal leading starts a new block.
/// Chosen to sit between single spacing (1.0x by definition) and the
/// blank-line spacing typical of a paragraph break (>=1.5x in every document
/// measured).
const PARAGRAPH_FACTOR: f64 = 1.35;

/// Relative font-size change that forces a break regardless of spacing. A
/// heading set 2pt larger than body text must not absorb the paragraph under
/// it. Loose, because the lines of one paragraph do not measure alike: a
/// paper set in 11pt reads 10.9 on one line and 11.1 on the next, and a
/// line whose median lands on an inline bold run reads 10.7.
const FONT_SIZE_TOLERANCE: f64 = 0.12;

/// The smallest size step a typesetter makes on purpose — half a point,
/// 5.6% at 9pt — and a break on its own, but only across a line that was
/// ended by hand. A service log sets each code as a short 9pt label over
/// its 8.5pt description at body leading, and three separate entries read as
/// one paragraph under the loose tolerance; between two full lines the same
/// step is the jitter above.
const SIZE_STEP: f64 = 0.05;

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

/// Fewest lines a block needs before its ragged right edge is read as hand
/// line breaks. Two lines are a wrapped title as often as a form, and a title
/// broken by hand in two must stay one heading.
const MIN_HARD_BREAK_LINES: usize = 3;

/// Longest text still considered a marker rather than content. Bullets (`■`,
/// `-`), list numbers (`1.`, `iv.`) and footnote references are all short.
const MAX_MARKER_CHARS: usize = 4;

/// Farthest, in its item's size, a marker stands left of the item's text: a
/// hanging indent or a tab stop. A number further off is a label of its own,
/// a drawing's callout beside another.
const MARKER_REACH: f64 = 5.0;

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
        let mut prev: Option<&Line> = None;
        for l in &self.lines {
            if !out.is_empty() {
                // A line ending on a hyphen joins the next as
                // [`HyphenJoin::of`] reads it.
                //
                // A script that sets no spaces between its words wraps between
                // any two characters, and a line it wrapped joins the next
                // without one: `有限公` and `司` are one name. A line that
                // stops short of the block's edge was ended by hand, and the
                // next is another item (an office over a company's name).
                let wrapped = prev
                    .is_some_and(|p| p.bbox.x1 >= self.bbox.x1 - p.font_size.max(1.0) as f64 * 1.5);
                let cjk = wrapped
                    && out
                        .chars()
                        .next_back()
                        .is_some_and(crate::line::wraps_anywhere)
                    && l.text
                        .chars()
                        .next()
                        .is_some_and(crate::line::wraps_anywhere);
                if let Some(stem) = out.strip_suffix('-') {
                    match HyphenJoin::of(stem, &l.text) {
                        HyphenJoin::Word => {
                            out.pop();
                        }
                        HyphenJoin::Hyphen => {}
                        HyphenJoin::Dash => out.push(' '),
                    }
                } else if !cjk && !closed_join(&out, &l.text) {
                    out.push(' ');
                }
            }
            out.push_str(&l.text);
            prev = Some(l);
        }
        out
    }
}

/// Whether a line ending in a dash set closed, or in a web address broken
/// at a slash, runs on into the next with no space: `restore—` over `D2D`,
/// `10–` over `20`, `…TechnicalDocumentLibrary/` over its file's name.
///
/// A dash set closed against the word before it joins that word to the
/// next, as the line's own hyphen does; one with a space before it was set
/// apart, and keeps its space. A slash ending a word may have had a space
/// after it on the line the break took (`fuego/ desprendimiento`), but an
/// address holds none.
pub(crate) fn closed_join(before: &str, next: &str) -> bool {
    let mut tail = before.chars().rev();
    let (Some(mark), Some(prev)) = (tail.next(), tail.next()) else {
        return false;
    };
    if prev.is_whitespace() || !next.chars().next().is_some_and(|c| !c.is_whitespace()) {
        return false;
    }
    match mark {
        '—' | '–' => true,
        '/' => {
            let token = before.rsplit(char::is_whitespace).next().unwrap_or("");
            token.contains("://") || token.to_ascii_lowercase().starts_with("www.")
        }
        _ => false,
    }
}

/// What a hyphen ending a line is, and so how the line joins the next.
#[derive(Debug, PartialEq)]
pub(crate) enum HyphenJoin {
    /// A break wrapping set inside a word (`microcom-` over `puter`):
    /// dropped, and the word read whole.
    Word,
    /// The text's own, in a number, a code, a compound or a range
    /// (`1-800-222-` over `1222`, `Polish-` over `German`, `(A)-` over
    /// `(L)`): kept, the lines joined without a space.
    Hyphen,
    /// A dash, set apart from the word before it (`a pause -`): kept, the
    /// lines joined with a space.
    Dash,
}

impl HyphenJoin {
    /// Read from the text before the hyphen and the next line.
    ///
    /// A typesetter breaks a word between two letters of one case, and
    /// leaves at least two on each side. Anything else at the end of a line
    /// was printed there: a figure beside it, a capital after a lowercase
    /// letter or before one (`Fly-Casting`, `R-warfarin`), a single letter
    /// (`e-mail`, `X-Forwarded`). An all-capital compound (`BY-PRODUCTS`)
    /// cannot be told from an all-capital word broken (`SO-LAR`), and is
    /// read as the word.
    pub(crate) fn of(stem: &str, next: &str) -> HyphenJoin {
        let Some(before) = stem.chars().next_back().filter(|c| !c.is_whitespace()) else {
            return HyphenJoin::Dash;
        };
        let Some(after) = next.chars().next() else {
            return HyphenJoin::Hyphen;
        };
        let left = stem.chars().rev().take_while(|c| c.is_alphabetic()).count();
        let right = next.chars().take_while(|c| c.is_alphabetic()).count();
        let one_case = (before.is_lowercase() && after.is_lowercase())
            || (before.is_uppercase() && after.is_uppercase());
        if one_case && left >= 2 && right >= 2 {
            HyphenJoin::Word
        } else {
            HyphenJoin::Hyphen
        }
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

/// The marker a block opened by a drawn box carries: `☐`.
pub(crate) const CHECKBOX_MARKER: &str = "\u{2610}";

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
/// Geometric shapes, the typographic bullets, the arrows and boxes the
/// dingbat fonts set as bullets, and the private-use codes a Word file
/// gives its symbol-font bullets: a PDF made from Word maps the bullet of a
/// default bulleted list to U+F0B7, the position of `•` in the Symbol font.
/// Extraction reads such a code as the character its font shows, so these
/// codes remain only where the font's name does not say which font it is.
/// Of them, only codes that are no letter in Symbol or Wingdings are taken;
/// U+F071 is `❑` in one and `θ` in the other.
///
/// Dashes are deliberately absent: a line opening with an en dash is as
/// likely to be a range, a dialogue dash or a torn compound as a list item,
/// and treating it as a bullet deletes the character from text where it
/// belonged.
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
    '\u{25BA}', // ►
    '\u{25C6}', // ◆
    '\u{2751}', // ❑
    '\u{2752}', // ❒
    '\u{2756}', // ❖
    '\u{27A2}', // ➢
    '\u{27A4}', // ➤
    '\u{F0B7}', // Symbol •
    '\u{F0A7}', // Wingdings ▪ (Symbol ♣)
    '\u{F0D8}', // Wingdings ➢ (Symbol ¬)
    '\u{F0E8}', // Wingdings ➔ (Symbol ⎝)
];

/// A character used as a bullet; see [`BULLETS`].
pub(crate) fn is_bullet(c: char) -> bool {
    BULLETS.contains(&c)
}

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
    if !is_bullet(first) {
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

/// A character that is content, not a bullet's shape: a letter or digit, a
/// sign that stands for letters or digits — `™` for `TM`, `℃`, `№`, `㎡` —
/// which line text keeps as printed and which spelled out was content, or a
/// mathematical operator, a formula's `∂` or `∑` set apart beside its bar.
/// The minus sign is read as a dash, and `∙` is a bullet.
fn says_something(c: char) -> bool {
    use unicode_normalization::UnicodeNormalization;
    c.is_alphanumeric()
        || c.to_string().nfkc().any(char::is_alphanumeric)
        || (('\u{2200}'..='\u{22FF}').contains(&c) && c != '\u{2212}' && !is_bullet(c))
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
/// each reads as the other's bullet. And a number marks an item, which says
/// more than a number: a tick label set beside another is none. Both vanished from every chart in an
/// annual report, leaving four figures with no years attached — the text was in
/// the glyph layer, and in the assembled lines, and disappeared here.
fn is_marker(l: &Line, neighbour: Option<&Line>, left_edge: f64) -> bool {
    let n = l.text.chars().filter(|c| !c.is_whitespace()).count();
    if n == 0 || n > MAX_MARKER_CHARS {
        return false;
    }
    // A registered or trade mark sign standing alone is a word's, set
    // beside a display title it did not join: never a bullet.
    if l.text.split_whitespace().all(crate::line::is_sign) {
        return false;
    }
    if !marker_shaped(&l.text) {
        return false;
    }
    let Some(nb) = neighbour else { return false };
    if l.text.chars().any(says_something) && l.bbox.x0 > left_edge + (l.font_size as f64).max(1.0) {
        return false;
    }
    // Nor does it mark another such: a chart's tick labels set side by side,
    // `2.4−` beside `2.6−`, are no number and its item.
    if l.text.chars().any(says_something) && marker_shaped(&nb.text) {
        return false;
    }
    // A list's marker is set at most a little larger than its item, never
    // as display type beside it: a margin title's `III.` beside a column.
    if l.font_size as f64 >= nb.font_size as f64 * DISPLAY_RATIO {
        return false;
    }
    let smaller = (l.font_size as f64) < nb.font_size as f64 * 0.92;
    let outdented = l.bbox.x1 <= nb.bbox.x0 + 1.0;
    let within = nb.bbox.x0 - l.bbox.x1 <= (nb.font_size as f64).max(1.0) * MARKER_REACH;
    (smaller || outdented) && within
}

/// Group one page's lines into blocks, given the document's modal leading.
pub fn assemble(lines: &[Line], leading: f64) -> Vec<Block> {
    assemble_with_marks(lines, leading, &[], &[], &[], 0.0)
}

/// The fill a line sits in: the smallest one that spans it horizontally and
/// holds its vertical centre. `None` on bare paper.
///
/// A row's shading is drawn once per row, so two lines under different fills
/// are two rows however close their baselines. A form that sets each item
/// on its own shaded band, with the item's label wrapped onto two lines
/// inside it, can leave the gap between bands a hundredth under the break
/// threshold, and whether two items merge then comes down to rounding. The
/// smallest containing fill wins so a box drawn *inside* a larger panel
/// still separates its rows.
fn band_of(l: &Line, bands: &[BBox]) -> Option<usize> {
    let fs = (l.font_size as f64).max(1.0);
    let cy = (l.bbox.y0 + l.bbox.y1) * 0.5;
    bands
        .iter()
        .enumerate()
        .filter(|(_, b)| {
            b.x0 <= l.bbox.x0 + fs && b.x1 >= l.bbox.x1 - fs && b.y0 <= cy && cy <= b.y1
        })
        .min_by(|(_, a), (_, b)| {
            a.height()
                .partial_cmp(&b.height())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
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

/// Is a drawn bullet set beside this line: level with it, and just left of
/// where its text starts?
fn dot_for(l: &Line, dots: &[BBox]) -> bool {
    let fs = (l.font_size as f64).max(1.0);
    dots.iter().any(|d| {
        let middle = (d.y0 + d.y1) * 0.5;
        let gap = l.bbox.x0 - d.x1;
        middle > l.bbox.y0 && middle < l.bbox.y1 && gap > 0.0 && gap < fs * DOT_REACH
    })
}

/// How far apart, in points, the marks of one dash-led list may start.
const RUN_ALIGN: f64 = 1.5;

/// The dots that mark a list's items on a page: each stands beside one of
/// `lines`, and another dot set at the same place stands beside another.
/// A list draws a bullet for each item, in a column; a seal or a chart
/// draws its dots in rings and scatters, and only by chance is one level
/// with a line of text.
///
/// A list of one item draws one bullet. It marks its line when the page's
/// lists draw theirs alike: a dot of their size, as far from text of their
/// items' size as theirs, `K-23723 Faucet cleaner` under a heading of its
/// own.
pub fn marking_dots(lines: &[Line], dots: &[BBox]) -> Vec<BBox> {
    let centre = |d: &BBox| ((d.x0 + d.x1) * 0.5, (d.y0 + d.y1) * 0.5);
    // A drawing's dots crowd one another: a map's towns, a chart's markers.
    // A list's bullets stand a line apart.
    let crowded = |d: &BBox, em: f64| {
        let (x, y) = centre(d);
        dots.iter().any(|o| {
            let (ox, oy) = centre(o);
            o != d && (ox - x).hypot(oy - y) < em * DOT_CROWD
        })
    };
    let marks: Vec<(BBox, usize)> = dots
        .iter()
        .filter_map(|d| {
            lines
                .iter()
                .position(|l| dot_for(l, std::slice::from_ref(d)))
                .map(|i| (*d, i))
        })
        .filter(|(d, i)| !crowded(d, (lines[*i].font_size as f64).max(1.0)))
        .collect();
    let listed: Vec<&(BBox, usize)> = marks
        .iter()
        .filter(|(d, i)| {
            marks
                .iter()
                .any(|(o, j)| j != i && (o.x0 - d.x0).abs() <= 1.0 && (o.x1 - d.x1).abs() <= 1.0)
        })
        .collect();
    let gap = |d: &BBox, i: usize| lines[i].bbox.x0 - d.x1;
    let alike = |d: &BBox, i: usize| {
        let em = (lines[i].font_size as f64).max(1.0);
        listed.iter().any(|(o, j)| {
            let size = o.width().max(o.height()).max(0.1);
            let item = lines[*j].font_size as f64;
            (o.width() - d.width()).abs() <= size * DOT_LIKE
                && (o.height() - d.height()).abs() <= size * DOT_LIKE
                && (gap(o, *j) - gap(d, i)).abs() <= em * DOT_GAP_LIKE
                // Beside text at the list items' size, and they phrases: a
                // drawing's labels, a word or two beside its terminals' dots
                // set in columns, are no list a lone dot can join.
                && (em - item).abs() <= item * SAME_SIZE
                && lines[*j].text.split_whitespace().count() >= LIST_WORDS
        })
    };
    marks
        .iter()
        .filter(|(d, i)| listed.iter().any(|(o, j)| o == d && j == i) || alike(d, *i))
        .map(|(d, _)| *d)
        .collect()
}

/// Nearest, in the em of the line it marks, another dot stands to a list's
/// bullet. A list sets its bullets a line apart, an em or more; dots
/// closer than half that are a drawing's, set beside a heading only by
/// chance (a district map's towns beside its district's title).
const DOT_CROWD: f64 = 0.5;

/// Fewest words a list's item says for a lone bullet to join its list: an
/// item is a phrase, where a drawing's label names a part in a word or two.
const LIST_WORDS: usize = 3;

/// Widest difference between the width or the height of two bullets drawn
/// alike, as a share of the listed bullet's size. One routine draws a
/// list's bullets, so they differ only by the rounding of their outline's
/// coordinates, a few hundredths of a point on a bullet of one or two; a
/// fifth of its size takes that and stops short of a dot drawn another
/// size, which is another mark.
const DOT_LIKE: f64 = 0.2;

/// Widest difference, in ems of the item's text, between how far two
/// bullets drawn alike stand from their text. A list sets one indent for
/// all its items; a tenth of an em takes the rounding of where the text is
/// set, and stops short of a word space, a quarter of an em, by which a dot
/// set elsewhere would differ.
const DOT_GAP_LIKE: f64 = 0.1;

/// Furthest a drawn bullet stands left of its line's text, in the line's
/// size: a bullet's own width and the space after it.
const DOT_REACH: f64 = 2.0;

/// Marks a list opens its items with in the text itself, where a dash or an
/// arrow alone could as well be a range, a minus or a dialogue's dash: a
/// run of lines each opening with one of them makes them a list's.
const RUN_LEADS: &[char] = &[
    '-', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2212}', '\u{2192}', '\u{2794}', '\u{27F6}', '>',
];

/// A glyph that says nothing as text, which a list can open its items with
/// as it does with a bullet: a code the font maps to a control character
/// or to the private use area (a `▶` set as 0x17), or an arrow, a shape or
/// a dingbat no list of bullets names. Taken as a mark only in a run of
/// lines that open with it, as a dash is.
fn mark_glyph(c: char) -> bool {
    !is_bullet(c)
        && (fluree_doc_model::is_stray_control(c)
            || ('\u{E000}'..='\u{F8FF}').contains(&c)
            || ('\u{2190}'..='\u{21FF}').contains(&c)
            || ('\u{25A0}'..='\u{25FF}').contains(&c)
            || ('\u{2700}'..='\u{27BF}').contains(&c))
}

/// A mark a run of lines can open a list's items with: a dash, an arrow,
/// or a glyph that says nothing as text.
fn leads_runs(c: char) -> bool {
    RUN_LEADS.contains(&c) || mark_glyph(c)
}

/// The dash, arrow or mark a line opens with, followed by a space and
/// words.
fn run_lead(l: &Line) -> Option<char> {
    let t = l.text.trim_start();
    let mut chars = t.chars();
    let c = chars.next()?;
    let rest = chars.as_str();
    // A `>` opens an item that opens a sentence or a figure and says a few
    // words, `> Ergonomic design`: a quoted message's lines open with one
    // too, going on with their sentences in lower case, and a comparison's
    // sign begins a fragment (`> C`, `> 0`).
    let opens = rest
        .trim_start()
        .chars()
        .next()
        .is_some_and(|f| f.is_uppercase() || f.is_ascii_digit())
        && rest.split_whitespace().count() >= 2;
    (leads_runs(c)
        && rest.starts_with([' ', '\u{00A0}', '\t'])
        && rest.chars().any(char::is_alphanumeric)
        && (c != '>' || opens))
        .then_some(c)
}

/// Lines apart, at most, two items of one dash-led list may stand: an item
/// wrapped over several lines, or a short line between (`or`). The lines
/// between hang under the item's text or say a word or two; a line of
/// prose set where the marks are ends the run.
const RUN_REACH: usize = 6;

/// Most words a line between two items may say and not end their list.
const RUN_SEPARATOR_WORDS: usize = 3;

/// A block's text without the dash or arrow it was marked by, when it
/// opens with it.
pub fn strip_run_lead<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    let mut m = marker.chars();
    let lead = m.next().filter(|&c| leads_runs(c) && m.next().is_none())?;
    let rest = text.trim_start().strip_prefix(lead)?;
    let rest = rest.strip_prefix([' ', '\u{00A0}', '\t'])?.trim_start();
    (!rest.is_empty()).then_some(rest)
}

/// The marker a block opened by a drawn bullet carries: `•`.
pub(crate) const BULLET_MARKER: &str = "\u{2022}";

/// Assemble blocks, treating each supplied checkbox and drawn bullet as a
/// list marker — the line it introduces starts its own block and carries
/// the marker — and each supplied fill as a band no block crosses; see
/// [`band_of`].
///
/// `body` is the page's body size ([`body_size`]), the measure display type
/// is told by; zero takes the column's own.
pub fn assemble_with_marks(
    lines: &[Line],
    leading: f64,
    checkboxes: &[BBox],
    dots: &[BBox],
    bands: &[BBox],
    body: f64,
) -> Vec<Block> {
    assemble_in_measure(lines, lines, leading, checkboxes, dots, bands, body)
}

/// [`assemble_with_marks`] over a stretch of a column, `lines`, whose
/// measure is set by `measure`, the column's lines: a wrapped title read
/// as a region of its own reaches the column's edge, not the edge of its
/// own longer line, and a figure's callout number at the head of a
/// stretch is no list marker unless it stands at the column's margin.
pub fn assemble_in_measure(
    lines: &[Line],
    measure: &[Line],
    leading: f64,
    checkboxes: &[BBox],
    dots: &[BBox],
    bands: &[BBox],
    body: f64,
) -> Vec<Block> {
    // Right edge of the content, used to decide whether a line was wrapped.
    let edge = Edge::of(measure, body);
    // A marker is set at the measure's left margin.
    let left_edge = measure.iter().map(|l| l.bbox.x0).fold(f64::MAX, f64::min);
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

    // Lines a checkbox introduces, by index into `lines`, and lines a drawn
    // bullet does. A producer that draws its bullets leaves no bullet in the
    // text, and its items split only where their spacing happened to read
    // as a paragraph's.
    let ticked: Vec<bool> = lines.iter().map(|l| checkbox_for(l, checkboxes)).collect();
    let dotted: Vec<bool> = lines
        .iter()
        .zip(&ticked)
        .map(|(l, &t)| !t && dot_for(l, dots))
        .collect();
    // Lines that open a dash- or arrow-led item: another line near it opens
    // with the same mark at the same place. A lone line so opening is a
    // range, a minus or a dialogue's dash, and keeps reading as it did.
    let leads: Vec<Option<char>> = lines.iter().map(run_lead).collect();
    let runs: Vec<Option<char>> = (0..lines.len())
        .map(|i| {
            let c = leads[i]?;
            // A mark that says nothing as text is no range or minus: its
            // items may wrap over any number of lines hanging under them.
            let reach = if mark_glyph(c) {
                lines.len()
            } else {
                RUN_REACH
            };
            let x = lines[i].bbox.x0;
            // A line that can stand between two items: it hangs under the
            // item's text, or says a word or two.
            let between = |k: usize| {
                lines[k].bbox.x0 > x + RUN_ALIGN
                    || lines[k].text.split_whitespace().count() <= RUN_SEPARATOR_WORDS
            };
            let partner =
                |j: usize| leads[j] == Some(c) && (lines[j].bbox.x0 - x).abs() <= RUN_ALIGN;
            // Outward each way, as far as a line that could stand between.
            let up = (i.saturating_sub(reach)..i)
                .rev()
                .find(|&j| partner(j) || !between(j))
                .is_some_and(partner);
            let down = (i + 1..=(i + reach).min(lines.len() - 1))
                .find(|&j| partner(j) || !between(j))
                .is_some_and(partner);
            (up || down).then_some(c)
        })
        .collect();
    // Lines a marker pulled out above stands beside: each is an item of its
    // own, not the wrapped text of the item above it (see [`hangs_under`]).
    let led: Vec<bool> = lines
        .iter()
        .map(|l| markers.iter().any(|m| marker_beside(m, l)))
        .collect();
    // The note each line opens, a raised number in it or pulled out beside
    // it, with where the number stands.
    let note: Vec<Option<(u32, f64)>> = lines
        .iter()
        .map(|l| {
            raised_number(l).map(|n| (n, l.bbox.x0)).or_else(|| {
                markers
                    .iter()
                    .find(|m| marker_beside(m, l) && note_number(m, l))
                    .and_then(|m| Some((m.text.trim().parse().ok()?, m.bbox.x0)))
            })
        })
        .collect();
    let band: Vec<Option<usize>> = lines.iter().map(|l| band_of(l, bands)).collect();
    let mut marked: Vec<(usize, String)> = Vec::new();
    let mut start = 0;
    let mark = |i: usize| {
        if ticked[i] {
            Some(CHECKBOX_MARKER.to_string())
        } else if dotted[i] {
            Some(BULLET_MARKER.to_string())
        } else {
            runs[i].map(String::from)
        }
    };
    for (i, l) in lines.iter().enumerate() {
        if cur.is_empty() {
            if let Some(m) = mark(i) {
                marked.push((blocks.len(), m));
            }
            start = i;
            cur.push(l.clone());
            continue;
        }
        // A checkbox or a drawn bullet always begins a new item, however
        // the geometry reads, and a fill's edge is a row's edge.
        if mark(i).is_none()
            && band[i] == band[i - 1]
            && !set_back(&cur, l)
            && !counts_on_note(note[start], note[i], l.font_size)
            && (continues(cur.last().unwrap(), l, leading, &edge, !led[i])
                || runs_on(&cur, l, leading))
        {
            cur.push(l.clone());
        } else {
            let b = with_mark(build(std::mem::take(&mut cur)), &marked, blocks.len());
            blocks.extend(finish(b, &edge));
            if let Some(m) = mark(i) {
                marked.push((blocks.len(), m));
            }
            start = i;
            cur.push(l.clone());
        }
    }
    if !cur.is_empty() {
        let b = with_mark(build(cur), &marked, blocks.len());
        blocks.extend(finish(b, &edge));
    }
    for (i, m) in marked {
        if let Some(b) = blocks.get_mut(i) {
            b.marker = Some(m);
        }
    }

    let mut blocks = rejoin_broken_items(stack_display(blocks), &markers);
    count_on(&mut blocks);

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
    let mut placed: Vec<(Line, usize, bool)> = Vec::new();
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
        // A list marker stands to the *left* of what it introduces. One
        // sitting inside the text is an affiliation or footnote reference
        // annotating a word — an author line with `∗†` set above three
        // names mid-line read as a marker and turned the whole author
        // block into a list item. Vertical containment alone cannot tell
        // them apart.
        let Some(i) = idx else { continue };
        let first = &blocks[i].lines[0];
        if m.bbox.x1 <= blocks[i].bbox.x0 + MARKER_OUTDENT_TOLERANCE {
            placed.push((m, i, false));
        } else if note_number(&m, first) && m.bbox.x1 <= first.bbox.x0 + MARKER_OUTDENT_TOLERANCE {
            // A note's raised number stands left of the line it opens,
            // however far left a later line of its block starts; it is kept
            // only as the note's number.
            placed.push((m, i, true));
        }
    }
    // A footnote's number, raised at the margin before its text, is the
    // note's: the body's note marks refer to it. It goes back in front of the
    // text it was pulled from, not into a list, where the notes count: the
    // block before opens with the number one less, or the one after with the
    // one more. A chart's tick labels beside the text are raised as often,
    // and count by tens.
    let raised: Vec<Option<u32>> = placed
        .iter()
        .map(|(m, i, _)| {
            blocks[*i]
                .lines
                .first()
                .filter(|l| blocks[*i].marker.is_none() && note_number(m, l))
                .and_then(|_| m.text.trim().parse().ok())
        })
        .collect();
    let mut opens: Vec<Option<u32>> = blocks
        .iter()
        .map(|b| b.lines.first().and_then(raised_number))
        .collect();
    for ((_, i, _), n) in placed.iter().zip(&raised) {
        if n.is_some() {
            opens[*i] = *n;
        }
    }
    let counts = |i: usize, n: u32| {
        (i > 0 && n > 1 && opens[i - 1] == Some(n - 1)) || opens.get(i + 1) == Some(&Some(n + 1))
    };
    for ((m, i, only_note), n) in placed.into_iter().zip(raised) {
        if n.is_some_and(|n| counts(i, n)) {
            let b = &mut blocks[i];
            b.bbox = b.bbox.union(&m.bbox);
            // Raised, it opens the block as a footnote's mark: no section's
            // number.
            let mut m = m;
            m.opens_with_script = true;
            b.lines.insert(0, m);
            continue;
        }
        if !only_note && blocks[i].marker.is_none() {
            blocks[i].marker = Some(m.text.clone());
        }
    }
    blocks
}

/// `m`, a marker, stands just left of `l` on its row, as a bullet drawn
/// apart from its item's text does.
fn marker_beside(m: &Line, l: &Line) -> bool {
    let fs = (l.font_size as f64).max(1.0);
    let gap = l.bbox.x0 - m.bbox.x1;
    m.bbox.y1 > l.bbox.y0 && m.bbox.y0 < l.bbox.y1 && gap > -fs * 0.5 && gap < fs * CHECKBOX_REACH
}

/// `hang`: whether `next` may go on as the wrapped line of a bulleted item
/// above it, which a line with a marker of its own may not.
/// Size ratio from which text is display type beside the text next to it.
const DISPLAY_RATIO: f64 = 2.0;

/// Most blocks of another column that may stand between two stacked lines
/// of a display title in reading order.
const STACK_WINDOW: usize = 8;

/// Join the lines of a display title stacked beside a column of smaller
/// text.
///
/// Where the page is read as one column, a title set in its margin beside
/// the text is read line by line with the text: `Executive`, a line of the
/// text, another, `Summary`. Each title line was a block of its own, and
/// came out a heading of its own. Two blocks at one display size, one under
/// the other at their own leading, with only smaller text set beside them
/// in between, are one block, in the first's place, whatever their weight.
fn stack_display(mut blocks: Vec<Block>) -> Vec<Block> {
    let mut i = 0;
    while i < blocks.len() {
        let mut j = i + 1;
        while j < blocks.len() && j <= i + STACK_WINDOW {
            let (a, b) = (&blocks[i], &blocks[j]);
            let size = a.font_size.max(b.font_size).max(1.0) as f64;
            // Weight may change: a bold number over its title (`III.`
            // over `Regulatory cholesterol`).
            let same = (a.font_size - b.font_size).abs() as f64 / size <= FONT_SIZE_TOLERANCE;
            let shares_width = a.bbox.x1.min(b.bbox.x1) > a.bbox.x0.max(b.bbox.x0);
            let gap = b.bbox.y0 - a.bbox.y1;
            let stacked = gap > -size * 0.2 && gap <= size * 0.75;
            let between_beside = blocks[i + 1..j].iter().all(|m| {
                m.bbox.x1.min(a.bbox.x1.max(b.bbox.x1)) <= m.bbox.x0.max(a.bbox.x0.min(b.bbox.x0))
                    && (m.font_size as f64) * DISPLAY_RATIO <= size
            });
            // An item of a list is not a title's next line: a slide's
            // bullets at display size, one under the other.
            let item =
                b.marker.is_some() || b.text().trim_start().chars().next().is_some_and(is_bullet);
            if same && shares_width && stacked && between_beside && !item && j > i + 1 {
                let b = blocks.remove(j);
                let a = &mut blocks[i];
                a.lines.extend(b.lines);
                a.bbox = a.bbox.union(&b.bbox);
                continue;
            }
            // The next block of this title's size settles it either way.
            if same {
                break;
            }
            j += 1;
        }
        i += 1;
    }
    blocks
}

fn continues(prev: &Line, next: &Line, leading: f64, edge: &Edge, hang: bool) -> bool {
    if prev.rotation_bucket != next.rotation_bucket {
        return false;
    }
    // Different columns: no vertical relationship at all.
    if prev.bbox.x1.min(next.bbox.x1) - prev.bbox.x0.max(next.bbox.x0) <= 0.0 {
        return false;
    }
    let fs = prev.font_size.max(next.font_size).max(1.0) as f64;

    // A bullet set at the head of its line begins an item, however the
    // geometry reads: below a full line at body leading and the same indent,
    // the items of a list ran together into one paragraph.
    if strip_leading_bullet(&next.text).is_some() {
        return false;
    }
    // So does a note opened by its reference marks: `*** Zero waste…`
    // under the full last line of the `**` note before ran into it.
    if opens_note(&next.text) {
        return false;
    }

    // Weight change — a run-in heading. Very common in academic papers, where
    // a bold section title sits directly above body text *at the same size*, so
    // the font-size test below cannot see it. Measured case: `2 Depth
    // Up-Scaling` was absorbed into an 11-line paragraph and never reached
    // heading detection at all.
    //
    // A line opening on a bold label begins a paragraph the same way: the
    // label is the paragraph's run-in head (`PUMP: With stainless steel…`
    // below `…and long life.`), and the space between such paragraphs is
    // often too slight to part them.
    //
    // Unless the line before ran out of room. A heading ends its line where
    // it ends, so a line the next word did not fit on goes on into the next
    // whatever its weight: bold names in a sentence wrapped onto a line of
    // their own (`…Sweden, and the` over `United Kingdom.`), or a run-in
    // label wrapped onto a second line.
    let run_in = next.opens_bold && !next.bold;
    if (prev.bold != next.bold || run_in) && !edge.ran_out_of_room(prev, next) {
        return false;
    }

    // Font size change — a heading, or a shift to a caption.
    let rel = (prev.font_size - next.font_size).abs() as f64 / fs;
    if rel > FONT_SIZE_TOLERANCE {
        return false;
    }
    let full = |l: &Line| {
        (l.bbox.x1 - (edge.right - edge.span * (1.0 - FULL_LINE_FRACTION))) >= -f64::EPSILON
    };
    let prev_full = full(prev);
    if rel > SIZE_STEP && !(prev_full && full(next)) {
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
    if indent > INDENT_TOLERANCE
        && !prev_full
        && !(hang && hangs_under(prev, next, fs, edge))
        && !display_aligned(prev, next, fs, edge)
    {
        return false;
    }

    true
}

/// Most reference marks a note opens with: `****` is the fourth note.
const MAX_NOTE_MARKS: usize = 4;

/// Whether a line opens a note with its reference marks: a run of
/// asterisks, daggers or section signs, then the note's words, with a space
/// between or not (`* Due to…`, `**Energy use…`, `† Patent pending`).
fn opens_note(text: &str) -> bool {
    let t = text.trim_start();
    let marks = t.chars().take_while(|c| "*†‡§¶".contains(*c)).count();
    if marks == 0 || marks > MAX_NOTE_MARKS {
        return false;
    }
    let rest: &str = &t[t.char_indices().nth(marks).map_or(t.len(), |(i, _)| i)..];
    let words = rest.trim_start();
    words.chars().next().is_some_and(char::is_alphanumeric) && words.split_whitespace().count() >= 2
}

/// The size most of a page's prose lines are set in, across all its
/// columns: the measure display type is told by. One column can be all
/// display type once a table's rows leave it, so a column's own lines are
/// no measure, and a page's tables, set small and many to the page, are
/// none either.
pub fn body_size<'a>(lines: impl IntoIterator<Item = &'a Line>) -> f64 {
    let mut sizes: Vec<i64> = lines
        .into_iter()
        .map(|l| (l.font_size as f64 * 4.0).round() as i64)
        .collect();
    sizes.sort_unstable();
    sizes
        .chunk_by(|a, b| a == b)
        .max_by(|a, b| a.len().cmp(&b.len()).then(b[0].cmp(&a[0])))
        .map_or(0.0, |run| run[0] as f64 / 4.0)
}

/// Smallest size, against the page's body, of a display line.
const DISPLAY_LINE_RATIO: f64 = 1.3;

/// `prev` and `next` are lines of one display title set centred or
/// right-aligned: they start where their length puts them, so their left
/// edges differ by design, and neither reaches the column's edge, which the
/// body text sets. `PROCAT®` / `PROFESSIONAL` / `INSULATION` / `SYSTEM`
/// right-aligned at 28pt, or `Author's Note to the` / `2021 Edition`
/// centred at 24pt, are each one title, not a heading per line.
///
/// Display type only: body lines that happen to share a centre are a list
/// or an address ended by hand. And short lines only: a line that runs to
/// the column's edge is text wrapping there, which shares its right edge
/// with every line of a justified paragraph. A line that opens with a list
/// number is a slide's next item, not the title's next line.
fn display_aligned(prev: &Line, next: &Line, fs: f64, edge: &Edge) -> bool {
    if count_prefix(&next.text).is_some() {
        return false;
    }
    let display = |l: &Line| l.font_size as f64 >= edge.body * DISPLAY_LINE_RATIO;
    let short = |l: &Line| l.bbox.x1 < edge.right - fs;
    let centre = |l: &Line| (l.bbox.x0 + l.bbox.x1) / 2.0;
    display(prev)
        && display(next)
        && short(next)
        && ((centre(prev) - centre(next)).abs() <= fs * 0.5
            || (prev.bbox.x1 - next.bbox.x1).abs() <= fs * 0.5)
}

/// Farthest, in ems, a bulleted item's wrapped lines start right of its
/// bullet: the bullet, and the space or tab after it.
const HANGING_INDENT: f64 = 3.0;

/// `next` is the wrapped line of the item `prev` opens with a bullet set in
/// its text run: it starts right of the bullet, where the item's text does.
///
/// A list with a hanging indent sets each item's bullet out at the margin
/// and every line of its text in from it, so a wrapped line starts an em or
/// two right of the line it wraps from, and the bullet line is short of the
/// edge more often than not. Read as an indent, it was cut off the item and
/// came out a paragraph of its own: `Large tapswitch with a rocker-style
/// dimmer for a` and `standard designer wallplate opening.`
///
/// An item opened by a dash or an arrow hangs its wrapped lines as one
/// opened by a bullet does.
///
/// Not a line that opens a list of its own under the item: one opening with
/// a dash, or below a bullet line that ends on a colon (`UL Listed to
/// control:` over `— Dimmable LEDs`). Nor the paragraph after the list,
/// which a word processor often sets where the items' text is, or a
/// first-line indent in from the bullets: below an item that ends a
/// sentence short of the edge, a line that opens one goes on only if the
/// item ran out of room for its first word.
fn hangs_under(prev: &Line, next: &Line, fs: f64, edge: &Edge) -> bool {
    let shift = (next.bbox.x0 - prev.bbox.x0) / fs;
    let opens_list = next
        .text
        .trim_start()
        .starts_with(['-', '\u{2013}', '\u{2014}']);
    let ends_sentence = ends_sentence(prev);
    let opens_sentence = opens_sentence(next);
    // An item opened by a letter (`a. For 120/277/347V
    // connect the black fixture lead to the` over `voltage supply …`)
    // hangs its wrapped lines too, where its sentence plainly goes on, in
    // lower case: a heading over an indented paragraph does not.
    // Nor does a line opening an item of its own, a sub-item's `i.`.
    let enumerated = enumerated_item(&prev.text) && !opens_sentence && !opens_item(&next.text);
    (strip_leading_bullet(&prev.text).is_some() || run_lead(prev).is_some() || enumerated)
        && !prev.text.trim_end().ends_with(':')
        && !opens_list
        && shift > HANG_SHIFT
        && shift <= HANGING_INDENT
        && (!(ends_sentence && opens_sentence) || edge.ran_out_of_room(prev, next))
}

/// `text` opens with an item's letter: `a.`, `b)`, `(c)`. Numbered items
/// are counted and joined to their lines by passes of their own.
fn enumerated_item(text: &str) -> bool {
    let Some(first) = text.split_whitespace().next() else {
        return false;
    };
    let core = first.trim_start_matches('(');
    let Some(core) = core.strip_suffix(['.', ')']) else {
        return false;
    };
    core.chars().count() == 1 && core.chars().all(|c| c.is_ascii_alphabetic())
}

/// `text` opens with an item's letter, number or roman numeral: `a.`,
/// `(b)`, `12.`, `iv)`.
fn opens_item(text: &str) -> bool {
    let Some(first) = text.split_whitespace().next() else {
        return false;
    };
    let core = first.trim_start_matches('(');
    let Some(core) = core.strip_suffix(['.', ')']) else {
        return false;
    };
    let letter = core.chars().count() == 1 && core.chars().all(|c| c.is_ascii_alphabetic());
    let number = (1..=3).contains(&core.len()) && core.chars().all(|c| c.is_ascii_digit());
    let roman = (1..=4).contains(&core.len()) && core.chars().all(|c| matches!(c, 'i' | 'v' | 'x'));
    letter || number || roman
}

/// `l` ends on a sentence's full stop, or a question or exclamation mark,
/// inside any closing quote or bracket.
fn ends_sentence(l: &Line) -> bool {
    l.text
        .trim_end()
        .trim_end_matches(['"', '\'', '\u{201D}', '\u{2019}', ')'])
        .ends_with(['.', '!', '?'])
}

/// `l` opens as a sentence or a heading does: its first letter or digit a
/// capital or a digit.
fn opens_sentence(l: &Line) -> bool {
    l.text
        .trim_start()
        .chars()
        .find(|c| c.is_alphanumeric())
        .is_some_and(|c| c.is_uppercase() || c.is_numeric())
}

/// Most of the text's size a raised note number is set at.
const RAISED_RATIO: f64 = 0.85;

/// `m`, a number alone, is raised before `l` as a footnote's number is:
/// smaller than the text, its top at the text's or above, its foot well
/// above the text's. A list's number set small sits on the baseline.
fn note_number(m: &Line, l: &Line) -> bool {
    let fs = (l.font_size as f64).max(1.0);
    let t = m.text.trim();
    !t.is_empty()
        && t.len() <= 3
        && t.chars().all(|c| c.is_ascii_digit())
        && (m.font_size as f64) < fs * RAISED_RATIO
        && m.bbox.y0 <= l.bbox.y0 + 0.15 * fs
        && m.bbox.y1 < l.bbox.y1 - 0.15 * fs
}

/// The number a line opens with when it is raised, a footnote's: its
/// first glyph a superscript and its first word digits alone.
fn raised_number(l: &Line) -> Option<u32> {
    if !l.opens_with_script {
        return None;
    }
    let w = l.text.split_whitespace().next()?;
    (w.len() <= 3 && w.chars().all(|c| c.is_ascii_digit()))
        .then(|| w.parse().ok())
        .flatten()
}

/// A line opens the footnote after the one the block opens: both raised
/// numbers at the margin, in the line or pulled out beside it, the line's
/// one more. A block of notes set at body leading, `7 1 MB/s = …` then `8
/// Portion of buffer…`, is a note to a number, not one paragraph.
fn counts_on_note(opens: Option<(u32, f64)>, next: Option<(u32, f64)>, size: f32) -> bool {
    matches!(
        (opens, next),
        (Some((m, x)), Some((n, y))) if n == m + 1 && (y - x).abs() <= 0.5 * (size as f64).max(1.0)
    )
}

/// Is the block a list's item: marked, opening with a bullet, a dash or an
/// arrow set in its text, or with a marker the text layer set apart beside
/// its first line?
fn is_item(b: &Block, markers: &[Line]) -> bool {
    b.marker.is_some()
        || b.lines.first().is_some_and(|l| {
            strip_leading_bullet(&l.text).is_some()
                || run_lead(l).is_some()
                || markers.iter().any(|m| marker_beside(m, l))
        })
}

/// Put back an item a line was broken off by hand: `Lifetime guaranteed
/// recessed door handles.` over `Each door fitted with 12" (305 mm) long
/// recessed handle …`, set at the item's hanging indent, at its size and
/// leading, between it and the next item of the list. [`hangs_under`]
/// leaves a line opening a sentence under an item that ended one, for the
/// paragraph a word processor sets after a list where the items' text is;
/// one the list goes on after is the item's.
fn rejoin_broken_items(blocks: Vec<Block>, markers: &[Line]) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len());
    let mut rest = blocks.into_iter().peekable();
    while let Some(b) = rest.next() {
        let joins = out.last().is_some_and(|p: &Block| {
            let (Some(first), Some(last), Some(head)) =
                (p.lines.first(), p.lines.last(), b.lines.first())
            else {
                return false;
            };
            let fs = (p.font_size as f64).max(1.0);
            let shift = (head.bbox.x0 - first.bbox.x0) / fs;
            // Set where the item's bullet is: within the rounding of where
            // a list sets its marks, well short of a sub-list's indent.
            let level = |x: f64| (x - first.bbox.x0).abs() <= fs * ITEM_ALIGN;
            let next_item = rest.peek().is_some_and(|n| {
                is_item(n, markers) && n.lines.first().is_some_and(|l| level(l.bbox.x0))
            });
            // Not under an item ending on a colon, which a list of its own
            // follows, nor a line opening an item of its own: a number, a
            // letter, or the `o` and rings word processors set as a second
            // level's bullets (`o Class A`).
            let opens_own = opens_item(&head.text)
                || head
                    .text
                    .trim_start()
                    .starts_with(['o', '\u{25CB}', '\u{25E6}'])
                    && head.text.trim_start().chars().nth(1) == Some(' ');
            is_item(p, markers)
                && !is_item(&b, markers)
                && next_item
                && !last.text.trim_end().ends_with(':')
                && !opens_own
                && !head.bold
                // At the item's size and style, where one style sets both.
                && ((b.font_size - p.font_size).abs() as f64) <= fs * SAME_SIZE
                && shift > HANG_SHIFT
                && shift <= HANGING_INDENT
                && b.lines
                    .iter()
                    .all(|l| (l.bbox.x0 - head.bbox.x0).abs() <= fs * ITEM_ALIGN)
                // At the list's leading: an em at most between the lines, a
                // paragraph's space more.
                && head.bbox.y0 - last.bbox.y1 <= fs
        });
        if joins {
            let p = out.pop().expect("an item to join");
            let (marker, gap_above) = (p.marker.clone(), p.gap_above);
            let mut lines = p.lines;
            lines.extend(b.lines);
            let mut joined = build(lines);
            joined.marker = marker;
            joined.gap_above = gap_above;
            out.push(joined);
        } else {
            out.push(b);
        }
    }
    out
}

/// Widest difference, in ems, between where two items of one list, or two
/// lines of one hanging text, start: the rounding of where text is set,
/// well short of the em or more a list nested under an item is set in by.
const ITEM_ALIGN: f64 = 0.5;

/// Widest difference between two sizes one style sets, as a share of the
/// size: the rounding of a font's scale in the content stream, well short
/// of the step between a text's size and a heading's or a note's.
const SAME_SIZE: f64 = 0.05;

/// Least shift, in ems, of a line out of an item's hang: the margin
/// [`hangs_under`] takes a wrapped line in by.
const HANG_SHIFT: f64 = 0.3;

/// `next` is set back out of the bulleted item `cur`: the item's wrapped
/// lines hang under its text, its last ends a sentence, and `next` opens one
/// back at its bullet's x, or left of it, with no marker of its own. It is
/// the heading or paragraph after the list (`…food service marketplace.`
/// over `REFRIGERATION SYSTEM`), however near an em the hang is, not the
/// item's next line. A sentence that runs on is no item's end: a paragraph
/// after the list can open where the items' text is and wrap back to the
/// margin (`…(including a resident` over `alien), to provide…`).
fn set_back(cur: &[Line], next: &Line) -> bool {
    let [first, wrapped @ ..] = cur else {
        return false;
    };
    if wrapped.is_empty()
        || (strip_leading_bullet(&first.text).is_none() && run_lead(first).is_none())
    {
        return false;
    }
    let fs = first.font_size.max(next.font_size).max(1.0) as f64;
    let hang = wrapped.iter().map(|l| l.bbox.x0).fold(f64::MAX, f64::min);
    let last = cur.last().expect("an item and its wrapped lines");
    hang - first.bbox.x0 > HANG_SHIFT * fs
        && next.bbox.x0 <= first.bbox.x0 + HANG_SHIFT * fs
        && ends_sentence(last)
        && opens_sentence(next)
}

/// The right edge a column's text wraps at.
struct Edge {
    /// The page's body size, or the column's where none is given.
    body: f64,
    right: f64,
    /// From the column's left edge to `right`.
    span: f64,
    /// Two lines running at least half way across end at `right`, within
    /// an em: it is where the text wraps, not only where the widest line
    /// happens to end.
    agreed: bool,
}

impl Edge {
    /// The rightmost end another long line also reaches. One line standing
    /// out past the rest (a centred notice under two columns, kept with the
    /// first) is not the measure, and taking it made no line of the column
    /// full. Where no two long lines agree, the widest line is.
    fn of(lines: &[Line], body: f64) -> Edge {
        let left = lines.iter().map(|l| l.bbox.x0).fold(f64::MAX, f64::min);
        let mut ends: Vec<(f64, f64)> = lines
            .iter()
            .map(|l| (l.bbox.x1, l.font_size.max(1.0) as f64))
            .collect();
        ends.sort_by(|a, b| b.0.total_cmp(&a.0));
        let widest = ends.first().map_or(f64::MIN, |e| e.0);
        let half = left + (widest - left) / 2.0;
        let agreed = ends
            .windows(2)
            .find(|w| w[1].0 >= half && w[0].0 - w[1].0 <= w[0].1.max(w[1].1))
            .map(|w| w[0].0);
        let right = agreed.unwrap_or(widest);
        let mut sizes: Vec<i64> = lines
            .iter()
            .map(|l| (l.font_size as f64 * 4.0).round() as i64)
            .collect();
        sizes.sort_unstable();
        let own = sizes
            .chunk_by(|a, b| a == b)
            .max_by(|a, b| a.len().cmp(&b.len()).then(b[0].cmp(&a[0])))
            .map_or(0.0, |run| run[0] as f64 / 4.0);
        let body = if body > 0.0 { body } else { own };
        Edge {
            body,
            right,
            span: (right - left).max(1.0),
            agreed: agreed.is_some(),
        }
    }

    /// `prev` was broken because the first word of `next` did not fit after
    /// it: it breaks a word with a hyphen, or ends short of the edge by less
    /// than that word, the test [`split_hard_breaks`] makes the other way
    /// round.
    ///
    /// Never against an edge no other line agrees on: the widest line is
    /// short of its own end by nothing, whatever follows it. And never for a
    /// line a dot leader carries to the edge (a contents entry and its
    /// page), which reaches it by design.
    fn ran_out_of_room(&self, prev: &Line, next: &Line) -> bool {
        if has_leader(&prev.text) {
            return false;
        }
        // A wrapped line goes on at its column's left edge, at most a
        // first-line indent left of the line it wraps from. One starting far
        // to the left is in another column: a heading the line pass ran on
        // across the gutter, below a full line of the column beside it.
        // Centred and right-aligned lines start where their length puts
        // them.
        let fs = prev.font_size.max(next.font_size).max(1.0) as f64;
        let centre = |l: &Line| (l.bbox.x0 + l.bbox.x1) / 2.0;
        if prev.bbox.x0 - next.bbox.x0 > fs * WRAP_OUTDENT
            && (centre(prev) - centre(next)).abs() > fs * 2.0
            && (prev.bbox.x1 - next.bbox.x1).abs() > fs
        {
            return false;
        }
        let mut end = prev.text.chars().rev();
        let breaks_word = matches!(end.next(), Some('-' | '\u{AD}'))
            && end.next().is_some_and(char::is_alphabetic);
        breaks_word || (self.agreed && self.right - prev.bbox.x1 < first_word_width(next))
    }
}

/// The furthest, in ems, a wrapped line goes on left of the line above it:
/// a first-line indent.
const WRAP_OUTDENT: f64 = 4.0;

/// Holds a dot leader: four or more dots in a run, spaced or not.
fn has_leader(text: &str) -> bool {
    let mut dots = 0;
    for c in text.chars() {
        match c {
            '.' | '·' | '․' => dots += 1,
            '…' => dots += 3,
            ' ' => {}
            _ => dots = 0,
        }
        if dots >= 4 {
            return true;
        }
    }
    false
}

/// Width of a line's first word plus the space before it, estimated from the
/// line's own average character width.
fn first_word_width(l: &Line) -> f64 {
    let chars = l.text.chars().count().max(1) as f64;
    let first = l
        .text
        .split_whitespace()
        .next()
        .map_or(0, |w| w.chars().count()) as f64;
    l.bbox.width() / chars * (first + 1.0)
}

/// The ordinal a line opens with — `1.`, `12)` — as an ordered list sets its
/// items, with the label's text.
fn ordinal_prefix(text: &str) -> Option<(u32, &str)> {
    let t = text.trim_start();
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = t[digits..].strip_prefix(['.', ')'])?;
    if !rest.starts_with([' ', '\t', '\u{00A0}']) {
        return None;
    }
    let label = &t[..t.len() - rest.len()];
    Some((t[..digits].parse().ok()?, label))
}

/// Where a lettered label's place in its count starts: `a.` is
/// `LETTERED + 1`. Letters count apart from numbers, so `b.` follows `a.`
/// and never `1.`.
const LETTERED: u32 = 1000;

/// The lower-case letter a line opens with as an ordered list's label,
/// `a.`, `b)`, `(c)`, with its place in the count (see [`LETTERED`]) and
/// the label's text. A capital, `A.`, opens an appendix or a section as
/// often as an item, and is no label here.
fn letter_prefix(text: &str) -> Option<(u32, &str)> {
    let t = text.trim_start();
    let body = t.strip_prefix('(');
    let inner = body.unwrap_or(t);
    let c = inner.chars().next().filter(char::is_ascii_lowercase)?;
    let after = &inner[1..];
    let rest = match body {
        Some(_) => after.strip_prefix(')')?,
        None => after.strip_prefix(['.', ')'])?,
    };
    if !rest.starts_with([' ', '\t', '\u{00A0}']) {
        return None;
    }
    let label = &t[..t.len() - rest.len()];
    Some((LETTERED + (c as u32 - 'a' as u32) + 1, label))
}

/// Does a count go on from place `m`? A lettered one opens at `a.`, or
/// carries on from a line already counted: lines that happen to open `(d)`
/// and `(e)`, a caption's panels wrapped one to a line, count off from
/// nothing.
fn opens_or_carries(m: u32, carried: bool) -> bool {
    m < LETTERED || m == LETTERED + 1 || carried
}

/// The label a line opens with as an ordered list's item, numbered or
/// lettered: `1.`, `12)`, `a.`, `(b)`.
fn count_prefix(text: &str) -> Option<(u32, &str)> {
    ordinal_prefix(text).or_else(|| letter_prefix(text))
}

/// The block `at` with the marker that leads it, if a checkbox, a drawn
/// bullet or a run of dashes does: an item, before it is parted, so the
/// passes that part blocks see it as the item it is, as they see one a
/// typed bullet leads. Its wrapped lines are its own: `Distributed file
/// systems, including` over `Hadoop and Ceph`, beside a drawn dot, was cut
/// in two as a list of entries set one per line.
fn with_mark(mut b: Block, marked: &[(usize, String)], at: usize) -> Block {
    if let Some((_, m)) = marked.last().filter(|(i, _)| *i == at) {
        b.marker = Some(m.clone());
    }
    b
}

/// A block as assembled, parted where its lines were ended by hand: one
/// field or entry per line ([`split_hard_breaks`], [`split_entries`]), one
/// numbered item per line run ([`split_counted`]).
fn finish(b: Block, edge: &Edge) -> Vec<Block> {
    split_hard_breaks(b)
        .into_iter()
        .flat_map(split_counted)
        .flat_map(|b| split_ended(b, edge))
        .flat_map(|b| split_entries(b, edge))
        .flat_map(|b| split_stacked_heads(b, edge))
        .collect()
}

/// Part a block before each line its paragraph was ended by hand ahead of;
/// see [`ended_by_hand`].
fn split_ended(b: Block, edge: &Edge) -> Vec<Block> {
    if b.lines.len() < 2 || !edge.agreed {
        return vec![b];
    }
    let cuts: Vec<usize> = {
        let mut cuts = Vec::new();
        let mut reach = b.lines[0].bbox.x1;
        for i in 1..b.lines.len() {
            if ended_by_hand(&b.lines[i - 1], reach, &b.lines[i], edge) {
                cuts.push(i);
                reach = b.lines[i].bbox.x1;
            } else {
                reach = reach.max(b.lines[i].bbox.x1);
            }
        }
        cuts
    };
    if cuts.is_empty() {
        return vec![b];
    }
    let Block {
        lines,
        marker,
        gap_above,
        ..
    } = b;
    let mut out: Vec<Block> = Vec::new();
    let mut cur: Vec<Line> = Vec::new();
    for (i, l) in lines.into_iter().enumerate() {
        if cuts.contains(&i) {
            out.push(build(std::mem::take(&mut cur)));
        }
        cur.push(l);
    }
    out.push(build(cur));
    out[0].marker = marker;
    out[0].gap_above = gap_above;
    out
}

/// Does the sentence plainly run on from `cur`'s last line into `next`,
/// across a step in the leading or a weight change? The line is full
/// against the lines at hand and ends without terminal punctuation, and
/// either
///
/// - the next opens in lower case: `…to see improved` over `performance in
///   4Kn fomat.`, in footnotes set at twice their size's leading. Under an
///   item whose lines hang under its first, a line set where they are goes
///   on whether or not the line above is full: a list's `or` between two of
///   its items, set in another weight; or
/// - the break falls inside a run of capitals the weight change belongs to:
///   `…lead dust. LEAD IS` over the bold `TOXIC. EXPOSURE…`, in a warning
///   set narrower than its column, whose measure says nothing of where the
///   warning wraps. A heading in bold capitals stands alone, with no line
///   of its paragraph above it, and its text opens in lower and upper case.
///
/// Only at one size, from one left edge, one line under the other within a
/// paragraph's spacing, a quarter again for a lower-case continuation; a
/// bullet or a number opening the next line begins an item whatever its
/// sentence does, and a contents entry's leader runs to its page number.
fn runs_on(cur: &[Line], next: &Line, leading: f64) -> bool {
    let Some(prev) = cur.last() else {
        return false;
    };
    let fs = prev.font_size.max(next.font_size).max(1.0) as f64;
    let dy = (next.bbox.y0 - prev.bbox.y0) / fs;
    if prev.rotation_bucket != next.rotation_bucket
        || prev.bbox.x1.min(next.bbox.x1) - prev.bbox.x0.max(next.bbox.x0) <= 0.0
        || (prev.font_size - next.font_size).abs() as f64 / fs > FONT_SIZE_TOLERANCE
        || (prev.bbox.x0 - next.bbox.x0).abs() > fs * INDENT_TOLERANCE
        || dy <= 0.0
        || strip_leading_bullet(&next.text).is_some()
        || ordinal_prefix(&next.text).is_some()
        || has_leader(&prev.text)
        || prev.text.trim_end().ends_with(['.', '!', '?', ':', ';'])
    {
        return false;
    }
    let first = next
        .text
        .trim_start()
        .trim_start_matches(['(', '[', '"', '\'', '“', '‘']);
    let lower = first.chars().next().is_some_and(char::is_lowercase);
    // A line hanging under an item, where the item's lines after its
    // first are set: a list's `or` between two of its items, under an
    // item of two lines whose second is the shorter.
    let hangs = cur.len() >= 2
        && cur[0].bbox.x0 + fs * 0.5 < prev.bbox.x0
        && cur[1..]
            .iter()
            .all(|l| (l.bbox.x0 - prev.bbox.x0).abs() <= fs * INDENT_TOLERANCE);
    let reach = cur.iter().map(|l| l.bbox.x1).fold(next.bbox.x1, f64::max);
    if reach - prev.bbox.x1 >= first_word_width(next) && !(lower && hangs) {
        return false;
    }
    if lower {
        return dy <= leading * PARAGRAPH_FACTOR * RUN_ON_LEADING;
    }
    let capitals = |w: Option<&str>| {
        w.is_some_and(|w| {
            let letters: Vec<char> = w.chars().filter(|c| c.is_alphabetic()).collect();
            letters.len() >= 2 && letters.iter().all(|c| c.is_uppercase())
        })
    };
    prev.bold != next.bold
        && cur.len() >= 2
        && dy <= leading * PARAGRAPH_FACTOR
        && capitals(prev.text.split_whitespace().last())
        && capitals(first.split_whitespace().next())
}

/// How far past a paragraph's spacing a sentence that plainly runs on may
/// still go on; see [`runs_on`]. A blank line's worth, twice the leading,
/// parts paragraphs whatever their sentences do.
const RUN_ON_LEADING: f64 = 1.25;

/// Does `next` open with a capital letter, past an opening bracket or
/// quote: a new sentence, a new entry?
fn opens_capital(next: &Line) -> bool {
    next.text
        .trim_start()
        .trim_start_matches(['(', '[', '"', '\'', '“', '‘'])
        .chars()
        .next()
        .is_some_and(char::is_uppercase)
}

/// Was a paragraph ended by hand at `prev`, before `next`, at the same
/// leading? Its last line stops short of where its lines and the next one
/// reach by more than the next line's first word and [`BY_HAND_ROOM`], and
/// the next opens with a capital: a typesetter wrapping the text would have
/// set that word there.
/// A paragraph's last line before the next paragraph, `…lo antes posible.`
/// over `El aislamiento ProCat®…`, or an entry before a longer one.
///
/// Measured against the lines at hand, not the column: text set narrower
/// than its column, in a box or beside a picture, wraps at its own edge.
/// Only where the column's text shows a wrap margin at all: lines with no
/// text around them, a title broken by hand, are left whole.
/// Display type, a weight change, a hyphen or a comma at the line's end,
/// and lines that do not start at one left edge are left to the other
/// tests.
///
/// `reach` is where the paragraph's lines up to `prev` reach.
fn ended_by_hand(prev: &Line, reach: f64, next: &Line, edge: &Edge) -> bool {
    let fs = prev.font_size.max(next.font_size).max(1.0) as f64;
    if prev.font_size as f64 >= edge.body * DISPLAY_LINE_RATIO
        || next.font_size as f64 >= edge.body * DISPLAY_LINE_RATIO
        || prev.bold != next.bold
        || (prev.bbox.x0 - next.bbox.x0).abs() > fs
        || prev.text.trim_end().ends_with(['-', '\u{AD}', ','])
        || !opens_capital(next)
    {
        return false;
    }
    reach.max(next.bbox.x1) - prev.bbox.x1 > first_word_width(next) + fs * BY_HAND_ROOM
}

/// Room, in ems past the next line's first word, a line must leave to have
/// been ended by hand. Typesetters that break a paragraph's lines together,
/// and designers setting a rag, end a line a word short of where a greedy
/// break would; a line ended by hand stops well before.
const BY_HAND_ROOM: f64 = 2.0;

/// Split a block of entries set one per line, `ASME A112.18.1/CSA B125.1`
/// over `NSF/ANSI/CAN 372`, into one block per line.
///
/// Entries of about one length agree on a right edge of their own, so the
/// test [`split_hard_breaks`] makes against the block's edge sees no line
/// end early. Against the column they all do: each stops short of the
/// measure the column's text wraps at by more than the next line's first
/// word, which a typesetter wrapping the text would have set there, and
/// none goes on in lower case, as a wrapped line of a narrow box's text
/// does. Display type and bold lines, a stacked title, are left whole.
fn split_entries(b: Block, edge: &Edge) -> Vec<Block> {
    if b.lines.len() < 2 || !edge.agreed || b.marker.is_some() {
        return vec![b];
    }
    let fs = (b.font_size as f64).max(1.0);
    let left = b.lines.iter().map(|l| l.bbox.x0).fold(f64::MAX, f64::min);
    let set = b.lines.iter().all(|l| {
        l.bbox.x0 - left <= fs
            && !l.bold
            && (l.font_size as f64) < edge.body * DISPLAY_LINE_RATIO
            && !l.text.trim_end().ends_with(['-', '\u{AD}', ','])
    });
    // An entry ends its line; a line ending on a word its phrase runs on
    // from (`…since the` over `April 2024 Financial Stability Report…`)
    // ends no entry. In lower case, as a sentence sets it: a capital `A`
    // ending an entry is its label's (`…with Part A`).
    let runs_on = |l: &Line| {
        l.text
            .split_whitespace()
            .next_back()
            .is_some_and(|w| RUNS_ON.contains(&w))
    };
    let ended = b.lines.windows(2).all(|w| {
        opens_capital(&w[1])
            && !runs_on(&w[0])
            && edge.right - w[0].bbox.x1 > first_word_width(&w[1]) + fs * 0.5
    });
    if !set || !ended {
        return vec![b];
    }
    let Block {
        lines, gap_above, ..
    } = b;
    let mut out: Vec<Block> = lines.into_iter().map(|l| build(vec![l])).collect();
    out[0].gap_above = gap_above;
    out
}

/// Words a wrapped line may end on, or its next line open with: the phrase
/// runs on across the break.
const RUNS_ON: &[&str] = &[
    "a", "an", "the", "and", "or", "nor", "but", "not", "of", "to", "in", "on", "at", "by", "for",
    "from", "with", "as", "into", "via", "among", "between", "through", "within", "without",
    "across", "over", "under", "after", "before", "during", "per", "versus", "vs", "about",
    "against", "toward", "towards", "upon", "my", "your", "our", "their", "its", "his", "her",
];

/// Part two bold lines set one over the other, each a heading of its own:
/// `Materials and methods` over `Animals`, `V - Etiology` over `A.
/// Genetics`. Entries are parted the same way ([`split_entries`]), but bold
/// lines are left whole there, as a title broken over two lines must be.
/// Here the break is no wrap: both lines are bold, at one size from the
/// body's up to display type (a diagram's labels are set smaller), set at
/// one indent; the second opens with a capital, not a bracket (`Hours:
/// 8:30 am - 6:00 pm EST` over `(Monday - Friday)`), and its first word
/// would have fitted on the first. The phrase does not run
/// on across the break (`RIGHT OF FIRST REFUSAL` over `AND CO-SALE
/// AGREEMENT`, `Where's My` over `Refund?`), and the second line is no
/// dateline (`Notes to Consolidated Financial Statements` over `December
/// 31, 2019`). Three or more lines are a title's: a company, a statement
/// and its date.
fn split_stacked_heads(b: Block, edge: &Edge) -> Vec<Block> {
    if b.lines.len() != 2 || !edge.agreed || b.marker.is_some() {
        return vec![b];
    }
    let fs = (b.font_size as f64).max(1.0);
    let (first, second) = (&b.lines[0], &b.lines[1]);
    let set = [first, second].iter().all(|l| {
        l.bold
            && (l.font_size - first.font_size).abs() <= 0.25
            && (l.font_size as f64) >= edge.body - 0.25
            && (l.font_size as f64) < edge.body * DISPLAY_LINE_RATIO
            && (l.bbox.x0 - first.bbox.x0).abs() <= fs * 0.5
    });
    let runs_on = |w: Option<&str>| w.is_some_and(|w| RUNS_ON.contains(&w.to_lowercase().as_str()));
    let ends_open = first
        .text
        .trim_end()
        .ends_with(['-', '\u{AD}', ',', ':', ';', '&', '/'])
        || runs_on(first.text.split_whitespace().next_back());
    let dateline = second
        .text
        .split(|c: char| !c.is_ascii_digit())
        .any(|n| n.len() == 4 && (n.starts_with("19") || n.starts_with("20")));
    let bracketed = second.text.trim_start().starts_with(['(', '[']);
    let stacked = opens_capital(second)
        && !bracketed
        && !ends_open
        && !runs_on(second.text.split_whitespace().next())
        && !dateline
        && edge.right - first.bbox.x1 > first_word_width(second) + fs * 0.5;
    if !set || !stacked {
        return vec![b];
    }
    let Block {
        lines, gap_above, ..
    } = b;
    let mut out: Vec<Block> = lines.into_iter().map(|l| build(vec![l])).collect();
    out[0].gap_above = gap_above;
    out
}

/// Carry a count across blocks: a block opening with `n.` set where the
/// item `n−1.` before it opens is that list's next item, and a block
/// opening with `n−1.` set where the item `n.` after it opens is the one
/// before. A list whose items stand a little further apart than its lines
/// parts into blocks between some items and not others.
///
/// A lettered list opens itself: a block opening with `a.` and the block
/// set where it is under it opening with `b.` are its first two items,
/// each a paragraph long or a step wrapped over lines. A lone `a.` is a
/// paragraph's. Numbers wait for [`gather_counted_items`], which tells a
/// list from numbered sections.
fn count_on(blocks: &mut [Block]) {
    let ordinal = |b: &Block| {
        b.lines
            .first()
            .and_then(|l| count_prefix(&l.text))
            .map(|(n, label)| (n, label.to_string()))
    };
    let item = |b: &Block| {
        let o = ordinal(b)?;
        (b.marker.as_deref() == Some(o.1.as_str())).then_some(o.0)
    };
    let beside = |a: &Block, z: &Block| {
        let fs = (a.font_size.max(z.font_size) as f64).max(1.0);
        (a.bbox.x0 - z.bbox.x0).abs() <= fs * 0.5
            && (a.font_size - z.font_size).abs() <= 0.5
            && z.bbox.y0 > a.bbox.y0
    };
    loop {
        let mut changed = false;
        for k in 1..blocks.len() {
            let (a, z) = (&blocks[k - 1], &blocks[k]);
            if !beside(a, z) {
                continue;
            }
            if let (Some(m), None, Some((n, label))) = (item(a), z.marker.as_ref(), ordinal(z)) {
                if n == m + 1 {
                    blocks[k].marker = Some(label);
                    changed = true;
                    continue;
                }
            }
            let (a, z) = (&blocks[k - 1], &blocks[k]);
            if let (None, Some(n), Some((m, label))) = (a.marker.as_ref(), item(z), ordinal(a)) {
                if n == m + 1 {
                    blocks[k - 1].marker = Some(label);
                    changed = true;
                    continue;
                }
            }
            let (a, z) = (&blocks[k - 1], &blocks[k]);
            if let (None, None, Some((m, first)), Some((n, second))) =
                (a.marker.as_ref(), z.marker.as_ref(), ordinal(a), ordinal(z))
            {
                if m == LETTERED + 1 && n == m + 1 {
                    blocks[k - 1].marker = Some(first);
                    blocks[k].marker = Some(second);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// Split a block where its lines count off, `1. Avoid exposure…` and,
/// after a line or two hanging under it, `2. Avoid exposure…` set where the
/// `1.` is, or `a.` over `b.`: an ordered list's items printed one after
/// another at the paragraph's leading. Each starts its own block and
/// carries its label as its marker, as [`split_hard_breaks`] marks a
/// hand-broken list's steps; the lines it wraps onto hang under it and stay
/// with it. Text before the first item is a block of its own, and so is a
/// line set left of the last item's label that opens a count of its own,
/// where the list the items sit under goes on (`f. The depth of focus…`
/// over `2. When changing…`). A plain line set left of it is the item's,
/// wrapped back to the margin its first line was indented from.
fn split_counted(b: Block) -> Vec<Block> {
    if b.lines.len() < 2 {
        return vec![b];
    }
    let fs = (b.font_size as f64).max(1.0);
    let ordinals: Vec<Option<(u32, String)>> = b
        .lines
        .iter()
        .map(|l| count_prefix(&l.text).map(|(n, label)| (n, label.to_string())))
        .collect();
    let mut counted: Vec<Option<String>> = vec![None; b.lines.len()];
    // The last line of each count, numbers and letters: a lettered step set
    // under `2.` leaves the count `3.` goes on from.
    let mut prev: [Option<(usize, u32)>; 2] = [None; 2];
    for (i, o) in ordinals.iter().enumerate() {
        let Some((n, label)) = o else { continue };
        let kind = usize::from(*n > LETTERED);
        if let Some((j, m)) = prev[kind] {
            let (a, z) = (&b.lines[j], &b.lines[i]);
            if *n == m + 1
                && opens_or_carries(m, counted[j].is_some())
                && (a.bbox.x0 - z.bbox.x0).abs() <= fs * 0.5
                && b.lines[j + 1..i]
                    .iter()
                    .all(|l| l.bbox.x0 >= a.bbox.x0 - fs * 0.5)
            {
                counted[i] = Some(label.clone());
                if counted[j].is_none() {
                    counted[j] = ordinals[j].as_ref().map(|(_, l)| l.clone());
                }
            }
        }
        prev[kind] = Some((i, *n));
    }
    if counted.iter().all(Option::is_none) {
        return vec![b];
    }
    let Block {
        lines,
        marker,
        gap_above,
        ..
    } = b;
    let mut out: Vec<Block> = Vec::new();
    let mut cur: Vec<Line> = Vec::new();
    let mut label: Option<String> = None;
    let mut item_left: Option<f64> = None;
    for (i, l) in lines.into_iter().enumerate() {
        let outdented =
            ordinals[i].is_some() && item_left.is_some_and(|x| l.bbox.x0 < x - fs * 0.5);
        if (counted[i].is_some() || outdented) && !cur.is_empty() {
            let mut piece = build(std::mem::take(&mut cur));
            piece.marker = label.take();
            out.push(piece);
        }
        if counted[i].is_some() {
            label = counted[i].clone();
            item_left = Some(l.bbox.x0);
        } else if outdented {
            item_left = None;
        }
        cur.push(l);
    }
    let mut piece = build(cur);
    piece.marker = label;
    out.push(piece);
    if out[0].marker.is_none() {
        out[0].marker = marker;
    }
    out[0].gap_above = gap_above;
    out
}

/// Split a block whose lines were ended by hand into one block per line.
///
/// A checklist sets one field per line — `Site access: [X] Badge`, `Visitor
/// log signed: [ ] Yes [X] No.` — at body leading with nothing between the
/// lines, and a page of fields read as one paragraph a consumer could not
/// take apart. The spacing says paragraph; the right edge
/// says otherwise. A wrapping typesetter ends a line only when the next word
/// does not fit, so in a wrapped paragraph no line but the last leaves room
/// for the word that follows it. Where most lines do, the breaks were put
/// there: a form, an address, a poem, a listing.
///
/// Room is measured against the block's own right edge, which is never
/// wider than the margin the text wrapped at, so the test errs toward
/// keeping a paragraph whole. Centred and hanging-indented text, whose left
/// edges vary, is left alone, and so is a block of fewer than
/// [`MIN_HARD_BREAK_LINES`].
///
/// Lines of such a block that count off in sequence — `1. Label four
/// plastic bags`, `2. Weigh 20 g of air-dry soil` — are an ordered list's
/// items: each starts its own block whether or not the line above it left
/// room, and carries its label as the marker. A step read as a block of its
/// own would otherwise pass for a numbered section, which is the one thing a
/// line that counts off from the line above it cannot be.
pub fn split_hard_breaks(b: Block) -> Vec<Block> {
    if b.lines.len() < MIN_HARD_BREAK_LINES {
        return vec![b];
    }
    let fs = (b.font_size as f64).max(1.0);
    let left = b.lines.iter().map(|l| l.bbox.x0).fold(f64::MAX, f64::min);
    if b.lines.iter().any(|l| l.bbox.x0 - left > fs) {
        return vec![b];
    }
    let right = b.lines.iter().map(|l| l.bbox.x1).fold(f64::MIN, f64::max);
    let ends_early: Vec<bool> = b
        .lines
        .windows(2)
        .map(|w| {
            let (cur, next) = (&w[0], &w[1]);
            // A hyphen at the end is a wrap by definition.
            !cur.text.ends_with('-') && right - cur.bbox.x1 >= first_word_width(next)
        })
        .collect();
    let early = ends_early.iter().filter(|e| **e).count();
    if early * 2 <= ends_early.len() {
        return vec![b];
    }
    // Lines counting off from the one before: `Some(label)` where the line
    // continues a sequence, or opens one that the next numbered line
    // continues.
    let ordinals: Vec<Option<(u32, String)>> = b
        .lines
        .iter()
        .map(|l| count_prefix(&l.text).map(|(n, label)| (n, label.to_string())))
        .collect();
    let mut counted: Vec<Option<String>> = vec![None; b.lines.len()];
    // The last line of each count, numbers and letters: a lettered step set
    // under `2.` leaves the count `3.` goes on from.
    let mut prev: [Option<(usize, u32)>; 2] = [None; 2];
    for (i, o) in ordinals.iter().enumerate() {
        if let Some((n, label)) = o {
            let kind = usize::from(*n > LETTERED);
            if let Some((j, m)) = prev[kind] {
                if *n == m + 1 && opens_or_carries(m, counted[j].is_some()) {
                    counted[i] = Some(label.clone());
                    if counted[j].is_none() {
                        counted[j] = ordinals[j].as_ref().map(|(_, l)| l.clone());
                    }
                }
            }
            prev[kind] = Some((i, *n));
        }
    }
    let Block {
        lines,
        marker,
        gap_above,
        ..
    } = b;
    let mut out: Vec<Block> = Vec::new();
    let mut cur: Vec<Line> = Vec::new();
    let n = lines.len();
    for (i, l) in lines.into_iter().enumerate() {
        cur.push(l);
        let breaks = ends_early.get(i).copied().unwrap_or(false)
            || counted.get(i + 1).is_some_and(|c| c.is_some());
        if breaks || i + 1 == n {
            let start = i + 1 - cur.len();
            let mut piece = build(std::mem::take(&mut cur));
            piece.marker = counted[start].clone();
            out.push(piece);
        }
    }
    if out[0].marker.is_none() {
        out[0].marker = marker;
    }
    out[0].gap_above = gap_above;
    out
}

/// Fewest items that make a numbered list of a page's blocks. Two numbered
/// lines in a row are as often two short sections (`5. The dynamics`,
/// `6. Modeling the dynamics`) as a list.
const MIN_COUNTED_ITEMS: usize = 3;

/// Most blocks set under one numbered item before the next one, and most
/// lines among them: a title's description, a translation, a line the item
/// wrapped onto. A section's worth of text under the number is a section.
const MAX_ITEM_BODY_BLOCKS: usize = 3;
const MAX_ITEM_BODY_LINES: usize = 6;

/// Widest gap, in ems, between a list's item and the text above it.
const MAX_ITEM_GAP: f64 = 2.5;

/// Most items of a list nested under a numbered item.
const MAX_NESTED_ITEMS: usize = 12;

/// Gather each page's numbered items, each with the text set under it, into
/// one block per item: an ordered list's items.
///
/// A feature list sets each item's number in the item's own text run, `2.
/// PostureFit SL®` over a line or two of description, so the marker pass
/// has nothing to pull out and each title reads as a block of its own. Those
/// blocks pass for numbered sections, so a list of six made six sections,
/// the last of them holding everything after the list. A list is told from
/// numbered sections by what the document sets around the numbers:
///
/// - the numbers count off from one, `1.`, `2.`, `3.`, at least
///   [`MIN_COUNTED_ITEMS`] of them, each at the left edge of the one before.
///   A run carrying on where a run on the same page stopped, as a list
///   continued in the next column does, is the same list;
/// - under each number is at most a few short blocks, set no larger than the
///   item, within a line of it and not left of it, and none opening with a
///   number of its own, as a section's clauses do (`1.1 "Services" means…`);
/// - the numbering stops with the list. A document whose next numbered line
///   carries on the count, on this page or a later one, is numbering its
///   sections (`4. Term and Renewal`, then `5. Termination` overleaf);
/// - the list sits in a section: `in_section(page, block)` says a heading
///   other than the document's title comes before its first item. Numbers
///   straight under the title are the document's own sections (`1. SCOPE OF
///   SERVICES`, `2. TERM`, `3. FEES`), however short.
///
/// A block the outline names is a heading the author declared, and is never
/// an item. Returns whether any items were gathered.
///
/// Each item keeps its number in its text and carries it as its marker, as
/// [`split_hard_breaks`] marks a hand-broken list's steps. The last item
/// takes as many blocks as the items before it took, and no more, so the
/// paragraph after a list is not read into its last item.
pub fn gather_counted_items(
    pages: &mut [Vec<Block>],
    named: &dyn Fn(&str) -> bool,
    in_section: &dyn Fn(usize, usize) -> bool,
) -> bool {
    let ordinals: Vec<Vec<Option<u32>>> = pages
        .iter()
        .map(|blocks| {
            blocks
                .iter()
                .map(|b| {
                    if b.marker.is_some() || b.lines.len() > MAX_ITEM_BODY_LINES {
                        return None;
                    }
                    b.lines
                        .first()
                        .and_then(|l| ordinal_prefix(&l.text))
                        .map(|(n, _)| n)
                        .filter(|_| !named(&b.text()))
                })
                .collect()
        })
        .collect();
    // The ordinal of the first numbered block after `(page, block)`, in
    // reading order through the rest of the document.
    let next_ordinal = |page: usize, block: usize| -> Option<u32> {
        let rest = ordinals[page][block + 1..].iter();
        rest.chain(ordinals[page + 1..].iter().flatten())
            .find_map(|o| *o)
    };
    let mut gathered = false;
    for (pi, blocks) in pages.iter_mut().enumerate() {
        let counted = counted_items(
            blocks,
            &ordinals[pi],
            |b| next_ordinal(pi, b),
            |b| in_section(pi, b),
        );
        if counted.iter().any(Option::is_some) {
            let taken = std::mem::take(blocks);
            *blocks = merge_items(taken, &counted);
            gathered = true;
        }
    }
    gathered
}

/// Where each of a page's numbered items ends: `Some(end)` on the block that
/// opens an item, the item taking the blocks up to `end`.
fn counted_items(
    blocks: &[Block],
    ordinals: &[Option<u32>],
    next_ordinal: impl Fn(usize) -> Option<u32>,
    in_section: impl Fn(usize) -> bool,
) -> Vec<Option<usize>> {
    // `d` follows `prev` within a line or so, not left of the item.
    let placed = |item: &Block, prev: &Block, d: &Block| -> bool {
        let fs = (item.font_size as f64).max(1.0);
        d.bbox.x0 >= item.bbox.x0 - fs * 0.5
            && d.bbox.y0 >= prev.bbox.y1 - fs * 0.5
            && d.bbox.y0 - prev.bbox.y1 <= fs * 1.5
    };
    // `d` is text set under the item: a description, a translation.
    let under = |item: &Block, prev: &Block, d: &Block| -> bool {
        let fs = (item.font_size as f64).max(1.0);
        placed(item, prev, d)
            && d.lines.len() <= MAX_ITEM_BODY_LINES
            && d.font_size <= item.font_size * 1.1
            && d.bbox.x0 <= item.bbox.x0 + fs * 4.0
            && !d
                .lines
                .first()
                .is_some_and(|l| opens_with_subnumber(&l.text))
    };
    // An item of a list nested under the item, `• For most adult vaccines…`.
    let nested = |d: &Block| d.marker.is_some() || strip_leading_bullet(&d.text()).is_some();

    // What is set under item `i`, up to the next numbered block or the first
    // block that is neither: (where that stops, the blocks of text directly
    // under the item, whether all of it is placed as an item's).
    let body = |i: usize| -> (usize, usize, bool) {
        let item = &blocks[i];
        let (mut j, mut text, mut lines, mut nests) = (i + 1, 0, 0, 0);
        let mut first_nested: Option<usize> = None;
        while j < blocks.len() && ordinals[j].is_none() {
            let d = &blocks[j];
            if nested(d) && placed(item, &blocks[j - 1], d) {
                nests += 1;
                first_nested.get_or_insert(j);
                if nests > MAX_NESTED_ITEMS {
                    return (j, text, false);
                }
            } else if first_nested.is_none() && under(item, &blocks[j - 1], d) {
                text += 1;
                lines += d.lines.len();
                if text > MAX_ITEM_BODY_BLOCKS || lines > MAX_ITEM_BODY_LINES {
                    return (j, text, false);
                }
            } else {
                return (j, text, false);
            }
            j += 1;
        }
        (j, text, true)
    };

    // Runs of items counting off, each the indices of its items, and
    // whether the run ended on an item with a section's worth under it.
    let mut runs: Vec<(Vec<usize>, bool)> = Vec::new();
    let mut used = vec![false; blocks.len()];
    for start in 0..blocks.len() {
        let Some(mut n) = ordinals[start] else {
            continue;
        };
        if used[start] {
            continue;
        }
        let mut run = vec![start];
        let mut i = start;
        let sectioned;
        loop {
            let item = &blocks[i];
            let fs = (item.font_size as f64).max(1.0);
            let same_x = |k: usize| (blocks[k].bbox.x0 - item.bbox.x0).abs() <= fs;
            let (j, _, whole) = body(i);
            let next = (j < blocks.len() && ordinals[j] == Some(n + 1) && same_x(j)).then_some(j);
            // The next item follows the text above it. A wider gap holds
            // something the prose blocks leave out, a table or a drawing, or
            // ends a section.
            let follows = next.is_some_and(|j| {
                let gap = blocks[j].bbox.y0 - blocks[j - 1].bbox.y1;
                (-fs * 0.5..=fs * MAX_ITEM_GAP).contains(&gap)
            });
            match next {
                Some(j) if whole && follows => {
                    run.push(j);
                    i = j;
                    n += 1;
                }
                _ => {
                    // The count carries on further down the column, past
                    // more than an item holds: a section's worth.
                    sectioned = !whole
                        && (j..blocks.len())
                            .find(|&k| ordinals[k].is_some())
                            .is_some_and(|k| ordinals[k] == Some(n + 1) && same_x(k));
                    break;
                }
            }
        }
        for &k in &run {
            used[k] = true;
        }
        runs.push((run, sectioned));
    }
    // Lists: runs counting off from one, joined by the runs in another
    // column of the page that carry on their count. A number with a
    // section's worth under it spoils the list it is in.
    let mut lists: Vec<(Vec<usize>, bool)> = Vec::new();
    for (run, sectioned) in runs {
        let first = ordinals[run[0]].unwrap_or(0);
        let head = &blocks[run[0]];
        match lists.last_mut() {
            Some((list, spoiled))
                if ordinals[*list.last().unwrap_or(&0)].is_some_and(|m| first == m + 1)
                    && list.last().is_some_and(|&l| {
                        let fs = (blocks[l].font_size as f64).max(1.0);
                        (blocks[l].bbox.x0 - head.bbox.x0).abs() > fs * 2.0
                    }) =>
            {
                list.extend(run);
                *spoiled |= sectioned;
            }
            _ if first == 1 => lists.push((run, sectioned)),
            _ => {}
        }
    }
    let mut counted: Vec<Option<usize>> = vec![None; blocks.len()];
    for (list, spoiled) in lists {
        let Some(&last) = list.last() else {
            continue;
        };
        let carries_on = ordinals[last].is_some_and(|m| next_ordinal(last) == Some(m + 1));
        if spoiled || list.len() < MIN_COUNTED_ITEMS || carries_on || !in_section(list[0]) {
            continue;
        }
        // Each item takes the text directly under it. An item last in its
        // column takes as many blocks as most items of the list took, so the
        // paragraph after a list is not read into it.
        let takes: Vec<usize> = list
            .iter()
            .enumerate()
            .filter(|&(r, &i)| list.get(r + 1).is_some_and(|&next| body(i).0 == next))
            .map(|(_, &i)| body(i).1)
            .collect();
        let usual = {
            let mut counts: HashMap<usize, usize> = HashMap::new();
            for t in &takes {
                *counts.entry(*t).or_default() += 1;
            }
            counts
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
                .map_or(0, |(t, _)| t)
        };
        for (r, &i) in list.iter().enumerate() {
            let (_, text, _) = body(i);
            let closes_column = list.get(r + 1).is_none_or(|&next| body(i).0 != next);
            let take = if closes_column { text.min(usual) } else { text };
            counted[i] = Some(i + 1 + take);
        }
    }
    counted
}

/// Merge each counted item with the blocks it takes.
fn merge_items(blocks: Vec<Block>, counted: &[Option<usize>]) -> Vec<Block> {
    let mut out = Vec::with_capacity(blocks.len());
    let mut blocks = blocks.into_iter().enumerate().peekable();
    while let Some((i, b)) = blocks.next() {
        let Some(end) = counted[i] else {
            out.push(b);
            continue;
        };
        let label = b
            .lines
            .first()
            .and_then(|l| ordinal_prefix(&l.text))
            .map(|(_, label)| label.to_string());
        let gap_above = b.gap_above;
        let mut lines = b.lines;
        while blocks.peek().is_some_and(|(k, _)| *k < end) {
            if let Some((_, d)) = blocks.next() {
                lines.extend(d.lines);
            }
        }
        let mut item = build(lines);
        item.marker = label;
        item.gap_above = gap_above;
        out.push(item);
    }
    out
}

/// A line opening with a section's own number, `1.1` or `2.3.1`: a clause
/// under a numbered section, not an item's description.
fn opens_with_subnumber(text: &str) -> bool {
    let head: String = text
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let parts: Vec<&str> = head.trim_end_matches('.').split('.').collect();
    parts.len() >= 2 && parts.iter().all(|p| !p.is_empty())
}

/// A block that is an ordered list's item with its number in its own text,
/// as [`gather_counted_items`] and [`split_hard_breaks`] mark them: the
/// marker is the label the text opens with.
pub fn counts_off(b: &Block) -> bool {
    b.marker.as_deref().is_some_and(|m| {
        b.lines
            .first()
            .and_then(|l| count_prefix(&l.text))
            .is_some_and(|(_, label)| label == m)
    })
}

/// Most lines and words of a side head.
const MAX_SIDE_HEAD_LINES: usize = 3;
const MAX_SIDE_HEAD_WORDS: usize = 10;

/// Fewest side heads, and the share of its column's blocks they must be,
/// before a column is read as a column of side heads.
const MIN_SIDE_HEADS: usize = 2;
const MIN_SIDE_HEAD_SHARE: f64 = 0.6;

/// Read a column of side heads with the text they label.
///
/// Some data sheets set each section's heading in a narrow strip at the
/// left, level with the first line of its text in the wide column beside
/// it. Column detection finds the gutter between them and reads the strip
/// whole before the column, so every head came out an empty section, one
/// after another, with all the text under the last of them.
///
/// A column is one of side heads when it is narrower than the column to its
/// right, and most of its blocks, at least [`MIN_SIDE_HEADS`] of them, are
/// short lines whose top is level with the top of a block there. Its blocks
/// are then read in that column's order, each before the first block of the
/// text there that starts level with it or lower. The head reads before
/// the block it labels, and its section takes that block and those after
/// it up to the next head. A block of the strip level with nothing, a
/// corner tag over the page, goes where its height puts it.
///
/// `tables` are the page's tables, which leave the prose before blocks
/// form: a head beside a table is level with the table's top.
pub fn read_side_heads(columns: Vec<Vec<Block>>, tables: &[BBox]) -> Vec<Vec<Block>> {
    let mut out: Vec<Vec<Block>> = Vec::with_capacity(columns.len());
    let mut columns = columns.into_iter().peekable();
    while let Some(col) = columns.next() {
        let heads = columns
            .peek()
            .is_some_and(|text| side_heads(&col, text, tables));
        if !heads {
            out.push(col);
            continue;
        }
        let Some(text) = columns.next() else {
            out.push(col);
            continue;
        };
        let mut merged = Vec::with_capacity(col.len() + text.len());
        let mut heads = col.into_iter().peekable();
        for b in text {
            while heads
                .peek()
                .is_some_and(|h| h.bbox.y0 <= b.bbox.y0 + level_tolerance(h, &b))
            {
                merged.extend(heads.next());
            }
            merged.push(b);
        }
        merged.extend(heads);
        out.push(merged);
    }
    out
}

/// How far apart two tops can be and still be level: half a line.
fn level_tolerance(a: &Block, b: &Block) -> f64 {
    (a.font_size.max(b.font_size).max(1.0) as f64) * 0.5
}

/// Whether `col` is a strip of side heads for `text`, the column to its right.
fn side_heads(col: &[Block], text: &[Block], tables: &[BBox]) -> bool {
    if col.is_empty() || text.is_empty() {
        return false;
    }
    let width = |c: &[Block]| {
        let x0 = c.iter().map(|b| b.bbox.x0).fold(f64::MAX, f64::min);
        let x1 = c.iter().map(|b| b.bbox.x1).fold(f64::MIN, f64::max);
        x1 - x0
    };
    if width(col) >= width(text) {
        return false;
    }
    let short = |b: &Block| {
        b.lines.len() <= MAX_SIDE_HEAD_LINES
            && b.text().split_whitespace().count() <= MAX_SIDE_HEAD_WORDS
    };
    let text_x0 = text.iter().map(|b| b.bbox.x0).fold(f64::MAX, f64::min);
    let level = |h: &Block| {
        let half_line = (h.font_size.max(1.0) as f64) * 0.5;
        text.iter()
            .any(|b| (h.bbox.y0 - b.bbox.y0).abs() <= level_tolerance(h, b))
            || tables
                .iter()
                .any(|t| t.x1 > text_x0 && (h.bbox.y0 - t.y0).abs() <= half_line * 2.0)
    };
    let heads = col.iter().filter(|h| short(h) && level(h)).count();
    heads >= MIN_SIDE_HEADS && heads as f64 >= col.len() as f64 * MIN_SIDE_HEAD_SHARE
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
            opens_with_script: false,
            opens_bold: false,
        }
    }

    #[test]
    fn a_line_of_chinese_wrapped_mid_word_joins_without_a_space() {
        let block = |a: &str, b: &str| Block {
            lines: vec![l(a, 50.0, 100.0, 11.0), l(b, 50.0, 114.0, 11.0)],
            bbox: BBox {
                x0: 50.0,
                y0: 100.0,
                x1: 350.0,
                y1: 125.0,
            },
            page: 0,
            font_size: 11.0,
            bold: false,
            gap_above: None,
            marker: None,
        };
        assert_eq!(
            block("deliver it to 青松纸业有限公", "司.").text(),
            "deliver it to 青松纸业有限公司."
        );
        // A line ended short of the edge is another item.
        let mut b = block("放送課", "株式会社");
        b.lines[0].bbox.x1 = 90.0;
        assert_eq!(b.text(), "放送課 株式会社");
        // Korean spaces its words and wraps at the spaces; Latin too.
        assert_eq!(
            block("한국어 문장이", "이어집니다").text(),
            "한국어 문장이 이어집니다"
        );
        assert_eq!(
            block("the order", "was confirmed").text(),
            "the order was confirmed"
        );
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
        let b = assemble_with_marks(&lines, 1.2, &boxes, &[], &[], 0.0);
        assert_eq!(b.len(), 3, "one block per option, got {b:?}");
        assert!(b.iter().all(|x| x.marker.as_deref() == Some("\u{2610}")));
    }

    #[test]
    fn drawn_bullets_start_their_items() {
        // Four items, each led by a 1.5pt square the producer drew, and
        // spaced barely more than their lines: two pairs fused.
        let lines = vec![
            l(
                "Sweep spray features specially angled nozzles",
                52.0,
                0.0,
                9.0,
            ),
            l("Boost technology increases the flow rate", 52.0, 12.0, 9.0),
            l(
                "High-arch swivel spout offers superior clearance",
                52.0,
                24.0,
                9.0,
            ),
            l("1.5 gpm maximum flow rate at 60 psi", 52.0, 36.0, 9.0),
        ];
        assert_eq!(assemble(&lines, 1.2).len(), 1, "no bullets: one paragraph");
        let dots: Vec<BBox> = (0..4)
            .map(|i| BBox {
                x0: 43.5,
                y0: i as f64 * 12.0 + 4.0,
                x1: 45.0,
                y1: i as f64 * 12.0 + 5.5,
            })
            .collect();
        let dots = marking_dots(&lines, &dots);
        let b = assemble_with_marks(&lines, 1.2, &[], &dots, &[], 0.0);
        assert_eq!(b.len(), 4, "one item per bullet, got {b:?}");
        assert!(b.iter().all(|x| x.marker.as_deref() == Some(BULLET_MARKER)));
    }

    #[test]
    fn a_drawn_bullet_item_keeps_its_wrapped_line() {
        // A list set narrower than its column: the item's first line ends
        // far short of the column's edge, and its wrapped line opens with
        // a name. Led by a drawn dot, it is one item, not two entries.
        let mut first = l("Distributed file systems, including", 56.0, 0.0, 9.0);
        first.bbox.x1 = 240.0;
        let mut wrapped = l("Hadoop and Ceph", 56.0, 12.0, 9.0);
        wrapped.bbox.x1 = 125.0;
        let lines = vec![first, wrapped];
        let mut measure = lines.clone();
        for y in [-60.0, -48.0] {
            let mut wide = l("Text that runs to the column's edge", 56.0, y, 9.0);
            wide.bbox.x1 = 400.0;
            measure.push(wide);
        }
        let dots = [BBox {
            x0: 47.0,
            y0: 4.0,
            x1: 50.0,
            y1: 7.0,
        }];
        let b = assemble_in_measure(&lines, &measure, 1.2, &[], &dots, &[], 9.0);
        assert_eq!(b.len(), 1, "one item, got {b:?}");
        assert_eq!(b[0].marker.as_deref(), Some(BULLET_MARKER));
    }

    #[test]
    fn a_lone_bullet_like_the_pages_others_marks_its_item() {
        // A list of three at the margin, and a list of one further down
        // under a heading of its own, its square bullet drawn alike, as far
        // from its 10pt text.
        let square = |x: f64, y: f64| BBox {
            x0: x,
            y0: y + 3.0,
            x1: x + 3.0,
            y1: y + 6.0,
        };
        let lines = vec![
            l("Opening paragraph item", 80.0, 0.0, 10.0),
            l("Second item of the list", 80.0, 14.0, 10.0),
            l("Third item of the list", 80.0, 28.0, 10.0),
            l("The one item of a list", 80.0, 120.0, 10.0),
        ];
        let listed = [square(70.0, 0.0), square(70.0, 14.0), square(70.0, 28.0)];
        let mut dots = listed.to_vec();
        dots.push(square(70.0, 120.0));
        assert_eq!(marking_dots(&lines, &dots).len(), 4);
        // Drawn half as large again, as far from its text, it is another
        // mark.
        let mut dots = listed.to_vec();
        dots.push(BBox {
            x0: 68.5,
            y0: 122.0,
            x1: 73.0,
            y1: 126.5,
        });
        assert_eq!(marking_dots(&lines, &dots).len(), 3);
        // Beside a drawing's small label, a terminal's dot drawn alike is
        // none of the list's.
        let mut small = lines.clone();
        small[3] = l("Terminal", 130.0, 120.0, 5.0);
        let mut dots = listed.to_vec();
        dots.push(square(120.0, 120.0));
        assert_eq!(marking_dots(&small, &dots).len(), 3);
        // Nor are a drawing's terminals, dots in a column beside one-word
        // labels at the items' size, a list a lone dot can join; the same
        // dot beside a list of phrases joins it.
        let labels = vec![
            l("Red", 80.0, 0.0, 10.0),
            l("Black", 80.0, 14.0, 10.0),
            l("White", 80.0, 28.0, 10.0),
            l("Blue", 90.0, 120.0, 10.0),
        ];
        let mut dots = listed.to_vec();
        dots.push(square(80.0, 120.0));
        assert_eq!(marking_dots(&labels, &dots).len(), 3);
        let mut phrases = lines.clone();
        phrases[3] = l("The one item of a list", 90.0, 120.0, 10.0);
        assert_eq!(marking_dots(&phrases, &dots).len(), 4);
        // Set a word space further out, it is no bullet of that list.
        let mut dots = listed.to_vec();
        dots.push(square(67.5, 120.0));
        assert_eq!(marking_dots(&lines, &dots).len(), 3);
    }

    #[test]
    fn a_line_broken_off_an_item_by_hand_goes_on_with_it() {
        // The item ended a sentence, and its next sentence, at its hanging
        // indent, read as a paragraph: the list goes on after it.
        let item = build(vec![l(
            "\u{2022} The cover comes off without tools.",
            202.4,
            0.0,
            8.1,
        )]);
        let broken = build(vec![
            l(
                "Each door fitted with 12\" long recessed handle",
                211.6,
                10.0,
                8.1,
            ),
            l("springs back when it is let go.", 211.6, 20.0, 8.1),
        ]);
        let next = build(vec![l(
            "\u{2022} The base takes a standard fitting.",
            202.4,
            30.0,
            8.1,
        )]);
        let b = rejoin_broken_items(vec![item.clone(), broken.clone(), next], &[]);
        assert_eq!(
            b.len(),
            2,
            "{:?}",
            b.iter().map(Block::text).collect::<Vec<_>>()
        );
        assert_eq!(b[0].lines.len(), 3);
        // The paragraph after a list's last item is the page's.
        assert_eq!(rejoin_broken_items(vec![item, broken], &[]).len(), 2);
    }

    #[test]
    fn a_formulas_operator_says_something() {
        // Set apart beside a formula's bar, `∂` or `∑` is the formula's,
        // and a marker only at the margin of what it would mark.
        for c in ['\u{2202}', '\u{2211}', '\u{221A}', '\u{2264}'] {
            assert!(says_something(c), "{c}");
        }
        // The minus sign reads as a dash, and `∙` is a bullet.
        for c in ['\u{2212}', '\u{2219}', '\u{2022}'] {
            assert!(!says_something(c), "{c}");
        }
    }

    #[test]
    fn a_drawings_dots_are_no_bullets() {
        // A two-line title at 20pt beside a scatter of markers, two of
        // which happen to stand one above the other level with its lines:
        // each has another marker within a few points, as a list's
        // bullets, a line apart, never do.
        let lines = vec![
            l("Northern Region", 200.0, 300.0, 20.0),
            l("Office", 200.0, 326.0, 20.0),
        ];
        let dot = |x: f64, y: f64| BBox {
            x0: x,
            y0: y,
            x1: x + 2.0,
            y1: y + 2.0,
        };
        let scatter = [
            dot(170.0, 308.0),
            dot(174.0, 311.0),
            dot(167.0, 314.0),
            dot(170.0, 334.0),
            dot(176.0, 331.0),
            dot(173.0, 338.0),
        ];
        assert!(marking_dots(&lines, &scatter).is_empty());
        // The two in a column alone are a list's bullets.
        assert_eq!(marking_dots(&lines, &[scatter[0], scatter[3]]).len(), 2);
    }

    #[test]
    fn a_dot_alone_is_no_bullet() {
        let dot = |y: f64| BBox {
            x0: 155.0,
            y0: y,
            x1: 158.0,
            y1: y + 3.0,
        };
        let lines = vec![l("Boston", 184.0, 117.7, 23.9)];
        // A seal's ring of dots: one level with a heading's second line,
        // another at the same place above it, beside nothing.
        assert!(marking_dots(&lines, &[dot(127.9), dot(60.0)]).is_empty());
        // Drawn twice in one place, it is still alone.
        assert!(marking_dots(&lines, &[dot(127.9), dot(127.9)]).is_empty());
    }

    #[test]
    fn dash_led_lines_in_a_run_are_items() {
        // Sub-items under a bullet, each opening with an em dash at one x;
        // the second wraps under its text.
        let lines = vec![
            l("\u{2014} Dimmable LEDs", 46.0, 0.0, 9.0),
            l(
                "\u{2014} Dimmable compact fluorescent lamps with",
                46.0,
                12.0,
                9.0,
            ),
            l("integrated ballast", 59.3, 24.0, 9.0),
            l("\u{2014} Halogen", 46.0, 36.0, 9.0),
        ];
        let b = assemble(&lines, 1.2);
        assert_eq!(b.len(), 3, "{b:?}");
        assert!(b.iter().all(|x| x.marker.as_deref() == Some("\u{2014}")));
        assert_eq!(
            strip_run_lead(&b[1].text(), "\u{2014}"),
            Some("Dimmable compact fluorescent lamps with integrated ballast")
        );
    }

    #[test]
    fn a_mark_the_font_maps_to_a_control_code_leads_items() {
        // A sidebar's `▶` bullets, mapped by the font to 0x17, each item
        // wrapped over more lines than a dash-led list's reach.
        let mut lines = vec![l(
            "\u{17} Designed using the highest quality",
            446.8,
            0.0,
            8.6,
        )];
        for k in 1..7 {
            lines.push(l(
                "materials and components to provide",
                456.5,
                9.3 * k as f64,
                8.6,
            ));
        }
        lines.push(l(
            "\u{17} Factory engineered, self-contained,",
            446.8,
            70.0,
            8.6,
        ));
        for k in 1..7 {
            lines.push(l(
                "capillary tube system using refrigerant",
                456.2,
                70.0 + 9.3 * k as f64,
                8.6,
            ));
        }
        lines.push(l(
            "\u{17} High capacity, factory balanced",
            446.8,
            140.0,
            8.6,
        ));
        let b = assemble(&lines, 1.1);
        assert_eq!(b.len(), 3, "{b:?}");
        assert!(b.iter().all(|x| x.marker.as_deref() == Some("\u{17}")));
        assert_eq!(
            strip_run_lead(&b[2].text(), "\u{17}"),
            Some("High capacity, factory balanced")
        );
    }

    #[test]
    fn a_lone_dash_line_is_not_an_item() {
        // A range, a minus, a dialogue's dash: one line so opening, among
        // lines of prose at the same place.
        let lines = vec![
            l("the readings fell over the year from", 46.0, 0.0, 9.0),
            l("- 2.0% in March to a low in the autumn", 46.0, 12.0, 9.0),
            l("and recovered by the end of December", 46.0, 24.0, 9.0),
            l("- as the report had expected it would", 46.0, 36.0, 9.0),
        ];
        let b = assemble(&lines, 1.2);
        assert!(b.iter().all(|x| x.marker.is_none()), "{b:?}");
        // A minus run into its number opens no item either.
        assert_eq!(run_lead(&l("-2.0%", 46.0, 0.0, 9.0)), None);
    }

    #[test]
    fn a_minus_or_an_arrow_opens_a_run_of_items() {
        // A topic's points, each opening with a minus sign or with `>`,
        // the text hanging after it.
        for mark in ["\u{2212}", ">"] {
            let lines = vec![
                l(
                    &format!("{mark} Pre-failure Alerts with the array"),
                    54.0,
                    0.0,
                    9.0,
                ),
                l("controller and its firmware.", 72.0, 12.0, 9.0),
                l(
                    &format!("{mark} Best-in-class supply assurance"),
                    54.0,
                    24.0,
                    9.0,
                ),
            ];
            let b = assemble(&lines, 1.2);
            assert_eq!(b.len(), 2, "{mark}: {b:?}");
            assert!(
                b.iter().all(|x| x.marker.as_deref() == Some(mark)),
                "{mark}: {b:?}"
            );
        }
        // A quoted message's lines open with `>` too, going on with their
        // sentences in lower case: no items.
        let quote = vec![
            l("> We met on Tuesday and agreed that", 46.0, 0.0, 9.0),
            l("> the draft should go out by Friday", 46.0, 12.0, 9.0),
            l("> once legal has had a look at it.", 46.0, 24.0, 9.0),
        ];
        assert!(assemble(&quote, 1.2).iter().all(|x| x.marker.is_none()));
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
        let b = assemble_with_marks(&lines, 1.2, &boxes, &[], &[], 0.0);
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
        h.bbox.x1 = 100.0;
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
    fn a_full_line_does_not_go_on_into_a_line_far_to_its_left() {
        // The right column's full line, then a heading the line pass ran on
        // from the left column across the gutter into the right one.
        let mut fused = l(
            "1.2 Changing Business demand during the year;",
            0.0,
            0.0,
            10.0,
        );
        fused.bbox.x1 = 550.0;
        let item = l(
            "• Moving into new products and services in high",
            250.0,
            12.0,
            10.0,
        );
        let mut heading = l(
            "9.5. Adapting to the New Normal: Changing demand",
            0.0,
            24.0,
            10.0,
        );
        heading.bbox.x1 = 450.0;
        heading.opens_bold = true;
        let b = assemble(&[fused, item, heading], 1.2);
        assert!(b.last().unwrap().text().starts_with("9.5."), "{b:?}");
        // Right-aligned: each line starts where its length puts it.
        let mut name = l("OWENS CORNING INSULATING SYSTEMS, LLC", 393.0, 0.0, 9.0);
        name.bbox.x1 = 582.0;
        name.bold = true;
        let mut street = l(
            "ONE OWENS CORNING PARKWAY TOLEDO, OH 43659 USA",
            351.0,
            10.0,
            9.0,
        );
        street.bbox.x1 = 582.0;
        assert_eq!(assemble(&[name, street], 1.2).len(), 1);
    }

    #[test]
    fn a_line_out_of_room_goes_on_whatever_the_weight_below() {
        // Bold names in a sentence, the last wrapped onto its own line.
        let mut name = l("United Kingdom.", 10.0, 24.0, 10.0);
        name.bold = true;
        name.bbox.x1 = 90.0;
        let lines = vec![
            l(
                "We identified 10 countries that do not restrict",
                10.0,
                0.0,
                10.0,
            ),
            l(
                "land ownership: Belgium, France, Sweden, and the",
                10.0,
                12.0,
                10.0,
            ),
            name,
        ];
        let b = assemble(&lines, 1.2);
        assert_eq!(b.len(), 1);
        assert!(b[0].text().ends_with("and the United Kingdom."));
    }

    #[test]
    fn a_line_carried_to_the_edge_by_design_has_not_run_out_of_room() {
        let bold = |text: &str, y: f64| {
            let mut b = l(text, 10.0, y, 10.0);
            b.bold = true;
            b.bbox.x1 = 150.0;
            b
        };
        // A contents entry, its leader running to the page number.
        let lines = vec![
            l(
                "Summary . . . . . . . . . . . . . . . . . . . . . 1",
                10.0,
                0.0,
                10.0,
            ),
            l(
                "Overview . . . . . . . . . . . . . . . . . . . . 3",
                10.0,
                12.0,
                10.0,
            ),
            bold("Part 1: Recent Developments", 24.0),
        ];
        assert_eq!(assemble(&lines, 1.2).len(), 2);
        // A short rule set as a row of dashes breaks no word.
        let mut dashes = l("----------", 10.0, 0.0, 10.0);
        dashes.bbox.x1 = 60.0;
        let lines = vec![dashes, bold("Product Name", 12.0)];
        assert_eq!(assemble(&lines, 1.2).len(), 2);
    }

    #[test]
    fn a_word_broken_at_a_hyphen_goes_on_whatever_the_weight_below() {
        let mut label = l(
            "STRAINER SYSTEM: Equipped with self-flush-",
            10.0,
            0.0,
            10.0,
        );
        label.bbox.x1 = 250.0;
        label.bold = true;
        let lines = vec![
            label,
            l("ing, easily removable strainers", 10.0, 12.0, 10.0),
        ];
        assert_eq!(assemble(&lines, 1.2).len(), 1);
    }

    #[test]
    fn a_line_opening_on_a_bold_label_begins_a_paragraph() {
        let label = |text: &str, y: f64| {
            let mut b = l(text, 10.0, y, 10.0);
            b.opens_bold = true;
            b
        };
        let mut last = l("trim panels are 20 gauge.", 10.0, 24.0, 10.0);
        last.bbox.x1 = 120.0;
        let lines = vec![
            label("CONSTRUCTION: Drawn tank, tank shelf and feet", 0.0),
            l(
                "of 16 gauge stainless steel. Wash chamber and",
                10.0,
                12.0,
                10.0,
            ),
            last,
            label("PUMP: With stainless steel pump and impeller", 36.0),
            l(
                "motor assures alignment and quiet operation.",
                10.0,
                48.0,
                10.0,
            ),
        ];
        let b = assemble(&lines, 1.2);
        assert_eq!(b.len(), 2);
        assert!(b[1].text().starts_with("PUMP:"));
    }

    #[test]
    fn a_bold_word_opening_a_wrapped_line_begins_nothing() {
        let mut names = l("Germany, Ireland, Japan, the Netherlands", 10.0, 12.0, 10.0);
        names.opens_bold = true;
        let lines = vec![
            l(
                "We identified 10 countries: Belgium, France,",
                10.0,
                0.0,
                10.0,
            ),
            names,
        ];
        assert_eq!(assemble(&lines, 1.2).len(), 1);
    }

    #[test]
    fn a_bullet_at_the_head_of_a_line_begins_an_item() {
        // Each item full or nearly, at body leading and the same indent.
        let lines = vec![
            l(
                "• store at room temperature away from direct light",
                10.0,
                0.0,
                10.0,
            ),
            l(
                "• keep the carton for full directions and warnings",
                10.0,
                12.0,
                10.0,
            ),
            l("• keep out of reach of children", 10.0, 24.0, 10.0),
        ];
        assert_eq!(assemble(&lines, 1.2).len(), 3);
    }

    #[test]
    fn a_line_set_back_to_the_bullet_ends_its_item() {
        // A spec sheet's feature list: wrapped lines hang under the item's
        // text, just under an em in, and the next section's heading starts
        // back at the bullet's x with no bullet of its own.
        let at = |text: &str, x: f64, y: f64, x1: f64| {
            let mut line = l(text, x, y, 8.7);
            line.bbox.x1 = x1;
            line
        };
        let lines = vec![
            at(
                "• True's commitment to using the highest",
                18.4,
                105.3,
                180.0,
            ),
            at(
                "quality materials and optimized refrigeration",
                27.3,
                115.8,
                184.0,
            ),
            at("today's food service marketplace.", 27.2, 126.4, 140.0),
            at("REFRIGERATION SYSTEM", 18.6, 137.0, 110.0),
            at(
                "• Factory engineered, self-contained, capillary",
                18.4,
                147.6,
                184.0,
            ),
        ];
        let texts: Vec<String> = assemble(&lines, 1.22).iter().map(Block::text).collect();
        assert_eq!(texts.len(), 3, "{texts:?}");
        assert_eq!(texts[1], "REFRIGERATION SYSTEM");
        // A paragraph after the list that opens where the items' text is and
        // wraps back to the margin reads on: its sentence did not end.
        let lines = vec![
            at(
                "• Form 1099-A (acquisition or abandonment",
                18.4,
                105.3,
                180.0,
            ),
            at(
                "Use Form W-9 only if you are a U.S. person",
                27.3,
                115.8,
                184.0,
            ),
            at("(including a resident", 27.2, 126.4, 120.0),
            at("alien), to provide your correct TIN.", 18.6, 137.0, 170.0),
        ];
        let texts: Vec<String> = assemble(&lines, 1.22).iter().map(Block::text).collect();
        assert!(texts.iter().all(|t| !t.starts_with("alien")), "{texts:?}");
    }

    #[test]
    fn footnotes_numbered_with_raised_numerals_are_a_note_each() {
        // A data sheet's notes at body leading, each opening with a raised
        // numeral at the margin and wrapping under its text.
        let note = |text: &str, y: f64| {
            let mut line = l(text, 312.1, y, 5.5);
            line.opens_with_script = true;
            line
        };
        let lines = vec![
            note(
                "7 1 MB/s = 1 million bytes per second. Based on internal",
                640.1,
            ),
            l(
                "host device, usage conditions and other factors.",
                318.1,
                645.9,
                5.5,
            ),
            note(
                "8 Portion of buffer capacity used for drive firmware.",
                654.3,
            ),
            note("9 Random RW 50/50 4KB QD=4 @40 IOPS.", 662.6),
            note("10 Based on use of Idle_A.", 670.8),
        ];
        let texts: Vec<String> = assemble(&lines, 1.5).iter().map(Block::text).collect();
        assert_eq!(texts.len(), 4, "{texts:?}");
        assert!(texts[3].starts_with("10 Based"), "{texts:?}");
        // A note's number set apart from its line keeps its place in the
        // text, and makes no list item, where the notes count: note 1 opens
        // with its number raised in its line, note 2's was set apart.
        let raised = |n: &str, y: f64| {
            let mut m = l(n, 36.1, y, 3.2);
            m.bbox.x1 = 37.8;
            m.bbox.y1 = y + 2.3;
            m
        };
        let text = |t: &str, y: f64, x1: f64| {
            let mut line = l(t, 41.8, y, 5.5);
            line.bbox.y1 = y + 4.0;
            line.bbox.x1 = x1;
            line
        };
        let mut one = l(
            "1 1MB = 1 million bytes, and 1TB = 1 trillion bytes.",
            36.1,
            605.5,
            5.5,
        );
        one.opens_with_script = true;
        one.bbox.x1 = 290.0;
        let lines = vec![
            one,
            text("depending on operating environment.", 611.7, 130.0),
            raised("2", 619.8),
            text(
                "Workload Rate is defined as the amount of user data",
                620.0,
                288.0,
            ),
            text("transferred to or from the hard drive.", 625.7, 150.0),
        ];
        // The page's leading: its notes wrap 6.2pt apart at 5.5pt.
        let b = assemble(&lines, 1.13);
        assert_eq!(b.len(), 2, "{b:?}");
        assert_eq!(b[1].marker, None);
        assert!(
            b[1].text().starts_with("2 Workload Rate"),
            "{:?}",
            b[1].text()
        );
        // A number raised beside a line that counts with nothing, a chart's
        // tick label, does not go into the text.
        let b = assemble(
            &[
                raised("67", 619.8),
                text(
                    "December (figure 14). Although population aging has",
                    620.0,
                    288.0,
                ),
            ],
            1.5,
        );
        assert!(b.iter().all(|x| !x.text().starts_with("67")), "{b:?}");
    }

    #[test]
    fn a_lettered_items_wrapped_line_hangs_under_its_text() {
        let at = |text: &str, x: f64, y: f64, x1: f64| {
            let mut line = l(text, x, y, 8.0);
            line.bbox.x1 = x1;
            line
        };
        // Steps beside a wiring diagram: each wrapped line an em and a half
        // in from its letter, its sentence going on. The column runs on
        // past them, to the diagram's labels, so none of them is full.
        let lines = vec![
            at(
                "Make the following connections to the supply cord, and see the diagram beside them:",
                47.5,
                481.0,
                560.0,
            ),
            at(
                "a. For 120/277/347V connect the black fixture lead to the",
                74.0,
                504.5,
                300.0,
            ),
            at(
                "voltage supply or Hot 1 (for 208/240/480V wiring).",
                87.2,
                514.4,
                290.0,
            ),
            at(
                "b. For 120/277/347V connect the white fixture lead to the",
                74.4,
                529.0,
                300.0,
            ),
            at(
                "neutral supply or Hot 2 (for 208/240/480V wiring).",
                87.9,
                538.9,
                290.0,
            ),
        ];
        let texts: Vec<String> = assemble(&lines, 1.24).iter().map(Block::text).collect();
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with("a. For") && t.ends_with("wiring).")),
            "{texts:?}"
        );
        // A lettered heading over a paragraph set in under it opens no
        // item: the paragraph opens a sentence of its own.
        let lines = vec![
            at("A. Introduction", 74.0, 504.5, 150.0),
            at(
                "The study measured the effect of three treatments.",
                87.2,
                514.4,
                290.0,
            ),
        ];
        assert_eq!(assemble(&lines, 1.24).len(), 2);
    }

    #[test]
    fn a_bulleted_items_wrapped_line_hangs_under_its_text() {
        // A submittal's feature list: the bullet at the margin, each line
        // of the item's text an em and a quarter in from it.
        let at = |text: &str, x: f64, y: f64, x1: f64| {
            let mut line = l(text, x, y, 11.0);
            line.bbox.x1 = x1;
            line
        };
        let lines = vec![
            at(
                "• Large tapswitch with a rocker-style dimmer for a",
                31.3,
                121.0,
                250.0,
            ),
            at("standard designer wallplate opening.", 44.9, 135.0, 230.0),
            at(
                "• Advanced dimming technology designed for",
                31.3,
                153.0,
                240.0,
            ),
            at(
                "compatibility with a broader range of high efficacy bulbs.",
                44.9,
                167.0,
                330.0,
            ),
            at("• UL Listed to control:", 31.3, 185.0, 150.0),
            at("— Dimmable LEDs", 44.9, 199.0, 130.0),
            at(
                "• Coordinating Claro and stainless steel wallplates",
                31.3,
                217.0,
                329.0,
            ),
        ];
        let b = assemble(&lines, 1.27);
        let texts: Vec<String> = b.iter().map(Block::text).collect();
        assert_eq!(
            texts,
            [
                "• Large tapswitch with a rocker-style dimmer for a standard designer wallplate opening.",
                "• Advanced dimming technology designed for compatibility with a broader range of high efficacy bulbs.",
                "• UL Listed to control:",
                "— Dimmable LEDs",
                "• Coordinating Claro and stainless steel wallplates",
            ]
        );
        // The paragraph after a list, set where the items' text is, below
        // an item that ends a sentence with room to spare.
        let lines = vec![
            at("• Keep the unit dry.", 87.0, 634.8, 230.0),
            at(
                "• Store it upright when it is not in use.",
                87.0,
                657.7,
                330.0,
            ),
            at(
                "The parts listed below are sold separately and may include:",
                108.4,
                680.7,
                520.0,
            ),
            at(
                "filters, seals and the like, which are described in turn below.",
                108.4,
                703.7,
                519.0,
            ),
        ];
        assert_eq!(assemble(&lines, 1.9).len(), 3);
        // The next item's bullet drawn apart from its text, on a lower
        // baseline: the text is an item of its own.
        let mut ring = at("\u{25CB}", 78.6, 216.0, 86.0);
        ring.font_size = 12.9;
        let lines = vec![
            at(
                "\u{25CB} Shipping to new regions: northern sites",
                78.6,
                197.8,
                280.0,
            ),
            ring,
            at("Faster service for existing sites", 104.5, 212.5, 300.0),
            at("\u{25CB} Online ordering and tracking", 78.6, 227.3, 329.0),
            at("\u{25CB} Online returns and refunds", 78.6, 242.1, 330.0),
        ];
        let b = assemble(&lines, 1.27);
        let texts: Vec<String> = b.iter().map(Block::text).collect();
        assert_eq!(texts.len(), 4, "{texts:?}");
        assert_eq!(b[1].marker.as_deref(), Some("\u{25CB}"));
        // A paragraph indented far past the bullet is not the item's text.
        let lines = vec![
            at(
                "• Large tapswitch with a rocker-style dimmer",
                31.3,
                121.0,
                250.0,
            ),
            at("A note set well in from the list", 80.0, 135.0, 330.0),
        ];
        assert_eq!(assemble(&lines, 1.27).len(), 2);
    }

    #[test]
    fn a_display_title_stacked_beside_a_column_is_one_block() {
        // Read as one column, the margin title's lines alternate with the
        // column's: `Executive`, two lines of text, `Summary`.
        let at = |text: &str, x: f64, y: f64, x1: f64, fs: f32| {
            let mut line = l(text, x, y, fs);
            line.bbox.x1 = x1;
            line
        };
        let lines = vec![
            at("Executive", 99.0, 97.2, 231.5, 34.0),
            at("India suffers from regulatory", 306.0, 115.0, 491.0, 10.5),
            at("cholesterol that is getting in", 339.0, 131.0, 490.0, 10.5),
            at("Summary", 99.0, 141.2, 235.7, 34.0),
            at("the way of doing business. The", 339.0, 147.0, 491.0, 10.5),
        ];
        let b = assemble(&lines, 1.52);
        let texts: Vec<String> = b.iter().map(Block::text).collect();
        assert!(
            texts.contains(&"Executive Summary".to_string()),
            "{texts:?}"
        );
        // Two headings one under the other with text of their own size in
        // between are two.
        let lines = vec![
            at("Results", 99.0, 97.2, 231.5, 34.0),
            at("A line of the same size", 306.0, 115.0, 491.0, 34.0),
            at("Methods", 99.0, 141.2, 235.7, 34.0),
        ];
        assert_eq!(assemble(&lines, 1.2).len(), 3);
    }

    #[test]
    fn a_slides_bullets_at_display_size_are_not_one_title() {
        let at = |text: &str, x: f64, y: f64, x1: f64, fs: f32| {
            let mut line = l(text, x, y, fs);
            line.bbox.x1 = x1;
            line
        };
        let lines = vec![
            at("• Faster setup on site", 99.0, 97.2, 231.5, 34.0),
            at("a note in the column beside", 306.0, 115.0, 491.0, 10.5),
            at("• Lower running costs", 99.0, 141.2, 235.7, 34.0),
        ];
        assert_eq!(assemble(&lines, 1.52).len(), 3);
    }

    #[test]
    fn display_type_is_not_a_list_marker() {
        // `III.` at 36pt in the margin beside a 10.5pt column.
        let mut num = l("III.", 102.0, 100.7, 36.0);
        num.bbox.x1 = 141.0;
        let mut text = l("his report defines", 306.0, 115.5, 10.5);
        text.bbox.x1 = 491.0;
        let b = assemble(&[num, text], 1.5);
        assert!(b.iter().all(|x| x.marker.is_none()), "{b:?}");
        assert!(b.iter().any(|x| x.text() == "III."));
    }

    #[test]
    fn a_bold_line_wider_than_the_rest_has_not_run_out_of_room() {
        // The widest line ends at its own end; no other line says the text
        // wraps there.
        let mut h = l("Results of the second survey round", 10.0, 0.0, 10.0);
        h.bold = true;
        let mut body = l("Most firms kept their staff.", 10.0, 12.0, 10.0);
        body.bbox.x1 = 200.0;
        assert_eq!(assemble(&[h, body], 1.2).len(), 2);
    }

    #[test]
    fn one_line_standing_out_past_the_column_is_not_its_measure() {
        // A centred notice under two columns, kept with the first, ends far
        // past the column's lines.
        let mut lines: Vec<Line> = (0..4)
            .map(|k| l("column text", 10.0, 12.0 * k as f64, 10.0))
            .collect();
        let mut notice = l("As continued improvement is a policy", 80.0, 60.0, 10.0);
        notice.bbox.x1 = 500.0;
        lines.push(notice);
        let edge = Edge::of(&lines, 0.0);
        assert!(edge.agreed);
        assert_eq!(edge.right, 310.0);
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
    fn a_half_point_step_across_a_short_line_is_a_break() {
        // A 9pt code label directly above the 8.5pt description it introduces:
        // the spacing is body leading and the left edges agree, so only the
        // size step separates them. Both directions: the full description
        // line is followed by the next short label.
        let mut label = l("A-100", 10.0, 0.0, 9.0);
        label.bbox.x1 = 35.0;
        let mut next = l("A-110", 10.0, 32.0, 9.0);
        next.bbox.x1 = 35.0;
        let lines = vec![
            label,
            l(
                "A-100: Site survey — perimeter and access roads",
                10.0,
                14.0,
                8.5,
            ),
            next,
        ];
        assert_eq!(assemble(&lines, 1.48).len(), 3);
    }

    #[test]
    fn size_jitter_between_full_lines_is_not_a_break() {
        // One paragraph of an 11pt paper: the lines measure 11.1 and 10.7,
        // and both run to the margin.
        let lines = vec![
            l(
                "ious benchmarks, outperforming established",
                10.0,
                0.0,
                11.1,
            ),
            l(
                "models like Llama 2 and Mistral 7B in reason-",
                10.0,
                13.5,
                10.7,
            ),
            l(
                "ing, mathematics, and the MMLU framework.",
                10.0,
                27.0,
                11.1,
            ),
        ];
        assert_eq!(assemble(&lines, 1.2).len(), 1);
    }

    #[test]
    fn hand_broken_lines_that_count_off_are_list_items() {
        // A lab's steps, one per line, the fourth long enough to reach the
        // block's edge. Every step is its own block with its number as the
        // marker — including the fifth, which the full fourth line would
        // otherwise have kept.
        let mut lines = vec![
            l("1. Label four plastic bags", 60.0, 0.0, 9.0),
            l("2. Weigh 20 g of air-dry soil into each plastic bag.", 60.0, 14.0, 9.0),
            l("3. Weigh 0.1 gram of designated liming material onto weighing paper.", 60.0, 28.0, 9.0),
            l("4. Add the liming material to the soil and mix thoroughly to distribute evenly in the soil.", 60.0, 42.0, 9.0),
            l("5. Add a few mL of water to each bag and mix.", 60.0, 56.0, 9.0),
            l("6. Close the bags to start incubation.", 60.0, 70.0, 9.0),
        ];
        for line in &mut lines {
            line.bbox.x1 = line.bbox.x0 + line.text.chars().count() as f64 * 4.2;
        }
        let b = assemble(&lines, 1.48);
        assert_eq!(b.len(), 6, "{b:?}");
        assert_eq!(b[0].marker.as_deref(), Some("1."));
        assert_eq!(b[4].marker.as_deref(), Some("5."));
        assert!(b[4].text().starts_with("5. Add a few"));
    }

    /// A page of blocks, one per entry, each `(text, x, y, lines)` at 7pt.
    fn page(entries: &[(&str, f64, f64, usize)]) -> Vec<Block> {
        entries
            .iter()
            .map(|&(text, x, y, n)| {
                let lines = (0..n)
                    .map(|k| {
                        l(
                            if k == 0 { text } else { "and more of it" },
                            x,
                            y + k as f64 * 8.0,
                            7.0,
                        )
                    })
                    .collect();
                build(lines)
            })
            .collect()
    }

    fn feature_list() -> Vec<Block> {
        page(&[
            ("Key Features", 40.0, 60.0, 1),
            ("1. 8Z Pellicle", 40.0, 100.0, 1),
            (
                "Offers comfort through eight zones of tension.",
                40.0,
                108.0,
                3,
            ),
            ("2. PostureFit SL", 40.0, 136.0, 1),
            ("Stabilizes and supports the lower back.", 40.0, 144.0, 3),
            ("3. Harmonic 2 Tilt", 40.0, 172.0, 1),
            ("Helps the chair move with you.", 40.0, 180.0, 2),
            ("Details", 40.0, 240.0, 1),
        ])
    }

    #[test]
    fn numbered_items_with_short_descriptions_are_one_list() {
        let mut pages = vec![feature_list()];
        gather_counted_items(&mut pages, &|_| false, &|_, _| true);
        let texts: Vec<String> = pages[0].iter().map(Block::text).collect();
        assert_eq!(texts.len(), 5, "{texts:?}");
        assert!(
            texts[1].starts_with("1. 8Z Pellicle Offers comfort"),
            "{texts:?}"
        );
        assert!(
            texts[3].starts_with("3. Harmonic 2 Tilt Helps the chair"),
            "{texts:?}"
        );
        assert_eq!(pages[0][1].marker.as_deref(), Some("1."));
        assert!(counts_off(&pages[0][2]));
        // The heading after the list is not read into its last item.
        assert_eq!(texts[4], "Details");
    }

    #[test]
    fn numbered_sections_are_not_a_list() {
        // The count carries on overleaf: the document's sections.
        let mut pages = vec![
            feature_list(),
            page(&[
                ("4. Termination", 40.0, 60.0, 1),
                ("Either party may end it.", 40.0, 68.0, 2),
            ]),
        ];
        gather_counted_items(&mut pages, &|_| false, &|_, _| true);
        assert_eq!(pages[0].len(), 8);
        // Clauses under each number are a section's.
        let mut pages = vec![page(&[
            ("1. Definitions", 40.0, 100.0, 1),
            (
                "1.1 \"Services\" means the hosted applications.",
                40.0,
                108.0,
                2,
            ),
            ("2. Subscription and Use", 40.0, 136.0, 1),
            ("2.1 Customer may use the Services.", 40.0, 144.0, 2),
            ("3. Fees and Payment", 40.0, 172.0, 1),
            ("3.1 Customer will pay the fees.", 40.0, 180.0, 2),
        ])];
        gather_counted_items(&mut pages, &|_| false, &|_, _| true);
        assert_eq!(pages[0].len(), 6);
        // A count that does not start from one carries on from elsewhere.
        let mut pages = vec![page(&[
            ("16. Hertog M: Optimization of a method.", 40.0, 100.0, 2),
            ("17. Yernool D: Cloning and expression.", 40.0, 120.0, 2),
            ("18. French A: Conformational energy.", 40.0, 140.0, 2),
        ])];
        gather_counted_items(&mut pages, &|_| false, &|_, _| true);
        assert!(pages[0].iter().all(|b| b.marker.is_none()));
        // Two in a row are as often two short sections.
        let mut pages = vec![page(&[
            ("1. The dynamics", 40.0, 100.0, 1),
            ("Short text.", 40.0, 108.0, 2),
            ("2. Modeling the dynamics", 40.0, 136.0, 1),
        ])];
        gather_counted_items(&mut pages, &|_| false, &|_, _| true);
        assert_eq!(pages[0].len(), 3);
    }

    #[test]
    fn a_count_broken_by_what_the_prose_leaves_out_is_not_one_list() {
        // A table between the third item and the fourth: the heading over
        // the table is not the third item's description. Whether the count
        // carried on below the table is one list or a document's sections,
        // the page does not say, and the numbers are left as they are.
        let mut pages = vec![page(&[
            (
                "1. Description: the parts below are discontinued.",
                40.0,
                100.0,
                2,
            ),
            ("2. Reason: the line is closing.", 40.0, 120.0, 2),
            ("3. Replacement: see the datasheet.", 40.0, 140.0, 2),
            ("Discontinued items", 40.0, 160.0, 1),
            ("4. Key dates: last buy in December.", 40.0, 300.0, 2),
            ("5. Please confirm your quantity.", 40.0, 320.0, 2),
        ])];
        gather_counted_items(&mut pages, &|_| false, &|_, _| true);
        let texts: Vec<String> = pages[0].iter().map(Block::text).collect();
        assert!(
            texts.contains(&"Discontinued items".to_string()),
            "{texts:?}"
        );
        assert!(pages[0].iter().all(|b| b.marker.is_none()));
    }

    #[test]
    fn numbers_straight_under_the_title_are_sections() {
        let mut pages = vec![feature_list()];
        assert!(!gather_counted_items(&mut pages, &|_| false, &|_, _| false));
        assert!(pages[0].iter().all(|b| b.marker.is_none()));
    }

    #[test]
    fn a_numbered_heading_the_outline_names_is_not_an_item() {
        let mut pages = vec![feature_list()];
        gather_counted_items(&mut pages, &|t| t == "2. PostureFit SL", &|_, _| true);
        assert!(pages[0].iter().all(|b| b.marker.is_none()));
    }

    #[test]
    fn a_list_carried_on_in_the_next_column_is_one_list() {
        let mut blocks = feature_list();
        blocks.extend(page(&[
            ("4. Three sizes", 300.0, 100.0, 1),
            ("Aeron comes in three sizes.", 300.0, 108.0, 2),
            ("5. Dematerialized base", 300.0, 136.0, 1),
        ]));
        let mut pages = vec![blocks];
        gather_counted_items(&mut pages, &|_| false, &|_, _| true);
        let marked: Vec<&str> = pages[0]
            .iter()
            .filter_map(|b| b.marker.as_deref())
            .collect();
        assert_eq!(marked, ["1.", "2.", "3.", "4.", "5."]);
    }

    #[test]
    fn an_ordinal_opens_a_line_only_before_a_space() {
        assert_eq!(ordinal_prefix("1. Label four"), Some((1, "1.")));
        assert_eq!(ordinal_prefix("12) Twelve"), Some((12, "12)")));
        assert_eq!(ordinal_prefix("1.5 mL of water"), None);
        assert_eq!(ordinal_prefix("2024. The year"), None);
        assert_eq!(ordinal_prefix("Label four"), None);
    }

    #[test]
    fn a_lower_case_letter_labels_an_item_counted_apart_from_numbers() {
        assert_eq!(count_prefix("a. The size"), Some((LETTERED + 1, "a.")));
        assert_eq!(count_prefix("b) The field"), Some((LETTERED + 2, "b)")));
        assert_eq!(count_prefix("(c) The image"), Some((LETTERED + 3, "(c)")));
        assert_eq!(count_prefix("2. When changing"), Some((2, "2.")));
        // A capital opens an appendix as often as an item, and an
        // abbreviation or a word is no label.
        assert_eq!(count_prefix("A. Appendix"), None);
        assert_eq!(count_prefix("e.g. this"), None);
        assert_eq!(count_prefix("ab. Not one"), None);
        assert_eq!(count_prefix("(c The image"), None);
    }

    #[test]
    fn lettered_lines_are_items_and_the_text_set_left_of_them_is_not() {
        // Six short steps lettered under a numbered question, and the next
        // question set left of them, all at the paragraph's leading.
        let lines = vec![
            row(
                "a. The size of the field of view decreases",
                60.0,
                0.0,
                230.0,
            ),
            row("b. The field of view becomes darker", 60.0, 10.0, 200.0),
            row("c. The size of the image increases", 60.0, 20.0, 190.0),
            row(
                "2. When changing from scanning to low power the field",
                40.0,
                30.0,
                300.0,
            ),
            row("of view gets smaller.", 40.0, 40.0, 120.0),
        ];
        let b = assemble(&lines, 1.2);
        let marks: Vec<Option<&str>> = b.iter().map(|b| b.marker.as_deref()).collect();
        assert_eq!(
            marks,
            [Some("a."), Some("b."), Some("c."), None],
            "{:?}",
            b.iter().map(Block::text).collect::<Vec<_>>()
        );
        assert_eq!(b[3].lines.len(), 2);
        // A note's wrapped line goes back to the margin its number was
        // indented from, and is the note's.
        let note = vec![
            row(
                "1. Projections of real gross domestic product.",
                48.0,
                0.0,
                250.0,
            ),
            row(
                "2. Measure is the overall consumer price index, the price",
                48.0,
                10.0,
                300.0,
            ),
            row("measure that has been most widely used.", 40.0, 20.0, 230.0),
        ];
        let b = assemble(&note, 1.2);
        assert_eq!(
            b.last().map(|b| b.lines.len()),
            Some(2),
            "{:?}",
            b.iter().map(Block::text).collect::<Vec<_>>()
        );
        // Lines that happen to open `(d)` and `(e)`, a caption's panels
        // wrapped one to a line, count off from nothing.
        let caption = vec![
            row(
                "Figure 6. Detection results: (a) input frame; (b) truth; (c)",
                40.0,
                0.0,
                300.0,
            ),
            row(
                "(d) result of the method based on the wavelet transform;",
                40.0,
                10.0,
                300.0,
            ),
            row("(e) result when J = 4.", 40.0, 20.0, 140.0),
        ];
        assert!(assemble(&caption, 1.2).iter().all(|b| b.marker.is_none()));
    }

    #[test]
    fn lettered_blocks_open_a_list_of_their_own() {
        // Wiring steps a step apart, each wrapped over two lines.
        let lines = vec![
            row(
                "a. For 120V connect the black fixture lead to the",
                40.0,
                0.0,
                300.0,
            ),
            row("voltage supply.", 49.0, 10.0, 120.0),
            row(
                "b. For 120V connect the white fixture lead to the",
                40.0,
                30.0,
                300.0,
            ),
            row("neutral supply.", 49.0, 40.0, 120.0),
        ];
        let b = assemble(&lines, 1.2);
        let marks: Vec<Option<&str>> = b.iter().map(|b| b.marker.as_deref()).collect();
        assert_eq!(
            marks,
            [Some("a."), Some("b.")],
            "{:?}",
            b.iter().map(Block::text).collect::<Vec<_>>()
        );
        // A lone `a.` opens a paragraph, not a list.
        let lone = vec![
            row(
                "a. For 120V connect the black fixture lead to the",
                40.0,
                0.0,
                300.0,
            ),
            row("voltage supply.", 49.0, 10.0, 120.0),
            row("Then close the box.", 40.0, 30.0, 140.0),
        ];
        assert!(assemble(&lone, 1.2).iter().all(|b| b.marker.is_none()));
    }

    #[test]
    fn lines_under_different_fills_are_different_rows() {
        // Two form items, each a label wrapped onto two lines inside its own
        // shaded band. The gap between bands is just under the paragraph
        // threshold, so without the bands the four lines read as one item.
        let lines = vec![
            l("Q12 — Satisfied with the onboarding", 47.0, 0.0, 7.5),
            l("materials", 47.0, 11.0, 7.5),
            l("Q13 — Would recommend the program to", 47.0, 25.0, 7.5),
            l("a colleague", 47.0, 36.0, 7.5),
        ];
        assert_eq!(assemble(&lines, 1.48).len(), 1, "no bands: one block");
        let bands = vec![
            BBox {
                x0: 43.0,
                y0: -4.0,
                x1: 569.0,
                y1: 21.0,
            },
            BBox {
                x0: 43.0,
                y0: 21.0,
                x1: 569.0,
                y1: 46.0,
            },
        ];
        let b = assemble_with_marks(&lines, 1.48, &[], &[], &bands, 0.0);
        assert_eq!(b.len(), 2, "one block per band, got {b:?}");
        assert!(b[0].text().starts_with("Q12") && b[0].text().ends_with("materials"));
        assert!(b[1].text().starts_with("Q13"));
    }

    #[test]
    fn a_box_around_a_whole_paragraph_does_not_split_it() {
        let lines = vec![
            l("first line of", 10.0, 0.0, 10.0),
            l("the paragraph", 10.0, 12.0, 10.0),
        ];
        let bands = vec![BBox {
            x0: 0.0,
            y0: -5.0,
            x1: 400.0,
            y1: 30.0,
        }];
        assert_eq!(
            assemble_with_marks(&lines, 1.2, &[], &[], &bands, 0.0).len(),
            1
        );
    }

    #[test]
    fn hand_broken_lines_become_one_block_each() {
        // A checklist: one field per line, body leading, nothing between the
        // lines but a ragged right edge with room for the next word.
        let mut lines = vec![
            l("Site access: [X] Badge [ ] Escort", 50.0, 0.0, 7.5),
            l("Hard hat issued: [X] Yes [ ] No", 50.0, 11.0, 7.5),
            l("Visitor log signed: [ ] Yes [X] No.", 50.0, 22.0, 7.5),
            l(
                "Hazards briefed: [X] Yes [ ] No. If Yes list: working at height, live cables",
                50.0,
                33.0,
                7.5,
            ),
        ];
        // Widths in proportion to the text, at 3.5pt a character.
        for line in &mut lines {
            line.bbox.x1 = line.bbox.x0 + line.text.chars().count() as f64 * 3.5;
        }
        let b = assemble(&lines, 1.48);
        assert_eq!(b.len(), 4, "one block per field, got {b:?}");
        assert!(b[2].text().starts_with("Visitor log signed"));
    }

    #[test]
    fn a_wrapped_paragraph_keeps_its_lines() {
        // Every line but the last reaches the edge: the next word never fit.
        let mut lines = vec![
            l(
                "The quick brown fox jumps over the lazy dog and",
                10.0,
                0.0,
                10.0,
            ),
            l(
                "keeps running through the long and winding field",
                10.0,
                12.0,
                10.0,
            ),
            l(
                "until the evening comes and the light goes out of",
                10.0,
                24.0,
                10.0,
            ),
            l("the sky.", 10.0, 36.0, 10.0),
        ];
        for line in &mut lines {
            line.bbox.x1 = line.bbox.x0 + line.text.chars().count() as f64 * 5.0;
        }
        assert_eq!(assemble(&lines, 1.2).len(), 1);
    }

    #[test]
    fn centred_lines_are_not_hand_breaks() {
        // A title set centred over three lines: the left edges wander, and
        // that is the layout, not a form.
        let mut lines = vec![
            l("Very Long Document Titles", 80.0, 0.0, 16.0),
            l("A Study of", 120.0, 19.0, 16.0),
            l("in Practice", 118.0, 38.0, 16.0),
        ];
        for line in &mut lines {
            line.bbox.x1 = line.bbox.x0 + line.text.chars().count() as f64 * 8.0;
        }
        assert_eq!(assemble(&lines, 1.2).len(), 1);
    }

    #[test]
    fn a_display_title_set_right_aligned_or_centred_is_one_block() {
        let at = |text: &str, x0: f64, x1: f64, y: f64, fs: f32| {
            let mut line = l(text, x0, y, fs);
            line.bbox.x1 = x1;
            line
        };
        let body = |y: f64| {
            at(
                "Body text runs the full measure of the column here",
                55.0,
                400.0,
                y,
                8.0,
            )
        };
        // Four 28pt lines right-aligned, over 8pt body text that sets the
        // column's edge.
        let lines = vec![
            at("PROCAT\u{00AE}", 159.0, 252.0, 126.0, 28.0),
            at("PROFESSIONAL", 82.0, 247.0, 159.0, 28.0),
            at("INSULATION", 117.0, 246.0, 192.0, 28.0),
            at("SYSTEM", 163.0, 246.0, 225.0, 28.0),
            body(300.0),
            body(310.0),
            body(320.0),
        ];
        let blocks = assemble_with_marks(&lines, 1.2, &[], &[], &[], 8.0);
        assert_eq!(
            blocks[0].text(),
            "PROCAT\u{00AE} PROFESSIONAL INSULATION SYSTEM"
        );
        // Centred.
        let lines = vec![
            at("Author's Note to the", 111.0, 351.0, 100.0, 24.0),
            at("2021 Edition", 147.0, 315.0, 129.0, 24.0),
            body(200.0),
            body(210.0),
        ];
        let blocks = assemble_with_marks(&lines, 1.2, &[], &[], &[], 8.0);
        assert_eq!(blocks[0].text(), "Author's Note to the 2021 Edition");
        // Body-size lines sharing a centre are lines ended by hand.
        let lines = vec![
            at("Acme Corporation", 150.0, 250.0, 100.0, 8.0),
            at("12 Main Street", 160.0, 240.0, 110.0, 8.0),
            body(200.0),
            body(210.0),
        ];
        let blocks = assemble_with_marks(&lines, 1.2, &[], &[], &[], 8.0);
        assert_eq!(blocks[0].text(), "Acme Corporation");
    }

    #[test]
    fn side_heads_are_read_with_the_text_they_are_level_with() {
        let at = |text: &str, x0: f64, x1: f64, y: f64| {
            let mut line = l(text, x0, y, 8.0);
            line.bbox.x1 = x1;
            build(vec![line])
        };
        let heads = vec![
            at("USG Interior Panel", 40.0, 190.0, 18.0),
            at("DESCRIPTION", 40.0, 100.0, 305.0),
            at("INTENDED FOR", 40.0, 100.0, 370.0),
        ];
        let text = vec![
            at("USG SHEETROCK BRAND GYPSUM PANELS", 210.0, 560.0, 84.0),
            at(
                "Available in two thicknesses, the panels feature a core.",
                210.0,
                560.0,
                305.0,
            ),
            at(
                "Commercial or residential applications.",
                210.0,
                560.0,
                370.0,
            ),
            at(
                "New or repair and remodel construction.",
                210.0,
                560.0,
                382.0,
            ),
        ];
        let cols = read_side_heads(vec![heads, text], &[]);
        assert_eq!(cols.len(), 1);
        let order: Vec<String> = cols[0].iter().map(Block::text).collect();
        assert_eq!(
            order,
            [
                "USG Interior Panel",
                "USG SHEETROCK BRAND GYPSUM PANELS",
                "DESCRIPTION",
                "Available in two thicknesses, the panels feature a core.",
                "INTENDED FOR",
                "Commercial or residential applications.",
                "New or repair and remodel construction.",
            ]
        );
        // A column of paragraphs is not one of heads, however its tops fall.
        let left = vec![
            at(
                "A paragraph of the left column that runs on for several words more.",
                40.0,
                190.0,
                100.0,
            ),
            at(
                "Another paragraph of the left column that runs on for many more words.",
                40.0,
                190.0,
                200.0,
            ),
        ];
        let right = vec![
            at("The right column's text.", 210.0, 560.0, 100.0),
            at("More of it.", 210.0, 560.0, 200.0),
        ];
        assert_eq!(read_side_heads(vec![left, right], &[]).len(), 2);
    }

    #[test]
    fn a_sign_alone_on_its_line_is_not_a_list_marker() {
        let sign = l("\u{00AE}", 247.0, 127.0, 16.0);
        let title = l("PROCAT", 159.0, 126.0, 28.0);
        assert!(!is_marker(&sign, Some(&title), 159.0));
        assert!(is_marker(
            &l("\u{2022}", 150.0, 126.0, 8.0),
            Some(&title),
            150.0
        ));
    }

    #[test]
    fn a_tick_label_marks_no_other() {
        // A chart's tick labels side by side on a row, at the column's
        // margin: neither is the other's number.
        let mut tick = l("1.5", 300.0, 80.0, 7.0);
        tick.bbox.x1 = 309.0;
        let next = l("3.0", 330.0, 80.0, 7.0);
        assert!(!is_marker(&tick, Some(&next), 300.0));
        // A number beside the item it opens still marks it.
        let mut number = l("4.", 50.0, 80.0, 7.0);
        number.bbox.x1 = 56.0;
        let item = l("Check the seal before each use", 62.0, 80.0, 7.0);
        assert!(is_marker(&number, Some(&item), 50.0));
    }

    #[test]
    fn two_hand_broken_lines_stay_together() {
        // Below the line minimum: a two-line title broken by hand must remain
        // one heading.
        let mut lines = vec![
            l("Deep Learning for", 10.0, 0.0, 16.0),
            l("Document Understanding at Scale", 10.0, 19.0, 16.0),
        ];
        for line in &mut lines {
            line.bbox.x1 = line.bbox.x0 + line.text.chars().count() as f64 * 8.0;
        }
        assert_eq!(assemble(&lines, 1.2).len(), 1);
    }

    #[test]
    fn rejoins_hyphenated_line_break() {
        let lines = vec![l("innova-", 10.0, 0.0, 10.0), l("tion", 10.0, 12.0, 10.0)];
        assert_eq!(assemble(&lines, 1.2)[0].text(), "innovation");
    }

    #[test]
    fn a_hyphen_beside_a_digit_or_a_capital_is_the_texts_own() {
        let joined = |a: &str, b: &str| {
            let lines = vec![l(a, 10.0, 0.0, 10.0), l(b, 10.0, 12.0, 10.0)];
            assemble(&lines, 1.2)[0].text()
        };
        assert_eq!(
            joined("call Poison Control (1-800-222-", "1222) right away."),
            "call Poison Control (1-800-222-1222) right away."
        );
        assert_eq!(
            joined("in the 2019-", "2020 period"),
            "in the 2019-2020 period"
        );
        assert_eq!(
            joined("The Polish-", "German Gazetteer"),
            "The Polish-German Gazetteer"
        );
        assert_eq!(
            joined("test kits are MESA-", "approved"),
            "test kits are MESA-approved"
        );
        // A dash set apart from its words stays one.
        assert_eq!(joined("a pause -", "then more"), "a pause - then more");
        // A dash or slash set closed runs on into the next line.
        assert_eq!(
            joined("backup and restore—", "D2D, virtual tape"),
            "backup and restore—D2D, virtual tape"
        );
        assert_eq!(joined("pages 10–", "20 cover it"), "pages 10–20 cover it");
        assert_eq!(
            joined("see https://example.com/library/", "warranty.pdf"),
            "see https://example.com/library/warranty.pdf"
        );
        assert_eq!(
            joined("at www.example.com/", "support"),
            "at www.example.com/support"
        );
        // A slash ending a word may have had its space after it.
        assert_eq!(joined("cash and/", "or credit"), "cash and/ or credit");
        // Set apart, each keeps its space.
        assert_eq!(joined("a pause —", "then more"), "a pause — then more");
        assert_eq!(joined("Red /", "Blue"), "Red / Blue");
    }

    #[test]
    fn a_line_opened_by_reference_marks_opens_a_note() {
        assert!(opens_note("* Due to natural refrigerant"));
        assert!(opens_note("**Energy use measured for a run"));
        assert!(opens_note("†† Patent pending in the EU"));
        // A rule of asterisks, a lone mark or one before a figure alone are
        // no note.
        assert!(!opens_note("*****"));
        assert!(!opens_note("* x"));
        assert!(!opens_note("*****Five marks is a rule"));
        assert!(!opens_note("Values marked * differ"));
    }

    #[test]
    fn a_hyphen_is_a_word_break_only_between_two_letters_of_one_case() {
        use HyphenJoin::*;
        for (stem, next, want) in [
            ("microcom", "puter is", Word),
            ("accidental", "ly drained", Word),
            ("the SO", "LAR model", Word),
            ("Fly", "Casting", Hyphen),
            ("S-warfarin/R", "warfarin", Hyphen),
            ("an e", "mail address", Hyphen),
            ("the X", "Forwarded-Proto header", Hyphen),
            ("PD-1/PD", "L1 inhibitors", Hyphen),
            ("COVID", "19", Hyphen),
            ("subdivisions (v)(1)(A)", "(L), (ae)(1)", Hyphen),
            ("a pause ", "then", Dash),
        ] {
            assert_eq!(HyphenJoin::of(stem, next), want, "{stem}- / {next}");
        }
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
    fn a_trade_mark_raised_after_a_name_is_not_a_list_marker() {
        // Set apart in a smaller size after the name, as a flyer sets it on
        // a line of its own; read as a bullet it was dropped.
        for mark in ["\u{2122}", "\u{2120}"] {
            let mut sup = l(mark, 120.0, 0.0, 6.0);
            sup.bbox.x1 = 126.0;
            let lines = vec![l("Thermo Scientific", 30.0, 2.0, 10.0), sup];
            let b = assemble(&lines, 1.2);
            assert!(b.iter().all(|b| b.marker.is_none()), "{mark}");
            assert!(b.iter().any(|b| b.text().contains(mark)), "{mark}");
        }
    }

    /// `text` from `x0` to `x1` on the row at `y`, 8pt.
    fn row(text: &str, x0: f64, y: f64, x1: f64) -> Line {
        let mut line = l(text, x0, y, 8.0);
        line.bbox.x1 = x1;
        line
    }

    /// A column's measure: full lines from 40 to 300.
    fn measure(y: f64) -> Vec<Line> {
        (0..4)
            .map(|i| {
                row(
                    "the column's running text set full to its edge",
                    40.0,
                    y + 10.0 * i as f64,
                    300.0,
                )
            })
            .collect()
    }

    #[test]
    fn a_lowercase_line_hanging_under_an_item_goes_on_with_it() {
        let at = |t: &str, x0: f64, y: f64, x1: f64, bold: bool| {
            let mut n = l(t, x0, y, 10.0);
            n.bbox.x1 = x1;
            n.bold = bold;
            n
        };
        // An item of two lines, its second the shorter, over a lowercase
        // line set in another weight at the item's hanging indent, then the
        // next item. Every line opens a little right of where the item's
        // dash stands.
        let lines = vec![
            at(
                "- First choice of the kind described in the guide",
                60.0,
                400.0,
                320.0,
                false,
            ),
            at(
                "under its own heading (see below)",
                68.0,
                413.0,
                230.0,
                false,
            ),
            at("and", 69.0, 428.0, 85.0, true),
            at(
                "- Second choice set the same way",
                60.0,
                441.0,
                250.0,
                false,
            ),
        ];
        let b = assemble(&lines, 1.2);
        let texts: Vec<String> = b.iter().map(Block::text).collect();
        assert!(
            texts.iter().any(|t| t.ends_with("(see below) and")),
            "{texts:?}"
        );
        assert!(texts.iter().all(|t| t != "and"), "{texts:?}");
    }

    #[test]
    fn a_sentence_runs_on_past_a_wide_leading() {
        // Footnotes at nearly twice their size's leading: the first wraps.
        let note = |t: &str, y: f64, x1: f64| {
            let mut n = l(t, 40.0, y, 5.0);
            n.bbox.x1 = x1;
            n
        };
        let lines = vec![
            note(
                "1 Models ship in one format. Data must be aligned to see improved",
                0.0,
                560.0,
            ),
            note("performance in that format.", 9.8, 110.0),
            note(
                "2 Drives available through franchised partners only.",
                19.6,
                300.0,
            ),
        ];
        let b = assemble(&lines, 1.2);
        assert_eq!(
            b.len(),
            2,
            "{:?}",
            b.iter().map(Block::text).collect::<Vec<_>>()
        );
        assert!(b[0].text().ends_with("that format."));
    }

    #[test]
    fn a_sentence_runs_on_into_bold_capitals() {
        let mut lines = vec![
            row(
                "WARNING! If you scrape, sand or remove old",
                36.0,
                0.0,
                205.0,
            ),
            row(
                "paint, you may release lead dust. LEAD IS",
                36.0,
                9.2,
                208.0,
            ),
            row("TOXIC. EXPOSURE TO LEAD DUST CAN", 36.0, 18.4, 200.0),
        ];
        lines[2].bold = true;
        let b = assemble(&lines, 1.2);
        assert_eq!(
            b.len(),
            1,
            "{:?}",
            b.iter().map(Block::text).collect::<Vec<_>>()
        );
        // A bold heading in capitals over its text stands alone.
        let mut h = row("7.2. DETERMINATION OF THE COEFFICIENT", 36.0, 0.0, 208.0);
        h.bold = true;
        let body = row("If the coefficient is assumed constant", 36.0, 9.2, 180.0);
        assert_eq!(assemble(&[h, body], 1.2).len(), 2);
    }

    #[test]
    fn numbered_items_with_hanging_lines_part_at_each_number() {
        let lines = vec![
            row(
                "1. Avoid exposure to sustained temperatures.",
                40.0,
                0.0,
                220.0,
            ),
            row(
                "2. Avoid exposure to excessive, repetitive or continuous",
                40.0,
                10.0,
                300.0,
            ),
            row(
                "moisture before, during and after installation.",
                49.0,
                20.0,
                250.0,
            ),
            row(
                "3. Must be stored off the ground and under cover in",
                40.0,
                30.0,
                298.0,
            ),
            row(
                "accordance with the association's guide.",
                49.0,
                40.0,
                200.0,
            ),
        ];
        let b = assemble(&lines, 1.2);
        let marks: Vec<Option<&str>> = b.iter().map(|b| b.marker.as_deref()).collect();
        assert_eq!(
            marks,
            [Some("1."), Some("2."), Some("3.")],
            "{:?}",
            b.iter().map(Block::text).collect::<Vec<_>>()
        );
        assert_eq!(b[1].lines.len(), 2);
        // One numbered line alone counts off from nothing.
        let one = vec![
            row(
                "2. Avoid exposure to excessive, repetitive or continuous",
                40.0,
                0.0,
                300.0,
            ),
            row(
                "moisture before, during and after installation.",
                49.0,
                10.0,
                250.0,
            ),
        ];
        assert!(assemble(&one, 1.2).iter().all(|b| b.marker.is_none()));
    }

    #[test]
    fn a_count_carries_from_block_to_block() {
        // Item 3 stands a little further from item 2 than its lines do.
        let lines = vec![
            row(
                "1. Avoid exposure to sustained temperatures.",
                40.0,
                0.0,
                220.0,
            ),
            row(
                "2. Avoid exposure to excessive moisture.",
                40.0,
                10.0,
                210.0,
            ),
            row("3. Must be stored off the ground.", 40.0, 30.0, 180.0),
        ];
        let b = assemble(&lines, 1.0);
        assert_eq!(
            b.last().unwrap().marker.as_deref(),
            Some("3."),
            "{:?}",
            b.iter().map(Block::text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_paragraph_ended_by_hand_parts_from_the_next() {
        let mut lines = measure(0.0);
        lines.push(row("posible.", 40.0, 40.0, 75.0));
        lines.push(row(
            "El aislamiento está clasificado como material del",
            40.0,
            50.0,
            299.0,
        ));
        lines.push(row("tipo uno por la norma.", 40.0, 60.0, 140.0));
        let b = assemble_with_marks(&lines, 1.2, &[], &[], &[], 8.0);
        assert_eq!(
            b.len(),
            2,
            "{:?}",
            b.iter().map(Block::text).collect::<Vec<_>>()
        );
        assert!(b[1].text().starts_with("El aislamiento"));
    }

    #[test]
    fn a_line_a_word_short_is_a_rag_not_an_end() {
        // A designer's rag ends a line a word short of a greedy break: here
        // `Harmonic` and a space would have fit, with a little to spare.
        let mut lines = measure(0.0);
        lines.push(row(
            "like PostureFit SL, 8Z Pellicle, and",
            40.0,
            40.0,
            232.0,
        ));
        lines.push(row(
            "Harmonic 2 Tilt enable the sitter to the",
            40.0,
            50.0,
            299.0,
        ));
        let b = assemble_with_marks(&lines, 1.2, &[], &[], &[], 8.0);
        assert_eq!(
            b.len(),
            1,
            "{:?}",
            b.iter().map(Block::text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn stacked_bold_heads_part() {
        let bold = |text: &str, y: f64, x1: f64| {
            let mut line = row(text, 40.0, y, x1);
            line.bold = true;
            line
        };
        // A section's heading over its first subsection's.
        let heads = vec![
            bold("Materials and methods", 100.0, 130.0),
            bold("Animals", 110.0, 75.0),
        ];
        let mut column = measure(0.0);
        column.extend(heads.clone());
        let b = assemble_in_measure(&heads, &column, 1.2, &[], &[], &[], 8.0);
        assert_eq!(
            b.iter().map(Block::text).collect::<Vec<_>>(),
            ["Materials and methods", "Animals"]
        );
        // A title wrapped at the column's edge, or ending on a word that
        // runs on, stays whole.
        for title in [
            vec![
                bold(
                    "Inhibition of telomerase activity of human ovarian",
                    100.0,
                    295.0,
                ),
                bold("Cancer Cells by AZT", 110.0, 140.0),
            ],
            vec![
                bold("Effect of AZT on telomerase activity in", 100.0, 200.0),
                bold("HO-8910 Cells", 110.0, 110.0),
            ],
        ] {
            let mut column = measure(0.0);
            column.extend(title.clone());
            let b = assemble_in_measure(&title, &column, 1.2, &[], &[], &[], 8.0);
            assert_eq!(
                b.len(),
                1,
                "{:?}",
                b.iter().map(Block::text).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn entries_set_one_per_line_part() {
        let entries = vec![
            row("ASME A112.18.1/CSA B125.1", 40.0, 100.0, 150.0),
            row("NSF/ANSI/CAN 372", 40.0, 110.0, 115.0),
            row("NSF/ANSI/CAN 61", 40.0, 120.0, 110.0),
        ];
        let mut column = measure(0.0);
        column.extend(entries.clone());
        let b = assemble_in_measure(&entries, &column, 1.2, &[], &[], &[], 8.0);
        assert_eq!(
            b.len(),
            3,
            "{:?}",
            b.iter().map(Block::text).collect::<Vec<_>>()
        );
        // A narrow box's prose goes on in lower case, and stays whole.
        let prose = vec![
            row("Install this product according", 40.0, 100.0, 150.0),
            row("to the instructions included", 40.0, 110.0, 145.0),
            row("with the faucet.", 40.0, 120.0, 100.0),
        ];
        let mut column = measure(0.0);
        column.extend(prose.clone());
        let b = assemble_in_measure(&prose, &column, 1.2, &[], &[], &[], 8.0);
        assert_eq!(b.len(), 1);
        // A sentence broken before a title its line had no room for, the
        // line ending on `the`, goes on into it: no entry ends on `the`.
        let sentence = vec![
            row("The changes since the", 40.0, 100.0, 120.0),
            row("Annual Report are as follows:", 40.0, 110.0, 150.0),
        ];
        let mut column = measure(0.0);
        column.extend(sentence.clone());
        let b = assemble_in_measure(&sentence, &column, 1.2, &[], &[], &[], 8.0);
        assert_eq!(
            b.len(),
            1,
            "{:?}",
            b.iter().map(Block::text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_number_far_from_the_text_beside_it_is_no_marker() {
        // A drawing's callouts: `2` at the left of a stretch read alone,
        // and `6, 7` a long way off on its row.
        let mut two = l("2", 291.0, 0.0, 8.0);
        two.bbox.x1 = 296.0;
        let mut six = l("6, 7", 529.0, 0.0, 8.0);
        six.bbox.x1 = 545.0;
        let b = assemble(&[two.clone(), six], 1.2);
        assert!(
            b.iter().any(|b| b.text() == "2"),
            "{:?}",
            b.iter().map(Block::text).collect::<Vec<_>>()
        );
        // Set before its item's text, it marks the item.
        let mut item = l("Remove the front housing", 305.0, 0.0, 8.0);
        item.bbox.x1 = 420.0;
        let b = assemble(&[two, item], 1.2);
        assert!(!b.iter().any(|b| b.text() == "2"));
    }

    #[test]
    fn a_wrapped_title_read_alone_keeps_its_columns_measure() {
        // A two-line display title the region pass reads as a stretch of
        // its column: only the column's text shows its first line is full.
        let line = |t: &str, x0: f64, y: f64, x1: f64, fs: f32| {
            let mut l = l(t, x0, y, fs);
            l.bbox.x1 = x1;
            // Its ink: capitals and no descenders.
            l.bbox.y1 = y + fs as f64 * 0.72;
            l
        };
        // Its first line carries a registered sign in a lighter weight, so
        // the two lines differ in weight: only a line that ran out of room
        // goes on across that.
        let mut second = line("GYPSUM PANELS", 225.0, 111.0, 430.0, 30.0);
        second.bold = true;
        let title = vec![line("SHEETROCK® BRAND", 225.0, 84.0, 540.0, 30.0), second];
        let mut column = title.clone();
        for i in 0..6 {
            column.push(line(
                "body text of the column set to its measure",
                224.0,
                160.0 + 10.0 * i as f64,
                570.0,
                8.0,
            ));
        }
        let alone = assemble_with_marks(&title, 1.2, &[], &[], &[], 8.0);
        let measured = assemble_in_measure(&title, &column, 1.2, &[], &[], &[], 8.0);
        assert_eq!(alone.len(), 2);
        assert_eq!(measured.len(), 1);
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
    fn a_word_files_symbol_bullet_and_the_dingbat_boxes_mark_a_list() {
        // Word maps its default bullet to U+F0B7, the Symbol font's `•`,
        // and sets a tab after it.
        assert_eq!(
            strip_leading_bullet("\u{F0B7}\tHigh stiffness"),
            Some("High stiffness")
        );
        assert_eq!(strip_leading_bullet("\u{2751} Peg rack"), Some("Peg rack"));
        assert_eq!(
            strip_leading_bullet("\u{27A2} Install before use"),
            Some("Install before use")
        );
        // A private-use code that is a letter in the Symbol font is not
        // taken: U+F071 is `θ` there.
        assert_eq!(strip_leading_bullet("\u{F071} is the angle"), None);
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
