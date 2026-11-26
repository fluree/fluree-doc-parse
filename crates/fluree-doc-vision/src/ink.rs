//! A word's type measured in the image it was read from, guided by its
//! text.
//!
//! A detector box is the ink and a margin, and the margin varies: with the
//! letters under it, the room around it, the pixels its text spans. Sized by
//! its box, a line read about 5% either side of its type, while a heading
//! set a fifth larger than its body has 4% to spare over the ratio a heading
//! needs. So the engine measures the ink itself, column by column of the box
//! set level, and the text says what the ink should show: which letters rise
//! to a capital's height or an ascender's, which stop at the x-height, which
//! drop below the baseline, which carry a dot or an accent.
//!
//! [`measure`] gives, in pixels of the image read: the height of a capital
//! opening the word, from its foot ([`Ink::first_cap`]); the height of the
//! word's highest line and whether it is the capitals' or the ascenders'
//! ([`Ink::top`], [`Ink::top_line`]); and its x-height ([`Ink::x_height`]).
//! The three are in a face's own proportions, which a page calibrates from
//! its words (see `fluree-doc-pdf`'s OCR injection); none is a type size on
//! its own.
//!
//! What the measure is robust to, measured on rasterised born-digital pages
//! read by the built-in engine at 150 to 300 dpi: text and paper of any
//! colour, light text on dark ground (the background is the box's border, ink
//! its distance from it in colour, over the word's own contrast), a gradient
//! or a photo under the text, specks of noise (an inked pixel with at most one
//! inked neighbour is not ink), JPEG artefacts, a fax's threshold, blur, a page turned
//! a degree or two (the box is sampled level, by the engine's angle), an
//! underline, a strike or a box drawn round the word (a row inked across
//! most of the word, or a column across nearly the box's height, is a rule
//! and taken out), a raised note mark after a word, and the ink of the lines
//! above and below where it reaches into the box (it touches the box's edge).
//! Italic reads a little wide: a slanted capital's columns may run into the
//! next letter's.
//!
//! Latin script: a word whose letters say nothing of these lines (another
//! script, punctuation alone) has no measure, and is sized by its box.

use crate::{Layout, Pixels};
use serde::{Deserialize, Serialize};

/// The lines of a word's ink, in pixels of the image read: heights above the
/// baseline.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Ink {
    /// A capital opening the word, from its foot to its top: the capitals'
    /// height, measured on one letter standing apart from the next.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_cap: Option<f64>,
    /// The highest line the word's letters reach, which [`Ink::top_line`]
    /// names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_line: Option<TopLine>,
    /// The x-height: where the letters without ascenders stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_height: Option<f64>,
}

/// Which line a word's highest letters reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TopLine {
    /// Capitals and figures, and nothing taller.
    Cap,
    /// An ascender's (`b d f h k l`), a little over a capital's in most
    /// faces.
    Ascender,
}

/// Letters that rise to about a capital's height (`t` a little less).
fn rises(c: char) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || "bdfhklt".contains(c)
}

/// Letters with a descender.
fn drops(c: char) -> bool {
    "gjpqy".contains(c)
}

/// Letters that stop at the x-height, the dotted ones counted by their
/// stems.
fn small(c: char) -> bool {
    "acemnorsuvwxzij".contains(c) || drops(c)
}

/// The ascenders, whose line is a little over the capitals'.
fn ascends(c: char) -> bool {
    "bdfhkl".contains(c)
}

/// A letter with a dot or an accent over it.
fn dotted(c: char) -> bool {
    matches!(c, 'i' | 'j') || (!c.is_ascii() && c.is_alphabetic())
}

/// Which line the highest of `text`'s letters reach, if any.
pub fn top_line(text: &str) -> Option<TopLine> {
    if text.chars().any(ascends) {
        Some(TopLine::Ascender)
    } else if text
        .chars()
        .any(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        Some(TopLine::Cap)
    } else {
        None
    }
}

/// Ink fully on, as a share of the word's contrast.
const ON: f64 = 0.5;

/// Fewest columns that make a line: about a stem's width. One column alone,
/// a speck or a serif's corner, is not one.
const STEM_COLUMNS: usize = 3;

