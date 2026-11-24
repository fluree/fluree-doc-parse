//! Header, footer, page-number and watermark detection.
//!
//! This must run **before** table assembly. Page furniture that leaks into the
//! body does more than add noise: on one regression document a leaked
//! `Acme Analytics, Inc. | AA00733622` landed inside a table cell, and the `|`
//! broke the markdown column count on 2 of 47 rows. It also corrupts NER — the
//! same footer contributes 15 phantom `Acme Analytics, Inc.` organisation
//! mentions and the watermark yields a spurious person/PII entity from an
//! embedded email address, when neither is document content.
//!
//! The signal is **cross-page repetition at a stable position**. A single page
//! carries no evidence; furniture is only identifiable in aggregate.

use crate::line::Line;
use std::collections::{BTreeMap, HashMap};

/// Fraction of page height from the top/bottom edge within which a line is
/// eligible to be furniture. Wide enough for multi-line headers and for a
/// footer/watermark pair sitting as low as 0.91 and 0.95 of page height.
pub const EDGE_BAND: f64 = 0.18;

/// Fraction of pages a repeated line must appear on. Deliberately low: a
/// document may change its running head between front matter and body, and a
/// footer that appears on only a third of pages is still a footer.
const MIN_PAGE_FRACTION: f64 = 0.30;

/// Pages a line must repeat on before repetition alone makes it furniture.
/// A shorter document's running lines are read by [`exact_running`].
const MIN_PAGES: usize = 3;

/// How closely two copies of a short document's running line must agree in
/// position, as a fraction of their size: a template sets them to the
/// point, and body text that happens to repeat does not land there.
const EXACT_PLACE: f64 = 0.1;

/// Positional agreement required across occurrences, as a fraction of page
/// height. Running heads sit at the same y on every page; a body line that
/// happens to repeat does not.
const POSITION_TOLERANCE: f64 = 0.02;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Furniture {
    Header,
    Footer,
    /// Page number: position is stable, text varies by page.
    PageNumber,
    /// Repeated at a stable position but not near an edge — a watermark or
    /// running side-note.
    Watermark,
}

/// Replace the digits in a line with a placeholder so `Page 1 of 15` and
/// `Page 13 of 15` compare equal. Without this, page numbers never repeat and
/// are never detected.
fn signature(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_num = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            if !in_num {
                out.push('#');
                in_num = true;
            }
        } else {
            in_num = false;
            out.push(c);
        }
    }
    out
}

/// Classify lines across a whole document. Returns, per page, the indices of
/// lines that are furniture and what kind, in the page's line order.
///
/// `pages` is one Vec<Line> per page, each paired with that page's height.
pub fn detect(pages: &[(Vec<Line>, f64)]) -> Vec<BTreeMap<usize, Furniture>> {
    detect_all(pages).0
}

/// As [`detect`], also returning, per page, the lines read as a short
/// document's exact copies ([`exact_running`]) rather than by repetition.
///
/// Those need no scrubbing from cells and blocks. They were found as lines
/// of their own on every page, or every page but one, so no copy merged
/// into a block or a table, and the page left out is where the words are
/// not running: a title page's title, or a product code in a table.
pub fn detect_with_exact(
    pages: &[(Vec<Line>, f64)],
) -> (
    Vec<BTreeMap<usize, Furniture>>,
    Vec<std::collections::HashSet<usize>>,
) {
    let (marks, _, exact) = detect_all(pages);
    (marks, exact)
}

/// Of the digit runs a page-number line carries, the one that is the page
/// number must take at least this many distinct values per page it appears
/// on. A footer's `Page 352` differs on nearly every page; the `16:00` row
/// label of a flow sheet repeats at a stable position on hundreds of pages
/// with six values between them, and is a clock, not a folio.
const FOLIO_DISTINCT_SHARE: f64 = 0.25;

/// As [`detect`], also returning each page's printed page number — its
/// folio — where the document has one.
///
/// Furniture is stripped from the body because a line that repeats on every
/// page says nothing about any one of them. The page number is the one
/// exception: it is the piece of furniture that identifies a *page*, and a
/// consumer reassembling a shuffled, duplicated chart from its footers has
/// no other way to. The folio is the digit run that varies most across the
/// occurrences of the most widespread page-number line; `Page 3 of 15`
/// yields `3`, and a footer carrying an MRN and an account number beside
/// the page number yields the page number. `None` on a page where the line
/// is absent or the document has no page-number line.
pub fn detect_with_folios(
    pages: &[(Vec<Line>, f64)],
) -> (Vec<BTreeMap<usize, Furniture>>, Vec<Option<String>>) {
    let (marks, folios, _) = detect_all(pages);
    (marks, folios)
}

