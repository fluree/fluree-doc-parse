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
pub(crate) const MAX_RULE_THICKNESS: f64 = 3.0;

/// A rule must be at least this many times longer than it is thick, so a small
/// square is not mistaken for a very short line.
pub(crate) const MIN_RULE_ASPECT: f64 = 4.0;

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
/// A stroked line of straight segments, most of them neither horizontal nor
/// vertical: a chart's trace. Read as its box, as a path is, it was a fill;
/// kept apart, it says a plot is drawn here.
#[derive(Debug, Clone, Copy)]
pub struct Trace {
    pub bbox: BBox,
    /// Its straight segments, and of them the diagonal ones.
    pub segments: usize,
    pub diagonal: usize,
    /// Its segments running across the page one way, left to right or right
    /// to left, never back: a series plotted against its axis.
    pub onward: usize,
    pub page: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct Fill {
    pub bbox: BBox,
    pub page: usize,
}

/// Share of a fill's width the text on it must cover for the fill to be a
/// highlight behind that text.
const HIGHLIGHT_COVERAGE: f64 = 0.6;

/// Tallest a highlight stands over its line, in multiples of the text's
/// size: a marker pen's band, not a box around a paragraph.
const HIGHLIGHT_LINES: f64 = 2.0;

/// How far past its text a highlight may reach at either end, in multiples
/// of the text's size: a marker's band sits within a point or so of the
/// words, where a table's shading leaves its cells their padding.
const HIGHLIGHT_FIT: f64 = 0.2;

/// Drop the fills that are highlights behind a line of text.
///
/// A reviewer marks up a document by highlighting passages, and each
/// highlighted line is drawn as a filled band fitted to it. They are no part
/// of the page's structure, and read as structure they did harm twice over:
/// a stack of bands of assorted widths clusters as a bar chart does, so the
/// page's prose became a figure sent to a model; and a band behind each
/// line is the drawn geometry that vouches for text aligned as a table, so
/// clauses with their numbers hung in the margin were cut into columns.
///
/// A highlight is no taller than its line, mostly covered by it, and fitted
/// to it at both ends, as a marker pen leaves it. A table row's shading
/// leaves its cells their padding and its columns their gaps; a bar's label
/// covers a little of its bar, if it sits on it at all.
pub fn strip_highlights(fills: &mut Vec<Fill>, glyphs: &[crate::glyph::Glyph]) {
    // Inked glyphs by the height of their middle.
    let mut marks: Vec<(f64, BBox, f64)> = glyphs
        .iter()
        .filter(|g| g.is_horizontal() && !g.text.trim().is_empty())
        .filter_map(|g| g.bbox.map(|b| ((b.y0 + b.y1) * 0.5, b, g.font_size as f64)))
        .collect();
    marks.sort_by(|a, b| a.0.total_cmp(&b.0));
    fills.retain(|f| {
        let b = f.bbox;
        let lo = marks.partition_point(|m| m.0 < b.y0);
        let on: Vec<&(f64, BBox, f64)> = marks[lo..]
            .iter()
            .take_while(|m| m.0 <= b.y1)
            .filter(|m| {
                let x = (m.1.x0 + m.1.x1) * 0.5;
                x >= b.x0 && x <= b.x1
            })
            .collect();
        if on.is_empty() {
            return true;
        }
        let mut sizes: Vec<f64> = on.iter().map(|m| m.2).collect();
        sizes.sort_by(f64::total_cmp);
        let size = sizes[sizes.len() / 2].max(1.0);
        if b.height() > HIGHLIGHT_LINES * size {
            return true;
        }
        // The width the text covers, its marks' extents merged.
        let mut spans: Vec<(f64, f64)> = on
            .iter()
            .map(|m| (m.1.x0.max(b.x0), m.1.x1.min(b.x1)))
            .collect();
        spans.sort_by(|a, c| a.0.total_cmp(&c.0));
        let gap = size * 0.6;
        let (mut covered, mut cur) = (0.0, None::<(f64, f64)>);
        for (x0, x1) in spans {
            cur = match cur {
                // Word spaces belong to the text that spans them.
                Some((a, e)) if x0 <= e + gap => Some((a, e.max(x1))),
                Some((a, e)) => {
                    covered += e - a;
                    Some((x0, x1))
                }
                None => Some((x0, x1)),
            };
        }
        if let Some((a, e)) = cur {
            covered += e - a;
        }
        let text_x0 = on.iter().map(|m| m.1.x0).fold(f64::INFINITY, f64::min);
        let text_x1 = on.iter().map(|m| m.1.x1).fold(f64::NEG_INFINITY, f64::max);
        let fitted = (text_x0 - b.x0).abs() <= HIGHLIGHT_FIT * size
            && (b.x1 - text_x1).abs() <= HIGHLIGHT_FIT * size;
        !(fitted && covered >= HIGHLIGHT_COVERAGE * b.width())
    });
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

/// Is a filled shape a dot or a small square: a drawn bullet's size and
/// shape? Smaller than a checkbox, larger than a speck, and about as wide
/// as it is tall.
pub fn is_dot(b: &BBox) -> bool {
    let (w, h) = (b.width(), b.height());
    let (small, large) = (w.min(h), w.max(h));
    small >= DOT_MIN && large < CHECKBOX_MIN && small >= large * DOT_SQUARENESS
}

/// Smallest side, in points, of a drawn bullet: a 1.5pt square is one; a
/// hairline's end or a speck of a drawing is not.
const DOT_MIN: f64 = 1.0;

/// Shortest side of a drawn bullet against its longest.
const DOT_SQUARENESS: f64 = 0.7;

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
    // A letter of a word drawn as shapes, a logo's `O` or `N`, stands in a
    // run of three letters or more, each abutting the next on their
    // baseline; a box to tick stands apart, or beside one other.
    let lettered = |b: &BBox| {
        let mut row: Vec<BBox> = fills
            .iter()
            .map(|f| f.bbox)
            .filter(|o| {
                (o.y1 - b.y1).abs() <= 1.5 && (o.height() - b.height()).abs() <= b.height() * 0.2
            })
            .collect();
        row.sort_by(|p, q| p.x0.partial_cmp(&q.x0).unwrap_or(std::cmp::Ordering::Equal));
        row.dedup_by(|p, q| (p.x0 - q.x0).abs() < 0.5 && (p.x1 - q.x1).abs() < 0.5);
        let Some(at) = row.iter().position(|o| o == b) else {
            return false;
        };
        let abuts = |p: &BBox, q: &BBox| {
            let gap = q.x0 - p.x1;
            (-0.5..=b.height() * 0.5).contains(&gap)
        };
        let mut lo = at;
        while lo > 0 && abuts(&row[lo - 1], &row[lo]) {
            lo -= 1;
        }
        let mut hi = at;
        while hi + 1 < row.len() && abuts(&row[hi], &row[hi + 1]) {
            hi += 1;
        }
        hi - lo + 1 >= 3
    };
    // Nor is a piece of a drawing: a logo's illustration is built of
    // shapes, some of them small squares, and set beside an address its
    // pieces read as boxes before the address's lines. A box to tick stands
    // alone, on a panel or holding its tick; a drawing's piece overlaps its
    // neighbours part way, or lies in a shape that holds three pieces or
    // more that are neither boxes nor strips.
    let inside = |a: &BBox, o: &BBox| {
        a.x0 >= o.x0 - 0.5 && a.y0 >= o.y0 - 0.5 && a.x1 <= o.x1 + 0.5 && a.y1 <= o.y1 + 0.5
    };
    let squarish = |b: &BBox| (b.width() - b.height()).abs() <= CHECKBOX_SQUARENESS;
    let strip = |b: &BBox| b.width() >= b.height() * 3.0 || b.height() >= b.width() * 3.0;
    let piece = |b: &BBox| {
        fills.iter().map(|f| f.bbox).any(|o| {
            // The box itself, or its tick drawn inside it.
            if o == *b || inside(&o, b) {
                return false;
            }
            // A shape it lies in is a drawing when it holds three pieces;
            // a panel or the box's own outline does not.
            if inside(b, &o) {
                return fills
                    .iter()
                    .map(|f| f.bbox)
                    .filter(|p| *p != *b && *p != o && inside(p, &o) && !squarish(p) && !strip(p))
                    .count()
                    >= 3;
            }
            // A neighbour it overlaps part way.
            let w = b.x1.min(o.x1) - b.x0.max(o.x0);
            let h = b.y1.min(o.y1) - b.y0.max(o.y0);
            w > 0.0 && h > 0.0
        })
    };
    fills
        .iter()
        .map(|f| f.bbox)
        .filter(|b| {
            let (w, h) = (b.width(), b.height());
            (CHECKBOX_MIN..=CHECKBOX_MAX).contains(&w)
                && (CHECKBOX_MIN..=CHECKBOX_MAX).contains(&h)
                && (w - h).abs() <= CHECKBOX_SQUARENESS
                && !lettered(b)
                && !piece(b)
        })
        .collect()
}

/// A shape at least this fraction of the page's width wide is the page's
/// own decoration.
const PAGE_DECORATION_WIDTH: f64 = 0.98;

/// A full-width fill taller than this fraction of the page is a background,
/// not a band, and is left alone.
const PAGE_BAND_HEIGHT: f64 = 0.2;

/// Drop the page's own bands: fills and horizontal rules that run the full
/// width of the page. Returns how many were removed.
///
/// A header band, a footer band and the line drawn under a running head all
/// span the page edge to edge. Nothing a table or a chart draws does — they
/// sit inside the margins — yet to every detector downstream these are the
/// same primitives. Two header bands and a title box beneath them clustered
/// into a three-shape "figure" whose text was the page's banner heading; the
/// line under a running head corroborated the head's own two columns of
/// patient identifiers as an aligned table on dozens of pages of a chart.
/// Neither is a reading of content, and the evidence is the same in both:
/// the shape is as wide as the page and no taller than a band.
///
/// A full-page background fill is as wide, and stays. It fools nothing —
/// too large to be a figure, too large to be a row — and a slide deck's
/// aligned tables lean on it as the fill spanning them.
pub fn strip_page_decoration(
    rules: &mut Vec<Rule>,
    fills: &mut Vec<Fill>,
    page_width: f64,
    page_height: f64,
) -> usize {
    if page_width <= 0.0 || page_height <= 0.0 {
        return 0;
    }
    let full = page_width * PAGE_DECORATION_WIDTH;
    let band = page_height * PAGE_BAND_HEIGHT;
    let before = rules.len() + fills.len();
    rules.retain(|r| r.orientation != Orientation::Horizontal || r.bbox.width() < full);
    fills.retain(|f| f.bbox.width() < full || f.bbox.height() > band);
    before - rules.len() - fills.len()
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

    #[test]
    fn page_wide_bands_and_lines_are_decoration() {
        let band = |y0: f64, y1: f64, x0: f64, x1: f64| Fill {
            bbox: BBox { x0, y0, x1, y1 },
            page: 0,
        };
        let mut fills = vec![
            band(0.0, 30.0, 0.0, 612.0),
            band(100.0, 112.0, 40.0, 572.0),
            band(0.0, 792.0, 0.0, 612.0),
        ];
        let mut rules = vec![
            Rule {
                bbox: BBox {
                    x0: 0.0,
                    y0: 70.0,
                    x1: 612.0,
                    y1: 71.0,
                },
                orientation: Orientation::Horizontal,
                page: 0,
            },
            Rule {
                bbox: BBox {
                    x0: 40.0,
                    y0: 200.0,
                    x1: 572.0,
                    y1: 201.0,
                },
                orientation: Orientation::Horizontal,
                page: 0,
            },
        ];
        assert_eq!(
            strip_page_decoration(&mut rules, &mut fills, 612.0, 792.0),
            2
        );
        assert_eq!(
            fills.len(),
            2,
            "the zebra band inside the margins and the page background stay"
        );
        assert_eq!(rules.len(), 1, "the table rule inside the margins stays");
    }

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
    fn a_wordmarks_letters_are_no_checkboxes() {
        let f = |x0, y0, x1, y1| Fill {
            bbox: bb(x0, y0, x1, y1),
            page: 0,
        };
        // A logo's lettering, its squarish letters abutting, and a box to
        // tick beside a line of its own.
        let mut fills: Vec<Fill> = (0..5)
            .map(|k| {
                let x = 120.0 + 8.4 * k as f64;
                f(x, 300.0, x + 8.0, 308.5)
            })
            .collect();
        fills.push(f(60.0, 150.0, 67.0, 157.0));
        assert_eq!(checkboxes(&fills), [bb(60.0, 150.0, 67.0, 157.0)]);
    }

    #[test]
    fn a_drawings_pieces_are_no_checkboxes() {
        let f = |x0, y0, x1, y1| Fill {
            bbox: bb(x0, y0, x1, y1),
            page: 0,
        };
        // A logo's illustration: a shape holding pieces, one a small
        // square, another square overlapping a piece part way; and a box to
        // tick on a form's panel, with its tick inside it.
        let fills = vec![
            f(373.0, 636.0, 423.0, 663.0),
            f(393.0, 652.0, 404.0, 657.0),
            f(393.0, 655.0, 406.0, 663.0),
            f(388.0, 641.0, 397.0, 653.0),
            f(416.0, 639.0, 420.0, 645.0),
            f(419.0, 695.0, 425.0, 734.0),
            f(421.0, 694.0, 425.0, 699.0),
            f(30.0, 500.0, 300.0, 560.0),
            f(40.0, 511.0, 47.0, 518.0),
            f(42.0, 513.0, 45.0, 516.0),
        ];
        assert_eq!(checkboxes(&fills), [bb(40.0, 511.0, 47.0, 518.0)]);
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

    fn fills(v: &[BBox]) -> Vec<Fill> {
        v.iter().map(|b| Fill { bbox: *b, page: 0 }).collect()
    }

    /// A line of 10pt text from `x0`, one 5pt glyph per character.
    fn line(text: &str, x0: f64, base: f64) -> Vec<crate::glyph::Glyph> {
        text.chars()
            .enumerate()
            .map(|(i, c)| {
                let x = x0 + i as f64 * 5.0;
                crate::glyph::Glyph {
                    text: c.to_string(),
                    bbox: (c != ' ').then_some(bb(x, base - 7.0, x + 4.5, base)),
                    page: 0,
                    origin: (x, base),
                    rotation_deg: 0.0,
                    font_size: 10.0,
                    weight: None,
                    advance: Some(5.0),
                    draw_index: i,
                }
            })
            .collect()
    }

    #[test]
    fn highlighted_lines_are_not_a_drawing() {
        // Three lines of a marked-up page, each behind a band fitted to it:
        // stacked bands of assorted widths, as a bar chart's are.
        let mut g = line("Remove all materials from the line", 100.0, 110.0);
        g.extend(line("before the next lot", 100.0, 125.0));
        g.extend(line("and record the clearance on the form", 100.0, 140.0));
        let f = fills(&[
            bb(99.0, 101.0, 270.5, 113.0),
            bb(99.0, 116.0, 195.5, 128.0),
            bb(99.0, 131.0, 280.5, 143.0),
        ]);
        let mut f = f;
        strip_highlights(&mut f, &g);
        assert!(f.is_empty());
        // A bar with its value on it is still a bar.
        let mut bar = fills(&[bb(98.0, 101.0, 400.0, 113.0)]);
        let label = line("42%", 380.0, 110.0);
        strip_highlights(&mut bar, &label);
        assert_eq!(bar.len(), 1);
        // A shaded table row, its cells padded and its columns apart.
        let mut row = fills(&[bb(90.0, 101.0, 400.0, 113.0)]);
        let mut cells = line("Sodium chloride", 96.0, 110.0);
        cells.extend(line("0.9%", 300.0, 110.0));
        strip_highlights(&mut row, &cells);
        assert_eq!(row.len(), 1);
    }
}
