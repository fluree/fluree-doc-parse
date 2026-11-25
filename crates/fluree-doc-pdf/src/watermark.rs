//! Watermarks laid over a page's text.
//!
//! A watermark is drawn once per page, at the same place on every page, and
//! over (or under) the text: a large `DRAFT`, a diagonal `CONFIDENTIAL`, an
//! `Uncontrolled copy` stamp, a `Downloaded from …` line turned up the
//! margin. Left among the page's glyphs it does not stay a line of its own:
//! where it crosses a line of text it shares that line's baseline, and line
//! assembly sorts its letters in among the line's words, `imDpeller
//! flangeR impeller gasAket`. So it is taken out before lines are assembled,
//! as the run of glyphs it is drawn as.
//!
//! Repetition alone does not make one. Templated documents repeat real text
//! at the same place on every page (a spec table's labels, a box's heading,
//! the close of a standard sentence), and that is content. A run is a
//! watermark when it also lies on other text, or beside all of it in a
//! margin. Template text does neither: it sits in the page's text, between
//! other lines, never on them.
//!
//! A line OCR read is never one, as it is never furniture: a scan repeats
//! its content for its own reasons (a form copied per applicant, its note
//! down the margin of every copy), and a watermark printed on the paper is
//! read with the rest of what the page says. Its words still count as the
//! text a watermark drawn over the scan lies on.

use crate::extract::Page;
use crate::furniture::EDGE_BAND;
use crate::geom::BBox;
use crate::glyph::Glyph;
use std::collections::HashMap;

/// Fewest pages, and least share of them, a watermark must stand on; as for
/// the rest of the page furniture.
const MIN_PAGES: usize = 3;
const MIN_PAGE_FRACTION: f64 = 0.30;

/// Agreement of a run's centre across pages, as a share of the page's size.
const POSITION_TOLERANCE: f64 = 0.02;

/// Fewest letters or digits in a watermark: a figure's callout (`3`) or a
/// bullet repeats at a fixed place too, and is the figure's.
const MIN_CHARS: usize = 3;

/// Farthest one glyph's origin may be from the last, in sizes, for both to
/// be one run as drawn.
const RUN_STEP: f64 = 1.5;

/// Fewest glyphs of other text a run must lie on to be laid over it.
const MIN_COVERED: usize = 2;

/// A run of glyphs drawn one after another along one line.
struct Run {
    glyphs: Vec<usize>,
    text: String,
    bbox: BBox,
    rotation: i32,
    size: f32,
}

/// The page's glyphs in the runs they were drawn in: consecutive, of one
/// size and turn, each a step from the last.
fn runs(glyphs: &[Glyph]) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    let mut last: Option<usize> = None;
    for (i, g) in glyphs.iter().enumerate() {
        let Some(b) = g.bbox.filter(|_| !g.text.trim().is_empty()) else {
            // A space joins the run it falls in.
            if let (Some(r), Some(_)) = (out.last_mut(), last) {
                r.glyphs.push(i);
                r.text.push(' ');
            }
            continue;
        };
        let joins = last.is_some_and(|p| {
            let p = &glyphs[p];
            let size = p.font_size.max(g.font_size).max(1.0);
            p.rotation_bucket() == g.rotation_bucket()
                && (p.font_size - g.font_size).abs() <= 0.15 * size
                && (g.origin.0 - p.origin.0).hypot(g.origin.1 - p.origin.1)
                    <= RUN_STEP * size as f64
        });
        match out.last_mut() {
            Some(r) if joins => {
                r.glyphs.push(i);
                r.text.push_str(&g.text);
                r.bbox = r.bbox.union(&b);
            }
            _ => out.push(Run {
                glyphs: vec![i],
                text: g.text.clone(),
                bbox: b,
                rotation: g.rotation_bucket(),
                size: g.font_size,
            }),
        }
        last = Some(i);
    }
    out
}