/// Marks, folios, and the marks read as exact copies.
#[allow(clippy::type_complexity)]
fn detect_all(
    pages: &[(Vec<Line>, f64)],
) -> (
    Vec<BTreeMap<usize, Furniture>>,
    Vec<Option<String>>,
    Vec<std::collections::HashSet<usize>>,
) {
    let n_pages = pages.len();
    let mut out: Vec<BTreeMap<usize, Furniture>> = vec![BTreeMap::new(); n_pages];
    let mut folios: Vec<Option<String>> = vec![None; n_pages];
    let mut exact: Vec<std::collections::HashSet<usize>> = vec![Default::default(); n_pages];
    if n_pages < 2 {
        // Repetition is the only evidence there is, and one page has none.
        return (out, folios, exact);
    }

    // signature -> occurrences (page, line index, relative y)
    let mut groups: HashMap<String, Vec<(usize, usize, f64, String)>> = HashMap::new();
    for (pi, (lines, height)) in pages.iter().enumerate() {
        if *height <= 0.0 {
            continue;
        }
        for (li, l) in lines.iter().enumerate() {
            let rel = l.bbox.y0 / height;
            // Digit-collapsing is scoped to the edge bands, where pagination
            // lives. Applied everywhere it is too permissive: body text that
            // differs only by a number ("...on page 3" vs "...on page 4")
            // would collapse to one signature and be stripped as furniture.
            // Away from the edges, only exactly-repeated text counts, which is
            // what a watermark or running side-note actually is.
            let in_edge_band = !(EDGE_BAND..=1.0 - EDGE_BAND).contains(&rel);
            let key = if in_edge_band {
                signature(&l.text)
            } else {
                l.text.clone()
            };
            groups
                .entry(key)
                .or_default()
                .push((pi, li, rel, l.text.clone()));
        }
    }

    let needed = ((n_pages as f64 * MIN_PAGE_FRACTION).ceil() as usize).max(MIN_PAGES);

    // The page-number group the folios come from: (pages it covers, distinct
    // values of its varying run), then the per-page values.
    let mut folio_sources: Vec<FolioSource> = Vec::new();

    for (sig, occ) in groups {
        if sig.trim().is_empty() {
            continue;
        }
        // Count distinct pages, not occurrences: a line repeated many times on
        // one page is not furniture.
        let mut pages_seen: Vec<usize> = occ.iter().map(|o| o.0).collect();
        pages_seen.sort_unstable();
        pages_seen.dedup();

        // Position must be stable. Use the median as the reference so a single
        // outlier occurrence does not disqualify the group.
        let mut ys: Vec<f64> = occ.iter().map(|o| o.2).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = ys[ys.len() / 2];

        let stable = occ
            .iter()
            .filter(|o| (o.2 - median).abs() < POSITION_TOLERANCE)
            .count();
        if pages_seen.len() < needed || stable < needed {
            // Too few pages for repetition alone: a two-page data sheet's
            // letterhead, or a three-page one's head over the pages after
            // its title page.
            if let Some((kind, hits)) = exact_running(pages, &occ) {
                for (pi, li, _) in &hits {
                    out[*pi].insert(*li, kind);
                    exact[*pi].insert(*li);
                }
                if kind == Furniture::PageNumber {
                    let stable: Vec<(usize, &str)> =
                        hits.iter().map(|(pi, _, t)| (*pi, *t)).collect();
                    offer_folios(&mut folio_sources, stable.len(), &stable);
                }
            }
            continue;
        }

        // A page number is a furniture line whose digits *change* between
        // pages — that is what distinguishes "Page 13 of 15" from a static
        // running head that merely happens to contain a number.
        let mut raw: Vec<&str> = occ.iter().map(|o| o.3.as_str()).collect();
        raw.sort_unstable();
        raw.dedup();
        let digits_vary = sig.contains('#') && raw.len() > 1;

        // A number alone at one place on page after page is a page number
        // only if it counts the pages. An install guide's step numbers, a
        // figure's callouts and a chart's axis labels sit at the same place
        // too, and say something different there.
        if digits_vary && bare_number(&sig) {
            let stable: Vec<(usize, &str)> = occ
                .iter()
                .filter(|o| (o.2 - median).abs() < POSITION_TOLERANCE)
                .map(|o| (o.0, o.3.as_str()))
                .collect();
            if !advances(&stable) {
                continue;
            }
        }

        let kind = classify(median, digits_vary);

        // A watermark appears once per page. `发布机构:` ("issuing agency")
        // occurs four times per page in one government filing as a field
        // label in a record listing, at stable positions — indistinguishable
        // from a watermark by position alone, but clearly content. Requiring
        // at-most-once-per-page separates them. Edge-band furniture is exempt:
        // a header and footer legitimately share a signature on some layouts.
        if kind == Furniture::Watermark {
            let mut per_page: HashMap<usize, usize> = HashMap::new();
            for o in &occ {
                *per_page.entry(o.0).or_default() += 1;
            }
            if per_page.values().any(|&n| n > 1) {
                continue;
            }
            // Repetition is not enough away from the edges: templated pages
            // repeat real text there (a spec table's labels, a box's
            // heading, the close of a standard sentence). A watermark also
            // shows itself as one: turned, laid on other text, or standing
            // beside all of it in a margin.
            let shown = occ
                .iter()
                .filter(|o| (o.2 - median).abs() < POSITION_TOLERANCE)
                .filter(|o| watermark_shown(&pages[o.0].0, o.1))
                .count();
            if shown < needed {
                continue;
            }
        }
        for (pi, li, rel, _) in &occ {
            if (rel - median).abs() < POSITION_TOLERANCE {
                out[*pi].insert(*li, kind);
            }
        }

        if kind == Furniture::PageNumber {
            let stable: Vec<(usize, &str)> = occ
                .iter()
                .filter(|o| (o.2 - median).abs() < POSITION_TOLERANCE)
                .map(|o| (o.0, o.3.as_str()))
                .collect();
            offer_folios(&mut folio_sources, pages_seen.len(), &stable);
        }
    }

    // The page-number line that covers the most pages supplies the folios.
    // A page it leaves without one takes it from another: a guide in three
    // languages words its `1 of 3` three ways. Each must be a page's number
    // (see [`FolioSource`]); the widest line is no more one for its width.
    folio_sources.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for (_, values, numbers_pages) in folio_sources {
        if !numbers_pages {
            continue;
        }
        let bare: Vec<bool> = folios.iter().map(Option::is_none).collect();
        for (pi, v) in values {
            // First occurrence on the page wins: a layout that prints the
            // number in both header and footer says it twice.
            if bare[pi] {
                folios[pi].get_or_insert(v);
            }
        }
    }

    // A line read by repetition as well keeps its scrub.
    for (pi, set) in exact.iter_mut().enumerate() {
        set.retain(|li| out[pi].contains_key(li));
    }
    (out, folios, exact)
}

/// A page-number line the folios may come from: (pages it covers, distinct
/// values of its varying run), the per-page values, and whether its number
/// is a page's.
///
/// A line whose digits vary near the top or foot of page after page is not
/// a page's number for that alone. A data sheet's spec table opens each
/// page with its drives' capacities (`26TB`, `16TB`, `8TB`), and a
/// catalogue's year ranges (`1991-1998`) stand at its foot, several to a
/// page; each was taken for the folio on every page it reached. A page's
/// number either counts the pages (see [`advances`]) or says it is one
/// (see [`says_page`]): a chart whose pages were shuffled and printed again
/// numbers them `Page 352` where they now stand seventh, and reassembling
/// such a chart is what folios are for.
type FolioSource = ((usize, usize), Vec<(usize, String)>, bool);

/// Offer a page-number line's occurrences as a folio source.
fn offer_folios(sources: &mut Vec<FolioSource>, covers: usize, stable: &[(usize, &str)]) {
    if let Some((distinct, values)) = folio_run(stable) {
        let numbers_pages = advances(stable) || stable.iter().all(|(_, t)| says_page(t));
        sources.push(((covers, distinct), values, numbers_pages));
    }
}

/// The words a page's number is set with, in the languages documents print
/// them in: `Page 3`, `p. 3`, `Página 3`, `pág. 3`, `Seite 3`, `Blatt 3`,
/// `Sida 3`, `Strona 3`.
const PAGE_WORDS: &[&str] = &[
    "page", "pages", "pg", "p", "pp", "pag", "pág", "pagina", "página", "seite", "blatt", "sida",
    "side", "strona", "str",
];

/// The words that set a page's number against the count of pages: `3 of 15`,
/// `3 sur 3`, `3 de 9`, `3 von 4`, `3 di 5`, `3 da 5`, `3 z 8`, `3 van 4`,
/// `3 av 4`.
const OF_WORDS: &[&str] = &["of", "sur", "de", "von", "di", "da", "z", "van", "av"];

/// Whether a line names its number a page's: `Page 3`, `p. 3`, `Pg 3`,
/// `3 of 15`, `Página 3`, `3 sur 3`, `Seite 3`.
fn says_page(text: &str) -> bool {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let number = |w: &str| w.chars().all(|c| c.is_ascii_digit());
    words.iter().any(|w| PAGE_WORDS.contains(&w.as_str()))
        || words
            .windows(3)
            .any(|w| number(&w[0]) && OF_WORDS.contains(&w[1].as_str()) && number(&w[2]))
}

/// A running line's copies: page, line index, text.
type Copies<'a> = Vec<(usize, usize, &'a str)>;

