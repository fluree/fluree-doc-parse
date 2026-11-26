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

/// Parts a bare number's signature from the place its group stands at.
const PLACE: char = '\u{1f}';

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
    detect_all(pages, &[], &[]).0
}

/// As [`detect`], also returning, per page, the lines read as a short
/// document's exact copies ([`exact_running`]) rather than by repetition.
///
/// Those need no scrubbing from cells and blocks. They were found as lines
/// of their own on every page, or every page but one, so no copy merged
/// into a block or a table, and the page left out is where the words are
/// not running: a title page's title, or a product code in a table.
///
/// A section's name read as running at the head of the page
/// ([`section_running_heads`]) is taken the same way, as the line it is.
pub fn detect_with_exact(
    pages: &[(Vec<Line>, f64)],
) -> (
    Vec<BTreeMap<usize, Furniture>>,
    Vec<std::collections::HashSet<usize>>,
) {
    let (marks, _, exact) = detect_all(pages, &[], &[]);
    (marks, exact)
}

/// As [`detect_with_exact`], also reading the running heads that name the
/// section the page is in ([`section_running_heads`]), and returning, per
/// page, the lines kept as a section's heading: text the scrub must not
/// take, though the same words run as furniture on the pages after.
/// `bookmarks` are the document's outline entries, as the page each lands
/// on and its title; `widths` the pages' widths, by which a short
/// document's facing pages are seen to mirror their template
/// ([`mirrored_running`]), or empty where they are not known.
#[allow(clippy::type_complexity)]
pub fn detect_with_heads(
    pages: &[(Vec<Line>, f64)],
    bookmarks: &[(usize, String)],
    widths: &[f64],
    drawings: &[Drawing],
) -> (
    Vec<BTreeMap<usize, Furniture>>,
    Vec<std::collections::HashSet<usize>>,
    Vec<std::collections::HashSet<usize>>,
) {
    let (mut marks, _, mut exact) = detect_all(pages, widths, drawings);
    let heads = section_running_heads(pages, &mut marks, &mut exact, bookmarks);
    (marks, exact, heads)
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
    let (marks, folios, _) = detect_all(pages, &[], &[]);
    (marks, folios)
}

/// As [`detect_with_folios`], each page's folio with where its line stands,
/// as a share of the page's height: the place the page number is printed
/// on that page, `None` where the folio's line is not among `pages`' lines.
/// `widths` are as for [`detect_with_heads`].
pub fn detect_folio_places(
    pages: &[(Vec<Line>, f64)],
    widths: &[f64],
) -> Vec<(Option<String>, Option<f64>)> {
    let (marks, folios, _) = detect_all(pages, widths, &[]);
    folios
        .into_iter()
        .enumerate()
        .map(|(pi, folio)| {
            let (lines, height) = &pages[pi];
            let place = folio.as_ref().and_then(|f| {
                marks[pi]
                    .iter()
                    .filter(|(_, k)| **k == Furniture::PageNumber)
                    .map(|(li, _)| &lines[*li])
                    .find(|l| digit_runs(&l.text).iter().any(|r| r == f))
                    .filter(|_| *height > 0.0)
                    .map(|l| l.bbox.y0 / height)
            });
            (folio, place)
        })
        .collect()
}

/// Whether a line of the page's own is its page number: nothing but the
/// page's folio, standing where the folio's line does (`place`, a share of
/// the page's height). A chart's tick in the band, `6` on page 6 among
/// `20 18 … 4 2`, stands elsewhere.
pub fn is_folio_line(text: &str, folio: &str, rel_y: f64, place: f64) -> bool {
    text.trim() == folio && (rel_y - place).abs() < POSITION_TOLERANCE
}