/// A word's crop, set level, as ink levels 0-1, row by row.
struct Crop {
    w: usize,
    h: usize,
    ink: Vec<f64>,
}

impl Crop {
    fn at(&self, x: usize, y: usize) -> f64 {
        self.ink[y * self.w + x]
    }
}

/// Measure the word `text` boxed at `bbox` (`[x0, y0, x1, y1]`, in pixels
/// of `image`, the box set upright about its center) turned `angle` degrees
/// counter-clockwise.
pub fn measure(image: &Pixels, bbox: [f64; 4], angle: f64, text: &str) -> Option<Ink> {
    let letters: Vec<char> = text.chars().filter(|c| c.is_alphanumeric()).collect();
    if letters.is_empty() || !bbox.iter().all(|v| v.is_finite()) {
        return None;
    }
    let crop = sample(image, bbox, angle)?;
    let crop = without_rules(crop);
    let has_dot = text.chars().any(dotted);
    // Each column's ink: its top and its bottom, a fraction of a pixel.
    let mut xs = Vec::new();
    let mut tops = Vec::new();
    let mut bots = Vec::new();
    for x in 0..crop.w {
        if let Some((t, b)) = column(&crop, x, has_dot) {
            xs.push(x);
            tops.push(t);
            bots.push(b);
        }
    }
    if xs.len() < 4 {
        return None;
    }
    let has_rise = letters.iter().any(|&c| rises(c));
    let has_small = letters.iter().any(|&c| small(c));
    let drop_share = letters.iter().filter(|&&c| drops(c)).count() as f64 / letters.len() as f64;
    // The baseline: the bottoms of the columns that do not drop, the larger
    // share of them.
    let base = quantile(&bots, ((1.0 - drop_share) * 0.5).max(0.05));
    let mid = quantile(&tops, 0.5);
    let h0 = (base - mid).max(1.0);
    let mut ink = Ink {
        first_cap: None,
        top: None,
        top_line: None,
        x_height: None,
    };
    if has_rise {
        // The highest level holding a stem's worth of columns.
        let mut sorted = tops.clone();
        sorted.sort_by(f64::total_cmp);
        let window = 0.1 * h0;
        for &t in &sorted {
            let near: Vec<f64> = tops
                .iter()
                .copied()
                .filter(|&v| v >= t && v <= t + window)
                .collect();
            if near.len() >= STEM_COLUMNS {
                ink.top = Some(base - quantile(&near, 0.5));
                ink.top_line = top_line(text);
                break;
            }
        }
    }
    if has_small {
        // The x-height line: where most columns start, under the top line.
        let floor = ink.top.map(|top| (base - top) + 0.15 * top);
        let low: Vec<f64> = tops
            .iter()
            .copied()
            .filter(|&t| floor.is_none_or(|f| t > f))
            .collect();
        if !low.is_empty() {
            ink.x_height = Some(base - quantile(&low, 0.5));
        }
    }
    // A capital opening the word: its first run of inked columns, a letter
    // standing apart from the next, from its own foot to its top.
    let first = text.chars().next()?;
    if first.is_ascii_uppercase() && !matches!(first, 'J' | 'Q') {
        let mut end = 1;
        while end < xs.len() && xs[end] == xs[end - 1] + 1 {
            end += 1;
        }
        if end >= 2 {
            let top = tops[..end].iter().copied().fold(f64::INFINITY, f64::min);
            let foot = bots[..end]
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            ink.first_cap = Some(foot - top);
        }
    }
    let usable = |v: Option<f64>| v.filter(|h| h.is_finite() && *h > 0.0);
    // The opening capital agrees with the word's other lines, or it was not
    // one letter: the end of a `T`'s bar set apart by a faint column, a
    // capital run into its neighbour. A capital stands within a tenth or so
    // of the word's highest line, and 1.2 to 2 x-heights.
    ink.first_cap =
        usable(ink.first_cap).filter(|&c| match (usable(ink.top), usable(ink.x_height)) {
            (Some(top), _) => (0.8..=1.2).contains(&(c / top)),
            (None, Some(x)) => (1.2..=2.0).contains(&(c / x)),
            (None, None) => true,
        });
    ink.top = usable(ink.top);
    ink.x_height = usable(ink.x_height);
    if ink.top.is_none() {
        ink.top_line = None;
    }
    (ink.first_cap.is_some() || ink.top.is_some() || ink.x_height.is_some()).then_some(ink)
}