/// A short document's running line, read from how exactly its copies
/// agree, since there are too few of them for repetition alone to say.
///
/// A two-page data sheet's letterhead is on both pages, and a three-page
/// one's running head on the two after its title page, where the title
/// block says the same thing in its own place. So the line must be on every
/// page or every page but one, in the top or bottom band, and its copies
/// must agree as only a template's do: the same text, size and weight, at
/// the same place to the point. A page number may differ, if it counts the
/// pages and the line says nothing else (`Page 1 of 2`); `Table 1` over
/// `Table 2` is a heading each time.
///
/// Two things a running line is that text which merely repeats is not:
///
/// - **Restated where it is not running.** The page left out prints the
///   same words somewhere, as a title page sets its running head as its
///   title. Two signature pages at the end of an agreement repeat their
///   block line for line, and the agreement's first page says none of it.
/// - **Apart from the text beside it**, or set with lines that repeat too.
///   A paragraph's first line goes on, at its size and leading, into a
///   second that differs from page to page: a legal text opens two
///   paragraphs with one sentence, at the top of two pages. A letterhead of
///   several lines repeats whole.
///
/// The occurrences that agree, with the kind they are.
fn exact_running<'a>(
    pages: &[(Vec<Line>, f64)],
    occ: &'a [(usize, usize, f64, String)],
) -> Option<(Furniture, Copies<'a>)> {
    let want = pages.len().saturating_sub(1).max(2);
    let line = |o: &(usize, usize, f64, String)| &pages[o.0].0[o.1];
    let mut best: Option<Vec<&(usize, usize, f64, String)>> = None;
    for r in occ {
        let mut hits: Vec<&(usize, usize, f64, String)> = Vec::new();
        for o in occ {
            if !hits.iter().any(|h| h.0 == o.0) && same_place(line(r), line(o)) {
                hits.push(o);
            }
        }
        if hits.len() >= want && best.as_ref().is_none_or(|b| hits.len() > b.len()) {
            best = Some(hits);
        }
    }
    let hits = best?;
    let on: Vec<usize> = hits.iter().map(|h| h.0).collect();
    let words = |pi: usize| {
        pages[pi]
            .0
            .iter()
            .flat_map(|l| l.text.split_whitespace())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let says: String = hits[0].3.split_whitespace().collect::<Vec<_>>().join(" ");
    if (0..pages.len()).any(|pi| !on.contains(&pi) && !words(pi).contains(&says)) {
        return None;
    }
    for h in &hits {
        let l = line(h);
        for nb in pages[h.0].0.iter().filter(|nb| runs_on(l, nb)) {
            let copied = |g: &&(usize, usize, f64, String)| {
                g.0 == h.0
                    || pages[g.0]
                        .0
                        .iter()
                        .any(|m| m.text == nb.text && same_place(m, nb))
            };
            if !hits.iter().all(copied) {
                return None;
            }
        }
    }
    let mut texts: Vec<&str> = hits.iter().map(|h| h.3.as_str()).collect();
    texts.sort_unstable();
    texts.dedup();
    let digits_vary = texts.len() > 1;
    if digits_vary {
        let numbers: Vec<(usize, &str)> = hits.iter().map(|h| (h.0, h.3.as_str())).collect();
        if texts.iter().any(|t| names_something(t)) || !advances(&numbers) {
            return None;
        }
    }
    let mut ys: Vec<f64> = hits.iter().map(|h| h.2).collect();
    ys.sort_by(f64::total_cmp);
    let kind = classify(ys[ys.len() / 2], digits_vary);
    if kind == Furniture::Watermark {
        return None;
    }
    Some((
        kind,
        hits.iter().map(|h| (h.0, h.1, h.3.as_str())).collect(),
    ))
}

/// Two lines set as copies of one: the same size and weight, the same top
/// and bottom, and the same left edge, right edge or centre, each to a tenth
/// of their size.
fn same_place(a: &Line, b: &Line) -> bool {
    let size = f64::from(a.font_size.max(b.font_size)).max(1.0);
    let near = |p: f64, q: f64| (p - q).abs() <= EXACT_PLACE * size;
    let (ca, cb) = ((a.bbox.x0 + a.bbox.x1) / 2.0, (b.bbox.x0 + b.bbox.x1) / 2.0);
    a.rotation_bucket == b.rotation_bucket
        && f64::from((a.font_size - b.font_size).abs()) <= EXACT_PLACE * size
        && a.bold == b.bold
        && near(a.bbox.y0, b.bbox.y0)
        && near(a.bbox.y1, b.bbox.y1)
        && (near(a.bbox.x0, b.bbox.x0) || near(a.bbox.x1, b.bbox.x1) || near(ca, cb))
}

/// `b` is set beside `a` as the next or last line of one paragraph: at its
/// size, sharing its width, a line's leading above or below it.
fn runs_on(a: &Line, b: &Line) -> bool {
    let size = f64::from(a.font_size.max(b.font_size)).max(1.0);
    let dy = (b.bbox.y0 - a.bbox.y0).abs();
    !std::ptr::eq(a, b)
        && a.rotation_bucket == b.rotation_bucket
        && f64::from((a.font_size - b.font_size).abs()) <= 0.12 * size
        && a.bbox.x1.min(b.bbox.x1) > a.bbox.x0.max(b.bbox.x0)
        && dy > 0.0
        && dy <= RUN_ON_LEADING * size
}

/// The widest line spacing, in ems, at which two lines read as one
/// paragraph's: body leading with room to spare, short of a paragraph's
/// break.
const RUN_ON_LEADING: f64 = 1.6;

/// A line that is one number and nothing else, by its signature: `3`,
/// `- 3 -`, `3.`.
fn bare_number(sig: &str) -> bool {
    sig.matches('#').count() == 1 && !sig.chars().any(char::is_alphanumeric)
}

/// Whether a line's number counts the pages: printed once on a page, and
/// from one page it is on to the next, going up by as many pages as lie
/// between. On most of them, not all: a document in parts numbers each
/// from 1 (an install guide's `1 of 3` again in each language), and an
/// unnumbered insert skips one.
///
/// The number is the line's digit run that varies most.
fn advances(occ: &[(usize, &str)]) -> bool {
    let runs: Vec<Vec<String>> = occ.iter().map(|(_, t)| digit_runs(t)).collect();
    let Some(n_runs) = runs.iter().map(Vec::len).min() else {
        return false;
    };
    let Some(j) = (0..n_runs).max_by_key(|&j| {
        let mut v: Vec<&str> = runs.iter().map(|r| r[j].as_str()).collect();
        v.sort_unstable();
        v.dedup();
        v.len()
    }) else {
        return false;
    };
    let mut by_page: std::collections::BTreeMap<usize, Vec<i64>> = Default::default();
    for ((pi, _), r) in occ.iter().zip(&runs) {
        let Ok(v) = r[j].parse::<i64>() else {
            return false;
        };
        by_page.entry(*pi).or_default().push(v);
    }
    // Two numbers at the place on one page are not that page's number.
    let twice = by_page.values().filter(|v| v.len() > 1).count();
    if twice * 4 > by_page.len() {
        return false;
    }
    let pages: Vec<(&usize, &Vec<i64>)> = by_page.iter().collect();
    let pairs = pages.len().saturating_sub(1);
    let up = pages
        .windows(2)
        .filter(|w| {
            let step = (*w[1].0 - *w[0].0) as i64;
            w[0].1.iter().any(|a| w[1].1.iter().any(|b| b - a == step))
        })
        .count();
    pairs > 0 && up * 2 >= pairs
}

/// The digit runs of a line, in order.
fn digit_runs(text: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            runs.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        runs.push(cur);
    }
    runs
}

/// Which digit run of a page-number line is the page number, and its value
/// on each page.
///
/// The occurrences share a signature, so they carry the same number of
/// runs; the folio is the run with the most distinct values across pages —
/// the `352` of `MRN-2093471 | ACCT-40218563 Page 352`, not the constants
/// beside it. `None` when no run varies enough to be a page number (see
/// [`FOLIO_DISTINCT_SHARE`]): a clock label that repeats at one position
/// carries digits that change, but not once per page.
fn folio_run(occ: &[(usize, &str)]) -> Option<(usize, Vec<(usize, String)>)> {
    let runs: Vec<Vec<String>> = occ.iter().map(|(_, t)| digit_runs(t)).collect();
    let n_runs = runs.iter().map(Vec::len).min()?;
    let mut pages: Vec<usize> = occ.iter().map(|o| o.0).collect();
    pages.sort_unstable();
    pages.dedup();
    let mut best: Option<(usize, usize)> = None; // (distinct, run index)
    for j in 0..n_runs {
        let mut vals: Vec<&str> = runs.iter().map(|r| r[j].as_str()).collect();
        vals.sort_unstable();
        vals.dedup();
        if best.is_none_or(|(d, _)| vals.len() > d) {
            best = Some((vals.len(), j));
        }
    }
    let (distinct, j) = best?;
    if distinct < 2 || (distinct as f64) < (pages.len() as f64) * FOLIO_DISTINCT_SHARE {
        return None;
    }
    let values = occ
        .iter()
        .zip(&runs)
        .map(|((pi, _), r)| (*pi, r[j].clone()))
        .collect();
    Some((distinct, values))
}

/// Line `li` of a page shows itself as a watermark rather than as text
/// that repeats: it is turned, it lies on another line, or it stands
/// beside every other line, sharing none of their width, as a margin note
/// does.
fn watermark_shown(lines: &[Line], li: usize) -> bool {
    let l = &lines[li];
    if l.rotation_bucket != 0 {
        return true;
    }
    let others = lines.iter().enumerate().filter(|(j, _)| *j != li);
    let mut beside = true;
    for (_, o) in others {
        let w = l.bbox.x1.min(o.bbox.x1) - l.bbox.x0.max(o.bbox.x0);
        if w > 0.0 {
            beside = false;
            let h = l.bbox.y1.min(o.bbox.y1) - l.bbox.y0.max(o.bbox.y0);
            let smaller = (l.bbox.width() * l.bbox.height()).min(o.bbox.width() * o.bbox.height());
            if h > 0.0 && smaller > 0.0 && w * h / smaller > 0.3 {
                return true;
            }
        }
    }
    beside
}