/// The text the run lies on: glyphs not its own whose ink its glyphs'
/// ink crosses, in drawing order. Empty where it crosses too few to be
/// laid over text.
fn covered(glyphs: &[Glyph], run: &Run) -> String {
    let own: Vec<BBox> = run.glyphs.iter().filter_map(|&i| glyphs[i].bbox).collect();
    let mut mine = run.glyphs.clone();
    mine.sort_unstable();
    let under: Vec<&str> = glyphs
        .iter()
        .enumerate()
        .filter(|(i, g)| {
            mine.binary_search(i).is_err()
                && !g.text.trim().is_empty()
                && g.bbox.is_some_and(|b| {
                    b.intersects(&run.bbox) && own.iter().any(|o| overlap(o, &b) > 0.1)
                })
        })
        .map(|(_, g)| g.text.as_str())
        .collect();
    if under.len() < MIN_COVERED {
        String::new()
    } else {
        under.concat()
    }
}

/// The run stands apart from the text it lies on, as a layer of its own:
/// turned, much larger than the page's text, or drawn before or after all
/// of it. A label of a drawing that overlaps its neighbour is set like
/// them and drawn among them.
fn apart(glyphs: &[Glyph], run: &Run) -> bool {
    if run.rotation != 0 {
        return true;
    }
    let mut sizes: Vec<f32> = glyphs
        .iter()
        .filter(|g| g.bbox.is_some() && !g.text.trim().is_empty())
        .map(|g| g.font_size)
        .collect();
    sizes.sort_by(f32::total_cmp);
    let body = sizes.get(sizes.len() / 2).copied().unwrap_or(0.0);
    if run.size >= LARGE * body {
        return true;
    }
    let (first, last) = (run.glyphs[0], *run.glyphs.last().unwrap());
    let inked = |g: &Glyph| g.bbox.is_some() && !g.text.trim().is_empty();
    let before = glyphs[..first].iter().any(inked);
    let after = glyphs[last + 1..].iter().any(inked);
    !before || !after
}

/// How many times the page's median text size a watermark set large is.
const LARGE: f32 = 2.0;

/// The run stands beside all the page's other text: no glyph of it shares
/// any of the run's width, as a note in a side margin does.
fn beside(glyphs: &[Glyph], run: &Run) -> bool {
    let mut mine = run.glyphs.clone();
    mine.sort_unstable();
    !glyphs.iter().enumerate().any(|(i, g)| {
        mine.binary_search(&i).is_err()
            && !g.text.trim().is_empty()
            && g.bbox
                .is_some_and(|b| b.x1 > run.bbox.x0 && b.x0 < run.bbox.x1)
    })
}

/// Intersection over the smaller box's area.
fn overlap(a: &BBox, b: &BBox) -> f64 {
    let w = a.x1.min(b.x1) - a.x0.max(b.x0);
    let h = a.y1.min(b.y1) - a.y0.max(b.y0);
    if w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    let smaller = (a.width() * a.height()).min(b.width() * b.height());
    if smaller <= 0.0 {
        0.0
    } else {
        w * h / smaller
    }
}