/// The box's region set level, a sixth of its height more above and below,
/// sampled bilinearly, as ink levels: the background is the commonest colour
/// of the crop's border, ink a pixel's distance from it in colour over the
/// crop's own contrast, its specks of noise taken out ([`despeckled`]).
fn sample(image: &Pixels, bbox: [f64; 4], angle: f64) -> Option<Crop> {
    let [x0, y0, x1, y1] = bbox;
    let (bw, bh) = (x1 - x0, y1 - y0);
    if !(bw >= 2.0 && bh >= 2.0) || bw * bh > 4.0e7 {
        return None;
    }
    let pad = (bh / 6.0).floor().max(1.0);
    let (w, h) = (bw.round() as usize, (bh + 2.0 * pad).round() as usize);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (sin, cos) = angle.to_radians().sin_cos();
    let (iw, ih) = (image.width as usize, image.height as usize);
    if iw < 2 || ih < 2 {
        return None;
    }
    let px = |x: usize, y: usize| rgb_at(image, y * iw + x);
    let mut colour = vec![[255.0f64; 3]; w * h];
    for v in 0..h {
        for u in 0..w {
            // The crop's pixel center, from the box's center, turned onto
            // the image.
            let dx = u as f64 + 0.5 - bw / 2.0;
            let dy = v as f64 + 0.5 - (bh / 2.0 + pad);
            let x = cx + dx * cos + dy * sin - 0.5;
            let y = cy - dx * sin + dy * cos - 0.5;
            if !(x >= 0.0 && y >= 0.0 && x < (iw - 1) as f64 && y < (ih - 1) as f64) {
                continue;
            }
            let (xi, yi) = (x.floor() as usize, y.floor() as usize);
            let (fx, fy) = (x - xi as f64, y - yi as f64);
            let (a, b, c, d) = (
                px(xi, yi),
                px(xi + 1, yi),
                px(xi, yi + 1),
                px(xi + 1, yi + 1),
            );
            let mut o = [0.0; 3];
            for k in 0..3 {
                o[k] = a[k] * (1.0 - fx) * (1.0 - fy)
                    + b[k] * fx * (1.0 - fy)
                    + c[k] * (1.0 - fx) * fy
                    + d[k] * fx * fy;
            }
            colour[v * w + u] = o;
        }
    }
    Some(despeckled(levels(&colour, w, h)))
}

/// Pixel `i` of `image` as RGB, composited over white as
/// [`Pixels::to_rgb`] does.
fn rgb_at(image: &Pixels, i: usize) -> [f64; 3] {
    let d = &image.data;
    match image.layout {
        Layout::Luma8 => [f64::from(d[i]); 3],
        Layout::Rgb8 => [
            f64::from(d[3 * i]),
            f64::from(d[3 * i + 1]),
            f64::from(d[3 * i + 2]),
        ],
        Layout::Rgba8 => {
            let a = f64::from(d[4 * i + 3]) / 255.0;
            let over = |c: u8| f64::from(c) * a + 255.0 * (1.0 - a);
            [over(d[4 * i]), over(d[4 * i + 1]), over(d[4 * i + 2])]
        }
    }
}

/// Ink levels of a crop's colours: distance from the border's commonest
/// colour, over the far end of that distance.
fn levels(colour: &[[f64; 3]], w: usize, h: usize) -> Crop {
    let mut border: Vec<[f64; 3]> = Vec::with_capacity(2 * (w + h));
    for u in 0..w {
        border.push(colour[u]);
        border.push(colour[(h - 1) * w + u]);
    }
    for v in 0..h {
        border.push(colour[v * w]);
        border.push(colour[v * w + w - 1]);
    }
    let bg: Vec<f64> = (0..3)
        .map(|k| quantile(&border.iter().map(|c| c[k]).collect::<Vec<_>>(), 0.5))
        .collect();
    let dist: Vec<f64> = colour
        .iter()
        .map(|c| ((c[0] - bg[0]).powi(2) + (c[1] - bg[1]).powi(2) + (c[2] - bg[2]).powi(2)).sqrt())
        .collect();
    let far = quantile(&dist, 0.99);
    let ink = if far < 1e-6 {
        vec![0.0; dist.len()]
    } else {
        dist.iter().map(|d| (d / far).clamp(0.0, 1.0)).collect()
    };
    Crop { w, h, ink }
}