fn classify(rel_y: f64, digits_vary: bool) -> Furniture {
    if digits_vary {
        return Furniture::PageNumber;
    }
    if rel_y < EDGE_BAND {
        Furniture::Header
    } else if rel_y > 1.0 - EDGE_BAND {
        Furniture::Footer
    } else {
        Furniture::Watermark
    }
}

/// A running header or footer line, where one page printed it.
#[derive(Debug, Clone, PartialEq)]
pub struct Running {
    pub page: usize,
    pub bbox: crate::geom::BBox,
    pub text: String,
    /// Printed in the top half of the page: a header, not a footer.
    pub top: bool,
}

/// Whether a furniture line says anything besides where the reader is.
/// `Page 3 of 15`, `3 sur 3`, `Página 2 de 9` and `- 3 -` do not; `ACME
/// LABS`, `Revision 4` and `Form QA-12 Page 3` do. Nor does `P age 3`, set
/// with its letters apart. The paging words are any language's a document
/// numbers its pages in ([`PAGE_WORDS`], [`OF_WORDS`]): read in English
/// alone, a French report's `3 sur 3` named something, and its last page's
/// number was kept as the document's footer.
pub fn names_something(text: &str) -> bool {
    let paging = |w: &str| {
        let w = w.to_lowercase();
        PAGE_WORDS.contains(&w.as_str()) || OF_WORDS.contains(&w.as_str())
    };
    let names = |text: &str| {
        text.split(|c: char| !c.is_alphabetic())
            .any(|w| w.chars().count() >= 2 && !paging(w))
    };
    let closed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    names(text) && names(&closed)
}

/// Whether a furniture line is a header or footer that says something
/// about the document. A watermark is neither.
pub fn identifies(text: &str, kind: Furniture) -> bool {
    kind != Furniture::Watermark && says_something(text, kind == Furniture::PageNumber)
}

/// Whether a running line says something about the document, given whether
/// its digits change from page to page.
///
/// Only a line whose number changes can be a page number, and one that
/// does must also [name something](names_something) besides the page. A
/// line the same on every page is no page number however it reads: a
/// telephone number, a revision year, a form number.
pub fn says_something(text: &str, digits_vary: bool) -> bool {
    if digits_vary {
        names_something(text)
    } else {
        text.chars().any(char::is_alphanumeric)
    }
}

