//! Whether pixels look like text: part of the probe that decides which
//! images are worth reading.
//!
//! A raster image may hold text (a screen capture, a scanned table, a
//! labelled chart) or not (a photograph, a gradient, an illustration). The
//! PDF router sends a reader only to an image whose pixels look like text:
//! ink on paper (most pixels dark or light, many rows crossing from one to
//! the other often: a page of text, a table, a dense screen), or **lines of
//! text** ([`lines_of_text`]): on the image's ground, its most common
//! luminance, bands a line high, framed above and below by rows of bare
//! ground, whose ink falls in many narrow runs across the band. A screen
//! capture whose text is sparse or small, a few lines at the top of a wide,
//! light screen, is the second and not the first.
//!
//! The rule works on luminance, one byte a pixel, at the image's own size,
//! in two passes over its pixels: its cost grows with the pixels and stays
//! under the decode's. Measured on one core, a 1366 × 768 capture takes
//! about 0.6 ms, a page scanned at 300 dpi (2550 × 3300) about 5 ms, and
//! 100 million pixels about 55 ms, where decoding them takes seconds.
//! Averaging a large image down first would cost a pass of its own, and
//! would change what the rule sees: its lines are counted in pixels.

/// How far from the ground's luminance a pixel is still ground.
const GROUND_TOLERANCE: usize = 16;

/// Least share of the pixels on the ground: an image with no ground of one
/// colour (a photograph, a gradient) holds no lines on it.
const MIN_GROUND_SHARE: f64 = 0.25;

/// Width, in pixels, of the vertical bands the image is cut into, each
/// looked at for lines of its own: a line of text that spans a narrow
/// column of a wide screen is not lost in the bare ground beside it.
const BAND_WIDTH: usize = 320;

/// Most ink a row of a band may hold to be bare ground, as a share of the
/// band's width: what frames a line above and below.
const MAX_GAP_INK: f64 = 0.02;

/// Least and most rows a line of text spans: from a small type on a
/// screen to a large title.
const LINE_ROWS: std::ops::RangeInclusive<usize> = 4..=80;

/// Least runs of ink across a line: letters, or words.
const MIN_LINE_RUNS: usize = 4;

/// Share of a line's runs that must be narrow, no wider than three times
/// the line is high: letters and words are, a bar or a rule is not.
const MIN_NARROW_RUNS: f64 = 0.7;

/// Least share of a line's extent, from its first ink to its last, that
/// ink covers: a line of text is mostly letters, a scatter of specks is
/// not.
const MIN_LINE_COVER: f64 = 0.25;

/// Most of a line's band that ink may fill: text leaves ground between its
/// strokes, a solid block does not.
const MAX_LINE_FILL: f64 = 0.6;

/// Least lines for an image to hold text, or least share of its height one
/// line must cover alone ([`MIN_ONE_LINE_SHARE`]).
const MIN_LINES: usize = 2;

/// Least share of the image's height a single line must cover for it to
/// hold text alone: a crop of one line of text.
const MIN_ONE_LINE_SHARE: f64 = 0.08;

