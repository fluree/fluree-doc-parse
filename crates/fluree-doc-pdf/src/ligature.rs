//! Ligatures a document draws without saying which letters they are.
//!
//! A PDF maps each glyph it draws to the characters it stands for, and some
//! producers map a ligature, `ff` or `fi` drawn as one glyph, to U+FFFD,
//! the character for "unknown". Some set the ligatures apart in a font of
//! their own, a subset holding two or three glyphs and nothing else: no
//! glyph names, no character map, no ligature table. Every reader of the
//! text layer then reads `d’a�aires` where the page prints `d’affaires`,
//! and `béné�cie` for `bénéficie`.
//!
//! The shape is still on the page. The same family's `f`, `i` and `l` are
//! drawn elsewhere with their characters known, and a ligature is those
//! letters set together, joined where the `f` meets the next letter. So an
//! unknown glyph is set beside each ligature those letters can make, and
//! named after the one it covers best, when it covers that one well and
//! clearly better than any other.

use std::collections::HashMap;

use kurbo::{Affine, BezPath, Point, Rect, Shape};

/// The ligatures recognised, as the letters they join. These are the Latin
/// ligatures Unicode keeps a compatibility character for (U+FB00–U+FB04),
/// the ones fonts set by default.
const LIGATURES: &[&str] = &["ff", "fi", "fl", "ffi", "ffl"];

/// The letters the ligatures are made of.
pub(crate) fn is_ligature_letter(text: &str) -> bool {
    matches!(text, "f" | "i" | "l")
}

/// Share of the two shapes' combined ink that both must cover: the
/// ligature's join replaces an `i`'s dot or the overlap of two `f`s, and
/// the rest of the letters is the same ink.
const MIN_OVERLAP: f64 = 0.72;

/// How much more of the two shapes the next-best ligature must leave
/// uncovered than the best, at least: a shape between two readings is left
/// unread rather than guessed.
const MIN_MARGIN: f64 = 0.03;

/// And how many times as much.
const MIN_MISS_RATIO: f64 = 2.0;

/// Rows of the grid the two shapes are compared on.
const GRID_ROWS: usize = 48;

/// The outlines of the letters a document draws with their characters
/// known, by font family, each with its advance, in the font's 1000-unit
/// em; and the family each font belongs to.
#[derive(Default)]
pub(crate) struct Letters {
    by_family: HashMap<String, HashMap<char, (BezPath, f64)>>,
    family_of: HashMap<u128, Option<String>>,
}

impl Letters {
    /// The family of a font, its `/BaseFont` without the subset prefix,
    /// asked of the font once.
    pub(crate) fn family(
        &mut self,
        font: u128,
        name: impl FnOnce() -> Option<String>,
    ) -> Option<String> {
        self.family_of.entry(font).or_insert_with(name).clone()
    }

    /// Whether a letter of this family is still to be kept.
    pub(crate) fn wants(&self, family: &str, letter: char) -> bool {
        !self
            .by_family
            .get(family)
            .is_some_and(|m| m.contains_key(&letter))
    }

    pub(crate) fn keep(&mut self, family: String, letter: char, outline: BezPath, advance: f64) {
        self.by_family
            .entry(family)
            .or_default()
            .entry(letter)
            .or_insert((outline, advance));
    }

    /// The ligature a glyph of this family draws, read from its outline.
    pub(crate) fn name(&self, family: &str, outline: &BezPath) -> Option<&'static str> {
        let letters = self.by_family.get(family)?;
        let mut fits: Vec<(f64, &'static str)> = LIGATURES
            .iter()
            .filter_map(|lig| {
                let joined = set_together(lig, letters)?;
                Some((overlap(outline, &joined), *lig))
            })
            .collect();
        fits.sort_by(|a, b| b.0.total_cmp(&a.0));
        let (best, name) = *fits.first()?;
        let next = fits.get(1).map_or(0.0, |f| f.0);
        // What each leaves uncovered. The runner-up must miss clearly more:
        // `fi` and `fl` differ only by the `i`'s dot against the `l`'s stem,
        // so a near-perfect `fi` beats `fl` by a little, and a rough fit
        // that beats it by as little is a guess.
        let (miss, next_miss) = (1.0 - best, 1.0 - next);
        let clear = next_miss >= MIN_MISS_RATIO * miss && next_miss - miss >= MIN_MARGIN;
        (best >= MIN_OVERLAP && clear).then_some(name)
    }
}