/// Put a document's running header and footer into its text, once.
///
/// Furniture is stripped from every page because a line that repeats on all
/// of them says nothing about any one. But it does say something about the
/// document: the running header is often the only place a document names
/// its company, its form number or its revision, and a reader of the text
/// should find them there as a reader of the page does.
///
/// As a Word file's are read: the header once, before the body, and the
/// footer once, after it, marked `header` and `footer`. The header is the
/// one printed on `first`, the first page converted, and the footer the one
/// on `last`; a line whose page number changes is that page's copy. A
/// running head that starts later in the document (a report's chapter
/// title, the date on its even pages) stays out of the body, where it would
/// stand between two pages' text: it is in the running text with the rest.
///
/// Run after anything that replaces a page's elements, so a page read
/// again keeps its header. Element ids are renumbered.
pub fn keep_running(
    elements: &mut Vec<crate::document::Element>,
    running: &[Running],
    first: usize,
    last: usize,
) {
    use crate::document::Element;
    let mut seen = std::collections::HashSet::new();
    let mut lines = |top: bool, page: usize| -> Vec<Element> {
        running
            .iter()
            .filter(|r| r.top == top && r.page == page && seen.insert(signature(&r.text)))
            .map(|r| Element {
                page: r.page,
                bbox: Some(r.bbox),
                ..Element::new(
                    "doco:Paragraph",
                    r.text.trim(),
                    "rust",
                    if top { "header" } else { "footer" },
                )
            })
            .collect()
    };
    let (head, foot) = (lines(true, first), lines(false, last));
    if head.is_empty() && foot.is_empty() {
        return;
    }
    let after = elements
        .iter()
        .rposition(|e| e.page <= last)
        .map_or(0, |i| i + 1);
    elements.splice(after..after, foot);
    let before = elements
        .iter()
        .position(|e| e.page >= first)
        .unwrap_or(elements.len());
    elements.splice(before..before, head);
    // A continued table names the part before it by id.
    let mut renamed = std::collections::HashMap::new();
    for (i, e) in elements.iter_mut().enumerate() {
        let id = format!("elem-{:05}", i + 1);
        let old = std::mem::replace(&mut e.id, id.clone());
        renamed.insert(old, id);
    }
    for e in elements.iter_mut() {
        if let Some(new) = e.continues.as_ref().and_then(|c| renamed.get(c)) {
            e.continues = Some(new.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::BBox;

    fn line(text: &str, y: f64) -> Line {
        Line {
            text: text.into(),
            bbox: BBox {
                x0: 10.0,
                y0: y,
                x1: 200.0,
                y1: y + 12.0,
            },
            page: 0,
            rotation_bucket: 0,
            glyphs: vec![],
            font_size: 10.0,
            bold: false,
            opens_with_script: false,
            opens_bold: false,
        }
    }

    fn doc(n: usize, f: impl Fn(usize) -> Vec<Line>) -> Vec<(Vec<Line>, f64)> {
        (0..n).map(|i| (f(i), 1000.0)).collect()
    }

    #[test]
    fn detects_repeated_footer() {
        let pages = doc(10, |i| {
            vec![
                line(&format!("Body text unique to page {i}"), 400.0),
                line("Acme Analytics, Inc. | AA00733622", 950.0),
            ]
        });
        let r = detect(&pages);
        assert_eq!(r[0].get(&1), Some(&Furniture::Footer));
        assert_eq!(r[0].get(&0), None, "body must not be flagged");
    }

    #[test]
    fn detects_page_number_despite_varying_digits() {
        let pages = doc(10, |i| vec![line(&format!("Page {} of 10", i + 1), 950.0)]);
        let r = detect(&pages);
        assert_eq!(r[3].get(&0), Some(&Furniture::PageNumber));
    }

    #[test]
    fn the_folio_is_the_digit_run_that_varies() {
        let pages = doc(10, |i| {
            vec![line(
                &format!("MRN-2093471 | ACCT-40218563   Page {}", i + 1),
                950.0,
            )]
        });
        let (marks, folios) = detect_with_folios(&pages);
        assert_eq!(marks[3].get(&0), Some(&Furniture::PageNumber));
        let got: Vec<Option<String>> = folios;
        assert_eq!(got[0].as_deref(), Some("1"));
        assert_eq!(got[9].as_deref(), Some("10"));
    }

    #[test]
    fn page_n_of_m_yields_n() {
        let pages = doc(10, |i| vec![line(&format!("Page {} of 10", i + 1), 950.0)]);
        let (_, folios) = detect_with_folios(&pages);
        assert_eq!(folios[4].as_deref(), Some("5"));
    }

    #[test]
    fn a_clock_label_with_few_values_is_not_a_folio() {
        // Six clock times cycling at one position across many pages: the
        // digits vary, but not once per page.
        let times = ["00:00", "04:00", "08:00", "12:00", "16:00", "20:00"];
        let pages = doc(30, |i| vec![line(times[i % 6], 60.0)]);
        let (marks, folios) = detect_with_folios(&pages);
        assert_eq!(marks[0].get(&0), Some(&Furniture::PageNumber));
        assert!(folios.iter().all(Option::is_none));
    }

    #[test]
    fn the_widest_page_number_line_supplies_the_folio() {
        // A footer on every page and a clock label on most: the footer's
        // number is the page's.
        let times = ["00:00", "04:00", "08:00", "12:00", "16:00", "20:00"];
        let pages = doc(30, |i| {
            vec![
                line(times[i % 6], 60.0),
                line(&format!("Report — Page {}", i + 1), 950.0),
            ]
        });
        let (_, folios) = detect_with_folios(&pages);
        assert_eq!(folios[7].as_deref(), Some("8"));
    }

    #[test]
    fn a_tables_figures_near_the_top_are_no_folio() {
        // A data sheet's spec table opens each page after the first with
        // its largest drive: the figures vary, and fall.
        let pages = doc(4, |i| match i {
            0 => vec![line("Data sheet", 40.0)],
            _ => vec![line(&format!("{}TB", [26, 16, 8][i - 1]), 160.0)],
        });
        let (_, folios) = detect_with_folios(&pages);
        assert!(folios.iter().all(Option::is_none), "got {folios:?}");
    }

    #[test]
    fn year_ranges_at_the_foot_leave_the_footer_its_folio() {
        // Year ranges stand at the foot several to a page and on more
        // pages than either form of the footer; the footer counts the
        // pages and is the folio.
        let pages = doc(12, |i| {
            let n = i + 37;
            let footer = if n % 2 == 1 {
                format!("© Example 2012/2013 | {n}")
            } else {
                format!("{n} | © Example 2012/2013")
            };
            vec![
                line(&format!("{}-{}", 1990 + i, 1998 + i), 900.0),
                line(&format!("{}-{}", 1991 + i, 2002 + i), 900.5),
                line(&footer, 960.0),
            ]
        });
        let (_, folios) = detect_with_folios(&pages);
        assert_eq!(folios[2].as_deref(), Some("39"));
        assert_eq!(folios[3].as_deref(), Some("40"));
    }

    #[test]
    fn a_shuffled_charts_page_numbers_are_its_folios() {
        // Pages shuffled and some printed twice: the numbers do not count
        // the pages, but each says it is a page's.
        let order = [12, 3, 3, 40, 7, 19, 2, 19, 25, 8];
        let pages = doc(order.len(), |i| {
            vec![line(&format!("Page {}", order[i]), 960.0)]
        });
        let (_, folios) = detect_with_folios(&pages);
        let got: Vec<Option<&str>> = folios.iter().map(|f| f.as_deref()).collect();
        let want: Vec<Option<String>> = order.iter().map(|n| Some(n.to_string())).collect();
        assert_eq!(got, want.iter().map(|w| w.as_deref()).collect::<Vec<_>>());
    }

    #[test]
    fn a_page_says_it_is_one() {
        assert!(says_page("Page 3"));
        assert!(says_page("MRN-2093471 | ACCT-40218563 Page 352"));
        assert!(says_page("p. 14"));
        assert!(says_page("3 of 15"));
        assert!(says_page("Página 4"));
        assert!(!says_page("26TB"));
        assert!(!says_page("1991-1998"));
        assert!(!says_page("© Example 2012/2013 | 39"));
        assert!(!says_page("Table 3 of the report"));
    }

    #[test]
    fn a_document_without_page_numbers_has_no_folios() {
        let pages = doc(10, |_| vec![line("Running head", 20.0)]);
        let (_, folios) = detect_with_folios(&pages);
        assert!(folios.iter().all(Option::is_none));
    }

    #[test]
    fn detects_watermark_away_from_edges() {
        let pages = doc(10, |_| vec![line("CONFIDENTIAL DRAFT", 500.0)]);
        let r = detect(&pages);
        assert_eq!(r[0].get(&0), Some(&Furniture::Watermark));
    }

    #[test]
    fn ignores_repeated_text_at_unstable_positions() {
        // Same text, but it drifts down the page — body content, not furniture.
        let pages = doc(10, |i| vec![line("See note", 100.0 + i as f64 * 60.0)]);
        let r = detect(&pages);
        assert!(
            r.iter().all(|m| m.is_empty()),
            "drifting text is not furniture"
        );
    }

    #[test]
    fn repeated_field_label_is_not_a_watermark() {
        // Four occurrences per page at stable positions: a record-listing field
        // label, not a watermark. Position alone cannot tell them apart.
        let pages = doc(10, |_| {
            vec![
                line("Issuer:", 200.0),
                line("Issuer:", 400.0),
                line("Issuer:", 600.0),
            ]
        });
        assert!(detect(&pages).iter().all(|m| m.is_empty()));
    }

    #[test]
    fn short_documents_are_left_alone() {
        // Two pages give little repetition evidence; a line that merely
        // repeats, a little apart, may be real content.
        let pages = doc(2, |i| vec![line("Header", 20.0 + i as f64 * 4.0)]);
        assert!(detect(&pages).iter().all(|m| m.is_empty()));
        let pages = doc(2, |i| {
            let mut l = line("Header", 20.0);
            l.bbox.x0 += i as f64 * 40.0;
            l.bbox.x1 += i as f64 * 40.0;
            vec![l]
        });
        assert!(detect(&pages).iter().all(|m| m.is_empty()));
        let pages = doc(2, |i| {
            let mut l = line("Header", 20.0);
            l.bold = i == 1;
            vec![l]
        });
        assert!(detect(&pages).iter().all(|m| m.is_empty()));
    }

    #[test]
    fn a_short_documents_running_lines_are_copies_to_the_point() {
        // A two-page data sheet's letterhead and footer, set by its template.
        let pages = doc(2, |i| {
            vec![
                line("Flexible Packaging Films", 58.3),
                line(&format!("Body text unique to page {i}"), 400.0),
                line("7770 North Central Drive / Lewis Center", 960.0),
            ]
        });
        let r = detect(&pages);
        for m in &r {
            assert_eq!(m.get(&0), Some(&Furniture::Header));
            assert_eq!(m.get(&1), None);
            assert_eq!(m.get(&2), Some(&Furniture::Footer));
        }
        // Three pages: the head over the two after the title page, where
        // the title block prints the same words in its own place.
        let pages = doc(3, |i| {
            let mut head = line("Guild | K-R33912", 36.0);
            if i == 0 {
                head = line("Guild | K-R33912", 74.0);
                head.font_size = 25.0;
            } else {
                head.bbox.x0 = 400.0;
                head.bbox.x1 = 580.0;
            }
            vec![head, line(&format!("Body text unique to page {i}"), 400.0)]
        });
        let r = detect(&pages);
        assert_eq!(r[0].get(&0), None, "the title page's copy is its title");
        assert_eq!(r[1].get(&0), Some(&Furniture::Header));
        assert_eq!(r[2].get(&0), Some(&Furniture::Header));
        // On one page of three, it is not running.
        let pages = doc(3, |i| {
            let t = if i == 1 { "Notes" } else { "Body" };
            vec![line(&format!("{t} {i}"), 36.0)]
        });
        assert!(detect(&pages).iter().all(|m| m.is_empty()));
    }

    #[test]
    fn a_short_documents_exact_copies_are_reported_as_such() {
        let pages = doc(2, |_| vec![line("Flexible Packaging Films", 58.3)]);
        let (marks, exact) = detect_with_exact(&pages);
        assert_eq!(marks[1].get(&0), Some(&Furniture::Header));
        assert!(exact.iter().all(|e| e.contains(&0)));
        // Read by repetition, it is not.
        let pages = doc(10, |_| vec![line("Running head", 20.0)]);
        let (marks, exact) = detect_with_exact(&pages);
        assert_eq!(marks[3].get(&0), Some(&Furniture::Header));
        assert!(exact.iter().all(|e| e.is_empty()));
    }

    #[test]
    fn a_short_documents_repeated_text_is_not_running_unless_it_stands_apart() {
        // Two paragraphs a legal text opens with one sentence, at the top
        // of its second and third pages: the next lines differ.
        let pages = doc(3, |i| {
            let mut v = vec![line(&format!("Body text unique to page {i}"), 400.0)];
            if i > 0 {
                v.push(line("Where either party fails to perform any", 40.0));
                v.push(line(
                    &format!("obligation under clause {i} of this agreement"),
                    52.0,
                ));
            }
            v
        });
        assert!(detect(&pages).iter().all(|m| m.is_empty()));
        // A letterhead of two lines repeats whole.
        let pages = doc(2, |i| {
            vec![
                line("MF Supply", 40.0),
                line("(888) 555-0100", 52.0),
                line(&format!("Body text unique to page {i}"), 400.0),
            ]
        });
        let r = detect(&pages);
        assert!(r.iter().all(|m| m.len() == 2), "{r:?}");
        // Two signature pages after an agreement: the page left out never
        // says it.
        let pages = doc(3, |i| {
            if i == 0 {
                vec![line("The parties agree as follows", 400.0)]
            } else {
                vec![line("Signed on behalf of the parties below.", 40.0)]
            }
        });
        assert!(detect(&pages).iter().all(|m| m.is_empty()));
    }

    #[test]
    fn a_short_documents_page_number_must_count_the_pages() {
        let pages = doc(2, |i| vec![line(&format!("Page {} of 2", i + 1), 950.0)]);
        let (marks, folios) = detect_with_folios(&pages);
        assert_eq!(marks[1].get(&0), Some(&Furniture::PageNumber));
        assert_eq!(folios[1].as_deref(), Some("2"));
        // A heading over each page names something, and is kept.
        let pages = doc(2, |i| vec![line(&format!("Table {}", i + 1), 40.0)]);
        assert!(detect(&pages).iter().all(|m| m.is_empty()));
        // A number that does not go up with the page is not its number.
        let pages = doc(2, |i| vec![line(&format!("{}", 5 - i), 40.0)]);
        assert!(detect(&pages).iter().all(|m| m.is_empty()));
    }

    #[test]
    fn a_bare_number_is_a_page_number_only_if_it_counts_the_pages() {
        // An install guide in three languages, three pages each: step
        // numbers at the top of the second and third pages of each, and
        // the page numbers at the foot, from 1 in each language.
        let pages = doc(9, |i| {
            let mut v = vec![line(&format!("{} of 3", i % 3 + 1), 950.0)];
            match i % 3 {
                1 => {
                    v.push(line("3", 44.7));
                    let mut four = line("4", 44.8);
                    four.bbox.x0 = 370.0;
                    v.push(four);
                }
                2 => v.push(line("5", 45.0)),
                _ => {}
            }
            v
        });
        let (marks, folios) = detect_with_folios(&pages);
        for (i, m) in marks.iter().enumerate() {
            assert_eq!(m.get(&0), Some(&Furniture::PageNumber), "page {i}");
            assert!(
                m.get(&1).is_none() && m.get(&2).is_none(),
                "page {i}: {m:?}"
            );
        }
        let got: Vec<Option<&str>> = folios.iter().map(Option::as_deref).collect();
        assert_eq!(got[4], Some("2"));
        // Worded three ways, one line per language: each page still has
        // its number.
        let words = ["of", "de", "sur"];
        let pages = doc(9, |i| {
            vec![line(&format!("{} {} 3", i % 3 + 1, words[i / 3]), 950.0)]
        });
        let (_, folios) = detect_with_folios(&pages);
        let got: Vec<Option<&str>> = folios.iter().map(Option::as_deref).collect();
        assert_eq!(
            got,
            [
                Some("1"),
                Some("2"),
                Some("3"),
                Some("1"),
                Some("2"),
                Some("3"),
                Some("1"),
                Some("2"),
                Some("3")
            ]
        );
        // Bare page numbers, on alternate sides and with an unnumbered
        // insert, count the pages.
        let pages = doc(10, |i| {
            if i == 4 {
                return vec![line("Insert", 400.0)];
            }
            let mut n = line(&format!("{}", if i < 4 { i + 1 } else { i }), 950.0);
            if i % 2 == 1 {
                n.bbox.x0 = 500.0;
                n.bbox.x1 = 510.0;
            }
            vec![n]
        });
        let (marks, folios) = detect_with_folios(&pages);
        assert_eq!(marks[7].get(&0), Some(&Furniture::PageNumber));
        assert_eq!(folios[7].as_deref(), Some("7"));
    }

    #[test]
    fn static_running_head_with_a_number_is_not_a_page_number() {
        // Same number on every page: a document ID in the footer, not pagination.
        let pages = doc(10, |_| {
            vec![line("Acme Analytics, Inc. | AA00733622", 950.0)]
        });
        let r = detect(&pages);
        assert_eq!(r[0].get(&0), Some(&Furniture::Footer));
    }

    #[test]
    fn a_templated_body_line_is_kept_where_a_watermark_is_not() {
        // A template repeats real text at one place on every page, between
        // the page's other lines: a box's heading, a spec table's label.
        let pages = doc(10, |i| {
            vec![
                line(&format!("Model {i} has a unique description here"), 380.0),
                line("Technical data", 400.0),
                line(&format!("Rated {i}0 W at the nominal supply"), 420.0),
            ]
        });
        assert!(detect(&pages).iter().all(|m| m.get(&1).is_none()));
        // The same line laid on the text, or out in a margin, is one.
        let pages = doc(10, |i| {
            let mut stamp = line("Uncontrolled copy", 402.0);
            stamp.bbox.x0 = 60.0;
            let mut note = line("For internal use only", 600.0);
            note.bbox.x0 = 520.0;
            note.bbox.x1 = 590.0;
            vec![
                line(&format!("Model {i} has a unique description here"), 400.0),
                stamp,
                note,
            ]
        });
        let r = detect(&pages);
        assert_eq!(r[0].get(&1), Some(&Furniture::Watermark));
        assert_eq!(r[0].get(&2), Some(&Furniture::Watermark));
    }

    #[test]
    fn signature_collapses_digit_runs() {
        assert_eq!(signature("Page 13 of 15"), "Page # of #");
        assert_eq!(signature("Page 1 of 15"), "Page # of #");
        assert_ne!(signature("Chapter One"), signature("Chapter Two"));
    }

    #[test]
    fn a_running_line_identifies_when_it_names_something() {
        assert!(identifies("ACME LABS", Furniture::Header));
        assert!(identifies("Form QA-12 Page 3", Furniture::PageNumber));
        assert!(!identifies("Page 3 of 15", Furniture::PageNumber));
        assert!(!identifies("- 3 -", Furniture::PageNumber));
        assert!(!identifies("p. 3", Furniture::PageNumber));
        assert!(!identifies("P age 3", Furniture::PageNumber));
        assert!(!identifies("DRAFT", Furniture::Watermark));
        // The same on every page, it is no page number.
        assert!(identifies(
            "(888) 972-7397 / 973-777-5411",
            Furniture::Header
        ));
        assert!(identifies("2016", Furniture::Header));
        assert!(!identifies("- 3 -", Furniture::PageNumber));
        assert!(names_something("DRAFT"));
        assert!(!names_something("3 of 15"));
        // In the languages documents number their pages in.
        for page in [
            "3 sur 3",
            "1 de 3",
            "Página 2 de 9",
            "pág. 4",
            "Seite 1 von 4",
            "Pagina 3 di 5",
            "Strona 2 z 8",
            "Sida 3 av 4",
        ] {
            assert!(!names_something(page), "{page}");
            assert!(says_page(page), "{page}");
        }
        // A line with a name besides them still names it.
        assert!(names_something("Banco de España 3"));
        assert!(names_something("Rapport annuel 3 sur 3"));
    }

    #[test]
    fn the_running_header_and_footer_are_kept_once() {
        use crate::document::Element;
        let el = |id: &str, page: usize, kind: &str, text: &str| Element {
            id: id.into(),
            page,
            ..Element::new(kind, text, "rust", "test")
        };
        let mut elements = vec![
            el("elem-00001", 0, "doco:Paragraph", "Body"),
            el("elem-00002", 1, "doco:Table", "A | B"),
            Element {
                continues: Some("elem-00002".into()),
                ..el("elem-00003", 2, "doco:Table", "C | D")
            },
        ];
        let at = |page: usize, text: &str, top: bool| Running {
            page,
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 1.0,
                y1: 1.0,
            },
            text: text.into(),
            top,
        };
        let mut running: Vec<Running> = (0..3)
            .flat_map(|p| {
                [
                    at(p, "ACME LABS", true),
                    at(p, &format!("Form QA-12 Page {}", p + 1), true),
                    at(p, "Uncontrolled copy", false),
                ]
            })
            .collect();
        // A running head from the second page on, as a report's chapter
        // title is: it would stand between two pages' text.
        running.push(at(1, "Chapter Two", true));
        running.push(at(2, "Chapter Two", true));
        let mut later = elements.clone();
        keep_running(&mut elements, &running, 0, 2);
        let got: Vec<(&str, usize, &str, &str)> = elements
            .iter()
            .map(|e| (e.id.as_str(), e.page, e.evidence, e.text.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("elem-00001", 0, "header", "ACME LABS"),
                ("elem-00002", 0, "header", "Form QA-12 Page 1"),
                ("elem-00003", 0, "test", "Body"),
                ("elem-00004", 1, "test", "A | B"),
                ("elem-00005", 2, "test", "C | D"),
                ("elem-00006", 2, "footer", "Uncontrolled copy"),
            ]
        );
        assert_eq!(elements[4].continues.as_deref(), Some("elem-00004"));
        // Converting from the second page on: its own copy, and the head
        // that starts there.
        later.retain(|e| e.page >= 1);
        keep_running(&mut later, &running, 1, 2);
        let texts: Vec<&str> = later.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "ACME LABS",
                "Form QA-12 Page 2",
                "Chapter Two",
                "A | B",
                "C | D",
                "Uncontrolled copy"
            ]
        );
    }
}

