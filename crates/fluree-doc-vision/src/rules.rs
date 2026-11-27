//! The rules an image draws: what a table set as an image is ruled by.
//!
//! A table pasted into a document as a picture (a spreadsheet's screenshot,
//! a word processor's table saved as an image) keeps its ruling, but as
//! pixels: the page's drawing has no rule where the picture shows one, and
//! the words an OCR engine reads in it are all a table reader has. Read from
//! the pixels, each rule is a thin straight band darker than the image's
//! ground, with ground on either side of it along most of its length, and
//! the grid they make is the table's, cells, header and empty cells alike.
//!
//! The rules are read in the frame of the reading they go with
//! ([`WordReading::rules`]), and only what a table reader asks of a picture
//! is found here: long straight strokes. Whether they make a table is the
//! layout engine's to say (`fluree_doc_pdf`).

use serde::{Deserialize, Serialize};

use crate::{Coords, Layout, OcrWord, Pixels, WordReading};

/// Which way a rule runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// A rule an image draws: a thin straight band of ink.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrawnRule {
    /// `[x0, y0, x1, y1]`, y down, in the frame of the reading it goes with.
    #[serde(rename = "box")]
    pub bbox: [f64; 4],
    pub axis: Axis,
}

/// How much darker than the ground a pixel is to be ink, as a share of the
/// gap between the ground and the ink the image's text is set in. A
/// spreadsheet's grid is often drawn in light grey: on white, with its text
/// in black, at about a sixth of the text's contrast (rules at 212 on 255,
/// text at 7). A tenth takes such a grid with room to spare, whatever the
/// image's own contrast, while a tint laid behind a row (`#F2F2F2` on
/// white) stays under it at a twentieth. A tint dark enough to pass is a
/// band a row tall, which [`THICK`] turns away.
const CONTRAST: f64 = 0.1;

/// Least length of a rule, as a share of the image's width (a horizontal
/// one) or height (a vertical one). A table's rules run across it, and a
/// table set as an image is most of the image; a letter's stroke, even a
/// large one, is short beside it.
const LENGTH: f64 = 0.1;

/// Least length of a rule, in the heights of the image's words. A letter's
/// stroke is no taller than its word, and in an image a line or two high a
/// tenth of its height ([`LENGTH`]) is shorter than an `l`; a table's rule
/// runs at least a row, which is taller than its text.
const LETTERS: f64 = 1.5;

/// Thickest a rule may be, as a share of the height of the image's words.
/// A rule is a stroke, thinner than the letters it ranges with, even drawn
/// heavy; a band of shading, a filled bar, a dark panel is a row tall or
/// more.
const THICK: f64 = 0.35;

/// Least share of a rule's length along which the pixels next to it, on
/// each side, are ground and not ink. A drawn rule stands clear of what is
/// beside it; the edge of a dark area, a photograph's texture or a run of
/// bold type has ink alongside it nearly all the way.
const CLEAR: f64 = 0.6;

/// Where in the brightness of the pixels under the image's words its text's
/// ink is read: the darkest twentieth. A word's box is mostly the ground
/// between its strokes; the strokes are the darkest of it.
const INK_SHARE: f64 = 0.05;

