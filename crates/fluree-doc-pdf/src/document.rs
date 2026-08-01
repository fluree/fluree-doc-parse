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
    pub tables: usize,
    /// Wall clock per stage; see [`StageTimings`].
    pub timings: StageTimings,
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

fn prefer_horizontal_bands(ruled: &[Grid], bands: &[Grid], glyphs: &[Glyph]) -> bool {
    if ruled.len() != 1 || bands.is_empty() {
        return false;
    }
    let ruled_grid = &ruled[0];
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
            band.cols() >= ruled_grid.cols() + 3
                || (ruled_grid.cols() <= 2
                    && band.cols() == ruled_grid.cols()
                    && band.rows() >= ruled_grid.rows() + 2)
        })
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GridSource {
    Ruled,
    FillBand,
    Aligned,
    HorizontalBand,
}

struct PageLayout {
    grids: Vec<Grid>,
    grid_sources: Vec<GridSource>,
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
            !grids.iter().any(|grid| grid.cell_at(x, y).is_some())
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

    for p in raw.pages.iter_mut() {
        timed(&mut timings.dedup, || {
            dedup::remove_faux_bold(&mut p.glyphs, 8)
        });
        // Before anything reads the geometry: a designer's baseline grid is
        // drawn with the same primitive a table's ruling is, and every
        // consumer of `rules` — grids, column rulers, row banding — would
        // otherwise take it for structure.
        crate::rule::strip_layout_lattice(&mut p.rules, p.width, p.height);
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
            trim(table::detect(&p.rules, p.index))
        });
        let mut grid_sources = vec![GridSource::Ruled; grids.len()];
        if !grids.is_empty() {
            let parts = timed(&mut timings.tables, || {
                trim(table::detect_by_component(&p.rules, p.index))
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
            fill_grids.retain(|grid| {
                let cells = grid.cell_texts(&p.glyphs);
                let occupied = cells.iter().filter(|cell| !cell.trim().is_empty()).count();
                occupied * 2 >= grid.rows() * grid.cols()
            });
            grids = fill_grids;
            grid_sources = vec![GridSource::FillBand; grids.len()];
        }

        let aligned_full = timed(&mut timings.tables, || {
            table::detect_aligned(&p.glyphs, &p.rules, &p.fills, p.index)
        });
        if prefer_aligned_over_ruled(&grids, &aligned_full, &p.glyphs) {
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
            prose_columns: cols,
            route,
            missing_tables: missing,
            demoted_tables: demoted,
        });
    }

    let marks = timed(&mut timings.furniture, || furniture::detect(&flat_lines));

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
                let key: String = if digits_vary {
                    text.chars()
                        .map(|c| if c.is_ascii_digit() { '#' } else { c })
                        .collect()
                } else {
                    text.clone()
                };
                if !text.is_empty() && seen.insert(key) {
                    v.push((text, digits_vary));
                }
            }
        }
        v
    };
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
        let block_columns: Vec<Vec<Block>> = timed(&mut timings.blocks, || {
            kept_columns
                .iter()
                .map(|col| block::assemble_with_marks(col, leading, &checkboxes))
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
        heading::detect(&blocks_per_page, outline)
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
    let mut n = 0usize;
    let mut tables = 0usize;
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
        let block_boxes: Vec<(crate::geom::BBox, usize)> =
            blocks.iter().map(|b| (b.bbox, b.lines.len())).collect();
        figure::attach(&mut figures, &block_boxes, (page.width, page.height));
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
                None if b.marker.is_some() => ("doco:ListItem", None, "marker"),
                // A bullet the producer set inside the text run rather than
                // beside it. The marker pass cannot pull out what was never
                // separate, so the list read as paragraphs opening with a
                // square.
                None if block::strip_leading_bullet(&text).is_some() => {
                    ("doco:ListItem", None, "bullet")
                }
                None => ("doco:Paragraph", None, "layout"),
                Some(_) => unreachable!("weak figure heading has figure membership"),
            };
            // A footer that merged into a unique line on one page escapes
            // the cross-page furniture match (singletons cannot repeat);
            // known furniture texts are stripped as substrings instead.
            let scrubbed = furniture::scrub_block(&text, &furniture_texts);
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
                Some(without) if kind == "doco:ListItem" => without.to_string(),
                _ => scrubbed,
            };
            page_elements.push(Element {
                id: String::new(),
                kind: kind.into(),
                page: pi,
                bbox: Some(b.bbox),
                text,
                level,
                cells: None,
                header_rows: None,
                sub_headers: None,
                merged_down: None,
                merged_left: None,
                figure: figure_of,
                links: None,
                provenance: "rust",
                evidence,
            });
        }
        coalesce_heading_fragments(&mut page_elements);
        let mut table_elements = Vec::new();
        let layout = &layouts[pi];
        debug_assert_eq!(layout.grid_sources.len(), layout.grids.len());
        for (gi, g) in layout.grids.iter().enumerate() {
            let flat: Vec<String> = g
                .cell_texts(&raw.pages[pi].glyphs)
                .into_iter()
                .map(|c| furniture::scrub_cell(&c, &furniture_texts))
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
                    id: String::new(),
                    kind: "doco:Paragraph".into(),
                    page: pi,
                    bbox: Some(g.bbox),
                    text,
                    level: None,
                    cells: None,
                    header_rows: None,
                    sub_headers: None,
                    merged_down: None,
                    merged_left: None,
                    figure: None,
                    links: None,
                    provenance: "rust",
                    evidence: "layout-demoted",
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
            let merges = g.merges(&raw.pages[pi].rules, &raw.pages[pi].fills);
            let header_rows = g.header_rows(&rows, &raw.pages[pi].glyphs, &raw.pages[pi].fills);
            // Banner bands below the header block are sub-headers: they label
            // the rows beneath them rather than the columns.
            let sub_headers: Vec<usize> = merges
                .full_width_row
                .iter()
                .enumerate()
                .filter(|(r, full)| {
                    **full && *r >= header_rows && rows[*r].iter().any(|c| !c.trim().is_empty())
                })
                .map(|(r, _)| r)
                .collect();
            tables += 1;
            table_elements.push(Element {
                id: String::new(),
                kind: "doco:Table".into(),
                page: pi,
                bbox: Some(g.bbox),
                text: rows
                    .iter()
                    .map(|r| r.join(" | "))
                    .collect::<Vec<_>>()
                    .join("\n"),
                level: None,
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
                figure: None,
                links: None,
                provenance: "rust",
                evidence: "rules",
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
                    id: String::new(),
                    kind: "doco:Figure".into(),
                    page: pi,
                    bbox: Some(bbox),
                    text: format!("[[VLMTAB:p{pi}:t{ti}]]"),
                    level: None,
                    cells: None,
                    header_rows: None,
                    sub_headers: None,
                    merged_down: None,
                    merged_left: None,
                    figure: None,
                    links: None,
                    provenance: "rust",
                    evidence: "table-confidence",
                });
            }
        }

        // Insert anchors for tables the layout detector found and we did
        // not. There is nothing deterministic to arbitrate against, so the
        // VLM's reading is inserted at the region's position outright.
        if opts.emit_anchors && opts.insert_missing_tables {
            for (ni, b) in layout.missing_tables.iter().enumerate() {
                table_elements.push(Element {
                    id: String::new(),
                    kind: "doco:Figure".into(),
                    page: pi,
                    bbox: Some(*b),
                    text: format!("[[VLMNEW:p{pi}:n{ni}]]"),
                    level: None,
                    cells: None,
                    header_rows: None,
                    sub_headers: None,
                    merged_down: None,
                    merged_left: None,
                    figure: None,
                    links: None,
                    provenance: "rust",
                    evidence: "table-missing",
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
                        id: String::new(),
                        kind: "doco:Figure".into(),
                        page: pi,
                        bbox: Some(*b),
                        text: format!("[[VLM:p{pi}:r{ri}]]"),
                        level: None,
                        cells: None,
                        header_rows: None,
                        sub_headers: None,
                        merged_down: None,
                        merged_left: None,
                        figure: None,
                        links: None,
                        provenance: "rust",
                        evidence: "route",
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

    // Per page. Averaged over a document the ratio disappears: a deck whose
    // stat pages are four fifths headings reads 0.36 across all its pages.
    let mut suspect_headings = Vec::new();
    for page in 0..raw.pages.len() {
        let kinds: Vec<(&str, &str)> = elements
            .iter()
            .filter(|e| e.page == page)
            .map(|e| (e.kind.as_str(), e.evidence))
            .collect();
        if let Some(d) = heading::doubt_on_page(page, &kinds) {
            suspect_headings.push(d);
        }
    }

    let suspect_figures = (0..raw.pages.len())
        .filter_map(|p| figure::doubt(&elements, p))
        .collect();

    Analysis {
        elements,
        suspect_headings,
        suspect_figures,
        suspect_tables,
        leading,
        body_font,
        furniture_removed,
        furniture: furniture_texts,
        tables,
        timings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::BBox;

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
        }
    }

    fn element(text: &str, x0: f64, y0: f64, x1: f64, y1: f64, table: bool) -> Element {
        Element {
            id: String::new(),
            kind: if table {
                "doco:Table"
            } else {
                "doco:Paragraph"
            }
            .into(),
            page: 0,
            bbox: Some(BBox { x0, y0, x1, y1 }),
            text: text.into(),
            level: None,
            cells: table.then(|| vec![vec![text.into()]]),
            header_rows: None,
            sub_headers: None,
            merged_down: None,
            merged_left: None,
            figure: None,
            links: None,
            provenance: "rust",
            evidence: if table { "rules" } else { "layout" },
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