/// Remove a known furniture string from text captured inside a table cell.
///
/// Grids take their glyphs before furniture detection runs, so a footer that
/// crosses a table region on one page is invisible to the cross-page pass —
/// the classic leak is a repeated footer landing inside an appendix table's
/// cells. Furniture whose digits vary across pages (page numbers) matches
/// digit-insensitively: each digit run in the pattern matches any digit run
/// in the cell.
///
/// A page number alone in a cell has no text to match on; see
/// [`is_folio_cell`].
pub fn scrub_cell(cell: &str, furniture: &[(String, bool)]) -> String {
    scrub_with(cell, furniture, false)
}

/// As [`scrub_cell`], for text that already went through positional furniture
/// removal, and only at the block's start or end, where a header or footer
/// merged into it sits: a running header is often a phrase the document uses
/// in its sentences too, and stripping it wherever it occurs cut
/// `Federal Open Market Committee` out of every sentence of the minutes
/// that bore it as their header.
pub fn scrub_block(text: &str, furniture: &[(String, bool)]) -> String {
    scrub_with(text, furniture, true)
}

/// Whether a table cell holding `cell` is its page's number, taken in by a
/// grid that reaches into the top or foot of the page: the page's own folio
/// alone, in the cell, where page numbers are printed. `rel_y` is the
/// cell's middle as a share of the page's height.
///
/// A grid takes its glyphs before furniture is read, so a folio it took in
/// is not removed by position, and a page number has no text to match a
/// cell's on. Matching by width alone emptied every cell as wide as the
/// page numbers: an accessory list's `CARTON COUNT | 1` and a catalogue's
/// column of I/O points, `10`, `14`, `32`. A cell's figure is the folio
/// only where it is the page's and stands where the folio does.
pub fn is_folio_cell(cell: &str, folio: Option<&str>, rel_y: f64) -> bool {
    let t = cell.trim();
    !t.is_empty() && folio == Some(t) && !(EDGE_BAND..=1.0 - EDGE_BAND).contains(&rel_y)
}