/// The letters of a ligature set one after another at their advances.
fn set_together(ligature: &str, letters: &HashMap<char, (BezPath, f64)>) -> Option<BezPath> {
    let mut path = BezPath::new();
    let mut x = 0.0;
    for c in ligature.chars() {
        let (outline, advance) = letters.get(&c)?;
        path.extend(Affine::translate((x, 0.0)) * outline.clone());
        x += advance;
    }
    Some(path)
}

/// Intersection over union of two shapes' ink, sampled on a grid over the
/// box that holds them both.
fn overlap(a: &BezPath, b: &BezPath) -> f64 {
    let ba = a.bounding_box();
    let bb = b.bounding_box();
    if ba.area() <= 0.0 || bb.area() <= 0.0 {
        return 0.0;
    }
    let r: Rect = ba.union(bb);
    let step = r.height() / GRID_ROWS as f64;
    if step <= 0.0 {
        return 0.0;
    }
    let cols = (r.width() / step).ceil() as usize;
    let (mut both, mut either) = (0usize, 0usize);
    for row in 0..GRID_ROWS {
        let y = r.y0 + (row as f64 + 0.5) * step;
        for col in 0..cols {
            let p = Point::new(r.x0 + (col as f64 + 0.5) * step, y);
            let (ina, inb) = (a.winding(p) != 0, b.winding(p) != 0);
            both += usize::from(ina && inb);
            either += usize::from(ina || inb);
        }
    }
    if either == 0 {
        0.0
    } else {
        both as f64 / either as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> BezPath {
        Rect::new(x0, y0, x1, y1).to_path(0.1)
    }

    /// Letters drawn as blocks: an `f` a tall stem with a crossbar, an `i` a
    /// short stem with a dot, an `l` a tall stem.
    fn letters() -> Letters {
        let mut l = Letters::default();
        let mut f = rect(100.0, 0.0, 180.0, 720.0);
        f.extend(rect(40.0, 440.0, 300.0, 500.0));
        f.extend(rect(180.0, 640.0, 330.0, 720.0));
        l.keep("Sans".into(), 'f', f, 300.0);
        let mut i = rect(60.0, 0.0, 140.0, 500.0);
        i.extend(rect(60.0, 600.0, 140.0, 690.0));
        l.keep("Sans".into(), 'i', i, 200.0);
        l.keep("Sans".into(), 'l', rect(60.0, 0.0, 140.0, 760.0), 200.0);
        l
    }

    #[test]
    fn an_unmapped_ligature_is_named_by_the_letters_it_joins() {
        let l = letters();
        // A ligature is its letters set together, the join a little
        // different: here the `i` loses its dot to the `f`'s hook.
        let mut fi = set_together("fi", &l.by_family["Sans"]).unwrap();
        fi.extend(rect(330.0, 640.0, 440.0, 720.0));
        assert_eq!(l.name("Sans", &fi), Some("fi"));
        let ff = set_together("ff", &l.by_family["Sans"]).unwrap();
        assert_eq!(l.name("Sans", &ff), Some("ff"));
        let ffl = set_together("ffl", &l.by_family["Sans"]).unwrap();
        assert_eq!(l.name("Sans", &ffl), Some("ffl"));
    }

    #[test]
    fn a_shape_that_is_no_ligature_stays_unread() {
        let l = letters();
        // A dot leader's period, and a glyph of a family never seen.
        assert_eq!(l.name("Sans", &rect(40.0, 0.0, 120.0, 80.0)), None);
        let ff = set_together("ff", &l.by_family["Sans"]).unwrap();
        assert_eq!(l.name("Serif", &ff), None);
    }
}