/// The crop with its specks of noise taken out: inked pixels with at most
/// one inked neighbour. A stroke's pixels all have more, so no letter loses
/// any (a median over the crop rounds a stem's corners off, and with them a
/// pixel of its height).
fn despeckled(mut c: Crop) -> Crop {
    let on: Vec<bool> = c.ink.iter().map(|&v| v > ON).collect();
    let at = |x: isize, y: isize| {
        x >= 0
            && y >= 0
            && (x as usize) < c.w
            && (y as usize) < c.h
            && on[y as usize * c.w + x as usize]
    };
    let mut specks = Vec::new();
    for y in 0..c.h as isize {
        for x in 0..c.w as isize {
            if !at(x, y) {
                continue;
            }
            let neighbours = (-1..=1)
                .flat_map(|dy| (-1..=1).map(move |dx| (dx, dy)))
                .filter(|&(dx, dy)| (dx, dy) != (0, 0) && at(x + dx, y + dy))
                .count();
            if neighbours <= 1 {
                specks.push(y as usize * c.w + x as usize);
            }
        }
    }
    for i in specks {
        c.ink[i] = 0.0;
    }
    c
}

/// The crop with its rules taken out: a row inked in one stretch over most
/// of the word's width (an underline, a strike, a box's edge), with the two
/// rows either side a rule drawn between rows spreads over, and a column
/// inked in one stretch over nearly the crop's height (a box's side). No
/// letter runs that far either way.
fn without_rules(mut c: Crop) -> Crop {
    let on: Vec<bool> = c.ink.iter().map(|&v| v > ON).collect();
    let inked_cols: Vec<usize> = (0..c.w)
        .filter(|&x| (0..c.h).any(|y| on[y * c.w + x]))
        .collect();
    let (Some(&first), Some(&last)) = (inked_cols.first(), inked_cols.last()) else {
        return c;
    };
    let width = (last - first + 1) as f64;
    let h = c.h as f64;
    let longest = |cells: &mut dyn Iterator<Item = bool>| {
        let (mut best, mut cur) = (0usize, 0usize);
        for v in cells {
            cur = if v { cur + 1 } else { 0 };
            best = best.max(cur);
        }
        best as f64
    };
    let ruled: Vec<usize> = (0..c.h)
        .filter(|&y| {
            let run = longest(&mut (0..c.w).map(|x| on[y * c.w + x]));
            run >= 0.8 * width || run >= (0.6 * width).max(1.5 * h)
        })
        .collect();
    for y in ruled {
        for yy in y.saturating_sub(2)..(y + 3).min(c.h) {
            for x in 0..c.w {
                c.ink[yy * c.w + x] = 0.0;
            }
        }
    }
    for x in 0..c.w {
        if longest(&mut (0..c.h).map(|y| on[y * c.w + x])) >= 0.85 * h {
            for y in 0..c.h {
                c.ink[y * c.w + x] = 0.0;
            }
        }
    }
    c
}