/// Longest furniture string, in characters other than spaces, that is
/// removed from a cell only as the whole cell.
const SHORT_FURNITURE: usize = 3;

/// `out[i..j]` stands as whole words: no letter or digit runs on into it
/// at either end, nor a number through a decimal or grouping mark (`3` in
/// `3,3`).
fn whole(out: &str, i: usize, j: usize) -> bool {
    let before: Vec<char> = out[..i].chars().rev().take(2).collect();
    let after: Vec<char> = out[j..].chars().take(2).collect();
    let first = out[i..j].chars().next();
    let last = out[i..j].chars().last();
    let joins = |near: &[char], edge: Option<char>| match near {
        [c, ..] if c.is_alphanumeric() => true,
        [c, d, ..] => {
            matches!(c, '.' | ',') && d.is_ascii_digit() && edge.is_some_and(|e| e.is_ascii_digit())
        }
        _ => false,
    };
    !joins(&before, first) && !joins(&after, last)
}

/// `ends_only`: remove a match only where nothing but whitespace stands
/// before it or after it.
///
/// What one pattern takes off can bring another to the text's end, or leave
/// it the whole of the text: a block can open with two running lines, a
/// title over a document number, and the number is at the start only once
/// the title is gone. So the patterns are tried again until none takes
/// anything more. Where two overlap, the longer is tried first: of a title
/// block's `DATE:` and `DATE`, a cell `DATE: -` loses `DATE:`, and the `-`
/// left is a pattern of its own. What is left does not depend on the order
/// the patterns were found in.
fn scrub_with(cell: &str, furniture: &[(String, bool)], ends_only: bool) -> String {
    let mut out = cell.to_string();
    let at_end = |out: &str, start: usize, end: usize| {
        !ends_only || out[..start].trim().is_empty() || out[end..].trim().is_empty()
    };
    let mut furniture: Vec<&(String, bool)> = furniture.iter().collect();
    furniture.sort_by_key(|(pat, _)| std::cmp::Reverse(pat.chars().count()));
    loop {
        let before = out.clone();
        for &(pat, digits_vary) in &furniture {
            if pat.is_empty() {
                continue;
            }
            if !*digits_vary {
                // A figure's callout (`1`), a short mark (`FCC`) or a run of
                // numbers (`4, 5`) is furniture in a cell only as the whole of
                // it: cut from inside the cell's text, `3` took the digit out of
                // `30 po-lb` and `3,3 Nm`.
                let p = pat.trim();
                let short = p.chars().filter(|c| !c.is_whitespace()).count() <= SHORT_FURNITURE
                    || !p.chars().any(char::is_alphabetic);
                if short {
                    if out.trim() == p {
                        out.clear();
                    }
                    continue;
                }
                let mut from = 0;
                while let Some(k) = out[from..].find(pat.as_str()) {
                    let i = from + k;
                    if at_end(&out, i, i + pat.len()) && whole(&out, i, i + pat.len()) {
                        out.replace_range(i..i + pat.len(), " ");
                        from = 0;
                    } else {
                        from = i + pat.len();
                    }
                }
                continue;
            }
            // Segment the pattern at digit runs; match literal segments with any
            // digit run between them.
            let mut segs: Vec<&str> = Vec::new();
            let mut rest = pat.as_str();
            while let Some(d0) = rest.find(|c: char| c.is_ascii_digit()) {
                segs.push(&rest[..d0]);
                let after = rest[d0..]
                    .find(|c: char| !c.is_ascii_digit())
                    .map(|k| d0 + k)
                    .unwrap_or(rest.len());
                rest = &rest[after..];
            }
            segs.push(rest);
            if segs.len() < 2 {
                continue;
            }
            // A pattern that is nothing but digits — a bare page number — has no
            // literal text to anchor on, so digit-insensitive matching would
            // strip the leading digit run out of *any* cell it is applied to: a
            // financial statement's `85,159` became `,159`, silently changing
            // the figure. It matches nothing here; a cell that is a page's
            // number is told by where it stands (see [`is_folio_cell`]).
            if segs.iter().all(|s| s.is_empty()) {
                continue;
            }
            let mut from = 0;
            #[allow(clippy::while_let_loop)] // labeled break from a nested match reads clearer here
            'scan: loop {
                let Some(start) = (if segs[0].is_empty() {
                    (from == 0).then_some(0)
                } else {
                    out.get(from..)
                        .and_then(|rest| rest.find(segs[0]))
                        .map(|k| from + k)
                }) else {
                    break;
                };
                let mut pos = start + segs[0].len();
                for seg in &segs[1..] {
                    let digit_end = out[pos..]
                        .find(|c: char| !c.is_ascii_digit())
                        .map(|k| pos + k)
                        .unwrap_or(out.len());
                    if digit_end == pos {
                        break 'scan; // expected a digit run, none present
                    }
                    if !out[digit_end..].starts_with(seg) {
                        break 'scan;
                    }
                    pos = digit_end + seg.len();
                }
                if at_end(&out, start, pos) && whole(&out, start, pos) {
                    out.replace_range(start..pos, " ");
                    from = 0;
                } else if segs[0].is_empty() {
                    break;
                } else {
                    from = start + segs[0].len();
                }
            }
        }
        if out == before {
            break;
        }
    }
    // A line that is a long prefix of a known furniture text is that
    // furniture wrapped or truncated differently on its page (the watermark
    // crossing a table region breaks at a different point than it does in
    // the running footer).
    let trimmed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.chars().count() >= 24 && furniture.iter().any(|(pat, _)| pat.starts_with(&trimmed)) {
        return String::new();
    }
    trimmed
}