/// The rules `image` draws, in the frame of `reading`, the words an engine
/// read in it. Nothing for an image with no words, which says nothing of
/// what ink its text is set in, or with no text darker than its ground
/// (light text on a dark ground is not read here), or a reading not in the
/// image's pixels.
pub fn drawn(image: &Pixels, reading: &WordReading) -> Vec<DrawnRule> {
    if reading.coords != Coords::Px || reading.words.is_empty() {
        return Vec::new();
    }
    let (w, h) = (image.width as usize, image.height as usize);
    // The reading's frame onto the image's pixels, where an engine read it
    // at another size.
    let sx = w as f64 / reading.width.unwrap_or(w as f64).max(1.0);
    let sy = h as f64 / reading.height.unwrap_or(h as f64).max(1.0);
    let luma = luma(image);

    let ground = quantile(&histogram(luma.iter().copied()), 0.5);
    let under: Vec<u8> = words_pixels(&luma, w, h, &reading.words, sx, sy);
    if under.is_empty() {
        return Vec::new();
    }
    let ink = quantile(&histogram(under.iter().copied()), INK_SHARE);
    let gap = f64::from(ground) - f64::from(ink);
    let cut = f64::from(ground) - CONTRAST * gap;
    if gap <= 0.0 || cut >= f64::from(ground) {
        return Vec::new();
    }
    let dark: Vec<bool> = luma.iter().map(|&v| f64::from(v) < cut).collect();

    let mut heights: Vec<f64> = reading
        .words
        .iter()
        .map(|wd| (wd.bbox[3] - wd.bbox[1]).abs() * sy)
        .filter(|h| *h > 0.0)
        .collect();
    heights.sort_by(f64::total_cmp);
    let text = heights.get(heights.len() / 2).copied().unwrap_or(0.0);
    let thick = (THICK * text).floor().max(1.0) as usize;

    let least = |extent: usize| (LENGTH * extent as f64).max(LETTERS * text).ceil() as usize;
    let mut out = Vec::new();
    let across = |x: usize, y: usize| dark[y * w + x];
    for (y0, y1, x0, x1) in bands(&across, w, h, least(w), thick) {
        out.push(DrawnRule {
            bbox: [
                x0 as f64 / sx,
                y0 as f64 / sy,
                x1 as f64 / sx,
                y1 as f64 / sy,
            ],
            axis: Axis::Horizontal,
        });
    }
    let down = |y: usize, x: usize| dark[y * w + x];
    for (x0, x1, y0, y1) in bands(&down, h, w, least(h), thick) {
        out.push(DrawnRule {
            bbox: [
                x0 as f64 / sx,
                y0 as f64 / sy,
                x1 as f64 / sx,
                y1 as f64 / sy,
            ],
            axis: Axis::Vertical,
        });
    }
    out
}

/// The image's brightness, 0 black to 255 white, a byte a pixel;
/// transparency is paper, white.
fn luma(image: &Pixels) -> Vec<u8> {
    let rgb =
        |r: u8, g: u8, b: u8| ((299 * r as u32 + 587 * g as u32 + 114 * b as u32) / 1000) as u8;
    match image.layout {
        Layout::Luma8 => image.data.clone(),
        Layout::Rgb8 => image
            .data
            .chunks_exact(3)
            .map(|p| rgb(p[0], p[1], p[2]))
            .collect(),
        Layout::Rgba8 => image
            .data
            .chunks_exact(4)
            .map(|p| {
                let a = p[3] as u16;
                let over = |c: u8| ((c as u16 * a + 255 * (255 - a)) / 255) as u8;
                rgb(over(p[0]), over(p[1]), over(p[2]))
            })
            .collect(),
    }
}

/// The brightness of the pixels inside the words' boxes, scaled onto the
/// image.
fn words_pixels(luma: &[u8], w: usize, h: usize, words: &[OcrWord], sx: f64, sy: f64) -> Vec<u8> {
    let mut out = Vec::new();
    for word in words {
        let [x0, y0, x1, y1] = word.bbox;
        let clamp = |v: f64, max: usize| (v.max(0.0) as usize).min(max);
        let (x0, x1) = (clamp(x0.min(x1) * sx, w), clamp(x0.max(x1) * sx, w));
        let (y0, y1) = (clamp(y0.min(y1) * sy, h), clamp(y0.max(y1) * sy, h));
        for y in y0..y1 {
            out.extend_from_slice(&luma[y * w + x0..y * w + x1]);
        }
    }
    out
}

fn histogram(values: impl Iterator<Item = u8>) -> [u64; 256] {
    let mut hist = [0u64; 256];
    for v in values {
        hist[v as usize] += 1;
    }
    hist
}

/// The brightness below which `share` of the values lie.
fn quantile(hist: &[u64; 256], share: f64) -> u8 {
    let total: u64 = hist.iter().sum();
    let want = (share * total as f64).ceil() as u64;
    let mut seen = 0u64;
    for (v, &n) in hist.iter().enumerate() {
        seen += n;
        if seen >= want.max(1) {
            return v as u8;
        }
    }
    255
}

