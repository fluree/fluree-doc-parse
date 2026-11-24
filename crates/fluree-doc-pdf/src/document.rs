//! The whole layout pipeline, producing DoCO elements.
//!
//! Order matters and is load-bearing at every step:
//!
//! 1. **dedup** — faux-bold overprint, before anything counts glyphs.
//! 2. **tables** — grids are found from ruling geometry, and their glyphs are
//!    withheld from prose assembly. Without this a table's cells appear twice:
//!    once as cells and again as paragraphs.
//! 3. **columns** — before lines, because line assembly groups by baseline and
//!    columns share baselines.
//! 4. **lines**, then **furniture**, then **blocks** — furniture must go before
//!    blocks or a footer is absorbed into the last paragraph.
//! 5. **headings** — over blocks, using the outline tree where present.

use crate::block::{self, Block};
use crate::dedup;
use crate::extract::Document as RawDoc;
use crate::extract::Page;
use crate::heading::{self, Evidence};
use crate::line::{self, Line};
use crate::outline::OutlineItem;
use crate::table::{self, Grid};
use crate::{figure, furniture, glyph::Glyph};
pub use fluree_doc_model::element::Element;
pub use fluree_doc_model::emit::{to_markdown, to_xhtml};

pub struct Analysis {
    pub elements: Vec<Element>,
    /// Table regions whose detected structure disagrees with itself — the
    /// escalation candidates for a model tier. Reported, never acted on
    /// here; see `table::suspect_tables`.
    pub suspect_tables: Vec<table::SuspectTable>,
    /// A heading hierarchy resting on weak evidence — the escalation
    /// candidate no existing trigger can see. Reported, never acted on here;
    /// see `heading::doubt`.
    /// One entry per page whose hierarchy is doubtful; empty when none is.
    pub suspect_headings: Vec<heading::Doubt>,
    /// Pages whose text sits mostly inside their drawings — a designed
    /// layout, where geometry was never arranged to be read linearly.
    /// Reported, never acted on here; see `figure::doubt`.
    pub suspect_figures: Vec<figure::Doubt>,
    pub leading: f64,
    pub body_font: f32,
    pub furniture_removed: usize,
    /// The repeated header/footer strings this document carries, with a flag
    /// for the ones whose digits vary across pages (page numbers).
    ///
    /// Exposed because a model tier needs them. The deterministic pass strips
    /// furniture before assembling anything, so its output never carries a
    /// running footer; a page reading is transcribed from the pixels and
    /// always does. Handing the same list to [`crate::arbiter::scrub_furniture`]
    /// is what makes the two paths agree.
    pub furniture: Vec<(String, bool)>,
    /// Each page's printed page number, where the document numbers its
    /// pages; see [`furniture::detect_with_folios`]. Indexed by page
    /// position.
    pub folios: Vec<Option<String>>,
    /// Every line removed as furniture, as `(page, text, kind)` — the record
    /// of what the pass above actually stripped, for a tool explaining it.
    pub furniture_marks: Vec<(usize, String, furniture::Furniture)>,
    /// Every running header and footer line that names something, on every
    /// page that printed it, in reading order; see
    /// [`furniture::keep_running`].
    pub running: Vec<furniture::Running>,
    pub tables: usize,
    /// Wall clock per stage; see [`StageTimings`].
    pub timings: StageTimings,
}

/// Row 1 names each column a label of row 0 spans: under every span a label
/// of its own, all different, and no number anywhere in the row. Then the
/// two rows are one header, and a column is named by both.
///
/// Its own: a value run across the span's columns (`3.1 mg in 2 mL` under
/// `Strength`) continues left and names nothing. And short where the label
/// above spans every column, which is as often a band titling the rows
/// beneath (`Monotherapy` over `Adult patients with …`) as a header over
/// the column names (`Establishment` over `Name | Address | ID/FEI`).
/// `continues_left` is row-major over the whole grid.
fn names_spanned_columns(rows: &[Vec<String>], continues_left: &[bool]) -> bool {
    let cols = rows.first().map(Vec::len).unwrap_or(0);
    if rows.len() < 3 || continues_left.len() < 2 * cols {
        return false;
    }
    let spans = header_spans(&continues_left[..cols]);
    let labels = |a: usize, b: usize| {
        let names: Vec<&str> = rows[1][a..b].iter().map(|t| t.trim()).collect();
        let short = b - a < cols || names.iter().all(|t| t.split_whitespace().count() <= 4);
        short
            && (a + 1..b).all(|k| !continues_left[cols + k])
            && names
                .iter()
                .all(|t| !t.is_empty() && !table::is_numeric_cell(t))
            && names.iter().collect::<std::collections::HashSet<_>>().len() == names.len()
    };
    !spans.is_empty()
        && spans.iter().all(|&(a, b)| labels(a, b))
        && rows[1]
            .iter()
            .all(|t| t.trim().is_empty() || !table::is_numeric_cell(t.trim()))
}

/// The columns each spanning label of a header row covers, as `start..end`,
/// from that row's flags for a cell continuing the one to its left.
fn header_spans(continues_left: &[bool]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut c = 0;
    while c < continues_left.len() {
        let end = (c + 1..continues_left.len())
            .find(|&k| !continues_left[k])
            .unwrap_or(continues_left.len());
        if end - c > 1 {
            spans.push((c, end));
        }
        c = end;
    }
    spans
}

/// Row `r` labels the rows beneath it: a label in its first column and
/// nothing in the rest, drawn as cells of the table rather than a band
/// across it, over a row with a value in every column (`Focus-Inject` over
/// `Position | 12% | 12% | 12% | 12%`).
///
/// The full row below is what tells a group from a row whose values are
/// missing: a receipt's `TOTAL` with no amount sits over `Cash` and
/// `Change`, as sparse as it is. Not a label wrapped onto the next row
/// either (`Median in` over `months (95% CI)`): a first cell below that
/// starts lowercase or with a bracket carries this one on. And not in a
/// two-column list, where a field left empty looks the same.
fn group_label(rows: &[Vec<String>], r: usize) -> bool {
    let (Some(row), Some(next)) = (rows.get(r), rows.get(r + 1)) else {
        return false;
    };
    let label = row[0].trim();
    let below = next[0].trim();
    row.len() >= 3
        && !label.is_empty()
        && !table::is_numeric_cell(label)
        && row[1..].iter().all(|t| t.trim().is_empty())
        && next.iter().all(|t| !t.trim().is_empty())
        && !below.starts_with(|c: char| c.is_lowercase() || c == '(' || c == '[')
}

/// A table's frame on its page: where its columns are drawn, and whether
/// its first row is set apart the way a header is.
struct TableFrame {
    page: usize,
    bbox: crate::geom::BBox,
    xs: Vec<f64>,
    styled: bool,
    bold: bool,
}

/// How far apart, in points, two parts' column edges may be drawn and still
/// be one table's columns. A table the page break splits is drawn by one
/// layout on both pages, so its edges agree to within a rule's stroke.
const CONTINUED_EDGE_TOLERANCE: f64 = 3.0;

/// Link a table the page break split to its part on the page before.
///
/// A table that fills the foot of one page and goes on at the head of the
/// next is read as one grid per page. The second part has no header unless
/// the document repeats it, and read alone its first data row is taken for
/// one; a cell open into the rows below it even grows that into a header
/// three rows deep, which names every column below after a row of data.
///
/// Two parts are one table when nothing comes between them (the first is
/// the last thing read on its page, the second the first on the next) and
/// their columns are drawn at the same places. A second part that opens
/// with a styled row is a new table, unless that row repeats the first
/// part's header.
///
/// A break can also fall inside the header: the arms over the columns on
/// one page, `All Grades | Grades 3-4` under each on the next. The first
/// part is then nothing but header, and the second opens with a bold row
/// of labels. That row finishes the header and stays one, so the columns
/// are named by both.
fn link_continued_tables(elements: &mut [Element], frames: &[TableFrame]) {
    let frame = |e: &Element| {
        frames
            .iter()
            .find(|f| f.page == e.page && Some(f.bbox) == e.bbox)
    };
    let same = |a: &[String], b: &[String]| {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(x, y)| x.split_whitespace().eq(y.split_whitespace()))
    };
    for i in 1..elements.len() {
        let (before, after) = elements.split_at_mut(i);
        let (prev, next) = (&before[i - 1], &mut after[0]);
        if prev.kind != "doco:Table" || next.kind != "doco:Table" || next.page != prev.page + 1 {
            continue;
        }
        let (Some(a), Some(b)) = (frame(prev), frame(next)) else {
            continue;
        };
        let (Some(head), Some(rows)) = (prev.cells.as_ref(), next.cells.as_ref()) else {
            continue;
        };
        let aligned = a.xs.len() == b.xs.len()
            && a.xs
                .iter()
                .zip(&b.xs)
                .all(|(x, y)| (x - y).abs() <= CONTINUED_EDGE_TOLERANCE);
        if !aligned {
            continue;
        }
        // A second part that is the first one over again, every row, is
        // the next copy of a form printed box after box down the pages, not
        // the rest of the first: no table carries on by repeating its body.
        if head.len() == rows.len() && head.iter().zip(rows).all(|(a, b)| same(a, b)) {
            continue;
        }
        let n = prev.header_rows.unwrap_or(0).min(head.len());
        let repeats = n > 0 && rows.len() > n && (0..n).all(|r| same(&head[r], &rows[r]));
        let finishes_header = prev.header_only()
            && rows.len() > 1
            && b.bold
            && rows[0]
                .iter()
                .all(|t| t.trim().is_empty() || !table::is_numeric_cell(t.trim()));
        if b.styled && !repeats && !finishes_header {
            continue;
        }
        next.continues = Some(prev.id.clone());
        next.header_rows = Some(if repeats {
            n
        } else if finishes_header {
            1
        } else {
            0
        });
    }
}