#[cfg(test)]
mod scrub_tests {
    use super::{scrub_block, scrub_cell};

    #[test]
    fn a_short_furniture_string_is_cut_only_as_the_whole_cell() {
        // A figure's callouts taken for furniture: they must not take the
        // digits out of a torque, a step's number or a figure reference.
        let f: Vec<(String, bool)> = ["3", "1", "FCC", "4, 5"]
            .iter()
            .map(|p| (p.to_string(), false))
            .collect();
        assert_eq!(
            scrub_cell("Serrer les vis à 30 po-lb (3,3 Nm).", &f),
            "Serrer les vis à 30 po-lb (3,3 Nm)."
        );
        assert_eq!(scrub_cell("STEP 1:", &f), "STEP 1:");
        assert_eq!(scrub_cell("See Figure 1.", &f), "See Figure 1.");
        assert_eq!(scrub_cell("FCC Part 15", &f), "FCC Part 15");
        assert_eq!(scrub_cell("3", &f), "");
        assert_eq!(scrub_cell(" 4, 5 ", &f), "");
    }

    #[test]
    fn a_furniture_string_is_cut_from_a_cell_only_as_whole_words() {
        let f = vec![("www.example.com".to_string(), false)];
        assert_eq!(scrub_cell("Total 12 www.example.com", &f), "Total 12");
        assert_eq!(
            scrub_cell("see www.example.community", &f),
            "see www.example.community"
        );
    }

    #[test]
    fn a_header_phrase_inside_a_sentence_stays() {
        let f = vec![("Federal Open Market Committee".to_string(), false)];
        assert_eq!(
            scrub_block(
                "A joint meeting of the Federal Open Market Committee was held",
                &f
            ),
            "A joint meeting of the Federal Open Market Committee was held"
        );
        // Merged onto the block's first line, it goes.
        assert_eq!(
            scrub_block("Federal Open Market Committee Minutes of the meeting", &f),
            "Minutes of the meeting"
        );
        let p = vec![("Page 4 of 15".to_string(), true)];
        assert_eq!(
            scrub_block("the rest of it Page 12 of 15", &p),
            "the rest of it"
        );
        assert_eq!(
            scrub_block("see Page 12 of 15 below", &p),
            "see Page 12 of 15 below"
        );
    }

    #[test]
    fn running_lines_stacked_at_a_block_end_all_go_in_either_order() {
        let lines = ["Annual Report 2024", "Northwind Holdings Ltd"];
        for order in [[0, 1], [1, 0]] {
            let f: Vec<(String, bool)> = order
                .iter()
                .map(|&i| (lines[i].to_string(), false))
                .collect();
            assert_eq!(
                scrub_block(
                    "The fund closed the year ahead. Northwind Holdings Ltd Annual Report 2024",
                    &f
                ),
                "The fund closed the year ahead."
            );
            assert_eq!(
                scrub_block(
                    "Annual Report 2024 Northwind Holdings Ltd The fund closed the year ahead.",
                    &f
                ),
                "The fund closed the year ahead."
            );
        }
    }

    #[test]
    fn of_two_overlapping_furniture_texts_the_longer_goes_first() {
        // A title block's `DATE` over `DATE:` and a `-` left blank.
        for f in [["DATE", "DATE:", "-"], ["DATE:", "DATE", "-"]] {
            let f: Vec<(String, bool)> = f.iter().map(|p| (p.to_string(), false)).collect();
            assert_eq!(scrub_cell("DATE: -", &f), "");
        }
    }

    #[test]
    fn constant_footer_is_removed_from_a_cell() {
        let f = vec![("Acme Analytics, Inc. | AA00733622".to_string(), false)];
        assert_eq!(
            scrub_cell("Riding the Curve Acme Analytics, Inc. | AA00733622", &f),
            "Riding the Curve"
        );
    }

    #[test]
    fn page_numbers_match_digit_insensitively() {
        let f = vec![("Page 4 of 15".to_string(), true)];
        assert_eq!(scrub_cell("body Page 12 of 15 more", &f), "body more");
    }

    #[test]
    fn a_truncated_watermark_prefix_is_furniture() {
        let f = vec![(
            "This research note is restricted to the personal use of user@example.com.".to_string(),
            false,
        )];
        assert_eq!(
            scrub_cell(
                "This research note is restricted to the personal use of user@exa",
                &f
            ),
            ""
        );
    }

    #[test]
    fn ordinary_cell_text_is_untouched() {
        let f = vec![("Acme Analytics, Inc. | AA00733622".to_string(), false)];
        assert_eq!(
            scrub_cell("Emerging or Adolescent", &f),
            "Emerging or Adolescent"
        );
    }

    #[test]
    fn scrubbing_is_idempotent() {
        let f = vec![("Page 4 of 15".to_string(), true)];
        let once = scrub_cell("body Page 12 of 15 more", &f);
        assert_eq!(scrub_cell(&once, &f), once);
    }

    #[test]
    fn independent_furniture_order_does_not_change_output() {
        let a = ("Confidential".to_string(), false);
        let b = ("Page 4 of 15".to_string(), true);
        let cell = "Confidential body Page 12 of 15";
        assert_eq!(
            scrub_cell(cell, &[a.clone(), b.clone()]),
            scrub_cell(cell, &[b, a])
        );
    }
}
#[test]
fn a_bare_page_number_never_truncates_a_figure() {
    // The pattern is only digits, so it has no literal text to anchor
    // on. Applied as a prefix match it strips the leading digit run out
    // of every numeric cell -- a financial statement's 85,159 became
    // ,159, silently changing the figure.
    let furn = vec![("85".to_string(), true)];
    assert_eq!(scrub_cell("85,159", &furn), "85,159");
    assert_eq!(scrub_cell("$88,821", &furn), "$88,821");
    assert_eq!(scrub_cell("27,471", &furn), "27,471");
    // Nor does a cell that is only a number of that width: whether it is
    // the page's number is told by where it stands.
    assert_eq!(scrub_cell("85", &furn), "85");
    assert_eq!(scrub_cell(" 12 ", &furn).trim(), "12");
}

#[test]
fn a_cell_is_the_folio_only_as_the_pages_own_number_where_it_stands() {
    // A grid reaching the foot of page 85 took its folio in.
    assert!(is_folio_cell("85", Some("85"), 0.95));
    assert!(is_folio_cell(" 85 ", Some("85"), 0.04));
    // An accessory list's `CARTON COUNT | 1` on page 3, at the foot or in
    // the middle of the page, is a count.
    assert!(!is_folio_cell("1", Some("3"), 0.95));
    assert!(!is_folio_cell("3", Some("3"), 0.5));
    // On a page with no folio, no cell is one.
    assert!(!is_folio_cell("3", None, 0.95));
    // A wider figure is no page number.
    assert!(!is_folio_cell("2024", Some("24"), 0.95));
}