/// Take out of each page the glyphs of a watermark laid over its text.
/// Returns the watermarks' text, once each.
pub fn strip_overlaid(pages: &mut [Page]) -> Vec<String> {
    let n = pages.len();
    if n < MIN_PAGES {
        return Vec::new();
    }
    let needed = ((n as f64 * MIN_PAGE_FRACTION).ceil() as usize).max(MIN_PAGES);
    let page_runs: Vec<Vec<Run>> = pages.iter().map(|p| runs(&p.glyphs)).collect();
    // Runs by what they say, at what size and turn.
    type Key = (String, i32, i64);
    let mut groups: HashMap<Key, Vec<(usize, usize)>> = HashMap::new();
    for (pi, rs) in page_runs.iter().enumerate() {
        let page = &pages[pi];
        for (ri, r) in rs.iter().enumerate() {
            if r.glyphs.iter().any(|&i| page.is_ocr(&page.glyphs[i])) {
                continue;
            }
            let text = r.text.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.chars().filter(|c| c.is_alphanumeric()).count() < MIN_CHARS {
                continue;
            }
            let key = (text, r.rotation, (r.size as f64 * 2.0).round() as i64);
            groups.entry(key).or_default().push((pi, ri));
        }
    }
    let mut keys: Vec<Key> = groups.keys().cloned().collect();
    keys.sort();
    let mut strike: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut found = Vec::new();
    for key in keys {
        let occ = &groups[&key];
        let mut seen: Vec<usize> = occ.iter().map(|o| o.0).collect();
        seen.sort_unstable();
        let total = seen.len();
        seen.dedup();
        // Once a page, on enough of them.
        if seen.len() < needed || seen.len() != total {
            continue;
        }
        let centre = |&(pi, ri): &(usize, usize)| {
            let (p, b) = (&pages[pi], &page_runs[pi][ri].bbox);
            (
                (b.x0 + b.x1) / 2.0 / p.width.max(1.0),
                (b.y0 + b.y1) / 2.0 / p.height.max(1.0),
            )
        };
        let mut xs: Vec<f64> = occ.iter().map(|o| centre(o).0).collect();
        let mut ys: Vec<f64> = occ.iter().map(|o| centre(o).1).collect();
        xs.sort_by(f64::total_cmp);
        ys.sort_by(f64::total_cmp);
        let (mx, my) = (xs[xs.len() / 2], ys[ys.len() / 2]);
        // Away from the top and bottom, where headers and footers are read.
        if !(EDGE_BAND..=1.0 - EDGE_BAND).contains(&my) {
            continue;
        }
        let stable: Vec<(usize, usize)> = occ
            .iter()
            .copied()
            .filter(|o| {
                let (x, y) = centre(o);
                (x - mx).abs() < POSITION_TOLERANCE && (y - my).abs() < POSITION_TOLERANCE
            })
            .collect();
        // Laid over the page's text, which changes beneath it from page to
        // page; or beside all of it. A figure the document repeats (an
        // install guide's, once per language) overlaps its own labels the
        // same way on every page, and is the figure's.
        let mut beneath: Vec<String> = Vec::new();
        let mut marked = 0usize;
        for &(pi, ri) in &stable {
            let (g, r) = (&pages[pi].glyphs, &page_runs[pi][ri]);
            if beside(g, r) {
                marked += 1;
                continue;
            }
            let under = covered(g, r);
            if !under.is_empty() && apart(g, r) {
                marked += 1;
                beneath.push(under);
            }
        }
        beneath.sort_unstable();
        beneath.dedup();
        let varies = beneath.len() >= 2 || marked > beneath.len();
        if stable.len() < needed || marked < needed || !varies {
            continue;
        }
        for (pi, ri) in stable {
            strike[pi].extend(&page_runs[pi][ri].glyphs);
        }
        found.push(key.0);
    }
    for (p, gone) in pages.iter_mut().zip(strike.iter_mut()) {
        if gone.is_empty() {
            continue;
        }
        gone.sort_unstable();
        gone.dedup();
        let mut k = 0usize;
        let mut i = 0usize;
        p.glyphs.retain(|_| {
            let drop = gone.get(k) == Some(&i);
            if drop {
                k += 1;
            }
            i += 1;
            !drop
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text` set from `x` on baseline `y` at `size`, one glyph a
    /// character, spaces outline-less.
    fn set(text: &str, x: f64, y: f64, size: f32) -> Vec<Glyph> {
        let step = size as f64 * 0.55;
        text.chars()
            .enumerate()
            .map(|(i, ch)| {
                let cx = x + step * i as f64;
                Glyph {
                    text: ch.to_string(),
                    bbox: (ch != ' ').then_some(BBox {
                        x0: cx,
                        y0: y - size as f64 * 0.75,
                        x1: cx + step * 0.9,
                        y1: y,
                    }),
                    page: 0,
                    origin: (cx, y),
                    rotation_deg: 0.0,
                    font_size: size,
                    weight: None,
                    advance: Some(step),
                    draw_index: 0,
                }
            })
            .collect()
    }

    fn page(index: usize, glyphs: Vec<Glyph>) -> Page {
        Page {
            index,
            glyphs,
            images: Vec::new(),
            rules: Vec::new(),
            routing: None,
            fills: Vec::new(),
            dots: Vec::new(),
            traces: Vec::new(),
            ticks: Vec::new(),
            width: 612.0,
            height: 792.0,
            rotation: 0,
            ocr: None,
            warnings: Vec::new(),
        }
    }

    /// Five pages of body lines that differ from page to page, with
    /// `extra` drawn first on each, and `last` drawn after the body.
    fn doc(first: impl Fn(usize) -> Vec<Glyph>, last: impl Fn(usize) -> Vec<Glyph>) -> Vec<Page> {
        (0..5)
            .map(|p| {
                let mut g = first(p);
                for (l, y) in [380.0, 395.0, 410.0, 425.0].iter().enumerate() {
                    g.extend(set(
                        &format!("body line {l} of page {p} says something new"),
                        72.0,
                        *y,
                        11.0,
                    ));
                }
                g.extend(last(p));
                page(p, g)
            })
            .collect()
    }

    /// `pages` as OCR read them: every glyph added by the reading, after
    /// the ones the pages drew.
    fn read(pages: &mut [Page], drawn: usize) {
        for p in pages {
            for (i, g) in p.glyphs.iter_mut().enumerate() {
                g.draw_index = i;
            }
            p.ocr = Some(crate::extract::OcrRead {
                first_glyph: drawn,
                whole: true,
                regions: Vec::new(),
            });
        }
    }

    fn text(p: &Page) -> String {
        p.glyphs.iter().map(|g| g.text.as_str()).collect()
    }

    #[test]
    fn a_draft_laid_over_the_text_is_taken_out() {
        let mut pages = doc(|_| set("DRAFT", 150.0, 420.0, 80.0), |_| Vec::new());
        assert_eq!(strip_overlaid(&mut pages), ["DRAFT"]);
        assert!(!text(&pages[2]).contains("DRAFT"));
        assert!(text(&pages[2]).contains("body line 3 of page 2"));
    }

    #[test]
    fn a_stamp_drawn_after_the_text_and_a_margin_note_are_taken_out() {
        let mut pages = doc(
            |_| set("For internal use only", 540.0, 300.0, 6.0),
            |_| set("Uncontrolled copy", 150.0, 412.0, 9.0),
        );
        let mut found = strip_overlaid(&mut pages);
        found.sort();
        assert_eq!(found, ["For internal use only", "Uncontrolled copy"]);
    }

    #[test]
    fn a_margin_note_ocr_read_stays() {
        let mut pages = doc(
            |_| set("For internal use only", 540.0, 300.0, 6.0),
            |_| Vec::new(),
        );
        read(&mut pages, 0);
        assert!(strip_overlaid(&mut pages).is_empty());
        assert!(text(&pages[3]).contains("For internal use only"));
    }

    #[test]
    fn a_draft_drawn_over_a_scan_s_reading_is_taken_out() {
        // The page draws the stamp; OCR reads the scan beneath it.
        let mut pages = doc(|_| set("DRAFT", 150.0, 420.0, 80.0), |_| Vec::new());
        read(&mut pages, "DRAFT".len());
        assert_eq!(strip_overlaid(&mut pages), ["DRAFT"]);
        assert!(text(&pages[2]).contains("body line 3 of page 2"));
    }

    #[test]
    fn repeated_text_set_among_the_page_text_stays() {
        // A template's box heading between the body's lines, and a figure's
        // label overlapping its neighbour the same way on every page, drawn
        // among the page's text at its size.
        let mut pages = doc(|_| set("Technical data", 72.0, 360.0, 11.0), |_| Vec::new());
        for p in pages.iter_mut() {
            let mut mid = set("Center", 300.0, 500.0, 11.0);
            mid.extend(set("Grommet", 310.0, 502.0, 11.0));
            let tail = p.glyphs.split_off(20);
            p.glyphs.extend(mid);
            p.glyphs.extend(tail);
        }
        assert!(strip_overlaid(&mut pages).is_empty());
        assert!(text(&pages[1]).contains("Technical data"));
        assert!(text(&pages[1]).contains("Center"));
    }
}
