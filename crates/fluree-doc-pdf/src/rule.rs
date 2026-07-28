//! Ruling lines and filled cell backgrounds.
//!
//! Table detection needs the page's vector geometry, not just its text. Two
//! shapes matter and they are different signals:
//!
//! * **Rules** — thin stroked or filled shapes. A bordered table's grid.
//! * **Fills** — larger filled rectangles. Header shading and zebra striping,
//!   which mark row structure in tables that have no vertical rules at all.
//!
//! Everything here is geometry only. Deciding which rules form a table is
//! [`crate::table`]'s job.

use crate::geom::BBox;

/// Thickness at or below which a shape is a rule rather than a fill, in PDF
/// units. Hairlines are often 0.5pt; 3pt is a heavy rule but still a rule.
const MAX_RULE_THICKNESS: f64 = 3.0;

/// A rule must be at least this many times longer than it is thick, so a small
/// square is not mistaken for a very short line.
const MIN_RULE_ASPECT: f64 = 4.0;

/// Fraction of the page a rule must span to count as full-bleed.
const FULL_PAGE_SPAN: f64 = 0.95;

/// How many evenly spaced full-bleed rules make a lattice rather than a
/// coincidence. Two are a page border; three could be a border and a divider.
const MIN_LATTICE_RULES: usize = 4;

/// How far the gaps between them may vary and still be called even, in PDF
/// units. A drawn lattice is exact; this is for rounding, not for tolerance.
const LATTICE_PITCH_TOLERANCE: f64 = 1.0;

/// Below this the rules are not a lattice but one thick band drawn as strokes.
const MIN_LATTICE_PITCH: f64 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy)]
pub struct Rule {
    pub bbox: BBox,
    pub orientation: Orientation,
    pub page: usize,
}

impl Rule {
    /// Position along the axis the rule is perpendicular to — the y of a
    /// horizontal rule, the x of a vertical one. This is what table detection
    /// clusters on.
    pub fn axis_pos(&self) -> f64 {
        match self.orientation {
            Orientation::Horizontal => (self.bbox.y0 + self.bbox.y1) * 0.5,
            Orientation::Vertical => (self.bbox.x0 + self.bbox.x1) * 0.5,
        }
    }
    pub fn length(&self) -> f64 {
        match self.orientation {
            Orientation::Horizontal => self.bbox.width(),
            Orientation::Vertical => self.bbox.height(),
        }
    }
}

/// A filled area large enough to be a background rather than a rule.
#[derive(Debug, Clone, Copy)]
pub struct Fill {
    pub bbox: BBox,
    pub page: usize,
}

/// Classify a drawn shape's bounding box. Returns `None` for shapes that are
/// neither — glyph outlines drawn as paths, logos, curves.
pub fn classify(bbox: BBox, page: usize) -> Option<Shape> {
    let (w, h) = (bbox.width(), bbox.height());
    if w <= 0.0 && h <= 0.0 {
        return None;
    }
    if h <= MAX_RULE_THICKNESS && w >= h * MIN_RULE_ASPECT {
        return Some(Shape::Rule(Rule {
            bbox,
            orientation: Orientation::Horizontal,
            page,
        }));
    }
    if w <= MAX_RULE_THICKNESS && h >= w * MIN_RULE_ASPECT {
        return Some(Shape::Rule(Rule {
            bbox,
            orientation: Orientation::Vertical,
            page,
        }));
    }
    // Anything else with real area is a candidate fill. Table detection filters
    // further; a page background or a figure will simply not form a grid.
    if w > MAX_RULE_THICKNESS && h > MAX_RULE_THICKNESS {
        return Some(Shape::Fill(Fill { bbox, page }));
    }
    None
}

#[derive(Debug, Clone, Copy)]
pub enum Shape {
    Rule(Rule),
    Fill(Fill),
}