/// The rules along one axis of a boolean image read as `ink(along, line)`:
/// `lines` lines of `len` pixels each. Runs of ink at least `min_len` long
/// are joined with the runs they overlap on the next line into bands; a
/// band at most `thick` lines deep, with ground beside it on both sides
/// ([`CLEAR`]), is a rule. Returned as `(line0, line1, at0, at1)`, ends
/// exclusive.
fn bands(
    ink: &dyn Fn(usize, usize) -> bool,
    len: usize,
    lines: usize,
    min_len: usize,
    thick: usize,
) -> Vec<(usize, usize, usize, usize)> {
    let min_len = min_len.max(2);
    // Open bands: (first line, last line + 1, start, end).
    let mut open: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut closed = Vec::new();
    for line in 0..=lines {
        let runs = if line < lines {
            runs(|at| ink(at, line), len, min_len)
        } else {
            Vec::new()
        };
        let mut used = vec![false; runs.len()];
        let mut next = Vec::with_capacity(open.len() + runs.len());
        for band in open.drain(..) {
            let hit = runs
                .iter()
                .enumerate()
                .position(|(i, &(a, b))| !used[i] && a < band.3 && b > band.2);
            match hit {
                Some(i) => {
                    used[i] = true;
                    let (a, b) = runs[i];
                    next.push((band.0, line + 1, band.2.min(a), band.3.max(b)));
                }
                None => closed.push(band),
            }
        }
        for (i, &(a, b)) in runs.iter().enumerate() {
            if !used[i] {
                next.push((line, line + 1, a, b));
            }
        }
        open = next;
    }
    let beside_clear = |line: Option<usize>, a: usize, b: usize| match line {
        None => true,
        Some(l) if l >= lines => true,
        Some(l) => {
            let inked = (a..b).filter(|&at| ink(at, l)).count();
            (inked as f64) <= (1.0 - CLEAR) * (b - a) as f64
        }
    };
    closed
        .into_iter()
        .filter(|&(l0, l1, a, b)| {
            l1 - l0 <= thick
                && beside_clear(l0.checked_sub(1), a, b)
                && beside_clear(Some(l1), a, b)
        })
        .collect()
}