/// Lines of text on a ground: the image has a ground (at least
/// [`MIN_GROUND_SHARE`] of its pixels within [`GROUND_TOLERANCE`] of its
/// most common luminance), and within some band [`BAND_WIDTH`] wide, a run
/// of [`LINE_ROWS`] rows framed above and below by rows of bare ground
/// ([`MAX_GAP_INK`]) is a line when its ink falls in [`MIN_LINE_RUNS`] runs
/// or more across it, mostly narrow ([`MIN_NARROW_RUNS`]), covering
/// [`MIN_LINE_COVER`] of its extent and filling no more than
/// [`MAX_LINE_FILL`] of it. The image holds text with [`MIN_LINES`] such
/// lines, or one covering [`MIN_ONE_LINE_SHARE`] of its height.
///
/// Measured on synthetic images (screens light and dark, dialogs, charts,
/// tables, labels, against landscapes, foliage, gradients, textures, logos
/// and crops of photographs), OR-ed with ink on paper it misses 29 images
/// of 361 that hold text, where ink on paper alone misses 151, sparse
/// captures among them all 80; of 400 that hold none, it takes 96 for text
/// where ink on paper takes 79, the photographs it takes the same.
pub fn lines_of_text(w: usize, h: usize, luma: &[u8]) -> bool {
    if w < 8 || h < 8 || luma.len() < w * h {
        return false;
    }
    let luma = &luma[..w * h];
    let mut hist = [0usize; 256];
    for &v in luma {
        hist[usize::from(v)] += 1;
    }
    // The ground: the luminance with most pixels within the tolerance.
    let (mut ground, mut most) = (0usize, 0usize);
    for c in 0..256usize {
        let n: usize = hist[c.saturating_sub(GROUND_TOLERANCE)..=(c + GROUND_TOLERANCE).min(255)]
            .iter()
            .sum();
        if n > most {
            (ground, most) = (c, n);
        }
    }
    if (most as f64) < MIN_GROUND_SHARE * (w * h) as f64 {
        return false;
    }
    let ink: Vec<bool> = luma
        .iter()
        .map(|&v| usize::from(v).abs_diff(ground) > GROUND_TOLERANCE)
        .collect();
    let bands = (w / BAND_WIDTH).max(1);
    let mut line_rows = vec![false; h];
    let mut columns = Vec::new();
    for k in 0..bands {
        let (a, b) = (k * w / bands, (k + 1) * w / bands);
        let bw = b - a;
        let bare: Vec<bool> = (0..h)
            .map(|y| {
                let n = ink[y * w + a..y * w + b].iter().filter(|&&x| x).count();
                n as f64 <= MAX_GAP_INK * bw as f64
            })
            .collect();
        let mut i = 0;
        while i < h {
            if bare[i] {
                i += 1;
                continue;
            }
            let j = (i..h).find(|&y| bare[y]).unwrap_or(h);
            let rows = j - i;
            // Framed by bare ground on both sides: a run that touches the
            // image's edge may be cut.
            if LINE_ROWS.contains(&rows) && i > 0 && j < h {
                columns.clear();
                columns.resize(bw, false);
                let mut filled = 0usize;
                for y in i..j {
                    for (c, &x) in columns.iter_mut().zip(&ink[y * w + a..y * w + b]) {
                        if x {
                            *c = true;
                            filled += 1;
                        }
                    }
                }
                if is_line(&columns, rows, filled) {
                    line_rows[i..j].iter_mut().for_each(|r| *r = true);
                }
            }
            i = j;
        }
    }
    let lines = line_rows
        .iter()
        .zip(std::iter::once(&false).chain(&line_rows))
        .filter(|&(&r, &above)| r && !above)
        .count();
    let share = line_rows.iter().filter(|&&r| r).count() as f64 / h as f64;
    lines >= MIN_LINES || (lines >= 1 && share >= MIN_ONE_LINE_SHARE)
}