/// Marks, folios, and the marks read as exact copies.
#[allow(clippy::type_complexity)]
fn detect_all(
    pages: &[(Vec<Line>, f64)],
    widths: &[f64],
    drawings: &[Drawing],
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
        // Repetition is the only evidence there is, and one page has none:
        // but a page's number says which page it is.
        lone_folios(pages, &mut out, &mut folios);
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

    // A number alone is grouped by where it stands as well. Every bare
    // number in the bands shares one signature, a table's figures near the
    // foot with the page number under them, and the group's middle place was
    // the table's: the page number, set lower, was not where the group
    // stands, and went unread.
    //
    // So is a line that runs at two places at its pages' edges: a marking
    // set over the page and again at its foot (`TLP:CLEAR`). Read as one
    // group, the middle place was the foot's, and the head's copies went
    // unread.
    let groups: HashMap<String, Vec<(usize, usize, f64, String)>> = groups
        .into_iter()
        .flat_map(|(sig, mut occ)| {
            occ.sort_by(|a, b| a.2.total_cmp(&b.2));
            let mut places: Vec<Vec<(usize, usize, f64, String)>> = Vec::new();
            for o in occ.iter().cloned() {
                match places.last_mut() {
                    Some(p) if o.2 - p[p.len() - 1].2 < POSITION_TOLERANCE => p.push(o),
                    _ => places.push(vec![o]),
                }
            }
            let runs = |p: &Vec<(usize, usize, f64, String)>| {
                let mut on: Vec<usize> = p.iter().map(|o| o.0).collect();
                on.sort_unstable();
                on.dedup();
                on.len() >= needed
            };
            // Both at the page's edges: a form's answers (`Yes`, `No`) and a
            // table's labels repeat at fixed places down the page too.
            let edge = |p: &Vec<(usize, usize, f64, String)>| {
                p.iter()
                    .all(|o| !(EDGE_BAND..=1.0 - EDGE_BAND).contains(&o.2))
            };
            let running: Vec<&Vec<(usize, usize, f64, String)>> =
                places.iter().filter(|p| runs(p)).collect();
            if !bare_number(&sig) && (running.len() < 2 || !running.iter().all(|p| edge(p))) {
                return vec![(sig, occ)];
            }
            places
                .into_iter()
                .enumerate()
                .map(|(i, p)| (format!("{sig}{PLACE}{i}"), p))
                .collect()
        })
        .collect();
    let moves = template_moves(pages, widths);
    let body = crate::block::body_size(pages.iter().flat_map(|(lines, _)| lines));

    // The texts of the lines that repeat on enough pages to run, as set.
    let running: Vec<String> = groups
        .values()
        .filter(|occ| {
            let mut on: Vec<usize> = occ.iter().map(|o| o.0).collect();
            on.sort_unstable();
            on.dedup();
            on.len() >= needed && occ.iter().all(|o| o.3 == occ[0].3)
        })
        .map(|occ| occ[0].3.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    // Each line's group, and on how many pages each group's lines could run:
    // none where its number changes without counting the pages, which makes
    // it each page's own.
    let mut keys: Vec<Vec<String>> = pages
        .iter()
        .map(|(ls, _)| vec![String::new(); ls.len()])
        .collect();
    let mut seen_on: HashMap<String, usize> = HashMap::new();
    for (sig, occ) in &groups {
        let mut on: Vec<usize> = occ.iter().map(|o| o.0).collect();
        on.sort_unstable();
        on.dedup();
        let mut texts: Vec<&str> = occ.iter().map(|o| o.3.as_str()).collect();
        texts.sort_unstable();
        texts.dedup();
        let copies: Vec<(usize, &str)> = occ.iter().map(|o| (o.0, o.3.as_str())).collect();
        let counts =
            texts.len() < 2 || advances(&copies) || copies.iter().all(|(_, t)| says_page(t));
        seen_on.insert(sig.clone(), if counts { on.len() } else { 0 });
        for o in occ {
            keys[o.0][o.1] = sig.clone();
        }
    }

    // The page-number group the folios come from: (pages it covers, distinct
    // values of its varying run), then the per-page values.
    let mut folio_sources: Vec<FolioSource> = Vec::new();

    for (key, occ) in groups {
        let sig = key.split(PLACE).next().unwrap_or_default();
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
            let found = exact_running(pages, &occ).or_else(|| shifted_running(pages, &occ));
            // The same words may run in both bands, a letterhead's phone
            // line over the page and again in its footer: the copies one
            // band leaves may run where the template moved the other.
            let left: Vec<(usize, usize, f64, String)> = occ
                .iter()
                .filter(|o| {
                    !found
                        .as_ref()
                        .is_some_and(|(_, h)| h.iter().any(|(pi, li, _)| *pi == o.0 && *li == o.1))
                })
                .cloned()
                .collect();
            let moved = mirrored_running(pages, &moves, &left);
            for (kind, hits) in found.into_iter().chain(moved) {
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

        // A chart's own text, a legend's entries beside their swatches or an
        // axis's labels at its ticks, repeats at one place on every page a
        // template draws the chart on, as a running line does. Set in the
        // drawing on most of its pages, it is the drawing's.
        let in_drawing = occ
            .iter()
            .filter(|o| (o.2 - median).abs() < POSITION_TOLERANCE)
            .filter(|o| {
                drawings
                    .get(o.0)
                    .is_some_and(|d| in_drawing(&pages[o.0].0[o.1], &d.shapes, &d.ticks))
            })
            .count();
        if in_drawing * 2 > stable {
            continue;
        }

        // A page number is a furniture line whose digits *change* between
        // pages — that is what distinguishes "Page 13 of 15" from a static
        // running head that merely happens to contain a number.
        let mut raw: Vec<&str> = occ.iter().map(|o| o.3.as_str()).collect();
        raw.sort_unstable();
        raw.dedup();
        let digits_vary = sig.contains('#') && raw.len() > 1;

        // A line whose number changes at one place on page after page is a
        // page number only if the number counts the pages or the line says
        // it is one. An install guide's step numbers, a figure's callouts
        // and a chart's axis labels sit at the same place too, and so does a
        // catalogue's product title on each product's page, `EPIC® H-BE 6
        // Kits` over `… 10 Kits`: each says something of its own page.
        if digits_vary {
            let stable: Vec<(usize, &str)> = occ
                .iter()
                .filter(|o| (o.2 - median).abs() < POSITION_TOLERANCE)
                .map(|o| (o.0, o.3.as_str()))
                .collect();
            let says = !bare_number(sig) && stable.iter().all(|(_, t)| says_page(t));
            if !advances(&stable) && !says {
                continue;
            }
        }

        let kind = classify(median, digits_vary);

        if kind != Furniture::Watermark {
            let stable: Vec<&(usize, usize, f64, String)> = occ
                .iter()
                .filter(|o| (o.2 - median).abs() < POSITION_TOLERANCE)
                .collect();
            // A running line stands outside the body. Where, on most of its
            // pages, text of that page's own stands between it and the
            // page's edge in its column, it is a heading the body reached
            // past: an install guide's `FCC` partway down a column, under
            // the end of the section before it.
            let inside = stable
                .iter()
                .filter(|o| beyond_the_body(pages, o.0, o.1, &seen_on, &keys, needed))
                .count();
            if inside * 2 > stable.len() {
                continue;
            }
            // Nor where, on most of its pages, the page's own figures stand
            // between it and the edge: a template's table heads over each
            // page's values, a section's band under each page's key
            // figures (`Residents` over a country's counts).
            let figured = stable
                .iter()
                .filter(|o| {
                    let tables = drawings.get(o.0).map_or(&[][..], |d| &d.tables[..]);
                    figures_beyond(pages, o.0, o.1, tables, &seen_on, &keys, needed)
                })
                .count();
            if figured * 2 > stable.len() {
                continue;
            }
            // Nor is a line running where the pages it is missing from set
            // a longer line in its place that opens or closes with its
            // words: the place holds a title that varies from page to page
            // (`DISHWASHER` over `DISHWASHER – ELECTRIC` and `– GAS`), and
            // the pages that print the short form are that title's too. A
            // longer line that runs itself is the same running head set as
            // one line on some pages (`June 14, 2023 Chair Powell's Press
            // Conference FINAL`), and says nothing of a title.
            if !digits_vary {
                let text = stable.first().map_or("", |o| o.3.trim());
                let held: std::collections::HashSet<usize> = stable.iter().map(|o| o.0).collect();
                let varied = (0..n_pages)
                    .filter(|pi| !held.contains(pi))
                    .filter(|&pi| {
                        let (lines, height) = &pages[pi];
                        *height > 0.0
                            && lines.iter().enumerate().any(|(lj, l)| {
                                (l.bbox.y0 / height - median).abs() < POSITION_TOLERANCE
                                    && extends(l.text.trim(), text)
                                        .is_some_and(|rest| !merges_running(rest, &running))
                                    && seen_on.get(&keys[pi][lj]).copied().unwrap_or(1) < needed
                            })
                    })
                    .count();
                if varied > 0 && varied * 2 >= held.len() {
                    continue;
                }
            }
        }

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
                .filter(|o| watermark_shown(&pages[o.0].0, o.1, body))
                .count();
            if shown < needed {
                if let Some(later) = template_title(pages, &occ, body) {
                    for (pi, li) in later {
                        out[pi].insert(li, Furniture::Header);
                        exact[pi].insert(li);
                    }
                }
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

    lone_folios(pages, &mut out, &mut folios);

    // A line read by repetition as well keeps its scrub.
    for (pi, set) in exact.iter_mut().enumerate() {
        set.retain(|li| out[pi].contains_key(li));
    }
    (out, folios, exact)
}

/// Most words in a section's name set as a running head. A section is
/// named in a phrase, not a sentence (`Summary of Changes`, `DISHWASHER –
/// ELECTRIC`): a longer line at the head of the page is text set high.
const MAX_SECTION_HEAD_WORDS: usize = 8;

/// Two lines set in one style differ in measured size by less than this
/// share of the larger: the median of one face at one size, rounded, and
/// well short of the step from one style to the next.
const SAME_STYLE_SIZE: f32 = 0.1;

/// Section names set as running heads: one slot at the head of the page,
/// filled in turn with the name of each section, repeated on its pages
/// (`Overview` over two pages, `Standard Features` over four, `Technical
/// Specifications` over thirteen; or a two-line head's second line,
/// `DISHWASHER – ELECTRIC` then `DISHWASHER – GAS`).
///
/// A slot names sections in turn where two names or more are each set as a
/// heading at their first copy and each run over two pages or more. Set as
/// a heading means larger than the body, by the margin a font-size heading
/// needs, or bookmarked to that page or the one before. A running head set
/// no larger than the text and named by no bookmark (a journal's short
/// title, a form's name line, a signature block's label) is told apart
/// from the page by nothing but its repetition, and is left to it. One name
/// running after another set once is a title page's title and the running
/// title of the rest.
///
/// In such a slot, the first copy of each name set as a heading is the
/// section's heading and stays in the text, and the copies after it are
/// running text, as are the name's later runs in the slot. A name already
/// read as running by repetition is kept the same way, at its first copy.
///
/// A section whose heading is set elsewhere on the page its running head
/// first appears, or on the page before (a chapter's title over its first
/// page, the running head from the next), has its heading already, and
/// every copy in the slot is running text.
///
/// The copies this reads as running are taken as whole lines (`exact`):
/// their words elsewhere are content, as a shorter name is inside a longer
/// one (`DISHWASHER` in `DISHWASHER – GAS`).
fn section_running_heads(
    pages: &[(Vec<Line>, f64)],
    out: &mut [BTreeMap<usize, Furniture>],
    exact: &mut [std::collections::HashSet<usize>],
    bookmarks: &[(usize, String)],
) -> Vec<std::collections::HashSet<usize>> {
    let mut heads: Vec<std::collections::HashSet<usize>> = vec![Default::default(); pages.len()];
    // A slot: a place at the head of the page, by relative height and left
    // or right edge, and the lines set there in page order.
    struct Slot {
        rel_y: f64,
        x0: f64,
        x1: f64,
        size: f32,
        lines: Vec<(usize, usize)>,
    }
    let key = |t: &str| {
        t.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    // The lines read as running by repetition. A line that carries one of
    // them and more words is a row they merged into (`January 31, 2024
    // Chair Powell's Press Conference FINAL`, set as three groups on the
    // other pages): the scrub's to clean, not a section's name.
    let running: std::collections::HashSet<String> = out
        .iter()
        .enumerate()
        .flat_map(|(pi, marks)| marks.keys().map(move |&li| key(&pages[pi].0[li].text)))
        .filter(|k| !k.is_empty())
        .collect();
    let merges_running = |text: &str| {
        let k = key(text);
        let padded = format!(" {k} ");
        running
            .iter()
            .any(|r| *r != k && padded.contains(&format!(" {r} ")))
    };
    // A running head stands alone: no line of the page's own shares its
    // row (a margin label beside its regulation's number and text), and
    // none in its style sits within a line's height above or below it over
    // the same width (a display title's next line, `lash WITHOUT LIMITS`
    // over `TUBING EXTREME`, where the page before set the same first line
    // over another). Lines already running are the head's company, not the
    // page's: a two-line head's first line, the address set beside it.
    let alone = |pi: usize, li: usize| {
        let lines = &pages[pi].0;
        let l = &lines[li];
        let h = l.bbox.y1 - l.bbox.y0;
        lines.iter().enumerate().all(|(mi, m)| {
            if mi == li || out[pi].contains_key(&mi) {
                return true;
            }
            let mh = m.bbox.y1 - m.bbox.y0;
            let overlap = l.bbox.y1.min(m.bbox.y1) - l.bbox.y0.max(m.bbox.y0);
            let same_row = overlap > 0.5 * h.min(mh);
            let gap = (m.bbox.y0 - l.bbox.y1).max(l.bbox.y0 - m.bbox.y1);
            let stacked = gap < h
                && l.bbox.x1.min(m.bbox.x1) > l.bbox.x0.max(m.bbox.x0)
                && (l.font_size - m.font_size).abs()
                    <= SAME_STYLE_SIZE * l.font_size.max(m.font_size);
            !same_row && !stacked
        })
    };
    let mut slots: Vec<Slot> = Vec::new();
    for (pi, (lines, height)) in pages.iter().enumerate() {
        if *height <= 0.0 {
            continue;
        }
        for (li, l) in lines.iter().enumerate() {
            let rel_y = l.bbox.y0 / height;
            if rel_y >= EDGE_BAND
                || !names_a_section(&l.text)
                || merges_running(&l.text)
                || !alone(pi, li)
            {
                continue;
            }
            let fs = (l.font_size as f64).max(1.0);
            // Set from one left edge, or flush to one right edge: a running
            // head set right moves its left edge with the name's length.
            // Within an em of the edge, for the side bearing of the first
            // or last letter. And in one style: a running head's slot is
            // not the line of small print set beside it.
            match slots.iter_mut().find(|s| {
                (s.rel_y - rel_y).abs() < POSITION_TOLERANCE
                    && ((s.x0 - l.bbox.x0).abs() <= fs || (s.x1 - l.bbox.x1).abs() <= fs)
                    && (s.size - l.font_size).abs() <= SAME_STYLE_SIZE * s.size.max(l.font_size)
            }) {
                Some(s) => s.lines.push((pi, li)),
                None => slots.push(Slot {
                    rel_y,
                    x0: l.bbox.x0,
                    x1: l.bbox.x1,
                    size: l.font_size,
                    lines: vec![(pi, li)],
                }),
            }
        }
    }
    // Facing pages mirror their heads, flush left on one side of the spread
    // and flush right on the other: a slot on even pages only and one on
    // odd pages only, at one height and in one style, are one slot where
    // the two sides set the same name. A book's title over its left-hand
    // pages and the chapter's name over its right-hand ones are two.
    let shares_name = |a: &Slot, b: &Slot| {
        a.lines.iter().any(|&(pi, li)| {
            let k = key(&pages[pi].0[li].text);
            b.lines
                .iter()
                .any(|&(pj, lj)| key(&pages[pj].0[lj].text) == k)
        })
    };
    let parity = |s: &Slot| {
        let p = s.lines[0].0 % 2;
        s.lines.iter().all(|&(pi, _)| pi % 2 == p).then_some(p)
    };
    let mut i = 0;
    while i < slots.len() {
        if let Some(p) = parity(&slots[i]) {
            let facing = (i + 1..slots.len()).find(|&j| {
                parity(&slots[j]) == Some(1 - p)
                    && (slots[j].rel_y - slots[i].rel_y).abs() < POSITION_TOLERANCE
                    && (slots[j].size - slots[i].size).abs()
                        <= SAME_STYLE_SIZE * slots[j].size.max(slots[i].size)
                    && shares_name(&slots[i], &slots[j])
            });
            if let Some(j) = facing {
                let other = slots.remove(j);
                slots[i].lines.extend(other.lines);
                slots[i].lines.sort_unstable();
            }
        }
        i += 1;
    }
    let body = crate::block::body_size(pages.iter().flat_map(|(lines, _)| lines));
    let bookmarks: Vec<(usize, String)> = bookmarks
        .iter()
        .map(|(pi, title)| (*pi, key(title)))
        .collect();
    // Larger than the body by the margin a font-size heading needs.
    let display = |pi: usize, li: usize| {
        pages[pi].0[li].font_size as f64 > body * (1.0 + crate::heading::HEADING_SIZE_MARGIN)
    };
    for slot in slots {
        let texts: Vec<String> = slot
            .lines
            .iter()
            .map(|&(pi, li)| key(&pages[pi].0[li].text))
            .collect();
        let names: std::collections::HashSet<&str> = texts.iter().map(String::as_str).collect();
        // The page the outline opened `name`'s section at, where the page
        // `pi` is in it: the last of the slot's names bookmarked at or
        // before the page. A bookmark lands where its section opens, which
        // may be before the first page its running head is set on: the
        // page before, where facing pages set the head on every other one,
        // or the document's first page for its first section.
        let opened = |name: &str, pi: usize| {
            bookmarks
                .iter()
                .filter(|(p, k)| *p <= pi && names.contains(k.as_str()))
                .max_by_key(|(p, _)| *p)
                .filter(|(_, k)| k == name)
                .map(|(p, _)| *p)
        };
        // The slot's runs of one name, in page order.
        let mut runs: Vec<std::ops::Range<usize>> = Vec::new();
        for (k, t) in texts.iter().enumerate() {
            match runs.last_mut() {
                Some(r) if texts[r.start] == *t => r.end = k + 1,
                _ => runs.push(k..k + 1),
            }
        }
        // Whether `name`'s section has its heading already, from its head's
        // first page on: the same words set anywhere else on the page, no
        // smaller than the head, on one line or broken over two in one style
        // (`Federal Reserve Bank of` over `St. Louis`), from the page the
        // outline opened the section at, or the page before, to the head's
        // first page. An agenda or contents entry naming the section is set
        // smaller.
        let headed_by = |name: &str, first_page: usize, size: f32| {
            let in_body = |pi: usize| {
                pages.get(pi).is_some_and(|(lines, _)| {
                    let own = |li: usize| {
                        !slot.lines.contains(&(pi, li))
                            && lines[li].font_size >= size * (1.0 - SAME_STYLE_SIZE)
                    };
                    (0..lines.len()).any(|li| {
                        own(li)
                            && (key(&lines[li].text) == name
                                || lines.get(li + 1).is_some_and(|m| {
                                    own(li + 1)
                                        && (m.font_size - lines[li].font_size).abs()
                                            <= SAME_STYLE_SIZE
                                                * m.font_size.max(lines[li].font_size)
                                        && key(&format!("{} {}", lines[li].text, m.text)) == name
                                }))
                    })
                })
            };
            let from = opened(name, first_page)
                .unwrap_or(first_page)
                .min(first_page.saturating_sub(1));
            (from..=first_page).any(in_body)
        };
        // Each name's first copy, whether it is set as a heading there, and
        // whether its section has its heading already.
        let mut first: std::collections::HashMap<&str, (bool, bool)> = Default::default();
        for r in &runs {
            let (pi, li) = slot.lines[r.start];
            let name = texts[r.start].as_str();
            first.entry(name).or_insert_with(|| {
                let size = pages[pi].0[li].font_size;
                (
                    display(pi, li) || opened(name, pi).is_some(),
                    headed_by(name, pi, size),
                )
            });
        }
        // A section's name, set as a heading, runs over two pages or more,
        // or repeats its section's heading printed before it: a report whose
        // sections run two pages names each once, over its second page.
        let sections = first
            .iter()
            .filter(|(name, (set, headed))| {
                *set && (*headed
                    || runs
                        .iter()
                        .any(|r| texts[r.start] == **name && r.len() >= 2))
            })
            .count();
        if sections < 2 {
            continue;
        }
        // A name's later runs go on with its section: only its first run
        // keeps a copy as the heading.
        let mut named: std::collections::HashSet<&str> = Default::default();
        for r in &runs {
            let name = texts[r.start].as_str();
            let (set, headed_first) = first[name];
            if !set {
                continue;
            }
            let run = &slot.lines[r.clone()];
            let headed = !named.insert(name) || headed_first;
            for (k, &(pi, li)) in run.iter().enumerate() {
                if k == 0 && !headed {
                    out[pi].remove(&li);
                    exact[pi].remove(&li);
                    heads[pi].insert(li);
                } else if let std::collections::btree_map::Entry::Vacant(e) = out[pi].entry(li) {
                    e.insert(Furniture::Header);
                    exact[pi].insert(li);
                }
            }
        }
    }
    heads
}

/// A line that could name a section: a few words with letters in them, not
/// a page's number or a running line of figures.
fn names_a_section(text: &str) -> bool {
    let words = text.split_whitespace().count();
    (1..=MAX_SECTION_HEAD_WORDS).contains(&words)
        && text.chars().filter(|c| c.is_alphabetic()).count() >= 3
        && !says_page(text)
}

/// A page's number where no other page's says how they are numbered: a
/// one-page sheet's `Page 1`, a data sheet's `2` on the one page after its
/// unnumbered cover. A line in the foot band that is nothing but the page's
/// place in the file, as `Page N` or bare with nothing else on its row, is
/// that page's number, whatever the other pages carry: a table's `1` in a
/// row of figures is the table's. Only one such line on the page, and only
/// where no line read by repetition gave the page its folio.
fn lone_folios(
    pages: &[(Vec<Line>, f64)],
    out: &mut [BTreeMap<usize, Furniture>],
    folios: &mut [Option<String>],
) {
    for (pi, (lines, height)) in pages.iter().enumerate() {
        if folios[pi].is_some() || *height <= 0.0 {
            continue;
        }
        let place = (pi + 1).to_string();
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(li, l)| {
                !out[pi].contains_key(li)
                    && l.rotation_bucket == 0
                    && l.bbox.y0 / height > 1.0 - EDGE_BAND
                    && numbers_page(l.text.trim(), &place, pages.len())
                    && (says_page(l.text.trim())
                        || !lines.iter().any(|o| {
                            !std::ptr::eq(o, *l)
                                && !o.text.trim().is_empty()
                                && o.bbox.y0 < l.bbox.y1
                                && o.bbox.y1 > l.bbox.y0
                        }))
            })
            .map(|(li, _)| li)
            .collect();
        if let [li] = hits[..] {
            out[pi].insert(li, Furniture::PageNumber);
            folios[pi] = Some(place);
        }
    }
}

/// Whether `text` is nothing but page number `place` of a document of
/// `pages`: `2`, `Page 2`, `Page 2 of 3`, `2 / 3`. A slash's second number
/// is the count of pages, or the line is a date (`1/24`).
fn numbers_page(text: &str, place: &str, pages: usize) -> bool {
    let runs = digit_runs(text);
    runs.first().map(String::as_str) == Some(place)
        && !names_something(text)
        && (text == place
            || says_page(text)
            || runs.len() == 2 && text.contains('/') && runs[1] == pages.to_string())
}

/// Whether body text stands between line `li` of page `pi` and the edge of
/// the page it is nearer, in the line's column: a line on too few pages to
/// run (fewer than `needed`), above a line in the top half, below one in
/// the foot, set from the same left edge, a line of a paragraph there (see
/// [`in_paragraph`]). A guide printed in two versions
/// of a language repeats its body on two pages, and it is the body still.
/// A page's banner title over a running label set off to its right
/// (`Federal Reserve Bank of Philadelphia` over `The Beige Book`) is no
/// column of text the label sits in.
fn beyond_the_body(
    pages: &[(Vec<Line>, f64)],
    pi: usize,
    li: usize,
    seen_on: &HashMap<String, usize>,
    keys: &[Vec<String>],
    needed: usize,
) -> bool {
    let (lines, height) = &pages[pi];
    let line = &lines[li];
    if *height <= 0.0 || line.rotation_bucket != 0 {
        return false;
    }
    let top = (line.bbox.y0 + line.bbox.y1) / 2.0 < height / 2.0;
    lines.iter().enumerate().any(|(lj, o)| {
        lj != li
            && o.rotation_bucket == 0
            && !o.text.trim().is_empty()
            && o.bbox.x0 < line.bbox.x1
            && o.bbox.x1 > line.bbox.x0
            && (o.bbox.x0 - line.bbox.x0).abs() <= f64::from(line.font_size.max(1.0))
            && if top {
                o.bbox.y1 <= line.bbox.y0
            } else {
                o.bbox.y0 >= line.bbox.y1
            }
            && seen_on.get(&keys[pi][lj]).copied().unwrap_or(1) < needed
            && in_paragraph(lines, lj, li)
    })
}

/// Whether the page's own figures stand between line `li` and the page's
/// edge, overlapping it across: a line holding a figure that runs on no
/// page but its own (its group seen on fewer than `needed` pages), or a
/// table read on the page, set between the line and the head of the page
/// where the line is in its top half, or the foot where it is in its
/// bottom half. A running head or foot is the page's margin, and only the
/// page's furniture (its number) stands beyond it; a template's table
/// heads have each page's values under them, and a section's band each
/// page's key figures over it.
fn figures_beyond(
    pages: &[(Vec<Line>, f64)],
    pi: usize,
    li: usize,
    tables: &[crate::geom::BBox],
    seen_on: &HashMap<String, usize>,
    keys: &[Vec<String>],
    needed: usize,
) -> bool {
    let (lines, height) = &pages[pi];
    let line = &lines[li];
    if *height <= 0.0 || line.rotation_bucket != 0 {
        return false;
    }
    let top = (line.bbox.y0 + line.bbox.y1) / 2.0 < height / 2.0;
    let beyond = |b: &crate::geom::BBox| {
        b.x0 < line.bbox.x1
            && b.x1 > line.bbox.x0
            && if top {
                b.y1 <= line.bbox.y0
            } else {
                b.y0 >= line.bbox.y1
            }
    };
    tables.iter().any(beyond)
        || lines.iter().enumerate().any(|(lj, o)| {
            lj != li
                && o.rotation_bucket == 0
                && o.text.chars().any(|c| c.is_ascii_digit())
                && o.bbox.x0 < line.bbox.x1
                && o.bbox.x1 > line.bbox.x0
                && if top {
                    o.bbox.y1 <= line.bbox.y0
                } else {
                    o.bbox.y0 >= line.bbox.y1
                }
                && seen_on.get(&keys[pi][lj]).copied().unwrap_or(1) < needed
        })
}

/// Whether line `lj` is a line of a paragraph: another line, not `skip`,
/// set from its left edge a line's leading above or below it. A header
/// block's lines stand alone, each a label of its own (`INSTALLATION
/// INSTRUCTIONS` over its translation), and are no body a heading follows.
fn in_paragraph(lines: &[Line], lj: usize, skip: usize) -> bool {
    let o = &lines[lj];
    let fs = f64::from(o.font_size.max(1.0));
    lines.iter().enumerate().any(|(k, p)| {
        k != lj
            && k != skip
            && p.rotation_bucket == 0
            && !p.text.trim().is_empty()
            && (p.bbox.x0 - o.bbox.x0).abs() <= fs
            && {
                let d = (p.bbox.y0 - o.bbox.y0).abs();
                d > fs * 0.5 && d <= fs * LEADING_LIMIT
            }
    })
}

/// How far apart, in font sizes, two lines of one paragraph sit at most.
const LEADING_LIMIT: f64 = 1.6;

/// The words `longer` sets after or before `short`, where it opens or
/// closes with it.
fn extends<'a>(longer: &'a str, short: &str) -> Option<&'a str> {
    if short.is_empty() || longer.len() <= short.len() {
        return None;
    }
    if longer.starts_with(short) && longer[short.len()..].starts_with(char::is_whitespace) {
        return Some(longer[short.len()..].trim());
    }
    let cut = longer.len() - short.len();
    (longer.ends_with(short) && longer[..cut].ends_with(char::is_whitespace))
        .then(|| longer[..cut].trim())
}

/// Whether `rest`, the words a longer line sets beside a running line's,
/// open or close with another line that runs: the longer line is running
/// lines set as one (`Introduction Environmental Initiatives`, `Former Doc.:
/// Contract Review`), not a title of the page's own.
fn merges_running(rest: &str, running: &[String]) -> bool {
    running.iter().any(|t| {
        let t = t.as_str();
        rest == t
            || (rest.starts_with(t) && rest[t.len()..].starts_with(char::is_whitespace))
            || (rest.ends_with(t) && rest[..rest.len() - t.len()].ends_with(char::is_whitespace))
    })
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
///   A page number's words differ on every page, so no page could restate
///   them: it counts the pages instead, which a signature block does not.
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
    let mut texts: Vec<&str> = hits.iter().map(|h| h.3.as_str()).collect();
    texts.sort_unstable();
    texts.dedup();
    let digits_vary = texts.len() > 1;
    // A page number's words change from page to page, so the page left out
    // cannot print them: a cover with no number before `Page 2 of 3` and
    // `Page 3 of 3`. It counts the pages instead (below).
    let says: String = hits[0].3.split_whitespace().collect::<Vec<_>>().join(" ");
    if !digits_vary && (0..pages.len()).any(|pi| !on.contains(&pi) && !words(pi).contains(&says)) {
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

/// A short document's running line that page 2 sets on another template
/// than page 1, in another place.
///
/// A two-page data sheet may move its margins on page 2, and restate page
/// 1's title as page 2's running head at the top. The copies then agree in
/// text, size and weight, and lie in the same band, but not in place. Two
/// things show they are the template's all the same:
///
/// - **A line beside it moved with it**: the footer's next line, the
///   title's subtitle, each set the same distance off its page 1 copy.
/// - **It opens page 2**, the topmost line there, and stands lower on page
///   1, under the masthead above the title.
///
/// A title restated as the next page's head keeps its page 1 copy, the
/// document's title: only the copy that opens a later page, higher on it,
/// is running. Copies that move otherwise, a footer, are running on both.
fn shifted_running<'a>(
    pages: &[(Vec<Line>, f64)],
    occ: &'a [(usize, usize, f64, String)],
) -> Option<(Furniture, Copies<'a>)> {
    if pages.len().saturating_sub(1).max(2) != 2 {
        return None;
    }
    let line = |o: &(usize, usize, f64, String)| &pages[o.0].0[o.1];
    let band = |rel: f64| {
        if rel < EDGE_BAND {
            Some(true)
        } else if rel > 1.0 - EDGE_BAND {
            Some(false)
        } else {
            None
        }
    };
    let alike = |a: &Line, b: &Line| {
        let size = f64::from(a.font_size.max(b.font_size)).max(1.0);
        a.rotation_bucket == b.rotation_bucket
            && a.text == b.text
            && f64::from((a.font_size - b.font_size).abs()) <= EXACT_PLACE * size
            && a.bold == b.bold
    };
    // `b` is where `a` would be, moved by `(dx, dy)`.
    let moved = |a: &Line, b: &Line, dx: f64, dy: f64| {
        let size = f64::from(a.font_size.max(b.font_size)).max(1.0);
        let near = |p: f64, q: f64| (p - q).abs() <= EXACT_PLACE * size;
        alike(a, b)
            && near(a.bbox.y0 + dy, b.bbox.y0)
            && near(a.bbox.y1 + dy, b.bbox.y1)
            && (near(a.bbox.x0 + dx, b.bbox.x0) || near(a.bbox.x1 + dx, b.bbox.x1))
    };
    let first = occ.iter().map(|o| o.0).min()?;
    let words = |pi: usize| {
        pages[pi]
            .0
            .iter()
            .flat_map(|l| l.text.split_whitespace())
            .collect::<Vec<_>>()
            .join(" ")
    };
    for r in occ.iter().filter(|o| o.0 == first) {
        let Some(top) = band(r.2) else { continue };
        let a = line(r);
        // On each later page, the copy the template moved, if any.
        let mut later: Vec<&(usize, usize, f64, String)> = Vec::new();
        let mut raised = true;
        for o in occ.iter().filter(|o| o.0 != first) {
            let b = line(o);
            if later.iter().any(|h| h.0 == o.0) || band(o.2) != Some(top) || !alike(a, b) {
                continue;
            }
            let dx = b.bbox.x0 - a.bbox.x0;
            let dy = b.bbox.y0 - a.bbox.y0;
            // Its neighbours on page 1 are where they would be, moved alike.
            let neighbours: Vec<&Line> =
                pages[first].0.iter().filter(|nb| runs_on(a, nb)).collect();
            let carried = |nb: &&Line| pages[o.0].0.iter().any(|m| moved(nb, m, dx, dy));
            let with_a_neighbour = !neighbours.is_empty() && neighbours.iter().all(carried);
            let opens_page = top
                && dy < 0.0
                && pages[o.0]
                    .0
                    .iter()
                    .filter(|l| l.rotation_bucket == 0)
                    .all(|l| l.bbox.y0 >= b.bbox.y0 - EXACT_PLACE * f64::from(b.font_size));
            // A line set beside the copy on its page was moved alike, or is
            // none of the template's.
            let beside_carried = pages[o.0]
                .0
                .iter()
                .filter(|nb| runs_on(b, nb))
                .all(|nb| pages[first].0.iter().any(|m| moved(nb, m, -dx, -dy)));
            if (with_a_neighbour || opens_page) && beside_carried {
                raised &= top && dy < 0.0;
                later.push(o);
            }
        }
        let on: Vec<usize> = std::iter::once(first)
            .chain(later.iter().map(|o| o.0))
            .collect();
        if on.len() < 2 {
            continue;
        }
        // A page left out restates the words, as a title page does; two
        // signature pages that repeat their block do not run it.
        let says: String = a.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if (0..pages.len()).any(|pi| !on.contains(&pi) && !words(pi).contains(&says)) {
            continue;
        }
        let kind = classify(later[0].2, false);
        if kind == Furniture::Watermark {
            return None;
        }
        // The title page keeps its title.
        let mut copies: Copies<'a> = later.iter().map(|o| (o.0, o.1, o.3.as_str())).collect();
        if !raised {
            copies.insert(0, (r.0, r.1, r.3.as_str()));
        }
        return Some((kind, copies));
    }
    None
}

/// A page template's title, set below its header band: the same line at
/// the same place on every page, to the point, saying something in words,
/// set larger than the body by the margin a font-size heading needs, and
/// with nothing of the page's own above it anywhere across the page, only
/// the header band. Datasheets of one table over several pages print the
/// table's title on each (`Dimensions of Hexagon Socket Head Cap Screws`).
/// The first page's copy is the document's title; the copies after it,
/// returned here, are running. Text that only repeats, a table's column
/// labels on every page, stands under the title, or at the body's size.
fn template_title(
    pages: &[(Vec<Line>, f64)],
    occ: &[(usize, usize, f64, String)],
    body: f64,
) -> Option<Vec<(usize, usize)>> {
    let line = |o: &(usize, usize, f64, String)| &pages[o.0].0[o.1];
    let first = occ.iter().min_by_key(|o| o.0)?;
    let a = line(first);
    let mut on: Vec<usize> = occ.iter().map(|o| o.0).collect();
    on.sort_unstable();
    on.dedup();
    let every = on.len() == pages.len() && on.len() == occ.len();
    let larger = a.font_size as f64 > body * (1.0 + crate::heading::HEADING_SIZE_MARGIN);
    // A title says something in words: a drawing's callouts (`1`, `2`,
    // `4`) stand at their places on each page as well.
    if !every || !larger || !names_something(&a.text) || !occ.iter().all(|o| same_place(a, line(o)))
    {
        return None;
    }
    // Above it, across the page, only the header band: it opens the
    // page's text. A drawing's label repeated on each slide stands among
    // the slide's text.
    let heads = occ.iter().all(|o| {
        let (lines, height) = &pages[o.0];
        let l = line(o);
        lines
            .iter()
            .enumerate()
            .all(|(lj, m)| lj == o.1 || m.bbox.y1 > l.bbox.y0 || m.bbox.y0 / height < EDGE_BAND)
    });
    heads.then(|| {
        occ.iter()
            .filter(|o| o.0 != first.0)
            .map(|o| (o.0, o.1))
            .collect()
    })
}

/// Widest difference, in a line's size, between where a moved copy stands
/// and where its template's move puts it. A mirrored copy is measured edge
/// to opposite edge, the first letter's side bearing against the last's,
/// and each line's ink sits its own way on its baseline when the band is
/// set lower: a fifth of an em apart in a catalogue's footer, short of the
/// line's own height that another line would stand away.
const MOVED_PLACE: f64 = 0.25;

/// Furthest, in a line's size, a facing page sets its band from where page
/// 1 sets it: within the line's own height, a few points. A page set
/// further off is on another template, which [`shifted_running`] reads by
/// a line moved with its neighbour.
const MAX_MOVE: f64 = 1.0;

/// How a short document's later page sets one of its edge bands against
/// page 1's: reflected about the page's centre (facing pages, the outer
/// margin on each side), and moved up or down by a few points.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Move {
    page: usize,
    /// The page's width, the axis a mirrored copy is reflected about.
    width: f64,
    top: bool,
    mirrored: bool,
    dy: f64,
}

/// Whether `b`, on a page `width` wide, stands where `a` stands, mirrored
/// or not, and moved down by `dy`, to within [`MOVED_PLACE`] of its size.
fn placed_by(a: &Line, b: &Line, width: f64, mirrored: bool, dy: f64) -> bool {
    let size = f64::from(a.font_size.max(b.font_size)).max(1.0);
    let near = |p: f64, q: f64| (p - q).abs() <= MOVED_PLACE * size;
    let (ca, cb) = ((a.bbox.x0 + a.bbox.x1) / 2.0, (b.bbox.x0 + b.bbox.x1) / 2.0);
    let across = if mirrored {
        near(a.bbox.x0, width - b.bbox.x1)
            || near(a.bbox.x1, width - b.bbox.x0)
            || near(ca, width - cb)
    } else {
        near(a.bbox.x0, b.bbox.x0) || near(a.bbox.x1, b.bbox.x1) || near(ca, cb)
    };
    across && near(b.bbox.y0 - a.bbox.y0, dy) && near(b.bbox.y1 - a.bbox.y1, dy)
}

/// The moves a short document's later pages set their edge bands by,
/// where two lines or more of a band are copies of page 1's, alike in text,
/// size and weight, and the page set every one of them the same way: the
/// template moved, not a line. A catalogue's facing pages swap the sides
/// of their footer (`114` and the sales address) and set it a few points
/// lower, the phone line and the print notice moving with it. The move that
/// changes nothing is [`exact_running`]'s.
fn template_moves(pages: &[(Vec<Line>, f64)], widths: &[f64]) -> Vec<Move> {
    if pages.len().saturating_sub(1).max(2) != 2 || widths.len() != pages.len() {
        return Vec::new();
    }
    let band = |l: &Line, height: f64| {
        let rel = l.bbox.y0 / height;
        if rel < EDGE_BAND {
            Some(true)
        } else if rel > 1.0 - EDGE_BAND {
            Some(false)
        } else {
            None
        }
    };
    let alike = |a: &Line, b: &Line| {
        let size = f64::from(a.font_size.max(b.font_size)).max(1.0);
        a.rotation_bucket == b.rotation_bucket
            && a.text == b.text
            && f64::from((a.font_size - b.font_size).abs()) <= EXACT_PLACE * size
            && a.bold == b.bold
    };
    let (first, h0) = (&pages[0].0, pages[0].1);
    let mut moves = Vec::new();
    for (pj, (later, hj)) in pages.iter().enumerate().skip(1) {
        if h0 <= 0.0 || *hj <= 0.0 {
            continue;
        }
        let width = widths[pj];
        for top in [true, false] {
            // Each line of page 1's band and its copy on this page.
            let pairs: Vec<(&Line, &Line)> = first
                .iter()
                .filter(|a| band(a, h0) == Some(top) && !a.text.trim().is_empty())
                .filter_map(|a| {
                    later
                        .iter()
                        .find(|b| band(b, *hj) == Some(top) && alike(a, b))
                        .map(|b| (a, b))
                })
                .collect();
            for &(a, b) in &pairs {
                for mirrored in [true, false] {
                    let dy = b.bbox.y0 - a.bbox.y0;
                    let size = f64::from(a.font_size).max(1.0);
                    if (!mirrored && dy.abs() <= EXACT_PLACE * size) || dy.abs() > MAX_MOVE * size {
                        continue;
                    }
                    let set: Vec<&str> = pairs
                        .iter()
                        .filter(|(x, y)| placed_by(x, y, width, mirrored, dy))
                        .map(|(x, _)| x.text.as_str())
                        .collect();
                    // A mirror is seen where a line swapped sides: a line
                    // centred on the page stands where a mirror puts it and
                    // where it stood. Seen once where the band moved down
                    // too, twice where it did not.
                    let swapped = pairs
                        .iter()
                        .filter(|(x, y)| {
                            placed_by(x, y, width, true, dy) && !placed_by(x, y, width, false, dy)
                        })
                        .count();
                    let moved_down = dy.abs() > EXACT_PLACE * size;
                    if mirrored && swapped < if moved_down { 1 } else { 2 } {
                        continue;
                    }
                    let m = Move {
                        page: pj,
                        width,
                        top,
                        mirrored,
                        dy,
                    };
                    if set.len() >= 2 && !moves.iter().any(|o: &Move| close_moves(o, &m, size)) {
                        moves.push(m);
                    }
                }
            }
        }
    }
    moves
}

/// Two moves found from different lines that are one.
fn close_moves(a: &Move, b: &Move, size: f64) -> bool {
    a.page == b.page
        && a.top == b.top
        && a.mirrored == b.mirrored
        && (a.dy - b.dy).abs() <= MOVED_PLACE * size
}

/// A short document's running line that a later page sets where its
/// template moved it ([`template_moves`]): page 1's copy and the later
/// page's, alike in size and weight and in the same band, the later copy
/// placed by the band's move. Its text is the same, or a page number that
/// counts the pages (`114`, `115`). In the head band, page 1 keeps its
/// copy: a catalogue's banner over its first page is its title, set again
/// over the facing page.
fn mirrored_running<'a>(
    pages: &[(Vec<Line>, f64)],
    moves: &[Move],
    occ: &'a [(usize, usize, f64, String)],
) -> Option<(Furniture, Copies<'a>)> {
    if moves.is_empty() {
        return None;
    }
    let line = |o: &(usize, usize, f64, String)| &pages[o.0].0[o.1];
    let first = occ.iter().find(|o| o.0 == 0)?;
    let a = line(first);
    let first_top = first.2 < EDGE_BAND;
    if !first_top && first.2 <= 1.0 - EDGE_BAND {
        return None;
    }
    let mut later: Vec<&(usize, usize, f64, String)> = Vec::new();
    for o in occ.iter().filter(|o| o.0 != 0) {
        let b = line(o);
        let size = f64::from(a.font_size.max(b.font_size)).max(1.0);
        let style = a.rotation_bucket == b.rotation_bucket
            && f64::from((a.font_size - b.font_size).abs()) <= EXACT_PLACE * size
            && a.bold == b.bold;
        let top = o.2 < EDGE_BAND;
        let in_band = if first_top {
            top
        } else {
            o.2 > 1.0 - EDGE_BAND
        };
        let placed = moves
            .iter()
            .any(|m| m.page == o.0 && m.top == top && placed_by(a, b, m.width, m.mirrored, m.dy));
        if style && in_band && placed && !later.iter().any(|h| h.0 == o.0) {
            later.push(o);
        }
    }
    if later.is_empty() || later.len() + 1 < pages.len().saturating_sub(1).max(2) {
        return None;
    }
    let mut copies: Vec<&(usize, usize, f64, String)> =
        std::iter::once(first).chain(later).collect();
    let texts: std::collections::HashSet<&str> = copies.iter().map(|c| c.3.as_str()).collect();
    let digits_vary = texts.len() > 1;
    if digits_vary {
        let numbers: Vec<(usize, &str)> = copies.iter().map(|c| (c.0, c.3.as_str())).collect();
        if texts.iter().any(|t| names_something(t)) || !advances(&numbers) {
            return None;
        }
    }
    let kind = classify(copies[0].2, digits_vary);
    if kind == Furniture::Header {
        copies.remove(0);
    }
    Some((
        kind,
        copies.iter().map(|c| (c.0, c.1, c.3.as_str())).collect(),
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
/// beside every other line, sharing none of their width, out in a margin
/// as a note is. In a margin, past the page's other lines to the left or
/// the right: a page title over a table, whose cells are no lines here,
/// shares no line's width either, and is set between the running head's
/// and the footer's ends.
///
/// A turned line is a watermark set across the page large, or a stamp or
/// a thumb index's tabs along its edge, past its level text. A chart turns
/// its own labels too, at the text's size or smaller, among the page's
/// level lines: the years under a bar chart's bars, the axis's title
/// beside its scale (`Mt CO2eq`). Set no larger than the body (`body`),
/// sharing the width of a level line and lying on none, a turned line is
/// the page's, and shows nothing of a watermark.
fn watermark_shown(lines: &[Line], li: usize, body: f64) -> bool {
    let l = &lines[li];
    let others = lines.iter().enumerate().filter(|(j, _)| *j != li);
    if l.rotation_bucket != 0 {
        let across = |o: &Line| l.bbox.x1.min(o.bbox.x1) - l.bbox.x0.max(o.bbox.x0);
        let among = f64::from(l.font_size) <= body
            && others
                .clone()
                .any(|(_, o)| o.rotation_bucket == 0 && across(o) > 0.0);
        return !among || others.clone().any(|(_, o)| lies_on(l, o));
    }
    let (left, right) = others.clone().fold((f64::MAX, f64::MIN), |(a, b), (_, o)| {
        (a.min(o.bbox.x0), b.max(o.bbox.x1))
    });
    let mut beside = left > right || l.bbox.x1 <= left || l.bbox.x0 >= right;
    for (_, o) in others {
        if l.bbox.x1.min(o.bbox.x1) - l.bbox.x0.max(o.bbox.x0) > 0.0 {
            beside = false;
            if lies_on(l, o) {
                return true;
            }
        }
    }
    beside
}

/// Whether line `l` lies on line `o`: their boxes overlap by more than
/// [`LAID_ON`] of the smaller.
fn lies_on(l: &Line, o: &Line) -> bool {
    let w = l.bbox.x1.min(o.bbox.x1) - l.bbox.x0.max(o.bbox.x0);
    let h = l.bbox.y1.min(o.bbox.y1) - l.bbox.y0.max(o.bbox.y0);
    let smaller = (l.bbox.width() * l.bbox.height()).min(o.bbox.width() * o.bbox.height());
    w > 0.0 && h > 0.0 && smaller > 0.0 && w * h / smaller > LAID_ON
}

/// How much of the smaller of two lines' boxes their overlap covers, at
/// least, for one to lie on the other.
const LAID_ON: f64 = 0.3;

/// A page's drawn shapes, its fills, strokes and traces, and its tick
/// marks: what [`in_drawing`] reads a line against. And the boxes of the
/// tables read on the page, the page's own content ([`figures_beyond`]).
#[derive(Debug, Clone, Default)]
pub struct Drawing {
    pub shapes: Vec<crate::geom::BBox>,
    pub ticks: Vec<crate::geom::BBox>,
    pub tables: Vec<crate::geom::BBox>,
}

/// Whether a line is set in a chart's drawing: a legend's entry, with its
/// swatch standing just before it on its line, or an axis's label, at a
/// tick mark aligned with it.
///
/// Running heads and feet sit on drawn shapes too (a band behind the head,
/// a page number in a box, a logo beside the title), so the cues are
/// narrow:
/// - **A swatch** is a small patch, no larger than the line's text is
///   tall ([`SWATCH_SIZE`]) and no thinner than [`MIN_SWATCH`] of it, its
///   middle on the line, standing alone directly before the entry, within
///   [`SWATCH_GAP_EMS`]: not the last of a logo's letters drawn as shapes.
/// - **A tick** is a short stroke, no longer than an em, set at the
///   label's middle across the axis: under or over a label within its
///   width, or beside one within its height, and within [`TICK_GAP_EMS`].
///
/// `shapes` are the page's fills, strokes and traces; `ticks` its tick
/// marks.
pub fn in_drawing(line: &Line, shapes: &[crate::geom::BBox], ticks: &[crate::geom::BBox]) -> bool {
    let fs = f64::from(line.font_size.max(1.0));
    let b = &line.bbox;
    let tall = b.height().max(fs);
    let patch = |m: &crate::geom::BBox| {
        m.width().min(m.height()) >= MIN_SWATCH * tall
            && m.width() <= SWATCH_SIZE * tall
            && m.height() <= SWATCH_SIZE * tall
    };
    let swatch = shapes.iter().any(|m| {
        let mid = (m.y0 + m.y1) / 2.0;
        let gap = b.x0 - m.x1;
        if !(patch(m) && mid >= b.y0 && mid <= b.y1 && gap >= 0.0 && gap <= SWATCH_GAP_EMS * fs) {
            return false;
        }
        // A logo's letters drawn as shapes abut one another on their line:
        // the last of them is no swatch.
        !shapes.iter().any(|o| {
            (o.height() - m.height()).abs() <= m.height() * 0.3
                && o.width() >= MIN_SWATCH * tall
                && o.x1 <= m.x0 + 0.5
                && m.x0 - o.x1 <= m.height() * 0.5
                && (o.y0 + o.y1) / 2.0 >= m.y0
                && (o.y0 + o.y1) / 2.0 <= m.y1
        })
    });
    let at_tick = ticks.iter().any(|t| {
        if t.width().max(t.height()) > fs {
            return false;
        }
        let (tx, ty) = ((t.x0 + t.x1) / 2.0, (t.y0 + t.y1) / 2.0);
        let near = |g: f64| (0.0..=TICK_GAP_EMS * fs).contains(&g);
        let across = tx >= b.x0 && tx <= b.x1 && (near(b.y0 - t.y1) || near(t.y0 - b.y1));
        let beside = ty >= b.y0 && ty <= b.y1 && (near(t.x0 - b.x1) || near(b.x0 - t.x1));
        across || beside
    });
    swatch || at_tick
}

/// A legend's swatch at most as large as its entry's text is tall, a
/// colour patch or a series' marker; a bullet is as small, but no running
/// line repeats a bullet's item at one place page after page.
pub const SWATCH_SIZE: f64 = 1.0;
/// And at least this share of it each way: a patch, not the stroke of a
/// box's edge beside a page number.
const MIN_SWATCH: f64 = 0.3;
/// How far before its entry a swatch stands, in the entry's ems.
const SWATCH_GAP_EMS: f64 = 1.0;
/// How far an axis's label stands from its tick, in ems.
const TICK_GAP_EMS: f64 = 1.0;

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
        names_something(text) || carries_code(text)
    } else {
        text.chars().any(char::is_alphanumeric)
    }
}

/// Whether a line carries a code of letters and figures, a document's
/// number in its footer beside the page's: `369613g 1 04.08.24`. Four
/// characters or more, a letter and a figure among them, and no paging
/// word (`p3`).
fn carries_code(text: &str) -> bool {
    text.split_whitespace().any(|w| {
        let w: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
        let lower = w.to_lowercase();
        w.chars().count() >= 4
            && w.chars().any(|c| c.is_ascii_digit())
            && w.chars().any(char::is_alphabetic)
            && !PAGE_WORDS.iter().any(|p| {
                lower
                    .strip_prefix(p)
                    .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()))
            })
    })
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
    crate::document::renumber(elements);
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
            italic: false,
            x_height: None,
            compounds: Vec::new(),
            opens_bold: false,
        }
    }

    fn doc(n: usize, f: impl Fn(usize) -> Vec<Line>) -> Vec<(Vec<Line>, f64)> {
        (0..n).map(|i| (f(i), 1000.0)).collect()
    }

    /// A line at `y` set larger than the test lines' body, as a heading is.
    fn head(text: &str, y: f64) -> Line {
        let mut l = line(text, y);
        l.font_size = 16.0;
        l
    }

    fn at(x0: f64, y0: f64, x1: f64, y1: f64) -> BBox {
        BBox { x0, y0, x1, y1 }
    }

    #[test]
    fn a_legends_entry_stands_beside_its_swatch() {
        // `Buildings`, 10pt, from x 10: a 7pt colour patch 4pt before it,
        // its middle on the line.
        let entry = line("Buildings", 100.0);
        assert!(in_drawing(&entry, &[at(-1.0, 102.0, 6.0, 109.0)], &[]));
        // A patch beside the line but above it is another entry's.
        assert!(!in_drawing(&entry, &[at(-1.0, 85.0, 6.0, 92.0)], &[]));
        // A band behind a running head, a logo beside the title, or a box
        // round a page number is larger than the text.
        assert!(!in_drawing(&entry, &[at(-40.0, 95.0, 6.0, 118.0)], &[]));
        assert!(!in_drawing(&entry, &[at(0.0, 96.0, 300.0, 116.0)], &[]));
        // A series' line drawn as its sample, wider than the text is tall,
        // is left to a cue of its own.
        assert!(!in_drawing(&entry, &[at(-20.0, 106.0, 6.0, 106.0)], &[]));
        // Text after the patch is not its entry: the patch must stand before.
        assert!(!in_drawing(&entry, &[at(204.0, 102.0, 211.0, 109.0)], &[]));
        // A box's edge before a page number is a stroke, not a patch.
        assert!(!in_drawing(&entry, &[at(7.0, 100.0, 7.0, 112.0)], &[]));
        // A logo's letters drawn as shapes, abutting, before a running line.
        let logo = [
            at(-22.0, 101.0, -12.0, 110.0),
            at(-11.5, 101.0, -1.5, 110.0),
            at(-1.0, 101.0, 6.0, 110.0),
        ];
        assert!(!in_drawing(&entry, &logo, &[]));
    }

    #[test]
    fn an_axis_label_stands_at_its_tick() {
        let mut label = line("2010", 300.0);
        label.bbox = at(100.0, 300.0, 120.0, 310.0);
        // A 4pt tick over the label's middle, 2pt above it.
        assert!(in_drawing(&label, &[], &[at(110.0, 294.0, 110.0, 298.0)]));
        // A tick beside the next label is not this one's.
        assert!(!in_drawing(&label, &[], &[at(130.0, 294.0, 130.0, 298.0)]));
        // A long stroke is an axis or a rule, not a tick.
        assert!(!in_drawing(&label, &[], &[at(110.0, 280.0, 110.0, 298.0)]));
    }

    #[test]
    fn a_templates_heads_over_each_pages_figures_are_not_running() {
        // Twelve economies' pages, each setting a table at its foot: its
        // heads at one place on every page, each page's own counts under
        // them, and the report's running foot under all of it.
        let pages = doc(12, |i| {
            vec![
                line(&format!("Economy {i}"), 20.0),
                line(&format!("Body text of page {i}."), 400.0),
                line("Residents Non-residents Total", 860.0),
                line(&format!("{} {} {}", 23 + i * 7, 1 + i, 24 + i * 8), 875.0),
                line("World Trade Report", 960.0),
            ]
        });
        let (marks, _, _) = detect_with_heads(&pages, &[], &[], &[]);
        for m in &marks {
            assert!(!m.contains_key(&2), "the heads are the page's: {m:?}");
            assert!(m.contains_key(&4), "the running foot runs: {m:?}");
        }
        // A section's band under each page's key figures, read as a table.
        let pages = doc(12, |i| {
            vec![
                line(&format!("Economy {i}"), 20.0),
                line("MERCHANDISE TRADE", 140.0),
                line(&format!("Body text of page {i}."), 400.0),
                line("World Trade Report", 960.0),
            ]
        });
        let drawings: Vec<Drawing> = (0..12)
            .map(|_| Drawing {
                tables: vec![at(10.0, 60.0, 200.0, 130.0)],
                ..Default::default()
            })
            .collect();
        let (marks, _, _) = detect_with_heads(&pages, &[], &[], &drawings);
        assert!(marks.iter().all(|m| !m.contains_key(&1)), "{marks:?}");
        // Without the table, the band runs.
        let (marks, _, _) = detect_with_heads(&pages, &[], &[], &[]);
        assert!(marks.iter().all(|m| m.contains_key(&1)));
    }

    #[test]
    fn a_charts_turned_labels_among_the_text_are_no_watermark() {
        // A line set turned at `x0`, `size` points, from `y` down.
        let turned = |text: &str, x0: f64, y: f64, size: f32| {
            let mut l = line_at(text, y, x0, x0 + f64::from(size) * 0.7);
            l.bbox.y1 = y + f64::from(size) * 2.5;
            l.rotation_bucket = 90;
            l.font_size = size;
            l
        };
        // Twelve country pages, each with its chart's years set turned under
        // its bars, its axis's title turned beside its scale, a stamp turned
        // in the margin past the text, and a large turned watermark.
        let pages = doc(12, |i| {
            vec![
                line(&format!("Country {i}"), 40.0),
                line_at(&format!("{}", 5000 + i * 100), 150.0, 30.0, 50.0),
                turned("Mt CO2eq", 12.0, 230.0, 10.0),
                turned("1990", 60.0, 380.0, 10.0),
                turned("1992", 80.0, 380.0, 10.0),
                line(&format!("Body text of page {i}."), 450.0),
                turned("Verified at example.org", 280.0, 600.0, 7.0),
                turned("DRAFT", 120.0, 560.0, 40.0),
            ]
        });
        let (marks, _, _) = detect_with_heads(&pages, &[], &[], &[]);
        for m in &marks {
            for li in [2, 3, 4] {
                assert!(
                    !m.contains_key(&li),
                    "the chart's label {li} is the page's: {m:?}"
                );
            }
            assert_eq!(m.get(&6), Some(&Furniture::Watermark), "the stamp: {m:?}");
            assert_eq!(
                m.get(&7),
                Some(&Furniture::Watermark),
                "the watermark: {m:?}"
            );
        }
    }

    #[test]
    fn a_charts_legend_repeated_on_every_page_is_not_running() {
        // Twelve country pages, each drawing one chart whose legend
        // entries stand at one place, under a running head on every page.
        let pages = doc(12, |i| {
            vec![
                line("Annual report on emissions", 20.0),
                line(&format!("Country {i}"), 60.0),
                line("Power Industry", 105.0),
                line("Transport", 141.0),
                line(&format!("Body text of page {i}."), 500.0),
            ]
        });
        // Each entry's 9pt swatch stands 3pt before it.
        let drawings: Vec<Drawing> = (0..12)
            .map(|_| Drawing {
                shapes: vec![at(-2.0, 107.0, 7.0, 116.0), at(-2.0, 143.0, 7.0, 152.0)],
                ticks: vec![],
                tables: vec![],
            })
            .collect();
        let (marks, _, _) = detect_with_heads(&pages, &[], &[], &drawings);
        for m in &marks {
            assert!(m.contains_key(&0), "the running head runs");
            assert!(!m.contains_key(&2) && !m.contains_key(&3), "{m:?}");
        }
        // Read without the drawing, the entries are taken for running heads.
        let (marks, _, _) = detect_with_heads(&pages, &[], &[], &[]);
        assert!(marks.iter().all(|m| m.contains_key(&2)));
    }

    #[test]
    fn a_marking_over_the_page_and_at_its_foot_runs_at_both() {
        // A classification marking set at the head and again at the foot
        // of every page: one text at two places, each running.
        let pages = doc(10, |i| {
            vec![
                line("TLP:CLEAR", 20.0),
                line(&format!("Text of page {i}, its own."), 400.0),
                line("TLP:CLEAR", 960.0),
            ]
        });
        let marks = detect(&pages);
        for (i, m) in marks.iter().enumerate() {
            assert!(m.contains_key(&0), "head, page {i}");
            assert!(m.contains_key(&2), "foot, page {i}");
            assert!(!m.contains_key(&1), "text, page {i}");
        }
    }

    #[test]
    fn a_running_head_naming_its_section_keeps_its_first_copy() {
        // Twelve pages; the head slot names the section the page is in:
        // `Installation` on pages 0-3, `Maintenance` on 4-8, `Warranty`
        // on 9-11, under a series name that runs on every page.
        let name = |i: usize| match i {
            0..=3 => "Installation",
            4..=8 => "Maintenance",
            _ => "Warranty",
        };
        let sheet = |set: fn(&str, f64) -> Line| {
            doc(12, move |i| {
                vec![
                    line("Model 40 Series", 30.0),
                    set(name(i), 60.0),
                    line(&format!("Text on page {i} of its section."), 400.0),
                ]
            })
        };
        let firsts = [
            (0, true),
            (1, false),
            (4, true),
            (7, false),
            (9, true),
            (11, false),
        ];
        let (marks, _, heads) = detect_with_heads(&sheet(head), &[], &[], &[]);
        for (i, first) in firsts {
            assert_eq!(heads[i].contains(&1), first, "page {i}");
            assert_eq!(marks[i].contains_key(&1), !first, "page {i}");
        }
        // Set at the body's size, the names are told from the page by
        // their repetition alone, and left to it.
        let pages = sheet(line);
        let (marks, _, heads) = detect_with_heads(&pages, &[], &[], &[]);
        assert!(heads.iter().all(|h| h.is_empty()));
        assert_eq!(marks, detect(&pages));
        // Unless the outline names each where it starts.
        let bookmarks: Vec<(usize, String)> = [0, 4, 9]
            .into_iter()
            .map(|i| (i, name(i).to_string()))
            .collect();
        let (marks, _, heads) = detect_with_heads(&pages, &bookmarks, &[], &[]);
        for (i, first) in firsts {
            assert_eq!(heads[i].contains(&1), first, "page {i}");
            assert_eq!(marks[i].contains_key(&1), !first, "page {i}");
        }
        // A section whose heading is set in its first page's body: every
        // copy in the slot runs.
        let pages = doc(12, |i| {
            let mut ls = vec![line("Model 40 Series", 30.0), head(name(i), 60.0)];
            if i == 4 {
                ls.push(head("Maintenance", 300.0));
            }
            ls.push(line(&format!("Text on page {i}."), 400.0));
            ls
        });
        let (_, _, heads) = detect_with_heads(&pages, &[], &[], &[]);
        assert!(!heads[4].contains(&1));
        assert!(heads[0].contains(&1));
        // A chapter's title over the page before has it too; an agenda's
        // entry there, set smaller than the head, is no heading.
        for (size, kept) in [(24.0, false), (10.0, true)] {
            let pages = doc(12, |i| {
                let mut ls = vec![line("Model 40 Series", 30.0), head(name(i), 60.0)];
                if i == 3 {
                    let mut l = line("Maintenance", 500.0);
                    l.font_size = size;
                    ls.push(l);
                }
                ls.push(line(&format!("Text on page {i}."), 400.0));
                ls
            });
            let (_, _, heads) = detect_with_heads(&pages, &[], &[], &[]);
            assert_eq!(heads[4].contains(&1), kept, "{size}pt");
        }
        // A slot whose names each appear once is each page's own title.
        let pages = doc(6, |i| {
            vec![
                head(
                    &format!(
                        "Topic number {}",
                        ["one", "two", "three", "four", "five", "six"][i]
                    ),
                    60.0,
                ),
                line("Text.", 400.0),
            ]
        });
        let (_, _, heads) = detect_with_heads(&pages, &[], &[], &[]);
        assert!(heads.iter().all(|h| h.is_empty()));
        // A title page's title, then one running title over the rest.
        let pages = doc(9, |i| {
            vec![
                head(
                    if i == 0 {
                        "Field Guide"
                    } else {
                        "Northern Region"
                    },
                    60.0,
                ),
                line(&format!("Text on page {i}."), 400.0),
            ]
        });
        let (_, _, heads) = detect_with_heads(&pages, &[], &[], &[]);
        assert!(heads.iter().all(|h| h.is_empty()));
    }

    /// A line set from `x0` to `x1` at `y`, at the test lines' size.
    fn line_at(text: &str, y: f64, x0: f64, x1: f64) -> Line {
        let mut l = line(text, y);
        l.bbox.x0 = x0;
        l.bbox.x1 = x1;
        l
    }

    /// A line set from `x0` to `x1` at `y`, at the size of a heading.
    fn head_at(text: &str, y: f64, x0: f64, x1: f64) -> Line {
        let mut l = head(text, y);
        l.bbox.x0 = x0;
        l.bbox.x1 = x1;
        l
    }

    #[test]
    fn a_running_head_mirrored_on_facing_pages_is_one_slot() {
        // Nine pages, the head flush left on even pages and flush right on
        // odd ones, its left edge moving with the name's length. Read as two
        // slots, each would keep a first copy of its own (`Assembly` on
        // pages 0 and 1).
        let name = |i: usize| match i {
            0..=2 => "Assembly",
            3..=5 => "Operation",
            _ => "Service",
        };
        let pages = doc(9, |i| {
            let width = 11.0 * name(i).len() as f64;
            let head = if i % 2 == 0 {
                head_at(name(i), 45.0, 40.0, 40.0 + width)
            } else {
                head_at(name(i), 45.0, 570.0 - width, 570.0)
            };
            vec![head, line(&format!("Text on page {i}."), 400.0)]
        });
        let (marks, _, heads) = detect_with_heads(&pages, &[], &[], &[]);
        for i in 0..9 {
            let first = [0, 3, 6].contains(&i);
            assert_eq!(heads[i].contains(&0), first, "page {i}");
            assert_eq!(marks[i].contains_key(&0), !first, "page {i}");
        }
    }

    #[test]
    fn a_section_named_once_over_its_second_page_runs() {
        // A report of two-page sections: each opens with its title, large,
        // and names itself once more over its second page, flush right in
        // the head band.
        let name = |i: usize| ["Northern Region", "Central Region", "Coastal Region"][i / 2];
        let pages = doc(6, |i| {
            let mut ls = vec![];
            if i % 2 == 0 {
                let mut l = line(name(i), 120.0);
                l.font_size = 24.0;
                ls.push(l);
            } else {
                ls.push(head_at(name(i), 40.0, 420.0, 560.0));
            }
            ls.push(line(&format!("Text on page {i}."), 400.0));
            ls
        });
        let (marks, _, heads) = detect_with_heads(&pages, &[], &[], &[]);
        assert!(heads.iter().all(|h| h.is_empty()), "{heads:?}");
        for i in [1, 3, 5] {
            assert_eq!(marks[i].get(&0), Some(&Furniture::Header), "page {i}");
        }
        // Without the heading before it, a name printed once is each
        // page's own.
        let pages = doc(6, |i| {
            let mut ls = vec![];
            if i % 2 == 1 {
                ls.push(head_at(name(i), 40.0, 420.0, 560.0));
            }
            ls.push(line(&format!("Text on page {i}."), 400.0));
            ls
        });
        let (marks, _, _) = detect_with_heads(&pages, &[], &[], &[]);
        assert!(marks.iter().all(|m| m.is_empty()));
    }

    #[test]
    fn a_bookmark_opens_its_section_before_its_running_head() {
        // Each section opens on an even page with its heading, large, and
        // bookmarked there. Its name runs at the body's size over the later
        // right-hand pages, flush right; the book's title runs over the
        // left-hand ones, flush left, at the same height and in the same
        // style. The name's first running copy is two pages after its
        // bookmark, and every copy runs: the section has its heading.
        let name = |i: usize| {
            if i < 6 {
                "Northern Region"
            } else {
                "Central Region"
            }
        };
        let pages = doc(12, |i| {
            let mut ls = vec![];
            if i % 6 == 0 {
                let mut l = line(name(i), 120.0);
                l.font_size = 24.0;
                ls.push(l);
            } else if i % 2 == 0 {
                ls.push(line_at(name(i), 30.0, 420.0, 560.0));
            } else {
                ls.push(line_at("Annual Review", 30.0, 40.0, 140.0));
            }
            ls.push(line(&format!("Text on page {i}."), 400.0));
            ls
        });
        let bookmarks = vec![(0, name(0).to_string()), (6, name(6).to_string())];
        let (marks, _, heads) = detect_with_heads(&pages, &bookmarks, &[], &[]);
        assert!(heads.iter().all(|h| h.is_empty()), "{heads:?}");
        for i in [2, 4, 8, 10] {
            assert!(marks[i].contains_key(&0), "page {i}");
        }
    }

    #[test]
    fn a_line_of_a_block_is_no_running_head() {
        // A margin label at the head of each page, beside its article's
        // number and the first line of its text: `Article` over six pages,
        // `Annex` over four. Each is a line of its row, not a head.
        let pages = doc(10, |i| {
            let label = if i < 6 { "Article" } else { "Annex" };
            vec![
                head_at(label, 100.0, 40.0, 110.0),
                head_at(&format!("{}", i / 2 + 1), 100.0, 150.0, 160.0),
                line_at(&format!("(a) the text of page {i}"), 102.0, 200.0, 560.0),
                line(&format!("Text on page {i}."), 400.0),
            ]
        });
        let (marks, _, heads) = detect_with_heads(&pages, &[], &[], &[]);
        assert!(heads.iter().all(|h| h.is_empty()), "{heads:?}");
        assert_eq!(marks, detect(&pages));
        // A display title of two lines, the first shared by a product's
        // two variants on facing pages: a title's line, not a running head.
        let pages = doc(6, |i| {
            let first = ["Lumen Series", "Aurora Series", "Nova Series"][i / 2];
            let second = ["Red", "Blue", "Green", "Black", "White", "Grey"][i];
            vec![
                head_at(first, 40.0, 60.0, 260.0),
                head_at(&format!("{second} Variant"), 58.0, 60.0, 240.0),
                line(&format!("Text on page {i}."), 400.0),
            ]
        });
        let (marks, _, heads) = detect_with_heads(&pages, &[], &[], &[]);
        assert!(heads.iter().all(|h| h.is_empty()), "{heads:?}");
        assert_eq!(marks, detect(&pages));
    }

    #[test]
    fn facing_pages_mirror_their_template() {
        // Two facing pages 600 wide. Page 2 sets its footer 7pt lower and
        // swaps its sides: the folio and the address change places, the
        // centred line stays centred. The banner at the head swaps sides
        // too: page 1's is the document's title.
        let footer = |folio: &str, left: bool, dy: f64| {
            let (fx, ax) = if left { (30.0, 450.0) } else { (550.0, 30.0) };
            vec![
                line_at(folio, 960.0 + dy, fx, fx + 20.0),
                line_at("Order line 0800 000 000", 962.0 + dy, 230.0, 370.0),
                line_at("orders@example.test", 961.0 + dy, ax, ax + 120.0),
            ]
        };
        let page = |left: bool| {
            let mut ls = if left {
                vec![
                    head_at("PART CATALOGUE", 40.0, 30.0, 230.0),
                    head_at("Steel Hardware", 70.0, 30.0, 170.0),
                ]
            } else {
                vec![
                    head_at("PART CATALOGUE", 40.0, 370.0, 570.0),
                    head_at("Steel Hardware", 70.0, 430.0, 570.0),
                ]
            };
            for k in 0..20 {
                ls.push(line(
                    &format!("Row {k} of page {left}"),
                    200.0 + 30.0 * k as f64,
                ));
            }
            ls
        };
        let mut p1 = page(true);
        p1.extend(footer("40", true, 0.0));
        let mut p2 = page(false);
        p2.extend(footer("41", false, 7.0));
        let pages = vec![(p1, 1000.0), (p2, 1000.0)];
        let (marks, _, _) = detect_with_heads(&pages, &[], &[600.0, 600.0], &[]);
        let n = pages[0].0.len();
        for (pi, li) in [
            (0, n - 3),
            (0, n - 2),
            (0, n - 1),
            (1, n - 3),
            (1, n - 2),
            (1, n - 1),
        ] {
            assert!(marks[pi].contains_key(&li), "page {pi} line {li}");
        }
        assert_eq!(marks[0].get(&0), None, "page 1 keeps its title");
        assert_eq!(marks[1].get(&0), Some(&Furniture::Header));
        assert_eq!(marks[1].get(&1), Some(&Furniture::Header));
        let folios: Vec<Option<String>> = detect_folio_places(&pages, &[600.0, 600.0])
            .into_iter()
            .map(|(f, _)| f)
            .collect();
        assert_eq!(folios, [Some("40".into()), Some("41".into())]);
        // Without the pages' widths, no mirror is seen.
        let (marks, _, _) = detect_with_heads(&pages, &[], &[], &[]);
        assert!(!marks[1].contains_key(&0));
    }

    #[test]
    fn a_page_templates_title_is_kept_once() {
        // Three pages of one table under its title, set below the header
        // band at the same place on each, over the table's column labels,
        // which repeat too.
        let pages = doc(3, |i| {
            let mut ls = vec![
                line_at("www.example.test", 40.0, 700.0, 800.0),
                head_at("Sizes of Steel Brackets", 300.0, 260.0, 560.0),
                line_at("Size Width Depth", 330.0, 60.0, 260.0),
            ];
            for k in 0..12 {
                ls.push(line(
                    &format!("{} 1{k} 2{k}", i * 12 + k),
                    360.0 + 20.0 * k as f64,
                ));
            }
            ls
        });
        let (marks, _, _) = detect_with_heads(&pages, &[], &[], &[]);
        assert_eq!(marks[0].get(&1), None, "the first page keeps its title");
        assert_eq!(marks[1].get(&1), Some(&Furniture::Header));
        assert_eq!(marks[2].get(&1), Some(&Furniture::Header));
        assert!(marks.iter().all(|m| !m.contains_key(&2)), "the labels stay");
    }

    #[test]
    fn a_running_row_merged_names_no_section() {
        // A head of three groups, set apart on every page but two, where
        // they run together as one line: the merged line is the running
        // line leaking, not a second name in the date's slot.
        let pages = doc(10, |i| {
            let mut ls = if i == 6 || i == 7 {
                vec![head_at(
                    "May 2, 2030 Quarterly Review DRAFT",
                    30.0,
                    40.0,
                    560.0,
                )]
            } else {
                vec![
                    head_at("May 2, 2030", 30.0, 40.0, 140.0),
                    head_at("Quarterly Review", 30.0, 250.0, 400.0),
                    head_at("DRAFT", 30.0, 510.0, 560.0),
                ]
            };
            ls.push(line(&format!("Text on page {i}."), 400.0));
            ls
        });
        let (_, _, heads) = detect_with_heads(&pages, &[], &[], &[]);
        assert!(heads.iter().all(|h| h.is_empty()), "{heads:?}");
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
    fn a_title_numbered_by_its_product_is_not_running() {
        // A catalogue's product pages each open with their product's title,
        // a size in it: the number changes, but does not count the pages.
        let sizes = [6, 10, 16, 24];
        let pages = doc(9, |i| {
            let mut ls = vec![
                line(&format!("Catalogue — {}", 19 + i), 20.0),
                line(&format!("Body text of page {i}"), 400.0),
            ];
            if (4..8).contains(&i) {
                ls.push(line(&format!("Kit H-BE {} Kits", sizes[i - 4]), 60.0));
            }
            ls
        });
        let (marks, folios) = detect_with_folios(&pages);
        for (i, m) in marks.iter().enumerate().take(8).skip(4) {
            assert_eq!(m.get(&2), None, "page {i}");
        }
        // The page number beside it still counts the pages.
        assert_eq!(marks[4].get(&0), Some(&Furniture::PageNumber));
        assert_eq!(folios[4].as_deref(), Some("23"));
    }

    #[test]
    fn a_heading_the_body_reaches_past_is_not_running() {
        // A guide's `FCC` heading, partway down a column near the top of the
        // last page of each language, under the end of the section before.
        // A header with nothing above it on those pages still runs.
        let pages = doc(9, |i| {
            let mut ls = vec![
                line("Install Guide", 20.0),
                line(&format!("Body text of page {i}"), 400.0),
            ];
            if i % 3 == 2 {
                let section = ["one", "two", "three"][i / 3];
                ls.push(line(&format!("the end of section {section} goes"), 48.0));
                ls.push(line(&format!("on and ends here, {section}"), 60.0));
                ls.push(line("FCC", 100.0));
            }
            ls
        });
        let marks = detect(&pages);
        assert_eq!(marks[2].get(&0), Some(&Furniture::Header));
        for i in [2, 5, 8] {
            assert_eq!(marks[i].get(&4), None, "page {i}");
        }
        // A header block's own lines over it are no paragraph: each is a
        // label of its own, and the block runs.
        let pages = doc(9, |i| {
            let mut ls = vec![line(&format!("Body text of page {i}"), 400.0)];
            if i % 3 == 0 {
                let title = ["INSTALLATION", "INSTALACIÓN", "INSTALLATION FR"][i / 3];
                ls.push(line(title, 60.0));
                ls.push(line("INSTRUCTIONS D’INSTALLATION", 100.0));
            }
            ls
        });
        let marks = detect(&pages);
        for i in [0, 3, 6] {
            assert_eq!(marks[i].get(&2), Some(&Furniture::Header), "page {i}");
        }
    }

    #[test]
    fn a_title_that_varies_from_page_to_page_runs_whole_after_its_first_copy() {
        // A sheet's title names the part the page is in, and some pages set
        // the short name alone: each name is kept at its first page and
        // runs after it, read as the whole line it is, so the short name is
        // not cut from the longer ones.
        let pages = doc(8, |i| {
            let title = match i {
                1 | 2 => "PUMP – DIESEL",
                3 | 4 => "PUMP – PETROL",
                _ => "PUMP",
            };
            vec![
                head(title, 60.0),
                line(&format!("Body text of page {i}"), 400.0),
            ]
        });
        let (marks, exact, heads) = detect_with_heads(&pages, &[], &[], &[]);
        for (i, kept) in [true, true, false, true, false, false, false, false]
            .into_iter()
            .enumerate()
        {
            assert_eq!(heads[i].contains(&0), kept, "page {i}");
            assert_eq!(marks[i].contains_key(&0), !kept, "page {i}");
            assert_eq!(exact[i].contains(&0), !kept, "page {i}");
        }
    }

    #[test]
    fn a_page_number_under_a_tables_figures_is_read_where_it_stands() {
        // An accessory list's table runs into the foot band with its counts,
        // and the page number stands under it: the bare numbers share one
        // signature, but not one place.
        let pages = doc(9, |i| {
            let mut ls = vec![line(&format!("Body text of page {i}"), 400.0)];
            for (k, y) in [880.0, 895.0, 910.0, 925.0].into_iter().enumerate() {
                ls.push(line(&format!("{}", (i * 7 + k * 3) % 40 + 1), y));
            }
            ls.push(line(&format!("{}", i + 1), 975.0));
            ls
        });
        let (marks, folios) = detect_with_folios(&pages);
        for (i, f) in folios.iter().enumerate() {
            assert_eq!(f.as_deref(), Some((i + 1).to_string().as_str()), "page {i}");
        }
        assert_eq!(marks[4].get(&5), Some(&Furniture::PageNumber));
        assert_eq!(marks[4].get(&1), None, "a table figure is no page number");
    }

    #[test]
    fn a_page_number_on_its_page_alone_is_the_folio() {
        // A data sheet numbers the one page after its unnumbered cover, and
        // a one-page sheet says `Page 1`.
        let pages = doc(3, |i| {
            let mut ls = vec![line(&format!("Body text of page {i}"), 400.0)];
            if i == 1 {
                ls.push(line("2", 970.0));
            }
            ls
        });
        let (marks, folios) = detect_with_folios(&pages);
        assert_eq!(folios, vec![None, Some("2".to_string()), None]);
        assert_eq!(marks[1].get(&1), Some(&Furniture::PageNumber));
        let pages = doc(1, |_| vec![line("Body", 400.0), line("Page 1", 970.0)]);
        let (marks, folios) = detect_with_folios(&pages);
        assert_eq!(folios, vec![Some("1".to_string())]);
        assert_eq!(marks[0].get(&1), Some(&Furniture::PageNumber));
        // Not a number that is not the page's place, nor one up in the body.
        let pages = doc(3, |i| {
            vec![
                line("Body", 400.0),
                line(if i == 1 { "5" } else { "" }, 970.0),
                line(if i == 2 { "3" } else { "" }, 500.0),
            ]
        });
        assert!(detect_with_folios(&pages).1.iter().all(Option::is_none));
        // Nor a table's `1` among the figures of its row, nor a date.
        let figure = |text: &str, x: f64| Line {
            bbox: BBox {
                x0: x,
                y0: 900.0,
                x1: x + 20.0,
                y1: 912.0,
            },
            ..line(text, 900.0)
        };
        let pages = doc(2, |i| match i {
            0 => vec![line("Body", 400.0), figure("1", 100.0), figure("27", 200.0)],
            _ => vec![line("Body", 400.0), line("1/24", 970.0)],
        });
        assert!(detect_with_folios(&pages).1.iter().all(Option::is_none));
    }

    #[test]
    fn a_clock_label_with_few_values_is_not_a_folio() {
        // Six clock times cycling at one position across many pages: the
        // digits vary, but not once per page. A flow sheet's row label says
        // something of its page, and is no furniture either.
        let times = ["00:00", "04:00", "08:00", "12:00", "16:00", "20:00"];
        let pages = doc(30, |i| vec![line(times[i % 6], 60.0)]);
        let (marks, folios) = detect_with_folios(&pages);
        assert_eq!(marks[0].get(&0), None);
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

    /// `line` moved by `(dx, dy)`.
    fn shifted(text: &str, x: f64, y: f64) -> Line {
        let mut l = line(text, y);
        l.bbox.x0 += x;
        l.bbox.x1 += x;
        l
    }

    #[test]
    fn a_footer_moved_with_page_twos_margins_is_running() {
        // Page 2 sets its margins 18pt further left: the footer's two lines
        // move together.
        let pages = doc(2, |i| {
            let dx = if i == 0 { 0.0 } else { -18.0 };
            vec![
                line(&format!("Body text unique to page {i}"), 400.0),
                shifted("Website: example.com", dx, 950.0 + 0.7 * i as f64),
                shifted("US: (800) 555-0100", dx, 962.0 + 0.7 * i as f64),
            ]
        });
        let r = detect(&pages);
        for m in &r {
            assert_eq!(m.get(&1), Some(&Furniture::Footer), "{r:?}");
            assert_eq!(m.get(&2), Some(&Furniture::Footer), "{r:?}");
        }
        // One line alone, moved, is not shown to be the template's.
        let pages = doc(2, |i| {
            let dx = if i == 0 { 0.0 } else { -18.0 };
            vec![
                line(&format!("Body text unique to page {i}"), 400.0),
                shifted("Website: example.com", dx, 950.0),
            ]
        });
        assert!(detect(&pages).iter().all(|m| m.is_empty()));
    }

    #[test]
    fn a_title_restated_as_page_twos_head_is_running_there_only() {
        // Page 1 sets the title under a masthead; page 2 opens with it.
        let pages = doc(2, |i| {
            if i == 0 {
                vec![
                    line("Masthead", 30.0),
                    line("Product Name Series", 80.0),
                    line("Body text unique to page 0", 400.0),
                ]
            } else {
                vec![
                    line("Product Name Series", 20.0),
                    line("Body text unique to page 1", 400.0),
                ]
            }
        });
        let r = detect(&pages);
        assert_eq!(r[1].get(&0), Some(&Furniture::Header), "{r:?}");
        assert!(r[0].is_empty(), "the title page keeps its title: {r:?}");
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
    fn a_short_documents_cover_need_not_print_its_page_number() {
        // Three pages, the first a cover with no number: the page left out
        // cannot restate a number that changes, and the number counts.
        let pages = doc(3, |i| match i {
            0 => vec![line("Annual Review", 300.0)],
            _ => vec![
                line(&format!("Body text of page {}", i + 1), 400.0),
                line(&format!("Page {} of 3", i + 1), 950.0),
            ],
        });
        let (marks, folios) = detect_with_folios(&pages);
        assert_eq!(marks[1].get(&1), Some(&Furniture::PageNumber));
        assert_eq!(marks[2].get(&1), Some(&Furniture::PageNumber));
        assert_eq!(folios[1].as_deref(), Some("2"));
        assert_eq!(folios[2].as_deref(), Some("3"));
        assert_eq!(folios[0], None);
        // A number that falls is no page's, cover or not.
        let pages = doc(3, |i| match i {
            0 => vec![line("Annual Review", 300.0)],
            _ => vec![line(&format!("Page {} of 3", 4 - i), 950.0)],
        });
        assert!(detect_with_folios(&pages).1.iter().all(Option::is_none));
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
    fn a_title_over_a_table_is_no_margin_note() {
        // A page title repeated on every page over a table whose cells are no
        // lines here: a running head at the top right, a footer's note at the
        // left and page number at the right, and the title between them.
        let pages = doc(3, |i| {
            let mut head = line("www.example.com", 30.0);
            head.bbox.x0 = 600.0;
            head.bbox.x1 = 745.0;
            let mut note = line("NOTE: ALL DIMENSIONS ARE IN INCHES", 960.0);
            note.bbox.x0 = 50.0;
            note.bbox.x1 = 250.0;
            let mut folio = line(&format!("Page {} of 3", i + 1), 960.0);
            folio.bbox.x0 = 680.0;
            folio.bbox.x1 = 745.0;
            let mut title = line("Dimensions of Socket Head Cap Screws", 300.0);
            title.bbox.x0 = 263.0;
            title.bbox.x1 = 540.0;
            vec![head, title, note, folio]
        });
        assert!(detect(&pages).iter().all(|m| m.get(&1).is_none()));
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
        // A document's code beside the page's number, and its date.
        assert!(identifies("369613g 1 04.08.24", Furniture::PageNumber));
        assert!(!identifies("p3", Furniture::PageNumber));
        assert!(!identifies("Page3 of 15", Furniture::PageNumber));
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
    // A block whose lines were stacked by hand, an address, keeps its line
    // breaks; a cell's text is one run.
    if ends_only && out.contains('\n') {
        return out
            .lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
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