/// The runs of ink along one line of `len` pixels at least `min_len` long,
/// as `(start, end)`, end exclusive.
fn runs(ink: impl Fn(usize) -> bool, len: usize, min_len: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for at in 0..=len {
        let on = at < len && ink(at);
        match (on, start) {
            (true, None) => start = Some(at),
            (false, Some(s)) => {
                if at - s >= min_len {
                    out.push((s, at));
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white image of `w` by `h` grey pixels, drawn on by `paint`.
    fn image(w: u32, h: u32, paint: impl Fn(u32, u32) -> u8) -> Pixels {
        let data = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| paint(x, y))
            .collect();
        Pixels::new(w, h, Layout::Luma8, data).unwrap()
    }

    /// A reading of `w` by `h` pixels whose words sit in `boxes`.
    fn reading(w: u32, h: u32, boxes: &[[f64; 4]]) -> WordReading {
        WordReading {
            coords: Coords::Px,
            width: Some(f64::from(w)),
            height: Some(f64::from(h)),
            engine: None,
            words: boxes
                .iter()
                .map(|&bbox| OcrWord {
                    text: "w".into(),
                    bbox,
                    angle: None,
                    conf: None,
                })
                .collect(),
            rules: Vec::new(),
        }
    }

    /// Text: a solid block of ink a word's size in each cell, so the words'
    /// boxes have their strokes in them.
    fn letters(x: u32, y: u32, cells: &[[u32; 4]]) -> bool {
        cells.iter().any(|c| {
            x >= c[0] + 4 && x < c[2] - 4 && y >= c[1] + 6 && y < c[3] - 6 && (x / 3) % 2 == 0
        })
    }

    /// A 3 by 3 grid of 100 by 30 pixel cells, ruled in `rule` grey, each
    /// cell holding a word in black.
    fn grid(rule: u8) -> (Pixels, WordReading) {
        let cells: Vec<[u32; 4]> = (0..3)
            .flat_map(|r| {
                (0..3).map(move |c| [10 + c * 100, 10 + r * 30, 110 + c * 100, 40 + r * 30])
            })
            .collect();
        let on_rule = |v: u32| (v - 10) % 100 == 0 || v == 310;
        let img = image(320, 110, |x, y| {
            let h = (10..=100).contains(&y) && (y - 10) % 30 == 0 && (10..=310).contains(&x);
            let v = (10..=100).contains(&y) && x >= 10 && x <= 310 && on_rule(x);
            if letters(x, y, &cells) {
                0
            } else if h || v {
                rule
            } else {
                255
            }
        });
        let boxes: Vec<[f64; 4]> = cells
            .iter()
            .map(|c| {
                [
                    f64::from(c[0] + 4),
                    f64::from(c[1] + 6),
                    f64::from(c[2] - 4),
                    f64::from(c[3] - 6),
                ]
            })
            .collect();
        (img, reading(320, 110, &boxes))
    }

    fn count(rules: &[DrawnRule], axis: Axis) -> usize {
        rules.iter().filter(|r| r.axis == axis).count()
    }

    #[test]
    fn a_grid_drawn_in_black_or_light_grey_is_found() {
        for grey in [0u8, 212] {
            let (img, read) = grid(grey);
            let rules = drawn(&img, &read);
            assert_eq!(count(&rules, Axis::Horizontal), 4, "grey {grey}");
            assert_eq!(count(&rules, Axis::Vertical), 4, "grey {grey}");
            let top = rules.iter().find(|r| r.axis == Axis::Horizontal).unwrap();
            assert_eq!(top.bbox, [10.0, 10.0, 311.0, 11.0]);
        }
    }

    #[test]
    fn a_band_of_shading_or_a_pale_tint_is_no_rule() {
        // Two rows of words, the second set on a grey band a row tall, the
        // first on a tint barely off white.
        let cells = [[10, 10, 300, 40], [10, 40, 300, 70]];
        let img = image(320, 90, |x, y| {
            if letters(x, y, &cells) {
                0
            } else if (40..70).contains(&y) {
                190
            } else if (10..40).contains(&y) {
                242
            } else {
                255
            }
        });
        let read = reading(
            320,
            90,
            &[[14.0, 16.0, 296.0, 34.0], [14.0, 46.0, 296.0, 64.0]],
        );
        assert!(drawn(&img, &read).is_empty());
    }

    #[test]
    fn a_textured_ground_is_no_rule() {
        // Speckle over half the pixels, darker than any rule's cut: no run of
        // it is long, and none stands clear.
        let mut seed = 7u32;
        let mut noise = Vec::new();
        for _ in 0..(320 * 90) {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            noise.push(if (seed >> 16) & 1 == 0 { 120 } else { 255 });
        }
        let cells = [[10, 30, 300, 60]];
        let img = image(320, 90, |x, y| {
            if letters(x, y, &cells) {
                0
            } else {
                noise[(y * 320 + x) as usize]
            }
        });
        let read = reading(320, 90, &[[14.0, 36.0, 296.0, 54.0]]);
        assert!(drawn(&img, &read).is_empty());
    }

    #[test]
    fn an_underline_is_one_rule_across_and_none_down() {
        // A word underlined: the line is a rule, the strokes over it are not.
        let cells = [[40, 20, 280, 50]];
        let img = image(320, 70, |x, y| {
            if letters(x, y, &cells) || (y == 46 && (44..276).contains(&x)) {
                0
            } else {
                255
            }
        });
        let read = reading(320, 70, &[[44.0, 26.0, 276.0, 47.0]]);
        let rules = drawn(&img, &read);
        assert_eq!(count(&rules, Axis::Horizontal), 1);
        assert_eq!(count(&rules, Axis::Vertical), 0);
    }

    #[test]
    fn rules_are_in_the_reading_frame() {
        // The engine read the image at half its size: the rules come back
        // at half too.
        let (img, mut read) = grid(0);
        read.width = Some(160.0);
        read.height = Some(55.0);
        for w in read.words.iter_mut() {
            w.bbox = w.bbox.map(|v| v / 2.0);
        }
        let rules = drawn(&img, &read);
        let top = rules.iter().find(|r| r.axis == Axis::Horizontal).unwrap();
        assert_eq!(top.bbox, [5.0, 5.0, 155.5, 5.5]);
    }

    #[test]
    fn no_words_no_rules() {
        let (img, mut read) = grid(0);
        read.words.clear();
        assert!(drawn(&img, &read).is_empty());
    }
}