/// Whether a band `rows` high whose columns hold ink as `columns` does,
/// `filled` pixels of it ink, reads as a line of text.
fn is_line(columns: &[bool], rows: usize, filled: usize) -> bool {
    let (Some(first), Some(last)) = (
        columns.iter().position(|&c| c),
        columns.iter().rposition(|&c| c),
    ) else {
        return false;
    };
    let covered = columns.iter().filter(|&&c| c).count();
    let (mut runs, mut narrow, mut run) = (0usize, 0usize, 0usize);
    for &c in columns.iter().chain(std::iter::once(&false)) {
        if c {
            run += 1;
        } else if run > 0 {
            runs += 1;
            if run <= 3 * rows {
                narrow += 1;
            }
            run = 0;
        }
    }
    runs >= MIN_LINE_RUNS
        && narrow as f64 >= MIN_NARROW_RUNS * runs as f64
        && covered as f64 >= MIN_LINE_COVER * (last - first + 1) as f64
        && filled as f64 <= MAX_LINE_FILL * (rows * columns.len()) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A canvas of `w × h` pixels of one luminance.
    struct Canvas {
        w: usize,
        h: usize,
        ground: u8,
        px: Vec<u8>,
    }

    impl Canvas {
        fn new(w: usize, h: usize, ground: u8) -> Self {
            Canvas {
                w,
                h,
                ground,
                px: vec![ground; w * h],
            }
        }

        fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, v: u8) {
            for yy in y..(y + h).min(self.h) {
                for xx in x..(x + w).min(self.w) {
                    self.px[yy * self.w + xx] = v;
                }
            }
        }

        /// A line of text as a raster draws it: `words` words of `letters`
        /// letters, each letter a stroke `size * 0.6` wide with a counter
        /// in it, `size` high, a letter's gap between letters and three
        /// between words.
        fn text(&mut self, x: usize, y: usize, size: usize, words: usize, letters: usize, v: u8) {
            let letter = (size * 3 / 5).max(2);
            let gap = (size / 5).max(1);
            let mut at = x;
            for _ in 0..words {
                for _ in 0..letters {
                    self.rect(at, y, letter, size, v);
                    // The counter of an `o`, an `e`: ground inside the letter.
                    if letter >= 3 && size >= 3 {
                        self.rect(at + 1, y + 1, letter - 2, size - 2, self.ground);
                    }
                    at += letter + gap;
                }
                at += 2 * gap + letter;
            }
        }

        fn texty(&self) -> bool {
            lines_of_text(self.w, self.h, &self.px)
        }
    }

    /// A deterministic stream of bytes.
    fn noise(seed: u64) -> impl FnMut() -> u8 {
        let mut s = seed;
        move || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (s >> 56) as u8
        }
    }

    #[test]
    fn a_wide_light_screen_with_two_short_lines_at_its_top_holds_text() {
        // As issue #10's capture: a wide, light screen, a few short lines at
        // its top, the rest bare.
        let mut c = Canvas::new(1600, 700, 243);
        c.text(24, 20, 12, 3, 6, 40);
        c.text(24, 48, 12, 2, 7, 40);
        assert!(c.texty());
    }

    #[test]
    fn a_dark_screen_holds_text() {
        let mut c = Canvas::new(1200, 600, 30);
        for i in 0..8 {
            c.text(40, 40 + 30 * i, 11, 5, 5, 220);
        }
        assert!(c.texty());
    }

    #[test]
    fn a_capture_of_tiny_text_holds_text() {
        // Type five pixels high, in two short lines.
        let mut c = Canvas::new(900, 300, 250);
        c.text(10, 10, 5, 4, 5, 60);
        c.text(10, 22, 5, 3, 6, 60);
        assert!(c.texty());
    }

    #[test]
    fn a_crop_of_one_line_holds_text() {
        // One line filling most of its crop, a few pixels of margin.
        let mut c = Canvas::new(400, 30, 255);
        c.text(5, 5, 20, 3, 5, 0);
        assert!(c.texty());
    }

    #[test]
    fn a_photograph_like_texture_holds_no_text() {
        // Mid-grey values at random, as foliage or a crowd: no ground of
        // one colour, no bare rows.
        let mut next = noise(3);
        let mut c = Canvas::new(800, 600, 0);
        c.px.iter_mut().for_each(|p| *p = 64 + next() / 2);
        assert!(!c.texty());
        // Smoothed into blobs, as a photograph's regions are.
        let mut c = Canvas::new(800, 600, 128);
        let mut next = noise(9);
        for _ in 0..400 {
            let (x, y) = (usize::from(next()) * 3, usize::from(next()) * 2);
            let s = 10 + usize::from(next() / 8);
            c.rect(x, y, s, s, 70 + next() / 2);
        }
        assert!(!c.texty());
    }

    #[test]
    fn a_gradient_holds_no_text() {
        let (w, h) = (900, 500);
        let px: Vec<u8> = (0..w * h).map(|i| ((i % w) * 255 / w) as u8).collect();
        assert!(!lines_of_text(w, h, &px));
        // Top to bottom too.
        let px: Vec<u8> = (0..w * h).map(|i| ((i / w) * 255 / h) as u8).collect();
        assert!(!lines_of_text(w, h, &px));
    }

    #[test]
    fn a_flat_illustration_holds_no_text() {
        // A few large shapes of flat colour on a light ground: a logo, a
        // diagram's boxes with nothing written in them.
        let mut c = Canvas::new(800, 600, 240);
        c.rect(100, 100, 250, 200, 60);
        c.rect(450, 150, 200, 300, 120);
        c.rect(150, 380, 220, 120, 30);
        assert!(!c.texty());
    }

    #[test]
    fn a_line_needs_bare_ground_above_and_below() {
        // The same text touching the image's top edge, alone: cut, it may
        // be anything. Framed, it is a line.
        let mut c = Canvas::new(1600, 700, 243);
        c.text(24, 0, 12, 3, 6, 40);
        assert!(!lines_of_text(c.w, c.h, &c.px));
        let mut c = Canvas::new(1600, 700, 243);
        c.text(24, 20, 12, 3, 6, 40);
        c.text(24, 300, 12, 3, 6, 40);
        assert!(lines_of_text(c.w, c.h, &c.px));
    }

    #[test]
    fn a_rule_or_a_bar_is_no_line() {
        // Wide bars framed by ground: few runs, none narrow.
        let mut c = Canvas::new(1000, 400, 250);
        for i in 0..4 {
            c.rect(20, 30 + 60 * i, 900, 10, 20);
        }
        assert!(!lines_of_text(c.w, c.h, &c.px));
        // Specks spread thin across a band: many runs, little cover.
        let mut c = Canvas::new(1000, 400, 250);
        for y in [30, 90] {
            for x in (20..980).step_by(40) {
                c.rect(x, y, 2, 8, 20);
            }
        }
        assert!(!lines_of_text(c.w, c.h, &c.px));
    }

    #[test]
    fn lines_cut_by_both_edges_are_no_lines() {
        // Two lines, one touching the top edge and one the bottom: either
        // may be cut, and neither counts.
        let mut c = Canvas::new(1600, 400, 243);
        c.text(24, 0, 12, 3, 6, 40);
        c.text(24, 388, 12, 3, 6, 40);
        assert!(!lines_of_text(c.w, c.h, &c.px));
    }

    #[test]
    fn without_a_ground_of_one_colour_there_are_no_lines() {
        // Five bands of five greys, the first the largest but under a
        // quarter of the pixels: no ground, whatever the first band holds.
        let mut c = Canvas::new(1600, 300, 0);
        for (k, v) in [60u8, 120, 180, 240].into_iter().enumerate() {
            c.rect(380 + k * 305, 0, 305, 300, v);
        }
        c.ground = 0;
        c.text(10, 40, 12, 3, 5, 255);
        c.text(10, 100, 12, 3, 5, 255);
        assert!(!lines_of_text(c.w, c.h, &c.px));
    }

    /// `rows` bands, each `height` high and framed by ground, drawn by
    /// `band(canvas, y)`.
    fn bands(rows: usize, height: usize, band: impl Fn(&mut Canvas, usize)) -> Canvas {
        let mut c = Canvas::new(320, (rows + 1) * height * 2, 250);
        for r in 0..rows {
            band(&mut c, height + r * height * 2);
        }
        c
    }

    #[test]
    fn what_is_no_line_of_text() {
        // Too tall, a picket fence 100 rows high; too thin, 3 rows.
        for h in [100, 3] {
            let c = bands(2, h, |c, y| {
                for x in (10..300).step_by(12) {
                    c.rect(x, y, 6, h, 20);
                }
            });
            assert!(!lines_of_text(c.w, c.h, &c.px), "{h}");
        }
        // Three marks across: too few runs.
        let c = bands(2, 12, |c, y| {
            for x in [10, 70, 130] {
                c.rect(x, y, 30, 12, 20);
            }
        });
        assert!(!lines_of_text(c.w, c.h, &c.px));
        // Dashes, each over three times as wide as the band is high.
        let c = bands(2, 12, |c, y| {
            for x in [0, 80, 160, 240] {
                c.rect(x, y, 40, 12, 20);
            }
        });
        assert!(!lines_of_text(c.w, c.h, &c.px));
        // A grille: narrow runs, but ink fills the band.
        let c = bands(2, 12, |c, y| {
            for x in (0..320).step_by(11) {
                c.rect(x, y, 10, 12, 20);
            }
        });
        assert!(!lines_of_text(c.w, c.h, &c.px));
        // The same marks, sized and spaced as letters: lines.
        let c = bands(2, 12, |c, y| {
            for x in (10..250).step_by(12) {
                c.rect(x, y, 7, 12, 20);
            }
        });
        assert!(lines_of_text(c.w, c.h, &c.px));
    }

    #[test]
    fn one_line_alone_must_cover_enough_of_the_height() {
        // One line in a tall picture is not enough; in a short one it is.
        let mut c = Canvas::new(600, 600, 250);
        c.text(10, 300, 10, 4, 5, 20);
        assert!(!lines_of_text(c.w, c.h, &c.px));
        let mut c = Canvas::new(600, 100, 250);
        c.text(10, 40, 10, 4, 5, 20);
        assert!(lines_of_text(c.w, c.h, &c.px));
    }

    #[test]
    fn too_small_or_short_buffers_hold_no_text() {
        assert!(!lines_of_text(7, 100, &[0; 700]));
        assert!(!lines_of_text(100, 100, &[0; 50]));
    }
}