/// A column's ink, its top and bottom to a fraction of a pixel (the edge
/// pixels' own levels), or `None`. Ink touching the crop's top or bottom is
/// a neighbouring line's. In a word with a dotted or accented letter, a
/// short run set apart above a stroke that still rises high is the dot or
/// the accent, and the column's top is that stroke's.
fn column(c: &Crop, x: usize, has_dot: bool) -> Option<(f64, f64)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut start = None;
    for y in 0..c.h {
        let on = c.at(x, y) > ON;
        match (on, start) {
            (true, None) => start = Some(y),
            (false, Some(s)) => {
                runs.push((s, y - 1));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push((s, c.h - 1));
    }
    runs.retain(|&(t, b)| t > 0 && b + 1 < c.h);
    let (&(t0, b0), &(_, last)) = (runs.first()?, runs.last()?);
    let mut t = t0;
    if has_dot && runs.len() >= 2 {
        let t1 = runs[1].0;
        if (b0 - t0) as f64 <= 0.5 * (t1 - t0) as f64
            && (last - t1) as f64 >= 0.62 * (last - t0) as f64
        {
            t = t1;
        }
    }
    let top = t as f64 - if t > 0 { c.at(x, t - 1) } else { 0.0 };
    let bot = (last + 1) as f64
        + if last + 1 < c.h {
            c.at(x, last + 1)
        } else {
            0.0
        };
    Some((top, bot))
}

/// The `q` quantile of `v`, linearly interpolated.
fn quantile(v: &[f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let pos = q.clamp(0.0, 1.0) * (s.len() - 1) as f64;
    let (i, f) = (pos.floor() as usize, pos - pos.floor());
    if i + 1 < s.len() {
        s[i] * (1.0 - f) + s[i + 1] * f
    } else {
        s[i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strokes of letters as rectangles in an upright frame: `(x0, y0, x1,
    /// y1)` from the word's left and its baseline, y up.
    type Strokes = Vec<(f64, f64, f64, f64)>;

    /// `Hon` with its baseline at 0: capitals 20 px tall, the x-height 14,
    /// stems 3 px wide, letters 4 px apart.
    fn hon() -> Strokes {
        vec![
            // H
            (0.0, 0.0, 3.0, 20.0),
            (10.0, 0.0, 13.0, 20.0),
            (0.0, 9.0, 13.0, 12.0),
            // o
            (17.0, 0.0, 20.0, 14.0),
            (25.0, 0.0, 28.0, 14.0),
            (17.0, 0.0, 28.0, 3.0),
            (17.0, 11.0, 28.0, 14.0),
            // n
            (32.0, 0.0, 35.0, 14.0),
            (40.0, 0.0, 43.0, 14.0),
            (32.0, 11.0, 43.0, 14.0),
        ]
    }

    /// A 200 × 80 grey image with `strokes` drawn in `ink` on `paper`, their
    /// baseline at (`ox`, `oy`) and turned `angle` degrees
    /// counter-clockwise about it, each pixel covered by 4 × 4 samples.
    fn draw(strokes: &Strokes, ink: u8, paper: u8, angle: f64) -> Pixels {
        let (w, h, ox, oy) = (200u32, 80u32, 60.0, 50.0);
        let (sin, cos) = angle.to_radians().sin_cos();
        let mut data = vec![paper; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let mut hit = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let (px, py) = (
                            x as f64 + (sx as f64 + 0.5) / 4.0 - ox,
                            oy - (y as f64 + (sy as f64 + 0.5) / 4.0),
                        );
                        // Back into the word's frame.
                        let (u, v) = (px * cos + py * sin, -px * sin + py * cos);
                        if strokes
                            .iter()
                            .any(|&(a, b, c, d)| u >= a && u < c && v >= b && v < d)
                        {
                            hit += 1;
                        }
                    }
                }
                let t = f64::from(hit) / 16.0;
                data[(y * w + x) as usize] =
                    (f64::from(paper) * (1.0 - t) + f64::from(ink) * t).round() as u8;
            }
        }
        Pixels::new(w, h, Layout::Luma8, data).unwrap()
    }

    /// The box an engine gives `Hon` drawn by [`draw`]: the ink and a
    /// margin, set upright about its center.
    fn hon_box(angle: f64) -> ([f64; 4], f64) {
        // The word's middle, 21.5 px along its baseline and 10 up, turned.
        let (sin, cos) = angle.to_radians().sin_cos();
        let (u, v) = (21.5, 10.0);
        let (cx, cy) = (60.0 + u * cos - v * sin, 50.0 - (u * sin + v * cos));
        ([cx - 25.5, cy - 14.0, cx + 25.5, cy + 14.0], angle)
    }

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 0.6)
    }

    fn assert_hon(ink: Option<Ink>) {
        let ink = ink.expect("measured");
        assert!(close(ink.first_cap, 20.0), "{ink:?}");
        assert!(close(ink.top, 20.0), "{ink:?}");
        assert_eq!(ink.top_line, Some(TopLine::Cap));
        assert!(close(ink.x_height, 14.0), "{ink:?}");
    }

    #[test]
    fn a_word_gives_its_capital_its_top_line_and_its_x_height() {
        let (bbox, angle) = hon_box(0.0);
        assert_hon(measure(&draw(&hon(), 0, 255, 0.0), bbox, angle, "Hon"));
    }

    #[test]
    fn light_text_on_a_dark_ground_measures_the_same() {
        let (bbox, angle) = hon_box(0.0);
        assert_hon(measure(&draw(&hon(), 240, 20, 0.0), bbox, angle, "Hon"));
    }

    #[test]
    fn faint_text_measures_by_its_own_contrast() {
        let (bbox, angle) = hon_box(0.0);
        assert_hon(measure(&draw(&hon(), 170, 230, 0.0), bbox, angle, "Hon"));
    }

    #[test]
    fn an_underline_is_a_rule_not_the_baseline() {
        let mut strokes = hon();
        strokes.push((-1.0, -4.0, 44.0, -2.5));
        let (bbox, angle) = hon_box(0.0);
        assert_hon(measure(&draw(&strokes, 0, 255, 0.0), bbox, angle, "Hon"));
    }

    #[test]
    fn a_word_on_a_turned_page_is_measured_level() {
        let (bbox, angle) = hon_box(3.0);
        assert_hon(measure(&draw(&hon(), 0, 255, 3.0), bbox, angle, "Hon"));
    }

    #[test]
    fn a_dot_over_an_i_is_not_the_top_line() {
        // `oi`: an x-height `o`, an `i` whose dot stands higher than any
        // capital would.
        let strokes = vec![
            (0.0, 0.0, 3.0, 14.0),
            (8.0, 0.0, 11.0, 14.0),
            (0.0, 0.0, 11.0, 3.0),
            (0.0, 11.0, 11.0, 14.0),
            (15.0, 0.0, 18.0, 14.0),
            (15.0, 18.0, 18.0, 21.0),
        ];
        let (bbox, angle) = hon_box(0.0);
        let ink = measure(&draw(&strokes, 0, 255, 0.0), bbox, angle, "oi").unwrap();
        assert!(close(ink.x_height, 14.0), "{ink:?}");
        assert_eq!(ink.top, None, "{ink:?}");
        assert_eq!(ink.first_cap, None);
    }

    #[test]
    fn specks_of_noise_are_not_ink() {
        let mut img = draw(&hon(), 0, 255, 0.0);
        // A speck in about one pixel in a hundred, around the word and
        // over it, as a scanner's salt and pepper.
        for i in (0..img.data.len()).step_by(97) {
            if img.data[i] == 255 {
                img.data[i] = 0;
            }
        }
        let (bbox, angle) = hon_box(0.0);
        assert_hon(measure(&img, bbox, angle, "Hon"));
    }

    #[test]
    fn a_word_whose_letters_say_nothing_has_no_measure() {
        let (bbox, angle) = hon_box(0.0);
        let img = draw(&hon(), 0, 255, 0.0);
        assert_eq!(measure(&img, bbox, angle, "—"), None);
        assert_eq!(measure(&img, bbox, angle, "語"), None);
    }

    #[test]
    fn a_capital_that_disagrees_with_its_word_is_dropped() {
        // A `T` whose bar's end stands apart from the rest by a gap: the
        // first run of columns is that end alone, 3 px tall.
        let strokes = vec![
            (0.0, 17.0, 3.0, 20.0),
            (4.0, 17.0, 13.0, 20.0),
            (7.0, 0.0, 10.0, 20.0),
            (17.0, 0.0, 20.0, 14.0),
            (25.0, 0.0, 28.0, 14.0),
            (17.0, 0.0, 28.0, 3.0),
            (17.0, 11.0, 28.0, 14.0),
            (32.0, 0.0, 35.0, 14.0),
            (40.0, 0.0, 43.0, 14.0),
            (32.0, 11.0, 43.0, 14.0),
        ];
        let (bbox, angle) = hon_box(0.0);
        let ink = measure(&draw(&strokes, 0, 255, 0.0), bbox, angle, "Ton").unwrap();
        assert_eq!(ink.first_cap, None, "{ink:?}");
        assert!(close(ink.top, 20.0), "{ink:?}");
    }
}