/// Insert tables into the existing prose reading stream without reordering
/// blocks. Horizontal overlap selects the relevant column; within that lane a
/// table follows only elements that are entirely above its top edge. Blocks
/// overlapping the table's vertical band are captions or extraction artefacts,
/// not preceding prose.
fn interleave_tables(mut content: Vec<Element>, mut table_elements: Vec<Element>) -> Vec<Element> {
    table_elements.sort_by(|a, b| {
        a.rect()
            .y0
            .partial_cmp(&b.rect().y0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.rect()
                    .x0
                    .partial_cmp(&b.rect().x0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    for table in table_elements {
        let mut last_before = None;
        let mut first_after = None;
        for (i, element) in content.iter().enumerate() {
            let overlap =
                element.rect().x1.min(table.rect().x1) - element.rect().x0.max(table.rect().x0);
            if overlap <= 0.0 {
                continue;
            }
            if element.rect().y1 <= table.rect().y0 {
                last_before = Some(i);
            } else if first_after.is_none() {
                first_after = Some(i);
            }
        }
        let insertion = last_before
            .map(|i| i + 1)
            .or(first_after)
            .unwrap_or_else(|| {
                content
                    .iter()
                    .position(|e| e.rect().y0 >= table.rect().y1)
                    .unwrap_or(content.len())
            });
        content.insert(insertion, table);
    }
    content
}

/// Layout-detector boxes for one page, filtered by label set, in PDF units.
/// Returns None when no sidecar exists (layout arbitration not in use), so
/// callers can distinguish "layout saw nothing" from "layout never ran".
fn layout_boxes(
    prefix: Option<&std::path::Path>,
    page: usize,
    labels: &[&str],
) -> Option<Vec<crate::geom::BBox>> {
    let f = prefix?.as_os_str().to_string_lossy().to_string();
    let txt = std::fs::read_to_string(format!("{f}_p{page}_page.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    Some(
        v.as_array()
            .into_iter()
            .flatten()
            .filter(|b| {
                labels.contains(&b["label"].as_str().unwrap_or(""))
                    && b["score"].as_f64().unwrap_or(0.0) >= 0.6
            })
            .filter_map(|b| {
                let c: Vec<f64> = b["box"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x.as_f64())
                    .collect();
                (c.len() == 4).then(|| crate::geom::BBox {
                    x0: c[0] / 2.0,
                    y0: c[1] / 2.0,
                    x1: c[2] / 2.0,
                    y1: c[3] / 2.0,
                })
            })
            .collect(),
    )
}

/// The size most of a page's text is set in: its prose, against which a
/// drawing's labels are set smaller.
fn prose_size(blocks: &[Block]) -> f32 {
    let mut by_size: Vec<(f32, usize)> = Vec::new();
    for b in blocks {
        let size = (b.font_size * 2.0).round() / 2.0;
        let chars: usize = b.lines.iter().map(|l| l.text.chars().count()).sum();
        match by_size.iter_mut().find(|(s, _)| *s == size) {
            Some((_, n)) => *n += chars,
            None => by_size.push((size, chars)),
        }
    }
    by_size
        .into_iter()
        .max_by_key(|&(_, n)| n)
        .map_or(0.0, |(s, _)| s)
}

/// Fraction of a block that must lie inside a figure's box for the block to
/// belong to it. High enough that a paragraph brushing the drawing's margin
/// stays prose, low enough that a caption tucked under the axis comes along.
const FIGURE_CONTAINMENT: f64 = 0.6;

fn heading_yields_to_figure(evidence: Evidence, in_figure: bool) -> bool {
    in_figure && matches!(evidence, Evidence::FontSize | Evidence::Bold)
}

fn heading_fragments_join(a: &Element, b: &Element) -> bool {
    if a.kind != "doco:SectionTitle"
        || b.kind != "doco:SectionTitle"
        || a.page != b.page
        || a.level != b.level
        || a.evidence != b.evidence
        || !matches!(a.evidence, "font-size" | "bold")
        || !b
            .text
            .chars()
            .find(|c| c.is_alphabetic())
            .is_some_and(char::is_lowercase)
    {
        return false;
    }
    let (Some(ab), Some(bb)) = (a.bbox, b.bbox) else {
        return false;
    };
    let height = ab.height().max(bb.height()).max(1.0);
    (ab.x0 - bb.x0).abs() <= height && bb.y0 >= ab.y0 && bb.y0 - ab.y1 <= 1.5 * height
}

fn coalesce_heading_fragments(elements: &mut Vec<Element>) {
    let mut out: Vec<Element> = Vec::with_capacity(elements.len());
    for e in elements.drain(..) {
        if let Some(previous) = out.last_mut() {
            if heading_fragments_join(previous, &e) {
                previous.text.push(' ');
                previous.text.push_str(&e.text);
                if let (Some(a), Some(b)) = (previous.bbox.as_mut(), e.bbox) {
                    a.x0 = a.x0.min(b.x0);
                    a.y0 = a.y0.min(b.y0);
                    a.x1 = a.x1.max(b.x1);
                    a.y1 = a.y1.max(b.y1);
                }
                continue;
            }
        }
        out.push(e);
    }
    *elements = out;
}

fn overlap_area(a: &crate::geom::BBox, b: &crate::geom::BBox) -> f64 {
    let ix = (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0);
    let iy = (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0);
    ix * iy
}

/// Split a block where a layout-detector title box covers a contiguous prefix
/// or suffix of its lines but not all of them. See the call site for why.
fn split_block_at_titles(b: Block, boxes: &[crate::geom::BBox]) -> Vec<Block> {
    if b.lines.len() < 2 {
        return vec![b];
    }
    for title in boxes {
        let inside: Vec<bool> = b
            .lines
            .iter()
            .map(|l| {
                let (cx, cy) = ((l.bbox.x0 + l.bbox.x1) * 0.5, (l.bbox.y0 + l.bbox.y1) * 0.5);
                cx >= title.x0 && cx <= title.x1 && cy >= title.y0 && cy <= title.y1
            })
            .collect();
        let n_in = inside.iter().filter(|x| **x).count();
        if n_in == 0 || n_in == b.lines.len() {
            continue;
        }
        // Contiguous run only — a box straddling scattered lines is noise.
        let first = inside.iter().position(|x| *x).unwrap();
        let last = inside.iter().rposition(|x| *x).unwrap();
        if last - first + 1 != n_in {
            continue;
        }
        let marker = b.marker.clone();
        let mut parts = Vec::new();
        let mut lines = b.lines;
        let tail: Vec<Line> = lines.split_off(last + 1);
        let mid: Vec<Line> = lines.split_off(first);
        if !lines.is_empty() {
            parts.push(block::build(lines));
        }
        let mut title = block::build(mid);
        title.marker = marker;
        parts.push(title);
        if !tail.is_empty() {
            // The remainder may contain further titles.
            parts.extend(split_block_at_titles(block::build(tail), boxes));
        }
        return parts;
    }
    vec![b]
}

/// Signals for how much a detected grid can be trusted.
///
/// * occupancy — fraction of cells holding text; misdetected grids are
///   hole-ridden.
/// * vertical / horizontal rule support — how many drawn rules run inside the
///   grid's box. Alignment-inferred tables have none; their column boundaries
///   are guesses.
fn table_confidence_signals(
    g: &Grid,
    glyphs: &[Glyph],
    rules: &[crate::rule::Rule],
) -> (f64, usize, usize) {
    let cells = g.cell_texts(glyphs);
    let occupied = cells.iter().filter(|c| !c.trim().is_empty()).count();
    let occ = occupied as f64 / cells.len().max(1) as f64;
    let inside = |r: &&crate::rule::Rule| {
        r.bbox.x0 >= g.bbox.x0 - 3.0
            && r.bbox.x1 <= g.bbox.x1 + 3.0
            && r.bbox.y0 >= g.bbox.y0 - 3.0
            && r.bbox.y1 <= g.bbox.y1 + 3.0
    };
    let vsup = rules
        .iter()
        .filter(inside)
        .filter(|r| matches!(r.orientation, crate::rule::Orientation::Vertical))
        .count();
    let hsup = rules
        .iter()
        .filter(inside)
        .filter(|r| matches!(r.orientation, crate::rule::Orientation::Horizontal))
        .count();
    (occ, vsup, hsup)
}

/// Multi-column tables of contents are row-oriented: each title and its page
/// number form a visual row, so column-major order separates labels from their
/// numbers. Ordinary prose columns remain independent reading flows.
fn is_contents_layout(columns: &[Vec<Line>]) -> bool {
    columns.len() > 1
        && columns.iter().flatten().any(|line| {
            let normalized = line.text.trim().trim_end_matches(':').to_lowercase();
            matches!(normalized.as_str(), "contents" | "table of contents")
        })
}

/// Prefer independently corroborated alignment hypotheses when one coarse
/// ruled grid has swallowed several much wider tables. The column advantage
/// and prose-heavy ruled cell make this deliberately asymmetric: ordinary
/// ruled tables remain authoritative.
fn prefer_aligned_over_ruled(ruled: &[Grid], aligned: &[Grid], glyphs: &[Glyph]) -> bool {
    if ruled.len() != 1 || aligned.is_empty() {
        return false;
    }
    let ruled_grid = &ruled[0];
    let widest_aligned = aligned.iter().map(Grid::cols).max().unwrap_or(0);
    if widest_aligned < ruled_grid.cols() + 3 || aligned.iter().any(|grid| grid.rows() < 3) {
        return false;
    }
    let coarse = ruled_grid.cell_texts(glyphs).iter().any(|cell| {
        cell.split_whitespace().count() > 25
            || cell
                .split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .any(|words| {
                    words[0].eq_ignore_ascii_case("table")
                        && words[1].chars().any(|c| c.is_ascii_digit())
                })
    });
    let aligned_cells: usize = aligned.iter().map(|grid| grid.rows() * grid.cols()).sum();
    coarse || aligned_cells >= ruled_grid.rows() * ruled_grid.cols() * 3
}

/// Prefer a reading from horizontal bands over one coarse ruled grid whose
/// cells hold whole tables of prose: a page ruled into a frame, a table set
/// in one of its columns.
///
/// Not a reading whose first column is nothing but list markers. A table
/// whose cells hold lettered lists reads, cell by cell, as `a) | Change the
/// pump frit`, `b) | Flush the loop`: a list, not the table, and taking it
/// for one pulled the labels beside the lists out into a table of their own.
fn prefer_horizontal_bands(ruled: &[Grid], bands: &[Grid], glyphs: &[Glyph]) -> bool {
    if ruled.len() != 1 || bands.is_empty() {
        return false;
    }
    let ruled_grid = &ruled[0];
    let table_not_list = |band: &Grid| !is_marker_list(&band.cell_texts(glyphs), band.cols());
    let coarse = ruled_grid.cell_texts(glyphs).iter().any(|cell| {
        cell.split_whitespace().count() > 25
            || cell
                .split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .any(|words| {
                    words[0].eq_ignore_ascii_case("table")
                        && words[1].chars().any(|c| c.is_ascii_digit())
                })
    });
    coarse
        && bands.iter().any(|band| {
            table_not_list(band)
                && (band.cols() >= ruled_grid.cols() + 3
                    || (ruled_grid.cols() <= 2
                        && band.cols() == ruled_grid.cols()
                        && band.rows() >= ruled_grid.rows() + 2))
        })
}

/// A grid's first column holds nothing but list markers (`a)`, `2.`, `(iii)`,
/// `•`), at least two of them: an enumerated list read as a table.
fn is_marker_list(cells: &[String], cols: usize) -> bool {
    if cols < 2 {
        return false;
    }
    let firsts: Vec<&str> = cells
        .iter()
        .step_by(cols)
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    let marker = |t: &str| {
        let body = t.trim_start_matches('(').trim_end_matches(['.', ')']);
        matches!(t, "•" | "◦" | "▪" | "-" | "–" | "*")
            || (t != body
                && (1..=4).contains(&body.chars().count())
                && body.chars().all(|c| c.is_ascii_alphanumeric()))
    };
    firsts.len() >= 2 && firsts.iter().all(|t| marker(t))
}

/// Run the pipeline over an already-extracted document.
/// Configuration for [`analyze`]. The library API: no environment variables.
/// (The `fdoc` CLI maps its `FDOC_*` variables onto this struct.)
#[derive(Debug, Default, Clone)]
pub struct AnalyzeOptions {
    /// Path prefix for layout-detector sidecars: `<prefix>_p<N>_page.json`.
    /// Enables heading promotion/demotion, block splitting at title boxes,
    /// and table-region arbitration.
    pub layout_prefix: Option<std::path::PathBuf>,
    /// Emit `[[VLM…]]` splice anchors for the model tiers.
    pub emit_anchors: bool,
    /// Also emit insert anchors for tables the layout detector found that no
    /// grid covers. Off by default: on the benchmark corpus these insertions
    /// measured net-negative (ground truth rarely transcribes them), but a
    /// completeness-first deployment may prefer them.
    pub insert_missing_tables: bool,
    /// Print table-confidence signals to stderr (diagnostic).
    pub table_conf_debug: bool,
}

/// Drawn grids a document may hold before a reading per table stops being
/// earned; see the table-confidence anchors in [`analyze_with`].
const TRUSTED_RULING_TABLES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GridSource {
    Ruled,
    FillBand,
    Aligned,
    HorizontalBand,
    /// Read from its text by `textgrid`, with its structure stated.
    Text,
}

struct PageLayout {
    grids: Vec<Grid>,
    grid_sources: Vec<GridSource>,
    /// Per grid, rows whose text crosses a column boundary read from
    /// alignment rather than drawn; see [`Grid::split_by_alignment`].
    spanning_rows: Vec<Vec<usize>>,
    prose_columns: Vec<Vec<Line>>,
    route: crate::route::Route,
    missing_tables: Vec<crate::geom::BBox>,
    demoted_tables: Vec<bool>,
}

/// Where the time went, per pipeline stage.
///
/// Wall clock inside `analyze_with`, accumulated across pages. Extraction is
/// not here because it happens before this function; `fdoc dev timings` times
/// it around the call and reports both together.
///
/// This exists because the evaluation harness cannot answer the question. It
/// spawns one process per document, so startup and page cache dominate a
/// corpus that parses in under nine milliseconds a document — a per-page
/// saving disappears into the noise, and a change that removed real work
/// measured *slower* than the code it replaced. Stage totals from one process
/// are the only honest way to see which stage is worth attention.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StageTimings {
    pub dedup: std::time::Duration,
    pub routing: std::time::Duration,
    pub tables: std::time::Duration,
    pub columns: std::time::Duration,
    pub furniture: std::time::Duration,
    pub blocks: std::time::Duration,
    pub headings: std::time::Duration,
}

impl StageTimings {
    /// Every stage, longest first, for reporting.
    pub fn ranked(&self) -> Vec<(&'static str, std::time::Duration)> {
        let mut v = vec![
            ("dedup", self.dedup),
            ("routing", self.routing),
            ("tables", self.tables),
            ("columns", self.columns),
            ("furniture", self.furniture),
            ("blocks", self.blocks),
            ("headings", self.headings),
        ];
        v.sort_by_key(|(_, d)| std::cmp::Reverse(*d));
        v
    }

    /// Total of the measured stages. Less than the whole call: emission,
    /// interleaving and the sidecar reads are not attributed to a stage.
    pub fn measured(&self) -> std::time::Duration {
        self.ranked().iter().map(|(_, d)| *d).sum()
    }

    pub fn add(&mut self, other: &StageTimings) {
        self.dedup += other.dedup;
        self.routing += other.routing;
        self.tables += other.tables;
        self.columns += other.columns;
        self.furniture += other.furniture;
        self.blocks += other.blocks;
        self.headings += other.headings;
    }
}

/// Run `f`, adding its wall clock to `slot`.
fn timed<T>(slot: &mut std::time::Duration, f: impl FnOnce() -> T) -> T {
    let start = std::time::Instant::now();
    let out = f();
    *slot += start.elapsed();
    out
}

/// Does anything downstream read the routing verdict?
///
/// Only two things do: layout-table arbitration, which needs the sidecars,
/// and the splice anchors, which are env-gated. A pure deterministic run
/// reads neither, and [`crate::route::decide`] is not free — it scans every
/// glyph, rasterises image coverage onto a grid, and tests glyph containment
/// once per candidate region. Computing a verdict nobody consumes is the
/// clearest kind of waste, even where the corpus is too noisy to time it.
fn routing_is_consulted(opts: &AnalyzeOptions) -> bool {
    opts.emit_anchors || opts.layout_prefix.is_some()
}

fn glyphs_outside_grids(glyphs: &[Glyph], grids: &[Grid]) -> Vec<Glyph> {
    glyphs
        .iter()
        .filter(|g| {
            let (x, y) = match g.bbox {
                Some(b) => ((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5),
                None => g.origin,
            };
            !grids.iter().any(|grid| match grid.layout.as_deref() {
                Some(lay) if !lay.claimed.is_empty() => {
                    lay.claimed.binary_search(&g.draw_index).is_ok()
                }
                _ => grid.cell_at(x, y).is_some(),
            })
        })
        .cloned()
        .collect()
}

pub fn analyze(raw: &mut RawDoc, outline: &[OutlineItem]) -> Analysis {
    analyze_with(raw, outline, &AnalyzeOptions::default())
}

pub fn analyze_with(raw: &mut RawDoc, outline: &[OutlineItem], opts: &AnalyzeOptions) -> Analysis {
    let mut timings = StageTimings::default();
    let mut suspect_tables: Vec<table::SuspectTable> = Vec::new();
    let mut layouts: Vec<PageLayout> = Vec::new();
    let mut flat_lines: Vec<(Vec<Line>, f64)> = Vec::new();

    // Glyphs drawn twice first: a copy laid on its own original is not a
    // watermark laid over text.
    for p in raw.pages.iter_mut() {
        timed(&mut timings.dedup, || {
            dedup::remove_faux_bold(&mut p.glyphs, 8)
        });
    }
    // A watermark laid over the text, before it is read into the lines it
    // crosses.
    timed(&mut timings.furniture, || {
        crate::watermark::strip_overlaid(&mut raw.pages)
    });
    for p in raw.pages.iter_mut() {
        // Before anything reads the geometry: a designer's baseline grid is
        // drawn with the same primitive a table's ruling is, and every
        // consumer of `rules` — grids, column rulers, row banding — would
        // otherwise take it for structure.
        crate::rule::strip_layout_lattice(&mut p.rules, p.width, p.height);
        crate::rule::strip_page_decoration(&mut p.rules, &mut p.fills, p.width, p.height);
        // A table a line printer drew with `|` and `_` is ruled like any
        // other once its drawing is read as rules; its underlines, struck
        // over the words, are not text.
        let char_frames = timed(&mut timings.dedup, || {
            let frames = crate::textart::draw_rules(&mut p.glyphs, &mut p.rules, p.index);
            dedup::remove_struck_underlines(&mut p.glyphs);
            frames
        });
        // A reviewer's highlighting is drawn as fills fitted to the lines it
        // marks, and is not the page's structure.
        crate::rule::strip_highlights(&mut p.fills, &p.glyphs);
        // `Deterministic` is the right stand-in when nobody asks: both
        // consumers act only on `VlmRegions`, and both are unreachable in the
        // configurations that skip the call.
        let route = timed(&mut timings.routing, || {
            if routing_is_consulted(opts) {
                crate::route::decide(p).0
            } else {
                crate::route::Route::Deterministic
            }
        });

        // Tables first: a grid's glyphs must not also become paragraphs.
        // Two readings of the page's ruling, judged after trimming: a
        // page-wide grid can look richer before trim and collapse to a
        // handful of cells after it, which is exactly what happens on a form
        // of separate labelled boxes. Whichever survives trimming with more
        // populated cells is the one that explains the page.
        let trim = |mut gs: Vec<Grid>| -> Vec<Grid> {
            for g in gs.iter_mut() {
                g.trim_to_content(&p.glyphs);
            }
            gs.retain(|g| g.rows() >= 2 && g.cols() >= 1);
            gs
        };
        let filled = |gs: &[Grid]| -> usize {
            gs.iter()
                .map(|g| {
                    g.cell_texts(&p.glyphs)
                        .iter()
                        .filter(|c| !c.trim().is_empty())
                        .count()
                })
                .sum()
        };
        let mut grids = timed(&mut timings.tables, || {
            trim(table::detect_framed(&p.rules, p.index, &char_frames))
        });
        let mut grid_sources = vec![GridSource::Ruled; grids.len()];
        if !grids.is_empty() {
            let parts = timed(&mut timings.tables, || {
                trim(table::detect_by_component_framed(
                    &p.rules,
                    p.index,
                    &char_frames,
                ))
            });
            // Comfortably more, so a tie — the same table found either way —
            // keeps the page-wide reading.
            if filled(&parts) > filled(&grids) * 2 {
                grids = parts;
                grid_sources = vec![GridSource::Ruled; grids.len()];
            }
        }

        if grids.is_empty() {
            let mut fill_grids = timed(&mut timings.tables, || {
                table::detect_fill_bands(&p.fills, p.index)
            });
            // Half of the grid's cells hold text, counting a merged cell once:
            // fill edges cut a band into more columns than it has, and a
            // label run across two of them is one cell, not one empty and
            // one full, nor two full.
            fill_grids.retain(|grid| {
                let cells = grid.cell_texts(&p.glyphs);
                let m = grid.merges(&p.rules, &p.fills);
                let cols = grid.cols();
                let owner = |mut i: usize| {
                    while m.continues_left[i] || m.continues_above[i] {
                        i -= if m.continues_left[i] { 1 } else { cols };
                    }
                    i
                };
                let mut held = vec![false; cells.len()];
                for (i, cell) in cells.iter().enumerate() {
                    if !cell.trim().is_empty() {
                        held[owner(i)] = true;
                    }
                }
                let slots = (0..cells.len()).filter(|&i| owner(i) == i).count();
                let occupied = (0..cells.len())
                    .filter(|&i| owner(i) == i && held[i])
                    .count();
                occupied * 2 >= slots
            });
            grids = fill_grids;
            grid_sources = vec![GridSource::FillBand; grids.len()];
        }

        let aligned_full = timed(&mut timings.tables, || {
            table::detect_aligned(&p.glyphs, &p.rules, &p.fills, p.index)
        });
        let aligned_preferred = prefer_aligned_over_ruled(&grids, &aligned_full, &p.glyphs);
        // Tables ruled with horizontals only, or by nothing but the rules
        // under their figures, are read from their text: their bands are
        // not rows and their rules' ends are not columns. See `textgrid`.
        // Where alignment already reads the page's one coarse grid, it
        // keeps it.
        if !aligned_preferred {
            timed(&mut timings.tables, || {
                let lines = crate::textgrid::lines(&p.glyphs);
                let mut read: Vec<Grid> = Vec::new();
                let mut seeded: Vec<usize> = Vec::new();
                let debug = std::env::var_os("FDOC_TEXTGRID_DEBUG").is_some();
                for i in crate::textgrid::unruled_grids(&grids, &p.rules, &lines) {
                    for region in crate::textgrid::rule_regions(&p.rules, &grids[i].bbox) {
                        let got = crate::textgrid::read(&lines, &p.rules, region, p.index);
                        if debug {
                            eprintln!(
                                "TEXTGRID p{} seed grid {i} region {:?} -> {:?}",
                                p.index,
                                region,
                                got.iter()
                                    .map(|g| (g.rows(), g.cols(), g.bbox))
                                    .collect::<Vec<_>>()
                            );
                        }
                        if got.is_empty() {
                            continue;
                        }
                        if !seeded.contains(&i) {
                            seeded.push(i);
                        }
                        read.extend(got);
                    }
                }
                // Runs of figures no grid holds: a statement ruled by
                // nothing but the underlines under its figures. Not where
                // alignment finds a layout its type names (a transposed
                // table, a field list), which it reads better.
                let mut taken: Vec<crate::geom::BBox> = grids.iter().map(|g| g.bbox).collect();
                taken.extend(read.iter().map(|g| g.bbox));
                let outside = glyphs_outside_grids(&p.glyphs, &grids);
                let accepted = table::detect_aligned(&outside, &p.rules, &p.fills, p.index);
                let typed: Vec<crate::geom::BBox> = accepted
                    .iter()
                    .filter(|g| table::typed_layout(g, &outside))
                    .map(|g| g.bbox)
                    .collect();
                for region in crate::textgrid::figure_runs(&lines, &taken) {
                    if typed.iter().any(|t| overlap_area(t, &region) > 0.0) {
                        continue;
                    }
                    let got = crate::textgrid::read(&lines, &p.rules, region, p.index);
                    if debug {
                        eprintln!(
                            "TEXTGRID p{} figure run {:?} -> {:?}",
                            p.index,
                            region,
                            got.iter()
                                .map(|g| (g.rows(), g.cols(), g.bbox))
                                .collect::<Vec<_>>()
                        );
                    }
                    // Nor where alignment, accepting a table there, finds
                    // more columns than the gutters do: columns set closer
                    // than a gutter's width.
                    read.extend(got.into_iter().filter(|g| {
                        !typed.iter().any(|t| overlap_area(t, &g.bbox) > 0.0)
                            && !accepted.iter().any(|a| {
                                overlap_area(&a.bbox, &g.bbox) > 0.0 && {
                                    // Its columns that hold more than a
                                    // currency sign: alignment sets `$` apart.
                                    let cells = a.cell_texts(&outside);
                                    let n = a.cols();
                                    let real = (0..n)
                                        .filter(|&c| {
                                            (0..a.rows()).any(|r| {
                                                let t = cells[r * n + c].trim();
                                                !t.is_empty() && !matches!(t, "$" | "€" | "£" | "¥")
                                            })
                                        })
                                        .count();
                                    real > g.cols()
                                }
                            })
                    }));
                }
                // One table found from two seeds is kept once, the larger.
                read.sort_by_key(|g| std::cmp::Reverse(g.rows() * g.cols()));
                let mut kept: Vec<Grid> = Vec::new();
                for g in read {
                    if !kept.iter().any(|k| overlap_area(&k.bbox, &g.bbox) > 0.0) {
                        kept.push(g);
                    }
                }
                if kept.is_empty() {
                    return;
                }
                let mut out_grids = Vec::new();
                let mut out_sources = Vec::new();
                for (gi, (g, s)) in grids.drain(..).zip(grid_sources.drain(..)).enumerate() {
                    let area = ((g.bbox.x1 - g.bbox.x0) * (g.bbox.y1 - g.bbox.y0)).max(1.0);
                    // A seed that was read is replaced by its reading; another
                    // grid only where a reading covers much of it.
                    let replaced = seeded.contains(&gi)
                        || (!crate::textgrid::has_vertical(&p.rules, &g.bbox)
                            && kept
                                .iter()
                                .any(|k| overlap_area(&k.bbox, &g.bbox) >= 0.3 * area));
                    if !replaced {
                        out_grids.push(g);
                        out_sources.push(s);
                    }
                }
                for mut g in kept {
                    crate::textgrid::claim(&mut g, &p.glyphs);
                    if !out_grids
                        .iter()
                        .any(|o: &Grid| overlap_area(&o.bbox, &g.bbox) > 0.0)
                    {
                        out_grids.push(g);
                        out_sources.push(GridSource::Text);
                    }
                }
                grids = out_grids;
                grid_sources = out_sources;
            });
        }
        let read_from_text = grid_sources.contains(&GridSource::Text);

        if read_from_text {
        } else if aligned_preferred {
            grids = aligned_full;
            grid_sources = vec![GridSource::Aligned; grids.len()];
        } else {
            let horizontal_bands = timed(&mut timings.tables, || {
                table::detect_horizontal_bands(&p.glyphs, &p.rules, p.index)
            });
            if prefer_horizontal_bands(&grids, &horizontal_bands, &p.glyphs) {
                grids = horizontal_bands;
                grid_sources = vec![GridSource::HorizontalBand; grids.len()];
            }
        }

        // The labels a ruled table sets over the rules that group its
        // columns are its top row. Taken in once the readings above are
        // chosen: to them the rules under the labels are the table's top
        // edge, not a boundary between its rows ruled short.
        for (g, source) in grids.iter_mut().zip(&grid_sources) {
            if *source == GridSource::Ruled {
                g.take_group_labels(&p.rules, &p.glyphs);
            }
        }

        let prose = timed(&mut timings.columns, || {
            glyphs_outside_grids(&p.glyphs, &grids)
        });

        let mut cols = timed(&mut timings.columns, || {
            line::assemble_columns_with_rules(&prose, &p.rules, p.index)
        });

        // Second table strategy: alignment, for the 14 corpus documents whose
        // tables have no vertical rules at all and so are invisible to the
        // ruled detector. Gated on corroborating drawn geometry — see
        // `table::is_corroborated`. An earlier ungated version was
        // net-negative: it converted prose into tables and cost more in reading
        // order (NID 0.868 -> 0.839) than it gained in table structure.
        let mut aligned = timed(&mut timings.tables, || {
            table::detect_aligned(&prose, &p.rules, &p.fills, p.index)
        });
        aligned.retain(|g| g.rows() >= 2 && g.cols() >= 2);
        if !aligned.is_empty() {
            // Withhold their glyphs from prose too, and re-assemble what is left.
            let remaining = glyphs_outside_grids(&prose, &aligned);
            cols = timed(&mut timings.columns, || {
                line::assemble_columns_with_rules(&remaining, &p.rules, p.index)
            });
            grid_sources.extend(std::iter::repeat_n(GridSource::Aligned, aligned.len()));
            grids.extend(aligned);
        }

        // A contents page is a list, not a table. Right-aligned page numbers
        // form a perfectly aligned numeric column, so the alignment detectors
        // read every ToC as a two-column grid — emitting `|Executive
        // Summary|4|` rows against ground truth's plain lines, with the
        // column cut even splitting page numbers (`…Engagement 1|5|`).
        // The same marker that already suppresses heading detection below it
        // marks the region as entries. Drawn grids are unaffected: only the
        // inference-based detectors are being overruled here, and a genuinely
        // ruled table on a contents page keeps its rules.
        let toc_top = cols
            .iter()
            .flatten()
            .filter(|l| heading::is_toc_marker(&l.text))
            .map(|l| l.bbox.y0)
            .fold(f64::INFINITY, f64::min);
        if toc_top.is_finite() {
            let n = grids.len();
            let mut kept_grids = Vec::with_capacity(grids.len());
            let mut kept_sources = Vec::with_capacity(grid_sources.len());
            for (grid, source) in grids.into_iter().zip(grid_sources) {
                if grid.bbox.y0 < toc_top || source == GridSource::Ruled {
                    kept_grids.push(grid);
                    kept_sources.push(source);
                }
            }
            grids = kept_grids;
            grid_sources = kept_sources;
            if grids.len() != n {
                // The dropped grids' glyphs belong to prose after all.
                let remaining = glyphs_outside_grids(&p.glyphs, &grids);
                cols = timed(&mut timings.columns, || {
                    line::assemble_columns_with_rules(&remaining, &p.rules, p.index)
                });
            }
        }

        // A one-column grid — a key/value list ruled with horizontals only —
        // reads its columns from where its text sits.
        let spanning_rows: Vec<Vec<usize>> = timed(&mut timings.tables, || {
            grids
                .iter_mut()
                .map(|g| g.split_by_alignment(&p.glyphs))
                .collect()
        });

        flat_lines.push((cols.iter().flatten().cloned().collect(), p.height));
        if opts.table_conf_debug {
            for (ti, g) in grids.iter().enumerate() {
                let (occ, vsup, hsup) = table_confidence_signals(g, &p.glyphs, &p.rules);
                eprintln!(
                    "TABLECONF p{} t{ti} {}x{} occ={occ:.2} vrules={vsup} hrules={hsup} ruled={}",
                    p.index,
                    g.rows(),
                    g.cols(),
                    grid_sources[ti] == GridSource::Ruled,
                );
            }
        }
        // Layout arbitration for table *regions*. The three residual TEDS
        // deficits were all region decisions, not readings: a list promoted
        // to a grid, a grid over the wrong content, a table never found. The
        // layout detector boxes tables independently, so it arbitrates here
        // the same way it arbitrates headings:
        //   - an *inferred* grid no layout table overlaps is demoted to prose
        //     (drawn rules still outrank the detector);
        //   - a layout table no grid overlaps becomes an insert anchor for
        //     the VLM tier (`missing_per_page`).
        let mut missing: Vec<crate::geom::BBox> = Vec::new();
        let mut demoted: Vec<bool> = vec![false; grids.len()];
        if let Some(ltables) = layout_boxes(opts.layout_prefix.as_deref(), p.index, &["table"]) {
            // Demotion changes the *kind*, not the text: the grid's row-major
            // reading is kept as a paragraph. Returning the glyphs to prose
            // assembly instead re-ordered them worse than the grid had
            // (NID 0.625 → 0.484 on one such page), because a tabular
            // region's row-major order was already right — it just is not a
            // table.
            for (gi, g) in grids.iter().enumerate() {
                let area = ((g.bbox.x1 - g.bbox.x0) * (g.bbox.y1 - g.bbox.y0)).max(1.0);
                demoted[gi] = grid_sources[gi] != GridSource::Ruled
                    && !ltables
                        .iter()
                        .any(|b| overlap_area(b, &g.bbox) >= 0.2 * area);
            }
            // A layout table already covered by a router region (a raster
            // table the VLM reads via the route-tier splice) must not become
            // an insert anchor too — that emitted the same table twice.
            let routed: Vec<crate::geom::BBox> = match &route {
                crate::route::Route::VlmRegions(r) => r.clone(),
                _ => Vec::new(),
            };
            for b in &ltables {
                let area = ((b.x1 - b.x0) * (b.y1 - b.y0)).max(1.0);
                let covered = grids.iter().any(|g| overlap_area(b, &g.bbox) >= 0.2 * area)
                    || routed.iter().any(|r| overlap_area(b, r) >= 0.2 * area);
                if !covered {
                    missing.push(*b);
                }
            }
        }
        suspect_tables.extend(table::suspect_tables(&grids, &p.glyphs));
        assert_eq!(grids.len(), grid_sources.len());
        assert_eq!(grids.len(), demoted.len());
        layouts.push(PageLayout {
            grids,
            grid_sources,
            spanning_rows,
            prose_columns: cols,
            route,
            missing_tables: missing,
            demoted_tables: demoted,
        });
    }

    let (marks, exact) = timed(&mut timings.furniture, || {
        furniture::detect_with_exact(&flat_lines)
    });
    // The folio is read from the page's edges over *every* glyph, not from
    // the lines left after grids took theirs: a form's key/value grid that
    // runs down into the footer band takes the footer's glyphs with it, and
    // on those pages the page number was never seen at all. The edge bands
    // are where pagination lives, and assembling them again is cheap.
    let folios = timed(&mut timings.furniture, || {
        let edges: Vec<(Vec<Line>, f64)> = raw
            .pages
            .iter()
            .map(|p| {
                let band = p.height * furniture::EDGE_BAND;
                let edge: Vec<Glyph> = p
                    .glyphs
                    .iter()
                    .filter(|g| g.origin.1 < band || g.origin.1 > p.height - band)
                    .cloned()
                    .collect();
                (line::assemble_page(&edge), p.height)
            })
            .collect();
        furniture::detect_with_folios(&edges).1
    });
    let mut furniture_marks: Vec<(usize, String, furniture::Furniture)> = Vec::new();
    for (pi, m) in marks.iter().enumerate() {
        for (li, kind) in m {
            furniture_marks.push((pi, flat_lines[pi].0[*li].text.clone(), *kind));
        }
    }
    let mut running: Vec<furniture::Running> = Vec::new();
    for (pi, m) in marks.iter().enumerate() {
        let (lines, height) = &flat_lines[pi];
        let mut at: Vec<usize> = m
            .iter()
            .filter(|(li, kind)| furniture::identifies(&lines[**li].text, **kind))
            .map(|(li, _)| *li)
            .collect();
        at.sort_unstable();
        for li in at {
            let l = &lines[li];
            running.push(furniture::Running {
                page: pi,
                bbox: l.bbox,
                text: l.text.clone(),
                top: (l.bbox.y0 + l.bbox.y1) / 2.0 < height / 2.0,
            });
        }
    }

    // Furniture texts for cell scrubbing: grids capture their glyphs before
    // the cross-page furniture pass runs, so a footer crossing a table region
    // leaks into cells unless removed here. Digit-varying furniture (page
    // numbers) scrubs digit-insensitively.
    let furniture_texts: Vec<(String, bool)> = {
        let mut seen = std::collections::HashSet::new();
        let mut v = Vec::new();
        for (pi, page_marks) in marks.iter().enumerate() {
            for (li, kind) in page_marks {
                let text = flat_lines[pi].0[*li].text.trim().to_string();
                let digits_vary = matches!(kind, furniture::Furniture::PageNumber);
                if !text.is_empty() && seen.insert(furniture_key(&text, digits_vary)) {
                    v.push((text, digits_vary));
                }
            }
        }
        v
    };
    // The texts scrubbed from cells and blocks: those read by repetition.
    // A short document's exact copies were each found as a line of their
    // own, and the same words elsewhere are content (see
    // `furniture::detect_with_exact`).
    let scrub_texts: Vec<(String, bool)> = {
        let repeated: std::collections::HashSet<String> = marks
            .iter()
            .enumerate()
            .flat_map(|(pi, page_marks)| {
                let exact = &exact[pi];
                let lines = &flat_lines[pi].0;
                page_marks
                    .iter()
                    .filter(move |(li, _)| !exact.contains(li))
                    .map(move |(li, kind)| {
                        furniture_key(
                            lines[*li].text.trim(),
                            matches!(kind, furniture::Furniture::PageNumber),
                        )
                    })
            })
            .collect();
        furniture_texts
            .iter()
            .filter(|(t, vary)| repeated.contains(&furniture_key(t, *vary)))
            .cloned()
            .collect()
    };
    // The sizes each furniture text is printed at, to tell a running head
    // from a title page that sets the same words as its title.
    let mut furniture_sizes: std::collections::HashMap<String, Vec<f32>> = Default::default();
    for (pi, page_marks) in marks.iter().enumerate() {
        for li in page_marks.keys() {
            let l = &flat_lines[pi].0[*li];
            furniture_sizes
                .entry(l.text.trim().to_string())
                .or_default()
                .push(l.font_size);
        }
    }
    let bare: Vec<Vec<Line>> = flat_lines.iter().map(|(l, _)| l.clone()).collect();
    let leading = block::modal_leading(&bare);

    let mut blocks_per_page: Vec<Vec<Block>> = Vec::new();
    let mut furniture_removed = 0usize;
    for (pi, layout) in layouts.iter().enumerate() {
        let cols = &layout.prose_columns;
        furniture_removed += marks[pi].len();
        let mut idx = 0usize;
        let mut kept_columns = Vec::new();
        for col in cols {
            let kept: Vec<Line> = col
                .iter()
                .enumerate()
                .filter(|(k, _)| !marks[pi].contains_key(&(idx + k)))
                .map(|(_, l)| l.clone())
                .collect();
            idx += col.len();
            kept_columns.push(kept);
        }
        let contents_layout = is_contents_layout(&kept_columns);
        let checkboxes = crate::rule::checkboxes(&raw.pages[pi].fills);
        let bands: Vec<crate::geom::BBox> = raw.pages[pi].fills.iter().map(|f| f.bbox).collect();
        let block_columns: Vec<Vec<Block>> = timed(&mut timings.blocks, || {
            kept_columns
                .iter()
                .map(|col| block::assemble_with_marks(col, leading, &checkboxes, &bands))
                .collect()
        });
        let mut out: Vec<Block> = block_columns.into_iter().flatten().collect();
        if contents_layout {
            out.sort_by(|a, b| {
                a.bbox
                    .y0
                    .partial_cmp(&b.bbox.y0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        a.bbox
                            .x0
                            .partial_cmp(&b.bbox.x0)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            });
            // Strict (y, x) has the same jitter failure here as in line
            // assembly: a page number's top differs from its label's by
            // hundredths ("6" sorted ahead of "Legal Framework"), and y then
            // decides before x is consulted. Within a row, left comes first.
            let mut start = 0;
            for i in 1..=out.len() {
                if i < out.len() && {
                    let (a, b) = (&out[start], &out[i]);
                    let tol = a.font_size.max(b.font_size).max(1.0) as f64 * 0.3;
                    (b.bbox.y0 - a.bbox.y0).abs() < tol
                } {
                    continue;
                }
                out[start..i].sort_by(|a, b| {
                    a.bbox
                        .x0
                        .partial_cmp(&b.bbox.x0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                start = i;
            }
        }
        out = out
            .into_iter()
            .flat_map(block::split_structural_prefix)
            .collect();
        // Layout-detector arbitration, part 1: force block boundaries at
        // title boxes. A single-word heading set at body size with no weight
        // merges into the paragraph below it — no deterministic signal exists
        // to stop that — and once merged, the promotion pass in the CLI can
        // never mark it (a box cannot cover half a block). Splitting here is
        // still promotion-only in effect: text and order are untouched.
        if let Some(boxes) = layout_boxes(
            opts.layout_prefix.as_deref(),
            pi,
            &["paragraph_title", "doc_title", "title"],
        ) {
            if !boxes.is_empty() {
                out = out
                    .into_iter()
                    .flat_map(|b| split_block_at_titles(b, &boxes))
                    .collect();
            }
        }
        blocks_per_page.push(out);
    }

    let headings = timed(&mut timings.headings, || {
        let first = heading::detect(&blocks_per_page, outline);
        let named = heading::outline_names(outline);
        let in_section = heading::in_section(&first);
        if block::gather_counted_items(&mut blocks_per_page, &|t| named(t), &in_section) {
            heading::detect(&blocks_per_page, outline)
        } else {
            first
        }
    });
    let body_font = timed(&mut timings.headings, || {
        heading::body_font_size(&blocks_per_page)
    });

    // Heading lookup by source block. Text is not an identity: repeated chart
    // labels and form fields can have identical text on one page while only
    // one occurrence is structurally a heading.
    let mut is_heading: std::collections::HashMap<(usize, usize), (usize, Evidence)> =
        Default::default();
    for h in &headings {
        is_heading.insert((h.page, h.block_index), (h.level, h.evidence));
    }

    let mut elements = Vec::new();
    let ruled_tables = layouts
        .iter()
        .flat_map(|l| l.grid_sources.iter())
        .filter(|s| **s == GridSource::Ruled)
        .count();

    let mut n = 0usize;
    let mut tables = 0usize;
    // Each table's column edges and whether it opens with a styled row,
    // for joining a table the page break split.
    let mut frames: Vec<TableFrame> = Vec::new();
    for (pi, blocks) in blocks_per_page.iter().enumerate() {
        let mut page_elements = Vec::new();
        // Chart and diagram regions. Text inside one is real and its *order*
        // is not: a donut's labels and percentages interleave by position on
        // the page, so emitting them as consecutive paragraphs invites a
        // reader to pair them, and half of those pairings are wrong. They are
        // gathered into one figure instead, which says what is known (these
        // fragments belong together, here) without asserting what is not.
        let page = &raw.pages[pi];
        let mut figures = figure::detect(&page.fills, &page.rules, pi, (page.width, page.height));
        // The drawing does not contain its own labels; grow each region to
        // reach them before deciding which blocks belong to it.
        let block_texts: Vec<String> = blocks.iter().map(|b| b.text()).collect();
        let block_boxes: Vec<(crate::geom::BBox, f32, &str)> = blocks
            .iter()
            .zip(&block_texts)
            .map(|(b, t)| (b.bbox, b.font_size, t.as_str()))
            .collect();
        let images: Vec<crate::geom::BBox> = page.images.iter().map(|im| im.bbox).collect();
        figure::attach(
            &mut figures,
            &block_boxes,
            &images,
            (page.width, page.height),
            prose_size(blocks),
        );
        // Which figure each block falls in, if any. A block counts as inside
        // when most of it is: a caption sitting just below the drawing is
        // part of the figure, a paragraph merely overlapping its margin is
        // not.
        //
        // Region and heading evidence arbitrate rather than one always
        // winning. Outline/title/numbering evidence is authoritative enough
        // to survive an over-grown figure region. Relative typography is not:
        // chart legends and axis labels are commonly bold or larger than body
        // text, and treating every one as a section is the dominant
        // over-promotion failure on the benchmark.
        let in_figure = |b: &Block| -> Option<usize> {
            figures.iter().position(|f| {
                let bx = b.bbox;
                let w = (bx.x1.min(f.bbox.x1) - bx.x0.max(f.bbox.x0)).max(0.0);
                let h = (bx.y1.min(f.bbox.y1) - bx.y0.max(f.bbox.y0)).max(0.0);
                let area = (bx.width() * bx.height()).max(1.0);
                w * h / area >= FIGURE_CONTAINMENT
            })
        };
        for (block_idx, b) in blocks.iter().enumerate() {
            let text = b.text();
            let heading = is_heading.get(&(pi, block_idx));
            let figure_idx = in_figure(b);
            let weak_heading_in_figure =
                heading.is_some_and(|(_, ev)| heading_yields_to_figure(*ev, figure_idx.is_some()));
            let figure_of = figure_idx
                .filter(|_| heading.is_none() || weak_heading_in_figure)
                .map(|fi| format!("figure-{pi}-{fi}"));
            let (kind, level, evidence) = match heading {
                Some((l, ev)) if !weak_heading_in_figure => (
                    "doco:SectionTitle",
                    Some(*l),
                    match ev {
                        Evidence::Outline => "outline",
                        Evidence::Title => "title",
                        Evidence::Numbering => "numbering",
                        Evidence::Sequence => "sequence",
                        Evidence::FontSize => "font-size",
                        Evidence::Bold => "bold",
                    },
                ),
                _ if figure_of.is_some() => ("doco:Figure", None, "fills"),
                None if b.marker.is_some() => ("doc:ListItem", None, "marker"),
                // A bullet the producer set inside the text run rather than
                // beside it. The marker pass cannot pull out what was never
                // separate, so the list read as paragraphs opening with a
                // square.
                None if block::strip_leading_bullet(&text).is_some() => {
                    ("doc:ListItem", None, "bullet")
                }
                None => ("doco:Paragraph", None, "layout"),
                Some(_) => unreachable!("weak figure heading has figure membership"),
            };
            // A footer that merged into a unique line on one page escapes
            // the cross-page furniture match (singletons cannot repeat);
            // known furniture texts are stripped as substrings instead,
            // from a block that reaches the band headers and footers are
            // set in. A block in the middle of the page holds none.
            //
            // A block that is all of a running line, at another size, is
            // no leak: it is a title page setting the words the later
            // pages run over their heads (`Guild® | K-R33912` large on the
            // first page, small at the top of the rest).
            let height = raw.pages[pi].height;
            let band = height * furniture::EDGE_BAND;
            let retitled = furniture_sizes.get(text.trim()).is_some_and(|sizes| {
                sizes
                    .iter()
                    .all(|s| (b.font_size - s).abs() > 0.15 * s.max(b.font_size))
            });
            let scrubbed = if retitled {
                text.clone()
            } else if height <= 0.0 || b.bbox.y0 < band || b.bbox.y1 > height - band {
                furniture::scrub_block(&text, &scrub_texts)
            } else {
                text.clone()
            };
            // A block composed of furniture joined by punctuation ("Title:
            // Subtitle" as a one-off page header) scrubs down to the joiner
            // alone; carrying that residue forward can even leave a bare ":"
            // wearing a heading tag. Content-free residue only exists where
            // scrubbing removed something, so unscrubbed punctuation-only
            // blocks (a "* * *" divider) are untouched.
            let residue = scrubbed != text && !scrubbed.chars().any(char::is_alphanumeric);
            if scrubbed.is_empty() || residue {
                continue;
            }
            let text = match block::strip_leading_bullet(&scrubbed) {
                Some(without) if kind == "doc:ListItem" => without.to_string(),
                _ => scrubbed,
            };
            page_elements.push(Element {
                page: pi,
                bbox: Some(b.bbox),
                level,
                figure: figure_of,
                ..Element::new(kind, text, "rust", evidence)
            });
        }
        coalesce_heading_fragments(&mut page_elements);
        let mut table_elements = Vec::new();
        let layout = &layouts[pi];
        debug_assert_eq!(layout.grid_sources.len(), layout.grids.len());
        for (gi, g) in layout.grids.iter().enumerate() {
            let flat: Vec<String> = match g.layout.as_deref() {
                Some(lay) => lay.texts(&raw.pages[pi].glyphs, g.cols()),
                None => g.cell_texts(&raw.pages[pi].glyphs),
            }
            .into_iter()
            .map(|c| furniture::scrub_cell(&c, &scrub_texts))
            .collect();
            if layout.demoted_tables[gi] {
                // Judged non-table by the layout arbiter: same reading
                // order, prose form.
                let text = (0..g.rows())
                    .map(|r| {
                        (0..g.cols())
                            .map(|c| flat[r * g.cols() + c].trim().to_string())
                            .filter(|t| !t.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                table_elements.push(Element {
                    page: pi,
                    bbox: Some(g.bbox),
                    ..Element::new("doco:Paragraph", text, "rust", "layout-demoted")
                });
                continue;
            }
            let rows: Vec<Vec<String>> = (0..g.rows())
                .map(|r| {
                    (0..g.cols())
                        .map(|c| flat[r * g.cols() + c].clone())
                        .collect()
                })
                .collect();
            let explicit = g.layout.as_deref();
            let mut merges = match explicit {
                Some(lay) => lay.merges(g.rows(), g.cols()),
                None => {
                    let page = &raw.pages[pi];
                    let mut m = g.merges(&page.rules, &page.fills);
                    g.text_merges(&page.glyphs, &page.rules, &mut m);
                    g.group_label_merges(&page.glyphs, &page.rules, &mut m);
                    m
                }
            };
            // A row whose text crosses an alignment-derived boundary is one
            // cell across the row — a boxed notice over a key/value list.
            // Its text is read whole into the first cell, since the cut
            // would otherwise deal its words out by which side they fell.
            let cols = g.cols();
            let mut rows = rows;
            for &r in &layout.spanning_rows[gi] {
                rows[r][0] =
                    furniture::scrub_cell(&g.row_text(&raw.pages[pi].glyphs, r), &scrub_texts);
                for (c, cell) in rows[r].iter_mut().enumerate().skip(1) {
                    cell.clear();
                    merges.continues_left[r * cols + c] = true;
                }
            }
            let mut header_rows = match explicit {
                Some(lay) => {
                    for cell in rows.iter_mut().flatten() {
                        *cell = crate::textgrid::strip_leaders(cell);
                    }
                    lay.header_rows
                }
                None => {
                    let page = &raw.pages[pi];
                    let h = g.header_rows(&rows, &page.glyphs, &page.rules, &page.fills);
                    g.stacked_header_rows(h, &rows, &merges, &page.rules)
                }
            };
            // An unruled table read by alignment has no rules to say a header
            // label spans columns. Its column banner (`EUR` over `Unit` and
            // `Total`, taken in by `extend_by_banner`) is a first row whose
            // labels each leave the cells to their right empty, over a row of
            // labels naming every column: a two-row header, each banner
            // spanning the columns up to the next. A transposed table's empty
            // corner is no banner: its header row names every other column.
            let banner = layout.grid_sources[gi] == GridSource::Aligned
                && rows.len() >= 3
                && rows[0][0].trim().is_empty()
                && (1..cols).any(|c| {
                    rows[0][c].trim().is_empty()
                        && rows[0][..c].iter().any(|t| !t.trim().is_empty())
                })
                && rows[1]
                    .iter()
                    .all(|c| !c.trim().is_empty() && !table::is_numeric_cell(c.trim()));
            if banner {
                header_rows = 2;
                let mut owner = false;
                for (c, label) in rows[0].iter().enumerate() {
                    if !label.trim().is_empty() {
                        owner = true;
                    } else if owner {
                        merges.continues_left[c] = true;
                    }
                }
            }
            // A ruled table says where a header label spans columns: the
            // ruling leaves them open under it (`Primary Method` over two
            // antibodies). A row beneath that names each spanned column with
            // a label of its own is the header's second row, and every
            // column is named by both. Distinct labels, because data under a
            // banner repeats itself (`Short | Short`), and no numbers.
            if explicit.is_none()
                && header_rows == 1
                && names_spanned_columns(&rows, &merges.continues_left)
            {
                header_rows = 2;
            }
            // A merged cell is read whole, into its top-left cell. Its words
            // were dealt out by the grid's cuts through it, which split a
            // centred header across the row boundary under it (`time` above,
            // `Delivery` below) and a band across the columns it spans
            // (`Crucible | Logistics`).
            let rcount = rows.len();
            let mut taken = vec![false; rcount * cols];
            // A table read from its text joined its spans as it read them.
            if explicit.is_some() {
                taken.iter_mut().for_each(|t| *t = true);
            }
            for r in 0..rcount {
                for c in 0..cols {
                    if taken[r * cols + c] {
                        continue;
                    }
                    let mut c_end = c + 1;
                    while c_end < cols && merges.continues_left[r * cols + c_end] {
                        c_end += 1;
                    }
                    let mut r_end = r + 1;
                    while r_end < rcount && merges.continues_above[r_end * cols + c] {
                        r_end += 1;
                    }
                    for i in r..r_end {
                        for k in c..c_end {
                            taken[i * cols + k] = true;
                        }
                    }
                    if r_end - r == 1 && c_end - c == 1 {
                        continue;
                    }
                    // Text the cuts part cleanly is separate values and stays
                    // where it is, unless it is the block's only text: then it
                    // is the merged cell's, and its place is the first cell.
                    let held: Vec<(usize, usize)> = (r..r_end)
                        .flat_map(|i| (c..c_end).map(move |k| (i, k)))
                        .filter(|&(i, k)| !rows[i][k].trim().is_empty())
                        .collect();
                    let text =
                        match g.region_text(&raw.pages[pi].glyphs, &merges, r..r_end, c..c_end) {
                            Some(t) => furniture::scrub_cell(&t, &scrub_texts),
                            None if held.len() == 1 => rows[held[0].0][held[0].1].clone(),
                            None => continue,
                        };
                    for row in rows.iter_mut().take(r_end).skip(r) {
                        row[c..c_end].iter_mut().for_each(String::clear);
                    }
                    rows[r][c] = text;
                }
            }
            // Banner bands below the header block are sub-headers: they label
            // the rows beneath them rather than the columns.
            let sub_headers: Vec<usize> = match explicit {
                Some(lay) => lay.sub_headers.clone(),
                None => merges
                    .full_width_row
                    .iter()
                    .enumerate()
                    .filter(|(r, full)| {
                        (**full && rows[*r].iter().any(|c| !c.trim().is_empty())
                            || group_label(&rows, *r))
                            && *r >= header_rows
                    })
                    .map(|(r, _)| r)
                    .collect(),
            };
            tables += 1;
            let text = rows
                .iter()
                .map(|r| r.join(" | "))
                .collect::<Vec<_>>()
                .join("\n");
            table_elements.push(Element {
                page: pi,
                bbox: Some(g.bbox),
                cells: Some(rows),
                header_rows: Some(header_rows),
                sub_headers: (!sub_headers.is_empty()).then_some(sub_headers),
                merged_down: merges
                    .continues_above
                    .iter()
                    .any(|x| *x)
                    .then_some(merges.continues_above),
                merged_left: merges
                    .continues_left
                    .iter()
                    .any(|x| *x)
                    .then_some(merges.continues_left),
                ..Element::new("doco:Table", text, "rust", "rules")
            });
            frames.push(TableFrame {
                page: pi,
                bbox: g.bbox,
                xs: g.xs.clone(),
                styled: g.opens_styled(&raw.pages[pi].glyphs, &raw.pages[pi].fills),
                bold: g.opens_bold(&raw.pages[pi].glyphs),
            });
        }
        // Table-confidence anchors: a grid whose structure was *inferred* from
        // alignment rather than read from drawn rules has guessed column
        // boundaries, and the calibration against the benchmark's TEDS
        // component was unambiguous — every deterministic win came from a
        // ruled grid, every large loss from an inferred one. The anchor sits
        // immediately above its table so the hybrid adapter can replace the
        // pipe rows with the VLM's reading of the same region; the pure
        // engine's output is untouched.
        if opts.emit_anchors {
            for (ti, g) in layout.grids.iter().enumerate() {
                if layout.demoted_tables[ti] {
                    continue;
                }
                // A document with this many drawn grids is a chart or a
                // ledger built from one template, and its ruling is the
                // template's: a reading per table would cost a reading per
                // page of an 800-page record to confirm what the rules
                // already say. Escalation is earned per table by an
                // inferred structure or a disagreeing one, not by count.
                if layout.grid_sources[ti] == GridSource::Ruled
                    && ruled_tables > TRUSTED_RULING_TABLES
                {
                    continue;
                }
                // Ruled grids anchor too since the structure arbiter arrived:
                // when the VLM's shape agrees (the common case for drawn
                // grids) the anchor drops and nothing changes; when it
                // disagrees, a drawn grid was still trimmed or banded wrongly
                // and the pixels win. The arbiter is what makes this safe --
                // naive replacement measured catastrophic (TEDS 0.850->0.761).
                let mut bbox = g.bbox;
                bbox.y0 -= 0.01; // order the anchor just before its table
                bbox.y1 = bbox.y0;
                table_elements.push(Element {
                    page: pi,
                    bbox: Some(bbox),
                    ..Element::new(
                        "doco:Figure",
                        format!("[[VLMTAB:p{pi}:t{ti}]]"),
                        "rust",
                        "table-confidence",
                    )
                });
            }
        }

        // Insert anchors for tables the layout detector found and we did
        // not. There is nothing deterministic to arbitrate against, so the
        // VLM's reading is inserted at the region's position outright.
        if opts.emit_anchors && opts.insert_missing_tables {
            for (ni, b) in layout.missing_tables.iter().enumerate() {
                table_elements.push(Element {
                    page: pi,
                    bbox: Some(*b),
                    ..Element::new(
                        "doco:Figure",
                        format!("[[VLMNEW:p{pi}:n{ni}]]"),
                        "rust",
                        "table-missing",
                    )
                });
            }
        }

        // VLM splice anchors, for the hybrid tier. Each routed region becomes
        // a figure element carrying a stable anchor token, positioned by its
        // box so the ordinary interleave places it in reading order; the
        // hybrid adapter replaces the token with the VLM's transcription.
        // Env-gated: the pure deterministic engine's output is unchanged.
        if opts.emit_anchors {
            if let crate::route::Route::VlmRegions(regions) = &layout.route {
                for (ri, b) in regions.iter().enumerate() {
                    table_elements.push(Element {
                        page: pi,
                        bbox: Some(*b),
                        ..Element::new(
                            "doco:Figure",
                            format!("[[VLM:p{pi}:r{ri}]]"),
                            "rust",
                            "route",
                        )
                    });
                }
            }
        }
        for mut element in interleave_tables(page_elements, table_elements) {
            n += 1;
            element.id = format!("elem-{n:05}");
            elements.push(element);
        }
    }
    link_continued_tables(&mut elements, &frames);
    settle_cell_breaks(
        &mut elements,
        flat_lines
            .iter()
            .flat_map(|(ls, _)| ls.iter().map(|l| l.text.as_str())),
    );

    // Per page. Averaged over a document the ratio disappears: a deck whose
    // stat pages are four fifths headings reads 0.36 across all its pages.
    //
    // But judged across pages first: a heading style the document repeats
    // — the block labels of a templated chart, on every one of its
    // hundreds of pages — is corroborated by that repetition, and a page
    // full of such labels is not in doubt. Without this every page of such
    // a chart escalated for a hierarchy the deterministic pass already read
    // correctly.
    let template = heading::template_levels(
        elements
            .iter()
            .filter(|e| e.kind == "doco:SectionTitle")
            .map(|e| (e.page, e.level, e.text.as_str())),
    );
    let mut suspect_headings = Vec::new();
    for page in 0..raw.pages.len() {
        let on_page: Vec<&Element> = elements.iter().filter(|e| e.page == page).collect();
        let kinds: Vec<(&str, &str)> = on_page
            .iter()
            .map(|e| (e.kind.as_str(), e.evidence))
            .collect();
        let marks: Vec<bool> = on_page
            .iter()
            .map(|e| {
                e.kind == "doco:SectionTitle" && e.level.is_some_and(|l| template.contains(&l))
            })
            .collect();
        if let Some(d) = heading::doubt_on_page_with(page, &kinds, &marks) {
            suspect_headings.push(d);
        }
    }

    let suspect_figures = (0..raw.pages.len())
        .filter_map(|p| figure::doubt(&elements, p))
        .collect();

    // Everything above worked in each page's reading frame. The output's
    // boxes are in the display frame — the one a page render has — so a
    // page that was turned to be read has its boxes turned back here, once,
    // at the single point every element passes through.
    for e in elements.iter_mut() {
        if let (Some(b), Some(p)) = (e.bbox, raw.pages.get(e.page)) {
            if p.rotation != 0 {
                e.bbox = Some(p.to_display(b));
            }
        }
    }
    for r in running.iter_mut() {
        if let Some(p) = raw.pages.get(r.page).filter(|p| p.rotation != 0) {
            r.bbox = p.to_display(r.bbox);
        }
    }

    Analysis {
        elements,
        suspect_headings,
        suspect_figures,
        suspect_tables,
        leading,
        body_font,
        furniture_removed,
        furniture: furniture_texts,
        folios,
        furniture_marks,
        running,
        tables,
        timings,
    }
}

/// Settles each hyphen a table cell set down at a line's end as
/// [`table::CELL_BREAK`]: the word is read whole where the document prints
/// it whole elsewhere (`Fluo-` over `rescent`, and `Fluorescent` in the
/// title), and keeps its hyphen otherwise.
///
/// A paragraph reads such a break as a word broken, from its letters alone
/// (see [`block::Block::text`]). A cell needs more: a narrow column breaks a
/// compound at its own hyphen (`long-` over `term`) far more often than
/// running prose does, and the letters cannot tell the two apart. The page
/// usually can: a document printing `Ballasts` in one cell and `Bal-` over
/// `lasts` in the next was hyphenated by its typesetter. A form printed
/// hyphenated elsewhere, or no form at all, keeps the hyphen, as cells did
/// before.
///
/// Printed means set on one line: the words of `lines` (the prose, line by
/// line) and of the tables' cells, whose lines joined only at a space, at a
/// hyphen kept, or at a break still to settle. A paragraph's text is no
/// witness, since it has read its own breaks: two columns of a cell
/// interleaved into a paragraph give `conduc-` over `fixed`, joined there as
/// `conducfixed`.
fn settle_cell_breaks<'a>(elements: &mut [Element], lines: impl Iterator<Item = &'a str>) {
    use std::collections::HashSet;
    let brk = table::CELL_BREAK;
    let marked = |e: &Element| {
        e.text.contains(brk) || e.cells.iter().flatten().flatten().any(|c| c.contains(brk))
    };
    if !elements.iter().any(marked) {
        return;
    }
    let is_word_char = |c: char| c.is_alphanumeric() || c == '-' || c == brk;
    let mut words: HashSet<String> = HashSet::new();
    let mut witness = |text: &str| {
        for w in text.split(|c: char| !is_word_char(c)) {
            if w.is_empty() || w.contains(brk) {
                continue;
            }
            let w = w.to_lowercase();
            if w.contains('-') {
                for part in w.split('-').filter(|p| !p.is_empty()) {
                    words.insert(part.to_string());
                }
            }
            words.insert(w);
        }
    };
    lines.for_each(&mut witness);
    elements
        .iter()
        .filter_map(|e| e.cells.as_ref())
        .flatten()
        .flatten()
        .for_each(|c| witness(c));
    let settle = |text: &str| -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find(brk) {
            // The word around the break: back to its start, on to its end.
            let start = rest[..at]
                .char_indices()
                .rev()
                .take_while(|&(_, c)| is_word_char(c))
                .last()
                .map_or(at, |(i, _)| i);
            let end = rest[at..]
                .char_indices()
                .find(|&(_, c)| !is_word_char(c))
                .map_or(rest.len(), |(i, _)| at + i);
            let word = &rest[start..end];
            let joined = word.replace(brk, "").to_lowercase();
            let hyphened = word.replace(brk, "-").to_lowercase();
            let whole = words.contains(&joined) && !words.contains(&hyphened);
            out.push_str(&rest[..start]);
            out.push_str(&word.replace(brk, if whole { "" } else { "-" }));
            rest = &rest[end..];
        }
        out.push_str(rest);
        out
    };
    for e in elements.iter_mut() {
        if e.text.contains(brk) {
            e.text = settle(&e.text);
        }
        if let Some(rows) = e.cells.as_mut() {
            for cell in rows.iter_mut().flatten() {
                if cell.contains(brk) {
                    *cell = settle(cell);
                }
            }
        }
    }
}

/// Column doubt over a page's prose: its glyphs outside every table.
///
/// [`column::doubt`] sees a gutter that exists only over a band of rows and
/// asks whether the page has columns the page-wide pass missed. A table is
/// exactly that shape — its columns are gutters that exist only over its
/// rows — and on a chart of flow sheets every page answered yes. The
/// tables are already read as tables; the doubt is about the prose.
pub fn column_doubt(page: &Page, analysis: &Analysis) -> Option<crate::column::Doubt> {
    let tables: Vec<crate::geom::BBox> = analysis
        .elements
        .iter()
        .filter(|e| e.page == page.index && e.kind == "doco:Table")
        .filter_map(|e| e.bbox)
        .map(|b| page.from_display(b))
        .collect();
    if tables.is_empty() {
        return crate::column::doubt(&page.glyphs);
    }
    let prose: Vec<Glyph> = page
        .glyphs
        .iter()
        .filter(|g| {
            let (x, y) = g.center();
            !tables.iter().any(|t| t.contains(x, y))
        })
        .cloned()
        .collect();
    crate::column::doubt(&prose)
}

/// A furniture text as the scrub list tells copies apart: a page number by
/// its words, its digits any.
fn furniture_key(text: &str, digits_vary: bool) -> String {
    if digits_vary {
        text.chars()
            .map(|c| if c.is_ascii_digit() { '#' } else { c })
            .collect()
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::BBox;

    #[test]
    fn a_word_a_cell_breaks_is_read_whole_where_the_document_prints_it_whole() {
        let brk = table::CELL_BREAK;
        let cells = vec![vec![
            format!("3-wire Fluo{brk}rescent Bal{brk}lasts"),
            "20 Ballasts".to_string(),
            format!("long{brk}term"),
            format!("Un{brk}employ{brk}ment"),
            format!("Wid{brk}gets"),
        ]];
        let text = cells[0].join(" | ");
        let mut elements = vec![
            // A paragraph's own breaks are read already, and are no witness.
            Element::new("doco:Paragraph", "widgets for all", "rust", "blocks"),
            Element {
                cells: Some(cells),
                ..Element::new("doco:Table", text, "rust", "rules")
            },
        ];
        let lines = [
            "Fluorescent Dimmer",
            "a long-term lease",
            "UNEMPLOYMENT RATE",
        ];
        settle_cell_breaks(&mut elements, lines.into_iter());
        assert_eq!(
            elements[1].cells.as_ref().unwrap()[0],
            [
                "3-wire Fluorescent Ballasts",
                "20 Ballasts",
                // Printed hyphenated elsewhere: the compound's own hyphen.
                "long-term",
                "Unemployment",
                // Printed nowhere else: the hyphen stays, as it was.
                "Wid-gets",
            ]
        );
        assert_eq!(
            elements[1].text,
            "3-wire Fluorescent Ballasts | 20 Ballasts | long-term | Unemployment | Wid-gets"
        );
    }

    /// A table on `page` with these rows, `header` of them header, framed
    /// by columns at `xs`, and the frame it was read from.
    fn part(
        id: &str,
        page: usize,
        rows: &[&[&str]],
        header: usize,
        xs: &[f64],
        styled: bool,
        bold: bool,
    ) -> (Element, TableFrame) {
        let bbox = BBox {
            x0: xs[0],
            y0: if page == 0 { 600.0 } else { 72.0 },
            x1: *xs.last().unwrap(),
            y1: if page == 0 { 720.0 } else { 200.0 },
        };
        let cells: Vec<Vec<String>> = rows
            .iter()
            .map(|r| r.iter().map(|c| c.to_string()).collect())
            .collect();
        let e = Element {
            id: id.into(),
            page,
            bbox: Some(bbox),
            cells: Some(cells),
            header_rows: Some(header),
            ..Element::new("doco:Table", "", "rust", "rules")
        };
        let f = TableFrame {
            page,
            bbox,
            xs: xs.to_vec(),
            styled,
            bold,
        };
        (e, f)
    }

    const XS: [f64; 4] = [72.0, 180.0, 368.0, 595.0];

    fn grid(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(|c| c.to_string()).collect())
            .collect()
    }

    #[test]
    fn a_row_naming_each_spanned_column_is_the_headers_second() {
        let mut left = vec![false; 15];
        left[2] = true;
        left[4] = true;
        let methods = grid(&[
            &["", "Primary", "", "Secondary", ""],
            &["", "NISTmAb", "Amgen mAb", "NISTmAb", "Amgen mAb"],
            &["Channel", "Short", "Short", "Short", "Short"],
        ]);
        assert!(names_spanned_columns(&methods, &left));
        // Data under a banner repeats itself; it names nothing.
        let data = grid(&[
            &["", "Primary", "", "Secondary", ""],
            &["Channel", "Short", "Short", "Short", "Short"],
            &["Flow", "0.5", "0.5", "1.5", "1.5"],
        ]);
        assert!(!names_spanned_columns(&data, &left));
        // Nor is a row of numbers, nor a header with no span.
        let numbers = grid(&[
            &["", "Primary", "", "Secondary", ""],
            &["Flow", "0.5", "0.7", "1.5", "1.7"],
            &["Time", "3", "4", "5", "6"],
        ]);
        assert!(!names_spanned_columns(&numbers, &left));
        assert!(!names_spanned_columns(&methods, &[false; 15]));
        // A value run across the banner's columns names nothing.
        let mut run = left.clone();
        run[5 + 2] = true;
        assert!(!names_spanned_columns(&methods, &run));
        // A band over every column titles rows unless short labels follow.
        let mut band = vec![false; 9];
        band[1] = true;
        band[2] = true;
        let titled = grid(&[
            &["Monotherapy", "", ""],
            &[
                "Adult patients with melanoma",
                "200 mg every 3 weeks",
                "Until disease progression",
            ],
            &[
                "Adult patients with NSCLC",
                "200 mg every 3 weeks",
                "Until disease progression",
            ],
        ]);
        assert!(!names_spanned_columns(&titled, &band));
        let named = grid(&[
            &["Establishment", "", ""],
            &["Name", "Address", "ID/FEI"],
            &["Plant A", "1 Main St", "123"],
        ]);
        assert!(names_spanned_columns(&named, &band));
    }

    #[test]
    fn a_label_alone_over_full_rows_labels_them() {
        let t = grid(&[
            &["", "A", "B"],
            &["Focus-Inject", "", ""],
            &["Position", "12%", "12%"],
            &["Flow-Rate", "0.5", "1.5"],
        ]);
        assert!(group_label(&t, 1));
        assert!(!group_label(&t, 2), "a row with values labels nothing");
        // A total left empty over rows as sparse as it is: missing values.
        let receipt = grid(&[
            &["Item", "Units", "Amount"],
            &["TOTAL", "", ""],
            &["Cash", "", "USD 80.00"],
        ]);
        assert!(!group_label(&receipt, 1));
        // A label wrapped onto the next row is carried on, not a group.
        let wrapped = grid(&[
            &["Endpoint", "A", "B"],
            &["Median in", "", ""],
            &["months (95% CI)", "5.4", "6.5"],
        ]);
        assert!(!group_label(&wrapped, 1));
        // A field left empty in a two-column list is not a label.
        let list = grid(&[&["Name", "Value"], &["Notes", ""], &["Lot", "A12"]]);
        assert!(!group_label(&list, 1));
    }

    #[test]
    fn a_lettered_list_read_as_a_table_is_a_list() {
        let cells = |rows: &[[&str; 2]]| -> Vec<String> {
            rows.iter().flatten().map(|c| c.to_string()).collect()
        };
        let list = [
            ["a)", "Change the pump frit."],
            ["b)", "Flush the loop."],
            ["c)", "Filter it."],
        ];
        assert!(is_marker_list(&cells(&list), 2));
        assert!(is_marker_list(
            &cells(&[["1.", "Thaw."], ["2.", "Mix."]]),
            2
        ));
        assert!(is_marker_list(
            &cells(&[["(iii)", "One."], ["(iv)", "Two."]]),
            2
        ));
        assert!(is_marker_list(&cells(&[["•", "One."], ["•", "Two."]]), 2));
        // A table with labels in its first column is a table.
        let table = [["Added cation", "Relative size"], ["K+", ""], ["Na+", ""]];
        assert!(!is_marker_list(&cells(&table), 2));
        // A year column is not a list of markers, nor is one marker alone.
        assert!(!is_marker_list(
            &cells(&[["2023", "4.1"], ["2024", "3.9"]]),
            2
        ));
        assert!(!is_marker_list(
            &cells(&[["a)", "Only one."], ["", "More text."]]),
            2
        ));
    }

    #[test]
    fn a_table_the_page_break_split_is_linked_and_loses_its_false_header() {
        let (a, fa) = part(
            "elem-00001",
            0,
            &[
                &["Problem", "Cause", "Resolution"],
                &["Noise", "Dirty cell", "Clean it"],
            ],
            1,
            &XS,
            false,
            false,
        );
        // Read alone, the second part's open first cell grew its header
        // three rows deep.
        let (b, fb) = part(
            "elem-00002",
            1,
            &[
                &["", "Dirty system", "Clean it"],
                &["", "Purge due", "Purge it"],
                &["Broadening", "Old column", "Replace it"],
            ],
            3,
            &[72.4, 180.2, 367.9, 595.1],
            false,
            false,
        );
        let mut els = vec![a, b];
        link_continued_tables(&mut els, &[fa, fb]);
        assert_eq!(els[1].continues.as_deref(), Some("elem-00001"));
        assert_eq!(els[1].header_rows, Some(0));
    }

    #[test]
    fn a_new_table_with_its_own_header_is_not_a_continuation() {
        let (a, fa) = part(
            "elem-00001",
            0,
            &[&["A", "B", "C"], &["1", "2", "3"]],
            1,
            &XS,
            false,
            false,
        );
        // Same columns, but it opens with a bold row of its own labels.
        let (b, fb) = part(
            "elem-00002",
            1,
            &[&["D", "E", "F"], &["4", "5", "6"]],
            1,
            &XS,
            true,
            true,
        );
        let mut els = vec![a.clone(), b];
        link_continued_tables(&mut els, &[fa, fb]);
        assert_eq!(els[1].continues, None);
        assert_eq!(els[1].header_rows, Some(1));
        // Nor where the columns are drawn elsewhere.
        let (c, fc) = part(
            "elem-00002",
            1,
            &[&["4", "5", "6"]],
            0,
            &[72.0, 250.0, 400.0, 595.0],
            false,
            false,
        );
        let mut els = vec![a, c];
        let fa = part(
            "elem-00001",
            0,
            &[&["A", "B", "C"], &["1", "2", "3"]],
            1,
            &XS,
            false,
            false,
        )
        .1;
        link_continued_tables(&mut els, &[fa, fc]);
        assert_eq!(els[1].continues, None);
    }

    #[test]
    fn a_header_the_break_split_is_finished_on_the_next_page() {
        // The arms over the columns on one page, a sliver of the next row
        // (a superscript's top) beneath them; the grades on the next page.
        let (a, fa) = part(
            "elem-00001",
            0,
            &[&["Reaction", "Drug", "Placebo"], &["", "*", ""]],
            1,
            &XS,
            false,
            true,
        );
        let (b, fb) = part(
            "elem-00002",
            1,
            &[
                &["", "All Grades* (%)", "All Grades (%)"],
                &["Diarrhea", "56", "45"],
            ],
            1,
            &XS,
            true,
            true,
        );
        let mut els = vec![a, b];
        link_continued_tables(&mut els, &[fa, fb]);
        assert_eq!(els[1].continues.as_deref(), Some("elem-00001"));
        assert_eq!(
            els[1].header_rows,
            Some(1),
            "the grades row finishes the header"
        );
    }

    fn line(x: f64, y: f64) -> Line {
        Line {
            text: "line".into(),
            bbox: BBox {
                x0: x,
                y0: y,
                x1: x + 100.0,
                y1: y + 10.0,
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

    fn element(text: &str, x0: f64, y0: f64, x1: f64, y1: f64, table: bool) -> Element {
        Element {
            bbox: Some(BBox { x0, y0, x1, y1 }),
            cells: table.then(|| vec![vec![text.into()]]),
            ..Element::new(
                if table {
                    "doco:Table"
                } else {
                    "doco:Paragraph"
                },
                text,
                "rust",
                if table { "rules" } else { "layout" },
            )
        }
    }

    #[test]
    fn display_heading_prefix_splits_from_a_lettered_item() {
        let mut display = line(10.0, 10.0);
        display.text = "Replace".into();
        let mut item = line(10.0, 22.0);
        item.text = "l. Replace Plastics with Recyclable Materials.".into();
        let parts = block::split_structural_prefix(block::build(vec![display, item]));
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].text(), "Replace");
    }

    #[test]
    fn table_is_interleaved_with_single_column_prose() {
        let content = vec![
            element("before", 10.0, 10.0, 100.0, 20.0, false),
            element("after", 10.0, 90.0, 100.0, 100.0, false),
        ];
        let tables = vec![element("table", 10.0, 40.0, 100.0, 70.0, true)];
        let ordered = interleave_tables(content, tables);
        assert_eq!(
            ordered.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec!["before", "table", "after"]
        );
    }

    #[test]
    fn table_stays_with_its_column() {
        // Existing prose order is column-major. A right-column table must not
        // interrupt the left column merely because it is high on the page.
        let content = vec![
            element("left above", 10.0, 10.0, 100.0, 20.0, false),
            element("left below", 10.0, 90.0, 100.0, 100.0, false),
            element("right above", 200.0, 10.0, 300.0, 20.0, false),
            element("right below", 200.0, 90.0, 300.0, 100.0, false),
        ];
        let tables = vec![element("table", 200.0, 40.0, 300.0, 70.0, true)];
        let ordered = interleave_tables(content, tables);
        assert_eq!(
            ordered.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec![
                "left above",
                "left below",
                "right above",
                "table",
                "right below"
            ]
        );
    }

    #[test]
    fn block_overlapping_table_band_does_not_precede_table() {
        let content = vec![element("overlap", 10.0, 50.0, 100.0, 80.0, false)];
        let tables = vec![element("table", 10.0, 40.0, 100.0, 70.0, true)];
        let ordered = interleave_tables(content, tables);
        assert_eq!(
            ordered.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec!["table", "overlap"]
        );
    }

    #[test]
    fn ordinary_columns_are_not_y_interleaved() {
        let left: Vec<Line> = (0..20).map(|i| line(10.0, i as f64 * 12.0)).collect();
        let right: Vec<Line> = (0..19).map(|i| line(200.0, i as f64 * 12.0)).collect();
        assert!(!is_contents_layout(&[left, right]));
    }

    #[test]
    fn contents_columns_are_y_interleaved() {
        let mut title = line(10.0, 10.0);
        title.text = "Table of Contents".into();
        let left = vec![title, line(10.0, 100.0)];
        let right = vec![line(200.0, 50.0), line(200.0, 150.0)];
        assert!(is_contents_layout(&[left, right]));
    }

    #[test]
    fn only_weak_typography_yields_to_figure_membership() {
        assert!(heading_yields_to_figure(Evidence::Bold, true));
        assert!(heading_yields_to_figure(Evidence::FontSize, true));
        assert!(!heading_yields_to_figure(Evidence::Numbering, true));
        assert!(!heading_yields_to_figure(Evidence::Sequence, true));
        assert!(!heading_yields_to_figure(Evidence::Title, true));
        assert!(!heading_yields_to_figure(Evidence::Outline, true));
        assert!(!heading_yields_to_figure(Evidence::Bold, false));
    }

    #[test]
    fn prose_and_table_glyphs_are_exclusive() {
        let grid = Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 100.0],
            ys: vec![0.0, 100.0],
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 100.0,
                y1: 100.0,
            },
        };
        let glyph = |x: f64, draw_index: usize| Glyph {
            text: "x".into(),
            bbox: Some(BBox {
                x0: x,
                y0: 10.0,
                x1: x + 5.0,
                y1: 20.0,
            }),
            page: 0,
            origin: (x, 20.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            advance: None,
            draw_index,
        };
        let prose = glyphs_outside_grids(&[glyph(10.0, 1), glyph(110.0, 2)], &[grid]);
        assert_eq!(prose.len(), 1);
        assert_eq!(prose[0].draw_index, 2);
    }

    #[test]
    fn a_lowercase_aligned_heading_continuation_is_coalesced() {
        let mut first = element(
            "Performance details: Document",
            10.0,
            10.0,
            180.0,
            20.0,
            false,
        );
        first.kind = "doco:SectionTitle".into();
        first.level = Some(2);
        first.evidence = "font-size";
        let mut second = element("criteria", 10.0, 25.0, 45.0, 35.0, false);
        second.kind = "doco:SectionTitle".into();
        second.level = Some(2);
        second.evidence = "font-size";
        let mut elements = vec![first, second];
        coalesce_heading_fragments(&mut elements);
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].text, "Performance details: Document criteria");
    }

    #[test]
    fn independent_or_strong_headings_are_not_coalesced() {
        let mut first = element("First Section", 10.0, 10.0, 180.0, 20.0, false);
        first.kind = "doco:SectionTitle".into();
        first.level = Some(1);
        first.evidence = "numbering";
        let mut second = element("continuation", 10.0, 25.0, 80.0, 35.0, false);
        second.kind = "doco:SectionTitle".into();
        second.level = Some(1);
        second.evidence = "numbering";
        let mut elements = vec![first, second];
        coalesce_heading_fragments(&mut elements);
        assert_eq!(elements.len(), 2);
    }

    #[test]
    fn wider_aligned_tables_replace_a_prose_heavy_coarse_grid() {
        let ruled = Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 100.0, 200.0],
            ys: vec![0.0, 50.0, 100.0, 150.0],
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 200.0,
                y1: 150.0,
            },
        };
        let aligned = Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 40.0, 80.0, 120.0, 160.0, 200.0],
            ys: vec![0.0, 50.0, 100.0, 150.0],
            bbox: ruled.bbox,
        };
        let glyph = Glyph {
            text: "word ".repeat(26),
            bbox: Some(BBox {
                x0: 10.0,
                y0: 10.0,
                x1: 90.0,
                y1: 20.0,
            }),
            page: 0,
            origin: (10.0, 20.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            advance: None,
            draw_index: 0,
        };
        assert!(prefer_aligned_over_ruled(&[ruled], &[aligned], &[glyph]));
    }
}