/// Smallest and largest side, in points, of a fill that can be a checkbox.
/// Sized against body text: a box beside 9pt type runs 6-8pt, and one drawn
/// much larger is a design element, not an option to tick.
const CHECKBOX_MIN: f64 = 4.0;
const CHECKBOX_MAX: f64 = 16.0;

/// How far from square a checkbox may be, in points.
const CHECKBOX_SQUARENESS: f64 = 2.5;

/// Fills that look like an empty checkbox: small, square, drawn as a box
/// rather than written as a glyph.
///
/// A printed form's options carry no bullet character at all — the marker is
/// vector art — so without this every `☐ Cured` / `☐ Paid-off` option reads
/// as running prose and the whole list collapses into one paragraph.
pub fn checkboxes(fills: &[Fill]) -> Vec<BBox> {
    fills
        .iter()
        .map(|f| f.bbox)
        .filter(|b| {
            let (w, h) = (b.width(), b.height());
            (CHECKBOX_MIN..=CHECKBOX_MAX).contains(&w)
                && (CHECKBOX_MIN..=CHECKBOX_MAX).contains(&h)
                && (w - h).abs() <= CHECKBOX_SQUARENESS
        })
        .collect()
}

/// Drop a drawn layout lattice: full-bleed rules at a constant pitch on both
/// axes. Returns how many were removed.
///
/// A designer's baseline grid is drawn with the same primitive a table's
/// ruling is, and to everything downstream it looks like a very large table.
/// One deck's page carried nine full-width rules at a 21.7pt pitch and five
/// full-height rules at 45pt, and the grid built from them shredded two
/// columns of prose into a twelve-by-twenty-three "table" whose cells tore
/// words in half.
///
/// Both axes are required, and that is the whole safety of the rule. Evenly
/// spaced rules on *one* axis are an ordinary table with equal rows, which is
/// common — one document in the evaluation corpus has six of them. A lattice
/// that also repeats down the other axis, with every rule crossing the entire
/// page, is not something a table does: a table's ruling spans the table.
/// Nothing in the corpus matches it.
pub fn strip_layout_lattice(rules: &mut Vec<Rule>, page_width: f64, page_height: f64) -> usize {
    let full_bleed = |r: &Rule| -> bool {
        match r.orientation {
            Orientation::Horizontal => r.bbox.width() >= FULL_PAGE_SPAN * page_width,
            Orientation::Vertical => r.bbox.height() >= FULL_PAGE_SPAN * page_height,
        }
    };
    // The longest run of full-bleed rules at one constant pitch, if it is long
    // enough to be a lattice.
    //
    // A run rather than the whole set, because a page border sits among them:
    // this deck's verticals are 0, 39.8, 84.8, 129.8, 174.9 — the first is the
    // page edge and the remaining four are the grid at 45pt. Testing the set
    // as a whole sees one irregular gap and calls the lattice a table.
    let lattice_axes = |o: Orientation| -> Option<(f64, f64)> {
        let mut axes: Vec<f64> = rules
            .iter()
            .filter(|r| r.orientation == o && full_bleed(r))
            .map(Rule::axis_pos)
            .collect();
        axes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        // Rules drawn twice at one position are one position.
        axes.dedup_by(|a, b| (*a - *b).abs() < LATTICE_PITCH_TOLERANCE);
        let (mut best, mut start) = (0usize..0usize, 0usize);
        for i in 1..axes.len() {
            let pitch = axes[i] - axes[i - 1];
            let breaks = pitch < MIN_LATTICE_PITCH
                || (i > start + 1
                    && (pitch - (axes[start + 1] - axes[start])).abs() >= LATTICE_PITCH_TOLERANCE);
            if breaks {
                // The pair that formed the new pitch both belong to the new
                // run, so it starts at the earlier of them.
                start = i - 1;
            }
            if i + 1 - start > best.len() {
                best = start..i + 1;
            }
        }
        if best.len() < MIN_LATTICE_RULES {
            return None;
        }
        // The run establishes the pitch; the lattice is everything drawn on
        // it. Taking only the run would leave the far side of any stray rule
        // that interrupts it still looking like table ruling.
        Some((axes[best.start], axes[best.start + 1] - axes[best.start]))
    };
    if page_width <= 0.0 || page_height <= 0.0 {
        return 0;
    }
    let (Some(h), Some(v)) = (
        lattice_axes(Orientation::Horizontal),
        lattice_axes(Orientation::Vertical),
    ) else {
        return 0;
    };
    let on_lattice = |r: &Rule| -> bool {
        let (phase, pitch) = match r.orientation {
            Orientation::Horizontal => h,
            Orientation::Vertical => v,
        };
        if !full_bleed(r) || pitch <= 0.0 {
            return false;
        }
        let steps = (r.axis_pos() - phase) / pitch;
        (steps - steps.round()).abs() * pitch < LATTICE_PITCH_TOLERANCE
    };
    let before = rules.len();
    // Only the lattice's own members. A real table drawn over it keeps its
    // ruling, which spans the table rather than the page — and so does a page
    // border, which is nobody's row boundary but is not this rule's business.
    rules.retain(|r| !on_lattice(r));
    before - rules.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(o: Orientation, axis: f64, from: f64, to: f64) -> Rule {
        let bbox = match o {
            Orientation::Horizontal => BBox {
                x0: from,
                y0: axis,
                x1: to,
                y1: axis + 0.5,
            },
            Orientation::Vertical => BBox {
                x0: axis,
                y0: from,
                x1: axis + 0.5,
                y1: to,
            },
        };
        Rule {
            bbox,
            orientation: o,
            page: 0,
        }
    }

    /// A page carrying a designer's baseline grid, as one deck draws it.
    fn lattice() -> Vec<Rule> {
        let mut v = Vec::new();
        for i in 0..9 {
            v.push(rule(
                Orientation::Horizontal,
                12.0 + 21.7 * i as f64,
                0.0,
                612.0,
            ));
        }
        for i in 0..5 {
            v.push(rule(
                Orientation::Vertical,
                40.0 + 45.0 * i as f64,
                0.0,
                612.0,
            ));
        }
        v
    }

    #[test]
    fn a_full_bleed_grid_on_both_axes_is_decoration() {
        let mut rules = lattice();
        assert_eq!(strip_layout_lattice(&mut rules, 612.0, 612.0), 14);
        assert!(rules.is_empty());
    }

    #[test]
    fn a_page_edge_among_the_lattice_does_not_hide_it() {
        // The deck's own shape: a border rule at x=0, then the grid at 45pt.
        let mut rules = lattice();
        rules.push(rule(Orientation::Vertical, 0.0, 0.0, 612.0));
        strip_layout_lattice(&mut rules, 612.0, 612.0);
        // The lattice goes; the border sits off the pitch and is left.
        assert_eq!(rules.len(), 1);
        assert!(rules[0].axis_pos() < 1.0);
    }

    #[test]
    fn evenly_spaced_rows_on_one_axis_are_a_table() {
        // Six equal rows and no vertical ruling: the commonest table there is,
        // and one the corpus actually contains.
        let mut rules: Vec<Rule> = (0..6)
            .map(|i| rule(Orientation::Horizontal, 100.0 + 30.0 * i as f64, 0.0, 612.0))
            .collect();
        assert_eq!(strip_layout_lattice(&mut rules, 612.0, 612.0), 0);
        assert_eq!(rules.len(), 6);
    }

    #[test]
    fn a_table_drawn_over_a_lattice_keeps_its_own_ruling() {
        let mut rules = lattice();
        // Ruling that spans the table, not the page.
        rules.push(rule(Orientation::Horizontal, 300.0, 80.0, 400.0));
        rules.push(rule(Orientation::Vertical, 200.0, 280.0, 360.0));
        strip_layout_lattice(&mut rules, 612.0, 612.0);
        assert_eq!(rules.len(), 2);
        assert!(rules.iter().all(|r| r.bbox.width() < 400.0));
    }

    #[test]
    fn a_page_border_is_not_a_lattice() {
        let mut rules = vec![
            rule(Orientation::Horizontal, 10.0, 0.0, 612.0),
            rule(Orientation::Horizontal, 600.0, 0.0, 612.0),
            rule(Orientation::Vertical, 10.0, 0.0, 612.0),
            rule(Orientation::Vertical, 600.0, 0.0, 612.0),
        ];
        assert_eq!(strip_layout_lattice(&mut rules, 612.0, 612.0), 0);
    }

    #[test]
    fn unevenly_spaced_full_bleed_rules_are_left_alone() {
        // Full-bleed dividers at whatever spacing the content wanted. Nothing
        // repeats, so there is no lattice to find.
        let mut rules: Vec<Rule> = [10.0, 60.0, 130.0, 220.0, 330.0]
            .into_iter()
            .map(|y| rule(Orientation::Horizontal, y, 0.0, 612.0))
            .chain(
                [10.0, 70.0, 150.0, 260.0]
                    .into_iter()
                    .map(|x| rule(Orientation::Vertical, x, 0.0, 612.0)),
            )
            .collect();
        assert_eq!(strip_layout_lattice(&mut rules, 612.0, 612.0), 0);
    }

    #[test]
    fn one_extra_rule_does_not_hide_a_lattice() {
        // A lattice with something drawn across it is still a lattice: the
        // run is found among the rules, not required to be all of them.
        let mut rules = lattice();
        rules.push(rule(Orientation::Horizontal, 91.0, 0.0, 612.0));
        assert_eq!(strip_layout_lattice(&mut rules, 612.0, 612.0), 14);
        assert_eq!(rules.len(), 1);
    }

    fn bb(x0: f64, y0: f64, x1: f64, y1: f64) -> BBox {
        BBox { x0, y0, x1, y1 }
    }

    #[test]
    fn thin_wide_shape_is_a_horizontal_rule() {
        let s = classify(bb(10.0, 100.0, 400.0, 100.5), 0).unwrap();
        match s {
            Shape::Rule(r) => {
                assert_eq!(r.orientation, Orientation::Horizontal);
                assert!((r.axis_pos() - 100.25).abs() < 0.01);
                assert!((r.length() - 390.0).abs() < 0.01);
            }
            _ => panic!("expected a rule"),
        }
    }

    #[test]
    fn thin_tall_shape_is_a_vertical_rule() {
        match classify(bb(50.0, 10.0, 50.8, 300.0), 0).unwrap() {
            Shape::Rule(r) => assert_eq!(r.orientation, Orientation::Vertical),
            _ => panic!("expected a rule"),
        }
    }

    #[test]
    fn a_large_rectangle_is_a_fill_not_a_rule() {
        match classify(bb(10.0, 10.0, 300.0, 60.0), 0).unwrap() {
            Shape::Fill(_) => {}
            _ => panic!("expected a fill"),
        }
    }

    #[test]
    fn a_small_square_is_neither() {
        // Aspect ratio too low to be a rule, too small to be a background.
        assert!(classify(bb(10.0, 10.0, 12.0, 12.0), 0).is_none());
    }

    #[test]
    fn checkboxes_are_small_squares_only() {
        let f = |x0, y0, x1, y1| Fill {
            bbox: bb(x0, y0, x1, y1),
            page: 0,
        };
        let fills = vec![
            f(40.0, 511.0, 47.0, 518.0),  // a checkbox
            f(36.0, 211.0, 576.0, 229.0), // a header band
            f(439.0, 36.0, 576.0, 63.0),  // a logo
            f(10.0, 10.0, 14.0, 22.0),    // tall, not square
        ];
        let boxes = checkboxes(&fills);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].x0, 40.0);
    }

    #[test]
    fn zero_area_is_rejected() {
        assert!(classify(bb(10.0, 10.0, 10.0, 10.0), 0).is_none());
    }
}
